//! Interactive file-manager conflict handling. Agent uploads use their own
//! authorization and exclusive publication path; they never use these choices.
use super::*;
use russh_sftp::{
    client::error::Error,
    protocol::{FileAttributes, StatusCode},
};
use tokio::sync::{Mutex, oneshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConflictChoice {
    Replace,
    Skip,
    KeepBoth,
}

#[derive(Clone, Default)]
pub(crate) struct ConflictBatch(Arc<Mutex<Option<ConflictChoice>>>);

pub(crate) struct ConflictPrompt {
    pub(crate) source: String,
    pub(crate) destination: String,
    pub(crate) can_replace: bool,
    reply: Option<oneshot::Sender<(ConflictChoice, bool)>>,
}

impl ConflictPrompt {
    pub(crate) fn is_closed(&self) -> bool {
        self.reply.as_ref().is_none_or(oneshot::Sender::is_closed)
    }
    pub(crate) fn decide(&mut self, choice: ConflictChoice, remember: bool) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.send((choice, remember));
        }
    }
}

pub(super) struct TransferContext {
    events: Events,
    key: String,
    batch: ConflictBatch,
}

impl TransferContext {
    pub(super) fn new(events: &Events, key: &str, batch: ConflictBatch) -> Self {
        Self {
            events: events.clone(),
            key: key.into(),
            batch,
        }
    }

    async fn choose(
        &self,
        source: String,
        destination: String,
        can_replace: bool,
    ) -> ConflictChoice {
        // Serialize decisions in the batch so "remember" also applies to
        // conflicts already queued by parallel workers.
        let mut remembered = self.batch.0.lock().await;
        if let Some(choice) = *remembered
            && (choice != ConflictChoice::Replace || can_replace)
        {
            return choice;
        }
        progress(
            &self.events,
            &self.key,
            format!("Waiting for conflict decision: {destination}"),
            0.0,
            false,
            false,
        )
        .await;
        let (reply, receive) = oneshot::channel();
        self.events
            .send_async(BackendEvent::TransferConflict(ConflictPrompt {
                source,
                destination,
                can_replace,
                reply: Some(reply),
            }))
            .await;
        let (choice, remember) = tokio::time::timeout(Duration::from_secs(600), receive)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or((ConflictChoice::Skip, false));
        let choice = if choice == ConflictChoice::Replace && !can_replace {
            ConflictChoice::Skip
        } else {
            choice
        };
        if remember {
            *remembered = Some(choice);
        }
        choice
    }

    async fn transferring(&self, name: &str) {
        progress(
            &self.events,
            &self.key,
            format!("Transferring {name}"),
            0.0,
            false,
            false,
        )
        .await;
    }
}

async fn remote_info(sftp: &SftpSession, path: &str) -> Result<Option<FileAttributes>, String> {
    match sftp.symlink_metadata(path).await {
        Ok(info) => Ok(Some(info)),
        Err(Error::Status(status)) if status.status_code == StatusCode::NoSuchFile => Ok(None),
        Err(error) => Err(to_error(error)),
    }
}

async fn local_info(path: &Path) -> Result<Option<std::fs::Metadata>, String> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(info) => Ok(Some(info)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(to_error(error)),
    }
}

pub(super) fn same_local_file(expected: &std::fs::Metadata, actual: &std::fs::Metadata) -> bool {
    if !expected.is_file()
        || !actual.is_file()
        || expected.len() != actual.len()
        || expected.modified().ok() != actual.modified().ok()
    {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if expected.dev() != actual.dev()
            || expected.ino() != actual.ino()
            || expected.ctime() != actual.ctime()
            || expected.ctime_nsec() != actual.ctime_nsec()
        {
            return false;
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if actual.file_attributes() & 0x400 != 0
            || expected.creation_time() != actual.creation_time()
        {
            return false;
        }
    }
    true
}

fn copy_name(name: &str, index: usize, directory: bool) -> String {
    if !directory
        && let Some((stem, ext)) = name.rsplit_once('.')
        && !stem.is_empty()
    {
        return format!("{stem} ({index}).{ext}");
    }
    format!("{name} ({index})")
}

async fn unused_remote(sftp: &SftpSession, path: &str, directory: bool) -> Result<String, String> {
    let (parent, name) = path.rsplit_once('/').ok_or("Missing remote parent")?;
    for index in 1..=10_000 {
        let name = copy_name(name, index, directory);
        let candidate = join_remote(parent, safe_name(&name)?);
        if remote_info(sftp, &candidate).await?.is_none() {
            return Ok(candidate);
        }
    }
    Err("No available destination name".into())
}

async fn unused_local(path: &Path, directory: bool) -> Result<PathBuf, String> {
    let parent = path.parent().ok_or("Missing destination parent")?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("Invalid filename")?;
    for index in 1..=10_000 {
        let name = copy_name(name, index, directory);
        let candidate = parent.join(safe_name(&name)?);
        if local_info(&candidate).await?.is_none() {
            return Ok(candidate);
        }
    }
    Err("No available destination name".into())
}

pub(super) async fn upload(
    sftp: Arc<SftpSession>,
    local: PathBuf,
    mut remote: String,
    resume: Option<String>,
    tree: bool,
    ctx: TransferContext,
) -> Result<bool, String> {
    let source = checked_interactive_metadata(&local).await?;
    if let Some(existing) = remote_info(&sftp, &remote).await? {
        match ctx
            .choose(
                local.display().to_string(),
                remote.clone(),
                source.is_file() && existing.file_type().is_file(),
            )
            .await
        {
            ConflictChoice::Skip => return Ok(false),
            ConflictChoice::KeepBoth => {
                remote = unused_remote(&sftp, &remote, source.is_dir()).await?
            }
            ConflictChoice::Replace => {
                ctx.transferring(&file_label(&local)).await;
                replace_remote(sftp, local, &remote, existing, &ctx).await?;
                return Ok(true);
            }
        }
    }
    ctx.transferring(&file_label(&local)).await;
    if source.is_dir() {
        if !tree {
            return Err("Choose a regular file to upload".into());
        }
        upload_tree(&sftp, local, remote, &ctx).await?;
    } else if let Some(peer) = resume {
        recovery::upload(&sftp, &local, &remote, &peer, Some((&ctx.events, &ctx.key))).await?;
    } else {
        // Exclusive creation prevents late collisions from truncating files.
        upload_regular(
            &sftp,
            &local,
            &remote,
            &ctx,
            format!("Uploading {}", file_label(&local)),
        )
        .await?;
    }
    Ok(true)
}

pub(super) async fn upload_regular(
    sftp: &SftpSession,
    local: &Path,
    remote: &str,
    ctx: &TransferContext,
    name: String,
) -> Result<(), String> {
    use russh_sftp::protocol::OpenFlags;
    let mut input = open_interactive_source(local).await?;
    let total = input.metadata().await.map_err(to_error)?.len();
    let mut output = sftp
        .open_with_flags(
            remote,
            OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
        )
        .await
        .map_err(to_error)?;
    let result = async {
        let mut buffer = vec![0; 128 * 1024];
        let mut sent = 0u64;
        let mut meter = UploadMeter::new(Some((&ctx.events, &ctx.key)), name, total, 0);
        loop {
            let n = input.read(&mut buffer).await.map_err(to_error)?;
            if n == 0 {
                break;
            }
            output.write_all(&buffer[..n]).await.map_err(to_error)?;
            sent += n as u64;
            meter.report(sent, false).await;
        }
        if sent != total {
            return Err("Upload source changed during transfer".into());
        }
        output.shutdown().await.map_err(to_error)?;
        meter.report(sent, true).await;
        Ok(())
    }
    .await;
    drop(output);
    if result.is_err() {
        let _ = sftp.remove_file(remote).await;
    }
    result
}

async fn replace_remote(
    sftp: Arc<SftpSession>,
    local: PathBuf,
    remote: &str,
    existing: FileAttributes,
    ctx: &TransferContext,
) -> Result<(), String> {
    // Stage the complete new file before touching the original. SFTP v3
    // rename does not overwrite; preserve the original under a unique backup
    // name until publication succeeds. Never delete an unapproved target.
    let stage_path = remote_staging_path(remote);
    upload_regular(
        &sftp,
        &local,
        &stage_path,
        ctx,
        format!("Uploading {}", file_label(&local)),
    )
    .await?;
    let mut stage = RemoteUploadStage::new(sftp.clone(), stage_path.clone());
    let current = remote_info(&sftp, remote)
        .await?
        .ok_or("Replacement destination disappeared")?;
    if !current.file_type().is_file()
        || current.len() != existing.len()
        || current.mtime != existing.mtime
    {
        return Err("Replacement destination changed; retry to confirm again".into());
    }
    let (parent, _) = remote.rsplit_once('/').ok_or("Missing remote parent")?;
    let backup = join_remote(
        parent,
        &format!(".rterm-replaced-{}.bak", crate::agent::random_id()),
    );
    sftp.rename(remote, &backup).await.map_err(|error| {
        format!("Could not move original to {backup}; check both paths before retrying: {error}")
    })?;
    if let Err(error) = sftp.rename(&stage_path, remote).await {
        let restored = sftp.rename(&backup, remote).await.is_ok();
        return Err(if restored {
            format!("Replacement failed; original restored: {error}")
        } else {
            format!("Replacement failed; original preserved at {backup}: {error}")
        });
    }
    stage.finished = true;
    sftp.remove_file(&backup).await.map_err(|error| {
        format!("Replacement uploaded; original backup remains at {backup}: {error}")
    })?;
    Ok(())
}

pub(super) async fn download(
    sftp: Arc<SftpSession>,
    remote: String,
    mut local: PathBuf,
    directory: bool,
    options: DownloadOptions,
    resume: Option<String>,
    ctx: TransferContext,
) -> Result<bool, String> {
    let mut replace = None;
    if let Some(existing) = local_info(&local).await? {
        let regular_source = !directory
            && remote_info(&sftp, &remote)
                .await?
                .is_some_and(|metadata| metadata.file_type().is_file());
        match ctx
            .choose(
                remote.clone(),
                local.display().to_string(),
                regular_source && existing.is_file(),
            )
            .await
        {
            ConflictChoice::Skip => return Ok(false),
            ConflictChoice::KeepBoth => local = unused_local(&local, directory).await?,
            ConflictChoice::Replace => replace = Some(existing),
        }
    }
    ctx.transferring(&file_label(&local)).await;
    if directory {
        download_directory(sftp, remote, local, options).await?;
    } else {
        download_file_with_replace(
            sftp,
            &remote,
            &local,
            options,
            Some((&ctx.events, &ctx.key)),
            if replace.is_some() {
                None
            } else {
                resume.as_deref()
            },
            replace,
        )
        .await?;
    }
    ctx.events.send_async(BackendEvent::LocalChanged).await;
    Ok(true)
}
