use crate::models::{AgentAuditEntry, Connection, Profile, SshConfig, SshKey};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chacha20poly1305::XChaCha20Poly1305;
use chacha20poly1305::XNonce;
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};

const VAULT_MAGIC: &[u8; 4] = b"RTVL";
const VAULT_VERSION: u16 = 2;
pub(crate) const AGENT_AUDIT_LIMIT: usize = 250;

pub(crate) struct VaultKey([u8; 32]);

impl Drop for VaultKey {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.0.zeroize();
    }
}

impl VaultKey {
    pub(crate) fn derive(password: &str) -> Self {
        let mut key = [0u8; 32];
        let hash = blake3::hash(b"rterm-vault-v2");
        let salt: &[u8; 16] = hash.as_bytes()[..16].try_into().unwrap();
        argon2::Argon2::default()
            .hash_password_into(password.as_bytes(), salt, &mut key)
            .expect("argon2 key derivation failed");
        Self(key)
    }
}

#[derive(Default)]
pub(crate) struct Vault {
    pub(crate) profiles: Vec<Profile>,
    pub(crate) keys: Vec<SshKey>,
    pub(crate) agent_audit: VecDeque<AgentAuditEntry>,
    pub(crate) openai_tunnel: Option<crate::tunnel::Profile>,
}

/// Resolve user home on all supported platforms
pub(crate) fn home_dir() -> std::path::PathBuf {
    #[cfg(windows)]
    {
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            return std::path::PathBuf::from(profile);
        }
        if let (Some(drive), Some(path)) =
            (std::env::var_os("HOMEDRIVE"), std::env::var_os("HOMEPATH"))
        {
            let mut home = std::path::PathBuf::from(drive);
            home.push(path);
            return home;
        }
    }

    // HOME can still be supplied by shells such as Git Bash on Windows
    if let Some(home) = std::env::var_os("HOME") {
        return std::path::PathBuf::from(home);
    }
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

pub(crate) fn expand_local_path(path: &str) -> std::path::PathBuf {
    let path = path.trim();
    if path == "~" {
        return home_dir();
    }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        return home_dir().join(rest);
    }
    std::path::PathBuf::from(path)
}

pub(crate) fn default_download_dir() -> std::path::PathBuf {
    home_dir().join("Downloads")
}

fn legacy_config_dirs() -> Vec<std::path::PathBuf> {
    // Migrate config from legacy working directory locations
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(std::path::Path::to_path_buf);
        for _ in 0..5 {
            let Some(current) = dir else { break };
            candidates.push(current.join("rterm"));
            dir = current.parent().map(std::path::Path::to_path_buf);
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("rterm"));
    }
    // Include Git Bash and Unix style Windows paths
    candidates.push(home_dir().join(".config").join("rterm"));
    candidates
}

fn migrate_legacy_config(config_dir: &std::path::Path) {
    let marker = config_dir.join(".legacy-migrated");
    if marker.exists() {
        return;
    }
    let names = ["vault.dat", "settings.json"];
    let candidates = legacy_config_dirs();
    for name in names {
        let destination = config_dir.join(name);
        if destination.exists() {
            continue;
        }
        for candidate in &candidates {
            let source = candidate.join(name);
            if source == destination || !source.is_file() {
                continue;
            }
            if std::fs::create_dir_all(config_dir).is_ok()
                && std::fs::copy(&source, &destination).is_ok()
            {
                break;
            }
        }
    }
    // Mark migration complete to prevent rediscovery
    let _ = std::fs::create_dir_all(config_dir);
    let _ = std::fs::write(marker, b"1");
}

pub(crate) fn get_config_dir() -> std::path::PathBuf {
    let config_dir = if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| home_dir().join("AppData").join("Roaming"))
            .join("rterm")
    } else if cfg!(target_os = "macos") {
        home_dir()
            .join("Library")
            .join("Application Support")
            .join("rterm")
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".config"))
            .join("rterm")
    };
    migrate_legacy_config(&config_dir);
    config_dir
}

pub(crate) fn vault_exists() -> bool {
    get_config_dir().join("vault.dat").exists()
}

// Backups use the existing encrypted vault format, not a plaintext export.
const MAX_VAULT_BACKUP: u64 = 128 * 1024 * 1024;

fn read_vault_backup(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = std::fs::metadata(path).map_err(|e| format!("Open vault: {e}"))?;
    if !metadata.is_file() || metadata.len() > MAX_VAULT_BACKUP {
        return Err("Vault backup must be a regular file no larger than 128 MiB".into());
    }
    let file = std::fs::File::open(path).map_err(|e| format!("Open vault: {e}"))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Vault backup must be a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_VAULT_BACKUP + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Read vault: {e}"))?;
    if bytes.len() as u64 > MAX_VAULT_BACKUP {
        return Err("Vault backup exceeds 128 MiB".into());
    }
    if bytes.len() < 46 || &bytes[..4] != VAULT_MAGIC {
        return Err("Not an RTerm encrypted vault".into());
    }
    if u16::from_le_bytes([bytes[4], bytes[5]]) != VAULT_VERSION {
        return Err("Unsupported vault backup version".into());
    }
    Ok(bytes)
}

fn write_new_backup(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let directory = path.parent().ok_or("Missing backup directory")?;
    let mut file = tempfile::NamedTempFile::new_in(directory).map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|()| file.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    file.persist_noclobber(path)
        .map_err(|e| format!("Cannot create vault backup (choose a new filename): {e}"))?;
    Ok(())
}

pub(crate) fn export_vault(destination: &Path, key: &VaultKey) -> Result<(), String> {
    export_vault_at(&get_config_dir().join("vault.dat"), destination, key)
}

fn export_vault_at(path: &Path, destination: &Path, key: &VaultKey) -> Result<(), String> {
    let _lock = lock_storage(path)?;
    let bytes = read_vault_backup(path)?;
    decode_vault(&bytes, key)?;
    write_new_backup(destination, &bytes)
}

pub(crate) struct VaultBackup {
    bytes: Vec<u8>,
}

impl VaultBackup {
    pub(crate) fn read(path: &Path, password: &str) -> Result<Self, String> {
        let bytes = read_vault_backup(path)?;
        decode_vault(&bytes, &VaultKey::derive(password))?;
        Ok(Self { bytes })
    }
}

pub(crate) fn import_vault(
    backup: VaultBackup,
    current_key: Option<&VaultKey>,
    replace: bool,
) -> Result<Option<PathBuf>, String> {
    import_vault_at(&get_config_dir(), backup, current_key, replace)
}

fn import_vault_at(
    directory: &Path,
    backup: VaultBackup,
    current_key: Option<&VaultKey>,
    replace: bool,
) -> Result<Option<PathBuf>, String> {
    let path = directory.join("vault.dat");
    let _lock = lock_storage(&path)?;
    let previous = if path.exists() {
        if !replace {
            return Err("An existing vault must be explicitly replaced".into());
        }
        let key = current_key.ok_or("Unlock the existing vault before replacing it")?;
        let bytes = read_vault_backup(&path)?;
        decode_vault(&bytes, key)?;
        let mut file = tempfile::Builder::new()
            .prefix("vault-before-import-")
            .suffix(".rterm-vault")
            .tempfile_in(directory)
            .map_err(|e| e.to_string())?;
        file.write_all(&bytes)
            .and_then(|()| file.as_file().sync_all())
            .map_err(|e| e.to_string())?;
        let (_, saved) = file.keep().map_err(|e| e.to_string())?;
        Some((bytes, saved))
    } else {
        None
    };
    write_atomic(&path, &backup.bytes)?;
    if let Err(error) = save_settings_at(
        &directory.join("settings.json"),
        &[
            ("vault_enabled", "1".into()),
            ("ai_tunnel_autoconnect", "0".into()),
        ],
    ) {
        let rollback = match &previous {
            Some((bytes, _)) => write_atomic(&path, bytes),
            None => std::fs::remove_file(&path).map_err(|e| e.to_string()),
        };
        rollback.map_err(|e| format!("Vault settings failed: {error}; rollback failed: {e}"))?;
        return Err(error);
    }
    Ok(previous.map(|(_, saved)| saved))
}

pub(crate) fn save_vault(
    sessions_data: &[Profile],
    keys_data: &[SshKey],
    key: &VaultKey,
    create: bool,
) -> Result<(), String> {
    update_vault(&get_config_dir().join("vault.dat"), key, create, |vault| {
        vault.profiles = sessions_data.to_vec();
        vault.keys = keys_data.to_vec();
    })?;
    Ok(())
}

pub(crate) fn save_agent_audit(
    key: &VaultKey,
    entries: impl Iterator<Item = AgentAuditEntry>,
) -> Result<VecDeque<AgentAuditEntry>, String> {
    update_vault(&get_config_dir().join("vault.dat"), key, false, |vault| {
        for entry in entries {
            if let Some(existing) = vault
                .agent_audit
                .iter_mut()
                .find(|item| item.id == entry.id)
            {
                *existing = entry;
            } else {
                vault.agent_audit.push_front(entry);
            }
        }
        vault
            .agent_audit
            .make_contiguous()
            .sort_by_key(|entry| std::cmp::Reverse(entry.submitted_ms));
        vault.agent_audit.truncate(AGENT_AUDIT_LIMIT);
    })
    .map(|vault| vault.agent_audit)
}

pub(crate) fn save_openai_tunnel(
    key: &VaultKey,
    profile: Option<crate::tunnel::Profile>,
) -> Result<(), String> {
    update_vault(&get_config_dir().join("vault.dat"), key, false, |vault| {
        vault.openai_tunnel = profile;
    })?;
    Ok(())
}

fn update_vault(
    path: &Path,
    key: &VaultKey,
    create: bool,
    update: impl FnOnce(&mut Vault),
) -> Result<Vault, String> {
    // Vault writers replace the data file atomically. Lock a separate file so
    // another RTerm window cannot replace profiles or audit history mid-write.
    let _lock = lock_storage(path)?;
    let mut vault = if create {
        if path.exists() {
            return Err("Vault already exists. Unlock it before saving.".into());
        }
        Vault::default()
    } else {
        decode_vault(
            &std::fs::read(path).map_err(|error| error.to_string())?,
            key,
        )?
    };
    update(&mut vault);
    let bytes = encode_vault(&vault, key)?;
    write_atomic(path, &bytes)?;
    Ok(vault)
}

pub(crate) fn lock_storage(path: &Path) -> Result<std::fs::File, String> {
    let directory = path.parent().ok_or("Missing storage directory")?;
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))
        .map_err(|error| error.to_string())?;
    file.lock().map_err(|error| error.to_string())?;
    Ok(file)
}

fn encode_vault(vault: &Vault, key: &VaultKey) -> Result<Vec<u8>, String> {
    // Keep the old SSH session records so the vault can still be opened by Wry builds.
    let legacy = vault
        .profiles
        .iter()
        .filter_map(|profile| match &profile.connection {
            Connection::Ssh { config, .. } => {
                let mut config = config.clone();
                config.name = Some(profile.name.clone());
                config.vault_pass = None;
                Some(config)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let container = serde_json::json!({
        "v": 2,
        "sessions": legacy,
        "profiles": vault.profiles,
        "keys": vault.keys,
        "agent_audit": vault.agent_audit,
        "openai_tunnel": vault.openai_tunnel,
    });
    let json =
        zeroize::Zeroizing::new(serde_json::to_string(&container).map_err(|e| e.to_string())?);
    let cipher = XChaCha20Poly1305::new_from_slice(&key.0).unwrap();
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, json.as_bytes())
        .map_err(|e| format!("encrypt error: {:?}", e))?;
    let mut buf = Vec::new();
    buf.extend_from_slice(VAULT_MAGIC);
    buf.extend_from_slice(&VAULT_VERSION.to_le_bytes());
    buf.extend_from_slice(&nonce);
    buf.extend_from_slice(&ciphertext);
    Ok(buf)
}

pub(crate) fn load_vault(password: &str) -> Result<(Vault, VaultKey), String> {
    let config_dir = get_config_dir();
    let data = std::fs::read(config_dir.join("vault.dat")).map_err(|e| e.to_string())?;
    let key = VaultKey::derive(password);
    Ok((decode_vault(&data, &key)?, key))
}

fn decode_vault(data: &[u8], key: &VaultKey) -> Result<Vault, String> {
    if data.len() < 46 || &data[..4] != VAULT_MAGIC {
        return Err("wrong password or corrupted vault".to_string());
    }
    let nonce = XNonce::from_slice(&data[6..30]);
    let ciphertext = &data[30..];
    let cipher = XChaCha20Poly1305::new_from_slice(&key.0).unwrap();
    match cipher.decrypt(nonce, ciphertext) {
        Ok(plaintext) => {
            let json_str = zeroize::Zeroizing::new(
                String::from_utf8(plaintext).map_err(|_| "invalid utf8".to_string())?,
            );
            let mut container: serde_json::Value =
                serde_json::from_str(&json_str).map_err(|_| "invalid json".to_string())?;
            let sessions = container
                .get("profiles")
                .or_else(|| container.get("sessions"))
                .and_then(|sessions| sessions.as_array())
                .map(|sessions| {
                    sessions
                        .iter()
                        .filter_map(|session| {
                            serde_json::from_value::<Profile>(session.clone())
                                .ok()
                                .or_else(|| {
                                    serde_json::from_value::<SshConfig>(session.clone())
                                        .ok()
                                        .map(Profile::from)
                                })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let keys = container
                .get("keys")
                .and_then(|s| serde_json::from_value(s.clone()).ok())
                .unwrap_or_default();
            let agent_audit = container
                .get("agent_audit")
                .and_then(|entries| entries.as_array())
                .into_iter()
                .flatten()
                .filter_map(|entry| serde_json::from_value(entry.clone()).ok())
                .take(AGENT_AUDIT_LIMIT)
                .collect();
            let openai_tunnel = container
                .as_object_mut()
                .and_then(|map| map.remove("openai_tunnel"))
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| "Invalid OpenAI tunnel profile")?
                .flatten();
            Ok(Vault {
                profiles: sessions,
                keys,
                agent_audit,
                openai_tunnel,
            })
        }
        Err(_) => Err("wrong password".to_string()),
    }
}

fn get_settings_path() -> std::path::PathBuf {
    get_config_dir().join("settings.json")
}

pub(crate) fn save_settings(entries: &[(&str, String)]) -> Result<(), String> {
    save_settings_at(&get_settings_path(), entries)
}

fn save_settings_at(path: &Path, entries: &[(&str, String)]) -> Result<(), String> {
    // Serialize read-modify-write operations and never replace a corrupt or
    // unreadable settings file with an empty object.
    let _lock = lock_storage(path)?;
    let mut settings: serde_json::Map<String, serde_json::Value> = match std::fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|error| format!("Read settings: {error}"))?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Default::default(),
        Err(error) => return Err(format!("Read settings: {error}")),
    };
    settings.extend(
        entries
            .iter()
            .map(|(key, value)| (key.to_string(), serde_json::Value::String(value.clone()))),
    );
    write_atomic(
        path,
        serde_json::Value::Object(settings).to_string().as_bytes(),
    )
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let directory = path.parent().ok_or("Missing destination directory")?;
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(directory).map_err(|error| error.to_string())?;
    file.write_all(bytes)
        .and_then(|()| file.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    file.persist(path).map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn load_profiles() -> Vec<Profile> {
    std::fs::read(get_config_dir().join("sessions.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(crate) fn save_profiles(profiles: &[Profile]) -> Result<(), String> {
    let profiles = profiles
        .iter()
        .map(Profile::without_credentials)
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&profiles).map_err(|error| error.to_string())?;
    write_atomic(&get_config_dir().join("sessions.json"), &bytes)
}

pub(crate) fn import_vault_profiles(
    key: &VaultKey,
    incoming: Vec<Profile>,
) -> Result<(Vec<Profile>, usize), String> {
    import_vault_profiles_at(&get_config_dir().join("vault.dat"), key, incoming)
}

fn import_vault_profiles_at(
    path: &Path,
    key: &VaultKey,
    incoming: Vec<Profile>,
) -> Result<(Vec<Profile>, usize), String> {
    let mut added = 0;
    let vault = update_vault(path, key, false, |vault| {
        added = crate::session_import::append_unique(&mut vault.profiles, incoming);
    })?;
    Ok((vault.profiles, added))
}

pub(crate) fn delete_vault() -> Result<(), String> {
    let path = get_config_dir().join("vault.dat");
    let _lock = lock_storage(&path)?;
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

pub(crate) fn load_setting(key: &str) -> Option<String> {
    let path = get_settings_path();
    let settings: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())?;
    settings
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

pub(crate) fn ssh_dir() -> std::path::PathBuf {
    home_dir().join(".ssh")
}

pub(crate) fn make_key(
    name: &str,
    algo: &str,
    passphrase: &str,
    comment: &str,
) -> Result<SshKey, String> {
    use ssh_key::{
        Algorithm, EcdsaCurve, PrivateKey,
        private::{KeypairData, RsaKeypair},
    };
    let name = crate::backend::safe_name(name)?;
    let mut rng = rand::rng();
    let mut key = match algo {
        "rsa-2048" | "rsa-4096" => {
            let bits = if algo == "rsa-2048" { 2048 } else { 4096 };
            let key = RsaKeypair::random(&mut rng, bits).map_err(|error| error.to_string())?;
            PrivateKey::new(KeypairData::Rsa(key), comment)
        }
        _ => {
            let algorithm = match algo {
                "ecdsa-p256" => Algorithm::Ecdsa {
                    curve: EcdsaCurve::NistP256,
                },
                "ecdsa-p384" => Algorithm::Ecdsa {
                    curve: EcdsaCurve::NistP384,
                },
                "ecdsa-p521" => Algorithm::Ecdsa {
                    curve: EcdsaCurve::NistP521,
                },
                _ => Algorithm::Ed25519,
            };
            PrivateKey::random(&mut rng, algorithm)
        }
    }
    .map_err(|error| error.to_string())?;
    key.set_comment(comment);
    let public_key = key
        .public_key()
        .to_openssh()
        .map_err(|error| error.to_string())?;
    let encoded = if passphrase.is_empty() {
        key
    } else {
        key.encrypt(&mut rng, passphrase)
            .map_err(|error| error.to_string())?
    };
    let private_key = encoded
        .to_openssh(ssh_key::LineEnding::LF)
        .map_err(|error| error.to_string())?
        .to_string();
    Ok(SshKey {
        name: name.into(),
        public_key,
        private_key,
        key_type: algo.into(),
    })
}

pub(crate) fn parse_key(name: &str, private: &str, public: &str) -> Result<SshKey, String> {
    let name = crate::backend::safe_name(name)?;
    let private = private.trim();
    let public = public.trim();
    let derived = ssh_key::PrivateKey::from_openssh(private)
        .map(|key| key.public_key().clone())
        .or_else(|_| {
            russh::keys::decode_secret_key(private, None).map(|key| key.public_key().clone())
        })
        .map_err(|error| format!("Could not read the private key: {error}"))?;
    let key = if public.is_empty() {
        derived
    } else {
        let supplied =
            ssh_key::PublicKey::from_openssh(public).map_err(|error| error.to_string())?;
        if supplied.key_data() != derived.key_data() {
            return Err("The public key does not match the private key".into());
        }
        supplied
    };
    let public_key = key.to_openssh().map_err(|error| error.to_string())?;
    Ok(SshKey {
        name: name.into(),
        private_key: format!("{private}\n"),
        public_key,
        key_type: key.algorithm().to_string(),
    })
}

pub(crate) fn write_key(key: &SshKey) -> Result<(), String> {
    let directory = ssh_dir();
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let private = directory.join(crate::backend::safe_name(&key.name)?);
    let public = directory.join(format!("{}.pub", key.name));
    let mut private_file =
        tempfile::NamedTempFile::new_in(&directory).map_err(|error| error.to_string())?;
    let mut public_file =
        tempfile::NamedTempFile::new_in(&directory).map_err(|error| error.to_string())?;
    private_file
        .write_all(key.private_key.as_bytes())
        .map_err(|error| error.to_string())?;
    writeln!(public_file, "{}", key.public_key).map_err(|error| error.to_string())?;
    private_file
        .persist_noclobber(&private)
        .map_err(|error| error.to_string())?;
    if let Err(error) = public_file.persist_noclobber(&public) {
        let _ = std::fs::remove_file(&private);
        return Err(error.to_string());
    }
    Ok(())
}

pub(crate) fn list_keys() -> Vec<serde_json::Value> {
    let dir = ssh_dir();
    let mut keys = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return keys,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".pub")
            || name.starts_with('.')
            || entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
        {
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if !content.contains("BEGIN") || !content.contains("PRIVATE KEY") {
            continue;
        }
        let pub_path = dir.join(format!("{}.pub", name));
        let public_key = std::fs::read_to_string(&pub_path)
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let ktype = if content.contains("OPENSSH") || public_key.contains("ssh-ed25519") {
            "ed25519"
        } else if content.contains("RSA") || public_key.contains("ssh-rsa") {
            "rsa"
        } else if content.contains("EC") || public_key.contains("ecdsa") {
            "ecdsa"
        } else {
            "unknown"
        };
        keys.push(serde_json::json!({
            "name": name,
            "path": path.to_string_lossy().to_string(),
            "type": ktype,
            "public_key": public_key,
        }));
    }
    keys.sort_by(|a, b| {
        a.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or(""))
    });
    keys
}

pub(crate) fn delete_key(path: &str) -> Result<(), String> {
    let path = PathBuf::from(path);
    if path.parent() != Some(ssh_dir().as_path()) {
        return Err("SSH key is outside the key directory".into());
    }
    std::fs::remove_file(&path).map_err(|error| error.to_string())?;
    let mut public = path.into_os_string();
    public.push(".pub");
    match std::fs::remove_file(public) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}
pub(crate) fn remove_profiles() -> Result<(), String> {
    match std::fs::remove_file(get_config_dir().join("sessions.json")) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imported_ssh_credentials_are_written_only_to_an_existing_vault() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.dat");
        let export = dir.path().join("hosts_export_test.csv");
        std::fs::write(&export, "Label,Hostname/IP,Protocol,Username,Password,KeyPath,Passphrase\nImported,test.example,ssh,alice,fixture-netcatty-password,/test/key,fixture-key-passphrase\n").unwrap();
        let incoming = || crate::session_import::read(&export).unwrap().profiles;
        let key = VaultKey([42; 32]);
        assert!(import_vault_profiles_at(&path, &key, incoming()).is_err());
        assert!(!path.exists());
        update_vault(&path, &key, true, |vault| {
            vault.profiles.push(Profile {
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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new-config/settings.json");
        save_settings_at(&path, &[("font_size", "13".into())]).unwrap();
        save_settings_at(&path, &[("sftp_show_hidden", "1".into())]).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["font_size"], "13");
        assert_eq!(saved["sftp_show_hidden"], "1");
        std::fs::write(&path, b"invalid json").unwrap();
        assert!(save_settings_at(&path, &[("font_size", "14".into())]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"invalid json");
    }

    #[test]
    fn vault_backup_roundtrip_preserves_encrypted_contents_and_previous_vault() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.dat");
        let exported = dir.path().join("portable.rterm-vault");
        let destination = dir.path().join("restored");
        let key = VaultKey::derive("fixture backup password");
        let previous_key = VaultKey([9; 32]);
        update_vault(&source, &key, true, |vault| {
            vault.profiles.push(Profile {
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
            serde_json::from_slice(&std::fs::read(destination.join("settings.json")).unwrap())
                .unwrap();
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
    fn vault_import_rejects_invalid_files_and_rolls_back_on_settings_failure() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("incoming.rterm-vault");
        let key = VaultKey([7; 32]);
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
        let replacement = encode_vault(&Vault::default(), &VaultKey([8; 32])).unwrap();
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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.dat");
        let key = VaultKey([7; 32]);
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
        assert!(decode_vault(&bytes, &VaultKey([8; 32])).is_err());
        let vault = decode_vault(&bytes, &key).unwrap();
        assert_eq!(vault.profiles[0].name, "Updated profile");
        assert_eq!(vault.agent_audit[0].command, command);
        assert_eq!(vault.agent_audit[0].exit_code, Some(7));
        let profile = vault.openai_tunnel.unwrap();
        assert_eq!(profile.id, tunnel_id);
        assert_eq!(profile.api_key, runtime_key);
        assert!(!profile.setup().contains(runtime_key));
    }
}
