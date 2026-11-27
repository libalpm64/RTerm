use super::*;

#[test]
fn remote_paths_reject_native_separator_traversal() {
    #[cfg(windows)]
    let local = r"C:\tmp\source";
    #[cfg(unix)]
    let local = "/tmp/source";

    assert!(validate_paths(local, "/safe/target").is_ok());
    assert!(validate_paths(local, "/safe\\..\\outside").is_err());
    assert!(validate_paths(local, "/safe\\target").is_err());
}
