use crate::{agent::McpAccess, mcp};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::{
    net::{Ipv4Addr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    sync::oneshot,
};
use zeroize::Zeroize;

pub(crate) const PLATFORM_TUNNELS: &str =
    "https://platform.openai.com/settings/organization/tunnels";
pub(crate) const PLATFORM_API_KEYS: &str =
    "https://platform.openai.com/settings/organization/api-keys";
pub(crate) const GUIDE: &str = "https://developers.openai.com/api/docs/guides/secure-mcp-tunnels";
pub(crate) const DOWNLOAD: &str = "https://github.com/openai/tunnel-client/releases/latest";
pub(crate) const CHATGPT_PLUGINS: &str = "https://chatgpt.com/plugins";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Profile {
    pub(crate) id: String,
    pub(crate) api_key: String,
    pub(crate) client: PathBuf,
}

impl Drop for Profile {
    fn drop(&mut self) {
        self.api_key.zeroize();
    }
}

impl Profile {
    pub(crate) fn validate(&self) -> Result<(), String> {
        let valid = |part: &str, len| {
            part.len() == len
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        };
        let parts: Vec<_> = self.id.split('_').collect();
        if !match parts.as_slice() {
            ["tunnel", id] => valid(id, 32),
            ["tunnel", namespace, id] => valid(namespace, 4) && valid(id, 32),
            _ => false,
        } {
            return Err("Enter the complete tunnel ID from OpenAI Platform".into());
        }
        if !self.api_key.starts_with("sk-")
            || self.api_key.starts_with("sk-admin-")
            || !(16..=1024).contains(&self.api_key.len())
            || !self.api_key.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err("Enter a restricted tunnel runtime API key".into());
        }
        Ok(())
    }

    pub(crate) fn setup(&self) -> String {
        format!(
            "RTerm ChatGPT setup\n\n1. Start Agent and Connect Tunnel in RTerm. Wait until the tunnel is ready.\n2. Open {CHATGPT_PLUGINS} and choose Add custom MCP server.\n3. Name it RTerm. Set Connection to Tunnel and enter {}.\n4. Set authentication to No authentication. The tunnel is limited to its configured OpenAI organization/workspace and RTerm applies its local permissions to all calls.\n5. Create a private plugin. Test list_sessions, then run hostname only on an authorized connected SSH session.\n\nGuide: {GUIDE}",
            self.id
        )
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Status {
    Connecting,
    Ready,
    Reconnecting,
    Failed(String),
}

pub(crate) struct Running {
    access: McpAccess,
    status: Arc<Mutex<Status>>,
    stop: Option<oneshot::Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.access.stop();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Running {
    pub(crate) fn start(
        profile: Profile,
        access: McpAccess,
        context: &egui::Context,
    ) -> Result<Self, String> {
        profile.validate()?;
        let client = find_client(&profile.client)?;
        let work = tempfile::tempdir().map_err(|_| "Could not create tunnel runtime directory")?;
        let config = work.path().join("tunnel.yaml");
        std::fs::write(&config, b"config_version: 1\n")
            .map_err(|_| "Could not prepare the tunnel client configuration")?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .map_err(|err| format!("Cannot start the local MCP endpoint: {err}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|err| err.to_string())?;
        let address = listener.local_addr().map_err(|err| err.to_string())?;
        let token = zeroize::Zeroizing::new(crate::agent::random_id());
        let router = mcp::router(access.clone(), address, &token);
        let (stop, stopped) = oneshot::channel();
        let status = Arc::new(Mutex::new(Status::Connecting));
        let worker_status = status.clone();
        let worker_access = access.clone();
        let context = context.clone();
        let worker = std::thread::Builder::new().name("rterm-openai-tunnel".into()).spawn(move || {
            let update = |status| {
                let mut current = worker_status.lock().unwrap();
                if *current != status {
                    *current = status;
                    context.request_repaint();
                }
            };
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().max_blocking_threads(8).build();
            match runtime {
                Ok(runtime) => {
                    runtime.block_on(async move {
                        let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
                            update(Status::Failed("Could not start the local MCP listener".into()));
                            return;
                        };
                        let health = work.path().join("health-url");
                        let command = client_command(&client, &profile, address, &token, &config, &health);
                        tokio::select! {
                            biased;
                            _ = stopped => {},
                            _ = axum::serve(listener, router) => update(Status::Failed("Local MCP listener stopped".into())),
                            _ = supervise(command, &health, &update) => {},
                        }
                    });
                    worker_access.stop();
                    runtime.shutdown_timeout(Duration::from_millis(150));
                }
                Err(_) => {
                    worker_access.stop();
                    update(Status::Failed("Could not start the tunnel runtime".into()));
                }
            }
        }).map_err(|err| err.to_string())?;
        Ok(Self {
            access,
            status,
            stop: Some(stop),
            worker: Some(worker),
        })
    }

    pub(crate) fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }
}

fn find_client(configured: &Path) -> Result<PathBuf, String> {
    let name = if cfg!(windows) {
        "tunnel-client.exe"
    } else {
        "tunnel-client"
    };
    if !configured.as_os_str().is_empty() {
        if configured.is_absolute() && configured.is_file() {
            return Ok(configured.into());
        }
        return Err("Select the installed OpenAI tunnel-client executable".into());
    }
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|app| app.parent().map(|dir| dir.join(name)));
    let path = std::env::var_os("PATH");
    sibling
        .into_iter()
        .chain(
            path.iter()
                .flat_map(std::env::split_paths)
                .filter(|dir| dir.is_absolute())
                .map(|dir| dir.join(name)),
        )
        .find(|path| path.is_file())
        .ok_or_else(|| {
            "Download OpenAI tunnel-client and select its executable in Advanced AI Settings".into()
        })
}

fn client_command(
    client: &Path,
    profile: &Profile,
    address: SocketAddr,
    token: &str,
    config: &Path,
    health: &Path,
) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(client);
    cmd.env_clear();
    for name in [
        "SystemRoot",
        "WINDIR",
        "TEMP",
        "TMP",
        "TMPDIR",
        "HOME",
        "USERPROFILE",
        "PATH",
        "LANG",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "NO_PROXY",
        "https_proxy",
        "http_proxy",
        "no_proxy",
    ] {
        if let Some(value) = std::env::var_os(name) {
            cmd.env(name, value);
        }
    }
    if let Some(dir) = config.parent() {
        cmd.current_dir(dir);
    }
    let header = format!("{}: env:RTERM_MCP_TOKEN", mcp::TOKEN_HEADER);
    cmd.env("RTERM_OPENAI_TUNNEL_KEY", &profile.api_key)
        .env("CONTROL_PLANE_TUNNEL_ID", &profile.id)
        .env("RTERM_MCP_TOKEN", token)
        .args(["run", "--config"])
        .arg(config)
        .args([
            "--control-plane.api-key=env:RTERM_OPENAI_TUNNEL_KEY",
            "--control-plane.base-url=https://api.openai.com",
        ])
        .arg(format!("--mcp.server-url=http://{address}/mcp"))
        .args([
            "--mcp.extra-headers",
            &header,
            "--mcp.discovery-extra-headers",
            &header,
        ])
        .args(["--health.listen-addr=127.0.0.1:0", "--health.url-file"])
        .arg(health)
        .args([
            "--log.level=warn",
            "--log.format=json",
            "--log.http-raw-unsafe=false",
            "--open-web-ui=false",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    cmd
}

async fn supervise(mut command: tokio::process::Command, health: &Path, update: &impl Fn(Status)) {
    let mut delay = Duration::from_secs(1);
    let mut startup_failures = 0;
    loop {
        let _ = tokio::fs::remove_file(health).await;
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                update(Status::Failed(format!(
                    "Could not launch OpenAI tunnel-client: {error}"
                )));
                return;
            }
        };
        let started = Instant::now();
        let mut timer = tokio::time::interval(Duration::from_secs(2));
        let mut was_ready = false;
        let exit = loop {
            tokio::select! {
                exit = child.wait() => break exit,
                _ = timer.tick() => {
                    let ready = tokio::time::timeout(Duration::from_millis(750), readiness(health)).await.unwrap_or(false);
                    if ready {
                        delay = Duration::from_secs(1);
                        startup_failures = 0;
                        update(Status::Ready);
                    } else if was_ready {
                        update(Status::Reconnecting);
                    }
                    was_ready = ready;
                }
            }
        };
        if started.elapsed() < Duration::from_secs(5) {
            startup_failures += 1;
            if startup_failures == 3 {
                let outcome = match exit {
                    Ok(status) => status
                        .code()
                        .map_or_else(|| "terminated".into(), |code| format!("exit {code}")),
                    Err(_) => "could not read exit status".into(),
                };
                update(Status::Failed(format!(
                    "Tunnel client stopped during startup ({outcome}). Check the executable and try again."
                )));
                return;
            }
        } else {
            startup_failures = 0;
        }
        update(Status::Reconnecting);
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(30));
    }
}

async fn readiness(path: &Path) -> bool {
    let Ok(url) = tokio::fs::read_to_string(path).await else {
        return false;
    };
    let Some(address) = url
        .trim()
        .strip_prefix("http://")
        .and_then(|host| host.trim_end_matches('/').parse::<SocketAddr>().ok())
    else {
        return false;
    };
    if address.ip() != Ipv4Addr::LOCALHOST {
        return false;
    }
    let Ok(mut stream) = TcpStream::connect(address).await else {
        return false;
    };
    let request = format!("GET /readyz HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).await.is_err() {
        return false;
    }
    let mut line = String::new();
    let _ = BufReader::new(stream.take(256)).read_line(&mut line).await;
    line.split_ascii_whitespace().nth(1) == Some("200")
}
