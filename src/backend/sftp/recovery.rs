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
mod tests {
    use super::*;
    use russh_sftp::{
        protocol::{Attrs, Data, FileAttributes, Handle, Status, StatusCode},
        server,
    };
    use std::sync::Mutex as StdMutex;

    #[cfg(unix)]
    #[test]
    fn recovery_records_reject_links_and_oversized_files() {
        use std::os::unix::fs::symlink;

        let dir = crate::storage::test_private_tempdir();
        let target = dir.path().join("target.json");
        let link = dir.path().join("linked.json");
        let hardlink = dir.path().join("hardlinked.json");
        std::fs::write(&target, b"{}").unwrap();
        symlink(&target, &link).unwrap();
        assert!(read::<serde_json::Value>(&link).is_err());
        std::fs::hard_link(&target, &hardlink).unwrap();
        assert!(read::<serde_json::Value>(&hardlink).is_err());

        std::fs::remove_file(&link).unwrap();
        std::fs::remove_file(&hardlink).unwrap();
        std::fs::write(&target, vec![b'x'; MAX_RECOVERY_RECORD as usize + 1]).unwrap();
        assert!(read::<serde_json::Value>(&target).is_err());
    }

    #[derive(Default)]
    struct Files {
        data: HashMap<String, Vec<u8>>,
        reads: Vec<(u64, u32)>,
        writes: Vec<u64>,
        fail_writes: bool,
    }

    struct Fixture(Arc<StdMutex<Files>>);

    fn ok(id: u32) -> Status {
        Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: String::new(),
        }
    }

    impl server::Handler for Fixture {
        type Error = StatusCode;

        fn unimplemented(&self) -> Self::Error {
            StatusCode::OpUnsupported
        }

        async fn open(
            &mut self,
            id: u32,
            path: String,
            flags: OpenFlags,
            _: FileAttributes,
        ) -> Result<Handle, Self::Error> {
            let mut files = self.0.lock().unwrap();
            if flags.contains(OpenFlags::CREATE) {
                if files.data.contains_key(&path) && flags.contains(OpenFlags::EXCLUDE) {
                    return Err(StatusCode::Failure);
                }
                files.data.entry(path.clone()).or_default();
            }
            if !files.data.contains_key(&path) {
                return Err(StatusCode::NoSuchFile);
            }
            Ok(Handle { id, handle: path })
        }

        async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
            let files = self.0.lock().unwrap();
            let mut attrs = FileAttributes::empty();
            attrs.size = Some(files.data.get(&path).ok_or(StatusCode::NoSuchFile)?.len() as u64);
            attrs.mtime = Some(120);
            attrs.atime = Some(121);
            attrs.set_regular(true);
            Ok(Attrs { id, attrs })
        }

        async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
            self.stat(id, path).await
        }

        async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
            self.stat(id, handle).await
        }

        async fn read(
            &mut self,
            id: u32,
            path: String,
            offset: u64,
            len: u32,
        ) -> Result<Data, Self::Error> {
            let mut files = self.0.lock().unwrap();
            files.reads.push((offset, len));
            let data = files.data.get(&path).ok_or(StatusCode::NoSuchFile)?;
            let offset = offset as usize;
            if offset >= data.len() {
                return Err(StatusCode::Eof);
            }
            Ok(Data {
                id,
                data: data[offset..(offset + len as usize).min(data.len())].to_vec(),
            })
        }

        async fn write(
            &mut self,
            id: u32,
            path: String,
            offset: u64,
            data: Vec<u8>,
        ) -> Result<Status, Self::Error> {
            let mut files = self.0.lock().unwrap();
            if files.fail_writes && offset >= CHECKPOINT_BYTES {
                return Err(StatusCode::Failure);
            }
            files.writes.push(offset);
            let target = files.data.get_mut(&path).ok_or(StatusCode::NoSuchFile)?;
            let offset = offset as usize;
            let end = offset + data.len();
            if end > target.len() {
                target.resize(end, 0);
            }
            target[offset..end].copy_from_slice(&data);
            Ok(ok(id))
        }

        async fn close(&mut self, id: u32, _: String) -> Result<Status, Self::Error> {
            Ok(ok(id))
        }

        async fn rename(
            &mut self,
            id: u32,
            old: String,
            new: String,
        ) -> Result<Status, Self::Error> {
            let mut files = self.0.lock().unwrap();
            if files.data.contains_key(&new) {
                return Err(StatusCode::Failure);
            }
            let data = files.data.remove(&old).ok_or(StatusCode::NoSuchFile)?;
            files.data.insert(new, data);
            Ok(ok(id))
        }

        async fn remove(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
            self.0
                .lock()
                .unwrap()
                .data
                .remove(&path)
                .ok_or(StatusCode::NoSuchFile)?;
            Ok(ok(id))
        }
    }

    async fn session(files: Arc<StdMutex<Files>>) -> Arc<SftpSession> {
        let (client, stream) = tokio::io::duplex(1024 * 1024);
        server::run(stream, Fixture(files)).await;
        Arc::new(SftpSession::new(client).await.unwrap())
    }

    #[tokio::test]
    async fn downloads_use_recorded_ranges_and_discard_changed_sources() {
        let files = Arc::new(StdMutex::new(Files::default()));
        let data: Vec<_> = (0..3 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let remote = "/source";
        files
            .lock()
            .unwrap()
            .data
            .insert(remote.into(), data.clone());
        let temp = crate::storage::test_private_tempdir();
        let local = temp.path().join("download");
        let first = session(files.clone()).await;
        let (download, ranges, _) =
            Download::open(&first, remote, &local, "peer", 3, data.len() as u64)
                .await
                .unwrap();
        let complete = ranges[0].1;
        let mut staging = tokio::fs::OpenOptions::new()
            .write(true)
            .open(&download.part)
            .await
            .unwrap();
        staging.write_all(&data[..complete as usize]).await.unwrap();
        staging.sync_data().await.unwrap();
        download.checkpoint(0, complete).await.unwrap();
        drop(staging);
        drop(download);
        drop(first);
        files.lock().unwrap().reads.clear();

        let second = session(files.clone()).await;
        let options = DownloadOptions {
            parallel: 2,
            max_bytes_per_second: 0,
            timestamps: false,
        };
        download_file(second.clone(), remote, &local, options, None, Some("peer"))
            .await
            .unwrap();
        let downloaded = std::fs::read(&local).unwrap();
        assert_eq!(downloaded.len(), data.len());
        if let Some(at) = downloaded
            .iter()
            .zip(&data)
            .position(|(got, wanted)| got != wanted)
        {
            let reads = &files.lock().unwrap().reads;
            panic!(
                "download mismatch at {at}: {} != {}, resumed range reads {reads:?}",
                downloaded[at], data[at]
            );
        }
        assert!(
            files
                .lock()
                .unwrap()
                .reads
                .iter()
                .all(|&(offset, _)| !(64 * 1024..complete - SAMPLE_BYTES).contains(&offset))
        );
        let replacement = temp.path().join("replacement");
        let (download, _, _) =
            Download::open(&second, remote, &replacement, "peer", 2, data.len() as u64)
                .await
                .unwrap();
        download.checkpoint(0, 4096).await.unwrap();
        drop(download);
        files.lock().unwrap().data.get_mut(remote).unwrap()[0] ^= 1;
        let (_, _, offsets) =
            Download::open(&second, remote, &replacement, "peer", 2, data.len() as u64)
                .await
                .unwrap();
        assert_eq!(offsets[0], 0);
    }

    #[tokio::test]
    async fn resumed_download_never_overwrites_an_existing_destination() {
        let files = Arc::new(StdMutex::new(Files::default()));
        let data = b"remote download fixture".to_vec();
        let remote = "/source";
        files
            .lock()
            .unwrap()
            .data
            .insert(remote.into(), data.clone());
        let temp = crate::storage::test_private_tempdir();
        let local = temp.path().join("download");
        let sftp = session(files).await;
        let (download, ranges, _) =
            Download::open(&sftp, remote, &local, "peer", 1, data.len() as u64)
                .await
                .unwrap();
        std::fs::write(&download.part, &data).unwrap();
        download.checkpoint(0, ranges[0].1).await.unwrap();
        std::fs::write(&local, b"keep existing local data").unwrap();

        let error = download.publish(&sftp, remote).await.unwrap_err();
        assert!(error.contains("already exists"));
        assert_eq!(std::fs::read(&local).unwrap(), b"keep existing local data");
        assert!(download.part.exists());
    }

    #[tokio::test]
    async fn ordinary_download_never_overwrites_an_existing_destination() {
        let files = Arc::new(StdMutex::new(Files::default()));
        let data = b"remote download fixture".to_vec();
        let remote = "/source";
        files.lock().unwrap().data.insert(remote.into(), data);
        let temp = crate::storage::test_private_tempdir();
        let local = temp.path().join("download");
        std::fs::write(&local, b"keep existing local data").unwrap();
        let sftp = session(files).await;
        let options = super::super::DownloadOptions {
            parallel: 1,
            max_bytes_per_second: 0,
            timestamps: false,
        };

        let error = super::super::download_file(sftp, remote, &local, options, None, None)
            .await
            .unwrap_err();
        assert!(error.contains("already exists"));
        assert_eq!(std::fs::read(&local).unwrap(), b"keep existing local data");
    }

    fn upload_permit() -> crate::agent::Permit {
        let active = Arc::new(std::sync::atomic::AtomicBool::new(true));
        crate::agent::Permit {
            bridge: active.clone(),
            request: active,
            deadline: Instant::now() + Duration::from_secs(60),
        }
    }

    fn upload_display() -> crate::backend::AgentDisplay {
        Arc::new(StdMutex::new(crate::backend::AgentTranscript::default()))
    }

    #[tokio::test]
    async fn agent_upload_publishes_only_complete_files_and_never_overwrites() {
        let files = Arc::new(StdMutex::new(Files::default()));
        let temp = crate::storage::test_private_tempdir();
        let local = temp.path().join("upload");
        let data = b"complete staged upload";
        std::fs::write(&local, data).unwrap();
        let sftp = session(files.clone()).await;
        let mut input = tokio::fs::File::open(&local).await.unwrap();

        super::super::upload_agent_file(
            sftp.clone(),
            &mut input,
            data.len() as u64,
            "/target",
            &upload_permit(),
            &upload_display(),
            0,
            data.len() as u64,
        )
        .await
        .unwrap();

        let existing = b"keep existing remote data".to_vec();
        {
            let mut remote = files.lock().unwrap();
            assert_eq!(remote.data.get("/target").unwrap(), data);
            assert_eq!(
                remote.data.len(),
                1,
                "the staging name must be renamed away"
            );
            remote.data.insert("/existing".into(), existing.clone());
        }

        let mut input = tokio::fs::File::open(&local).await.unwrap();
        let error = super::super::upload_agent_file(
            sftp,
            &mut input,
            data.len() as u64,
            "/existing",
            &upload_permit(),
            &upload_display(),
            0,
            data.len() as u64,
        )
        .await
        .unwrap_err();
        assert!(error.contains("destination may already exist"));
        let remote = files.lock().unwrap();
        assert_eq!(remote.data.get("/existing"), Some(&existing));
        assert_eq!(
            remote.data.len(),
            2,
            "failed publication must remove its staging file"
        );
    }

    #[tokio::test]
    async fn agent_upload_discards_a_failed_staging_file() {
        let files = Arc::new(StdMutex::new(Files {
            fail_writes: true,
            ..Default::default()
        }));
        let temp = crate::storage::test_private_tempdir();
        let local = temp.path().join("upload");
        let data = vec![b'x'; CHECKPOINT_BYTES as usize + 1024];
        std::fs::write(&local, &data).unwrap();
        let sftp = session(files.clone()).await;
        let mut input = tokio::fs::File::open(&local).await.unwrap();
        let error = super::super::upload_agent_file(
            sftp,
            &mut input,
            data.len() as u64,
            "/target",
            &upload_permit(),
            &upload_display(),
            0,
            data.len() as u64,
        )
        .await
        .unwrap_err();

        assert!(error.contains("incomplete staging data was discarded"));
        let remote = files.lock().unwrap();
        assert!(!remote.data.contains_key("/target"));
        assert!(
            remote.data.is_empty(),
            "failed staging data must be removed"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn download_publication_uses_the_canonical_destination() {
        use std::os::unix::fs::symlink;

        let files = Arc::new(StdMutex::new(Files::default()));
        let data = b"remote download fixture".to_vec();
        let remote = "/source";
        files
            .lock()
            .unwrap()
            .data
            .insert(remote.into(), data.clone());
        let temp = crate::storage::test_private_tempdir();
        let original = temp.path().join("original");
        let redirected = temp.path().join("redirected");
        std::fs::create_dir(&original).unwrap();
        std::fs::create_dir(&redirected).unwrap();
        let alias = temp.path().join("download-dir");
        symlink(&original, &alias).unwrap();
        let local = alias.join("download");
        let sftp = session(files).await;
        let (download, _, _) = Download::open(&sftp, remote, &local, "peer", 1, data.len() as u64)
            .await
            .unwrap();
        std::fs::write(&download.part, &data).unwrap();
        download.checkpoint(0, data.len() as u64).await.unwrap();

        std::fs::remove_file(&alias).unwrap();
        symlink(&redirected, &alias).unwrap();
        download.publish(&sftp, remote).await.unwrap();

        assert_eq!(std::fs::read(original.join("download")).unwrap(), data);
        assert!(!redirected.join("download").exists());
    }

    #[cfg(unix)]
    #[test]
    fn staging_open_rejects_symlinks_without_truncating_the_target() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::fs::symlink;

        let temp = crate::storage::test_private_tempdir();
        let target = temp.path().join("important-file");
        let staging = temp.path().join("download.part");
        let hardlink = temp.path().join("download-hardlink.part");
        std::fs::write(&target, b"keep this data").unwrap();
        symlink(&target, &staging).unwrap();
        std::fs::hard_link(&target, &hardlink).unwrap();

        assert!(open_staging(&staging, false, true, false, true).is_err());
        assert!(open_staging(&hardlink, false, true, false, true).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep this data");

        std::fs::remove_file(&staging).unwrap();
        std::fs::remove_file(&hardlink).unwrap();
        std::fs::write(&staging, b"staging data").unwrap();
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o644)).unwrap();
        let file = open_staging(&staging, true, true, false, false).unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[tokio::test]
    async fn uploads_resume_after_the_last_saved_offset() {
        let files = Arc::new(StdMutex::new(Files {
            fail_writes: true,
            ..Default::default()
        }));
        let temp = crate::storage::test_private_tempdir();
        let local = temp.path().join("upload");
        let records = temp.path().join("records");
        let data: Vec<_> = (0..CHECKPOINT_BYTES + 10_000)
            .map(|i| (i % 251) as u8)
            .collect();
        std::fs::write(&local, &data).unwrap();
        let first = session(files.clone()).await;
        assert!(
            upload_with_records(&first, &local, "/target", "peer", &records)
                .await
                .is_err()
        );
        drop(first);
        {
            let mut files = files.lock().unwrap();
            assert!(!files.data.contains_key("/target"));
            files.fail_writes = false;
            files.writes.clear();
        }
        let second = session(files.clone()).await;
        upload_with_records(&second, &local, "/target", "peer", &records)
            .await
            .unwrap();
        let files = files.lock().unwrap();
        assert_eq!(files.writes.iter().min(), Some(&CHECKPOINT_BYTES));
        let uploaded = &files.data["/target"];
        assert_eq!(uploaded.len(), data.len());
        assert_eq!(
            uploaded
                .iter()
                .zip(&data)
                .position(|(got, wanted)| got != wanted),
            None
        );
    }
}
