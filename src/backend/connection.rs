use std::{
    fmt::Display,
    sync::{Arc, Mutex},
    time::Duration,
};

use bytes::Bytes;
use portable_pty::PtySize;
use russh::{
    ChannelMsg,
    client::{self, AuthResult, Handle},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::mpsc,
    task::JoinHandle,
};

use super::agent_shell::ShellJob;
use super::{BackendEvent, Events};
use crate::{
    host_keys::Key,
    models::{SerialConfig, SshConfig, TelnetConfig},
    storage,
};

pub(super) struct SshHandler {
    trusted: Option<Key>,
    declined: Arc<Mutex<Option<Key>>>,
}

impl client::Handler for SshHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key = Key::presented(key);
        if self.trusted.as_ref() == Some(&key) {
            return Ok(true);
        }
        *self.declined.lock().unwrap() = Some(key);
        Ok(false)
    }
}

// The tuple lets Tokio resolve DNS names and IPv6 addresses without manually
// assembling a host:port string. Brackets are syntax for a combined address.
fn endpoint(host: &str, port: u16) -> (&str, u16) {
    let host = host.trim();
    let host = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    (host, port)
}

async fn with_timeout<T, E: Display>(
    operation: &str,
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, String> {
    tokio::time::timeout(Duration::from_secs(15), future)
        .await
        .map_err(|_| format!("{operation} timed out"))?
        .map_err(|error| format!("{operation}: {error}"))
}

pub(super) enum SshConnection {
    Connected(Handle<SshHandler>),
    HostKey(Key),
}

pub(super) async fn connect_ssh(
    config: SshConfig,
    trusted: Option<Key>,
) -> Result<SshConnection, String> {
    let options = client::Config {
        window_size: 8 * 1024 * 1024,
        maximum_packet_size: 128 * 1024,
        nodelay: true,
        keepalive_interval: (config.keepalive > 0).then(|| Duration::from_secs(config.keepalive)),
        preferred: russh::Preferred {
            compression: if config.compression.unwrap_or(false) {
                std::borrow::Cow::Borrowed(&[
                    russh::compression::ZLIB_LEGACY,
                    russh::compression::ZLIB,
                    russh::compression::NONE,
                ])
            } else {
                std::borrow::Cow::Borrowed(&[russh::compression::NONE])
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let address = endpoint(&config.host, config.port);
    let declined = Arc::new(Mutex::new(None));
    let handler = SshHandler {
        trusted,
        declined: declined.clone(),
    };
    let connect = client::connect(Arc::new(options), address, handler);
    let mut session = match with_timeout("SSH connection", connect).await {
        Ok(session) => session,
        Err(error) => {
            return match declined.lock().unwrap().take() {
                Some(key) => Ok(SshConnection::HostKey(key)),
                None => Err(error),
            };
        }
    };

    match with_timeout("SSH authentication", authenticate(&mut session, &config)).await? {
        AuthResult::Success => Ok(SshConnection::Connected(session)),
        AuthResult::Failure {
            remaining_methods,
            partial_success,
        } => Err(format!(
            "Authentication failed (remaining methods: {remaining_methods:?}, partial success: {partial_success})"
        )),
    }
}

async fn authenticate(
    session: &mut Handle<SshHandler>,
    config: &SshConfig,
) -> Result<AuthResult, String> {
    if config.use_agent {
        return authenticate_agent(session, &config.user).await;
    }

    let passphrase = config.key_passphrase.as_deref();
    let key = if let Some(name) = &config.key_name {
        let password = config
            .vault_pass
            .as_deref()
            .ok_or("Unlock the vault to use this key")?;
        let (vault, _) =
            storage::load_vault(password).map_err(|error| format!("Vault: {error}"))?;
        let keys = vault.keys;
        let key = keys
            .into_iter()
            .find(|key| &key.name == name)
            .ok_or_else(|| format!("Key '{name}' was not found in the vault"))?;
        russh::keys::decode_secret_key(&key.private_key, passphrase)
    } else if let Some(path) = &config.key_path {
        russh::keys::load_secret_key(path, passphrase)
    } else if let Some(password) = &config.password {
        return session
            .authenticate_password(&config.user, password)
            .await
            .map_err(|error| error.to_string());
    } else {
        return Err("Choose an authentication method and supply its credentials".into());
    };

    let key = key.map_err(|error| format!("Could not load key: {error}"))?;
    let hash = session
        .best_supported_rsa_hash()
        .await
        .map_err(|error| error.to_string())?
        .flatten();
    let key = russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), hash);
    session
        .authenticate_publickey(&config.user, key)
        .await
        .map_err(|error| error.to_string())
}

async fn authenticate_agent(
    session: &mut Handle<SshHandler>,
    user: &str,
) -> Result<AuthResult, String> {
    use russh::keys::agent::client::AgentClient;
    #[cfg(unix)]
    let mut agent = AgentClient::connect_env()
        .await
        .map_err(|error| error.to_string())?
        .dynamic();
    #[cfg(windows)]
    let mut agent = match AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await {
        Ok(agent) => agent.dynamic(),
        Err(_) => AgentClient::connect_pageant()
            .await
            .map_err(|error| error.to_string())?
            .dynamic(),
    };
    let mut last = None;
    let hash = session
        .best_supported_rsa_hash()
        .await
        .map_err(|error| error.to_string())?
        .flatten();
    let identities = agent
        .request_identities()
        .await
        .map_err(|error| error.to_string())?;
    for identity in identities {
        let key = identity.public_key().into_owned();
        let result = session
            .authenticate_publickey_with(user, key, hash, &mut agent)
            .await
            .map_err(|error| error.to_string())?;
        if matches!(result, AuthResult::Success) {
            return Ok(result);
        }
        last = Some(result);
    }
    last.ok_or_else(|| "The SSH agent contains no keys".into())
}

pub(super) enum TransportMessage {
    Data(Vec<u8>),
    TerminalReply(Vec<u8>),
    Resize(PtySize),
    AgentShell(ShellJob),
}

// A session owns its pump. Closing a tab aborts all outstanding reads and
// releases the SSH channel, TCP socket, or serial port without waiting for EOF.
pub(super) struct Transport(Vec<JoinHandle<()>>);
impl Drop for Transport {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

pub(super) type Pipe = (Transport, mpsc::Sender<TransportMessage>);

pub(crate) const AGENT_PAGER_ENV: &str =
    "export PAGER=cat GIT_PAGER=cat SYSTEMD_PAGER=cat MANPAGER=cat\n";

pub(super) async fn agent_exec(
    mut channel: russh::Channel<client::Msg>,
    command: String,
    permit: crate::agent::Permit,
    result_reply: &crate::agent::Reply,
) -> Result<serde_json::Value, String> {
    use crate::agent::{MAX_OUTPUT, OutputStream};
    use serde_json::json;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut truncated = false;
    let mut exit_code = None;
    let mut failure = None;
    // Cancellation is checked before exec and while waiting on the channel.
    // Closing an SSH channel cannot guarantee a remote child has stopped.
    if !permit.valid() {
        let _ = tokio::time::timeout(Duration::from_secs(1), channel.close()).await;
        return Err("Agent authorization expired or revoked before execution".into());
    }
    let began = std::time::Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    // Scope the execution future so the channel can be read afterwards.
    let started = {
        let execution = async {
            channel
                .exec(true, format!("{AGENT_PAGER_ENV}{command}").into_bytes())
                .await
                .map_err(|e| e.to_string())?;
            channel.eof().await.map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        };
        tokio::pin!(execution);
        loop {
            tokio::select! {
                biased;
                _ = tick.tick() => if !permit.valid() { break Err("Agent authorization expired or revoked; remote effects may already have occurred".into()); },
                result = &mut execution => break result,
            }
        }
    };
    if let Err(error) = started {
        failure = Some(error);
    }
    while failure.is_none() {
        if !permit.valid() {
            failure = Some("Agent command timed out or access was revoked; remote effects may already have occurred".into());
            break;
        }
        let message = tokio::select! {
            _ = tick.tick() => continue,
            message = channel.wait() => message,
        };
        let (data, stream) = match message {
            Some(ChannelMsg::Data { data }) => (data, OutputStream::Stdout),
            Some(ChannelMsg::ExtendedData { data, .. }) => (data, OutputStream::Stderr),
            Some(ChannelMsg::ExitStatus { exit_status }) => {
                exit_code = Some(exit_status);
                continue;
            }
            Some(ChannelMsg::ExitSignal { signal_name, .. }) => {
                failure = Some(format!("Remote command exited on signal {signal_name:?}"));
                break;
            }
            Some(ChannelMsg::Failure) => {
                failure = Some("Server rejected SSH exec request".into());
                break;
            }
            Some(ChannelMsg::Close) | None => break,
            _ => continue,
        };
        let count = data
            .len()
            .min(MAX_OUTPUT.saturating_sub(stdout.len() + stderr.len()));
        truncated |= count < data.len();
        result_reply.capture_output(stream, &data);
        if stream == OutputStream::Stderr {
            stderr.extend_from_slice(&data[..count]);
        } else {
            stdout.extend_from_slice(&data[..count]);
        }
    }
    let duration_ms = began.elapsed().as_millis() as u64;
    if !permit.valid() {
        // Best-effort remote termination of THIS exec channel, never the
        // interactive terminal. Some servers ignore SSH signal requests.
        let _ = tokio::time::timeout(Duration::from_millis(250), channel.signal(russh::Sig::TERM))
            .await;
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), channel.close()).await;
    if failure.is_none() && exit_code.is_none() {
        failure =
            Some("SSH channel closed without exit status; command outcome is uncertain".into());
    }
    Ok(
        json!({"ok": failure.is_none() && exit_code.is_some(), "command_success": failure.is_none() && exit_code == Some(0), "stdout": String::from_utf8_lossy(&stdout), "stderr": String::from_utf8_lossy(&stderr), "exit_code": exit_code, "duration_ms": duration_ms, "truncated": truncated, "error": failure, "execution_mode":"background_exec","combined_output":false}),
    )
}

pub(super) async fn open_shell(
    session: &Handle<SshHandler>,
    id: u32,
    terminal: String,
    size: PtySize,
    events: Events,
) -> Result<Pipe, String> {
    let channel = with_timeout("Open SSH channel", session.channel_open_session()).await?;
    with_timeout(
        "Request remote PTY",
        channel.request_pty(
            true,
            &terminal,
            size.cols.into(),
            size.rows.into(),
            size.pixel_width.into(),
            size.pixel_height.into(),
            &[
                (russh::Pty::IUTF8, 1),
                (russh::Pty::ISTRIP, 0),
                (russh::Pty::CS8, 1),
            ],
        ),
    )
    .await?;
    // RTerm always sends UTF-8. Request a UTF-8 character locale without
    // overriding the server's message language or LC_ALL. Servers may reject
    // environment requests; no shell command or startup file is modified.
    with_timeout(
        "Request SSH character locale",
        channel.set_env(false, "LC_CTYPE", "C.UTF-8"),
    )
    .await?;
    with_timeout("Start remote shell", channel.request_shell(true)).await?;

    // SSH handles window resizes separately from terminal input, so keep the
    // typed control messages even though the session output is just bytes.
    let (mut reader, writer) = channel.split();
    let (send, mut messages) = mpsc::channel(128);
    let input_events = events.clone();
    let capture = Arc::new(Mutex::new(None::<ShellJob>));
    let input_capture = capture.clone();
    let changed = Arc::new(tokio::sync::Notify::new());
    let input_changed = changed.clone();
    let output = tokio::spawn(async move {
        let mut shell_exited = false;
        loop {
            let tracking = capture.lock().unwrap().is_some();
            let message = tokio::select! {
                _ = changed.notified() => continue,
                _ = tokio::time::sleep(Duration::from_millis(100)), if tracking => {
                    let mut current = capture.lock().unwrap();
                    if current.as_ref().is_some_and(|job| !job.permit.valid()) {
                        current.take().unwrap().require_reconnect();
                    }
                    continue;
                },
                message = reader.wait() => match message { Some(message) => message, None => break },
            };
            match message {
                ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. } => {
                    {
                        let mut current = capture.lock().unwrap();
                        if let Some(job) = current.as_mut() {
                            if !job.permit.valid() {
                                current.take().unwrap().require_reconnect();
                            } else if let Some(value) = job.consume(&data) {
                                current.take().unwrap().finish(value);
                            }
                        }
                    }
                    // Always deliver the actual remote bytes to the existing
                    // terminal grid, including ordinary ANSI output and prompts.
                    events.send_async(BackendEvent::Data { id, data }).await;
                }
                ChannelMsg::ExitStatus { .. } => shell_exited = true,
                ChannelMsg::Eof => {
                    if !shell_exited {
                        // Some servers send the exit status just after EOF.
                        shell_exited = matches!(
                            tokio::time::timeout(Duration::from_millis(250), reader.wait()).await,
                            Ok(Some(ChannelMsg::ExitStatus { .. }))
                        );
                    }
                    break;
                }
                ChannelMsg::Close => break,
                _ => {}
            }
        }
        if let Some(job) = capture.lock().unwrap().take() {
            job.fail("Interactive shell closed without a completion marker; outcome uncertain");
        }
        let event = if shell_exited {
            BackendEvent::SshShellExited(id)
        } else {
            BackendEvent::Closed(id)
        };
        events.send_async(event).await;
    });
    let input = tokio::spawn(async move {
        let mut pending = None;
        loop {
            let message = match pending.take() {
                Some(message) => message,
                None => match messages.recv().await {
                    Some(message) => message,
                    None => break,
                },
            };
            let result = match message {
                TransportMessage::Data(mut bytes) => {
                    // User input (including Ctrl+C) is never blocked. Stop
                    // capturing before forwarding it, so later user input
                    // and any echoed secrets are not part of the AI log.
                    if let Some(job) = input_capture.lock().unwrap().take() {
                        job.fail("Interactive input resumed; AI output capture ended. Check the terminal for the command outcome");
                    }
                    // Coalesce only input already queued; never wait to accumulate
                    // characters or reorder a resize behind subsequent input.
                    while bytes.len() < 64 * 1024 {
                        match messages.try_recv() {
                            Ok(TransportMessage::Data(next)) => bytes.extend(next),
                            Ok(control) => {
                                pending = Some(control);
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                    writer.data_bytes(bytes).await
                }
                // Cursor-position, color and other emulator replies are
                // protocol traffic, not a human taking over the shell.
                TransportMessage::TerminalReply(bytes) => writer.data_bytes(bytes).await,
                TransportMessage::Resize(mut size) => {
                    // Only the newest consecutive resize matters. Preserve the
                    // ordering relative to typed input.
                    while let Ok(next) = messages.try_recv() {
                        match next {
                            TransportMessage::Resize(next_size) => size = next_size,
                            data => {
                                pending = Some(data);
                                break;
                            }
                        }
                    }
                    writer
                        .window_change(
                            size.cols.into(),
                            size.rows.into(),
                            size.pixel_width.into(),
                            size.pixel_height.into(),
                        )
                        .await
                }
                TransportMessage::AgentShell(mut job) => {
                    if !job.permit.valid() {
                        job.fail("Interactive command authorization expired before input was sent");
                        continue;
                    }
                    let line = std::mem::take(&mut job.line);
                    let permit = job.permit.clone();
                    {
                        let mut current = input_capture.lock().unwrap();
                        if current.is_some() {
                            job.fail("Another interactive command is still being captured");
                            continue;
                        }
                        *current = Some(job);
                    }
                    input_changed.notify_one();
                    let sending = writer.data_bytes(line);
                    tokio::pin!(sending);
                    loop {
                        tokio::select! {
                            biased;
                            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                                if !permit.valid() {
                                    if let Some(job) = input_capture.lock().unwrap().take() {
                                        job.require_reconnect();
                                    }
                                    break Ok(());
                                }
                            }
                            result = &mut sending => break result,
                        }
                    }
                }
            };
            if let Err(error) = result {
                if let Some(job) = input_capture.lock().unwrap().take() {
                    job.fail("Interactive terminal input failed; command outcome uncertain");
                }
                input_events
                    .send_async(BackendEvent::InputFailed {
                        id,
                        error: error.to_string(),
                    })
                    .await;
                break;
            }
        }
    });
    Ok((Transport(vec![output, input]), send))
}

pub(super) async fn open_telnet(
    config: TelnetConfig,
    id: u32,
    events: Events,
) -> Result<Pipe, String> {
    let address = endpoint(&config.host, config.port);
    let stream = with_timeout("Telnet connection", tokio::net::TcpStream::connect(address)).await?;
    stream
        .set_nodelay(true)
        .map_err(|error| error.to_string())?;
    Ok(open_stream(stream, id, events, Some(Telnet::default())))
}

pub(super) fn open_serial(config: SerialConfig, id: u32, events: Events) -> Result<Pipe, String> {
    use tokio_serial::SerialPortBuilderExt;
    let bits = match config.bits {
        5 => serialport::DataBits::Five,
        6 => serialport::DataBits::Six,
        7 => serialport::DataBits::Seven,
        _ => serialport::DataBits::Eight,
    };
    let stream = tokio_serial::new(config.port, config.baud)
        .data_bits(bits)
        .open_native_async()
        .map_err(|error| error.to_string())?;
    Ok(open_stream(stream, id, events, None))
}

fn open_stream<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    stream: S,
    id: u32,
    events: Events,
    mut telnet: Option<Telnet>,
) -> Pipe {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (send, mut messages) = mpsc::channel(128);
    let task = tokio::spawn(async move {
        let mut buffer = [0; 8192];
        loop {
            tokio::select! {
                read = reader.read(&mut buffer) => {
                    let Ok(count) = read else { break };
                    if count == 0 { break; }
                    let packet = &buffer[..count];
                    let data = if let Some(telnet) = &mut telnet {
                        let (data, reply) = telnet.decode(packet);
                        if !reply.is_empty() && writer.write_all(&reply).await.is_err() { break; }
                        data
                    } else { Bytes::copy_from_slice(packet) };
                    if !data.is_empty() { events.send_async(BackendEvent::Data { id, data }).await; }
                }
                message = messages.recv() => {
                    let Some(message) = message else { break };
                    if let TransportMessage::Data(mut bytes) | TransportMessage::TerminalReply(mut bytes) = message {
                        if telnet.is_some() { bytes = escape_telnet(bytes); }
                        if writer.write_all(&bytes).await.is_err() { break; }
                    }
                }
            }
        }
        events.send_async(BackendEvent::Closed(id)).await;
    });
    (Transport(vec![task]), send)
}

const IAC: u8 = 255;
#[derive(Clone, Copy, Default)]
enum Telnet {
    #[default]
    Text,
    Command,
    Option(u8),
    Subnegotiation,
    SubnegotiationCommand,
}

impl Telnet {
    // Remember incomplete control sequences between reads. Reject unsupported
    // options and pass only the terminal payload through to the emulator.
    fn decode(&mut self, packet: &[u8]) -> (Bytes, Vec<u8>) {
        let mut data = Vec::with_capacity(packet.len());
        let mut reply = Vec::new();
        for &byte in packet {
            *self = match (*self, byte) {
                (Self::Text, IAC) => Self::Command,
                (Self::Text, byte) => {
                    data.push(byte);
                    Self::Text
                }
                (Self::Command, IAC) => {
                    data.push(IAC);
                    Self::Text
                }
                (Self::Command, verb @ 251..=254) => Self::Option(verb),
                (Self::Command, 250) => Self::Subnegotiation,
                (Self::Command, _) => Self::Text,
                (Self::Option(251), option) => {
                    reply.extend([IAC, 254, option]);
                    Self::Text
                }
                (Self::Option(253), option) => {
                    reply.extend([IAC, 252, option]);
                    Self::Text
                }
                (Self::Option(_), _) => Self::Text,
                (Self::Subnegotiation, IAC) => Self::SubnegotiationCommand,
                (Self::SubnegotiationCommand, 240) => Self::Text,
                (Self::Subnegotiation | Self::SubnegotiationCommand, _) => Self::Subnegotiation,
            };
        }
        (Bytes::from(data), reply)
    }
}

fn escape_telnet(bytes: Vec<u8>) -> Vec<u8> {
    if !bytes.contains(&IAC) {
        return bytes;
    }
    let mut escaped = Vec::with_capacity(bytes.len() + 1);
    for byte in bytes {
        escaped.push(byte);
        if byte == IAC {
            escaped.push(IAC);
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ssh_unicode_typing_ime_and_paste_round_trip() {
        use crate::{
            backend::Backend,
            highlight::Highlighter,
            terminal::{Kind, Terminal, TerminalOptions},
        };
        use eframe::egui;
        use russh::{Channel, ChannelId, Pty, server};

        #[derive(Default)]
        struct Echo {
            pty: bool,
            locale: bool,
        }
        impl server::Handler for Echo {
            type Error = russh::Error;
            async fn auth_password(
                &mut self,
                _: &str,
                _: &str,
            ) -> Result<server::Auth, Self::Error> {
                Ok(server::Auth::Accept)
            }
            async fn channel_open_session(
                &mut self,
                _: Channel<server::Msg>,
                reply: server::ChannelOpenHandle,
                _: &mut server::Session,
            ) -> Result<(), Self::Error> {
                reply.accept().await;
                Ok(())
            }
            async fn pty_request(
                &mut self,
                id: ChannelId,
                _: &str,
                _: u32,
                _: u32,
                _: u32,
                _: u32,
                modes: &[(Pty, u32)],
                session: &mut server::Session,
            ) -> Result<(), Self::Error> {
                assert!(modes.contains(&(Pty::IUTF8, 1)));
                assert!(modes.contains(&(Pty::ISTRIP, 0)));
                assert!(modes.contains(&(Pty::CS8, 1)));
                self.pty = true;
                session.channel_success(id)
            }
            async fn env_request(
                &mut self,
                _: ChannelId,
                name: &str,
                value: &str,
                _: &mut server::Session,
            ) -> Result<(), Self::Error> {
                assert_eq!((name, value), ("LC_CTYPE", "C.UTF-8"));
                self.locale = true;
                // A server is allowed to ignore an environment request.
                // Do not send a reply; the client must still open its shell.
                Ok(())
            }
            async fn shell_request(
                &mut self,
                id: ChannelId,
                session: &mut server::Session,
            ) -> Result<(), Self::Error> {
                assert!(self.pty && self.locale);
                session.channel_success(id)
            }
            async fn data(
                &mut self,
                id: ChannelId,
                data: &[u8],
                session: &mut server::Session,
            ) -> Result<(), Self::Error> {
                // Echo byte by byte, including splits inside UTF-8 sequences.
                for &byte in data {
                    session.data(id, Bytes::copy_from_slice(&[byte]))?;
                }
                Ok(())
            }
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let key =
            ssh_key::PrivateKey::random(&mut rand::rng(), ssh_key::Algorithm::Ed25519).unwrap();
        let trusted = Key {
            algorithm: key.public_key().algorithm().to_string(),
            fingerprint: key
                .public_key()
                .fingerprint(ssh_key::HashAlg::Sha256)
                .to_string(),
        };
        let config = Arc::new(server::Config {
            keys: vec![key],
            ..Default::default()
        });
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let running = server::run_stream(config, stream, Echo::default())
                .await
                .unwrap();
            let _ = running.await;
        });
        let config = SshConfig {
            host: "127.0.0.1".into(),
            port,
            user: "fixture".into(),
            password: Some("fixture".into()),
            key_path: None,
            key_name: None,
            vault_pass: None,
            name: None,
            compression: None,
            key_passphrase: None,
            use_agent: false,
            keepalive: 0,
        };
        let SshConnection::Connected(session) = connect_ssh(config, Some(trusted)).await.unwrap()
        else {
            panic!("fixture host must be trusted");
        };
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let (backend, mut events) = Backend::new(&ctx);
        let options = TerminalOptions {
            font: 13.0,
            cursor: Default::default(),
            scrollback: 100,
            copy_on_select: false,
            line_numbers: false,
        };
        let mut terminal = Terminal::new(1, "Unicode fixture".into(), Kind::Ssh, &options);
        let id = 7;
        terminal.endpoint = Some(id);
        let (_transport, writer) = open_shell(
            &session,
            id,
            "xterm-256color".into(),
            terminal.size(),
            backend.events.clone(),
        )
        .await
        .unwrap();
        backend.writers.lock().unwrap().insert(id, writer);
        terminal.ssh_ready(&backend, terminal.size());
        let highlighter = Highlighter::new(false, "");
        for (bracketed, events_in, expected) in [
            (
                false,
                vec![
                    egui::Event::Text("éПривет".into()),
                    egui::Event::Ime(egui::ImeEvent::Commit("日本語".into())),
                    egui::Event::Paste("中文한국어".into()),
                ],
                "éПривет日本語中文한국어",
            ),
            (
                true,
                vec![egui::Event::Paste("日本語 café 🐱".into())],
                "\x1b[200~日本語 café 🐱\x1b[201~",
            ),
        ] {
            if bracketed {
                terminal.receive(b"\x1b[?2004h");
            }
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 600.0),
                    )),
                    events: events_in,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.memory_mut(|m| {
                            m.request_focus(ui.id().with(("terminal", terminal.tab)))
                        });
                        let (_, _, error) = terminal.show(
                            ui,
                            ui.max_rect(),
                            &backend,
                            &options,
                            &highlighter,
                            true,
                            None,
                            None,
                        );
                        assert!(error.is_none(), "{error:?}");
                    });
                },
            );
            let received = tokio::time::timeout(Duration::from_secs(5), async {
                let mut received = Vec::new();
                while received.len() < expected.len() {
                    match events.recv().await.unwrap() {
                        BackendEvent::Data {
                            id: received_id,
                            data,
                        } => {
                            assert_eq!(received_id, id);
                            received.extend_from_slice(&data);
                            terminal.receive(&data);
                        }
                        _ => panic!("SSH connection closed before Unicode round trip"),
                    }
                }
                received
            })
            .await
            .expect("Unicode echo timed out");
            assert_eq!(received, expected.as_bytes());
        }
        server.abort();
    }

    #[test]
    fn normalizes_bracketed_ipv6_and_leaves_other_hosts_intact() {
        assert_eq!(endpoint(" [::1] ", 22), ("::1", 22));
        assert_eq!(endpoint("2001:db8::2", 23), ("2001:db8::2", 23));
        assert_eq!(endpoint(" example.net ", 2222), ("example.net", 2222));
    }

    #[test]
    fn telnet_negotiation_can_split_across_reads() {
        let mut parser = Telnet::default();
        let (data, reply) = parser.decode(&[b'a', IAC, 251]);
        assert_eq!(&data[..], b"a");
        assert!(reply.is_empty());
        let (data, reply) = parser.decode(&[1, b'b', IAC, IAC]);
        assert_eq!(&data[..], &[b'b', IAC]);
        assert_eq!(reply, [IAC, 254, 1]);
    }
}
