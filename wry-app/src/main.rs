use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;
use russh::client::{self, Config, Handle};
use russh::keys::PrivateKeyWithHashAlg;
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

fn derive_key(password: &str) -> [u8; 32] {
    let p = password.as_bytes();
    let mut key = [0u8; 32];
    let len = p.len().min(32);
    key[..len].copy_from_slice(&p[..len]);
    // xor with repeating pattern to mix better
    for i in len..32 {
        key[i] = p[i % p.len().max(1)] ^ (i as u8);
    }
    key
}

fn get_config_dir() -> std::path::PathBuf {
    std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join(".config").join("rterm"))
        .unwrap_or_else(|_| std::path::PathBuf::from("rterm"))
}

fn vault_exists() -> bool {
    get_config_dir().join("vault.dat").exists()
}

fn save_vault(sessions_data: &[SshConfig], password: &str) -> Result<(), String> {
    let config_dir = get_config_dir();
    std::fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string(sessions_data).map_err(|e| e.to_string())?;
    let key = derive_key(password);
    let cipher = XChaCha20Poly1305::new_from_slice(&key).unwrap();
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ciphertext = cipher.encrypt(&nonce, json.as_bytes())
        .map_err(|e| format!("encrypt error: {:?}", e))?;
    let mut buf = Vec::new();
    buf.extend_from_slice(&nonce);
    buf.extend_from_slice(&ciphertext);
    std::fs::write(config_dir.join("vault.dat"), buf).map_err(|e| e.to_string())?;
    Ok(())
}

fn load_vault(password: &str) -> Result<Vec<SshConfig>, String> {
    let config_dir = get_config_dir();
    let data = std::fs::read(config_dir.join("vault.dat")).map_err(|e| e.to_string())?;
    if data.len() < 24 { return Err("vault too small".into()); }
    let nonce = XNonce::from_slice(&data[..24]);
    let ciphertext = &data[24..];
    let key = derive_key(password);
    let cipher = XChaCha20Poly1305::new_from_slice(&key).unwrap();
    let plaintext = cipher.decrypt(nonce, ciphertext)
        .map_err(|_| "wrong password or corrupted vault".to_string())?;
    let json = String::from_utf8(plaintext).map_err(|_| "invalid utf8".to_string())?;
    serde_json::from_str(&json).map_err(|_| "invalid json".to_string())
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

    // Spawn SSH handler thread
    let rt_clone = rt.handle().clone();
    let _handle = thread::spawn(move || {
        let runtime = runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime");
        
        let _guard = rt_clone.enter();
        
        let mut sessions: HashMap<u32, Handle<SshHandler>> = HashMap::new();
        let mut read_halves: HashMap<u32, ChannelReadHalf> = HashMap::new();
        let mut write_halves: HashMap<u32, ChannelWriteHalf<_>> = HashMap::new();

        loop {
            match ssh_rx.recv_timeout(Duration::from_millis(1)) {
                Ok((cmd, reply_tx)) => {
                    if let Some(sep) = cmd.find(':') {
                        let (action, data) = cmd.split_at(sep);
                        let data = &data[1..]; // skip :

                        match action {
                            "connect" => {
                                let config: SshConfig = match serde_json::from_str(data) {
                                    Ok(c) => c,
                                    Err(e) => {
                                        let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, e));
                                        continue;
                                    }
                                };
                                
                                let config_ssh = Arc::new(Config::default());

                                match runtime.block_on(async {
                                    client::connect(config_ssh.clone(), (config.host.as_str(), config.port), SshHandler).await
                                }) {
                                    Ok(mut session) => {
                                        let auth_result = if let Some(key_path) = &config.key_path {
                                            let key_data = std::fs::read(key_path).unwrap_or_default();
                                            let private_key = ssh_key::PrivateKey::from_bytes(&key_data);
                                            match private_key {
                                                Ok(key) => {
                                                    let key_with_hash = PrivateKeyWithHashAlg::new(Arc::new(key), None);
                                                    runtime.block_on(session.authenticate_publickey(&config.user, key_with_hash))
                                                }
                                                Err(_) => {
                                                    let _ = reply_tx.send(r#"{"success":false,"error":"Invalid key"}"#.to_string());
                                                    continue;
                                                }
                                            }
                                        } else if let Some(password) = &config.password {
                                            runtime.block_on(session.authenticate_password(&config.user, password))
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
                                            _ => {
                                                let _ = reply_tx.send(r#"{"success":false,"error":"Auth failed"}"#.to_string());
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, e));
                                    }
                                }
                            }
                            "shell" => {
                                let id: u32 = match data.parse() {
                                    Ok(i) => i,
                                    Err(_) => {
                                        let _ = reply_tx.send(r#"{"success":false,"error":"Invalid id"}"#.to_string());
                                        continue;
                                    }
                                };

                                if let Some(session) = sessions.get_mut(&id) {
                                    match runtime.block_on(session.channel_open_session()) {
                                        Ok(channel) => {
                                            let _ = runtime.block_on(channel.request_pty(true, "xterm-256color", 80, 24, 0, 0, &[]));
                                            let _ = runtime.block_on(channel.request_shell(true));

                                            let (read_half, write_half) = runtime.block_on(async { channel.split() });
                                            read_halves.insert(id, read_half);
                                            write_halves.insert(id, write_half);

                                            let _ = reply_tx.send(r#"{"success":true,"result":"shell_ready"}"#.to_string());
                                        }
                                        Err(e) => {
                                            let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, e));
                                        }
                                    }
                                } else {
                                    let _ = reply_tx.send(r#"{"success":false,"error":"Session not found"}"#.to_string());
                                }
                            }
"write" => {
                                let parts: Vec<&str> = data.splitn(2, ':').collect();
                                if parts.len() == 2 {
                                    let id: u32 = match parts[0].parse() {
                                        Ok(i) => i,
                                        Err(_) => {
                                            let _ = reply_tx.send(r#"{"success":false,"error":"Invalid id"}"#.to_string());
                                            continue;
                                        }
                                    };
                                    let data_str = parts[1];
                                    
                                    if let Some(write_half) = write_halves.get_mut(&id) {
                                        let cursor = std::io::Cursor::new(data_str.to_owned());
                                        match runtime.block_on(write_half.data(cursor)) {
                                            Ok(_) => {
                                                let _ = reply_tx.send(r#"{"success":true}"#.to_string());
                                            }
                                            Err(e) => {
                                                let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, e));
                                            }
                                        }
                                    } else {
                                        let _ = reply_tx.send(r#"{"success":false,"error":"Channel not found"}"#.to_string());
                                    }
                                } else {
                                    let _ = reply_tx.send(r#"{"success":false,"error":"Invalid format"}"#.to_string());
                                }
                            }
                            "disconnect" => {
                                let id: u32 = match data.parse() {
                                    Ok(i) => i,
                                    Err(_) => {
                                        let _ = reply_tx.send(r#"{"success":false,"error":"Invalid id"}"#.to_string());
                                        continue;
                                    }
                                };
                                sessions.remove(&id);
                                read_halves.remove(&id);
                                write_halves.remove(&id);
                                let _ = reply_tx.send(r#"{"success":true}"#.to_string());
                            }
                            "resize" => {
                                let parts: Vec<&str> = data.splitn(3, ':').collect();
                                if parts.len() == 3 {
                                    let id: u32 = match parts[0].parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid id"}"#.to_string()); continue; } };
                                    let cols: u32 = match parts[1].parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid cols"}"#.to_string()); continue; } };
                                    let rows: u32 = match parts[2].parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid rows"}"#.to_string()); continue; } };
                                    if let Some(channel) = write_halves.get_mut(&id) {
                                        let _ = runtime.block_on(channel.window_change(cols, rows, 0, 0));
                                    }
                                    let _ = reply_tx.send(r#"{"success":true}"#.to_string());
                                } else {
                                    let _ = reply_tx.send(r#"{"success":false,"error":"Invalid format"}"#.to_string());
                                }
                            }
                            _ => {
                                let _ = reply_tx.send(r#"{"success":false,"error":"Unknown"}"#.to_string());
                            }
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let ipc_clone = ipc_tx_for_ssh.clone();
                    read_halves.retain(|id, read_half| {
                        match runtime.block_on(async {
                            tokio::time::timeout(std::time::Duration::from_millis(10), read_half.wait()).await
                        }) {
                            Ok(Some(ChannelMsg::Data { data })) => {
                                let data_str = String::from_utf8_lossy(&data);
                                let escaped = serde_json::to_string(&data_str.as_ref()).unwrap_or_default();
                                let msg = IpcOutMsg {
                                    script: format!(
                                        "window.__rterm_onData && window.__rterm_onData({}, {})",
                                        id, escaped
                                    ),
                                };
                                let _ = ipc_clone.send(msg);
                                true
                            }
                            Ok(Some(ChannelMsg::Eof)) => false,
                            Ok(None) => false,
                            Err(_) => true,
                            _ => true,
                        }
                    });
                }
                Err(_) => break,
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

            let send_resp = |resp: &serde_json::Value| {
                let resp_str = serde_json::to_string(resp).unwrap_or_default();
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
                    match reply_rx.recv_timeout(Duration::from_secs(10)) {
                        Ok(resp) => send_resp(&serde_json::from_str(&resp).unwrap_or(serde_json::json!({"success": false}))),
                        Err(_) => send_resp(&serde_json::json!({"success": false, "error": "timeout"})),
                    }
                }
                "ssh_shell" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let id = match args.get("id").and_then(|v| v.as_u64()) { Some(i) => i as u32, None => return };
                    let (reply_tx, reply_rx) = mpsc::channel();
                    ssh_tx_clone.send((format!("shell:{}", id), reply_tx)).ok();
                    match reply_rx.recv_timeout(Duration::from_secs(5)) {
                        Ok(resp) => send_resp(&serde_json::from_str(&resp).unwrap_or(serde_json::json!({"success": false}))),
                        Err(_) => send_resp(&serde_json::json!({"success": false, "error": "timeout"})),
                    }
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
                "save_sessions" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let sessions: Vec<SshConfig> = match serde_json::from_value(args.get("sessions").cloned().unwrap_or_default()) {
                        Ok(s) => s,
                        Err(_) => { send_resp(&serde_json::json!({"success": false, "error": "invalid sessions"})); return }
                    };
                    let password = args.get("password").and_then(|v| v.as_str()).unwrap_or("");
                    match save_vault(&sessions, password) {
                        Ok(_) => send_resp(&serde_json::json!({"success": true})),
                        Err(e) => send_resp(&serde_json::json!({"success": false, "error": e})),
                    }
                }
                "load_sessions" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let password = args.get("password").and_then(|v| v.as_str()).unwrap_or("");
                    match load_vault(password) {
                        Ok(sessions) => send_resp(&serde_json::json!({"success": true, "result": sessions})),
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
                _ => send_resp(&serde_json::json!({"success": false, "error": "Unknown method"})),
            }
        })
        .build(&window)
        .expect("Failed to create webview");

    *webview.lock().unwrap() = Some(webview_built);
    let webview_clone = webview.clone();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
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