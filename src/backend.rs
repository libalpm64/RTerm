use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
    sync::{Arc, Mutex, mpsc},
};

use bytes::Bytes;
use eframe::egui;
use portable_pty::PtySize;
use russh::client::Handle;
use tokio::sync::mpsc as async_mpsc;

use crate::models::{SerialConfig, SshConfig, TelnetConfig};

mod agent_shell;
mod connection;
mod sftp;

pub(crate) use agent_shell::validate as validate_foreground_command;
#[cfg(test)]
pub(crate) use connection::AGENT_PAGER_ENV;
use connection::{SshHandler, Transport, TransportMessage};
pub(crate) use sftp::{DownloadOptions, Entry, RemoteAction, join_remote, safe_name};

static SESSION_COUNTER: AtomicU32 = AtomicU32::new(0);

// SFTP progress, independent of the interactive terminal.
#[derive(Default)]
pub(crate) struct AgentTranscript {
    pub output: Vec<u8>,
    pub status: Option<String>,
    pub progress: Option<(u64, u64)>,
}

pub(crate) type AgentDisplay = Arc<Mutex<AgentTranscript>>;

pub(crate) fn next_session_id() -> u32 {
    SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
}

pub(crate) enum BackendEvent {
    Data {
        id: u32,
        data: Bytes,
    },
    Closed(u32),
    SshShellExited(u32),
    InputFailed {
        id: u32,
        error: String,
    },
    Progress {
        id: String,
        text: String,
        percent: f32,
        finished: bool,
        error: bool,
    },
    RemoteChanged(u32),
}

#[derive(Clone)]
pub(crate) struct Events {
    sender: async_mpsc::Sender<BackendEvent>,
    context: egui::Context,
    repaint_pending: Arc<AtomicBool>,
}

impl Events {
    pub(crate) fn send(&self, event: BackendEvent) {
        if self.sender.blocking_send(event).is_ok() {
            self.wake();
        }
    }

    pub(crate) async fn send_async(&self, event: BackendEvent) {
        if self.sender.send(event).await.is_ok() {
            self.wake();
        }
    }

    fn wake(&self) {
        if !self.repaint_pending.swap(true, Ordering::AcqRel) {
            self.context.request_repaint();
        }
    }

    pub(crate) fn begin_drain(&self) {
        // Acquire the pending sends before polling. Later sends can wake the UI again.
        self.repaint_pending.swap(false, Ordering::AcqRel);
    }
}

// The UI and the worker share these Rust types. Paths and terminal bytes cross
// the channel as they are, so a colon in a path or host never needs escaping.
pub(crate) enum Command {
    Ssh {
        config: SshConfig,
        trusted: Option<crate::host_keys::Key>,
    },
    Telnet(TelnetConfig),
    Serial(SerialConfig),
    Shell {
        id: u32,
        terminal: String,
        size: PtySize,
    },
    Disconnect(u32),
    AgentExec {
        id: u32,
        command: String,
        permit: crate::agent::Permit,
        result_reply: crate::agent::Reply,
    },
    AgentShell {
        id: u32,
        command: String,
        permit: crate::agent::Permit,
        result_reply: crate::agent::Reply,
    },
    AgentUpload {
        id: u32,
        local: String,
        remote: String,
        permit: crate::agent::Permit,
        approved: crate::agent_upload::ManifestSummary,
        result_reply: crate::agent::Reply,
        display: AgentDisplay,
    },
    AgentUploadInspect {
        local: String,
        remote: String,
        permit: crate::agent::Permit,
    },
    Sftp {
        id: u32,
        action: RemoteAction,
    },
}

pub(crate) enum Reply {
    Connected(u32),
    HostKey(crate::host_keys::Key),
    Files { path: String, entries: Vec<Entry> },
    Done,
    Agent(serde_json::Value),
    AgentUploadSummary(crate::agent_upload::ManifestSummary),
}

impl Reply {
    pub(crate) fn session_id(self) -> Result<u32, String> {
        match self {
            Self::Connected(id) => Ok(id),
            _ => Err("Expected a connection from the backend".into()),
        }
    }
}

pub(crate) type BackendResult = Result<Reply, String>;
pub(crate) enum InputError {
    Full(Vec<u8>),
    Closed,
}
type Request = (Command, mpsc::Sender<BackendResult>);
type Writers = Arc<Mutex<HashMap<u32, async_mpsc::Sender<TransportMessage>>>>;

pub(crate) struct Backend {
    writers: Writers,
    pub(crate) events: Events,
    worker: Mutex<Option<RunningWorker>>,
}

struct RunningWorker {
    sender: async_mpsc::Sender<Request>,
    runtime: tokio::runtime::Runtime,
}

impl Backend {
    pub(crate) fn new(context: &egui::Context) -> (Self, async_mpsc::Receiver<BackendEvent>) {
        let (event_tx, event_rx) = async_mpsc::channel(256);
        let events = Events {
            sender: event_tx,
            context: context.clone(),
            repaint_pending: Arc::new(AtomicBool::new(false)),
        };
        (
            Self {
                writers: Writers::default(),
                events,
                worker: Mutex::new(None),
            },
            event_rx,
        )
    }

    fn start_worker(&self) -> std::io::Result<RunningWorker> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let (sender, receiver) = async_mpsc::channel(256);
        let worker = Worker {
            connections: HashMap::new(),
            transports: HashMap::new(),
            writers: self.writers.clone(),
            events: self.events.clone(),
            sftp: sftp::Service::default(),
            upload_inspections: Arc::new(tokio::sync::Semaphore::new(2)),
        };
        runtime.spawn(worker.run(receiver));
        Ok(RunningWorker { sender, runtime })
    }

    pub(crate) fn request(&self, command: Command) -> mpsc::Receiver<BackendResult> {
        let (reply, receive) = mpsc::channel();
        // Submit directly to the existing terminal writer, in order with
        // user input. Do not open another SSH channel for foreground work.
        if let Command::AgentShell {
            id,
            command,
            permit,
            result_reply,
        } = command
        {
            let job = match agent_shell::ShellJob::new(
                &command,
                permit,
                result_reply.clone(),
                reply.clone(),
            ) {
                Ok(job) => job,
                Err(error) => {
                    let _ = result_reply.send(crate::agent::error(error.clone()));
                    let _ = reply.send(Err(error));
                    return receive;
                }
            };
            let writers = self.writers.lock().unwrap();
            if let Some(sender) = writers.get(&id) {
                if let Err(error) = sender.try_send(TransportMessage::AgentShell(job))
                    && let TransportMessage::AgentShell(job) = error.into_inner()
                {
                    job.fail("Interactive terminal writer busy or closed; command not sent");
                }
            } else {
                job.fail("Interactive terminal is not connected; command not sent");
            }
            return receive;
        }
        // Local PTYs use their own reader and writer. Start Tokio on the first backend request.
        let mut worker = self.worker.lock().unwrap();
        if worker.is_none() {
            match self.start_worker() {
                Ok(running) => *worker = Some(running),
                Err(error) => {
                    let _ = reply.send(Err(format!("Start backend: {error}")));
                    return receive;
                }
            }
        }
        if let Err(error) = worker.as_ref().unwrap().sender.try_send((command, reply)) {
            let (_, reply) = error.into_inner();
            let _ = reply.send(Err("Backend is busy or stopped".into()));
        }
        receive
    }

    pub(crate) fn write(&self, id: u32, data: Vec<u8>) -> Result<(), InputError> {
        self.write_input(id, data, false)
    }

    pub(crate) fn write_terminal_reply(&self, id: u32, data: Vec<u8>) -> Result<(), InputError> {
        self.write_input(id, data, true)
    }

    fn write_input(&self, id: u32, data: Vec<u8>, automatic: bool) -> Result<(), InputError> {
        let writers = self.writers.lock().unwrap();
        let sender = writers.get(&id).ok_or(InputError::Closed)?;
        let message = if automatic {
            TransportMessage::TerminalReply(data)
        } else {
            TransportMessage::Data(data)
        };
        match sender.try_send(message) {
            Ok(()) => Ok(()),
            Err(async_mpsc::error::TrySendError::Full(
                TransportMessage::Data(data) | TransportMessage::TerminalReply(data),
            )) => Err(InputError::Full(data)),
            Err(_) => Err(InputError::Closed),
        }
    }

    pub(crate) fn schedule_input_retry(&self) {
        self.events
            .context
            .request_repaint_after(std::time::Duration::from_millis(5));
    }

    pub(crate) fn resize(&self, id: u32, size: PtySize) -> bool {
        self.send(id, TransportMessage::Resize(size))
    }

    fn send(&self, id: u32, message: TransportMessage) -> bool {
        self.writers
            .lock()
            .unwrap()
            .get(&id)
            .is_some_and(|sender| sender.try_send(message).is_ok())
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.get_mut().unwrap().take() {
            worker.runtime.shutdown_background();
        }
    }
}

struct Worker {
    connections: HashMap<u32, Arc<Handle<SshHandler>>>,
    transports: HashMap<u32, Transport>,
    writers: Writers,
    events: Events,
    sftp: sftp::Service,
    upload_inspections: Arc<tokio::sync::Semaphore>,
}

enum SshSetup {
    Connection(
        mpsc::Sender<BackendResult>,
        Result<connection::SshConnection, String>,
    ),
    Shell(
        u32,
        mpsc::Sender<BackendResult>,
        Result<connection::Pipe, String>,
    ),
}

impl Worker {
    async fn run(mut self, mut requests: async_mpsc::Receiver<Request>) {
        let mut setup = tokio::task::JoinSet::new();
        loop {
            let (command, reply) = tokio::select! {
                request = requests.recv() => match request {
                    Some(request) => request,
                    None => break,
                },
                result = setup.join_next(), if !setup.is_empty() => {
                    if let Some(Ok(result)) = result {
                        self.finish_ssh_setup(result);
                    }
                    continue;
                }
            };
            if let Command::Ssh { config, trusted } = command {
                setup.spawn(async move {
                    SshSetup::Connection(reply, connection::connect_ssh(config, trusted).await)
                });
                continue;
            }
            if let Command::Shell { id, terminal, size } = command {
                if let Some(session) = self.connections.get(&id).cloned() {
                    let events = self.events.clone();
                    setup.spawn(async move {
                        SshSetup::Shell(
                            id,
                            reply,
                            connection::open_shell(&session, id, terminal, size, events).await,
                        )
                    });
                } else {
                    let _ = reply.send(Err("SSH session closed".into()));
                    self.events.context.request_repaint();
                }
                continue;
            }
            if let Command::AgentUpload {
                id,
                local,
                remote,
                permit,
                approved,
                result_reply,
                display,
            } = command
            {
                let session = self.connections.get(&id).cloned();
                let events = self.events.clone();
                tokio::spawn(async move {
                    let result = match session {
                        Some(session) => {
                            sftp::agent_upload(
                                &session, &local, &remote, permit, &display, approved,
                            )
                            .await
                        }
                        None => Err("SSH session closed".into()),
                    };
                    let value = match result {
                        Ok(bytes) => {
                            serde_json::json!({"ok":true,"remote":remote,"bytes_uploaded":bytes})
                        }
                        Err(error) => crate::agent::error(error),
                    };
                    let success = value["ok"] == true;
                    let _ = result_reply.send(value.clone());
                    let _ = reply.send(Ok(Reply::Agent(value)));
                    events.context.request_repaint();
                    if success {
                        events.send_async(BackendEvent::RemoteChanged(id)).await;
                    }
                });
                continue;
            }
            if let Command::AgentUploadInspect {
                local,
                remote,
                permit,
            } = command
            {
                let events = self.events.clone();
                let inspections = self.upload_inspections.clone();
                tokio::spawn(async move {
                    let result = async {
                        let _slot = inspections
                            .acquire_owned()
                            .await
                            .map_err(|_| "Upload inspection unavailable".to_owned())?;
                        let summary = sftp::inspect_agent_upload(&local, &remote, &permit).await?;
                        Ok(Reply::AgentUploadSummary(summary))
                    }
                    .await;
                    let _ = reply.send(result);
                    events.context.request_repaint();
                });
                continue;
            }
            if let Command::AgentExec {
                id,
                command,
                permit,
                result_reply,
            } = command
            {
                let session = self.connections.get(&id).cloned();
                let context = self.events.context.clone();
                tokio::spawn(async move {
                    // Opening a stalled agent channel must not block the
                    // backend's other session operations. Bound the open wait.
                    let opened = if !permit.valid() {
                        Err("Agent authorization expired or revoked".into())
                    } else if let Some(session) = session {
                        match tokio::time::timeout(
                            std::time::Duration::from_secs(10).min(
                                permit
                                    .deadline
                                    .saturating_duration_since(std::time::Instant::now()),
                            ),
                            session.channel_open_session(),
                        )
                        .await
                        {
                            Ok(result) => result.map_err(|e| e.to_string()),
                            Err(_) => Err("SSH channel open timed out".into()),
                        }
                    } else {
                        Err("SSH session closed".into())
                    };
                    let result = match opened {
                        Ok(channel) => {
                            connection::agent_exec(channel, command, permit, &result_reply)
                                .await
                                .map(Reply::Agent)
                        }
                        Err(error) => Err(error),
                    };
                    // Results must reach the CLI even if the desktop UI is busy.
                    let value = match &result {
                        Ok(Reply::Agent(value)) => value.clone(),
                        Err(error) => crate::agent::error(error.clone()),
                        _ => crate::agent::error("Unexpected SSH result"),
                    };
                    let _ = result_reply.send(value);
                    let _ = reply.send(result);
                    context.request_repaint();
                });
                continue;
            }
            if let Command::Sftp { id, action } = command {
                if let Some(session) = self.connections.get(&id).cloned() {
                    self.sftp.dispatch(id, session, action, reply, &self.events);
                } else {
                    let _ = reply.send(Err("SSH session closed".into()));
                    self.events.context.request_repaint();
                }
                continue;
            }
            let result = self.handle(command).await;
            let _ = reply.send(result);
            self.events.context.request_repaint();
        }
    }

    fn finish_ssh_setup(&mut self, setup: SshSetup) {
        match setup {
            SshSetup::Connection(reply, result) => {
                let result = result.map(|result| match result {
                    connection::SshConnection::Connected(session) => {
                        let id = next_session_id();
                        self.connections.insert(id, Arc::new(session));
                        Reply::Connected(id)
                    }
                    connection::SshConnection::HostKey(key) => Reply::HostKey(key),
                });
                if let Err(mpsc::SendError(Ok(Reply::Connected(id)))) = reply.send(result) {
                    self.connections.remove(&id);
                }
            }
            SshSetup::Shell(id, reply, result) => {
                let result = result.and_then(|(transport, writer)| {
                    if !self.connections.contains_key(&id) {
                        return Err("SSH session closed".into());
                    }
                    self.store_transport(id, transport, writer);
                    Ok(Reply::Done)
                });
                let _ = reply.send(result);
            }
        }
        self.events.context.request_repaint();
    }

    async fn handle(&mut self, command: Command) -> BackendResult {
        match command {
            Command::Telnet(config) => {
                let id = next_session_id();
                let (transport, writer) =
                    connection::open_telnet(config, id, self.events.clone()).await?;
                self.store_transport(id, transport, writer);
                Ok(Reply::Connected(id))
            }
            Command::Serial(config) => {
                let id = next_session_id();
                let (transport, writer) = connection::open_serial(config, id, self.events.clone())?;
                self.store_transport(id, transport, writer);
                Ok(Reply::Connected(id))
            }
            Command::Disconnect(id) => {
                self.writers.lock().unwrap().remove(&id);
                self.transports.remove(&id);
                self.sftp.close(id);
                if let Some(connection) = self.connections.remove(&id) {
                    let _ = connection
                        .disconnect(
                            russh::Disconnect::ByApplication,
                            "Rterm session closed",
                            "en",
                        )
                        .await;
                }
                Ok(Reply::Done)
            }
            Command::Ssh { .. } | Command::Shell { .. } => {
                Err("SSH setup was not dispatched".into())
            }
            Command::AgentExec { .. }
            | Command::AgentUpload { .. }
            | Command::AgentUploadInspect { .. }
            | Command::AgentShell { .. } => Err("Agent operation was not dispatched".into()),
            Command::Sftp { .. } => Err("SFTP operation was not dispatched".into()),
        }
    }

    fn store_transport(
        &mut self,
        id: u32,
        transport: Transport,
        writer: async_mpsc::Sender<TransportMessage>,
    ) {
        self.transports.insert(id, transport);
        self.writers.lock().unwrap().insert(id, writer);
    }
}

#[cfg(test)]
#[path = "../tests/unit/backend.rs"]
mod tests;
