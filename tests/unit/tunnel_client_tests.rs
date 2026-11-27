use super::*;

fn executable(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"fixture").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn automatic_client_uses_installed_copy_even_with_stale_saved_path() {
    let temp = crate::storage::test_private_tempdir();
    let root = temp.path();
    let name = if cfg!(windows) {
        "tunnel-client.exe"
    } else {
        "tunnel-client"
    };
    let directory = root.join("bin");
    let app = directory.join("rterm");
    #[cfg(target_os = "linux")]
    let bundled = root.join("lib").join("rterm").join(name);
    #[cfg(not(target_os = "linux"))]
    let bundled = directory.join(name);
    let override_path = root.join("custom").join(name);
    let external = root.join("external").join(name);
    executable(&bundled);
    executable(&external);
    executable(&override_path);
    let path = std::env::join_paths([external.parent().unwrap()]).unwrap();
    let resolve = |configured: &Path| resolve_client(configured, Some(&app), Some(&path), name);
    assert_eq!(resolve(Path::new("")).unwrap(), bundled);
    assert_eq!(resolve(&root.join("removed-client")).unwrap(), bundled);
    assert_eq!(resolve(&override_path).unwrap(), override_path);
    std::fs::remove_file(&bundled).unwrap();
    assert_eq!(resolve(Path::new("")).unwrap(), external);
    assert!(resolve(&root.join("removed-client")).is_err());
}

#[cfg(unix)]
#[test]
fn rejects_executables_in_shared_writable_path_directories() {
    use std::os::unix::fs::PermissionsExt;

    let temp = crate::storage::test_private_tempdir();
    let shared = temp.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
    let candidate = shared.join("tunnel-client");
    executable(&candidate);
    assert!(!client_executable(&candidate));
}
