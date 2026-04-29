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

    // Bridge: forward sync mpsc to async tokio mpsc
    let (tokio_tx, mut tokio_rx) = tokio::sync::mpsc::unbounded_channel::<(String, mpsc::Sender<String>)>();
    let t = tokio_tx.clone();
    std::thread::spawn(move || { while let Ok(cmd) = ssh_rx.recv() { let _ = t.send(cmd); } });

    // Async SSH handler on main multi-threaded runtime
    let ipc_tx_for_ssh_clone = ipc_tx_for_ssh.clone();
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
                                let mut cfg = Config::default();
                                cfg.window_size = 8388608;       // 8MB window for better throughput
                                cfg.maximum_packet_size = 131072; // 128KB packets
                                let config_ssh = Arc::new(cfg);
                                match client::connect(config_ssh.clone(), (config.host.as_str(), config.port), SshHandler).await {
                                    Ok(mut session) => {
                                        let auth_result = if let Some(key_path) = &config.key_path {
                                            // ... key auth ...
                                            let _ = reply_tx.send(r#"{"success":false,"error":"Key auth not implemented in async"}"#.to_string());
                                            continue;
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
                                            _ => { let _ = reply_tx.send(r#"{"success":false,"error":"Auth failed"}"#.to_string()); }
                                        }
                                    }
                                    Err(e) => { let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, e)); }
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
                                    // Spawn download in background (streaming)
                                    let ipc = ipc_tx_for_ssh_clone.clone();
                                    if let Some(sftp) = sftp_sessions.get(&id) {
                                        if let Ok(mut file) = sftp.open(&remote_path).await {
                                            tokio::spawn(async move {
                                                use tokio::io::AsyncReadExt;
                                                use tokio::io::AsyncWriteExt;
                                                let mut out = match tokio::fs::File::create(&save_path).await {
                                                    Ok(f) => f,
                                                    Err(e) => {
                                                        let _ = ipc.send(IpcOutMsg { script: format!("console.log('DL error: {}')", e) });
                                                        return;
                                                    }
                                                };
                                                let mut buf = vec![0u8; 1024*1024];
                                                let start = std::time::Instant::now();
                                                let mut total = 0u64;
                                                let mut last_progress = std::time::Instant::now();
                                                loop {
                                                    match file.read(&mut buf).await {
                                                        Ok(0) => break,
                                                        Ok(n) => {
                                                            if out.write_all(&buf[..n]).await.is_err() { break; }
                                                            total += n as u64;
                                                            if last_progress.elapsed().as_secs_f64() >= 1.0 {
                                                                let speed = total as f64 / start.elapsed().as_secs_f64().max(0.1) / (1024.0*1024.0);
                                                                let mb = total as f64 / (1024.0*1024.0);
                                                                let _ = ipc.send(IpcOutMsg {
                                                                    script: format!("{{let p=document.getElementById('dl-progress');if(p)p.innerHTML='<span>Downloaded {:.1}MB @ {:.1}MB/s</span>'}}", mb, speed),
                                                                });
                                                                last_progress = std::time::Instant::now();
                                                            }
                                                        }
                                                        Err(_) => break,
                                                    }
                                                }
                                                let elapsed = start.elapsed().as_secs_f64();
                                                let mb = total as f64 / (1024.0*1024.0);
                                                let speed = total as f64 / elapsed.max(0.1) / (1024.0*1024.0);
                                                let _ = ipc.send(IpcOutMsg {
                                                    script: format!("{{let p=document.getElementById('dl-progress');if(p){{p.innerHTML='<span style=\"color:var(--green)\">Saved {:.1}MB ({:.1}MB/s)</span>';setTimeout(function(){{p.remove()}},5000)}}}}", mb, speed),
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
                    
                    // Save to Downloads folder
                    let save_path = std::env::var("HOME").unwrap_or_else(|_| ".".to_string()) + "/Downloads/" + &filename;
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