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

use russh::client::Handle;
use russh_sftp::client::SftpSession;
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::Semaphore,
    task::JoinSet,
};

use super::{BackendEvent, BackendResult, Events, Reply, connection::SshHandler};

mod recovery;

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
    },
    Download {
        remote: String,
        local: PathBuf,
        directory: bool,
        options: DownloadOptions,
        resume: Option<String>,
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
}

impl Default for Service {
    fn default() -> Self {
        Self {
            sessions: HashMap::new(),
            transfer_slots: Arc::new(Semaphore::new(8)),
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
                let entries = files
                    .map(|file| Entry {
                        name: file.file_name(),
                        directory: file.file_type().is_dir(),
                        size: file.metadata().len(),
                        permissions: file.metadata().permissions.map(|mode| mode & 0o777),
                        modified: file
                            .metadata()
                            .mtime
                            .and_then(|seconds| {
                                chrono::DateTime::from_timestamp(i64::from(seconds), 0)
                            })
                            .map(|time| time.with_timezone(&chrono::Local)),
                    })
                    .collect();
                Ok(Reply::Files { path, entries })
            }
            RemoteAction::Rename { from, to } => {
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
            } => {
                let name = file_label(&local);
                self.transfer(events, format!("{id}:upload:{remote}"), name, async move {
                    if let Some(peer) = resume {
                        recovery::upload(&sftp, &local, &remote, &peer).await?;
                    } else {
                        upload(&sftp, &local, &remote).await?;
                    }
                    Ok(Some(id))
                });
                Ok(Reply::Done)
            }
            RemoteAction::Download {
                remote,
                local,
                directory,
                options,
                resume,
            } => {
                let key = format!("{id}:download:{}", local.display());
                let name = file_label(&local);
                let transfer_events = events.clone();
                let progress_key = key.clone();
                self.transfer(events, key, name, async move {
                    if directory {
                        download_directory(sftp, remote, local, options).await?;
                    } else {
                        download_file(
                            sftp,
                            &remote,
                            &local,
                            options,
                            Some((&transfer_events, &progress_key)),
                            resume.as_deref(),
                        )
                        .await?;
                    }
                    Ok(None)
                });
                Ok(Reply::Done)
            }
        }
    }

    fn transfer(
        &self,
        events: &Events,
        id: String,
        name: String,
        task: impl Future<Output = Result<Option<u32>, String>> + Send + 'static,
    ) {
        let slots = self.transfer_slots.clone();
        let events = events.clone();
        tokio::spawn(async move {
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
                Ok(changed) => {
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
                Err(error) => {
                    progress(&events, &id, format!("{name}: {error}"), 100.0, true, true).await
                }
            }
        });
    }
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
            finished,
            error,
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

async fn upload(sftp: &SftpSession, local: &Path, remote: &str) -> Result<(), String> {
    let mut input = tokio::fs::File::open(local).await.map_err(to_error)?;
    let mut output = sftp.create(remote).await.map_err(to_error)?;
    tokio::io::copy(&mut input, &mut output)
        .await
        .map_err(to_error)?;
    output.shutdown().await.map_err(to_error)
}

// The agent path is intentionally separate from the queued interactive
// explorer upload: it must return only after the transfer is confirmed.
pub(super) async fn agent_upload(
    session: &Handle<SshHandler>,
    local: &str,
    remote: &str,
    permit: crate::agent::Permit,
    display: &super::AgentDisplay,
) -> Result<u64, String> {
    use crate::agent_upload::validate_paths;
    validate_paths(local, remote)?;
    if !permit.valid() {
        return Err("Upload authorization expired or revoked".into());
    }
    let transfer = async {
        display.lock().unwrap().output =
            b"Inspecting source and calculating transfer size...".to_vec();
        let (entries, total) = plan_agent_upload(local, remote, &permit).await?;
        display.lock().unwrap().progress = Some((0, total));
        if !permit.valid() {
            return Err("Upload authorization expired or revoked".into());
        }
        let channel = session.channel_open_session().await.map_err(to_error)?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(to_error)?;
        let sftp = SftpSession::new(channel.into_stream())
            .await
            .map_err(to_error)?;
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
                        &sftp,
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
        let _ = sftp.close().await;
        result.map_err(|e: String| format!("{e}; destination may contain partial results"))
    };
    tokio::pin!(transfer);
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            biased;
            _ = tick.tick() => if !permit.valid() {
                return Err("Upload timed out or access was revoked; remote file may be incomplete".into());
            },
            result = &mut transfer => return result,
        }
    }
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
) -> Result<(Vec<AgentUploadEntry>, u64), String> {
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
    Ok((entries, total))
}

async fn checked_agent_metadata(local: &str) -> Result<std::fs::Metadata, String> {
    let path = PathBuf::from(local);
    // Do not follow symlinks, junctions or other reparse points. Opening
    // occurs only after approval; these checks are not a same-user sandbox.
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
    tokio::fs::symlink_metadata(&path).await.map_err(to_error)
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
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err("Upload source must not use reparse points".into());
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
    sftp: &SftpSession,
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
    // EXCLUDE is the protocol-level no-overwrite guarantee, not a
    // racy check for existence followed by a truncating open.
    let mut output = sftp
        .open_with_flags(
            remote,
            OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE,
        )
        .await
        .map_err(|e| {
            format!("Cannot create destination (existing files are not overwritten): {e}")
        })?;
    let mut sent = 0;
    let mut buffer = vec![0; 32 * 1024];
    let result = async {
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
    result.map_err(|e: String| format!("{e}; remote file may be incomplete"))
}

async fn download_directory(
    sftp: Arc<SftpSession>,
    remote: String,
    local: PathBuf,
    options: DownloadOptions,
) -> Result<(), String> {
    let parent = local.parent().ok_or("Missing download directory")?;
    tokio::fs::create_dir_all(parent).await.map_err(to_error)?;
    let mut folders = vec![(remote, local)];
    while let Some((remote, local)) = folders.pop() {
        // Never merge a server-controlled directory tree into existing local
        // files, symlinks, or junctions. Each directory must be newly created.
        tokio::fs::create_dir(&local).await.map_err(|error| {
            format!(
                "Cannot create download directory {}: {error}",
                local.display()
            )
        })?;
        for entry in sftp.read_dir(&remote).await.map_err(to_error)? {
            let name = entry.file_name();
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
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
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
    let metadata = sftp.metadata(remote).await.map_err(to_error)?;
    let size = metadata.len();
    let directory = local.parent().ok_or("Missing download directory")?;
    tokio::fs::create_dir_all(directory)
        .await
        .map_err(to_error)?;

    // Each worker owns a file handle and writes a disjoint range. Publish the
    // temporary file only after all ranges complete to avoid exposing partial downloads.
    let count = if size < 1024 * 1024 {
        1
    } else {
        options.parallel.clamp(1, 6) as u64
    };
    let (resumable, temporary, staging, work) = if let Some(peer) = resume {
        let (download, ranges, offsets) =
            recovery::Download::open(&sftp, remote, local, peer, count, size).await?;
        let work = ranges
            .into_iter()
            .zip(offsets)
            .map(|((start, end), offset)| (start, offset, end))
            .collect::<Vec<_>>();
        let path = download.part.clone();
        (Some(Arc::new(download)), None, path, work)
    } else {
        let file = tempfile::NamedTempFile::new_in(directory).map_err(to_error)?;
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
                let mib = 1024.0 * 1024.0;
                let text = format!("{}: {:.1} MB / {:.1} MB @ {:.1} MB/s", file_label(local), done as f64 / mib, size as f64 / mib, speed / mib);
                progress(events, id, text, percent, false, false).await;
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
        std::fs::OpenOptions::new()
            .write(true)
            .open(&staging)
            .map_err(to_error)?
            .set_times(times)
            .map_err(to_error)?;
    }
    if let Some(download) = resumable {
        download.publish(&sftp, remote, local).await?;
    } else if let Some(mut temporary) = temporary {
        temporary.flush().map_err(to_error)?;
        temporary.persist(local).map_err(to_error)?;
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
        let mut output = tokio::fs::OpenOptions::new()
            .write(true)
            .open(&self.staging)
            .await
            .map_err(to_error)?;
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
mod listing_tests {
    use super::*;
    use russh_sftp::{
        protocol::{File, Handle, Name, Status, StatusCode},
        server,
    };

    struct DirectoryFixture {
        batch: usize,
    }

    impl server::Handler for DirectoryFixture {
        type Error = StatusCode;
        fn unimplemented(&self) -> Self::Error {
            StatusCode::OpUnsupported
        }
        async fn realpath(&mut self, id: u32, _: String) -> Result<Name, Self::Error> {
            Ok(Name {
                id,
                files: vec![File::dummy("/fixture")],
            })
        }
        async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
            assert!(path == "." || path == "/fixture");
            self.batch = 0;
            Ok(Handle {
                id,
                handle: "dir".into(),
            })
        }
        async fn readdir(&mut self, id: u32, _: String) -> Result<Name, Self::Error> {
            if self.batch == 3 {
                return Err(StatusCode::Eof);
            }
            let first = self.batch * 100;
            self.batch += 1;
            Ok(Name {
                id,
                files: (first..first + 100)
                    .map(|i| File::dummy(format!("file-{i:03}")))
                    .collect(),
            })
        }
        async fn close(&mut self, id: u32, _: String) -> Result<Status, Self::Error> {
            Ok(Status {
                id,
                status_code: StatusCode::Ok,
                error_message: String::new(),
                language_tag: String::new(),
            })
        }
    }

    // Simulate a network link rather than delaying the serial SFTP handler:
    // concurrently submitted packets can travel in the same latency window.
    async fn delayed_link(
        mut read: impl tokio::io::AsyncRead + Unpin,
        mut write: impl tokio::io::AsyncWrite + Unpin,
        delay: Duration,
    ) {
        let mut buf = vec![0; 256 * 1024];
        while let Ok(mut count) = read.read(&mut buf).await {
            if count == 0 {
                break;
            }
            while count < buf.len() {
                match tokio::time::timeout(Duration::from_millis(1), read.read(&mut buf[count..]))
                    .await
                {
                    Ok(Ok(n)) if n > 0 => count += n,
                    _ => break,
                }
            }
            tokio::time::sleep(delay).await;
            if write.write_all(&buf[..count]).await.is_err() {
                break;
            }
        }
        let _ = write.shutdown().await;
    }

    #[tokio::test]
    async fn listing_returns_all_batches_and_canonical_path() {
        let (client, stream) = tokio::io::duplex(1024 * 1024);
        server::run(stream, DirectoryFixture { batch: 0 }).await;
        let session = SftpSession::new(client).await.unwrap();
        let (path, entries) = list_directory(&session, ".").await.unwrap();
        let mut names: Vec<_> = entries.map(|entry| entry.file_name()).collect();
        names.sort();
        assert_eq!(path, "/fixture");
        assert_eq!(
            names,
            (0..300).map(|i| format!("file-{i:03}")).collect::<Vec<_>>()
        );
        session.close().await.unwrap();
    }

    #[test]
    fn rejects_unsafe_download_names() {
        for name in [
            "",
            ".",
            "..",
            "../escape",
            "dir/file",
            "dir\\file",
            "a\0b",
            "a\nb",
        ] {
            assert!(safe_name(name).is_err(), "{name:?}");
        }
        for name in [
            "NUL",
            "con.txt",
            "COM1",
            "LPT².log",
            "file:stream",
            "file.",
            "file ",
            "a?b",
        ] {
            assert!(crate::agent_upload::invalid_windows_name(name), "{name:?}");
        }
        assert_eq!(safe_name(".config").unwrap(), ".config");
        assert_eq!(safe_name("file name.txt").unwrap(), "file name.txt");
    }

    #[tokio::test]
    #[ignore = "manual SFTP latency benchmark"]
    async fn listing_latency_benchmark() {
        let (client, proxy_client) = tokio::io::duplex(1024 * 1024);
        let (proxy_server, stream) = tokio::io::duplex(1024 * 1024);
        let (cr, cw) = tokio::io::split(proxy_client);
        let (sr, sw) = tokio::io::split(proxy_server);
        let delay = Duration::from_millis(20);
        tokio::spawn(delayed_link(cr, sw, delay));
        tokio::spawn(delayed_link(sr, cw, delay));
        server::run(stream, DirectoryFixture { batch: 0 }).await;
        let start = Instant::now();
        let session = SftpSession::new(client).await.unwrap();
        let init = start.elapsed();
        let mut sequential = Vec::new();
        let mut overlapped = Vec::new();
        for _ in 0..5 {
            let start = Instant::now();
            let path = session.canonicalize(".").await.unwrap();
            let mut old: Vec<_> = session
                .read_dir(&path)
                .await
                .unwrap()
                .map(|f| f.file_name())
                .collect();
            sequential.push(start.elapsed());
            let start = Instant::now();
            let (path, entries) = list_directory(&session, ".").await.unwrap();
            let mut new: Vec<_> = entries.map(|f| f.file_name()).collect();
            overlapped.push(start.elapsed());
            assert_eq!(path, "/fixture");
            old.sort();
            new.sort();
            assert_eq!(new, old);
            assert_eq!(new.len(), 300);
        }
        sequential.sort();
        overlapped.sort();
        println!(
            "SFTP benchmark (40ms simulated RTT, 300 entries, median of 5): init={:.1}ms sequential={:.1}ms overlapped={:.1}ms",
            init.as_secs_f64() * 1000.0,
            sequential[2].as_secs_f64() * 1000.0,
            overlapped[2].as_secs_f64() * 1000.0
        );
        session.close().await.unwrap();
    }
}
