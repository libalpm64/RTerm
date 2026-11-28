use super::*;

#[test]
fn vault_key_derivation_matches_legacy_argon2_defaults() {
    let key = VaultKey::derive_with_salt("RTerm vault compatibility check", [0x42; 16]);
    assert_eq!(
        key.0,
        [
            0x8a, 0x27, 0x2f, 0x47, 0x1a, 0x03, 0xab, 0xbb, 0xe6, 0x7a, 0x31, 0x42, 0x18, 0xb7,
            0x31, 0x90, 0xd4, 0x4d, 0xf6, 0xc5, 0xb0, 0x20, 0xbb, 0xdf, 0x00, 0x7f, 0xec, 0x7c,
            0xf9, 0x4f, 0x1f, 0x8c,
        ]
    );
}

#[cfg(unix)]
#[test]
fn config_parent_requires_a_trusted_owner() {
    assert!(safe_owned_directory(true, 1000, 0o755, 1000));
    assert!(safe_owned_directory(true, 0, 0o1777, 1000));
    assert!(!safe_owned_directory(true, 2000, 0o755, 1000));
    assert!(!safe_owned_directory(true, 2000, 0o1777, 1000));
    assert!(!safe_owned_directory(true, 1000, 0o777, 1000));
    assert!(!safe_owned_directory(false, 1000, 0o700, 1000));
}

#[test]
fn legacy_migration_accepts_only_trusted_regular_files() {
    let temp = test_private_tempdir();
    let trusted = temp.path().join("trusted");
    std::fs::create_dir(&trusted).unwrap();
    let source = trusted.join("settings.json");
    std::fs::write(&source, br#"{"ai_access":"ask"}"#).unwrap();
    assert_eq!(
        read_legacy_config_file(&source).unwrap(),
        br#"{"ai_access":"ask"}"#
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let shared = temp.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        let untrusted = shared.join("settings.json");
        std::fs::write(&untrusted, br#"{"ai_access":"unrestricted"}"#).unwrap();
        assert!(read_legacy_config_file(&untrusted).is_none());

        let alias = trusted.join("linked.json");
        symlink(&source, &alias).unwrap();
        assert!(read_legacy_config_file(&alias).is_none());
        let hardlink = trusted.join("hardlinked.json");
        std::fs::hard_link(&source, &hardlink).unwrap();
        assert!(read_legacy_config_file(&hardlink).is_none());
    }
}

#[test]
fn private_temp_files_have_restricted_permissions() {
    let directory = test_private_tempdir();
    let file = private_temp_file_in(directory.path(), "rterm-output-", ".log").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = file.as_file().metadata().unwrap();
        assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }
    #[cfg(windows)]
    assert!(windows_file_is_private(file.as_file()));
}

#[cfg(windows)]
#[test]
fn config_directory_and_files_require_private_windows_acls() {
    let temp = test_private_tempdir();
    let config = temp.path().join("rterm");
    assert!(ensure_private_config_dir(&config));
    let path = config.join("settings.json");
    write_atomic(&path, br#"{"ai_access":"ask"}"#).unwrap();
    let file = std::fs::File::open(&path).unwrap();
    assert!(windows_file_is_private(&file));
    assert!(
        read_private_config_file(&path, MAX_CONFIG_FILE)
            .unwrap()
            .is_some()
    );
}

#[cfg(unix)]
#[test]
fn config_directory_and_migrated_files_are_private() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let temp = test_private_tempdir();
    let config = temp.path().join("rterm");
    std::fs::create_dir(&config).unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(ensure_private_config_dir(&config));
    assert_eq!(
        std::fs::symlink_metadata(&config)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    let copied = config.join("vault.dat");
    std::fs::write(&copied, b"encrypted vault").unwrap();
    std::fs::set_permissions(&copied, std::fs::Permissions::from_mode(0o644)).unwrap();
    restrict_private_file(&copied).unwrap();
    assert_eq!(
        std::fs::metadata(&copied).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let target = temp.path().join("target");
    let alias = temp.path().join("alias");
    std::fs::create_dir(&target).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
    symlink(&target, &alias).unwrap();
    assert!(!ensure_private_config_dir(&alias));
    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o755
    );

    let shared = temp.path().join("shared");
    let private_child = shared.join("rterm");
    std::fs::create_dir(&shared).unwrap();
    std::fs::create_dir(&private_child).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(!ensure_private_config_dir(&private_child));

    let vault_path = config.join("vault.dat");
    let key = VaultKey::derive("private file fixture");
    write_atomic(&vault_path, &encode_vault(&Vault::default(), &key).unwrap()).unwrap();
    std::fs::set_permissions(&vault_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read_private_vault(&vault_path).is_ok());
    assert_eq!(
        std::fs::metadata(&vault_path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let vault_symlink = config.join("vault-symlink.dat");
    let vault_hardlink = config.join("vault-hardlink.dat");
    symlink(&vault_path, &vault_symlink).unwrap();
    std::fs::hard_link(&vault_path, &vault_hardlink).unwrap();
    assert!(read_private_vault(&vault_symlink).is_err());
    assert!(read_private_vault(&vault_hardlink).is_err());

    let config_symlink = config.join("settings-symlink.json");
    let config_hardlink = config.join("settings-hardlink.json");
    let settings = config.join("settings-target.json");
    std::fs::write(&settings, b"{\"ai_access\":\"ask\"}").unwrap();
    symlink(&settings, &config_symlink).unwrap();
    std::fs::hard_link(&settings, &config_hardlink).unwrap();
    assert!(read_private_config_file(&config_symlink, MAX_CONFIG_FILE).is_err());
    assert!(read_private_config_file(&config_hardlink, MAX_CONFIG_FILE).is_err());
}

#[cfg(unix)]
#[test]
fn unix_config_and_lock_reject_fifos_and_unsafe_links() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    let temp = test_private_tempdir();
    let config = temp.path().join("rterm");
    assert!(ensure_private_config_dir(&config));
    let settings = config.join("settings.json");
    let lock_path = settings.with_extension("lock");
    for path in [&settings, &lock_path] {
        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    }
    assert!(read_private_config_file(&settings, MAX_CONFIG_FILE).is_err());
    assert!(read_legacy_config_file(&settings).is_none());
    assert!(lock_storage(&settings).is_err());
    std::fs::remove_file(&settings).unwrap();
    std::fs::remove_file(&lock_path).unwrap();

    let target = config.join("target");
    std::fs::write(&target, b"unchanged").unwrap();
    symlink(&target, &lock_path).unwrap();
    assert!(lock_storage(&settings).is_err());
    std::fs::remove_file(&lock_path).unwrap();
    std::fs::hard_link(&target, &lock_path).unwrap();
    assert!(lock_storage(&settings).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"unchanged");
    std::fs::remove_file(&lock_path).unwrap();

    std::fs::write(&settings, b"settings").unwrap();
    std::fs::set_permissions(&settings, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        read_private_config_file(&settings, MAX_CONFIG_FILE)
            .unwrap()
            .unwrap(),
        b"settings"
    );
    assert_eq!(std::fs::metadata(&settings).unwrap().mode() & 0o777, 0o600);
    let lock = lock_storage(&settings).unwrap();
    let meta = lock.metadata().unwrap();
    assert_eq!(meta.uid(), unsafe { libc::geteuid() });
    assert_eq!(meta.nlink(), 1);
    assert_eq!(meta.mode() & 0o777, 0o600);
}

#[cfg(unix)]
#[test]
fn unix_directory_checks_symlink_target_ancestors() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let temp = test_private_tempdir();
    let shared = temp.path().join("shared");
    let target = shared.join("owned");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
    let alias = temp.path().join("alias");
    symlink(&target, &alias).unwrap();
    assert!(!unix_directory_is_private(&alias));

    let safe = temp.path().join("safe");
    std::fs::create_dir(&safe).unwrap();
    let safe_alias = temp.path().join("safe-alias");
    symlink("safe", &safe_alias).unwrap();
    assert!(unix_directory_is_private(&safe_alias));

    let intermediate = shared.join("redirect");
    symlink(&safe, &intermediate).unwrap();
    let indirect = temp.path().join("indirect");
    symlink(&intermediate, &indirect).unwrap();
    assert!(!unix_directory_is_private(&indirect));
}

fn legacy_vault(password: &str) -> Vec<u8> {
    let key = VaultKey::derive_with_salt(password, legacy_vault_salt());
    let plaintext = serde_json::to_vec(&serde_json::json!({
        "v": 2,
        "profiles": [{
            "name": "Legacy profile",
            "connection": {"type": "local", "shell": "bash"}
        }]
    }))
    .unwrap();
    let cipher = XChaCha20Poly1305::new_from_slice(&key.0).unwrap();
    let nonce = XNonce::generate();
    let ciphertext = cipher.encrypt(&nonce, plaintext.as_slice()).unwrap();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(VAULT_MAGIC);
    bytes.extend_from_slice(&LEGACY_VAULT_VERSION.to_le_bytes());
    bytes.extend_from_slice(&nonce);
    bytes.extend_from_slice(&ciphertext);
    bytes
}

#[test]
fn vault_cipher_upgrade_preserves_existing_ciphertext() {
    // Generated with chacha20poly1305 0.10.1 using a fixed fixture key
    // and nonce. No real vault or credentials are involved.
    let ciphertext = [
        0xb8, 0xce, 0x23, 0x86, 0xc7, 0x27, 0x84, 0x1d, 0xd8, 0xa3, 0x59, 0xc5, 0xa1, 0x5a, 0xb9,
        0xd1, 0x45, 0x89, 0x41, 0x0c, 0x40, 0x5e, 0x5c, 0x18, 0xd7, 0xbd, 0xfb, 0x5b, 0xe5, 0xd9,
        0x21, 0x0b, 0xda, 0x5b, 0x44, 0x5b, 0x0f, 0xf3, 0xbf, 0x66, 0xd4, 0x70, 0x7c, 0x23, 0x79,
        0xf6, 0x73, 0xb8, 0xb5, 0xa3, 0x76, 0x86, 0xfe, 0x01, 0x57, 0xa7, 0x32, 0x43, 0x69, 0x97,
        0x8f, 0x15, 0x1b, 0x91, 0x97, 0x31, 0x77, 0xc3, 0x92, 0x95, 0xa5, 0x56, 0x8d, 0x8e, 0xd0,
        0x20, 0x73, 0x66, 0xc0, 0x17, 0xf4, 0x51, 0xed, 0x19, 0x0a, 0xed, 0xe4, 0xda, 0xf0, 0x88,
        0x32, 0x98, 0xea, 0x2b, 0x0c, 0x34, 0xc3, 0x2f, 0x50, 0x98, 0x96, 0x38, 0xe3, 0x49, 0x87,
        0x31, 0xad, 0xef, 0xce, 0x98, 0xd8, 0x55, 0xda, 0xd1, 0x75, 0xda, 0x4c,
    ];
    let key = VaultKey([42; 32], [11; VAULT_SALT_LEN]);
    let mut bytes = VAULT_MAGIC.to_vec();
    bytes.extend_from_slice(&VAULT_VERSION.to_le_bytes());
    bytes.extend_from_slice(&key.1);
    bytes.extend_from_slice(&[7; 24]);
    bytes.extend_from_slice(&ciphertext);
    let vault = decode_vault(&bytes, &key).unwrap();
    assert_eq!(vault.profiles.len(), 1);
    assert_eq!(vault.profiles[0].name, "Compatibility fixture");
    assert!(
        matches!(&vault.profiles[0].connection, Connection::Local { shell } if shell == "fixture")
    );
    let cipher = XChaCha20Poly1305::new_from_slice(&key.0).unwrap();
    let nonce = XNonce::from([7; 24]);
    assert_eq!(cipher.encrypt(&nonce, br#"{"v":2,"profiles":[{"name":"Compatibility fixture","connection":{"type":"local","shell":"fixture"}}]}"#.as_slice()).unwrap(), ciphertext);
}

#[test]
fn imported_ssh_credentials_are_written_only_to_an_existing_vault() {
    let dir = test_private_tempdir();
    let path = dir.path().join("vault.dat");
    let export = dir.path().join("hosts_export_test.csv");
    std::fs::write(&export, "Label,Hostname/IP,Protocol,Username,Password,KeyPath,Passphrase\nImported,test.example,ssh,alice,fixture-netcatty-password,/test/key,fixture-key-passphrase\n").unwrap();
    let incoming = || crate::session_import::read(&export).unwrap().profiles;
    let key = VaultKey([42; 32], [42; VAULT_SALT_LEN]);
    assert!(import_vault_profiles_at(&path, &key, incoming()).is_err());
    assert!(!path.exists());
    update_vault(&path, &key, true, |vault| {
        vault.profiles.push(Profile {
            folder: None,
            name: "Existing profile".into(),
            connection: Connection::Local {
                shell: String::new(),
            },
        });
    })
    .unwrap();
    assert_eq!(
        import_vault_profiles_at(&path, &key, incoming()).unwrap().1,
        1
    );
    assert_eq!(
        import_vault_profiles_at(&path, &key, incoming()).unwrap().1,
        0
    );
    let bytes = std::fs::read(&path).unwrap();
    assert!(
        !bytes
            .windows(b"fixture-netcatty-password".len())
            .any(|window| window == b"fixture-netcatty-password")
    );
    assert!(!dir.path().join("sessions.json").exists());
    let reopened = decode_vault(&bytes, &key).unwrap();
    assert_eq!(reopened.profiles.len(), 2);
    assert_eq!(reopened.profiles[0].name, "Existing profile");
    let Connection::Ssh { config, .. } = &reopened.profiles[1].connection else {
        panic!("Expected SSH profile");
    };
    assert_eq!(
        config.password.as_deref(),
        Some("fixture-netcatty-password")
    );
    assert_eq!(
        config.key_passphrase.as_deref(),
        Some("fixture-key-passphrase")
    );
}

#[test]
fn settings_updates_preserve_other_keys_and_corrupt_files() {
    let dir = test_private_tempdir();
    let path = dir.path().join("new-config/settings.json");
    save_settings_at(&path, &[("font_size", "13".into())]).unwrap();
    save_settings_at(&path, &[("sftp_show_hidden", "1".into())]).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["font_size"], "13");
    assert_eq!(saved["sftp_show_hidden"], "1");
    std::fs::write(&path, b"invalid json").unwrap();
    assert!(save_settings_at(&path, &[("font_size", "14".into())]).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"invalid json");
}

#[test]
fn vault_backup_roundtrip_preserves_encrypted_contents_and_previous_vault() {
    let dir = test_private_tempdir();
    let source = dir.path().join("source.dat");
    let exported = dir.path().join("portable.rterm-vault");
    let destination = dir.path().join("restored");
    let key = VaultKey::derive("fixture backup password");
    let previous_key = VaultKey([9; 32], [9; VAULT_SALT_LEN]);
    update_vault(&source, &key, true, |vault| {
        vault.profiles.push(Profile {
            folder: None,
            name: "Backup session".into(),
            connection: Connection::Local {
                shell: "bash".into(),
            },
        });
        vault.keys.push(SshKey {
            name: "Fixture key".into(),
            private_key: "fixture private key".into(),
            public_key: "fixture public key".into(),
            key_type: "ed25519".into(),
        });
        vault.openai_tunnel = Some(crate::tunnel::Profile {
            id: "tunnel_0123456789abcdef0123456789abcdef".into(),
            api_key: "sk-fixture-runtime-key".into(),
            client: PathBuf::new(),
        });
    })
    .unwrap();
    export_vault_at(&source, &exported, &key).unwrap();
    let bytes = std::fs::read(&exported).unwrap();
    assert_eq!(bytes, std::fs::read(&source).unwrap());
    assert!(!bytes.windows(19).any(|w| w == b"fixture private key"));
    assert!(!bytes.windows(22).any(|w| w == b"sk-fixture-runtime-key"));
    assert!(export_vault_at(&source, &exported, &key).is_err());
    assert!(export_vault_at(&source, &source, &key).is_err());
    assert!(VaultBackup::read(&exported, "incorrect").is_err());

    let backup = || VaultBackup::read(&exported, "fixture backup password").unwrap();
    assert!(
        import_vault_at(&destination, backup(), None, false)
            .unwrap()
            .is_none()
    );
    let restored = destination.join("vault.dat");
    assert_eq!(std::fs::read(&restored).unwrap(), bytes);
    let vault = decode_vault(&std::fs::read(&restored).unwrap(), &key).unwrap();
    assert_eq!(vault.profiles[0].name, "Backup session");
    assert_eq!(vault.keys[0].private_key, "fixture private key");
    assert_eq!(
        vault.openai_tunnel.unwrap().api_key,
        "sk-fixture-runtime-key"
    );
    let settings: serde_json::Value =
        serde_json::from_slice(&std::fs::read(destination.join("settings.json")).unwrap()).unwrap();
    assert_eq!(settings["vault_enabled"], "1");
    assert_eq!(settings["ai_tunnel_autoconnect"], "0");

    let previous = encode_vault(&Vault::default(), &previous_key).unwrap();
    write_atomic(&restored, &previous).unwrap();
    assert!(import_vault_at(&destination, backup(), Some(&previous_key), false).is_err());
    assert!(import_vault_at(&destination, backup(), None, true).is_err());
    assert!(import_vault_at(&destination, backup(), Some(&key), true).is_err());
    assert_eq!(std::fs::read(&restored).unwrap(), previous);
    let saved = import_vault_at(&destination, backup(), Some(&previous_key), true)
        .unwrap()
        .unwrap();
    assert_eq!(std::fs::read(saved).unwrap(), previous);
    assert_eq!(std::fs::read(&restored).unwrap(), bytes);
    assert!(decode_vault(&std::fs::read(restored).unwrap(), &previous_key).is_err());
}

#[test]
fn legacy_shared_salt_vaults_migrate_after_successful_unlock() {
    const PASSWORD: &str = "legacy fixture password";
    let dir = test_private_tempdir();
    let path = dir.path().join("vault.dat");
    let backup_path = dir.path().join("legacy-backup.rterm-vault");
    let original = legacy_vault(PASSWORD);
    write_atomic(&path, &original).unwrap();
    write_atomic(&backup_path, &original).unwrap();

    assert!(VaultBackup::read(&backup_path, "incorrect password").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let backup = VaultBackup::read(&backup_path, PASSWORD).unwrap();
    assert_eq!(backup.bytes, original);

    let (vault, key) = load_vault_at(&path, PASSWORD).unwrap();
    assert_eq!(vault.profiles[0].name, "Legacy profile");
    assert_eq!(vault.profiles[0].subtitle(), "bash");

    let migrated = std::fs::read(&path).unwrap();
    assert_eq!(vault_version(&migrated), Ok(VAULT_VERSION));
    assert_ne!(&migrated[6..6 + VAULT_SALT_LEN], &legacy_vault_salt());
    assert_eq!(decode_vault(&migrated, &key).unwrap().profiles.len(), 1);
    assert!(!VaultKey::from_data(PASSWORD, &migrated).unwrap().1);
}

#[test]
fn equal_passwords_receive_independent_vault_salts() {
    let first = VaultKey::derive("same password");
    let second = VaultKey::derive("same password");
    assert_ne!(first.1, second.1);
    assert_ne!(first.0, second.0);
}

#[test]
fn vault_import_rejects_invalid_files_and_rolls_back_on_settings_failure() {
    let dir = test_private_tempdir();
    let file = dir.path().join("incoming.rterm-vault");
    let key = VaultKey([7; 32], [7; VAULT_SALT_LEN]);
    let mut bytes = encode_vault(&Vault::default(), &key).unwrap();
    std::fs::write(&file, b"not a vault").unwrap();
    assert!(read_vault_backup(&file).is_err());
    bytes[4..6].copy_from_slice(&999u16.to_le_bytes());
    std::fs::write(&file, &bytes).unwrap();
    assert!(read_vault_backup(&file).is_err());
    bytes[4..6].copy_from_slice(&VAULT_VERSION.to_le_bytes());
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(&file, &bytes).unwrap();
    assert!(decode_vault(&read_vault_backup(&file).unwrap(), &key).is_err());
    let original = encode_vault(&Vault::default(), &key).unwrap();
    let replacement =
        encode_vault(&Vault::default(), &VaultKey([8; 32], [8; VAULT_SALT_LEN])).unwrap();
    let settings = dir.path().join("settings.json");
    std::fs::write(&settings, b"invalid settings").unwrap();
    let path = dir.path().join("vault.dat");
    assert!(
        import_vault_at(
            dir.path(),
            VaultBackup {
                bytes: replacement.clone()
            },
            None,
            false
        )
        .is_err()
    );
    assert!(!path.exists());
    write_atomic(&path, &original).unwrap();
    assert!(
        import_vault_at(
            dir.path(),
            VaultBackup { bytes: replacement },
            Some(&key),
            true
        )
        .is_err()
    );
    assert_eq!(std::fs::read(path).unwrap(), original);
    assert_eq!(std::fs::read(settings).unwrap(), b"invalid settings");
}

#[test]
fn audit_commands_survive_encrypted_vault_updates() {
    let dir = test_private_tempdir();
    let path = dir.path().join("vault.dat");
    let key = VaultKey([7; 32], [7; VAULT_SALT_LEN]);
    let command = "echo vault-audit-fixture";
    let tunnel_id = "tunnel_0123456789abcdef0123456789abcdef";
    let runtime_key = "sk-fixture-only-not-an-openai-key";
    let entry = AgentAuditEntry {
        id: "audit-1".into(),
        operation: "operation-1".into(),
        session: "session-1".into(),
        session_name: "Fixture SSH".into(),
        action: "Interactive command".into(),
        command: command.into(),
        submitted_ms: 123,
        finished_ms: Some(456),
        phase: "completed".into(),
        exit_code: Some(7),
        duration_ms: Some(333),
        detail: None,
    };
    let empty = update_vault(&path, &key, true, |vault| {
        vault.profiles.push(Profile {
            folder: None,
            name: "Existing profile".into(),
            connection: Connection::Local {
                shell: "bash".into(),
            },
        });
    })
    .unwrap();
    assert!(empty.agent_audit.is_empty());
    assert!(empty.openai_tunnel.is_none());
    update_vault(&path, &key, false, |vault| {
        vault.openai_tunnel = Some(crate::tunnel::Profile {
            id: tunnel_id.into(),
            api_key: runtime_key.into(),
            client: PathBuf::new(),
        });
    })
    .unwrap();
    update_vault(&path, &key, false, |vault| {
        vault.agent_audit.push_back(entry)
    })
    .unwrap();
    update_vault(&path, &key, false, |vault| {
        vault.profiles[0].name = "Updated profile".into()
    })
    .unwrap();
    let bytes = std::fs::read(path).unwrap();
    for secret in [command, runtime_key, tunnel_id] {
        assert!(
            !bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes())
        );
    }
    assert!(decode_vault(&bytes, &VaultKey([8; 32], [8; VAULT_SALT_LEN])).is_err());
    let vault = decode_vault(&bytes, &key).unwrap();
    assert_eq!(vault.profiles[0].name, "Updated profile");
    assert_eq!(vault.agent_audit[0].command, command);
    assert_eq!(vault.agent_audit[0].exit_code, Some(7));
    let profile = vault.openai_tunnel.unwrap();
    assert_eq!(profile.id, tunnel_id);
    assert_eq!(profile.api_key, runtime_key);
    assert!(!profile.setup().contains(runtime_key));
}

#[cfg(unix)]
fn restrict_private_file(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| error.to_string())
}

pub(crate) fn test_private_tempdir() -> tempfile::TempDir {
    #[cfg(windows)]
    {
        let parent = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .expect("APPDATA is required for private Windows test directories");
        tempfile::Builder::new()
            .prefix("rterm-test-")
            .tempdir_in(parent)
            .unwrap()
    }
    #[cfg(unix)]
    {
        test_private_tempdir_in(&std::env::temp_dir())
    }
    #[cfg(not(any(unix, windows)))]
    {
        tempfile::tempdir().unwrap()
    }
}

// macOS exposes its temporary directory through system aliases such as
// /var -> /private/var. Resolve the fixture parent *before* creating the
// directory so tests intended to exercise regular uploads don't accidentally
// pass a symlink to the strict source validator. Never canonicalize a
// user-supplied upload source in production to bypass these checks.
#[cfg(unix)]
fn test_private_tempdir_in(parent: &Path) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let parent = parent
        .canonicalize()
        .expect("resolve test temporary directory");
    tempfile::Builder::new()
        .prefix("rterm-test-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(parent)
        .unwrap()
}

#[cfg(unix)]
#[test]
fn private_fixture_directory_resolves_symlinked_temp_parent() {
    use std::os::unix::fs::symlink;
    let root = test_private_tempdir();
    let actual = root.path().join("actual");
    let alias = root.path().join("alias");
    std::fs::create_dir(&actual).unwrap();
    symlink(&actual, &alias).unwrap();

    let fixture = test_private_tempdir_in(&alias);
    assert_eq!(fixture.path().parent(), Some(actual.as_path()));
    assert_eq!(fixture.path(), fixture.path().canonicalize().unwrap());
    assert!(unix_directory_is_private(fixture.path()));
    assert!(fixture.path().ancestors().all(|path| {
        !std::fs::symlink_metadata(path)
            .unwrap()
            .file_type()
            .is_symlink()
    }));
}

#[test]
fn saved_session_folders_round_trip_legacy_plain_and_encrypted() {
    let old = br#"[{"name":"Legacy","connection":{"type":"local","shell":"bash"}}]"#;
    let legacy = decode_saved_sessions(old).unwrap();
    assert_eq!(legacy.profiles.len(), 1);
    assert!(legacy.profiles[0].folder.is_none());
    assert!(legacy.folders.is_empty());
    let mut profiles = legacy.profiles;
    profiles[0].folder = Some("Production".into());
    let folders = BTreeSet::from(["Production".into(), "Empty".into()]);
    let plain = encode_saved_sessions(&profiles, &folders).unwrap();
    let loaded = decode_saved_sessions(&plain).unwrap();
    assert_eq!(loaded.folders, folders);
    assert_eq!(loaded.profiles[0].folder, profiles[0].folder);

    let key = VaultKey([42; 32], [42; VAULT_SALT_LEN]);
    let vault = Vault {
        profiles,
        session_folders: folders.clone(),
        ..Default::default()
    };
    let encrypted = encode_vault(&vault, &key).unwrap();
    let loaded = decode_vault(&encrypted, &key).unwrap();
    assert_eq!(loaded.session_folders, folders);
    assert_eq!(loaded.profiles[0].folder.as_deref(), Some("Production"));
    assert!(
        !encrypted
            .windows(b"Production".len())
            .any(|w| w == b"Production")
    );
}

#[test]
fn folder_persistence_keeps_credentials_encrypted_and_imports_in_existing_library() {
    let dir = test_private_tempdir();
    let path = dir.path().join("vault.dat");
    let key = VaultKey([43; 32], [43; VAULT_SALT_LEN]);
    let mut profile: Profile = serde_json::from_value(serde_json::json!({
        "name": "Fixture SSH",
        "folder": "Private",
        "connection": {"type":"ssh","terminal":"xterm-256color","config":{
            "host":"fixture.invalid","port":22,"user":"fixture","password":"fixture-password"
        }}
    }))
    .unwrap();
    let folders = BTreeSet::from(["Private".into(), "Empty".into()]);
    let plain = encode_saved_sessions(&[profile.clone()], &folders).unwrap();
    let loaded = decode_saved_sessions(&plain).unwrap();
    assert_eq!(loaded.folders, folders);
    assert_eq!(loaded.profiles[0].folder.as_deref(), Some("Private"));
    let Connection::Ssh { config, .. } = &loaded.profiles[0].connection else {
        panic!("SSH profile");
    };
    assert!(config.password.is_none());
    assert!(
        !plain
            .windows(b"fixture-password".len())
            .any(|w| w == b"fixture-password")
    );
    update_vault(&path, &key, true, |vault| {
        vault.profiles.push(profile.clone());
        vault.session_folders = folders.clone();
    })
    .unwrap();
    profile.name = "Imported".into();
    profile.folder = None;
    let (imported, added) = import_vault_profiles_at(&path, &key, vec![profile]).unwrap();
    assert_eq!(added, 1);
    assert_eq!(imported.folders, folders);
    assert_eq!(imported.profiles[0].folder.as_deref(), Some("Private"));
    assert!(imported.profiles[1].folder.is_none());
    let encrypted = std::fs::read(&path).unwrap();
    let reopened = decode_vault(&encrypted, &key).unwrap();
    let Connection::Ssh { config, .. } = &reopened.profiles[0].connection else {
        panic!("SSH profile");
    };
    assert_eq!(config.password.as_deref(), Some("fixture-password"));
    assert_eq!(reopened.session_folders, folders);
}

#[test]
fn vault_audit_retains_thousands_of_commands_and_updates_outcomes() {
    let dir = test_private_tempdir();
    let path = dir.path().join("vault.dat");
    let key = VaultKey([17; 32], [17; VAULT_SALT_LEN]);
    update_vault(&path, &key, true, |_| {}).unwrap();
    let entry = |i: usize| AgentAuditEntry {
        id: format!("audit-{i}"),
        operation: format!("operation-{i}"),
        session: "fixture".into(),
        session_name: "Fixture SSH".into(),
        action: "Background command".into(),
        command: format!("echo fixture-{i}"),
        submitted_ms: i as i64,
        finished_ms: Some(i as i64 + 1),
        phase: "completed".into(),
        exit_code: Some(0),
        duration_ms: Some(1),
        detail: None,
    };
    let history = save_agent_audit_at(&path, &key, (0..10_000).map(entry)).unwrap();
    assert_eq!(history.len(), 10_000);
    assert_eq!(history.front().unwrap().id, "audit-9999");
    assert_eq!(history.back().unwrap().id, "audit-0");
    let mut updated = entry(0);
    updated.exit_code = Some(7);
    let history = save_agent_audit_at(&path, &key, [entry(10_000), updated].into_iter()).unwrap();
    assert_eq!(history.len(), 10_001);
    assert_eq!(history.back().unwrap().exit_code, Some(7));
    update_vault(&path, &key, false, |vault| {
        vault.profiles.push(Profile {
            folder: None,
            name: "Fixture".into(),
            connection: Connection::Local {
                shell: "fixture".into(),
            },
        });
    })
    .unwrap();
    let bytes = read_private_vault(&path).unwrap();
    assert!(!bytes.windows(14).any(|window| window == b"echo fixture-0"));
    let vault = decode_vault(&bytes, &key).unwrap();
    assert_eq!(vault.agent_audit.len(), 10_001);
    assert_eq!(vault.agent_audit.back().unwrap().command, "echo fixture-0");
    assert_eq!(vault.agent_audit.back().unwrap().exit_code, Some(7));
    assert_eq!(vault.profiles[0].name, "Fixture");
}

#[test]
fn audit_deletion_updates_encrypted_vault_without_touching_other_records() {
    let dir = test_private_tempdir();
    let path = dir.path().join("vault.dat");
    let key = VaultKey([19; 32], [19; VAULT_SALT_LEN]);
    update_vault(&path, &key, true, |vault| {
        vault.profiles.push(Profile {
            name: "Keep profile".into(),
            folder: Some("Keep folder".into()),
            connection: Connection::Local {
                shell: "fixture".into(),
            },
        });
        vault.session_folders.insert("Keep folder".into());
        vault.openai_tunnel = Some(crate::tunnel::Profile {
            id: "tunnel_fixture".into(),
            api_key: "fixture-key".into(),
            client: PathBuf::new(),
        });
    })
    .unwrap();
    let make = |id: &str| AgentAuditEntry {
        id: id.into(),
        operation: id.into(),
        session: "fixture".into(),
        session_name: "Fixture SSH".into(),
        action: "Background command".into(),
        command: format!("fixture {id}"),
        submitted_ms: 1,
        finished_ms: Some(2),
        phase: "completed".into(),
        exit_code: Some(0),
        duration_ms: Some(1),
        detail: None,
    };
    save_agent_audit_at(&path, &key, ["delete", "keep", "new"].into_iter().map(make)).unwrap();
    let original = std::fs::read(&path).unwrap();
    let ids = BTreeSet::from(["delete".into()]);
    assert!(delete_agent_audit_at(&path, &VaultKey([20; 32], [19; VAULT_SALT_LEN]), &ids).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let retained = delete_agent_audit_at(&path, &key, &ids).unwrap();
    assert_eq!(retained.len(), 2);
    assert!(!retained.iter().any(|entry| entry.id == "delete"));
    // An unrelated later vault update must not bring the deleted row back.
    save_agent_audit_at(&path, &key, std::iter::once(make("another"))).unwrap();
    let vault = decode_vault(&read_private_vault(&path).unwrap(), &key).unwrap();
    assert_eq!(vault.agent_audit.len(), 3);
    assert!(!vault.agent_audit.iter().any(|entry| entry.id == "delete"));
    assert_eq!(vault.profiles[0].name, "Keep profile");
    assert!(vault.session_folders.contains("Keep folder"));
    assert_eq!(vault.openai_tunnel.as_ref().unwrap().api_key, "fixture-key");
    let all = vault
        .agent_audit
        .iter()
        .map(|entry| entry.id.clone())
        .collect();
    delete_agent_audit_at(&path, &key, &all).unwrap();
    let vault = decode_vault(&read_private_vault(&path).unwrap(), &key).unwrap();
    assert!(vault.agent_audit.is_empty());
    assert_eq!(vault.profiles.len(), 1);
    assert!(vault.openai_tunnel.is_some());
}
