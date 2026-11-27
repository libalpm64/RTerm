use super::*;
#[test]
fn launcher_quotes_absolute_install_path() {
    let entry = desktop_entry(Path::new("/opt/RTerm 100%/rterm")).unwrap();
    assert!(entry.contains(r#"Exec=/bin/sh -c "exec \\"\\$0\\"" "/opt/RTerm 100%%/rterm""#));
    assert!(
        desktop_entry(Path::new("/opt/RTerm Space/rterm"))
            .unwrap()
            .contains("Exec=\"/opt/RTerm Space/rterm\"\n")
    );
    assert!(desktop_entry(Path::new("/bad\npath/rterm")).is_err());
}
#[test]
fn shortcut_is_optional_and_never_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let setup = Setup {
        executable: dir.path().join("rterm"),
        destination: dir.path().join("RTerm.desktop"),
        decision: dir.path().join("config/choice"),
    };
    assert!(setup.pending());
    setup.choose(false).unwrap();
    assert!(!setup.pending());
    assert!(!setup.destination.exists());
    fs::write(&setup.destination, "existing shortcut").unwrap();
    assert!(setup.choose(true).is_err());
    assert_eq!(
        fs::read_to_string(&setup.destination).unwrap(),
        "existing shortcut"
    );
    fs::remove_file(&setup.destination).unwrap();
    setup.choose(true).unwrap();
    assert!(
        fs::read_to_string(&setup.destination)
            .unwrap()
            .contains("Name=RTerm")
    );
    setup.choose(true).unwrap();
}
