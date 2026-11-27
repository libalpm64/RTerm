use super::*;

#[tokio::test]
async fn upload_sources_reject_shared_writable_paths_and_files() {
    let temp = crate::storage::test_private_tempdir();
    let source = temp.path().join("source.txt");
    std::fs::write(&source, b"approved source").unwrap();
    let mut input = open_agent_source(source.to_str().unwrap()).await.unwrap();
    let mut contents = Vec::new();
    input.read_to_end(&mut contents).await.unwrap();
    assert_eq!(contents, b"approved source");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(open_agent_source(source.to_str().unwrap()).await.is_err());

        let shared = temp.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        let shared_source = shared.join("source.txt");
        std::fs::write(&shared_source, b"untrusted source").unwrap();
        assert!(
            checked_agent_metadata(shared_source.to_str().unwrap())
                .await
                .is_err()
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn interactive_upload_accepts_shared_files_but_rejects_links_and_special_files() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = crate::storage::test_private_tempdir();
    let shared = temp.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
    let source = shared.join("source.txt");
    std::fs::write(&source, b"manual upload fixture").unwrap();
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o666)).unwrap();
    let mut input = open_interactive_source(&source).await.unwrap();
    let mut contents = Vec::new();
    input.read_to_end(&mut contents).await.unwrap();
    assert_eq!(contents, b"manual upload fixture");
    assert!(open_agent_source(source.to_str().unwrap()).await.is_err());
    let link = shared.join("link.txt");
    symlink(&source, &link).unwrap();
    assert!(open_interactive_source(&link).await.is_err());
    let alias = temp.path().join("alias");
    symlink(&shared, &alias).unwrap();
    assert!(
        open_interactive_source(&alias.join("source.txt"))
            .await
            .is_err()
    );
    let fifo = shared.join("fifo");
    let raw = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);
    assert!(open_interactive_source(&fifo).await.is_err());
}

#[tokio::test]
async fn upload_manifest_reports_file_count_and_total_and_detects_size_changes() {
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    let temp = crate::storage::test_private_tempdir();
    let root = temp.path().join("folder");
    let nested = root.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(root.join("one.txt"), b"one").unwrap();
    std::fs::write(nested.join("two.txt"), b"four").unwrap();
    let active = Arc::new(AtomicBool::new(true));
    let permit = crate::agent::Permit {
        bridge: active.clone(),
        request: active,
        deadline: Instant::now() + Duration::from_secs(60),
    };
    let summary = inspect_agent_upload(root.to_str().unwrap(), "/upload", &permit)
        .await
        .unwrap();
    assert_eq!(summary.file_count, 2);
    assert_eq!(summary.directory_count, 2);
    assert_eq!(summary.total_bytes, 7);

    std::fs::write(nested.join("two.txt"), b"changed-size").unwrap();
    let (_, changed) = plan_agent_upload(root.to_str().unwrap(), "/upload", &permit)
        .await
        .unwrap();
    assert_ne!(summary, changed);
}
