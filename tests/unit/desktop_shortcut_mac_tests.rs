use super::*;
#[test]
fn shortcut_is_optional_and_never_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let setup = Setup {
        executable: dir.path().join("installed/RTerm.app"),
        destination: dir.path().join("RTerm.app"),
        decision: dir.path().join("config/choice"),
    };
    assert!(setup.pending());
    setup.choose(false).unwrap();
    assert!(!setup.pending());
    assert!(!setup.destination.exists());
    fs::create_dir(&setup.destination).unwrap();
    assert!(setup.choose(true).is_err());
    assert!(setup.destination.is_dir());
    fs::remove_dir(&setup.destination).unwrap();
    setup.choose(true).unwrap();
    assert_eq!(fs::read_link(&setup.destination).unwrap(), setup.executable);
    setup.choose(true).unwrap();
}
