use super::*;

const HEADER: &str = "\u{feff}Groups,Label,Tags,Notes,Hostname/IP,Protocol,Port,Username,Password,KeyPath,Passphrase,Proxy,ProxyJump,LocalShell,SshInputCompatibility\r\n";

#[test]
fn netcatty_export_imports_supported_endpoints_and_ssh_credentials() {
    let csv = format!(
        "{HEADER}Team,\"日本語, server\",,\"First line\r\nSecond \"\"quoted\"\" line\",[2001:db8::1],ssh,2222,alice,secret,~/.ssh/id_ed25519,__netcatty_csv_passphrase_v1__:%2Bpass,direct,,,true\r\n\
         ,Console,,,console.example,telnet,,,,,,,,,\r\n\
         ,Jump,,,inside.example,ssh,22,alice,,,,,bastion.example,,true\r\n\
         ,Proxy,,,proxy.example,ssh,22,alice,,,,socks5://proxy.example:1080,,,true\r\n\
         ,Invalid,,,bad.example,ssh,65536,alice,,,,,,,true\r\n\
         ,Local,,,localhost,local,22,,,,,,,,\r\n"
    );
    let imported = parse(csv.as_bytes()).unwrap();
    assert_eq!(imported.skipped, 4);
    assert_eq!(imported.profiles.len(), 2);
    assert_eq!(imported.profiles[0].name, "日本語, server");
    let Connection::Ssh { config, .. } = &imported.profiles[0].connection else {
        panic!("Expected SSH profile");
    };
    assert_eq!(
        (&*config.host, config.port, &*config.user),
        ("2001:db8::1", 2222, "alice")
    );
    assert_eq!(config.password.as_deref(), Some("secret"));
    assert_eq!(config.key_passphrase.as_deref(), Some("+pass"));
    assert_eq!(
        config.key_path.as_deref().map(Path::new),
        Some(crate::storage::home_dir().join(".ssh/id_ed25519").as_path())
    );
    let Connection::Telnet(config) = &imported.profiles[1].connection else {
        panic!("Expected Telnet profile");
    };
    assert_eq!((&*config.host, config.port), ("console.example", 23));
    assert!(parse(b"Host,Name\nexample.test,Example\n").is_err());
}

#[test]
fn repeat_import_matches_canonical_addresses_and_preserves_existing_profiles() {
    let active =
        b"Label,Hostname/IP,Protocol,Port,Username\nProduction,SERVER.example.,ssh,22,alice\n";
    let mut saved = parse(active).unwrap().profiles;
    let Connection::Ssh { config, .. } = &mut saved[0].connection else {
        panic!("Expected SSH profile");
    };
    config.use_agent = true;

    let csv = b"Label,Hostname/IP,Protocol,Username\nProduction,server.example,ssh,alice\nProduction,server.example,ssh,bob\nProduction,server.example,ssh,bob\n";
    assert_eq!(append_unique(&mut saved, parse(csv).unwrap().profiles), 1);
    assert_eq!(append_unique(&mut saved, parse(csv).unwrap().profiles), 0);
    assert_eq!(saved.len(), 2);
    assert!(matches!(&saved[0].connection, Connection::Ssh { config, .. } if config.use_agent));
}

#[test]
fn imported_hosts_reject_bidirectional_format_controls() {
    let csv = "Label,Hostname/IP,Protocol,Username\nSpoofed,example\u{202e}.com,ssh,alice\n";
    let imported = parse(csv.as_bytes()).unwrap();
    assert!(imported.profiles.is_empty());
    assert_eq!(imported.skipped, 1);
}

#[test]
fn detects_exports_by_netcattys_download_filename() {
    let directory = tempfile::tempdir().unwrap();
    let export = directory.path().join("hosts_export_2026-11-26.csv");
    std::fs::write(&export, HEADER).unwrap();
    std::fs::write(directory.path().join("unrelated.csv"), "unrelated").unwrap();
    assert_eq!(latest_export(directory.path()), Some(export));
}

#[test]
fn detects_the_session_format_from_contents_instead_of_the_filename() {
    let directory = tempfile::tempdir().unwrap();
    let renamed = directory.path().join("renamed.data");
    std::fs::write(&renamed, HEADER).unwrap();
    assert_eq!(read(&renamed).unwrap().format, "Netcatty CSV");

    let unrelated = directory.path().join("hosts_export_2026-11-26.csv");
    std::fs::write(&unrelated, "Name,Address\nExample,example.test\n").unwrap();
    assert_eq!(read(&unrelated).err().as_deref(), Some(UNSUPPORTED_FORMAT));
}
