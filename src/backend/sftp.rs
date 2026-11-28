use std::{
    collections::HashMap,
    io::{SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, UNIX_EPOCH},
};

const MAX_DIRECTORY_ENTRIES: usize = 100_000;
const MAX_ACTIVE_TRANSFERS: usize = 8;
const MAX_OUTSTANDING_TRANSFERS: usize = 256;
const MAX_ENTRY_NAME_BYTES: usize = 4096;

use russh::client::Handle;
use russh_sftp::client::SftpSession;
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::Semaphore,
    task::JoinSet,
};

use super::{BackendEvent, BackendResult, Events, Reply, connection::SshHandler};

mod conflicts;
mod recovery;
pub(crate) use conflicts::{ConflictBatch, ConflictChoice, ConflictPrompt};

#[derive(Clone)]
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) directory: bool,
    pub(crate) size: u64,
    pub(crate) permissions: Option<u32>,
    pub(crate) modified: Option<chrono::DateTime<chrono::Local>>,
}

pub(crate) enum RemoteAction {
    List(String),
    CreateFolder {
        directory: String,
        name: String,
    },
    Rename {
        from: String,
        to: String,
    },
    Delete {
        path: String,
        directory: bool,
    },
    Upload {
        local: PathBuf,
        remote: String,
        resume: Option<String>,
        batch: ConflictBatch,
    },
    UploadTree {
        local: PathBuf,
        remote: String,
        batch: ConflictBatch,
    },
    Download {
        remote: String,
        local: PathBuf,
        directory: bool,
        options: DownloadOptions,
        resume: Option<String>,
        batch: ConflictBatch,
    },
}

#[derive(Clone, Copy)]
pub(crate) struct DownloadOptions {
    pub(crate) parallel: usize,
    pub(crate) max_bytes_per_second: u64,
    pub(crate) timestamps: bool,
}

pub(super) struct Service {
    sessions: HashMap<u32, SessionWorker>,
    transfer_slots: Arc<Semaphore>,
    outstanding_transfers: Arc<Semaphore>,
}

struct SessionWorker {
    sender: tokio::sync::mpsc::Sender<(RemoteAction, std::sync::mpsc::Sender<BackendResult>)>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for SessionWorker {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct SessionService {
    session: Option<Arc<SftpSession>>,
    transfer_slots: Arc<Semaphore>,
    outstanding_transfers: Arc<Semaphore>,
}

impl Default for Service {
    fn default() -> Self {
        Self {
            sessions: HashMap::new(),
            transfer_slots: Arc::new(Semaphore::new(MAX_ACTIVE_TRANSFERS)),
            outstanding_transfers: Arc::new(Semaphore::new(MAX_OUTSTANDING_TRANSFERS)),
        }
    }
}

impl Service {
    pub(super) fn close(&mut self, id: u32) {
        self.sessions.remove(&id);
    }

    pub(super) fn dispatch(
        &mut self,
        id: u32,
        connection: Arc<Handle<SshHandler>>,
        action: RemoteAction,
        reply: std::sync::mpsc::Sender<BackendResult>,
        events: &Events,
    ) {
        // Serialize actions within each connection, but never make another
        // SSH connection or the global dispatcher wait on a directory read.
        let worker = self.sessions.entry(id).or_insert_with(|| {
            let (sender, mut receiver) = tokio::sync::mpsc::channel::<(
                RemoteAction,
                std::sync::mpsc::Sender<BackendResult>,
            )>(64);
            let events = events.clone();
            let mut service = SessionService {
                session: None,
                transfer_slots: self.transfer_slots.clone(),
                outstanding_transfers: self.outstanding_transfers.clone(),
            };
            let task = tokio::spawn(async move {
                while let Some((action, reply)) = receiver.recv().await {
                    let result = service.handle(id, &connection, action, &events).await;
                    let _ = reply.send(result);
                    events.context.request_repaint();
                }
            });
            SessionWorker { sender, task }
        });
        if let Err(error) = worker.sender.try_send((action, reply)) {
            let (_, reply) = error.into_inner();
            let _ = reply.send(Err("SFTP request queue busy or closed".into()));
            events.context.request_repaint();
        }
    }
}

impl SessionService {
    async fn handle(
        &mut self,
        id: u32,
        connection: &Handle<SshHandler>,
        action: RemoteAction,
        events: &Events,
    ) -> BackendResult {
        // Open one subsystem channel per SSH connection and reuse it for
        // directory browsing, commands, and simultaneous transfers.
        let sftp = match &self.session {
            Some(session) => session.clone(),
            None => {
                let open = async {
                    let channel = connection.channel_open_session().await.map_err(to_error)?;
                    channel
                        .request_subsystem(true, "sftp")
                        .await
                        .map_err(to_error)?;
                    SftpSession::new(channel.into_stream())
                        .await
                        .map_err(to_error)
                };
                let session = tokio::time::timeout(Duration::from_secs(15), open)
                    .await
                    .map_err(|_| "Opening SFTP timed out".to_owned())??;
                let session = Arc::new(session);
                self.session = Some(session.clone());
                session
            }
        };
        match action {
            RemoteAction::List(path) => {
                let (path, files) = list_directory(&sftp, &path).await?;
                let mut entries = Vec::new();
                for file in files {
                    if entries.len() == MAX_DIRECTORY_ENTRIES {
                        return Err(format!(
                            "Remote directory exceeds {MAX_DIRECTORY_ENTRIES} entries"
                        ));
                    }
                    let name = file.file_name();
                    if name.len() > MAX_ENTRY_NAME_BYTES {
                        return Err("Remote filename exceeds 4096 bytes".into());
                    }
                    let metadata = file.metadata();
                    entries.push(Entry {
                        name,
                        directory: file.file_type().is_dir(),
                        size: metadata.len(),
                        permissions: metadata.permissions.map(|mode| mode & 0o777),
                        modified: metadata
                            .mtime
                            .and_then(|seconds| {
                                chrono::DateTime::from_timestamp(i64::from(seconds), 0)
                            })
                            .map(|time| time.with_timezone(&chrono::Local)),
                    });
                }
                Ok(Reply::Files { path, entries })
            }
            RemoteAction::CreateFolder { directory, name } => {
                if !directory.starts_with('/') {
                    return Err("Choose an absolute SFTP directory".into());
                }
                if name.trim().is_empty() {
                    return Err("Enter a folder name".into());
                }
                let path = join_remote(&directory, safe_name(&name)?);
                // MKDIR is exclusive: an existing file or directory is never replaced.
                sftp.create_dir(path).await.map_err(to_error)?;
                Ok(Reply::Done)
            }
            RemoteAction::Rename { from, to } => {
                if from == to {
                    return Ok(Reply::Done);
                }
                if sftp.try_exists(&to).await.map_err(to_error)? {
                    return Err("Destination already exists".into());
                }
                sftp.rename(from, to).await.map_err(to_error)?;
                Ok(Reply::Done)
            }
            RemoteAction::Delete { path, directory } => {
                if directory {
                    sftp.remove_dir(path).await.map_err(to_error)?;
                } else {
                    sftp.remove_file(path).await.map_err(to_error)?;
                }
                Ok(Reply::Done)
            }
            RemoteAction::Upload {
                local,
                remote,
                resume,
                batch,
            } => {
                let name = file_label(&local);
                let key = format!("{id}:upload:{}", crate::agent::random_id());
                let ctx = conflicts::TransferContext::new(events, &key, batch);
                self.transfer(events, key, name, async move {
                    let done = conflicts::upload(sftp, local, remote, resume, false, ctx).await?;
                    Ok(if done {
                        TransferOutcome::Complete(Some(id))
                    } else {
                        TransferOutcome::Skipped
                    })
                })?;
                Ok(Reply::Done)
            }
            RemoteAction::UploadTree {
                local,
                remote,
                batch,
            } => {
                let name = file_label(&local);
                let key = format!("{id}:upload:{}", crate::agent::random_id());
                let ctx = conflicts::TransferContext::new(events, &key, batch);
                self.transfer(events, key, name, async move {
                    let done = conflicts::upload(sftp, local, remote, None, true, ctx).await?;
                    Ok(if done {
                        TransferOutcome::Complete(Some(id))
                    } else {
                        TransferOutcome::Skipped
                    })
                })?;
                Ok(Reply::Done)
            }
            RemoteAction::Download {
                remote,
                local,
                directory,
                options,
                resume,
                batch,
            } => {
                let name = file_label(&local);
                let key = format!("{id}:download:{}", crate::agent::random_id());
                let ctx = conflicts::TransferContext::new(events, &key, batch);
                self.transfer(events, key, name, async move {
                    let done =
                        conflicts::download(sftp, remote, local, directory, options, resume, ctx)
                            .await?;
                    Ok(if done {
                        TransferOutcome::Complete(None)
                    } else {
                        TransferOutcome::Skipped
                    })
                })?;
                Ok(Reply::Done)
            }
        }
    }

    fn transfer(
        &self,
        events: &Events,
        id: String,
        name: String,
        task: impl Future<Output = Result<TransferOutcome, String>> + Send + 'static,
    ) -> Result<(), String> {
        let admission = self.outstanding_transfers.clone().try_acquire_owned()
            .map_err(|_| format!("SFTP transfer queue is full ({MAX_OUTSTANDING_TRANSFERS} active or queued). Try again after transfers finish."))?;
        let slots = self.transfer_slots.clone();
        let events = events.clone();
        tokio::spawn(async move {
            let _admission = admission;
            progress(&events, &id, format!("Queued {name}"), 0.0, false, false).await;
            let Ok(_slot) = slots.acquire_owned().await else {
                return;
            };
            progress(
                &events,
                &id,
                format!("Transferring {name}"),
                0.0,
                false,
                false,
            )
            .await;
            match task.await {
                Ok(TransferOutcome::Complete(changed)) => {
                    progress(
                        &events,
                        &id,
                        format!("Transferred {name}"),
                        100.0,
                        true,
                        false,
                    )
                    .await;
                    if let Some(session) = changed {
                        events
                            .send_async(BackendEvent::RemoteChanged(session))
                            .await;
                    }
                }
                Ok(TransferOutcome::Skipped) => {
                    progress(&events, &id, format!("Skipped {name}"), 0.0, true, false).await;
                }
                Err(error) => {
                    progress(&events, &id, format!("{name}: {error}"), 0.0, true, true).await
                }
            }
        });
        Ok(())
    }
}

enum TransferOutcome {
    Complete(Option<u32>),
    Skipped,
}

// REALPATH and OPENDIR are independent read-only requests for the same
// server path. Overlap them rather than adding a full network round trip
// before each directory listing. Keep the server's canonical path for UI.
async fn list_directory(
    sftp: &SftpSession,
    path: &str,
) -> Result<(String, russh_sftp::client::fs::ReadDir), String> {
    let (canonical, files) = tokio::join!(sftp.canonicalize(path), sftp.read_dir(path));
    let canonical = canonical.map_err(to_error)?;
    // Some SFTP servers accept relative paths for REALPATH but require an
    // absolute path for OPENDIR. Preserve compatibility with those servers.
    let files = match files {
        Ok(files) => files,
        Err(_) if canonical != path => sftp.read_dir(&canonical).await.map_err(to_error)?,
        Err(error) => return Err(to_error(error)),
    };
    Ok((canonical, files))
}

async fn progress(
    events: &Events,
    id: &str,
    text: String,
    percent: f32,
    finished: bool,
    error: bool,
) {
    events
        .send_async(BackendEvent::Progress {
            id: id.into(),
            text,
            percent,
            rate_bps: None,
            finished,
            error,
        })
        .await;
}

// Report recent application-level payload throughput, not the theoretical
// link speed. Resumed bytes are not counted as newly transmitted bytes.
struct UploadMeter<'a> {
    status: Option<(&'a Events, &'a str)>,
    name: String,
    total: u64,
    previous: u64,
    sampled_at: Instant,
}

impl<'a> UploadMeter<'a> {
    fn new(status: Option<(&'a Events, &'a str)>, name: String, total: u64, offset: u64) -> Self {
        Self {
            status,
            name,
            total,
            previous: offset,
            sampled_at: Instant::now(),
        }
    }

    async fn report(&mut self, done: u64, force: bool) {
        let Some((events, id)) = self.status else {
            return;
        };
        let now = Instant::now();
        let elapsed = now.duration_since(self.sampled_at);
        if !force && elapsed < Duration::from_millis(250) {
            return;
        }
        // Don't replace a valid final sample with a second zero-byte sample.
        if done == self.previous && done != 0 {
            return;
        }
        let rate = done.saturating_sub(self.previous) as f64 / elapsed.as_secs_f64().max(0.001);
        transfer_rate(
            events,
            id,
            format!(
                "{} — {} / {}",
                self.name,
                transfer_bytes(done as f64),
                transfer_bytes(self.total as f64)
            ),
            (done as f64 * 100.0 / self.total.max(1) as f64).clamp(0.0, 100.0) as f32,
            rate,
        )
        .await;
        self.previous = done;
        self.sampled_at = now;
    }
}

pub(crate) fn transfer_bytes(bytes: f64) -> String {
    let mut amount = bytes;
    for unit in ["B", "KiB", "MiB", "GiB", "TiB"] {
        if amount < 1024.0 || unit == "TiB" {
            return if unit == "B" {
                format!("{amount:.0} {unit}")
            } else {
                format!("{amount:.1} {unit}")
            };
        }
        amount /= 1024.0;
    }
    unreachable!()
}

async fn transfer_rate(events: &Events, id: &str, text: String, percent: f32, rate: f64) {
    events
        .send_async(BackendEvent::Progress {
            id: id.into(),
            text,
            percent,
            rate_bps: Some(rate),
            finished: false,
            error: false,
        })
        .await;
}

fn to_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

// Drag-and-drop uses exclusive destination creation; never silently replace
// a file, merge an existing directory, or follow symlinks in a source tree.
async fn upload_tree(
    sftp: &SftpSession,
    local: PathBuf,
    remote: String,
    ctx: &conflicts::TransferContext,
) -> Result<(), String> {
    let root_name = file_label(&local);
    let mut pending = vec![(local, remote)];
    let mut count = 0;
    let mut path_bytes = 0usize;
    while let Some((local, remote)) = pending.pop() {
        count += 1;
        if count > MAX_DIRECTORY_ENTRIES {
            return Err("Upload directory tree too large".into());
        }
        let metadata = checked_interactive_metadata(&local).await?;
        if metadata.is_dir() {
            sftp.create_dir(&remote).await.map_err(to_error)?;
            let mut children = tokio::fs::read_dir(&local).await.map_err(to_error)?;
            while let Some(child) = children.next_entry().await.map_err(to_error)? {
                let name = child
                    .file_name()
                    .into_string()
                    .map_err(|_| "Upload path is not Unicode")?;
                let destination = join_remote(&remote, safe_name(&name)?);
                path_bytes =
                    path_bytes.saturating_add(child.path().as_os_str().len() + destination.len());
                if count + pending.len() >= MAX_DIRECTORY_ENTRIES || path_bytes > 32 * 1024 * 1024 {
                    return Err("Upload directory tree too large".into());
                }
                pending.push((child.path(), destination));
            }
        } else if metadata.is_file() {
            // Folder uploads report the current file's progress and rate.
            let name = format!(
                "Uploading {root_name} / {} (current file)",
                file_label(&local)
            );
            conflicts::upload_regular(sftp, &local, &remote, ctx, name).await?;
        } else {
            return Err("Upload source must be a regular file or directory".into());
        }
    }
    Ok(())
}

// The agent path is intentionally separate from the queued interactive
// explorer upload: it must return only after the transfer is confirmed.
pub(super) async fn agent_upload(
    session: &Handle<SshHandler>,
    local: &str,
    remote: &str,
    permit: crate::agent::Permit,
    display: &super::AgentDisplay,
    approved: crate::agent_upload::ManifestSummary,
) -> Result<u64, String> {
    use crate::agent_upload::validate_paths;
    validate_paths(local, remote)?;
    if !permit.valid() {
        return Err("Upload authorization expired or revoked".into());
    }
    let transfer = async {
        display.lock().unwrap().output = b"Rechecking the inspected upload manifest...".to_vec();
        let (entries, summary) = plan_agent_upload(local, remote, &permit).await?;
        if summary != approved {
            return Err(
                "Upload source changed after inspection; submit a new upload request".into(),
            );
        }
        let total = summary.total_bytes;
        display.lock().unwrap().progress = Some((0, total));
        if !permit.valid() {
            return Err("Upload authorization expired or revoked".into());
        }
        let channel = session.channel_open_session().await.map_err(to_error)?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(to_error)?;
        let sftp = Arc::new(
            SftpSession::new(channel.into_stream())
                .await
                .map_err(to_error)?,
        );
        let result = async {
            let mut sent = 0u64;
            for entry in entries {
                if !permit.valid() {
                    return Err("Upload authorization expired or revoked".into());
                }
                display.lock().unwrap().output =
                    format!("Transferring: {}", entry.remote).into_bytes();
                if let Some(size) = entry.size {
                    let mut input = open_agent_source(&entry.local).await?;
                    if input.metadata().await.map_err(to_error)?.len() != size {
                        return Err("Source file size changed since inspection".into());
                    }
                    let count = upload_agent_file(
                        sftp.clone(),
                        &mut input,
                        size,
                        &entry.remote,
                        &permit,
                        display,
                        sent,
                        total,
                    )
                    .await?;
                    sent += count;
                } else {
                    // Require a fresh directory tree; never merge into existing
                    // directories or traverse pre-existing destination symlinks.
                    if !checked_agent_metadata(&entry.local).await?.is_dir() {
                        return Err("Source directory changed since inspection".into());
                    }
                    if !permit.valid() {
                        return Err("Upload authorization expired or revoked".into());
                    }
                    sftp.create_dir(&entry.remote).await.map_err(|e| {
                        format!(
                            "Cannot create new destination directory {}: {e}",
                            entry.remote
                        )
                    })?;
                }
            }
            display.lock().unwrap().output = b"All files and directories transferred.".to_vec();
            Ok(sent)
        }
        .await;
        if result.is_ok() {
            let _ = sftp.close().await;
        }
        result.map_err(|e: String| {
            format!(
                "{e}; completed files and newly created directories may remain, while incomplete file data is kept under hidden staging names"
            )
        })
    };
    tokio::pin!(transfer);
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            biased;
            _ = tick.tick() => if !permit.valid() {
                return Err("Upload timed out or access was revoked; hidden staging files or newly created directories may remain, but incomplete file data is not published under final filenames".into());
            },
            result = &mut transfer => return result,
        }
    }
}

pub(super) async fn inspect_agent_upload(
    local: &str,
    remote: &str,
    permit: &crate::agent::Permit,
) -> Result<crate::agent_upload::ManifestSummary, String> {
    crate::agent_upload::validate_paths(local, remote)?;
    let (_, summary) = plan_agent_upload(local, remote, permit).await?;
    Ok(summary)
}

struct AgentUploadEntry {
    local: String,
    remote: String,
    // None denotes a directory (including empty directories).
    size: Option<u64>,
}

async fn plan_agent_upload(
    local: &str,
    remote: &str,
    permit: &crate::agent::Permit,
) -> Result<(Vec<AgentUploadEntry>, crate::agent_upload::ManifestSummary), String> {
    // Bound manifest memory independently of the amount of file data.
    const MAX_ENTRIES: usize = 100_000;
    const MAX_PATH_BYTES: usize = 32 * 1024 * 1024;
    let mut entries = Vec::new();
    let mut pending = vec![(local.to_owned(), remote.to_owned())];
    let mut path_bytes = local.len() + remote.len();
    let mut total = 0u64;
    while let Some((local, remote)) = pending.pop() {
        if !permit.valid() {
            return Err("Upload inspection cancelled or expired".into());
        }
        crate::agent_upload::validate_paths(&local, &remote)?;
        let metadata = checked_agent_metadata(&local).await?;
        let size = if metadata.is_file() {
            total = total
                .checked_add(metadata.len())
                .ok_or("Upload size overflow")?;
            Some(metadata.len())
        } else if metadata.is_dir() {
            let mut children = tokio::fs::read_dir(&local).await.map_err(to_error)?;
            while let Some(child) = children.next_entry().await.map_err(to_error)? {
                if !permit.valid() {
                    return Err("Upload inspection cancelled or expired".into());
                }
                let name = child
                    .file_name()
                    .into_string()
                    .map_err(|_| "Upload path is not Unicode")?;
                let child_local = child
                    .path()
                    .into_os_string()
                    .into_string()
                    .map_err(|_| "Upload path is not Unicode")?;
                let child_remote = join_remote(&remote, &name);
                crate::agent_upload::validate_paths(&child_local, &child_remote)?;
                path_bytes += child_local.len() + child_remote.len();
                if entries.len() + pending.len() + 2 > MAX_ENTRIES || path_bytes > MAX_PATH_BYTES {
                    return Err(
                        "Folder manifest too large; split the upload into smaller folders".into(),
                    );
                }
                pending.push((child_local, child_remote));
            }
            None
        } else {
            return Err("Upload source must be a regular file or directory".into());
        };
        entries.push(AgentUploadEntry {
            local,
            remote,
            size,
        });
    }
    entries.sort_by(|left, right| left.remote.cmp(&right.remote));
    let mut fingerprint = blake3::Hasher::new();
    let mut file_count = 0;
    let mut directory_count = 0;
    for entry in &entries {
        fingerprint.update(&(entry.local.len() as u64).to_le_bytes());
        fingerprint.update(entry.local.as_bytes());
        fingerprint.update(&(entry.remote.len() as u64).to_le_bytes());
        fingerprint.update(entry.remote.as_bytes());
        if let Some(size) = entry.size {
            file_count += 1;
            fingerprint.update(&[1]);
            fingerprint.update(&size.to_le_bytes());
        } else {
            directory_count += 1;
            fingerprint.update(&[0]);
        }
    }
    Ok((
        entries,
        crate::agent_upload::ManifestSummary {
            file_count,
            directory_count,
            total_bytes: total,
            fingerprint: *fingerprint.finalize().as_bytes(),
        },
    ))
}

// A manual file-picker or drag operation is explicitly initiated by the
// user. Readable files on shared volumes and in group-owned folders are valid
// sources. The stricter agent manifest and ownership checks below still apply
// to all autonomous uploads.
async fn checked_interactive_metadata(path: &Path) -> Result<std::fs::Metadata, String> {
    if !path.is_absolute() {
        return Err("Upload source must be an absolute path".into());
    }
    for ancestor in path.ancestors() {
        let metadata = tokio::fs::symlink_metadata(ancestor)
            .await
            .map_err(to_error)?;
        if metadata.file_type().is_symlink() {
            return Err("Upload source must not use symbolic links".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err("Upload source must not use reparse points".into());
            }
        }
    }
    let metadata = tokio::fs::symlink_metadata(path).await.map_err(to_error)?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err("Upload source must be a regular file or directory".into());
    }
    Ok(metadata)
}

async fn open_interactive_source(path: &Path) -> Result<tokio::fs::File, String> {
    let expected = checked_interactive_metadata(path).await?;
    if !expected.is_file() {
        return Err("Upload source must be a regular file".into());
    }
    let path = path.to_owned();
    let file = tokio::task::spawn_blocking(move || -> Result<std::fs::File, String> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1).custom_flags(0x00200000);
        }
        let file = options.open(path).map_err(to_error)?;
        if !conflicts::same_local_file(&expected, &file.metadata().map_err(to_error)?) {
            return Err("Upload source changed while opening; retry the transfer".into());
        }
        Ok(file)
    })
    .await
    .map_err(to_error)??;
    Ok(tokio::fs::File::from_std(file))
}

async fn checked_agent_metadata(local: &str) -> Result<std::fs::Metadata, String> {
    let path = PathBuf::from(local);
    // Do not follow symlinks, junctions or other reparse points. Opening
    // occurs only after authorization; these checks are not a same-user sandbox.
    for ancestor in path.ancestors().filter(|p| p.has_root()) {
        let metadata = tokio::fs::symlink_metadata(ancestor)
            .await
            .map_err(to_error)?;
        if metadata.file_type().is_symlink() {
            return Err("Upload source must not use symbolic links".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err("Upload source must not use reparse points".into());
            }
        }
    }
    let metadata = tokio::fs::symlink_metadata(&path).await.map_err(to_error)?;
    let protected_directory = if metadata.is_dir() {
        path.as_path()
    } else {
        path.parent()
            .ok_or("Upload source has no parent directory")?
    };
    #[cfg(unix)]
    if !crate::storage::unix_directory_is_private(protected_directory) {
        return Err("Upload source directory can be replaced by another user".into());
    }
    #[cfg(windows)]
    if !crate::storage::windows_directory_is_not_shared_writable(protected_directory) {
        return Err("Upload source directory can be replaced by another Windows account".into());
    }
    Ok(metadata)
}

async fn open_agent_source(local: &str) -> Result<tokio::fs::File, String> {
    if !checked_agent_metadata(local).await?.is_file() {
        return Err("Upload source must be a regular file".into());
    }
    let path = PathBuf::from(local);
    let file = tokio::task::spawn_blocking(move || -> Result<std::fs::File, String> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Deny concurrent writers/deletion while transferring, and do
            // not follow a final-component reparse point.
            options.share_mode(1).custom_flags(0x00200000);
        }
        let file = options.open(path).map_err(to_error)?;
        let metadata = file.metadata().map_err(to_error)?;
        if !metadata.is_file() {
            return Err("Upload source must be a regular file".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let uid = unsafe { libc::geteuid() };
            if (metadata.uid() != uid && metadata.uid() != 0)
                || metadata.mode() & 0o022 != 0
                || metadata.nlink() != 1
            {
                return Err(
                    "Upload source file is writable by another user or has multiple links".into(),
                );
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0
                || !crate::storage::windows_file_is_not_shared_writable(&file)
            {
                return Err("Upload source file is not a private regular file".into());
            }
        }
        Ok(file)
    })
    .await
    .map_err(to_error)??;
    Ok(tokio::fs::File::from_std(file))
}

// Explicit transfer inputs and authorization/progress context.
#[allow(clippy::too_many_arguments)]
async fn upload_agent_file(
    sftp: Arc<SftpSession>,
    input: &mut tokio::fs::File,
    size: u64,
    remote: &str,
    permit: &crate::agent::Permit,
    display: &super::AgentDisplay,
    base: u64,
    total: u64,
) -> Result<u64, String> {
    use russh_sftp::protocol::OpenFlags;
    if !permit.valid() {
        return Err("Upload authorization expired or revoked".into());
    }
    let staging = remote_staging_path(remote);
    // EXCLUDE prevents a random-name collision from opening an existing file.
    // The final path is published only after the complete staging file closes.
    let mut output = sftp
        .open_with_flags(
            &staging,
            OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE,
        )
        .await
        .map_err(|e| format!("Cannot create remote staging file: {e}"))?;
    let mut stage = RemoteUploadStage::new(sftp.clone(), staging);
    let mut sent = 0;
    let mut buffer = vec![0; 32 * 1024];
    let result: Result<u64, String> = async {
        while sent < size {
            if !permit.valid() {
                return Err("Upload authorization expired or revoked".into());
            }
            let limit = (size - sent).min(buffer.len() as u64) as usize;
            let count = input.read(&mut buffer[..limit]).await.map_err(to_error)?;
            if count == 0 {
                return Err("Local file changed or ended unexpectedly".into());
            }
            if !permit.valid() {
                return Err("Upload authorization expired or revoked".into());
            }
            output.write_all(&buffer[..count]).await.map_err(to_error)?;
            sent += count as u64;
            display.lock().unwrap().progress = Some((base + sent, total));
        }
        output.shutdown().await.map_err(to_error)?;
        Ok(sent)
    }
    .await;
    drop(output);
    match result {
        Ok(count) if permit.valid() => {
            match sftp.rename(stage.path.clone(), remote.to_owned()).await {
                Ok(()) => {
                    stage.finished = true;
                    Ok(count)
                }
                Err(error) => {
                    let removed = stage.remove().await;
                    Err(if removed {
                        format!(
                            "Cannot publish completed upload to {remote} (the destination may already exist): {error}"
                        )
                    } else {
                        format!(
                            "Cannot publish completed upload to {remote}: {error}; staging file may remain at {}",
                            stage.path
                        )
                    })
                }
            }
        }
        Ok(_) => {
            let removed = stage.remove().await;
            Err(if removed {
                "Upload authorization expired or revoked; incomplete staging data was discarded"
                    .into()
            } else {
                format!(
                    "Upload authorization expired or revoked; staging file may remain at {}",
                    stage.path
                )
            })
        }
        Err(error) => {
            let removed = stage.remove().await;
            Err(if removed {
                format!("{error}; incomplete staging data was discarded")
            } else {
                format!("{error}; staging file may remain at {}", stage.path)
            })
        }
    }
}

fn remote_staging_path(remote: &str) -> String {
    let parent = remote.rsplit_once('/').map_or("", |(parent, _)| parent);
    join_remote(
        parent,
        &format!(".rterm-upload-{}.part", crate::agent::random_id()),
    )
}

struct RemoteUploadStage {
    sftp: Arc<SftpSession>,
    path: String,
    finished: bool,
}

impl RemoteUploadStage {
    fn new(sftp: Arc<SftpSession>, path: String) -> Self {
        Self {
            sftp,
            path,
            finished: false,
        }
    }

    async fn remove(&mut self) -> bool {
        match self.sftp.remove_file(self.path.clone()).await {
            Ok(()) => {
                self.finished = true;
                true
            }
            Err(_) => false,
        }
    }
}

impl Drop for RemoteUploadStage {
    fn drop(&mut self) {
        if !self.finished
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let sftp = self.sftp.clone();
            let path = self.path.clone();
            runtime.spawn(async move {
                let _ = sftp.remove_file(path).await;
            });
        }
    }
}

async fn download_directory(
    sftp: Arc<SftpSession>,
    remote: String,
    local: PathBuf,
    options: DownloadOptions,
) -> Result<(), String> {
    let parent = local.parent().ok_or("Missing download directory")?;
    #[cfg(unix)]
    if !crate::storage::unix_directory_is_private(parent) {
        return Err(
            "Download directory is not protected against replacement by another user".into(),
        );
    }
    #[cfg(windows)]
    if !crate::storage::windows_directory_is_not_shared_writable(parent) {
        return Err("Download directory is writable by another Windows account".into());
    }
    tokio::fs::create_dir_all(parent).await.map_err(to_error)?;
    #[cfg(unix)]
    if !crate::storage::unix_directory_is_private(parent) {
        return Err(
            "Download directory is not protected against replacement by another user".into(),
        );
    }
    #[cfg(windows)]
    if !crate::storage::windows_directory_is_not_shared_writable(parent) {
        return Err("Download directory is writable by another Windows account".into());
    }
    let mut folders = vec![(remote, local)];
    let mut entry_count = 0;
    while let Some((remote, local)) = folders.pop() {
        // Never merge a server-controlled directory tree into existing local
        // files, symlinks, or junctions. Each directory must be newly created.
        tokio::fs::create_dir(&local).await.map_err(|error| {
            format!(
                "Cannot create download directory {}: {error}",
                local.display()
            )
        })?;
        #[cfg(unix)]
        if !crate::storage::unix_directory_is_private(&local) {
            return Err("Created download directory is writable by another user".into());
        }
        #[cfg(windows)]
        if !crate::storage::windows_directory_is_not_shared_writable(&local) {
            return Err("Created download directory is writable by another Windows account".into());
        }
        for entry in sftp.read_dir(&remote).await.map_err(to_error)? {
            entry_count += 1;
            if entry_count > MAX_DIRECTORY_ENTRIES {
                return Err(format!(
                    "Remote directory tree exceeds {MAX_DIRECTORY_ENTRIES} entries"
                ));
            }
            let name = entry.file_name();
            if name.len() > MAX_ENTRY_NAME_BYTES {
                return Err("Remote filename exceeds 4096 bytes".into());
            }
            if matches!(name.as_str(), "." | "..") {
                continue;
            }
            let path = local.join(safe_name(&name)?);
            let child = join_remote(&remote, &name);
            if entry.file_type().is_dir() {
                folders.push((child, path));
            } else if entry.file_type().is_file() {
                // Duplicate names, including case-folded collisions on
                // Windows, must not silently replace an earlier entry.
                match tokio::fs::symlink_metadata(&path).await {
                    Ok(_) => {
                        return Err(format!("Download entry already exists: {}", path.display()));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(to_error(error)),
                }
                download_file(sftp.clone(), &child, &path, options, None, None).await?;
            }
        }
    }
    Ok(())
}

pub(crate) fn join_remote(directory: &str, name: &str) -> String {
    format!("{}/{name}", directory.trim_end_matches('/'))
}

pub(crate) fn safe_name(name: &str) -> Result<&str, String> {
    let mut parts = Path::new(name).components();
    if name.is_empty()
        || name.len() > MAX_ENTRY_NAME_BYTES
        || name.contains(['/', '\\'])
        || crate::agent_upload::has_unsafe_text(name)
        || (cfg!(windows) && crate::agent_upload::invalid_windows_name(name))
        || !matches!(parts.next(), Some(Component::Normal(_)))
        || parts.next().is_some()
    {
        return Err(format!("Invalid filename: {name}"));
    }
    Ok(name)
}

async fn download_file(
    sftp: Arc<SftpSession>,
    remote: &str,
    local: &Path,
    options: DownloadOptions,
    status: Option<(&Events, &str)>,
    resume: Option<&str>,
) -> Result<(), String> {
    download_file_with_replace(sftp, remote, local, options, status, resume, None).await
}

async fn download_file_with_replace(
    sftp: Arc<SftpSession>,
    remote: &str,
    local: &Path,
    options: DownloadOptions,
    status: Option<(&Events, &str)>,
    resume: Option<&str>,
    replace: Option<std::fs::Metadata>,
) -> Result<(), String> {
    let metadata = sftp.metadata(remote).await.map_err(to_error)?;
    let size = metadata.len();
    let directory = local.parent().ok_or("Missing download directory")?;
    tokio::fs::create_dir_all(directory)
        .await
        .map_err(to_error)?;
    // Validate the user-selected path before resolving it. Canonicalizing
    // first would discard a hostile symlink/junction ancestor from the checks.
    #[cfg(windows)]
    if !crate::storage::windows_directory_is_not_shared_writable(directory) {
        return Err("Download directory is writable by another Windows account".into());
    }
    #[cfg(unix)]
    if !crate::storage::unix_directory_is_private(directory) {
        return Err(
            "Download directory is not protected against replacement by another user".into(),
        );
    }
    // Resolve directory aliases once and use the resulting destination for
    // staging and publication. Reusing the caller's path after a long transfer
    // would let a retargeted symlink/junction redirect the final rename.
    let directory = std::fs::canonicalize(directory).map_err(to_error)?;
    let local = directory.join(local.file_name().ok_or("Missing download name")?);
    #[cfg(windows)]
    if !crate::storage::windows_directory_is_not_shared_writable(
        local.parent().ok_or("Missing download directory")?,
    ) {
        return Err("Download directory is writable by another Windows account".into());
    }
    #[cfg(unix)]
    if !crate::storage::unix_directory_is_private(&directory) {
        return Err(
            "Download directory is not protected against replacement by another user".into(),
        );
    }

    // Each worker owns a file handle and writes a disjoint range. Publish the
    // temporary file only after all ranges complete to avoid exposing partial downloads.
    let count = if size < 1024 * 1024 {
        1
    } else {
        options.parallel.clamp(1, 6) as u64
    };
    let (resumable, temporary, staging, work) = if let Some(peer) = resume {
        let (download, ranges, offsets) =
            recovery::Download::open(&sftp, remote, &local, peer, count, size).await?;
        let work = ranges
            .into_iter()
            .zip(offsets)
            .map(|((start, end), offset)| (start, offset, end))
            .collect::<Vec<_>>();
        let path = download.part.clone();
        (Some(Arc::new(download)), None, path, work)
    } else {
        let file = tempfile::NamedTempFile::new_in(&directory).map_err(to_error)?;
        #[cfg(windows)]
        if !crate::storage::windows_file_is_not_shared_writable(file.as_file()) {
            return Err("Download staging file is writable by another Windows account".into());
        }
        file.as_file().set_len(size).map_err(to_error)?;
        let range_size = size.div_ceil(count);
        let work = (0..count)
            .filter_map(|index| {
                let start = index * range_size;
                let end = start.saturating_add(range_size).min(size);
                (start < end).then_some((start, start, end))
            })
            .collect();
        let path = file.path().to_owned();
        (None, Some(file), path, work)
    };
    let per_worker = if options.max_bytes_per_second > 0 {
        (options.max_bytes_per_second / work.len().max(1) as u64).max(1)
    } else {
        0
    };
    let resumed = work.iter().map(|&(start, offset, _)| offset - start).sum();
    let bytes_done = Arc::new(AtomicU64::new(resumed));
    let started = Instant::now();
    let mut workers = JoinSet::new();
    for (index, (_, start, end)) in work.into_iter().enumerate() {
        if start == end {
            continue;
        }
        let range = RangeCopy {
            sftp: sftp.clone(),
            remote: remote.to_owned(),
            staging: staging.clone(),
            start,
            end,
            bytes_done: bytes_done.clone(),
            max_rate: per_worker,
            checkpoint: resumable.as_ref().map(|download| (download.clone(), index)),
        };
        workers.spawn(range.run());
    }

    let mut ticker = tokio::time::interval(Duration::from_millis(300));
    while !workers.is_empty() {
        tokio::select! {
            entry = workers.join_next() => {
                if let Some(entry) = entry
                    && let Err(error) = entry.map_err(to_error).and_then(|result| result)
                {
                    workers.shutdown().await;
                    return Err(if resumable.is_some() { format!("{error}. Repeat the same download to resume.") } else { error });
                }
            }
            _ = ticker.tick(), if status.is_some() => {
                let (events, id) = status.unwrap();
                let done = bytes_done.load(Ordering::Relaxed);
                let percent = done as f32 * 100.0 / size.max(1) as f32;
                let speed = (done - resumed) as f64 / started.elapsed().as_secs_f64().max(0.1);
                let text = format!("{} — {} / {}", file_label(&local), transfer_bytes(done as f64), transfer_bytes(size as f64));
                transfer_rate(events, id, text, percent, speed).await;
            }
        }
    }
    if options.timestamps {
        let mut times = std::fs::FileTimes::new();
        if let Some(seconds) = metadata.mtime {
            times = times.set_modified(UNIX_EPOCH + Duration::from_secs(seconds.into()));
        }
        if let Some(seconds) = metadata.atime {
            times = times.set_accessed(UNIX_EPOCH + Duration::from_secs(seconds.into()));
        }
        if let Some(download) = &resumable {
            download.set_times(times)?;
        } else if let Some(temporary) = &temporary {
            temporary.as_file().set_times(times).map_err(to_error)?;
        }
    }
    if let Some(download) = resumable {
        download.publish(&sftp, remote).await?;
    } else if let Some(mut temporary) = temporary {
        temporary.flush().map_err(to_error)?;
        if let Some(expected) = replace {
            let target = std::fs::symlink_metadata(&local).map_err(to_error)?;
            if !conflicts::same_local_file(&expected, &target) {
                return Err("Replacement destination changed; retry to confirm again".into());
            }
            temporary
                .persist(&local)
                .map_err(|error| to_error(error.error))?;
            return Ok(());
        }
        temporary.persist_noclobber(&local).map_err(|error| {
            if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                "Download destination already exists; choose another folder or remove it first"
                    .into()
            } else {
                to_error(error.error)
            }
        })?;
    }
    Ok(())
}

struct RangeCopy {
    sftp: Arc<SftpSession>,
    remote: String,
    staging: PathBuf,
    start: u64,
    end: u64,
    bytes_done: Arc<AtomicU64>,
    max_rate: u64,
    checkpoint: Option<(Arc<recovery::Download>, usize)>,
}

impl RangeCopy {
    async fn run(self) -> Result<(), String> {
        let mut input = self.sftp.open(&self.remote).await.map_err(to_error)?;
        let mut output = tokio::fs::File::from_std(recovery::open_staging(
            &self.staging,
            false,
            true,
            false,
            false,
        )?);
        if self.start > 0 {
            input
                .seek(SeekFrom::Start(self.start))
                .await
                .map_err(to_error)?;
            output
                .seek(SeekFrom::Start(self.start))
                .await
                .map_err(to_error)?;
        }
        let start_time = Instant::now();
        let mut buffer = vec![0; 256 * 1024];
        let mut position = self.start;
        let mut saved = position;
        while position < self.end {
            let limit = (self.end - position).min(buffer.len() as u64) as usize;
            let count = input.read(&mut buffer[..limit]).await.map_err(to_error)?;
            if count == 0 {
                return Err("The remote file ended before its advertised size".into());
            }
            output.write_all(&buffer[..count]).await.map_err(to_error)?;
            position += count as u64;
            self.bytes_done.fetch_add(count as u64, Ordering::Relaxed);
            if let Some((journal, index)) = &self.checkpoint
                && (position == self.end || position - saved >= recovery::CHECKPOINT_BYTES)
            {
                output.sync_data().await.map_err(to_error)?;
                journal.checkpoint(*index, position).await?;
                saved = position;
            }
            if self.max_rate > 0 {
                let target =
                    Duration::from_secs_f64((position - self.start) as f64 / self.max_rate as f64);
                tokio::time::sleep(target.saturating_sub(start_time.elapsed())).await;
            }
        }
        output.flush().await.map_err(to_error)
    }
}

#[cfg(test)]
#[path = "../../tests/unit/backend/sftp_listing_tests.rs"]
mod listing_tests;

#[cfg(test)]
#[path = "../../tests/unit/backend/sftp_agent_upload_source_tests.rs"]
mod agent_upload_source_tests;

#[cfg(test)]
#[path = "../../tests/unit/backend/sftp_transfer_queue.rs"]
mod transfer_queue_tests;
