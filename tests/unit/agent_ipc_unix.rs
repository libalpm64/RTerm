use super::*;
use std::os::unix::fs::PermissionsExt;

fn private_test_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("rterm-test-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in("/tmp")
        .unwrap()
}

#[test]
fn rejects_public_and_symlinked_socket_directories() {
    let dir = private_test_dir();
    let public = dir.path().join("public");
    fs::create_dir(&public).unwrap();
    fs::set_permissions(&public, fs::Permissions::from_mode(0o755)).unwrap();
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(dir.path(), &alias).unwrap();
    for parent in [public, alias] {
        let name = parent.join("agent.sock");
        for create in [false, true] {
            assert_eq!(
                private_parent(&name, create).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
        }
    }
    let name = dir.path().join("new-private").join("agent.sock");
    private_parent(&name, true).unwrap();
    let meta = fs::symlink_metadata(name.parent().unwrap()).unwrap();
    assert_eq!(meta.uid(), uid());
    assert_eq!(meta.mode() & 0o077, 0);
}

#[tokio::test]
async fn local_socket_round_trip_and_exclusive_listener() {
    let dir = private_test_dir();
    let name = dir.path().join("agent.sock");
    let name = name.to_str().unwrap();
    let listener = listen(name).unwrap();
    assert!(listen(name).is_err());
    let server = tokio::spawn(async move {
        let mut peer = listener.accept().await.unwrap();
        assert_eq!(read_frame(&mut peer, MAX_REQUEST).await.unwrap(), b"test");
        write_frame(&mut peer, b"reply", MAX_RESPONSE)
            .await
            .unwrap();
    });
    assert_eq!(
        request(
            name,
            &std::env::current_exe().unwrap(),
            b"test",
            Duration::from_secs(2)
        )
        .await
        .unwrap(),
        b"reply"
    );
    server.await.unwrap();
    assert!(!Path::new(name).exists());
}

#[tokio::test]
async fn rejects_unsafe_lock_and_socket_objects() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;

    let dir = private_test_dir();
    let name = dir.path().join("agent.sock");
    let name_str = name.to_str().unwrap();
    let lock = dir.path().join("agent.sock.lock");
    let path = CString::new(lock.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert!(listen(name_str).is_err());
    fs::remove_file(&lock).unwrap();

    let target = dir.path().join("target");
    fs::write(&target, b"unchanged").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&target, &lock).unwrap();
    assert!(listen(name_str).is_err());
    fs::remove_file(&lock).unwrap();
    fs::hard_link(&target, &lock).unwrap();
    assert!(listen(name_str).is_err());
    fs::remove_file(&lock).unwrap();
    fs::write(&lock, b"").unwrap();
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(listen(name_str).is_err());
    fs::remove_file(&lock).unwrap();

    symlink(&target, &name).unwrap();
    assert!(listen(name_str).is_err());
    assert!(check_socket(&name).is_err());
    fs::remove_file(&name).unwrap();
    fs::write(&name, b"not a socket").unwrap();
    assert!(listen(name_str).is_err());
    assert_eq!(fs::read(&name).unwrap(), b"not a socket");
    assert_eq!(fs::read(&target).unwrap(), b"unchanged");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn rejects_unexpected_peer_executable_before_sending() {
    use tokio::io::AsyncReadExt;

    let dir = private_test_dir();
    let name = dir.path().join("agent.sock");
    let expected = dir.path().join("different-executable");
    fs::write(&expected, b"not the server executable").unwrap();
    let listener = listen(name.to_str().unwrap()).unwrap();
    let server = tokio::spawn(async move {
        let mut peer = listener.accept().await.unwrap();
        let mut buf = [0; 1];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), peer.read(&mut buf))
                .await
                .unwrap()
                .unwrap(),
            0,
            "request bytes must not reach an unexpected executable"
        );
    });
    let error = request(
        name.to_str().unwrap(),
        &expected,
        b"must not be sent",
        Duration::from_secs(2),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    server.await.unwrap();
}
