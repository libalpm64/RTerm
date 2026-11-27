use super::*;
use russh_sftp::protocol::OpenFlags;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

pub(super) const CHECKPOINT_BYTES: u64 = 4 * 1024 * 1024;
const SAMPLE_BYTES: u64 = 32 * 1024;
const MAX_RECOVERY_RECORD: u64 = 64 * 1024;

pub(super) fn open_staging(
    path: &Path,
    read: bool,
    write: bool,
    create_new: bool,
    truncate: bool,
) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(read).write(write);
    if create_new {
        options.create_new(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        if !path
            .parent()
            .is_some_and(crate::storage::unix_directory_is_private)
        {
            return Err("Download staging directory is writable by another user".into());
        }
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let file = options.open(path).map_err(to_error)?;
        let metadata = file.metadata().map_err(to_error)?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
        {
            return Err("Download staging path is not a private regular file".into());
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(to_error)?;
        }
        if truncate {
            file.set_len(0).map_err(to_error)?;
        }
        Ok(file)
    }
    #[cfg(windows)]
    {
        use std::os::windows::{
            fs::{MetadataExt, OpenOptionsExt},
            io::AsRawHandle,
        };
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        // Open the reparse point itself, never its target. Deny delete sharing
        // while range workers hold the file so its path cannot be swapped.
        options.custom_flags(0x00200000).share_mode(3);
        let file = options.open(path).map_err(to_error)?;
        let metadata = file.metadata().map_err(to_error)?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if !metadata.is_file()
            || metadata.file_attributes() & 0x400 != 0
            || info.nNumberOfLinks != 1
            || !crate::storage::windows_file_is_not_shared_writable(&file)
            || !path
                .parent()
                .is_some_and(crate::storage::windows_directory_is_not_shared_writable)
        {
            return Err("Download staging path is not a private regular file".into());
        }
        if truncate {
            file.set_len(0).map_err(to_error)?;
        }
        Ok(file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let file = options.open(path).map_err(to_error)?;
        if !file.metadata().map_err(to_error)?.is_file() {
            return Err("Download staging path is not a regular file".into());
        }
        if truncate {
            file.set_len(0).map_err(to_error)?;
        }
        Ok(file)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Version {
    size: u64,
    modified: Option<u32>,
    edges: String,
}

pub(super) async fn remote_version(sftp: &SftpSession, path: &str) -> Result<Version, String> {
    let mut input = sftp.open(path).await.map_err(to_error)?;
    let info = input.metadata().await.map_err(to_error)?;
    let size = info.len();
    let edges = sample(&mut input, size).await?;
    let after = input.metadata().await.map_err(to_error)?;
    if size != after.len() || info.mtime != after.mtime {
        return Err("The remote file changed while it was being checked".into());
    }
    Ok(Version {
        size,
        modified: info.mtime,
        edges,
    })
}

async fn sample(
    source: &mut (impl tokio::io::AsyncRead + tokio::io::AsyncSeek + Unpin),
    end: u64,
) -> Result<String, String> {
    let count = end.min(SAMPLE_BYTES) as usize;
    let mut bytes = vec![0; count];
    let mut hash = blake3::Hasher::new();
    for offset in [0, end - count as u64] {
        source
            .seek(SeekFrom::Start(offset))
            .await
            .map_err(to_error)?;
        source.read_exact(&mut bytes).await.map_err(to_error)?;
        hash.update(&bytes);
        if offset == end - count as u64 {
            break;
        }
    }
    Ok(hash.finalize().to_hex().to_string())
}

fn transfer_name(peer: &str, remote: &str, local: &Path) -> String {
    let mut hash = blake3::Hasher::new();
    for component in [
        peer.as_bytes(),
        remote.as_bytes(),
        local.as_os_str().as_encoded_bytes(),
    ] {
        hash.update(&(component.len() as u64).to_le_bytes());
        hash.update(component);
    }
    hash.finalize().to_hex()[..32].to_string()
}

fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    use std::io::Read;

    let mut file = match open_staging(path, true, false, false, false) {
        Ok(file) => file,
        Err(error) => match std::fs::symlink_metadata(path) {
            Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            _ => return Err(error),
        },
    };
    if file.metadata().map_err(to_error)?.len() > MAX_RECOVERY_RECORD {
        return Err("Transfer recovery record exceeds 64 KiB".into());
    }
    let mut data = Vec::new();
    std::io::Read::take(&mut file, MAX_RECOVERY_RECORD + 1)
        .read_to_end(&mut data)
        .map_err(to_error)?;
    if data.len() as u64 > MAX_RECOVERY_RECORD {
        return Err("Transfer recovery record exceeds 64 KiB".into());
    }
    serde_json::from_slice(&data).map(Some).map_err(to_error)
}

fn write(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(to_error)?;
    crate::storage::write_atomic(path, &bytes)
}

fn lock(path: &Path) -> Result<std::fs::File, String> {
    let file = match open_staging(path, true, true, false, false) {
        Ok(file) => file,
        Err(error) => match std::fs::symlink_metadata(path) {
            Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => {
                open_staging(path, true, true, true, false)?
            }
            _ => return Err(error),
        },
    };
    file.try_lock()
        .map_err(|error| format!("Could not lock transfer: {error}"))?;
    Ok(file)
}

#[derive(Serialize, Deserialize)]
struct DownloadState {
    version: Version,
    ranges: Vec<(u64, u64)>,
    offsets: Vec<u64>,
}

pub(super) struct Download {
    local: PathBuf,
    pub(super) part: PathBuf,
    journal: PathBuf,
    state: Mutex<DownloadState>,
    _lock: std::fs::File,
}

impl Download {
    pub(super) async fn open(
        sftp: &SftpSession,
        remote: &str,
        local: &Path,
        peer: &str,
        count: u64,
        size: u64,
    ) -> Result<(Self, Vec<(u64, u64)>, Vec<u64>), String> {
        let version = remote_version(sftp, remote).await?;
        if version.size != size {
            return Err("The remote file changed before download started".into());
        }
        let selected_parent = local.parent().ok_or("Missing download directory")?;
        #[cfg(unix)]
        if !crate::storage::unix_directory_is_private(selected_parent) {
            return Err(
                "Download directory is not protected against replacement by another user".into(),
            );
        }
        #[cfg(windows)]
        if !crate::storage::windows_directory_is_not_shared_writable(selected_parent) {
            return Err("Download directory is writable by another Windows account".into());
        }
        let parent = std::fs::canonicalize(selected_parent).map_err(to_error)?;
        #[cfg(unix)]
        if !crate::storage::unix_directory_is_private(&parent) {
            return Err(
                "Download directory is not protected against replacement by another user".into(),
            );
        }
        #[cfg(windows)]
        if !crate::storage::windows_directory_is_not_shared_writable(&parent) {
            return Err("Download directory is writable by another Windows account".into());
        }
        let local = parent.join(local.file_name().ok_or("Missing download name")?);
        let stem = format!(".rterm-download-{}", transfer_name(peer, remote, &local));
        let part = parent.join(format!("{stem}.part"));
        let journal = parent.join(format!("{stem}.json"));
        let _lock = lock(&parent.join(format!("{stem}.lock")))?;
        let previous = read::<DownloadState>(&journal)?;
        let staging = match std::fs::symlink_metadata(&part) {
            Ok(info) if info.is_file() => Some(info),
            Ok(_) => return Err("Download staging path is not a regular file".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        };
        let state = if let Some(state) = previous
            && state.version == version
            && staging
                .as_ref()
                .is_some_and(|info| info.len() == version.size)
            && valid_ranges(version.size, &state.ranges, &state.offsets)
            && verified_ranges(sftp, remote, &part, &state).await?
        {
            state
        } else {
            let span = version.size.div_ceil(count);
            let ranges: Vec<_> = (0..count)
                .filter_map(|index| {
                    let start = index * span;
                    let end = start.saturating_add(span).min(version.size);
                    (start < end).then_some((start, end))
                })
                .collect();
            let offsets = ranges.iter().map(|&(start, _)| start).collect();
            let state = DownloadState {
                version,
                ranges,
                offsets,
            };
            write(&journal, &state)?;
            let file = if staging.is_some() {
                open_staging(&part, false, true, false, true)
            } else {
                open_staging(&part, false, true, true, false)
            }?;
            file.set_len(state.version.size).map_err(to_error)?;
            state
        };
        let ranges = state.ranges.clone();
        let offsets = state.offsets.clone();
        Ok((
            Self {
                local,
                part,
                journal,
                state: Mutex::new(state),
                _lock,
            },
            ranges,
            offsets,
        ))
    }

    pub(super) async fn checkpoint(&self, index: usize, offset: u64) -> Result<(), String> {
        let mut state = self.state.lock().await;
        state.offsets[index] = offset;
        write(&self.journal, &*state)
    }

    pub(super) async fn publish(&self, sftp: &SftpSession, remote: &str) -> Result<(), String> {
        let version = remote_version(sftp, remote).await?;
        let state = self.state.lock().await;
        if state.version != version {
            return Err(
                "The remote file changed during download. Retry to download the current version."
                    .into(),
            );
        }
        if !state
            .ranges
            .iter()
            .zip(&state.offsets)
            .all(|((_, end), offset)| end == offset)
        {
            return Err(
                "Download checkpoints are incomplete. Repeat the download to resume.".into(),
            );
        }
        // Re-open the final staging path without following links immediately
        // before publication; workers may have closed their handles already.
        drop(open_staging(&self.part, false, true, false, false)?);
        // Persist with no-clobber semantics so a remote filename cannot
        // silently replace an existing local file. Retain the resumable part
        // when publication fails.
        if let Err(error) = tempfile::TempPath::try_from_path(&self.part)
            .map_err(to_error)?
            .persist_noclobber(&self.local)
        {
            let tempfile::PathPersistError { error, mut path } = error;
            path.disable_cleanup(true);
            return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
                "Download destination already exists; choose another folder or remove it first"
                    .into()
            } else {
                to_error(error)
            });
        }
        let _ = std::fs::remove_file(&self.journal);
        Ok(())
    }

    pub(super) fn set_times(&self, times: std::fs::FileTimes) -> Result<(), String> {
        open_staging(&self.part, false, true, false, false)?
            .set_times(times)
            .map_err(to_error)
    }
}

fn valid_ranges(size: u64, ranges: &[(u64, u64)], offsets: &[u64]) -> bool {
    let mut end = 0;
    ranges.len() == offsets.len()
        && ranges.len() <= 6
        && ranges.iter().zip(offsets).all(|(&(start, next), offset)| {
            let valid =
                start == end && start < next && next <= size && (start..=next).contains(offset);
            end = next;
            valid
        })
        && end == size
}

async fn verified_ranges(
    sftp: &SftpSession,
    remote: &str,
    part: &Path,
    state: &DownloadState,
) -> Result<bool, String> {
    let mut source = sftp.open(remote).await.map_err(to_error)?;
    let mut staged = tokio::fs::File::from_std(open_staging(part, true, true, false, false)?);
    for (&(start, _), &offset) in state.ranges.iter().zip(&state.offsets) {
        if !matches_edges(&mut source, &mut staged, start, offset).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn matches_edges(
    source: &mut (impl tokio::io::AsyncRead + tokio::io::AsyncSeek + Unpin),
    staged: &mut (impl tokio::io::AsyncRead + tokio::io::AsyncSeek + Unpin),
    start: u64,
    end: u64,
) -> Result<bool, String> {
    let count = (end - start).min(SAMPLE_BYTES) as usize;
    let mut expected = vec![0; count];
    let mut actual = vec![0; count];
    if count == 0 {
        return Ok(true);
    }
    for at in [start, end - count as u64] {
        source.seek(SeekFrom::Start(at)).await.map_err(to_error)?;
        source.read_exact(&mut expected).await.map_err(to_error)?;
        staged.seek(SeekFrom::Start(at)).await.map_err(to_error)?;
        staged.read_exact(&mut actual).await.map_err(to_error)?;
        if expected != actual {
            return Ok(false);
        }
        if at == end - count as u64 {
            break;
        }
    }
    Ok(true)
}

#[derive(Serialize, Deserialize)]
struct UploadState {
    upload_id: String,
    hash: String,
    position: u64,
}

impl UploadState {
    fn stage_path(&self, directory: &str) -> Result<String, String> {
        if self.upload_id.len() != 32
            || !self.upload_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("Invalid upload recovery record".into());
        }
        Ok(join_remote(
            directory,
            &format!(".rterm-upload-{}.part", self.upload_id),
        ))
    }
}

async fn hash_file(file: &mut tokio::fs::File) -> Result<String, String> {
    file.seek(SeekFrom::Start(0)).await.map_err(to_error)?;
    let mut hash = blake3::Hasher::new();
    let mut buffer = vec![0; 256 * 1024];
    loop {
        let count = file.read(&mut buffer).await.map_err(to_error)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash.finalize().to_hex().to_string())
}

pub(super) async fn upload(
    sftp: &SftpSession,
    local: &Path,
    remote: &str,
    peer: &str,
) -> Result<(), String> {
    let records = crate::storage::get_config_dir().join("transfers");
    upload_with_records(sftp, local, remote, peer, &records).await
}

async fn upload_with_records(
    sftp: &SftpSession,
    local: &Path,
    remote: &str,
    peer: &str,
    records: &Path,
) -> Result<(), String> {
    let (directory, _) = remote
        .rsplit_once('/')
        .ok_or("Missing remote upload directory")?;
    let directory = if directory.is_empty() { "/" } else { directory };
    let local = std::fs::canonicalize(local).map_err(to_error)?;
    if !crate::storage::ensure_private_config_dir(records) {
        return Err("Unsafe transfer recovery directory".into());
    }
    let name = transfer_name(peer, remote, &local);
    let journal = records.join(format!("{name}.json"));
    let _lock = lock(&records.join(format!("{name}.lock")))?;
    let mut input = tokio::fs::File::open(&local).await.map_err(to_error)?;
    let size = input.metadata().await.map_err(to_error)?.len();
    let hash = hash_file(&mut input).await?;
    let previous = read::<UploadState>(&journal)?;
    let mut resume = None;
    if let Some(state) = previous {
        let stage = state.stage_path(directory)?;
        if sftp.try_exists(&stage).await.map_err(to_error)? {
            let info = sftp.symlink_metadata(&stage).await.map_err(to_error)?;
            if !info.file_type().is_file() {
                return Err("Upload staging path is not a regular file".into());
            }
            if state.hash == hash
                && state.position <= info.len()
                && info.len() <= size
                && matches_edges(
                    &mut input,
                    &mut sftp.open(&stage).await.map_err(to_error)?,
                    0,
                    state.position,
                )
                .await?
            {
                resume = Some((state, stage));
            } else {
                sftp.remove_file(&stage).await.map_err(to_error)?;
            }
        }
    }
    let mut output;
    let (mut state, stage) = if let Some((state, stage)) = resume {
        output = sftp
            .open_with_flags(stage.clone(), OpenFlags::WRITE)
            .await
            .map_err(to_error)?;
        (state, stage)
    } else {
        let state = UploadState {
            upload_id: crate::agent::random_id(),
            hash,
            position: 0,
        };
        let stage = state.stage_path(directory)?;
        write(&journal, &state)?;
        output = sftp
            .open_with_flags(
                stage.clone(),
                OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
            )
            .await
            .map_err(to_error)?;
        (state, stage)
    };
    input
        .seek(SeekFrom::Start(state.position))
        .await
        .map_err(to_error)?;
    output
        .seek(SeekFrom::Start(state.position))
        .await
        .map_err(to_error)?;
    while state.position < size {
        let mut chunk = (&mut input).take((size - state.position).min(CHECKPOINT_BYTES));
        let expected = chunk.limit();
        let written = tokio::io::copy(&mut chunk, &mut output)
            .await
            .map_err(to_error)?;
        if written != expected {
            return Err("The local file changed during upload".into());
        }
        output.flush().await.map_err(to_error)?;
        state.position += written;
        write(&journal, &state)?;
    }
    output.shutdown().await.map_err(to_error)?;
    if hash_file(&mut input).await? != state.hash {
        return Err(
            "The local file changed during upload. Retry to upload the current version.".into(),
        );
    }
    sftp.rename(&stage, remote).await.map_err(|error| {
        format!("Could not publish the staged upload: {error}. The transfer can be retried.")
    })?;
    let _ = std::fs::remove_file(&journal);
    Ok(())
}

#[cfg(test)]
#[path = "../../../tests/unit/backend/sftp/recovery.rs"]
mod tests;
