use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use russh::client::{self, Handle};
use russh::{ChannelReadHalf, ChannelWriteHalf, ChannelMsg};
use tokio::runtime;
use tokio::time;
use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};
use wry::http::Request;
use wry::WebViewBuilder;

static SESSION_COUNTER: AtomicU32 = AtomicU32::new(0);

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
pub struct SshConfig {
    host: String,
    port: u16,
    user: String,
    password: Option<String>,
    key_path: Option<String>,
    #[serde(default)]
    key_name: Option<String>,
    #[serde(default)]
    vault_pass: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
pub struct SshKey {
    name: String,
    private_key: String,
    public_key: String,
    #[serde(default)]
    key_type: String,
}

struct SshHandler;

impl client::Handler for SshHandler {
    type Error = russh::Error;
    async fn check_server_key(&mut self, _server_public_key: &ssh_key::PublicKey) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

struct IpcOutMsg { script: String }

use chacha20poly1305::XChaCha20Poly1305;
use chacha20poly1305::XNonce;
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};

const VAULT_MAGIC: &[u8; 4] = b"RTVL";
const VAULT_VERSION: u16 = 2;

fn derive_key(password: &str) -> [u8; 32] {
    let mut key = [0u8; 32];
    let hash = blake3::hash(b"rterm-vault-v2");
    let salt: &[u8; 16] = hash.as_bytes()[..16].try_into().unwrap();
    let _ = argon2::Argon2::default().hash_password_into(password.as_bytes(), salt, &mut key);
    key
}

fn get_config_dir() -> std::path::PathBuf {
    std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join(".config").join("rterm"))
        .unwrap_or_else(|_| std::path::PathBuf::from("rterm"))
}

fn vault_exists() -> bool {
    get_config_dir().join("vault.dat").exists()
}

fn save_vault(sessions_data: &[SshConfig], keys_data: &[SshKey], password: &str) -> Result<(), String> {
    let config_dir = get_config_dir();
    std::fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
    let container = serde_json::json!({
        "v": 2,
        "sessions": sessions_data,
        "keys": keys_data,
    });
    let json = serde_json::to_string(&container).map_err(|e| e.to_string())?;
    let key = derive_key(password);
    let cipher = XChaCha20Poly1305::new_from_slice(&key).unwrap();
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ciphertext = cipher.encrypt(&nonce, json.as_bytes())
        .map_err(|e| format!("encrypt error: {:?}", e))?;
    let mut buf = Vec::new();
    buf.extend_from_slice(VAULT_MAGIC);
    buf.extend_from_slice(&VAULT_VERSION.to_le_bytes());
    buf.extend_from_slice(&nonce);
    buf.extend_from_slice(&ciphertext);
    std::fs::write(config_dir.join("vault.dat"), buf).map_err(|e| e.to_string())?;
    Ok(())
}

fn load_vault(password: &str) -> Result<(Vec<SshConfig>, Vec<SshKey>), String> {
    let config_dir = get_config_dir();
    let data = std::fs::read(config_dir.join("vault.dat")).map_err(|e| e.to_string())?;
    if data.len() < 6 || &data[..4] != VAULT_MAGIC {
        return Err("wrong password or corrupted vault".to_string());
    }
    let nonce = XNonce::from_slice(&data[6..30]);
    let ciphertext = &data[30..];
    let key = derive_key(password);
    let cipher = XChaCha20Poly1305::new_from_slice(&key).unwrap();
    match cipher.decrypt(nonce, ciphertext) {
        Ok(plaintext) => {
            let json_str = String::from_utf8(plaintext).map_err(|_| "invalid utf8".to_string())?;
            let container: serde_json::Value = serde_json::from_str(&json_str).map_err(|_| "invalid json".to_string())?;
            let sessions = container.get("sessions")
                .and_then(|s| serde_json::from_value(s.clone()).ok())
                .unwrap_or_default();
            let keys = container.get("keys")
                .and_then(|s| serde_json::from_value(s.clone()).ok())
                .unwrap_or_default();
            Ok((sessions, keys))
        }
        Err(_) => Err("wrong password".to_string()),
    }
}

fn get_settings_path() -> std::path::PathBuf {
    get_config_dir().join("settings.json")
}

fn save_setting(key: &str, value: &str) {
    let config_dir = get_config_dir();
    let _ = std::fs::create_dir_all(&config_dir);
    let path = get_settings_path();
    let mut settings: serde_json::Value = std::fs::read_to_string(&path)
        .ok().and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::json!({}));
    settings[key] = serde_json::json!(value);
    let _ = std::fs::write(&path, settings.to_string());
}

fn load_setting(key: &str) -> Option<String> {
    let path = get_settings_path();
    let settings: serde_json::Value = std::fs::read_to_string(&path)
        .ok().and_then(|s| serde_json::from_str(&s).ok())?;
    settings.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn ssh_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home).join(".ssh")
}

fn generate_key(name: &str, algo: &str, passphrase: &str) -> Result<serde_json::Value, String> {
    use ssh_key::PrivateKey;
    let algorithm = match algo {
        "ed25519" => ssh_key::Algorithm::Ed25519,
        "rsa-2048" => ssh_key::Algorithm::Rsa { hash: None },
        "rsa-4096" => ssh_key::Algorithm::Rsa { hash: None },
        "ecdsa-p256" => ssh_key::Algorithm::Ecdsa { curve: ssh_key::EcdsaCurve::NistP256 },
        "ecdsa-p384" => ssh_key::Algorithm::Ecdsa { curve: ssh_key::EcdsaCurve::NistP384 },
        "ecdsa-p521" => ssh_key::Algorithm::Ecdsa { curve: ssh_key::EcdsaCurve::NistP521 },
        _ => ssh_key::Algorithm::Ed25519,
    };
    let mut rng = rand::rng();
    let key = PrivateKey::random(&mut rng, algorithm)
        .map_err(|e| format!("key gen failed: {}", e))?;
    let pubkey = key.public_key().to_openssh()
        .map_err(|e| format!("pubkey failed: {}", e))?;
    let priv_pem = key.to_openssh(ssh_key::LineEnding::LF)
        .map_err(|e| format!("encode failed: {}", e))?;

    let dir = ssh_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let priv_path = dir.join(name);
    let pub_path = dir.join(format!("{}.pub", name));

    std::fs::write(&priv_path, priv_pem.as_bytes()).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(&priv_path, std::fs::Permissions::from_mode(0o600)); }
    std::fs::write(&pub_path, pubkey.as_bytes()).map_err(|e| e.to_string())?;

    let fp = key.public_key().fingerprint(ssh_key::HashAlg::Sha256);
    Ok(serde_json::json!({
        "name": name,
        "path": priv_path.to_string_lossy().to_string(),
        "type": algo,
        "public_key": pubkey,
        "fingerprint": fp.to_string(),
    }))
}

fn list_keys() -> Vec<serde_json::Value> {
    let dir = ssh_dir();
    let mut keys = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return keys,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".pub") || name.starts_with('.') || entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        // Only consider files that look like SSH private keys
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if !content.contains("BEGIN") || !content.contains("PRIVATE KEY") {
            continue;
        }
        let pub_path = dir.join(format!("{}.pub", name));
        let public_key = std::fs::read_to_string(&pub_path).ok().map(|s| s.trim().to_string()).unwrap_or_default();
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
    keys.sort_by(|a, b| a.get("name").and_then(|v| v.as_str()).unwrap_or("").cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or("")));
    keys
}

fn delete_key(path: &str) -> Result<(), String> {
    let p = std::path::PathBuf::from(path);
    let _ = std::fs::remove_file(&p);
    // remove .pub too
    if let Some(s) = p.to_str() {
        let _ = std::fs::remove_file(format!("{}.pub", s));
    }
    Ok(())
}

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();

    let html = std::fs::read_to_string("index.html").unwrap_or_else(|_| {
        std::fs::read_to_string("index.html").unwrap_or_else(|_| include_str!("../../index.html").to_string())
    });

    let (ssh_tx, ssh_rx) = mpsc::channel::<(String, mpsc::Sender<String>)>();
    let (ipc_tx, ipc_rx) = mpsc::channel::<IpcOutMsg>();
    let ipc_tx_for_ssh = ipc_tx.clone();

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("Rterm")
        .with_inner_size(tao::dpi::LogicalSize::new(1200.0, 800.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(800.0, 600.0))
        .build(&event_loop)
        .expect("Failed to create window");

    let ssh_tx_clone = ssh_tx.clone();
    let webview = Arc::new(Mutex::new(None::<wry::WebView>));
    let webview_for_ipc = webview.clone();

    // Bridge: forward sync mpsc to async tokio mpsc
    let (tokio_tx, mut tokio_rx) = tokio::sync::mpsc::unbounded_channel::<(String, mpsc::Sender<String>)>();
    let t = tokio_tx.clone();
    std::thread::spawn(move || { while let Ok(cmd) = ssh_rx.recv() { let _ = t.send(cmd); } });

    // Async SSH handler on main multi-threaded runtime
    let ipc_tx_for_ssh_clone = ipc_tx_for_ssh.clone();
    let ipc_tx_for_handler = ipc_tx_for_ssh.clone();
    rt.spawn(async move {
        let mut sessions: HashMap<u32, Handle<SshHandler>> = HashMap::new();
        let mut read_halves: HashMap<u32, ChannelReadHalf> = HashMap::new();
        let mut write_halves: HashMap<u32, ChannelWriteHalf<_>> = HashMap::new();
        let mut sftp_sessions: HashMap<u32, russh_sftp::client::SftpSession> = HashMap::new();
        let mut sftp_files: HashMap<u32, russh_sftp::client::fs::File> = HashMap::new();
        let mut sftp_file_counter: u32 = 0;

        let mut tick = tokio::time::interval(Duration::from_millis(10));

        loop {
            tokio::select! {
                Some((cmd, reply_tx)) = tokio_rx.recv() => {
                    if let Some(sep) = cmd.find(':') {
                        let (action, data) = cmd.split_at(sep);
                        let data = &data[1..];

                        match action {
                            "connect" => {
                                let config: SshConfig = match serde_json::from_str(data) {
                                    Ok(c) => c,
                                    Err(e) => { let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, e)); continue; }
                                };
                                use std::borrow::Cow;
                                let cfg = client::Config {
                                    window_size: 8388608,
                                    maximum_packet_size: 131072,
                                    preferred: russh::Preferred {
                                        kex: Cow::Owned(vec![
                                            russh::kex::CURVE25519_PRE_RFC_8731,
                                            russh::kex::EXTENSION_SUPPORT_AS_CLIENT,
                                        ]),
                                        ..<_>::default()
                                    },
                                    ..<_>::default()
                                };
                                let config_ssh = Arc::new(cfg);

                                // Helper to authenticate with a key PEM string
                                use russh::keys::decode_secret_key;
                                async fn auth_with_pem(session: &mut russh::client::Handle<SshHandler>, user: &str, pem: &str) -> Result<russh::client::AuthResult, russh::Error> {
                                    let key_pair = decode_secret_key(pem, None).map_err(|e| { eprintln!("Key decode error: {}", e); russh::Error::CouldNotReadKey })?;
                                    let key_with_alg = russh::keys::PrivateKeyWithHashAlg::new(
                                        Arc::new(key_pair),
                                        None,
                                    );
                                    session.authenticate_publickey(user, key_with_alg).await
                                }

                                match client::connect(config_ssh.clone(), (config.host.as_str(), config.port), SshHandler).await {
                                    Ok(mut session) => {
                                        let auth_result = if let (Some(key_name), Some(vp)) = (&config.key_name, &config.vault_pass) {
                                            eprintln!("Attempting vault key auth: {} for user: {}", key_name, config.user);
                                            match load_vault(vp) {
                                                Ok((_, keys)) => {
                                                    if let Some(key) = keys.iter().find(|k| k.name == *key_name) {
                                                        auth_with_pem(&mut session, &config.user, &key.private_key).await
                                                    } else {
                                                        eprintln!("Key '{}' not found in vault", key_name);
                                                        let _ = reply_tx.send(format!(r#"{{"success":false,"error":"Key '{}' not found in vault"}}"#, key_name));
                                                        continue;
                                                    }
                                                }
                                                Err(e) => {
                                                    eprintln!("Vault decrypt failed: {}", e);
                                                    let _ = reply_tx.send(format!(r#"{{"success":false,"error":"Vault error: {}"}}"#, e));
                                                    continue;
                                                }
                                            }
                                        } else if let Some(key_path) = &config.key_path {
                                            eprintln!("Attempting disk key auth with: {:?}", key_path);
                                            match russh::keys::load_secret_key(key_path, None) {
                                                Ok(key_pair) => {
                                                    let key_with_alg = russh::keys::PrivateKeyWithHashAlg::new(
                                                        Arc::new(key_pair),
                                                        None,
                                                    );
                                                    session.authenticate_publickey(&config.user, key_with_alg).await
                                                }
                                                Err(e) => {
                                                    let _ = reply_tx.send(format!(r#"{{"success":false,"error":"Key load failed: {}"}}"#, e));
                                                    continue;
                                                }
                                            }
                                        } else if let Some(password) = &config.password {
                                            session.authenticate_password(&config.user, password).await
                                        } else {
                                            let _ = reply_tx.send(r#"{"success":false,"error":"No auth"}"#.to_string());
                                            continue;
                                        };
                                        match auth_result {
                                            Ok(russh::client::AuthResult::Success) => {
                                                let id = SESSION_COUNTER.fetch_add(1, Ordering::SeqCst);
                                                sessions.insert(id, session);
                                                let _ = reply_tx.send(format!(r#"{{"success":true,"id":{}}}"#, id));
                                            }
                                            Ok(russh::client::AuthResult::Failure { remaining_methods, partial_success }) => {
                                                let msg = format!("Auth failed (methods: {:?}, partial: {})", remaining_methods, partial_success);
                                                eprintln!("{}", msg);
                                                let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, msg));
                                            }
                                            Err(e) => {
                                                let msg = format!("Auth error: {}", e);
                                                eprintln!("{}", msg);
                                                let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, msg));
                                            }
                                        }
                                    }
                                    Err(e) => { eprintln!("SSH connect error: {}", e); let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, e)); }
                                }
                            }
                            "shell" => {
                                let id: u32 = match data.parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid id"}"#.to_string()); continue; } };
                                if let Some(session) = sessions.get_mut(&id) {
                                    match session.channel_open_session().await {
                                        Ok(channel) => {
                                            let _ = channel.request_pty(true, "xterm-256color", 80, 24, 0, 0, &[]).await;
                                            let _ = channel.request_shell(true).await;
                                            let (read_half, write_half) = channel.split();
                                            read_halves.insert(id, read_half);
                                            write_halves.insert(id, write_half);
                                            let _ = reply_tx.send(r#"{"success":true,"result":"shell_ready"}"#.to_string());
                                        }
                                        Err(e) => { let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, e)); }
                                    }
                                } else { let _ = reply_tx.send(r#"{"success":false,"error":"Session not found"}"#.to_string()); }
                            }
                            "write" => {
                                let parts: Vec<&str> = data.splitn(2, ':').collect();
                                if parts.len() == 2 {
                                    let id: u32 = match parts[0].parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid id"}"#.to_string()); continue; } };
                                    if let Some(ch) = write_halves.get_mut(&id) {
                                        let cursor = std::io::Cursor::new(parts[1].to_owned());
                                        match ch.data(cursor).await {
                                            Ok(_) => { let _ = reply_tx.send(r#"{"success":true}"#.to_string()); }
                                            Err(e) => { let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, e)); }
                                        }
                                    } else { let _ = reply_tx.send(r#"{"success":false,"error":"Channel not found"}"#.to_string()); }
                                } else { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid format"}"#.to_string()); }
                            }
                            "disconnect" => {
                                let id: u32 = match data.parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid id"}"#.to_string()); continue; } };
                                sessions.remove(&id); read_halves.remove(&id); write_halves.remove(&id);
                                let _ = reply_tx.send(r#"{"success":true}"#.to_string());
                            }
                            "resize" => {
                                let parts: Vec<&str> = data.splitn(3, ':').collect();
                                if parts.len() == 3 {
                                    let id: u32 = match parts[0].parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid id"}"#.to_string()); continue; } };
                                    let cols: u32 = match parts[1].parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid cols"}"#.to_string()); continue; } };
                                    let rows: u32 = match parts[2].parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid rows"}"#.to_string()); continue; } };
                                    if let Some(ch) = write_halves.get_mut(&id) {
                                        let _ = ch.window_change(cols, rows, 0, 0).await;
                                    }
                                    let _ = reply_tx.send(r#"{"success":true}"#.to_string());
                                } else { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid format"}"#.to_string()); }
                            }
                            "exec" => {
                                let parts: Vec<&str> = data.splitn(2, ':').collect();
                                if parts.len() == 2 {
                                    let id: u32 = match parts[0].parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid id"}"#.to_string()); continue; } };
                                    if let Some(session) = sessions.get_mut(&id) {
                                        match async {
                                            let channel = session.channel_open_session().await.map_err(|e| format!("channel: {}", e))?;
                                            channel.exec(true, parts[1]).await.map_err(|e| format!("exec: {}", e))?;
                                            let (mut read_half, _) = channel.split();
                                            let mut output = vec![];
                                            loop {
                                                tokio::select! {
                                                    msg = read_half.wait() => {
                                                        match msg {
                                                            Some(ChannelMsg::Data { data }) => output.extend_from_slice(&data),
                                                            Some(ChannelMsg::Eof) | None => break,
                                                            _ => {}
                                                        }
                                                    }
                                                    _ = tokio::time::sleep(Duration::from_secs(5)) => break,
                                                }
                                            }
                                            Ok::<_, String>(String::from_utf8_lossy(&output).to_string())
                                        }.await {
                                            Ok(out) => { let _ = reply_tx.send(serde_json::json!({"success": true, "result": out}).to_string()); }
                                            Err(e) => { let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, e)); }
                                        }
                                    } else { let _ = reply_tx.send(r#"{"success":false,"error":"Session not found"}"#.to_string()); }
                                } else { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid format"}"#.to_string()); }
                            }
                            "sftp_open" => {
                                let id: u32 = match data.parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid id"}"#.to_string()); continue; } };
                                if let Some(session) = sessions.get_mut(&id) {
                                    match async {
                                        let channel = session.channel_open_session().await.map_err(|e| format!("channel: {}", e))?;
                                        channel.request_subsystem(true, "sftp").await.map_err(|e| format!("subsystem: {}", e))?;
                                        let sftp = russh_sftp::client::SftpSession::new(channel.into_stream()).await.map_err(|e| format!("sftp: {}", e))?;
                                        Ok::<_, String>(sftp)
                                    }.await {
                                        Ok(s) => { sftp_sessions.insert(id, s); let _ = reply_tx.send(r#"{"success":true}"#.to_string()); }
                                        Err(e) => { let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, e)); }
                                    }
                                } else { let _ = reply_tx.send(r#"{"success":false,"error":"Session not found"}"#.to_string()); }
                            }
                            "sftp_list" => {
                                let parts: Vec<&str> = data.splitn(2, ':').collect();
                                if parts.len() == 2 {
                                    let id: u32 = match parts[0].parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid id"}"#.to_string()); continue; } };
                                    if let Some(sftp) = sftp_sessions.get(&id) {
                                        match sftp.read_dir(parts[1]).await {
                                            Ok(read_dir) => {
                                                let files: Vec<_> = read_dir.map(|e| serde_json::json!({"name": e.file_name(), "dir": e.file_type().is_dir(), "size": e.metadata().len()})).collect();
                                                let _ = reply_tx.send(serde_json::json!({"success": true, "result": files}).to_string());
                                            }
                                            Err(e) => { let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, e)); }
                                        }
                                    } else { let _ = reply_tx.send(r#"{"success":false,"error":"SFTP not open"}"#.to_string()); }
                                } else { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid format"}"#.to_string()); }
                            }
                            "sftp_open_file" => {
                                let parts: Vec<&str> = data.splitn(2, ':').collect();
                                if parts.len() == 2 {
                                    let id: u32 = match parts[0].parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid id"}"#.to_string()); continue; } };
                                    if let Some(sftp) = sftp_sessions.get(&id) {
                                        match sftp.open(parts[1]).await {
                                            Ok(file) => {
                                                let fid = sftp_file_counter;
                                                sftp_file_counter += 1;
                                                sftp_files.insert(fid, file);
                                                let _ = reply_tx.send(serde_json::json!({"success": true, "handle": fid}).to_string());
                                            }
                                            Err(e) => { let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, e)); }
                                        }
                                    } else { let _ = reply_tx.send(r#"{"success":false,"error":"SFTP not open"}"#.to_string()); }
                                } else { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid format"}"#.to_string()); }
                            }
                            "sftp_read" => {
                                let parts: Vec<&str> = data.splitn(3, ':').collect();
                                if parts.len() == 3 {
                                    let fid: u32 = match parts[0].parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid handle"}"#.to_string()); continue; } };
                                    let size: usize = match parts[1].parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid size"}"#.to_string()); continue; } };
                                    if let Some(file) = sftp_files.get_mut(&fid) {
                                        use tokio::io::AsyncReadExt;
                                        let mut buf = vec![0u8; size.min(1024*1024)];
                                        match file.read(&mut buf).await {
                                            Ok(n) => {
                                                buf.truncate(n);
                                                let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &buf);
                                                let _ = reply_tx.send(serde_json::json!({"success": true, "data": b64, "size": n}).to_string());
                                            }
                                            Err(e) => { let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, e)); }
                                        }
                                    } else { let _ = reply_tx.send(r#"{"success":false,"error":"File handle not found"}"#.to_string()); }
                                } else { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid format"}"#.to_string()); }
                            }
                            "sftp_close_file" => {
                                let fid: u32 = match data.parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid handle"}"#.to_string()); continue; } };
                                sftp_files.remove(&fid);
                                let _ = reply_tx.send(r#"{"success":true}"#.to_string());
                            }
                            "sftp_download" => {
                                let parts: Vec<&str> = data.splitn(3, ':').collect();
                                if parts.len() == 3 {
                                    let id: u32 = match parts[0].parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid id"}"#.to_string()); continue; } };
                                    let remote_path = parts[1].to_string();
                                    let save_path = parts[2].to_string();
                                    // Respond immediately to unblock IPC
                                    let _ = reply_tx.send(r#"{"success":true}"#.to_string());
                                    // Parallel chunked download
                                    let ipc = ipc_tx_for_ssh_clone.clone();
                                    if let Some(sftp) = sftp_sessions.get(&id) {
                                        // Open first handle to get file info
                                        if let Ok(file) = sftp.open(&remote_path).await {
                                            let (raw, _) = file.raw();
                                            let raw_session = raw;
                                            let meta = file.metadata().await.ok();
                                            let file_size = meta.map(|m| m.len()).unwrap_or(0);
                                            
                                            tokio::spawn(async move {
                                                use tokio::io::AsyncWriteExt;
                                                
                                                if file_size == 0 {
                                                    let _ = ipc.send(IpcOutMsg { script: "console.log('DL: unknown file size, aborting')".to_string() });
                                                    return;
                                                }
                                                
                                                use std::sync::atomic::{AtomicU64, Ordering};
                                                use std::sync::Arc;
                                                
                                                let num_parts = 6usize;
                                                let part_size = (file_size as usize + num_parts - 1) / num_parts;
                                                let start_time = std::time::Instant::now();
                                                let progress = Arc::new(AtomicU64::new(0));
                                                
                                                // Spawn progress updater
                                                let prog = progress.clone();
                                                let ipc_p = ipc.clone();
                                                let total_sz = file_size;
                                                let progress_task = tokio::spawn(async move {
                                                    loop {
                                                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                                                        let done = prog.load(Ordering::Relaxed);
                                                        if done >= total_sz { break; }
                                                        let speed = done as f64 / start_time.elapsed().as_secs_f64().max(0.1) / (1024.0*1024.0);
                                                        let mb = done as f64 / (1024.0*1024.0);
                                                        let _ = ipc_p.send(IpcOutMsg {
                                                            script: format!("{{let p=document.getElementById('dl-progress');if(p)p.innerHTML='<span>Downloaded {:.1}MB @ {:.1}MB/s</span>'}}", mb, speed),
                                                        });
                                                    }
                                                });
                                                
                                                let mut part_handles = Vec::new();
                                                for pi in 0..num_parts {
                                                    let rs = raw_session.clone();
                                                    let p = remote_path.clone();
                                                    let sp = save_path.clone() + ".part" + &pi.to_string();
                                                    let begin = pi * part_size;
                                                    let end = ((pi + 1) * part_size).min(file_size as usize);
                                                    let prog = progress.clone();
                                                    
                                                    part_handles.push(tokio::spawn(async move {
                                                        if begin >= end { return; }
                                                        if let Ok(h) = rs.open(&p, russh_sftp::protocol::OpenFlags::READ, Default::default()).await {
                                                            let handle_str = h.handle;
                                                            let mut pos = begin as u64;
                                                            if let Ok(mut out) = tokio::fs::File::create(&sp).await {
                                                                while pos < end as u64 {
                                                                    let remaining = (end as u64 - pos).min(262144);
                                                                    match rs.read(&handle_str, pos, remaining as u32).await {
                                                                        Ok(data) if data.data.is_empty() => break,
                                                                        Ok(data) => {
                                                                            if out.write_all(&data.data).await.is_err() { break; }
                                                                            pos += data.data.len() as u64;
                                                                            prog.fetch_add(data.data.len() as u64, Ordering::Relaxed);
                                                                        }
                                                                        Err(_) => break,
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }));
                                                }
                                                
                                                for h in part_handles { let _ = h.await; }
                                                let _ = progress_task.await;
                                                
                                                // Merge parts
                                                if let Ok(mut out) = tokio::fs::File::create(&save_path).await {
                                                    for pi in 0..num_parts {
                                                        let pp = save_path.clone() + ".part" + &pi.to_string();
                                                        if let Ok(data) = tokio::fs::read(&pp).await {
                                                            let _ = out.write_all(&data).await;
                                                        }
                                                        let _ = tokio::fs::remove_file(&pp).await;
                                                    }
                                                }
                                                
                                                let elapsed = start_time.elapsed().as_secs_f64();
                                                let speed = file_size as f64 / elapsed.max(0.1) / (1024.0*1024.0);
                                                let _ = ipc.send(IpcOutMsg {
                                                    script: format!("{{let p=document.getElementById('dl-progress');if(p){{p.innerHTML='<span style=\"color:var(--green)\">Saved {:.1}MB ({:.1}MB/s)</span>';setTimeout(function(){{p.remove()}},5000)}}}}", file_size as f64/(1024.0*1024.0), speed),
                                                });
                                            });
                                        }
                                    }
                                } else { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid format"}"#.to_string()); }
                            }
                            _ => { let _ = reply_tx.send(r#"{"success":false,"error":"Unknown"}"#.to_string()); }
                        }
                    }
                }
                _ = tick.tick() => {
                    let ipc_clone = ipc_tx_for_ssh_clone.clone();
                    let mut to_remove = Vec::new();
                    for (&id, read_half) in read_halves.iter_mut() {
                        match tokio::time::timeout(Duration::from_millis(1), read_half.wait()).await {
                            Ok(Some(ChannelMsg::Data { data })) => {
                                let data_str = String::from_utf8_lossy(&data);
                                let escaped = serde_json::to_string(&data_str.as_ref()).unwrap_or_default();
                                let _ = ipc_clone.send(IpcOutMsg {
                                    script: format!("window.__rterm_onData && window.__rterm_onData({}, {})", id, escaped),
                                });
                            }
                            Ok(Some(ChannelMsg::Eof)) | Ok(None) => { to_remove.push(id); }
                            _ => {}
                        }
                    }
                    for id in to_remove { read_halves.remove(&id); }
                }
            }
        }
    });

    let webview_built = WebViewBuilder::new()
        .with_html(&html)
        .with_devtools(true)
        .with_ipc_handler(move |request: Request<String>| {
            let msg = request.body();
            let parsed = match serde_json::from_str::<serde_json::Value>(msg) {
                Ok(p) => p,
                Err(_) => return,
            };
            let method = match parsed.get("method").and_then(|m| m.as_str()) {
                Some(m) => m,
                None => return,
            };

            let rid = parsed.get("_rid").cloned();

            let send_resp = |resp: &serde_json::Value| {
                let mut resp_obj = resp.clone();
                if let Some(ref rid_val) = rid {
                    resp_obj["_rid"] = rid_val.clone();
                }
                let resp_str = serde_json::to_string(&resp_obj).unwrap_or_default();
                let script = format!("window.__rterm_resp && window.__rterm_resp({})", resp_str);
                if let Some(wv) = webview_for_ipc.lock().unwrap().as_ref() {
                    let _ = wv.evaluate_script(&script);
                }
            };

            match method {
                "ssh_connect" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let config: SshConfig = match serde_json::from_value(args.clone()) { Ok(c) => c, Err(_) => return };
                    let (reply_tx, reply_rx) = mpsc::channel();
                    ssh_tx_clone.send((format!("connect:{}", serde_json::to_string(&config).unwrap()), reply_tx)).ok();
                    let ipc1 = ipc_tx_for_handler.clone();
                    let rid1 = rid.clone();
                    std::thread::spawn(move || {
                        let resp = match reply_rx.recv_timeout(Duration::from_secs(10)) {
                            Ok(r) => r,
                            Err(_) => r#"{"success":false,"error":"timeout"}"#.to_string(),
                        };
                        let resp_val: serde_json::Value = serde_json::from_str(&resp).unwrap_or_default();
                        let mut resp_obj = resp_val;
                        if let Some(ref r) = rid1 { resp_obj["_rid"] = r.clone(); }
                        let _ = ipc1.send(IpcOutMsg { script: format!("window.__rterm_resp && window.__rterm_resp({})", serde_json::to_string(&resp_obj).unwrap_or_default()) });
                    });
                }
                "ssh_shell" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let id = match args.get("id").and_then(|v| v.as_u64()) { Some(i) => i as u32, None => return };
                    let (reply_tx, reply_rx) = mpsc::channel();
                    ssh_tx_clone.send((format!("shell:{}", id), reply_tx)).ok();
                    let ipc2 = ipc_tx_for_handler.clone();
                    let rid2 = rid.clone();
                    std::thread::spawn(move || {
                        let resp = match reply_rx.recv_timeout(Duration::from_secs(5)) {
                            Ok(r) => r,
                            Err(_) => r#"{"success":false,"error":"timeout"}"#.to_string(),
                        };
                        let resp_val: serde_json::Value = serde_json::from_str(&resp).unwrap_or_default();
                        let mut resp_obj = resp_val;
                        if let Some(ref r) = rid2 { resp_obj["_rid"] = r.clone(); }
                        let _ = ipc2.send(IpcOutMsg { script: format!("window.__rterm_resp && window.__rterm_resp({})", serde_json::to_string(&resp_obj).unwrap_or_default()) });
                    });
                }
                "ssh_write" => {
                    let args = match parsed.get("args") {
                        Some(a) => a,
                        None => return,
                    };
                    let id: u32 = match args.get("id").and_then(|v| v.as_u64()) {
                        Some(i) => i as u32,
                        None => return,
                    };
                    let data = args.get("data").and_then(|v| v.as_str()).unwrap_or("");
                    let data_owned = data.to_string();
                    let (reply_tx, reply_rx) = mpsc::channel();
                    let cmd = format!("write:{}:{}", id, data_owned);
                    if ssh_tx_clone.send((cmd, reply_tx)).is_err() {
                        send_resp(&serde_json::json!({"success": false, "error": "send failed"}));
                        return;
                    }
                    match reply_rx.recv_timeout(Duration::from_secs(1)) {
                        Ok(resp) => send_resp(&serde_json::from_str(&resp).unwrap_or(serde_json::json!({"success": false}))),
                        Err(_) => send_resp(&serde_json::json!({"success": false, "error": "timeout"})),
                    }
                }
                "ssh_resize" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let id: u32 = match args.get("id").and_then(|v| v.as_u64()) { Some(i) => i as u32, None => return };
                    let cols: u32 = args.get("cols").and_then(|v| v.as_u64()).unwrap_or(80) as u32;
                    let rows: u32 = args.get("rows").and_then(|v| v.as_u64()).unwrap_or(24) as u32;
                    let (reply_tx, reply_rx) = mpsc::channel();
                    ssh_tx_clone.send((format!("resize:{}:{}:{}", id, cols, rows), reply_tx)).ok();
                    match reply_rx.recv_timeout(Duration::from_secs(1)) {
                        Ok(resp) => send_resp(&serde_json::from_str(&resp).unwrap_or(serde_json::json!({"success": false}))),
                        Err(_) => send_resp(&serde_json::json!({"success": true})),
                    }
                }
                "ssh_disconnect" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let id = match args.get("id").and_then(|v| v.as_u64()) { Some(i) => i as u32, None => return };
                    let (reply_tx, reply_rx) = mpsc::channel();
                    ssh_tx_clone.send((format!("disconnect:{}", id), reply_tx)).ok();
                    match reply_rx.recv_timeout(Duration::from_secs(1)) {
                        Ok(resp) => send_resp(&serde_json::from_str(&resp).unwrap_or(serde_json::json!({"success": false}))),
                        Err(_) => send_resp(&serde_json::json!({"success": true})),
                    }
                }
                "sftp_open" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let id = match args.get("id").and_then(|v| v.as_u64()) { Some(i) => i as u32, None => return };
                    let (reply_tx, reply_rx) = mpsc::channel();
                    ssh_tx_clone.send((format!("sftp_open:{}", id), reply_tx)).ok();
                    match reply_rx.recv_timeout(Duration::from_secs(10)) {
                        Ok(resp) => send_resp(&serde_json::from_str(&resp).unwrap_or(serde_json::json!({"success": false}))),
                        Err(_) => send_resp(&serde_json::json!({"success": false, "error": "timeout"})),
                    }
                }
                "sftp_list" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let id = match args.get("id").and_then(|v| v.as_u64()) { Some(i) => i as u32, None => return };
                    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                    let (reply_tx, reply_rx) = mpsc::channel();
                    ssh_tx_clone.send((format!("sftp_list:{}:{}", id, path), reply_tx)).ok();
                    match reply_rx.recv_timeout(Duration::from_secs(10)) {
                        Ok(resp) => send_resp(&serde_json::from_str(&resp).unwrap_or(serde_json::json!({"success": false}))),
                        Err(_) => send_resp(&serde_json::json!({"success": false, "error": "timeout"})),
                    }
                }
                "sftp_open_file" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let id = match args.get("id").and_then(|v| v.as_u64()) { Some(i) => i as u32, None => return };
                    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                    let (reply_tx, reply_rx) = mpsc::channel();
                    ssh_tx_clone.send((format!("sftp_open_file:{}:{}", id, path), reply_tx)).ok();
                    match reply_rx.recv_timeout(Duration::from_secs(10)) {
                        Ok(resp) => send_resp(&serde_json::from_str(&resp).unwrap_or(serde_json::json!({"success": false}))),
                        Err(_) => send_resp(&serde_json::json!({"success": false, "error": "timeout"})),
                    }
                }
                "sftp_read" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let handle = match args.get("handle").and_then(|v| v.as_u64()) { Some(i) => i as u32, None => return };
                    let size: u32 = args.get("size").and_then(|v| v.as_u64()).unwrap_or(65536) as u32;
                    let (reply_tx, reply_rx) = mpsc::channel();
                    ssh_tx_clone.send((format!("sftp_read:{}:{}:{}", handle, size, ""), reply_tx)).ok();
                    match reply_rx.recv_timeout(Duration::from_secs(30)) {
                        Ok(resp) => send_resp(&serde_json::from_str(&resp).unwrap_or(serde_json::json!({"success": false}))),
                        Err(_) => send_resp(&serde_json::json!({"success": false, "error": "timeout"})),
                    }
                }
                "sftp_close_file" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let handle = match args.get("handle").and_then(|v| v.as_u64()) { Some(i) => i as u32, None => return };
                    let (reply_tx, reply_rx) = mpsc::channel();
                    ssh_tx_clone.send((format!("sftp_close_file:{}", handle), reply_tx)).ok();
                    match reply_rx.recv_timeout(Duration::from_secs(5)) {
                        Ok(resp) => send_resp(&serde_json::from_str(&resp).unwrap_or(serde_json::json!({"success": false}))),
                        Err(_) => send_resp(&serde_json::json!({"success": true})),
                    }
                }
                "sftp_download" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let id: u32 = match args.get("id").and_then(|v| v.as_u64()) { Some(i) => i as u32, None => return };
                    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let filename = args.get("filename").and_then(|v| v.as_str()).unwrap_or("download").to_string();
                    
                    // Use provided save_path or fallback to ~/Downloads
                    let save_dir = args.get("save_path").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| {
                            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                            home + "/Downloads"
                        });
                    let save_path = save_dir + "/" + &filename;
                    let (reply_tx, reply_rx) = mpsc::channel();
                    ssh_tx_clone.send((format!("sftp_download:{}:{}:{}", id, path, save_path), reply_tx)).ok();
                    match reply_rx.recv_timeout(Duration::from_secs(300)) {
                        Ok(resp) => send_resp(&serde_json::from_str(&resp).unwrap_or(serde_json::json!({"success": false}))),
                        Err(_) => send_resp(&serde_json::json!({"success": false, "error": "timeout"})),
                    }
                }
                "ssh_exec" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let id = match args.get("id").and_then(|v| v.as_u64()) { Some(i) => i as u32, None => return };
                    let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
                    let (reply_tx, reply_rx) = mpsc::channel();
                    ssh_tx_clone.send((format!("exec:{}:{}", id, cmd), reply_tx)).ok();
                    match reply_rx.recv_timeout(Duration::from_secs(10)) {
                        Ok(resp) => send_resp(&serde_json::from_str(&resp).unwrap_or(serde_json::json!({"success": false}))),
                        Err(_) => send_resp(&serde_json::json!({"success": false, "error": "timeout"})),
                    }
                }
                                "local_list" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let dir = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                    // Expand ~ to home directory
                    let expanded = if dir.starts_with("~") {
                        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                        dir.replacen('~', &home, 1)
                    } else {
                        dir.to_string()
                    };
                    // Handle relative paths - use CWD
                    let path = if expanded.starts_with('/') {
                        std::path::PathBuf::from(&expanded)
                    } else {
                        std::env::current_dir().unwrap_or_default().join(&expanded)
                    };
                    match std::fs::read_dir(&path) {
                        Ok(entries) => {
                            let mut files = Vec::new();
                            for entry in entries.flatten() {
                                let name = entry.file_name().to_string_lossy().to_string();
                                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                                files.push(serde_json::json!({"name": name, "dir": is_dir, "size": size}));
                            }
                            files.sort_by(|a, b| {
                                let ad = a.get("dir").and_then(|v| v.as_bool()).unwrap_or(false);
                                let bd = b.get("dir").and_then(|v| v.as_bool()).unwrap_or(false);
                                bd.cmp(&ad).then(a.get("name").and_then(|v| v.as_str()).unwrap_or("").cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or("")))
                            });
                            send_resp(&serde_json::json!({"success": true, "result": files, "path": dir}));
                        }
                        Err(e) => send_resp(&serde_json::json!({"success": false, "error": e.to_string()})),
                    }
                }
                "local_delete" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                    let is_dir = args.get("dir").and_then(|v| v.as_bool()).unwrap_or(false);
                    let expanded = if path.starts_with("~") {
                        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                        path.replacen('~', &home, 1)
                    } else {
                        path.to_string()
                    };
                    let result = if is_dir {
                        std::fs::remove_dir_all(&expanded)
                    } else {
                        std::fs::remove_file(&expanded)
                    };
                    match result {
                        Ok(_) => send_resp(&serde_json::json!({"success": true})),
                        Err(e) => send_resp(&serde_json::json!({"success": false, "error": e.to_string()})),
                    }
                }
                "local_exec" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
                    let cols: u16 = args.get("cols").and_then(|v| v.as_u64()).map(|v| v as u16).unwrap_or(80);
                    let resp = match std::process::Command::new("script")
                        .arg("-q").arg("/dev/null")
                        .arg("sh").arg("-c").arg(cmd)
                        .env("COLUMNS", cols.to_string())
                        .env("LINES", "40")
                        .env("TERM", "xterm-256color")
                        .output() {
                        Ok(o) => {
                            let out = String::from_utf8_lossy(&o.stdout);
                            // script prepends shell output marker, strip it
                            let clean = out.trim_start_matches(|c| c == '\r' || c == '\n');
                            serde_json::json!({"success": true, "result": clean})
                        }
                        Err(e) => serde_json::json!({"success": false, "error": e.to_string()}),
                    };
                    send_resp(&resp);
                }
                "get_test_config" => {
                    let config = serde_json::json!({
                        "ssh_host": std::env::var("RTEST_SSH_HOST").ok(),
                        "ssh_port": std::env::var("RTEST_SSH_PORT").ok().and_then(|p| p.parse::<u16>().ok()),
                        "ssh_user": std::env::var("RTEST_SSH_USER").ok(),
                        "ssh_pass": std::env::var("RTEST_SSH_PASS").ok(),
                        "ssh_key_path": std::env::var("RTEST_SSH_KEY_PATH").ok(),
                    });
                    send_resp(&serde_json::json!({"success": true, "result": config}));
                }
                "get_env" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("");
                    let val = std::env::var(key).ok();
                    send_resp(&serde_json::json!({"success": true, "result": val}));
                }
                "save_sessions" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let sessions: Vec<SshConfig> = match serde_json::from_value(args.get("sessions").cloned().unwrap_or_default()) {
                        Ok(s) => s,
                        Err(_) => { send_resp(&serde_json::json!({"success": false, "error": "invalid sessions"})); return }
                    };
                    let password = args.get("password").and_then(|v| v.as_str()).unwrap_or("");
                    // Preserve existing keys from vault (don't overwrite with empty array)
                    let keys_from_args: Vec<SshKey> = serde_json::from_value(args.get("keys").cloned().unwrap_or_default()).unwrap_or_default();
                    let keys = if keys_from_args.is_empty() {
                        load_vault(password).ok().map(|(_, k)| k).unwrap_or_default()
                    } else {
                        keys_from_args
                    };
                    match save_vault(&sessions, &keys, password) {
                        Ok(_) => send_resp(&serde_json::json!({"success": true})),
                        Err(e) => send_resp(&serde_json::json!({"success": false, "error": e})),
                    }
                }
                "load_sessions" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let password = args.get("password").and_then(|v| v.as_str()).unwrap_or("");
                    match load_vault(password) {
                        Ok((sessions, keys)) => send_resp(&serde_json::json!({"success": true, "result": sessions, "keys": keys})),
                        Err(e) => send_resp(&serde_json::json!({"success": false, "error": e})),
                    }
                }
                "import_vault_key" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let password = args.get("password").and_then(|v| v.as_str()).unwrap_or("");
                    let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("imported");
                    match load_vault(password) {
                        Ok((sessions, mut keys)) => {
                            let new_key = SshKey {
                                name: name.to_string(),
                                private_key: args.get("private_key").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                public_key: args.get("public_key").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                key_type: args.get("key_type").and_then(|v| v.as_str()).unwrap_or("ed25519").to_string(),
                            };
                            keys.push(new_key);
                            match save_vault(&sessions, &keys, password) {
                                Ok(_) => send_resp(&serde_json::json!({"success": true})),
                                Err(e) => send_resp(&serde_json::json!({"success": false, "error": e})),
                            }
                        }
                        Err(e) => send_resp(&serde_json::json!({"success": false, "error": e})),
                    }
                }
                "delete_vault_key" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let password = args.get("password").and_then(|v| v.as_str()).unwrap_or("");
                    let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    match load_vault(password) {
                        Ok((sessions, keys)) => {
                            let keys: Vec<SshKey> = keys.into_iter().filter(|k| k.name != name).collect();
                            match save_vault(&sessions, &keys, password) {
                                Ok(_) => send_resp(&serde_json::json!({"success": true})),
                                Err(e) => send_resp(&serde_json::json!({"success": false, "error": e})),
                            }
                        }
                        Err(e) => send_resp(&serde_json::json!({"success": false, "error": e})),
                    }
                }
                "vault_exists" => {
                    send_resp(&serde_json::json!({"success": true, "result": vault_exists()}));
                }
                "delete_vault" => {
                    let config_dir = get_config_dir();
                    let path = config_dir.join("vault.dat");
                    if path.exists() {
                        let _ = std::fs::remove_file(&path);
                    }
                    send_resp(&serde_json::json!({"success": true}));
                }
                "save_setting" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("");
                    let value = args.get("value").and_then(|v| v.as_str()).unwrap_or("");
                    save_setting(key, value);
                    send_resp(&serde_json::json!({"success": true}));
                }
                "load_setting" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("");
                    let value = load_setting(key);
                    send_resp(&serde_json::json!({"success": true, "result": value}));
                }
                "generate_key" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("id_ed25519");
                    let algo = args.get("type").and_then(|v| v.as_str()).unwrap_or("ed25519");
                    let passphrase = args.get("passphrase").and_then(|v| v.as_str()).unwrap_or("");
                    match generate_key(name, algo, passphrase) {
                        Ok(key_info) => send_resp(&serde_json::json!({"success": true, "result": key_info})),
                        Err(e) => send_resp(&serde_json::json!({"success": false, "error": e})),
                    }
                }
                "list_keys" => {
                    let keys = list_keys();
                    send_resp(&serde_json::json!({"success": true, "result": keys}));
                }
                "delete_key" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                    match delete_key(path) {
                        Ok(_) => send_resp(&serde_json::json!({"success": true})),
                        Err(e) => send_resp(&serde_json::json!({"success": false, "error": e})),
                    }
                }
                _ => send_resp(&serde_json::json!({"success": false, "error": "Unknown method"})),
            }
        })
        .build(&window)
        .expect("Failed to create webview");

    *webview.lock().unwrap() = Some(webview_built);
    let webview_clone = webview.clone();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Poll;
        while let Ok(msg) = ipc_rx.try_recv() {
            if let Some(wv) = webview_clone.lock().unwrap().as_ref() {
                let _ = wv.evaluate_script(&msg.script);
            }
            *control_flow = ControlFlow::Poll;
        }
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            *control_flow = ControlFlow::Exit;
        }
    });
}