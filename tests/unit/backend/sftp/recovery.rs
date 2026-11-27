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

#[cfg(unix)]
#[test]
fn staging_and_recovery_reject_fifos() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let dir = crate::storage::test_private_tempdir();
    let fifo = dir.path().join("staging.fifo");
    let path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert!(open_staging(&fifo, true, false, false, false).is_err());
    assert!(open_staging(&fifo, false, true, false, true).is_err());
    assert!(open_staging(&fifo, true, true, false, false).is_err());
    assert!(read::<serde_json::Value>(&fifo).is_err());
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

    async fn rename(&mut self, id: u32, old: String, new: String) -> Result<Status, Self::Error> {
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
    let (download, ranges, _) = Download::open(&sftp, remote, &local, "peer", 1, data.len() as u64)
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
        upload_with_records(&first, &local, "/target", "peer", &records, None)
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
    let (backend, mut events) = crate::backend::Backend::new(&eframe::egui::Context::default());
    upload_with_records(
        &second,
        &local,
        "/target",
        "peer",
        &records,
        Some((&backend.events, "resume")),
    )
    .await
    .unwrap();
    let mut saw_rate = false;
    while let Ok(event) = events.try_recv() {
        if let BackendEvent::Progress {
            id,
            percent,
            rate_bps: Some(rate),
            ..
        } = event
        {
            assert_eq!(id, "resume");
            assert!(rate.is_finite() && rate > 0.0);
            assert!(percent > 99.0 && percent <= 100.0);
            saw_rate = true;
        }
    }
    assert!(saw_rate, "resumed upload must report its payload rate");
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

#[tokio::test]
async fn regular_upload_reports_measured_rate_and_correct_bytes() {
    let files = Arc::new(StdMutex::new(Files::default()));
    let sftp = session(files.clone()).await;
    let temp = crate::storage::test_private_tempdir();
    let source = temp.path().join("source.bin");
    let payload = vec![0x5a; 512 * 1024];
    std::fs::write(&source, &payload).unwrap();
    let (backend, mut events) = crate::backend::Backend::new(&eframe::egui::Context::default());
    let ctx = conflicts::TransferContext::new(&backend.events, "regular", Default::default());
    conflicts::upload_regular(
        &sftp,
        &source,
        "/target",
        &ctx,
        "Uploading source.bin".into(),
    )
    .await
    .unwrap();
    assert_eq!(files.lock().unwrap().data["/target"], payload);
    let mut last = 0.0;
    while let Ok(event) = events.try_recv() {
        if let BackendEvent::Progress {
            id,
            text,
            percent,
            rate_bps: Some(rate),
            ..
        } = event
        {
            assert_eq!(id, "regular");
            assert!(text.contains("source.bin"));
            assert!(percent >= last && percent <= 100.0);
            assert!(rate.is_finite() && rate > 0.0);
            last = percent;
        }
    }
    assert_eq!(last, 100.0);
}

#[tokio::test]
async fn upload_meter_excludes_previously_transferred_bytes() {
    let (backend, mut events) = crate::backend::Backend::new(&eframe::egui::Context::default());
    let mut meter = UploadMeter::new(
        Some((&backend.events, "resume")),
        "fixture".into(),
        10_000_000,
        9_000_000,
    );
    meter.sampled_at = Instant::now() - Duration::from_secs(1);
    meter.report(9_001_000, false).await;
    let BackendEvent::Progress {
        percent,
        rate_bps: Some(rate),
        ..
    } = events.try_recv().unwrap()
    else {
        panic!("Expected speed sample");
    };
    assert!((90.0..91.0).contains(&percent));
    assert!(rate > 0.0 && rate <= 1000.0, "{rate}");
    assert_eq!(transfer_bytes(2.5 * 1024.0 * 1024.0), "2.5 MiB");
}

#[tokio::test]
async fn agent_upload_and_desktop_download_round_trip() {
    let files = Arc::new(StdMutex::new(Files::default()));
    let temp = crate::storage::test_private_tempdir();
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("download.bin");
    // Exercise binary contents, multiple read/write chunks and parallel download.
    let data: Vec<u8> = (0..2 * 1024 * 1024 + 31).map(|i| (i % 251) as u8).collect();
    std::fs::write(&source, &data).unwrap();
    let sftp = session(files.clone()).await;
    let mut input = tokio::fs::File::open(&source).await.unwrap();
    super::super::upload_agent_file(
        sftp.clone(),
        &mut input,
        data.len() as u64,
        "/round-trip.bin",
        &upload_permit(),
        &upload_display(),
        0,
        data.len() as u64,
    )
    .await
    .unwrap();
    assert_eq!(files.lock().unwrap().data["/round-trip.bin"], data);
    let options = super::super::DownloadOptions {
        parallel: 3,
        max_bytes_per_second: 0,
        timestamps: false,
    };
    super::super::download_file(sftp, "/round-trip.bin", &destination, options, None, None)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&destination).unwrap(), data);
}
