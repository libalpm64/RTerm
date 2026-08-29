#![cfg_attr(windows, windows_subsystem = "windows")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use bytes::Bytes;
use russh::client::{self, Handle};
use russh::ChannelMsg;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop, EventLoopProxy},
    window::WindowBuilder,
};
use wry::http::{header::CONTENT_TYPE, Request, Response};
use wry::WebViewBuilder;
#[cfg(windows)]
use wry::{MemoryUsageLevel, WebViewExtWindows};

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
    #[serde(default)]
    compression: Option<bool>,
}

#[derive(Debug, serde::Deserialize, Clone)]
pub struct TelnetConfig {
    host: String,
    port: u16,
}

#[derive(Debug, serde::Deserialize, Clone)]
pub struct SerialConfig {
    port: String,
    baud: u32,
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
    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

enum IpcOutMsg {
    Script(String),
    TerminalData { id: u32, data: Bytes },
}

/// Append JSON string directly to IPC batch
fn append_json_string(out: &mut String, value: &str) {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    out.push('"');
    let mut segment_start = 0;
    for (index, ch) in value.char_indices() {
        let escape = match ch {
            '"' => Some("\\\""),
            '\\' => Some("\\\\"),
            '\n' => Some("\\n"),
            '\r' => Some("\\r"),
            '\t' => Some("\\t"),
            '\u{08}' => Some("\\b"),
            '\u{0c}' => Some("\\f"),
            c if c <= '\u{1f}' => {
                out.push_str(&value[segment_start..index]);
                out.push_str("\\u00");
                let code = c as u8;
                out.push(HEX[(code >> 4) as usize] as char);
                out.push(HEX[(code & 0x0f) as usize] as char);
                segment_start = index + c.len_utf8();
                None
            }
            _ => None,
        };
        if let Some(escape) = escape {
            out.push_str(&value[segment_start..index]);
            out.push_str(escape);
            segment_start = index + ch.len_utf8();
        }
    }
    out.push_str(&value[segment_start..]);
    out.push('"');
}

impl IpcOutMsg {
    fn estimated_len(&self) -> usize {
        match self {
            Self::Script(script) => script.len() + 1,
            // Reserve space for escaped terminal data
            Self::TerminalData { data, .. } => data.len().saturating_mul(6).saturating_add(96),
        }
    }

    fn append_to(self, batch: &mut String) {
        match self {
            Self::Script(script) => {
                batch.push_str(&script);
                batch.push(';');
            }
            Self::TerminalData { id, data } => {
                batch.push_str("window.__rterm_onData && window.__rterm_onData(");
                batch.push_str(&id.to_string());
                batch.push(',');
                let data_str = String::from_utf8_lossy(&data);
                append_json_string(batch, data_str.as_ref());
                batch.push_str(");");
            }
        }
    }
}

#[cfg(test)]
mod ipc_encoding_tests {
    use super::append_json_string;

    #[test]
    fn append_json_string_matches_serde_json() {
        let values = [
            "plain text",
            "quotes \" and slash \\",
            "line\nfeed\ttab\rreturn",
            "unicode: café 日本",
            "control: \u{00}\u{01}\u{1f}",
        ];
        for value in values {
            let mut actual = String::new();
            append_json_string(&mut actual, value);
            assert_eq!(actual, serde_json::to_string(value).unwrap());
        }
    }
}

fn transfer_progress_message(id: &str, text: &str, pct: f64, done: bool, error: bool) -> IpcOutMsg {
    let id_json = serde_json::to_string(id).unwrap_or_else(|_| "\"download\"".to_string());
    let text_json = serde_json::to_string(text).unwrap_or_else(|_| "\"Transfer failed\"".to_string());
    let options = if error { "{error:true}" } else if done { "{done:true}" } else { "{}" };
    IpcOutMsg::Script(format!(
        "window.__rterm_transferProgress && window.__rterm_transferProgress({}, {}, {:.1}, {})",
        id_json, text_json, pct, options
    ))
}

enum SshChannelCommand {
    Data(Vec<u8>),
    Resize(u32, u32),
}

type SshWriter = tokio::sync::mpsc::Sender<SshChannelCommand>;
type SharedSshWriters = Arc<Mutex<HashMap<u32, SshWriter>>>;

/// Wake UI event loop when transport data arrives
#[derive(Clone)]
struct IpcBus {
    tx: mpsc::SyncSender<IpcOutMsg>,
    wake: EventLoopProxy<()>,
    wake_pending: Arc<AtomicBool>,
}

type BackendCommand = (String, mpsc::Sender<String>);

struct ActiveTransferGuard(Arc<AtomicU32>);

impl Drop for ActiveTransferGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Bound backend command queue and fail busy requests
fn queue_backend_command(
    tx: &tokio::sync::mpsc::Sender<BackendCommand>,
    command: String,
    reply_tx: mpsc::Sender<String>,
) {
    if tx.try_send((command, reply_tx.clone())).is_err() {
        let _ = reply_tx.send(r#"{"success":false,"error":"backend busy"}"#.to_string());
    }
}

impl IpcBus {
    fn send(&self, message: IpcOutMsg) {
        if self.tx.send(message).is_ok()
            && !self.wake_pending.swap(true, Ordering::AcqRel)
        {
            let _ = self.wake.send_event(());
        }
    }
}

use chacha20poly1305::XChaCha20Poly1305;
use chacha20poly1305::XNonce;
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};

const VAULT_MAGIC: &[u8; 4] = b"RTVL";
const VAULT_VERSION: u16 = 2;
const MAX_ACTIVE_SFTP_TRANSFERS: u32 = 8;
const MAX_EXEC_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

fn derive_key(password: &str) -> [u8; 32] {
    let mut key = [0u8; 32];
    let hash = blake3::hash(b"rterm-vault-v2");
    let salt: &[u8; 16] = hash.as_bytes()[..16].try_into().unwrap();
    argon2::Argon2::default()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .expect("argon2 key derivation failed");
    key
}

/// Resolve user home on all supported platforms
fn home_dir() -> std::path::PathBuf {
    #[cfg(windows)]
    {
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            return std::path::PathBuf::from(profile);
        }
        if let (Some(drive), Some(path)) = (std::env::var_os("HOMEDRIVE"), std::env::var_os("HOMEPATH")) {
            let mut home = std::path::PathBuf::from(drive);
            home.push(path);
            return home;
        }
    }

    #[cfg(not(windows))]
    if let Some(home) = std::env::var_os("HOME") {
        return std::path::PathBuf::from(home);
    }

    // HOME can still be supplied by shells such as Git Bash on Windows
    if let Some(home) = std::env::var_os("HOME") {
        return std::path::PathBuf::from(home);
    }
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

fn expand_local_path(path: &str) -> std::path::PathBuf {
    let path = path.trim();
    if path == "~" {
        return home_dir();
    }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        return home_dir().join(rest);
    }
    std::path::PathBuf::from(path)
}

fn default_download_dir() -> std::path::PathBuf {
    home_dir().join("Downloads")
}

#[cfg(windows)]
fn spawn_local_shell(command: &str, cols: u16) -> Result<std::process::Child, String> {
    use std::process::{Child, Command, Stdio};

    fn spawn(
        program: &std::path::Path,
        args: &[&str],
        cols: u16,
    ) -> std::io::Result<Child> {
        let mut shell = Command::new(program);
        shell
            .args(args)
            .env("COLUMNS", cols.to_string())
            .env("LINES", "40")
            .env("TERM", "xterm-256color")
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            shell.creation_flags(0x08000000);
        }
        shell.spawn()
    }

    let powershell_command = format!("& {{ {} }} *>&1", command);
    let powershell_args = [
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        powershell_command.as_str(),
    ];
    let mut powershell_candidates = Vec::with_capacity(2);
    if let Some(windir) = std::env::var_os("WINDIR") {
        powershell_candidates.push(
            std::path::PathBuf::from(windir)
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe"),
        );
    }
    powershell_candidates.push(std::path::PathBuf::from("powershell.exe"));
    let mut powershell_errors = Vec::new();
    for candidate in powershell_candidates {
        match spawn(&candidate, &powershell_args, cols) {
            Ok(child) => return Ok(child),
            Err(error) => powershell_errors.push(format!("{}: {}", candidate.display(), error)),
        }
    }

    let pwsh_path = std::path::Path::new("pwsh.exe");
    let pwsh_error = match spawn(pwsh_path, &powershell_args, cols) {
        Ok(child) => return Ok(child),
        Err(error) => error,
    };

    let cmd_path = std::env::var_os("ComSpec")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("WINDIR").map(|windir| {
                std::path::PathBuf::from(windir)
                    .join("System32")
                    .join("cmd.exe")
            })
        })
        .unwrap_or_else(|| std::path::PathBuf::from("cmd.exe"));
    let cmd_command = format!("{} 2>&1", command);
    let cmd_args = ["/D", "/S", "/C", cmd_command.as_str()];
    spawn(&cmd_path, &cmd_args, cols).map_err(|cmd_error| {
        format!(
            "PowerShell unavailable: {}; pwsh unavailable: {}; cmd failed: {}",
            powershell_errors.join("; "),
            pwsh_error,
            cmd_error
        )
    })
}

#[cfg(not(windows))]
fn spawn_local_shell(command: &str, cols: u16) -> Result<std::process::Child, String> {
    use std::process::{Command, Stdio};

    Command::new("script")
        .arg("-q")
        .arg("/dev/null")
        .arg("sh")
        .arg("-c")
        .arg(command)
        .env("COLUMNS", cols.to_string())
        .env("LINES", "40")
        .env("TERM", "xterm-256color")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())
}

fn run_local_command(command: &str, cols: u16) -> Result<String, String> {
    use std::io::Read;

    let mut child = spawn_local_shell(command, cols)?;

    let mut stdout = child.stdout.take().ok_or_else(|| "local command stdout unavailable".to_string())?;
    let reader = std::thread::spawn(move || {
        let mut output = Vec::with_capacity(64 * 1024);
        let mut limited = (&mut stdout).take((MAX_EXEC_OUTPUT_BYTES + 1) as u64);
        let _ = limited.read_to_end(&mut output);
        output
    });
    let mut output = reader.join().map_err(|_| "local command reader failed".to_string())?;
    let truncated = output.len() > MAX_EXEC_OUTPUT_BYTES;
    if truncated {
        let _ = child.kill();
        output.truncate(MAX_EXEC_OUTPUT_BYTES);
    }
    let _ = child.wait();

    let out = String::from_utf8_lossy(&output);
    let clean = out.trim_start_matches(|c| c == '\r' || c == '\n');
    if truncated {
        Ok(format!("{}\n[output truncated at 8 MiB]", clean))
    } else {
        Ok(clean.to_string())
    }
}

#[cfg(windows)]
#[derive(serde::Serialize)]
struct WebViewProcessMemory {
    pid: u32,
    parent_pid: u32,
    kind: &'static str,
    working_set_bytes: u64,
    private_bytes: u64,
}

#[cfg(windows)]
fn webview2_process_memory() -> serde_json::Value {
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    };

    let host_pid = std::process::id();
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return serde_json::json!({
            "success": false,
            "error": "could not enumerate Windows processes"
        });
    }

    let mut entries = Vec::new();
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut has_entry = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while has_entry {
        let name_len = entry
            .szExeFile
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(entry.szExeFile.len());
        entries.push((
            entry.th32ProcessID,
            entry.th32ParentProcessID,
            String::from_utf16_lossy(&entry.szExeFile[..name_len]),
        ));
        has_entry = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    unsafe { CloseHandle(snapshot) };

    let parents: HashMap<u32, u32> = entries
        .iter()
        .map(|(pid, parent_pid, _)| (*pid, *parent_pid))
        .collect();
    let is_descendant = |pid: u32| {
        let mut current = pid;
        for _ in 0..64 {
            if current == host_pid {
                return true;
            }
            let Some(parent_pid) = parents.get(&current) else {
                return false;
            };
            if *parent_pid == current {
                return false;
            }
            current = *parent_pid;
        }
        false
    };

    let mut processes = Vec::new();
    for (pid, parent_pid, name) in entries {
        if !name.eq_ignore_ascii_case("msedgewebview2.exe") || !is_descendant(pid) {
            continue;
        }
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
                0,
                pid,
            )
        };
        if handle.is_null() {
            continue;
        }

        let mut counters = PROCESS_MEMORY_COUNTERS_EX {
            cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        let read_ok = unsafe {
            GetProcessMemoryInfo(
                handle,
                &mut counters as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
                size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            )
        } != 0;
        unsafe { CloseHandle(handle) };
        if read_ok {
            processes.push(WebViewProcessMemory {
                pid,
                parent_pid,
                kind: if parent_pid == host_pid { "browser" } else { "child" },
                working_set_bytes: counters.WorkingSetSize as u64,
                private_bytes: counters.PrivateUsage as u64,
            });
        }
    }
    processes.sort_by_key(|process| std::cmp::Reverse(process.private_bytes));
    let total_private_bytes = processes
        .iter()
        .map(|process| process.private_bytes)
        .sum::<u64>();
    let total_working_set_bytes = processes
        .iter()
        .map(|process| process.working_set_bytes)
        .sum::<u64>();
    serde_json::json!({
        "success": true,
        "supported": true,
        "host_pid": host_pid,
        "total_private_bytes": total_private_bytes,
        "total_working_set_bytes": total_working_set_bytes,
        "processes": processes,
    })
}

#[cfg(not(windows))]
fn webview2_process_memory() -> serde_json::Value {
    serde_json::json!({
        "success": true,
        "supported": false,
        "processes": [],
    })
}

fn encode_command_field(value: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(value.as_bytes())
}

fn decode_command_field(value: &str, label: &str) -> Result<String, String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|e| format!("invalid {}: {}", label, e))?;
    String::from_utf8(bytes).map_err(|e| format!("invalid {} utf-8: {}", label, e))
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
            if std::fs::create_dir_all(config_dir).is_ok() && std::fs::copy(&source, &destination).is_ok() {
                break;
            }
        }
    }
    // Mark migration complete to prevent rediscovery
    let _ = std::fs::create_dir_all(config_dir);
    let _ = std::fs::write(marker, b"1");
}

fn get_config_dir() -> std::path::PathBuf {
    let config_dir = if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| home_dir().join("AppData").join("Roaming"))
            .join("rterm")
    } else if cfg!(target_os = "macos") {
        home_dir().join("Library").join("Application Support").join("rterm")
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".config"))
            .join("rterm")
    };
    migrate_legacy_config(&config_dir);
    config_dir
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
    if data.len() < 46 || &data[..4] != VAULT_MAGIC {
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
    home_dir().join(".ssh")
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
    let encoded_key = if passphrase.is_empty() {
        key.clone()
    } else {
        key.encrypt(&mut rng, passphrase)
            .map_err(|e| format!("passphrase encrypt failed: {}", e))?
    };
    let priv_pem = encoded_key.to_openssh(ssh_key::LineEnding::LF)
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
    if let Some(s) = p.to_str() {
        let _ = std::fs::remove_file(format!("{}.pub", s));
    }
    Ok(())
}

fn serve_file(root: &PathBuf, request: Request<Vec<u8>>) -> Result<Response<Vec<u8>>, String> {
    let uri_path = request.uri().path().to_string();
    let relative = if uri_path == "/" || uri_path == "/index.html" {
        "index.html".to_string()
    } else {
        uri_path.trim_start_matches('/').to_string()
    };

    // Return an empty response for optional favicon requests
    if relative.eq_ignore_ascii_case("favicon.ico") {
        return Response::builder()
            .status(204)
            .header(CONTENT_TYPE, "image/x-icon")
            .body(Vec::new())
            .map_err(|e| format!("response builder: {}", e));
    }

    if relative.split('/').any(|c| c == "..") {
        return Err("path traversal rejected".to_string());
    }
    let file_path = root.join(&relative);
    let content = std::fs::read(&file_path).map_err(|e| format!("read {:?}: {}", file_path, e))?;
    let mimetype = if relative.ends_with(".js") {
        "text/javascript"
    } else if relative.ends_with(".css") {
        "text/css"
    } else if relative.ends_with(".html") {
        "text/html"
    } else if relative.ends_with(".png") {
        "image/png"
    } else if relative.ends_with(".svg") {
        "image/svg+xml"
    } else {
        "application/octet-stream"
    };
    Response::builder()
        .header(CONTENT_TYPE, mimetype)
        .body(content)
        .map_err(|e| format!("response builder: {}", e))
}

fn strip_windows_verbatim_prefix(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let value = path.to_string_lossy();
        if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{}", rest));
        }
        if let Some(rest) = value.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    path
}

fn main() {
    let project_root = match std::env::current_exe() {
        Ok(exe) => {
            let mut p = std::fs::canonicalize(&exe).unwrap_or(exe);
            p.pop(); p.pop(); p.pop(); p.pop();
            strip_windows_verbatim_prefix(p)
        }
        Err(_) => std::env::current_dir().unwrap_or_default(),
    };
    eprintln!("Project root: {}", project_root.display());

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();

    // Bound queued WebView scripts during output bursts
    let (ipc_tx, ipc_rx) = mpsc::sync_channel::<IpcOutMsg>(128);
    let active_sftp_transfers = Arc::new(AtomicU32::new(0));
    let active_sftp_transfers_for_runtime = active_sftp_transfers.clone();

    let event_loop = EventLoop::new();
    let ipc_bus = IpcBus {
        tx: ipc_tx,
        wake: event_loop.create_proxy(),
        wake_pending: Arc::new(AtomicBool::new(false)),
    };
    let window = WindowBuilder::new()
        .with_title("Rterm")
        .with_inner_size(tao::dpi::LogicalSize::new(1200.0, 800.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(800.0, 600.0))
        .build(&event_loop)
        .expect("Failed to create window");

    let webview = Arc::new(Mutex::new(None::<wry::WebView>));
    let webview_for_ipc = webview.clone();

    let (tokio_tx, mut tokio_rx) = tokio::sync::mpsc::channel::<BackendCommand>(256);
    // Send WebView commands directly to Tokio
    let ssh_tx_clone = tokio_tx.clone();
    let ssh_writers: SharedSshWriters = Arc::new(Mutex::new(HashMap::new()));
    let ssh_writers_for_runtime = ssh_writers.clone();
    let ssh_writers_for_handler = ssh_writers.clone();

    let ipc_tx_for_ssh_clone = ipc_bus.clone();
    let ipc_tx_for_handler = ipc_bus.clone();
    rt.spawn(async move {
        let mut sessions: HashMap<u32, Handle<SshHandler>> = HashMap::new();
        let ssh_writers = ssh_writers_for_runtime;
        let mut telnet_writers: HashMap<u32, tokio::net::tcp::OwnedWriteHalf> = HashMap::new();
        let mut serial_writers: HashMap<u32, tokio::io::WriteHalf<tokio_serial::SerialStream>> = HashMap::new();
        let mut sftp_sessions: HashMap<u32, Arc<russh_sftp::client::SftpSession>> = HashMap::new();
        let mut sftp_files: HashMap<u32, russh_sftp::client::fs::File> = HashMap::new();
        let mut sftp_file_sessions: HashMap<u32, u32> = HashMap::new();
        let mut sftp_file_counter: u32 = 0;
        let active_sftp_transfers = active_sftp_transfers_for_runtime;

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
                                let mut compression_order = vec![russh::compression::NONE];
                                if config.compression.unwrap_or(false) {
                                    compression_order = vec![
                                        russh::compression::ZLIB_LEGACY,
                                        russh::compression::ZLIB,
                                        russh::compression::NONE,
                                    ];
                                }

                                let cfg = client::Config {
                                    window_size: 8388608,
                                    maximum_packet_size: 131072,
                                    // Disable Nagle for interactive input packets
                                    nodelay: true,
                                    preferred: russh::Preferred {
                                        kex: Cow::Owned(vec![
                                            russh::kex::CURVE25519_PRE_RFC_8731,
                                            russh::kex::EXTENSION_SUPPORT_AS_CLIENT,
                                        ]),
                                        compression: Cow::Owned(compression_order),
                                        ..<_>::default()
                                    },
                                    ..<_>::default()
                                };
                                let config_ssh = Arc::new(cfg);

                                use russh::keys::decode_secret_key;
                                async fn auth_with_pem(session: &mut russh::client::Handle<SshHandler>, user: &str, pem: &str) -> Result<russh::client::AuthResult, russh::Error> {
                                    let key_pair = decode_secret_key(pem, None).map_err(|e| { eprintln!("Key decode error: {}", e); russh::Error::CouldNotReadKey })?;
                                    let key_with_alg = russh::keys::PrivateKeyWithHashAlg::new(
                                        Arc::new(key_pair),
                                        None,
                                    );
                                    session.authenticate_publickey(user, key_with_alg).await
                                }

                                let addr = format!("{}:{}", config.host.trim(), config.port);
                                match client::connect(config_ssh.clone(), addr, SshHandler).await {
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
                                            let (mut read_half, write_half) = channel.split();
                                            let (write_tx, mut write_rx) = tokio::sync::mpsc::channel(128);
                                            ssh_writers.lock().unwrap().insert(id, write_tx);
                                            tokio::spawn(async move {
                                                while let Some(command) = write_rx.recv().await {
                                                    match command {
                                                        SshChannelCommand::Data(data) => {
                                                            if write_half.data_bytes(data).await.is_err() {
                                                                break;
                                                            }
                                                        }
                                                        SshChannelCommand::Resize(cols, rows) => {
                                                            if write_half.window_change(cols, rows, 0, 0).await.is_err() {
                                                                break;
                                                            }
                                                        }
                                                    }
                                                }
                                            });
                                            let ipc_clone = ipc_tx_for_ssh_clone.clone();
                                            tokio::spawn(async move {
                                                while let Some(msg) = read_half.wait().await {
                                                    match msg {
                                                        ChannelMsg::Data { data } => {
                                                            let _ = ipc_clone.send(IpcOutMsg::TerminalData {
                                                                id,
                                                                data,
                                                            });
                                                        }
                                                        ChannelMsg::ExtendedData { data, .. } => {
                                                            let _ = ipc_clone.send(IpcOutMsg::TerminalData {
                                                                id,
                                                                data,
                                                            });
                                                        }
                                                        ChannelMsg::Eof | ChannelMsg::Close => break,
                                                        _ => {}
                                                    }
                                                }
                                            });
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
                                    let payload = parts[1].as_bytes();
                                    if let Some(tx) = ssh_writers.lock().unwrap().get(&id).cloned() {
                                        let result = tx.try_send(SshChannelCommand::Data(payload.to_vec()));
                                        let _ = reply_tx.send(if result.is_ok() {
                                            r#"{"success":true}"#.to_string()
                                        } else {
                                            r#"{"success":false,"error":"SSH writer closed"}"#.to_string()
                                        });
                                    } else if let Some(tw) = telnet_writers.get_mut(&id) {
                                        match tw.write_all(payload).await {
                                            Ok(_) => { let _ = reply_tx.send(r#"{"success":true}"#.to_string()); }
                                            Err(e) => { let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, e)); }
                                        }
                                    } else if let Some(sw) = serial_writers.get_mut(&id) {
                                        match sw.write_all(payload).await {
                                            Ok(_) => { let _ = reply_tx.send(r#"{"success":true}"#.to_string()); }
                                            Err(e) => { let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, e)); }
                                        }
                                    } else { let _ = reply_tx.send(r#"{"success":false,"error":"Session not found"}"#.to_string()); }
                                } else { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid format"}"#.to_string()); }
                            }
                            "telnet_connect" => {
                                let config: TelnetConfig = match serde_json::from_str(data) {
                                    Ok(c) => c,
                                    Err(e) => { let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, e)); continue; }
                                };
                                let addr = format!("{}:{}", config.host.trim(), config.port);
                                match tokio::net::TcpStream::connect(addr).await {
                                    Ok(stream) => {
                                        let id = SESSION_COUNTER.fetch_add(1, Ordering::SeqCst);
                                        let (mut read_half, write_half) = stream.into_split();
                                        telnet_writers.insert(id, write_half);
                                        let ipc_clone = ipc_tx_for_ssh_clone.clone();
                                        tokio::spawn(async move {
                                            let mut buf = [0u8; 8192];
                                            while let Ok(n) = read_half.read(&mut buf).await {
                                                if n == 0 { break; }
                                                let _ = ipc_clone.send(IpcOutMsg::TerminalData {
                                                    id,
                                                    data: Bytes::copy_from_slice(&buf[..n]),
                                                });
                                            }
                                        });
                                        let _ = reply_tx.send(format!(r#"{{"success":true,"id":{}}}"#, id));
                                    }
                                    Err(e) => { let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, e)); }
                                }
                            }
                            "serial_connect" => {
                                let config: SerialConfig = match serde_json::from_str(data) {
                                    Ok(c) => c,
                                    Err(e) => { let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, e)); continue; }
                                };
                                use tokio_serial::SerialPortBuilderExt;
                                match tokio_serial::new(&config.port, config.baud).open_native_async() {
                                    Ok(stream) => {
                                        let id = SESSION_COUNTER.fetch_add(1, Ordering::SeqCst);
                                        let (mut read_half, write_half) = tokio::io::split(stream);
                                        serial_writers.insert(id, write_half);
                                        let ipc_clone = ipc_tx_for_ssh_clone.clone();
                                        tokio::spawn(async move {
                                            let mut buf = [0u8; 8192];
                                            while let Ok(n) = read_half.read(&mut buf).await {
                                                if n == 0 { break; }
                                                let _ = ipc_clone.send(IpcOutMsg::TerminalData {
                                                    id,
                                                    data: Bytes::copy_from_slice(&buf[..n]),
                                                });
                                            }
                                        });
                                        let _ = reply_tx.send(format!(r#"{{"success":true,"id":{}}}"#, id));
                                    }
                                    Err(e) => { let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, e)); }
                                }
                            }
                            "disconnect" => {
                                let id: u32 = match data.parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid id"}"#.to_string()); continue; } };
                                sessions.remove(&id); ssh_writers.lock().unwrap().remove(&id);
                                telnet_writers.remove(&id); serial_writers.remove(&id);
                                sftp_sessions.remove(&id);
                                let stale_file_ids: Vec<u32> = sftp_file_sessions
                                    .iter()
                                    .filter_map(|(fid, session_id)| (*session_id == id).then_some(*fid))
                                    .collect();
                                for fid in stale_file_ids {
                                    sftp_file_sessions.remove(&fid);
                                    sftp_files.remove(&fid);
                                }
                                let _ = reply_tx.send(r#"{"success":true}"#.to_string());
                            }
                            "resize" => {
                                let parts: Vec<&str> = data.splitn(3, ':').collect();
                                if parts.len() == 3 {
                                    let id: u32 = match parts[0].parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid id"}"#.to_string()); continue; } };
                                    let cols: u32 = match parts[1].parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid cols"}"#.to_string()); continue; } };
                                    let rows: u32 = match parts[2].parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid rows"}"#.to_string()); continue; } };
                                    if let Some(tx) = ssh_writers.lock().unwrap().get(&id).cloned() {
                                        let _ = tx.try_send(SshChannelCommand::Resize(cols, rows));
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
                                            let mut output = Vec::with_capacity(64 * 1024);
                                            let mut truncated = false;
                                            loop {
                                                tokio::select! {
                                                    msg = read_half.wait() => {
                                                        match msg {
                                                            Some(ChannelMsg::Data { data }) => {
                                                                let remaining = MAX_EXEC_OUTPUT_BYTES.saturating_sub(output.len());
                                                                if data.len() > remaining {
                                                                    output.extend_from_slice(&data[..remaining]);
                                                                    truncated = true;
                                                                    break;
                                                                }
                                                                output.extend_from_slice(&data);
                                                            }
                                                            Some(ChannelMsg::Eof) | None => break,
                                                            _ => {}
                                                        }
                                                    }
                                                    _ = tokio::time::sleep(Duration::from_secs(5)) => break,
                                                }
                                            }
                                            let mut result = String::from_utf8_lossy(&output).to_string();
                                            if truncated {
                                                result.push_str("\n[output truncated at 8 MiB]");
                                            }
                                            Ok::<_, String>(result)
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
                                        Ok(s) => { sftp_sessions.insert(id, Arc::new(s)); let _ = reply_tx.send(r#"{"success":true}"#.to_string()); }
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
                                                let files: Vec<_> = read_dir.map(|e| serde_json::json!({
                                                    "name": e.file_name(),
                                                    "dir": e.file_type().is_dir(),
                                                    "size": e.metadata().len()
                                                })).collect();
                                                let _ = reply_tx.send(serde_json::json!({"success": true, "result": files}).to_string());
                                            }
                                            Err(e) => { let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, e)); }
                                        }
                                    } else { let _ = reply_tx.send(r#"{"success":false,"error":"SFTP not open, clicking the refresh wheel typically fixes this issue."}"#.to_string()); }
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
                                                sftp_file_sessions.insert(fid, id);
                                                let _ = reply_tx.send(serde_json::json!({"success": true, "handle": fid}).to_string());
                                            }
                                            Err(e) => { let _ = reply_tx.send(format!(r#"{{"success":false,"error":"{}"}}"#, e)); }
                                        }
                                    } else { let _ = reply_tx.send(r#"{"success":false,"error":"SFTP not open"}"#.to_string()); }
                                } else { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid format"}"#.to_string()); }
                            }
                            "sftp_rename_b64" => {
                                let parts: Vec<&str> = data.splitn(3, ':').collect();
                                if parts.len() == 3 {
                                    let id: u32 = match parts[0].parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid id"}"#.to_string()); continue; } };
                                    let old_path = match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, parts[1]) {
                                        Ok(v) => String::from_utf8_lossy(&v).to_string(),
                                        Err(e) => { let _ = reply_tx.send(format!(r#"{{"success":false,"error":"bad old path: {}"}}"#, e)); continue; }
                                    };
                                    let new_path = match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, parts[2]) {
                                        Ok(v) => String::from_utf8_lossy(&v).to_string(),
                                        Err(e) => { let _ = reply_tx.send(format!(r#"{{"success":false,"error":"bad new path: {}"}}"#, e)); continue; }
                                    };
                                    if let Some(sftp) = sftp_sessions.get(&id) {
                                        match sftp.rename(old_path, new_path).await {
                                            Ok(_) => { let _ = reply_tx.send(r#"{"success":true}"#.to_string()); }
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
                                sftp_file_sessions.remove(&fid);
                                let _ = reply_tx.send(r#"{"success":true}"#.to_string());
                            }
                            "sftp_download" => {
                                // Encode path fields so colons do not break framing
                                let parts: Vec<&str> = data.splitn(4, ':').collect();
                                if parts.len() == 4 {
                                    let id: u32 = match parts[0].parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid id"}"#.to_string()); continue; } };
                                    let transfer_id = match decode_command_field(parts[1], "transfer id") {
                                        Ok(v) => v,
                                        Err(e) => { let _ = reply_tx.send(serde_json::json!({"success": false, "error": e}).to_string()); continue; }
                                    };
                                    let remote_path = match decode_command_field(parts[2], "remote path") {
                                        Ok(v) => v,
                                        Err(e) => { let _ = reply_tx.send(serde_json::json!({"success": false, "error": e}).to_string()); continue; }
                                    };
                                    let save_path = match decode_command_field(parts[3], "local path") {
                                        Ok(v) => v,
                                        Err(e) => { let _ = reply_tx.send(serde_json::json!({"success": false, "error": e}).to_string()); continue; }
                                    };
                                    let ipc = ipc_tx_for_ssh_clone.clone();
                                    if let Some(sftp) = sftp_sessions.get(&id) {
                                        if active_sftp_transfers
                                            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                                                (count < MAX_ACTIVE_SFTP_TRANSFERS).then_some(count + 1)
                                            })
                                            .is_err()
                                        {
                                            let _ = reply_tx.send(r#"{"success":false,"error":"too many active SFTP transfers"}"#.to_string());
                                            continue;
                                        }
                                        let _ = reply_tx.send(r#"{"success":true}"#.to_string());
                                        let sftp_c = Arc::clone(sftp);
                                        let active_guard = ActiveTransferGuard(active_sftp_transfers.clone());
                                        // Spawn transfers without blocking the command loop
                                        tokio::spawn(async move {
                                            let _active_guard = active_guard;
                                            // Read metadata before ranged transfer
                                            match sftp_c.metadata(&remote_path).await {
                                            Err(e) => {
                                                let _ = ipc.send(transfer_progress_message(&transfer_id, &format!("Download failed: {}", e), 100.0, false, true));
                                            }
                                            Ok(meta) => {
                                                let file_size = meta.len();
                                                let fname = std::path::Path::new(&remote_path)
                                                    .file_name().map(|n| n.to_string_lossy().to_string())
                                                    .unwrap_or_else(|| remote_path.clone());

                                                if file_size == 0 {
                                                    let path = std::path::Path::new(&save_path);
                                                    let result = if let Some(parent) = path.parent() {
                                                        tokio::fs::create_dir_all(parent).await.ok();
                                                        tokio::fs::File::create(path).await.map(|_| ())
                                                    } else {
                                                        tokio::fs::File::create(path).await.map(|_| ())
                                                    };
                                                    match result {
                                                        Ok(_) => { let _ = ipc.send(transfer_progress_message(&transfer_id, &format!("Saved {} (empty file)", fname), 100.0, true, false)); }
                                                        Err(e) => { let _ = ipc.send(transfer_progress_message(&transfer_id, &format!("Download failed: {}", e), 100.0, false, true)); }
                                                    }
                                                    return;
                                                }

                                                let save_file = std::path::Path::new(&save_path);
                                                if let Some(parent) = save_file.parent() {
                                                    if let Err(e) = tokio::fs::create_dir_all(parent).await {
                                                        let _ = ipc.send(transfer_progress_message(&transfer_id, &format!("Download failed: {}", e), 100.0, false, true));
                                                        return;
                                                    }
                                                }

                                                use std::sync::atomic::{AtomicU64, Ordering};
                                                use std::sync::Arc;

                                                let num_parts = 6usize;
                                                let part_size = (file_size + num_parts as u64 - 1) / num_parts as u64;
                                                let start_time = std::time::Instant::now();
                                                let progress = Arc::new(AtomicU64::new(0));

                                                let prog = progress.clone();
                                                let ipc_p = ipc.clone();
                                                let total_sz = file_size;
                                                let fname_p = fname.clone();
                                                let transfer_id_p = transfer_id.clone();
                                                let progress_task = tokio::spawn(async move {
                                                    loop {
                                                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                                                        let done = prog.load(Ordering::Relaxed);
                                                        if done >= total_sz { break; }
                                                        let speed = done as f64 / start_time.elapsed().as_secs_f64().max(0.1) / (1024.0*1024.0);
                                                        let mb = done as f64 / (1024.0*1024.0);
                                                        let total_mb = total_sz as f64 / (1024.0*1024.0);
                                                        let pct = done as f64 * 100.0 / total_sz as f64;
                                                        let text = format!("{}: {:.1}MB / {:.1}MB @ {:.1}MB/s", fname_p, mb, total_mb, speed);
                                                        let _ = ipc_p.send(transfer_progress_message(&transfer_id_p, &text, pct, false, false));
                                                    }
                                                });

                                                let mut part_handles = Vec::new();
                                                for pi in 0..num_parts {
                                                    let sftp_part = Arc::clone(&sftp_c);
                                                    let p = remote_path.clone();
                                                    let sp = format!("{}.part{}", save_path, pi);
                                                    let begin = pi as u64 * part_size;
                                                    let end = ((pi as u64 + 1) * part_size).min(file_size);
                                                    let prog = progress.clone();

                                                    part_handles.push(tokio::spawn(async move {
                                                        let mut out = tokio::fs::File::create(&sp)
                                                            .await
                                                            .map_err(|e| format!("create part {}: {}", pi, e))?;
                                                        if begin >= end {
                                                            return Ok::<(), String>(());
                                                        }

                                                        let mut remote_file = sftp_part
                                                            .open(&p)
                                                            .await
                                                            .map_err(|e| format!("open part {}: {}", pi, e))?;
                                                        remote_file
                                                            .seek(std::io::SeekFrom::Start(begin))
                                                            .await
                                                            .map_err(|e| format!("seek part {}: {}", pi, e))?;

                                                        let mut buf = vec![0u8; 262144];
                                                        let mut pos = begin;
                                                        while pos < end {
                                                            let limit = ((end - pos) as usize).min(buf.len());
                                                            let n = remote_file
                                                                .read(&mut buf[..limit])
                                                                .await
                                                                .map_err(|e| format!("read part {}: {}", pi, e))?;
                                                            if n == 0 {
                                                                return Err(format!("read part {}: unexpected end of file", pi));
                                                            }
                                                            out.write_all(&buf[..n])
                                                                .await
                                                                .map_err(|e| format!("write part {}: {}", pi, e))?;
                                                            pos += n as u64;
                                                            prog.fetch_add(n as u64, Ordering::Relaxed);
                                                        }
                                                        Ok::<(), String>(())
                                                    }));
                                                }

                                                let mut part_error = None;
                                                for h in part_handles {
                                                    match h.await {
                                                        Ok(Ok(())) => {}
                                                        Ok(Err(e)) => {
                                                            part_error.get_or_insert(e);
                                                        }
                                                        Err(e) => { part_error.get_or_insert(format!("download task failed: {}", e)); }
                                                    };
                                                }
                                                progress_task.abort();

                                                if let Some(error) = part_error {
                                                    for pi in 0..num_parts {
                                                        let _ = tokio::fs::remove_file(format!("{}.part{}", save_path, pi)).await;
                                                    }
                                                    let _ = ipc.send(transfer_progress_message(&transfer_id, &format!("Download failed: {}", error), 100.0, false, true));
                                                    return;
                                                }

                                                // Merge parts
                                                let merge_result = match tokio::fs::File::create(&save_path).await {
                                                    Err(e) => Err(format!("create destination: {}", e)),
                                                    Ok(mut out) => {
                                                        let mut result = Ok(());
                                                    for pi in 0..num_parts {
                                                        let pp = format!("{}.part{}", save_path, pi);
                                                        match tokio::fs::File::open(&pp).await {
                                                            Err(e) => {
                                                                result = Err(format!("open downloaded part {}: {}", pi, e));
                                                                break;
                                                            }
                                                            Ok(mut part) => {
                                                                if let Err(e) = tokio::io::copy(&mut part, &mut out).await {
                                                                    result = Err(format!("merge downloaded part {}: {}", pi, e));
                                                                    break;
                                                                }
                                                            }
                                                        }
                                                    }
                                                    result
                                                }
                                                };
                                                for pi in 0..num_parts {
                                                    let _ = tokio::fs::remove_file(format!("{}.part{}", save_path, pi)).await;
                                                }
                                                if let Err(error) = merge_result {
                                                    let _ = ipc.send(transfer_progress_message(&transfer_id, &format!("Download failed: {}", error), 100.0, false, true));
                                                    return;
                                                }

                                                let elapsed = start_time.elapsed().as_secs_f64();
                                                let speed = file_size as f64 / elapsed.max(0.1) / (1024.0*1024.0);
                                                let downloaded = progress.load(Ordering::Relaxed);
                                                if downloaded >= file_size {
                                                    let _ = ipc.send(transfer_progress_message(&transfer_id, &format!("Saved {}: {:.1}MB ({:.1}MB/s)", fname, file_size as f64/(1024.0*1024.0), speed), 100.0, true, false));
                                                } else {
                                                    let _ = ipc.send(transfer_progress_message(&transfer_id, &format!("Download incomplete {}: {:.1}MB of {:.1}MB", fname, downloaded as f64/(1024.0*1024.0), file_size as f64/(1024.0*1024.0)), downloaded as f64 * 100.0 / file_size as f64, false, true));
                                                }
                                            }
                                            }
                                        });
                                    } else {
                                        let _ = reply_tx.send(r#"{"success":false,"error":"SFTP not open"}"#.to_string());
                                    }
                                } else { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid format"}"#.to_string()); }
                            }
                            "sftp_upload" => {
                                let parts: Vec<&str> = data.splitn(3, ':').collect();
                                if parts.len() == 3 {
                                    let id: u32 = match parts[0].parse() { Ok(i) => i, Err(_) => { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid id"}"#.to_string()); continue; } };
                                    let local_path = match decode_command_field(parts[1], "local path") {
                                        Ok(v) => v,
                                        Err(e) => { let _ = reply_tx.send(serde_json::json!({"success": false, "error": e}).to_string()); continue; }
                                    };
                                    let remote_path = match decode_command_field(parts[2], "remote path") {
                                        Ok(v) => v,
                                        Err(e) => { let _ = reply_tx.send(serde_json::json!({"success": false, "error": e}).to_string()); continue; }
                                    };
                                    let _ = reply_tx.send(r#"{"success":true}"#.to_string());
                                    
                                    let ipc = ipc_tx_for_ssh_clone.clone();
                                    if let Some(sftp) = sftp_sessions.get(&id) {
                                        let sftp_c = Arc::clone(sftp);
                                        tokio::spawn(async move {
                                            use tokio::io::AsyncReadExt;
                                            let dl_err = |ipc: &IpcBus, msg: String| {
                                                ipc.send(IpcOutMsg::Script(format!("window.__rterm_dlProgress && window.__rterm_dlProgress({}, 100, {{error:true}})", serde_json::to_string(&msg).unwrap_or_default())));
                                            };
                                            let total = tokio::fs::metadata(&local_path).await.map(|m| m.len()).unwrap_or(0);
                                            match tokio::fs::File::open(&local_path).await {
                                                Err(e) => dl_err(&ipc, format!("Upload failed: {}", e)),
                                                Ok(mut local_file) => match sftp_c.create(&remote_path).await {
                                                    Err(e) => dl_err(&ipc, format!("Upload failed: {}", e)),
                                                    Ok(mut remote_file) => {
                                                        let mut uploaded = 0u64;
                                                        let mut failed = false;
                                                        let mut buf = vec![0u8; 262144];
                                                        let start_time = std::time::Instant::now();
                                                        // Throttle upload progress scripts
                                                        let mut last_progress = start_time - Duration::from_secs(1);
                                                        while let Ok(n) = local_file.read(&mut buf).await {
                                                            if n == 0 { break; }
                                                            if remote_file.write_all(&buf[..n]).await.is_err() { failed = true; break; }
                                                            uploaded += n as u64;
                                                            if last_progress.elapsed() >= Duration::from_millis(100) || (total > 0 && uploaded >= total) {
                                                                last_progress = std::time::Instant::now();
                                                                let speed = uploaded as f64 / start_time.elapsed().as_secs_f64().max(0.1) / (1024.0*1024.0);
                                                                let mb = uploaded as f64 / (1024.0*1024.0);
                                                                let pct = if total > 0 { uploaded as f64 * 100.0 / total as f64 } else { 0.0 };
                                                                let _ = ipc.send(IpcOutMsg::Script(format!("window.__rterm_dlProgress && window.__rterm_dlProgress('Uploading {:.1}MB / {:.1}MB @ {:.1}MB/s', {:.1})", mb, total as f64/(1024.0*1024.0), speed, pct)));
                                                            }
                                                        }
                                                        if failed || (total > 0 && uploaded < total) {
                                                            dl_err(&ipc, format!("Upload incomplete: {:.1}MB of {:.1}MB", uploaded as f64/(1024.0*1024.0), total as f64/(1024.0*1024.0)));
                                                        } else {
                                                            let _ = ipc.send(IpcOutMsg::Script(format!("window.__rterm_dlProgress && window.__rterm_dlProgress('Uploaded {:.1}MB', 100, {{done:true}})", uploaded as f64/(1024.0*1024.0))));
                                                        }
                                                        let remote_dir = std::path::Path::new(&remote_path).parent().and_then(|p| p.to_str()).unwrap_or("/");
                                                        let dir_json = serde_json::to_string(remote_dir).unwrap_or_else(|_| "\"/\"".to_string());
                                                        let _ = ipc.send(IpcOutMsg::Script(format!("loadSftpDir({})", dir_json)));
                                                    }
                                                }
                                            }
                                        });
                                    }
                                } else { let _ = reply_tx.send(r#"{"success":false,"error":"Invalid format"}"#.to_string()); }
                            }
                            _ => { let _ = reply_tx.send(r#"{"success":false,"error":"Unknown"}"#.to_string()); }
                        }
                    }
                }
            }
        }
    });

    let (noreply_tx, noreply_rx) = mpsc::channel::<String>();
    std::thread::spawn(move || { while noreply_rx.recv().is_ok() {} });

    let root_for_protocol = project_root.clone();
    let webview_built = WebViewBuilder::new()
        .with_custom_protocol("rterm".into(), move |_webview_id, request| {
            match serve_file(&root_for_protocol, request) {
                Ok(resp) => resp.map(Into::into),
                Err(e) => {
                    eprintln!("Protocol error: {}", e);
                    Response::builder()
                        .header(CONTENT_TYPE, "text/plain")
                        .status(404)
                        .body(b"Not found".to_vec())
                        .unwrap()
                        .map(Into::into)
                }
            }
        })
        .with_url("rterm://localhost/index.html")
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
                    queue_backend_command(&ssh_tx_clone, format!("connect:{}", serde_json::to_string(&config).unwrap()), reply_tx);
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
                        let _ = ipc1.send(IpcOutMsg::Script(format!("window.__rterm_resp && window.__rterm_resp({})", serde_json::to_string(&resp_obj).unwrap_or_default())));
                    });
                }
                "ssh_shell" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let id = match args.get("id").and_then(|v| v.as_u64()) { Some(i) => i as u32, None => return };
                    let (reply_tx, reply_rx) = mpsc::channel();
                    queue_backend_command(&ssh_tx_clone, format!("shell:{}", id), reply_tx);
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
                        let _ = ipc2.send(IpcOutMsg::Script(format!("window.__rterm_resp && window.__rterm_resp({})", serde_json::to_string(&resp_obj).unwrap_or_default())));
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
                    let direct_writer = ssh_writers_for_handler
                        .lock()
                        .unwrap()
                        .get(&id)
                        .cloned();
                    if let Some(writer) = direct_writer {
                        // Send shell input through bounded writer queue
                        let _ = writer.try_send(SshChannelCommand::Data(data.as_bytes().to_vec()));
                    } else {
                        let cmd = format!("write:{}:{}", id, data);
                        queue_backend_command(&ssh_tx_clone, cmd, noreply_tx.clone());
                    }
                }
                "ssh_resize" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let id: u32 = match args.get("id").and_then(|v| v.as_u64()) { Some(i) => i as u32, None => return };
                    let cols: u32 = args.get("cols").and_then(|v| v.as_u64()).unwrap_or(80) as u32;
                    let rows: u32 = args.get("rows").and_then(|v| v.as_u64()).unwrap_or(24) as u32;
                    let direct_writer = ssh_writers_for_handler
                        .lock()
                        .unwrap()
                        .get(&id)
                        .cloned();
                    if let Some(writer) = direct_writer {
                        let success = writer
                            .try_send(SshChannelCommand::Resize(cols, rows))
                            .is_ok();
                        send_resp(&serde_json::json!({"success": success}));
                    } else {
                        let (reply_tx, reply_rx) = mpsc::channel();
                        queue_backend_command(&ssh_tx_clone, format!("resize:{}:{}:{}", id, cols, rows), reply_tx);
                        match reply_rx.recv_timeout(Duration::from_secs(1)) {
                            Ok(resp) => send_resp(&serde_json::from_str(&resp).unwrap_or(serde_json::json!({"success": false}))),
                            Err(_) => send_resp(&serde_json::json!({"success": true})),
                        }
                    }
                }
                "ssh_disconnect" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let id = match args.get("id").and_then(|v| v.as_u64()) { Some(i) => i as u32, None => return };
                    let (reply_tx, reply_rx) = mpsc::channel();
                    queue_backend_command(&ssh_tx_clone, format!("disconnect:{}", id), reply_tx);
                    match reply_rx.recv_timeout(Duration::from_secs(1)) {
                        Ok(resp) => send_resp(&serde_json::from_str(&resp).unwrap_or(serde_json::json!({"success": false}))),
                        Err(_) => send_resp(&serde_json::json!({"success": true})),
                    }
                }
                "telnet_connect" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let (reply_tx, reply_rx) = mpsc::channel();
                    queue_backend_command(&ssh_tx_clone, format!("telnet_connect:{}", serde_json::to_string(&args).unwrap()), reply_tx);
                    let ipc = ipc_tx_for_handler.clone();
                    let rid_clone = rid.clone();
                    std::thread::spawn(move || {
                        let resp = match reply_rx.recv_timeout(Duration::from_secs(5)) {
                            Ok(r) => r,
                            Err(_) => r#"{"success":false,"error":"timeout"}"#.to_string(),
                        };
                        let mut resp_val: serde_json::Value = serde_json::from_str(&resp).unwrap_or_default();
                        if let Some(ref r) = rid_clone { resp_val["_rid"] = r.clone(); }
                        let _ = ipc.send(IpcOutMsg::Script(format!("window.__rterm_resp && window.__rterm_resp({})", serde_json::to_string(&resp_val).unwrap_or_default())));
                    });
                }
                "serial_connect" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let (reply_tx, reply_rx) = mpsc::channel();
                    queue_backend_command(&ssh_tx_clone, format!("serial_connect:{}", serde_json::to_string(&args).unwrap()), reply_tx);
                    let ipc = ipc_tx_for_handler.clone();
                    let rid_clone = rid.clone();
                    std::thread::spawn(move || {
                        let resp = match reply_rx.recv_timeout(Duration::from_secs(5)) {
                            Ok(r) => r,
                            Err(_) => r#"{"success":false,"error":"timeout"}"#.to_string(),
                        };
                        let mut resp_val: serde_json::Value = serde_json::from_str(&resp).unwrap_or_default();
                        if let Some(ref r) = rid_clone { resp_val["_rid"] = r.clone(); }
                        let _ = ipc.send(IpcOutMsg::Script(format!("window.__rterm_resp && window.__rterm_resp({})", serde_json::to_string(&resp_val).unwrap_or_default())));
                    });
                }
                "sftp_open" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let id = match args.get("id").and_then(|v| v.as_u64()) { Some(i) => i as u32, None => return };
                    let (reply_tx, reply_rx) = mpsc::channel();
                    queue_backend_command(&ssh_tx_clone, format!("sftp_open:{}", id), reply_tx);
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
                    queue_backend_command(&ssh_tx_clone, format!("sftp_list:{}:{}", id, path), reply_tx);
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
                    queue_backend_command(&ssh_tx_clone, format!("sftp_open_file:{}:{}", id, path), reply_tx);
                    match reply_rx.recv_timeout(Duration::from_secs(10)) {
                        Ok(resp) => send_resp(&serde_json::from_str(&resp).unwrap_or(serde_json::json!({"success": false}))),
                        Err(_) => send_resp(&serde_json::json!({"success": false, "error": "timeout"})),
                    }
                }
                "sftp_rename" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let id = match args.get("id").and_then(|v| v.as_u64()) { Some(i) => i as u32, None => return };
                    let old_path = args.get("old_path").and_then(|v| v.as_str()).unwrap_or("");
                    let new_path = args.get("new_path").and_then(|v| v.as_str()).unwrap_or("");
                    let old_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, old_path.as_bytes());
                    let new_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, new_path.as_bytes());
                    let (reply_tx, reply_rx) = mpsc::channel();
                    queue_backend_command(&ssh_tx_clone, format!("sftp_rename_b64:{}:{}:{}", id, old_b64, new_b64), reply_tx);
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
                    queue_backend_command(&ssh_tx_clone, format!("sftp_read:{}:{}:{}", handle, size, ""), reply_tx);
                    match reply_rx.recv_timeout(Duration::from_secs(30)) {
                        Ok(resp) => send_resp(&serde_json::from_str(&resp).unwrap_or(serde_json::json!({"success": false}))),
                        Err(_) => send_resp(&serde_json::json!({"success": false, "error": "timeout"})),
                    }
                }
                "sftp_close_file" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let handle = match args.get("handle").and_then(|v| v.as_u64()) { Some(i) => i as u32, None => return };
                    let (reply_tx, reply_rx) = mpsc::channel();
                    queue_backend_command(&ssh_tx_clone, format!("sftp_close_file:{}", handle), reply_tx);
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
                    let transfer_id = args.get("transfer_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
                        .unwrap_or("download")
                        .to_string();
                    let save_dir = args.get("save_path").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
                        .map(expand_local_path)
                        .unwrap_or_else(default_download_dir);
                    let safe_filename = std::path::Path::new(&filename)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .filter(|name| !name.is_empty())
                        .unwrap_or("download");
                    let save_path = save_dir.join(safe_filename).to_string_lossy().to_string();
                    let (reply_tx, reply_rx) = mpsc::channel();
                    queue_backend_command(&ssh_tx_clone, format!(
                        "sftp_download:{}:{}:{}:{}",
                        id,
                        encode_command_field(&transfer_id),
                        encode_command_field(&path),
                        encode_command_field(&save_path)
                    ), reply_tx);
                    match reply_rx.recv_timeout(Duration::from_secs(300)) {
                        Ok(resp) => send_resp(&serde_json::from_str(&resp).unwrap_or(serde_json::json!({"success": false}))),
                        Err(_) => send_resp(&serde_json::json!({"success": false, "error": "timeout"})),
                    }
                }
                "sftp_upload" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let id: u32 = match args.get("id").and_then(|v| v.as_u64()) { Some(i) => i as u32, None => return };
                    let local_path = args.get("local_path").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let remote_path = args.get("remote_path").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let (reply_tx, reply_rx) = mpsc::channel();
                    queue_backend_command(&ssh_tx_clone, format!(
                        "sftp_upload:{}:{}:{}",
                        id,
                        encode_command_field(&local_path),
                        encode_command_field(&remote_path)
                    ), reply_tx);
                    match reply_rx.recv_timeout(Duration::from_secs(5)) {
                        Ok(resp) => send_resp(&serde_json::from_str(&resp).unwrap_or(serde_json::json!({"success": false}))),
                        Err(_) => send_resp(&serde_json::json!({"success": false, "error": "timeout"})),
                    }
                }
                "ssh_exec" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let id = match args.get("id").and_then(|v| v.as_u64()) { Some(i) => i as u32, None => return };
                    let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
                    let (reply_tx, reply_rx) = mpsc::channel();
                    queue_backend_command(&ssh_tx_clone, format!("exec:{}:{}", id, cmd), reply_tx);
                    match reply_rx.recv_timeout(Duration::from_secs(10)) {
                        Ok(resp) => send_resp(&serde_json::from_str(&resp).unwrap_or(serde_json::json!({"success": false}))),
                        Err(_) => send_resp(&serde_json::json!({"success": false, "error": "timeout"})),
                    }
                }
                "local_list" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let dir = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                    let path = expand_local_path(dir);
                    match std::fs::read_dir(&path) {
                        Ok(entries) => {
                            let mut files = Vec::new();
                            for entry in entries.flatten() {
                                let name = entry.file_name().to_string_lossy().to_string();
                                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                                let meta = entry.metadata().ok();
                                let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                                #[cfg(unix)]
                                let perm = {
                                    use std::os::unix::fs::PermissionsExt;
                                    meta.as_ref().map(|m| m.permissions().mode() & 0o777).unwrap_or(0)
                                };
                                #[cfg(not(unix))]
                                let perm = 0u32;
                                files.push(serde_json::json!({"name": name, "dir": is_dir, "size": size, "perm": perm}));
                            }
                            files.sort_by(|a, b| {
                                let ad = a.get("dir").and_then(|v| v.as_bool()).unwrap_or(false);
                                let bd = b.get("dir").and_then(|v| v.as_bool()).unwrap_or(false);
                                bd.cmp(&ad).then(a.get("name").and_then(|v| v.as_str()).unwrap_or("").cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or("")))
                            });
                            send_resp(&serde_json::json!({
                                "success": true,
                                "result": files,
                                "path": path.to_string_lossy(),
                                "home": home_dir().to_string_lossy()
                            }));
                        }
                        Err(e) => send_resp(&serde_json::json!({"success": false, "error": e.to_string()})),
                    }
                }
                "local_delete" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                    let is_dir = args.get("dir").and_then(|v| v.as_bool()).unwrap_or(false);
                    if path.trim().is_empty() {
                        send_resp(&serde_json::json!({"success": false, "error": "empty path"}));
                        return;
                    }
                    let normalized = expand_local_path(path);

                    let result = if is_dir {
                        std::fs::remove_dir_all(&normalized).or_else(|e| {
                            // On macOS, UI metadata may misclassify; fallback to file delete.
                            if normalized.is_file() {
                                std::fs::remove_file(&normalized)
                            } else {
                                Err(e)
                            }
                        })
                    } else {
                        std::fs::remove_file(&normalized).or_else(|e| {
                            // Fallback for directories/symlinks reported inconsistently.
                            if normalized.is_dir() {
                                std::fs::remove_dir_all(&normalized)
                            } else {
                                Err(e)
                            }
                        })
                    };
                    match result {
                        Ok(_) => send_resp(&serde_json::json!({"success": true})),
                        Err(e) => send_resp(&serde_json::json!({
                            "success": false,
                            "error": format!("delete failed for {}: {}", normalized.to_string_lossy(), e)
                        })),
                    }
                }
                "local_move" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let from_path = args.get("from_path").and_then(|v| v.as_str()).unwrap_or("").trim();
                    let to_path = args.get("to_path").and_then(|v| v.as_str()).unwrap_or("").trim();
                    if from_path.is_empty() || to_path.is_empty() {
                        send_resp(&serde_json::json!({"success": false, "error": "empty path"}));
                        return;
                    }
                    let from = expand_local_path(from_path);
                    let to = expand_local_path(to_path);
                    match std::fs::rename(&from, &to) {
                        Ok(_) => send_resp(&serde_json::json!({"success": true})),
                        Err(e) => send_resp(&serde_json::json!({"success": false, "error": e.to_string()})),
                    }
                }
                "local_exec" => {
                    let args = match parsed.get("args") { Some(a) => a, None => return };
                    let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let cols: u16 = args.get("cols").and_then(|v| v.as_u64()).map(|v| v as u16).unwrap_or(80);
                    let ipc_local = ipc_tx_for_handler.clone();
                    let rid_local = rid.clone();
                    std::thread::spawn(move || {
                        let resp = match run_local_command(&cmd, cols) {
                            Ok(output) => serde_json::json!({"success": true, "result": output}),
                            Err(e) => serde_json::json!({"success": false, "error": e}),
                        };
                        let mut resp_obj = resp;
                        if let Some(rid_val) = rid_local {
                            resp_obj["_rid"] = rid_val;
                        }
                        let script = format!(
                            "window.__rterm_resp && window.__rterm_resp({})",
                            serde_json::to_string(&resp_obj).unwrap_or_default()
                        );
                        ipc_local.send(IpcOutMsg::Script(script));
                    });
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
                "webview_memory" => {
                    send_resp(&webview2_process_memory());
                }
                "open_devtools" => {
                    if let Some(wv) = webview_for_ipc.lock().unwrap().as_ref() {
                        wv.open_devtools();
                        send_resp(&serde_json::json!({"success": true}));
                    } else {
                        send_resp(&serde_json::json!({"success": false, "error": "WebView unavailable"}));
                    }
                }
                _ => send_resp(&serde_json::json!({"success": false, "error": "Unknown method"})),
            }
        })
        .build(&window)
        .expect("Failed to create webview");

    #[cfg(windows)]
    let _ = webview_built.set_memory_usage_level(MemoryUsageLevel::Normal);

    *webview.lock().unwrap() = Some(webview_built);
    let webview_clone = webview.clone();
    let ipc_wake_pending = ipc_bus.wake_pending.clone();
    let mut ipc_batch = String::new();
    let mut next_msg: Option<IpcOutMsg> = None;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        loop {
            ipc_batch.clear();
            if let Some(msg) = next_msg.take() {
                msg.append_to(&mut ipc_batch);
            }
            while let Ok(msg) = ipc_rx.try_recv() {
                // Bound each script evaluation during output bursts
                if !ipc_batch.is_empty() && ipc_batch.len() + msg.estimated_len() > 256 * 1024 {
                    if let Some(wv) = webview_clone.lock().unwrap().as_ref() {
                        let _ = wv.evaluate_script(&ipc_batch);
                    }
                    ipc_batch.clear();
                }
                msg.append_to(&mut ipc_batch);
            }

            if !ipc_batch.is_empty() {
                if let Some(wv) = webview_clone.lock().unwrap().as_ref() {
                    let _ = wv.evaluate_script(&ipc_batch);
                }
            }

            // Drain queued messages before clearing wake state
            ipc_wake_pending.store(false, Ordering::Release);
            match ipc_rx.try_recv() {
                Ok(msg) => {
                    ipc_wake_pending.store(true, Ordering::Release);
                    next_msg = Some(msg);
                    continue;
                }
                Err(_) => break,
            }
        }
        if let Event::WindowEvent { event, .. } = &event {
            if let WindowEvent::Focused(focused) = event {
                #[cfg(windows)]
                {
                    let level = if *focused {
                        MemoryUsageLevel::Normal
                    } else {
                        MemoryUsageLevel::Low
                    };
                    if let Some(wv) = webview_clone.lock().unwrap().as_ref() {
                        let _ = wv.set_memory_usage_level(level);
                    }
                }
            }
        }
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            *control_flow = ControlFlow::Exit;
        }
    });
}
