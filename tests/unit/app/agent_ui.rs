use super::*;
use russh::{Channel, ChannelId, server};
use std::sync::atomic::AtomicUsize;

const PAGER_PROBE: &str = "test ! -t 0 && test ! -t 1 && test ! -t 2 || exit 99\n\
    test \"$PAGER/$GIT_PAGER/$SYSTEMD_PAGER/$MANPAGER\" = cat/cat/cat/cat || exit 98\n\
    seq 1 100 | less || exit 97\n\
    printf 'pager fixture stderr\\n' >&2\n\
    exit 17";

type FixtureFiles = Arc<Mutex<BTreeMap<String, Vec<u8>>>>;

struct TestSftp(FixtureFiles, std::collections::HashSet<String>);
impl russh_sftp::server::Handler for TestSftp {
    type Error = russh_sftp::protocol::StatusCode;
    fn unimplemented(&self) -> Self::Error {
        Self::Error::OpUnsupported
    }
    async fn realpath(
        &mut self,
        id: u32,
        path: String,
    ) -> Result<russh_sftp::protocol::Name, Self::Error> {
        Ok(russh_sftp::protocol::Name {
            id,
            files: vec![russh_sftp::protocol::File::dummy(if path == "." {
                "/".to_owned()
            } else {
                path
            })],
        })
    }
    async fn opendir(
        &mut self,
        id: u32,
        path: String,
    ) -> Result<russh_sftp::protocol::Handle, Self::Error> {
        let path = if path == "." { "/".into() } else { path };
        self.1.remove(&path);
        Ok(russh_sftp::protocol::Handle { id, handle: path })
    }
    async fn readdir(
        &mut self,
        id: u32,
        path: String,
    ) -> Result<russh_sftp::protocol::Name, Self::Error> {
        if !self.1.insert(path.clone()) {
            return Err(Self::Error::Eof);
        }
        let prefix = format!("{}/", path.trim_end_matches('/'));
        let files = self.0.lock().unwrap();
        let mut entries = Vec::new();
        for (name, data) in files.iter() {
            let Some(relative) = name.strip_prefix(&prefix) else {
                continue;
            };
            let directory = relative.ends_with('/');
            let relative = relative.trim_end_matches('/');
            if relative.is_empty() || relative.contains('/') {
                continue;
            }
            let mut attrs = russh_sftp::protocol::FileAttributes::empty();
            if directory {
                attrs.set_dir(true);
            } else {
                attrs.set_regular(true);
            }
            attrs.size = Some(data.len() as u64);
            attrs.mtime = Some(1700000000);
            entries.push(russh_sftp::protocol::File::new(relative, attrs));
        }
        Ok(russh_sftp::protocol::Name { id, files: entries })
    }
    async fn stat(
        &mut self,
        id: u32,
        path: String,
    ) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        let files = self.0.lock().unwrap();
        let mut attrs = russh_sftp::protocol::FileAttributes::empty();
        if let Some(data) = files.get(&path) {
            attrs.set_regular(true);
            attrs.size = Some(data.len() as u64);
        } else if path == "/" || files.contains_key(&format!("{path}/")) {
            attrs.set_dir(true);
        } else {
            return Err(Self::Error::NoSuchFile);
        }
        attrs.mtime = Some(1700000000);
        Ok(russh_sftp::protocol::Attrs { id, attrs })
    }
    async fn lstat(
        &mut self,
        id: u32,
        path: String,
    ) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        self.stat(id, path).await
    }
    async fn fstat(
        &mut self,
        id: u32,
        handle: String,
    ) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        self.stat(id, handle).await
    }
    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<russh_sftp::protocol::Data, Self::Error> {
        let files = self.0.lock().unwrap();
        let data = files.get(&handle).ok_or(Self::Error::NoSuchFile)?;
        let start = offset as usize;
        if start >= data.len() {
            return Err(Self::Error::Eof);
        }
        Ok(russh_sftp::protocol::Data {
            id,
            data: data[start..(start + len as usize).min(data.len())].to_vec(),
        })
    }
    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _: russh_sftp::protocol::FileAttributes,
    ) -> Result<russh_sftp::protocol::Status, Self::Error> {
        let mut files = self.0.lock().unwrap();
        if files.contains_key(&path) || files.contains_key(&format!("{path}/")) {
            return Err(Self::Error::Failure);
        }
        files.insert(format!("{path}/"), Vec::new());
        Ok(russh_sftp::protocol::Status {
            id,
            status_code: Self::Error::Ok,
            error_message: "OK".into(),
            language_tag: "en".into(),
        })
    }
    async fn open(
        &mut self,
        id: u32,
        filename: String,
        flags: russh_sftp::protocol::OpenFlags,
        _: russh_sftp::protocol::FileAttributes,
    ) -> Result<russh_sftp::protocol::Handle, Self::Error> {
        use russh_sftp::protocol::OpenFlags;
        let mut files = self.0.lock().unwrap();
        if flags.contains(OpenFlags::READ) && !flags.contains(OpenFlags::WRITE) {
            if !files.contains_key(&filename) {
                return Err(Self::Error::NoSuchFile);
            }
        } else {
            assert!(flags.contains(OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::EXCLUDE));
            assert!(!flags.contains(OpenFlags::TRUNCATE));
            if files.contains_key(&filename) || files.contains_key(&format!("{filename}/")) {
                return Err(Self::Error::Failure);
            }
            files.insert(filename.clone(), Vec::new());
        }
        Ok(russh_sftp::protocol::Handle {
            id,
            handle: filename,
        })
    }
    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<russh_sftp::protocol::Status, Self::Error> {
        let mut files = self.0.lock().unwrap();
        let bytes = files.get_mut(&handle).ok_or(Self::Error::Failure)?;
        assert_eq!(offset, bytes.len() as u64);
        bytes.extend_from_slice(&data);
        Ok(russh_sftp::protocol::Status {
            id,
            status_code: Self::Error::Ok,
            error_message: "OK".into(),
            language_tag: "en".into(),
        })
    }
    async fn close(
        &mut self,
        id: u32,
        _: String,
    ) -> Result<russh_sftp::protocol::Status, Self::Error> {
        Ok(russh_sftp::protocol::Status {
            id,
            status_code: Self::Error::Ok,
            error_message: "OK".into(),
            language_tag: "en".into(),
        })
    }
    async fn rename(
        &mut self,
        id: u32,
        old: String,
        new: String,
    ) -> Result<russh_sftp::protocol::Status, Self::Error> {
        let mut files = self.0.lock().unwrap();
        if files.contains_key(&new) || files.contains_key(&format!("{new}/")) {
            return Err(Self::Error::Failure);
        }
        let bytes = files.remove(&old).ok_or(Self::Error::Failure)?;
        files.insert(new, bytes);
        Ok(russh_sftp::protocol::Status {
            id,
            status_code: Self::Error::Ok,
            error_message: "OK".into(),
            language_tag: "en".into(),
        })
    }
    async fn remove(
        &mut self,
        id: u32,
        path: String,
    ) -> Result<russh_sftp::protocol::Status, Self::Error> {
        self.0
            .lock()
            .unwrap()
            .remove(&path)
            .ok_or(Self::Error::Failure)?;
        Ok(russh_sftp::protocol::Status {
            id,
            status_code: Self::Error::Ok,
            error_message: "OK".into(),
            language_tag: "en".into(),
        })
    }
}
struct TestServer(
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    FixtureFiles,
    std::collections::HashMap<ChannelId, Channel<server::Msg>>,
    Option<ChannelId>,
    Option<String>,
);
impl server::Handler for TestServer {
    type Error = russh::Error;
    async fn auth_password(&mut self, _: &str, _: &str) -> Result<server::Auth, Self::Error> {
        Ok(server::Auth::Accept)
    }
    async fn channel_open_session(
        &mut self,
        channel: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> Result<(), Self::Error> {
        self.3.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }
    async fn subsystem_request(
        &mut self,
        id: ChannelId,
        name: &str,
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        if name == "sftp" {
            let channel = self.3.remove(&id).unwrap();
            session.channel_success(id)?;
            russh_sftp::server::run(
                channel.into_stream(),
                TestSftp(self.2.clone(), Default::default()),
            )
            .await;
        } else {
            session.channel_failure(id)?;
        }
        Ok(())
    }
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _: &str,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: &[(russh::Pty, u32)],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        self.3.remove(&channel);
        session.channel_success(channel)
    }
    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        self.3.remove(&channel);
        self.4 = Some(channel);
        session.channel_success(channel)?;
        session.data(channel, bytes::Bytes::from_static(b"fixture:/home/test$ "))
    }
    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        if self.4 != Some(channel) {
            return Ok(());
        }
        if data == b"disconnect-fixture\n" {
            return session.close(channel);
        }
        if data == b"exit-fixture\n" {
            session.eof(channel)?;
            session.exit_status_request(channel, 0)?;
            return session.close(channel);
        }
        if data.starts_with(b"\x1b[") && data.ends_with(b"R") {
            if let Some(nonce) = self.5.take() {
                session.data(
                    channel,
                    bytes::Bytes::from_static(b"\r\nforeground result\r\n"),
                )?;
                session.data(
                    channel,
                    bytes::Bytes::from(format!("\x1b]777;rt-{nonce};E;7\x07")),
                )?;
                session.data(
                    channel,
                    bytes::Bytes::from_static(b"fixture:/home/test$ \x1b[6n"),
                )?;
            }
            return Ok(());
        }
        // Emulate a POSIX prompt on the existing PTY, without executing
        // any real command on the test machine.
        let text = String::from_utf8_lossy(data);
        if let Some((_, rest)) = text.split_once("]777;rt-")
            && let Some((nonce, _)) = rest.split_once(";B")
        {
            assert!(text.contains("eval"));
            assert_eq!(nonce.len(), 32);
            session.data(channel, bytes::Bytes::copy_from_slice(data))?;
            let begin = format!("\x1b]777;rt-{nonce};B\x07");
            let end = format!("\x1b]777;rt-{nonce};E;7\x07");
            // Fragment markers across SSH packets.
            session.data(
                channel,
                bytes::Bytes::copy_from_slice(&begin.as_bytes()[..12]),
            )?;
            session.data(
                channel,
                bytes::Bytes::copy_from_slice(&begin.as_bytes()[12..]),
            )?;
            if text.contains("cursor-query-fixture") {
                self.5 = Some(nonce.to_owned());
                session.data(channel, bytes::Bytes::from_static(b"\x1b[6n"))?;
                return Ok(());
            }
            if text.contains("pager-fixture") {
                session.data(channel, bytes::Bytes::from_static(b"\x1b[?1049h(END)"))?;
                return Ok(());
            }
            session.data(
                channel,
                bytes::Bytes::from_static(b"\r\nforeground result\r\n"),
            )?;
            session.data(
                channel,
                bytes::Bytes::copy_from_slice(&end.as_bytes()[..10]),
            )?;
            session.data(
                channel,
                bytes::Bytes::copy_from_slice(&end.as_bytes()[10..]),
            )?;
            session.data(channel, bytes::Bytes::from_static(b"fixture:/home/test$ "))?;
        } else {
            session.data(channel, bytes::Bytes::copy_from_slice(data))?;
        }
        Ok(())
    }
    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        let body = data
            .strip_prefix(crate::backend::AGENT_PAGER_ENV.as_bytes())
            .expect("agent exec must disable pagers");
        assert!(
            self.3.remove(&channel).is_some(),
            "exec must not request a PTY"
        );
        self.0.fetch_add(1, Ordering::Relaxed);
        session.channel_success(channel)?;
        // A manual run can provide a POSIX shell with less installed.
        // Run only the hardcoded probe, never arbitrary fixture commands.
        if body == PAGER_PROBE.as_bytes()
            && let Some(shell) = std::env::var_os("RTERM_TEST_PAGER_SHELL")
        {
            let script = format!("{}{PAGER_PROBE}", crate::backend::AGENT_PAGER_ENV);
            let output = tokio::time::timeout(
                Duration::from_secs(3),
                tokio::process::Command::new(shell)
                    .arg("-c")
                    .arg(script)
                    .stdin(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .output(),
            )
            .await
            .expect("less must exit without a PTY")
            .expect("run the local pager probe");
            session.data(channel, bytes::Bytes::from(output.stdout))?;
            session.extended_data(channel, 1, bytes::Bytes::from(output.stderr))?;
            session.exit_status_request(channel, output.status.code().unwrap() as u32)?;
            session.eof(channel)?;
            return session.close(channel);
        }
        session.data(channel, bytes::Bytes::copy_from_slice(body))?;
        if body == b"wait" {
            return Ok(());
        }
        if body == b"delayed" {
            tokio::time::sleep(Duration::from_millis(120)).await;
        }
        session.extended_data(channel, 1, bytes::Bytes::from_static(b"test stderr"))?;
        session.exit_status_request(channel, 7)?;
        session.eof(channel)?;
        session.close(channel)
    }
    async fn signal(
        &mut self,
        _: ChannelId,
        signal: russh::Sig,
        _: &mut server::Session,
    ) -> Result<(), Self::Error> {
        if matches!(signal, russh::Sig::TERM) {
            self.1.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

#[test]
fn local_agent_approval_expiry_and_ssh_output() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let signals = Arc::new(AtomicUsize::new(0));
    let files: FixtureFiles = Arc::new(Mutex::new(BTreeMap::new()));
    let port = runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let config = Arc::new(server::Config {
            keys: vec![
                ssh_key::PrivateKey::random(&mut rand::rng(), ssh_key::Algorithm::Ed25519).unwrap(),
            ],
            ..Default::default()
        });
        let count = count.clone();
        let signals = signals.clone();
        let files = files.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let config = config.clone();
                let handler = TestServer(
                    count.clone(),
                    signals.clone(),
                    files.clone(),
                    Default::default(),
                    None,
                    None,
                );
                tokio::spawn(async move {
                    if let Ok(running) = server::run_stream(config, stream, handler).await {
                        let _ = running.await;
                    }
                });
            }
        });
        port
    });
    let context = egui::Context::default();
    let mut app = App::with_settings(&context, Settings::benchmark_defaults(), false);
    assert_eq!(app.agent_mode(), Mode::Disabled);
    assert!(!app.settings.on("ai_auto_open"));
    assert!(!app.settings.on("ai_upload"));
    assert!(!app.settings.on("ai_interactive"));
    app.settings.set("ai_interactive", "1".into());
    assert!(!app.settings.on("ai_audit_vault"));
    app.copy_agent_prompt(&context);
    assert!(!app.agent.active());
    app.start_agent(&context);
    assert!(!app.agent.active());
    app.dialog = None;
    app.settings.set("ai_access", "ask".into());
    app.profiles.push(Profile {
        name: "Test SSH".into(),
        connection: Connection::Ssh {
            terminal: "xterm-256color".into(),
            config: crate::models::SshConfig {
                host: "127.0.0.1".into(),
                port,
                user: "test".into(),
                password: Some("fixture-secret-not-for-discovery".into()),
                key_path: None,
                key_name: None,
                vault_pass: None,
                name: None,
                compression: None,
                key_passphrase: None,
                use_agent: false,
                keepalive: 0,
            },
        },
    });
    app.profiles.push(Profile {
        name: "Local fixture".into(),
        connection: Connection::Local {
            shell: String::new(),
        },
    });
    fn pump(app: &mut App, context: &egui::Context, ready: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready(app) {
            app.poll(context);
            app.poll_agent(context);
            assert!(Instant::now() < deadline, "agent test timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    app.start_agent(&context);
    assert!(app.agent.active());
    let prompt = app.agent.bridge.as_ref().unwrap().prompt(120);
    assert!(prompt.contains("rterm-agent sessions"));
    assert!(prompt.contains("rterm-agent execute"));
    assert!(prompt.contains("User-configured maximum command timeout: 120"));
    assert!(
        !prompt.contains(
            &*std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .to_string_lossy()
        )
    );
    assert!(!prompt.contains("C:\\Projects"));
    let post = |app: &App, value| app.agent.bridge.as_ref().unwrap().test_ipc(value);
    let invalid = post(&app, json!({"action":"list_sessions","unexpected":true}));
    assert_eq!(invalid.join().unwrap()["ok"], false);
    let invalid = post(&app, json!({"action":"unsupported"}));
    assert_eq!(invalid.join().unwrap()["ok"], false);
    let list = post(&app, json!({"action":"list_sessions"}));
    pump(&mut app, &context, |_| list.is_finished());
    let sessions = list.join().unwrap();
    assert_eq!(sessions["sessions"], json!([]));
    assert_eq!(sessions["saved_sessions"].as_array().unwrap().len(), 1);
    assert!(
        !sessions
            .to_string()
            .contains("fixture-secret-not-for-discovery")
    );
    let profile_id = sessions["saved_sessions"][0]["profile_id"]
        .as_str()
        .unwrap();
    assert_eq!(sessions["protocol_version"], 2);
    let again = post(&app, json!({"action":"list_sessions"}));
    pump(&mut app, &context, |_| again.is_finished());
    assert_eq!(
        again.join().unwrap()["saved_sessions"][0]["profile_id"],
        profile_id
    );
    let open = |app: &App, op: &str| {
        post(
            app,
            json!({"action":"open_session","profile_id":profile_id,"operation_id":op}),
        )
    };
    let denied = open(&app, "open-denied");
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    app.resolve_agent_permission(false);
    assert_eq!(denied.join().unwrap()["ok"], false);
    assert!(app.terminals.is_empty());
    let cancelled = open(&app, "open-cancelled");
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    let cancel = post(
        &app,
        json!({"action":"cancel","operation_id":"open-cancelled"}),
    );
    assert_eq!(cancel.join().unwrap()["status"], "cancelled");
    app.resolve_agent_permission(true);
    assert_eq!(cancelled.join().unwrap()["status"], "cancelled");
    assert!(app.terminals.is_empty());
    let stale = open(&app, "open-stale");
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    app.profiles[0].name = "Changed fixture".into();
    app.resolve_agent_permission(true);
    assert_eq!(stale.join().unwrap()["ok"], false);
    assert!(app.terminals.is_empty());
    app.profiles[0].name = "Test SSH".into();
    // Even unrestricted command mode does not silently start connections.
    app.settings.set("ai_access", "unrestricted".into());
    let approved = open(&app, "open-approved");
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    assert!(app.terminals.is_empty());
    app.resolve_agent_permission(true);
    let opened = approved.join().unwrap();
    assert_eq!(opened["ok"], true);
    assert_eq!(opened["connection_state"], "connecting");
    assert_eq!(app.terminals.len(), 1);
    assert_eq!(app.active, Some(opened["tab_id"].as_u64().unwrap()));
    let duplicate = open(&app, "open-approved");
    assert_eq!(duplicate.join().unwrap(), opened);
    assert_eq!(app.terminals.len(), 1);
    assert!(app.agent.grants.is_empty());
    app.settings.set("ai_access", "ask".into());
    pump(&mut app, &context, |app| app.host_trust.blocking());
    assert!(app.agent_sessions().is_empty());
    assert!(app.approve_fixture_host());
    pump(&mut app, &context, |app| !app.agent_sessions().is_empty());
    let id = app.agent_sessions()[0].0;
    let tab = app.terminals[0].tab;
    let initial_endpoint = app.terminals[0].endpoint.unwrap();
    assert_eq!(opened["session_id"], id);
    assert_ne!(initial_endpoint, id);
    let list = post(&app, json!({"action":"list_sessions"}));
    pump(&mut app, &context, |_| list.is_finished());
    let sessions = list.join().unwrap();
    assert_eq!(sessions["tabs"][0]["tab_id"], opened["tab_id"]);
    assert_eq!(sessions["tabs"][0]["state"], "connected");
    assert_eq!(sessions["sessions"][0]["session_id"], id);
    assert_eq!(sessions["active_session_id"], id);
    let unknown = post(
        &app,
        json!({"action":"execute","session_id":id+100,"command":"noop"}),
    );
    pump(&mut app, &context, |_| unknown.is_finished());
    assert_eq!(unknown.join().unwrap()["ok"], false);
    let execute = |app: &App, command: &str| {
        post(
            app,
            json!({"action":"execute","session_id":id,"command":command}),
        )
    };
    // Unrestricted foreground execution uses the existing PTY from the
    // first command. The actual output reaches the terminal grid.
    app.settings.set("ai_access", "unrestricted".into());
    let list = post(&app, json!({"action":"list_sessions"}));
    pump(&mut app, &context, |_| list.is_finished());
    assert_eq!(list.join().unwrap()["tabs"][0]["shell_confirmed"], true);
    let foreground = execute(&app, "pwd; echo foreground");
    pump(&mut app, &context, |app| {
        assert!(
            app.agent.running.is_empty(),
            "foreground command must wait for approval"
        );
        app.agent.pending.is_some()
    });
    app.resolve_agent_permission(true);
    pump(&mut app, &context, |app| !app.agent.running.is_empty());
    assert_eq!(app.active, Some(tab));
    assert_eq!(
        app.agent.activity.entries()[0].command,
        "pwd; echo foreground"
    );
    assert_eq!(count.load(Ordering::Relaxed), 0);
    pump(&mut app, &context, |_| foreground.is_finished());
    let result = foreground.join().unwrap();
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["execution_mode"], "interactive_shell");
    assert_eq!(result["stdout"], "\r\nforeground result\r\n");
    assert_eq!(result["stderr"], "");
    assert_eq!(result["exit_code"], 7);
    assert_eq!(count.load(Ordering::Relaxed), 0);
    assert_eq!(app.terminals.len(), 1);
    pump(&mut app, &context, |app| app.agent.running.is_empty());
    let entries = app.agent.activity.entries();
    assert_eq!(entries[0].exit_code, Some(7));
    assert_eq!(entries[0].phase, "completed");
    assert!(entries[0].finished_ms.is_some());
    #[cfg(feature = "benchmark")]
    assert!(app.terminals[0].visible_line_matches(|line| line.contains("foreground result")));
    assert!(app.agent.displays.is_empty());
    // Unrestricted foreground execution continues on the same PTY
    // after a nonzero exit and handles automatic cursor reports.
    let query = execute(&app, "echo cursor-query-fixture");
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    app.resolve_agent_permission(true);
    pump(&mut app, &context, |_| query.is_finished());
    let result = query.join().unwrap();
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["exit_code"], 7);
    pump(&mut app, &context, |app| app.agent.running.is_empty());
    let tunnel = app.agent.bridge.as_ref().unwrap().mcp_access();
    let tunnel_command = std::thread::spawn(move || {
        tunnel.request(json!({"action":"execute","session_id":id,"command":"echo tunnel fixture","operation_id":"tunnel-shell-fixture","wait_ms":4000}))
    });
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    assert!(
        app.agent
            .is_tunnel_request(app.agent.pending.as_ref().unwrap())
    );
    app.resolve_agent_permission(true);
    pump(&mut app, &context, |_| tunnel_command.is_finished());
    let result = tunnel_command.join().unwrap();
    assert_eq!(result["execution_mode"], "interactive_shell");
    assert_eq!(result["exit_code"], 7);
    assert!(result.get("output_path").is_none());
    pump(&mut app, &context, |app| app.agent.running.is_empty());
    assert!(
        app.agent.activity.entries()[0]
            .action
            .contains("OpenAI tunnel")
    );
    let followup = execute(&app, "echo followup");
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    app.resolve_agent_permission(true);
    pump(&mut app, &context, |_| followup.is_finished());
    assert_eq!(
        followup.join().unwrap()["execution_mode"],
        "interactive_shell"
    );
    pump(&mut app, &context, |app| app.agent.running.is_empty());
    assert_eq!(count.load(Ordering::Relaxed), 0);
    // Full-screen programs still block dispatch without an approval dialog.
    app.terminals[0].receive(b"\x1b[?1049h");
    let blocked = execute(&app, "echo blocked");
    pump(&mut app, &context, |app| {
        assert!(app.agent.pending.is_none());
        blocked.is_finished()
    });
    let result = blocked.join().unwrap();
    assert_eq!(result["status"], "failed");
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("Interactive shell unavailable")
    );
    app.terminals[0].receive(b"\x1b[?1049l");
    // Returning to the shell after typing still requires approval.
    assert!(app.terminals[0].send(&app.backend, b"\r".to_vec()));
    let resumed = execute(&app, "echo after manual input");
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    app.resolve_agent_permission(true);
    pump(&mut app, &context, |_| resumed.is_finished());
    assert_eq!(
        resumed.join().unwrap()["execution_mode"],
        "interactive_shell"
    );
    pump(&mut app, &context, |app| app.agent.running.is_empty());
    app.settings.set("ai_access", "ask".into());
    let denied = execute(&app, "echo foreground denied");
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    assert_eq!(app.agent.activity.entries()[0].phase, "awaiting_approval");
    app.resolve_agent_permission(false);
    assert_eq!(denied.join().unwrap()["ok"], false);
    app.refresh_agent_activity();
    assert_eq!(
        app.agent.activity.entries()[0].detail.as_deref(),
        Some("User denied command")
    );
    let approved = execute(&app, "echo foreground approved");
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    app.resolve_agent_permission(true);
    pump(&mut app, &context, |_| approved.is_finished());
    assert_eq!(
        approved.join().unwrap()["execution_mode"],
        "interactive_shell"
    );
    pump(&mut app, &context, |app| app.agent.running.is_empty());
    let revoked = execute(&app, "echo foreground disabled");
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    app.settings.set("ai_access", "disabled".into());
    app.poll_agent(&context);
    assert_eq!(revoked.join().unwrap()["ok"], false);
    assert!(!app.agent.active());
    assert!(app.agent.pending.is_none());
    app.agent.activity.clear();
    app.settings.set("ai_access", "ask".into());
    app.settings.set("ai_interactive", "0".into());
    app.start_agent(&context);
    let denied = execute(&app, "denied");
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    assert_eq!(count.load(Ordering::Relaxed), 0);
    app.resolve_agent_permission(false);
    assert_eq!(denied.join().unwrap()["ok"], false);
    assert_eq!(count.load(Ordering::Relaxed), 0);
    assert!(app.agent.displays.is_empty());
    let once = execute(&app, "one command");
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    app.active = None;
    app.resolve_agent_permission(true);
    assert_eq!(app.active, None);
    pump(&mut app, &context, |_| once.is_finished());
    let result = once.join().unwrap();
    assert_eq!(result["stdout"], "one command");
    assert_eq!(result["stderr"], "test stderr");
    assert_eq!(result["exit_code"], 7);
    assert_eq!(result["ok"], true);
    assert_eq!(result["command_success"], false);
    assert_eq!(result["status"], "completed");
    assert_eq!(count.load(Ordering::Relaxed), 1);
    pump(&mut app, &context, |app| app.agent.running.is_empty());
    assert!(app.agent.grants.is_empty());
    let timed = execute(&app, "grant");
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    app.agent.minutes = 5;
    app.resolve_agent_permission(true);
    pump(&mut app, &context, |_| timed.is_finished());
    assert_eq!(timed.join().unwrap()["stdout"], "grant");
    let next = execute(&app, "followup");
    pump(&mut app, &context, |_| next.is_finished());
    assert_eq!(next.join().unwrap()["stdout"], "followup");
    assert_eq!(count.load(Ordering::Relaxed), 3);
    let tunnel = app.agent.bridge.as_ref().unwrap().mcp_access();
    let remote = std::thread::spawn({
        let tunnel = tunnel.clone();
        move || {
            tunnel.request(json!({"action":"execute","session_id":id,"command":"must ask through the tunnel","operation_id":"tunnel-approval-fixture","wait_ms":5000}))
        }
    });
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    assert!(
        app.agent
            .is_tunnel_request(app.agent.pending.as_ref().unwrap())
    );
    app.resolve_agent_permission(false);
    assert_eq!(remote.join().unwrap()["status"], "failed");
    assert_eq!(count.load(Ordering::Relaxed), 3);
    assert_eq!(
        tunnel.request(json!({"action":"history"}))["operations"][0]["operation_id"],
        "tunnel-approval-fixture"
    );
    let local_history = post(&app, json!({"action":"history"})).join().unwrap();
    assert!(
        !local_history
            .to_string()
            .contains("tunnel-approval-fixture")
    );
    tunnel.stop();
    assert_eq!(tunnel.request(json!({"action":"history"}))["ok"], false);
    app.agent
        .grants
        .insert(id, Instant::now() - Duration::from_secs(1));
    let expired = execute(&app, "expired");
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    assert_eq!(count.load(Ordering::Relaxed), 3);
    app.agent.pending.as_mut().unwrap().deadline = Instant::now() - Duration::from_secs(1);
    app.resolve_agent_permission(true);
    assert_eq!(expired.join().unwrap()["ok"], false);
    assert_eq!(count.load(Ordering::Relaxed), 3);
    let running = execute(&app, "wait");
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    app.resolve_agent_permission(true);
    pump(&mut app, &context, |_| count.load(Ordering::Relaxed) == 4);
    assert!(app.agent.displays.is_empty());
    app.agent.stop();
    assert_eq!(running.join().unwrap()["ok"], false);
    assert!(!app.agent.active());
    assert!(app.agent.grants.is_empty());
    assert!(app.agent.activity.saved().is_empty());
    // Revocation completes the client's reply immediately, but the SSH
    // channel must finish cleanup before a new operation can be accepted.
    pump(&mut app, &context, |app| app.agent.running.is_empty());
    app.settings.set("ai_access", "unrestricted".into());
    app.copy_agent_prompt(&context);
    assert!(!app.agent.active());
    app.start_agent(&context);
    assert!(app.agent.active());
    app.settings.set("ai_interactive", "0".into());
    app.agent.remove_display(tab);
    app.active = None;
    let unrestricted = execute(&app, "unrestricted");
    pump(&mut app, &context, |_| unrestricted.is_finished());
    let unrestricted = unrestricted.join().unwrap();
    assert_eq!(unrestricted["stdout"], "unrestricted", "{unrestricted}");
    assert_eq!(count.load(Ordering::Relaxed), 5);
    assert!(app.agent.displays.is_empty());
    assert_eq!(app.active, None);
    // Keep the remaining isolated exec-channel regression tests in
    // background mode; foreground has its own same-PTY assertions.

    // A caller can receive an ID before the UI sees the request, then
    // cancel a pending approval without any new SSH exec.
    app.agent.stop();
    app.settings.set("ai_access", "ask".into());
    app.start_agent(&context);
    let submit = |app: &App, op: &str, cmd: &str, wait: u64| {
        post(
            app,
            json!({"action":"execute","session_id":id,"command":cmd,"operation_id":op,"wait_ms":wait}),
        )
    };
    let poll = |app: &App, op: &str, wait: u64| {
        post(
            app,
            json!({"action":"poll","operation_id":op,"wait_ms":wait}),
        )
    };
    let cancel = |app: &App, op: &str| post(app, json!({"action":"cancel","operation_id":op}));
    let queued = submit(&app, "cancel-before-approval", "never-run", 0)
        .join()
        .unwrap();
    assert_eq!(queued["status"], "queued");
    assert_eq!(queued["operation_id"], "cancel-before-approval");
    assert!(queued["remaining_ms"].as_u64().unwrap() <= 60_000);
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    assert_eq!(
        poll(&app, "cancel-before-approval", 0).join().unwrap()["status"],
        "awaiting_approval"
    );
    assert_eq!(
        poll(&app, "cancel-before-approval", 0).join().unwrap()["hint"],
        "Waiting for approval in RTerm"
    );
    assert_eq!(
        cancel(&app, "cancel-before-approval").join().unwrap()["status"],
        "cancelled"
    );
    app.resolve_agent_permission(true); // a stale click cannot execute it
    assert_eq!(count.load(Ordering::Relaxed), 5);
    assert_eq!(
        poll(&app, "cancel-before-approval", 0).join().unwrap()["status"],
        "cancelled"
    );

    app.agent.stop();
    app.settings.set("ai_access", "unrestricted".into());
    app.start_agent(&context);
    let start = Instant::now();
    app.settings.set("ai_command_timeout", "600".into());
    assert_eq!(
        post(&app, json!({"action":"execute","session_id":id,"command":"wait","operation_id":"stuck","wait_ms":0,"timeout_secs":900})).join().unwrap()["status"],
        "queued"
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    pump(&mut app, &context, |_| count.load(Ordering::Relaxed) == 6);
    let start = Instant::now();
    let pending = poll(&app, "stuck", 150).join().unwrap();
    assert_eq!(pending["status"], "running");
    assert!(pending["remaining_ms"].as_u64().unwrap() > 590_000);
    assert!(pending["remaining_ms"].as_u64().unwrap() <= 600_000);
    assert!(
        start.elapsed() >= Duration::from_millis(130) && start.elapsed() < Duration::from_secs(2)
    );
    assert_eq!(
        post(&app, json!({"action":"execute","session_id":id,"command":"wait","operation_id":"stuck","wait_ms":0,"timeout_secs":900})).join().unwrap()["status"],
        "running"
    );
    assert_eq!(
        submit(&app, "stuck", "wait", 0).join().unwrap()["ok"],
        false
    );
    assert_eq!(
        submit(&app, "stuck", "different", 0).join().unwrap()["ok"],
        false
    );
    assert_eq!(
        submit(&app, "another", "nope", 0).join().unwrap()["ok"],
        false
    );
    assert_eq!(count.load(Ordering::Relaxed), 6);
    let long_poll = poll(&app, "stuck", 5000);
    let second_poll = poll(&app, "stuck", 5000);
    std::thread::sleep(Duration::from_millis(40));
    assert!(!long_poll.is_finished());
    let invalid = post(
        &app,
        json!({"action":"cancel","operation_id":"stuck","unexpected":true}),
    );
    assert_eq!(invalid.join().unwrap()["ok"], false);
    assert_eq!(poll(&app, "stuck", 0).join().unwrap()["status"], "running");
    let prior_signals = signals.load(Ordering::Relaxed);
    let start = Instant::now();
    let cancelled = cancel(&app, "stuck").join().unwrap();
    assert_eq!(cancelled["status"], "cancelled");
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(long_poll.join().unwrap()["status"], "cancelled");
    assert_eq!(second_poll.join().unwrap()["status"], "cancelled");
    assert_eq!(cancel(&app, "stuck").join().unwrap()["status"], "cancelled");
    pump(&mut app, &context, |app| app.agent.running.is_empty());
    pump(&mut app, &context, |_| {
        signals.load(Ordering::Relaxed) > prior_signals
    });

    // A new command works after cancellation; its completion can be
    // observed while the UI is deliberately not being pumped.
    assert_eq!(
        submit(&app, "recovery", "delayed", 0).join().unwrap()["status"],
        "queued"
    );
    pump(&mut app, &context, |app| !app.agent.running.is_empty());
    let result = poll(&app, "recovery", 2000).join().unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["stdout"], "delayed");
    assert_eq!(result["exit_code"], 7);
    assert!(result["duration_ms"].as_u64().unwrap() >= 100);
    let page = post(
        &app,
        json!({"action":"poll","operation_id":"recovery","since":0,"wait_ms":0}),
    )
    .join()
    .unwrap();
    assert_eq!(page["output"], "delayedtest stderr");
    assert_eq!(page["next_offset"], 18);
    assert_eq!(page["stdout"], "delayed");
    assert_eq!(page["stderr"], "test stderr");
    assert_eq!(page["combined_output"], false);
    let history = post(&app, json!({"action":"history","limit":1}))
        .join()
        .unwrap();
    assert_eq!(history["operations"][0]["operation_id"], "recovery");
    assert_eq!(history["operations"][0]["exit_code"], 7);
    assert_eq!(count.load(Ordering::Relaxed), 7);
    assert_eq!(
        submit(&app, "recovery", "delayed", 0).join().unwrap(),
        result
    );
    assert_eq!(cancel(&app, "recovery").join().unwrap(), result);
    assert_eq!(count.load(Ordering::Relaxed), 7);
    assert_eq!(poll(&app, "unknown-id", 0).join().unwrap()["ok"], false);
    assert!(poll(&app, "recovery", 30_001).join().unwrap()["ok"] == false);
    pump(&mut app, &context, |app| app.agent.running.is_empty());

    if std::env::var_os("RTERM_AGENT_BENCH").is_some() {
        // Same loopback SSH connection, same command and UI polling cadence
        // for before/after comparisons. No human approval time included.
        fn profile(app: &mut App, context: &egui::Context, body: serde_json::Value, label: &str) {
            let mut samples = Vec::with_capacity(120);
            for index in 0..130 {
                let start = Instant::now();
                let task = app.agent.bridge.as_ref().unwrap().test_ipc(body.clone());
                while !task.is_finished() {
                    app.poll(context);
                    app.poll_agent(context);
                    assert!(start.elapsed() < Duration::from_secs(5));
                    std::thread::sleep(Duration::from_millis(1));
                }
                let response = task.join().unwrap();
                if label == "exec" {
                    assert_eq!(response["stdout"], "benchmark");
                    assert_eq!(response["exit_code"], 7);
                } else {
                    assert_eq!(response["ok"], true);
                }
                if index >= 10 {
                    samples.push(start.elapsed().as_secs_f64() * 1000.0);
                }
            }
            samples.sort_by(f64::total_cmp);
            let total: f64 = samples.iter().sum();
            println!(
                "AGENT {label}: n={} median={:.3}ms p95={:.3}ms mean={:.3}ms throughput={:.1}/s",
                samples.len(),
                samples[samples.len() / 2],
                samples[samples.len() * 95 / 100],
                total / samples.len() as f64,
                samples.len() as f64 * 1000.0 / total
            );
        }
        profile(
            &mut app,
            &context,
            json!({"action":"list_sessions"}),
            "list",
        );
        profile(
            &mut app,
            &context,
            json!({"action":"execute","session_id":id,"command":"benchmark"}),
            "exec",
        );
    }
    // Auto-open is an independent local preference; it must not
    // implicitly authorize commands or bypass profile validation.
    app.agent.stop();
    app.settings.set("ai_access", "ask".into());
    app.settings.set("ai_auto_open", "1".into());
    app.start_agent(&context);
    let list = post(&app, json!({"action":"list_sessions"}));
    pump(&mut app, &context, |_| list.is_finished());
    let sessions = list.join().unwrap();
    let profile = sessions["saved_sessions"][0]["profile_id"]
        .as_str()
        .unwrap();
    let reuse = post(
        &app,
        json!({"action":"open_session","profile_id":profile,"operation_id":"reuse-tab"}),
    );
    pump(&mut app, &context, |_| reuse.is_finished());
    let reused = reuse.join().unwrap();
    assert_eq!(reused["tab_id"], tab);
    assert_eq!(reused["session_id"], id);
    assert_eq!(reused["connection_state"], "connected");
    assert_eq!(app.terminals.len(), 1);

    let mut second_profile = app.profiles[0].clone();
    second_profile.name = "Second fixture SSH".into();
    app.profiles.push(second_profile);
    let list = post(&app, json!({"action":"list_sessions"}));
    pump(&mut app, &context, |_| list.is_finished());
    let sessions = list.join().unwrap();
    let profile = sessions["saved_sessions"][1]["profile_id"]
        .as_str()
        .unwrap();
    let open_body =
        json!({"action":"open_session","profile_id":profile,"operation_id":"auto-open"});
    let request = post(&app, open_body.clone());
    pump(&mut app, &context, |app| {
        assert!(app.agent.pending.is_none());
        request.is_finished()
    });
    let opened = request.join().unwrap();
    assert_eq!(opened["ok"], true);
    assert_eq!(opened["connection_state"], "connecting");
    let new_tab = opened["tab_id"].as_u64().unwrap();
    assert_eq!(app.active, Some(new_tab));
    assert_eq!(app.terminals.len(), 2);
    assert!(app.agent.grants.is_empty());
    assert_eq!(post(&app, open_body).join().unwrap(), opened);
    assert_eq!(app.terminals.len(), 2);
    pump(&mut app, &context, |app| app.agent_sessions().len() == 2);
    let new_id = opened["session_id"].as_u64().unwrap() as u32;
    let (new_endpoint, _) = app.agent_endpoint(new_id).unwrap();
    let first_endpoint = app.agent_endpoint(id).unwrap();
    app.split_existing(tab, splits::Direction::Right);
    assert!(app.splits.contains(tab) && app.splits.contains(new_tab));
    assert_eq!(app.agent_endpoint(id), Some(first_endpoint));
    assert_eq!(app.agent_endpoint(new_id), Some((new_endpoint, new_tab)));
    assert_eq!(app.terminals.len(), 2);

    let command = post(
        &app,
        json!({"action":"execute","session_id":new_id,"command":"must-ask","operation_id":"auto-open-command"}),
    );
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    app.resolve_agent_permission(false);
    assert_eq!(command.join().unwrap()["ok"], false);

    // Keep one session occupied while executing and discovering through the same bridge.
    app.settings.set("ai_access", "unrestricted".into());
    assert_eq!(
        submit(&app, "fleet-long", "wait", 0).join().unwrap()["status"],
        "queued"
    );
    pump(&mut app, &context, |app| app.agent.running_on(Some(id)));
    let second = post(
        &app,
        json!({"action":"execute","session_id":new_id,"command":"fleet quick","operation_id":"fleet-quick"}),
    );
    let list = post(&app, json!({"action":"list_sessions"}));
    pump(&mut app, &context, |_| {
        second.is_finished() && list.is_finished()
    });
    let result = second.join().unwrap();
    assert_eq!(result["execution_mode"], "background_exec");
    assert_eq!(result["stdout"], "fleet quick");
    assert_eq!(result["stderr"], "test stderr");
    assert_eq!(result["exit_code"], 7);
    assert_eq!(
        list.join().unwrap()["sessions"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        poll(&app, "fleet-long", 0).join().unwrap()["status"],
        "running"
    );
    let busy = submit(&app, "fleet-conflict", "never run", 0)
        .join()
        .unwrap();
    assert_eq!(busy["ok"], false);
    assert!(busy["error"].as_str().unwrap().contains("SSH session"));
    let tunnel = app.agent.bridge.as_ref().unwrap().mcp_access();
    let cross_transport = std::thread::spawn(move || {
        tunnel.request(json!({"action":"execute","session_id":id,"command":"never run through MCP","operation_id":"cross-transport"}))
    });
    pump(&mut app, &context, |_| cross_transport.is_finished());
    let busy = cross_transport.join().unwrap();
    assert_eq!(busy["ok"], false);
    assert!(busy["error"].as_str().unwrap().contains("SSH session"));
    assert_eq!(
        cancel(&app, "fleet-long").join().unwrap()["status"],
        "cancelled"
    );
    pump(&mut app, &context, |app| app.agent.running.is_empty());
    let recovered = execute(&app, "fleet recovered");
    pump(&mut app, &context, |_| recovered.is_finished());
    assert_eq!(recovered.join().unwrap()["stdout"], "fleet recovered");
    assert_eq!(app.terminals.len(), 2);

    // Unrestricted background access is automatic; foreground shell
    // commands still require approval because prompt detection is local.
    app.agent.stop();
    app.settings.set("ai_access", "unrestricted".into());
    app.settings.set("ai_interactive", "1".into());
    app.start_agent(&context);
    let before_exec = count.load(Ordering::Relaxed);
    for session_id in [id, new_id, id, new_id] {
        let call = post(
            &app,
            json!({"action":"execute","session_id":session_id,"command":"echo multi-session fixture"}),
        );
        // Service foreground requests through the non-painting native hook,
        // as if the compositor has suspended all redraws.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if eframe::App::native_background_update(&mut app, &context).1 {
                assert!(app.agent.pending.is_some());
                break;
            }
            assert!(Instant::now() < deadline, "foreground activation timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!eframe::App::native_background_update(&mut app, &context).1);
        assert_eq!(
            app.active,
            app.agent_endpoint(session_id).map(|(_, tab)| tab)
        );
        assert_eq!(count.load(Ordering::Relaxed), before_exec);
        assert!(app.splits.contains(tab) && app.splits.contains(new_tab));
        app.resolve_agent_permission(true);
        while !call.is_finished() {
            assert!(!eframe::App::native_background_update(&mut app, &context).1);
            assert!(Instant::now() < deadline, "foreground completion timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
        let result = call.join().unwrap();
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["execution_mode"], "interactive_shell");
        pump(&mut app, &context, |app| app.agent.running.is_empty());
    }
    assert_eq!(count.load(Ordering::Relaxed), before_exec);
    let entries = app.agent.activity.entries();
    let session_rows: Vec<_> = entries
        .iter()
        .filter(|entry| entry.command == "echo multi-session fixture")
        .collect();
    assert_eq!(session_rows.len(), 4);
    assert_eq!(
        session_rows
            .iter()
            .map(|entry| &entry.session)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        2
    );
    assert!(app.agent.activity.saved().is_empty());
    app.agent.stop();
    app.settings.set("ai_access", "ask".into());
    app.settings.set("ai_interactive", "0".into());
    app.start_agent(&context);
    let list = post(&app, json!({"action":"list_sessions"}));
    pump(&mut app, &context, |_| list.is_finished());
    let sessions = list.join().unwrap();
    let profile = sessions["saved_sessions"][0]["profile_id"]
        .as_str()
        .unwrap();
    app.profiles[0].name = "Changed again".into();
    let stale = post(
        &app,
        json!({"action":"open_session","profile_id":profile,"operation_id":"auto-open-stale"}),
    );
    pump(&mut app, &context, |_| stale.is_finished());
    assert_eq!(stale.join().unwrap()["ok"], false);
    assert_eq!(app.terminals.len(), 2);
    // File uploads use the actual loopback SFTP subsystem, never a real
    // server or user file. The fixture stores only bytes in memory.
    let fixture_dir = crate::storage::test_private_tempdir();
    #[cfg(unix)]
    let fixture_root = std::fs::canonicalize(fixture_dir.path()).unwrap();
    #[cfg(windows)]
    let fixture_root = fixture_dir.path().to_path_buf();
    let source = fixture_root.join("sample code.py");
    let payload = b"print('fixture upload')\n".repeat(4096);
    std::fs::write(&source, &payload).unwrap();
    let source = source.to_str().unwrap();
    let upload = |app: &App, op: &str| {
        post(
            app,
            json!({
                "action":"upload","session_id":new_id,"operation_id":op,
                "local":source,"remote":"/fixture/sample.py"
            }),
        )
    };
    // Unrestricted is not an upload opt-in.
    app.settings.set("ai_access", "unrestricted".into());
    let disabled = upload(&app, "upload-disabled");
    pump(&mut app, &context, |_| disabled.is_finished());
    assert_eq!(disabled.join().unwrap()["ok"], false);
    assert!(files.lock().unwrap().is_empty());
    app.settings.set("ai_upload", "1".into());
    app.settings.set("ai_access", "ask".into());
    let denied = upload(&app, "upload-denied");
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    app.resolve_agent_permission(false);
    assert_eq!(denied.join().unwrap()["ok"], false);
    assert!(files.lock().unwrap().is_empty());
    let approved = upload(&app, "upload-approved");
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    app.resolve_agent_permission(true);
    assert_eq!(app.active, Some(new_tab));
    pump(&mut app, &context, |_| approved.is_finished());
    let result = approved.join().unwrap();
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["bytes_uploaded"], payload.len() as u64);
    assert_eq!(files.lock().unwrap()["/fixture/sample.py"], payload);
    pump(&mut app, &context, |app| app.agent.running.is_empty());
    assert!(
        app.agent.displays[&new_tab]
            .lock()
            .unwrap()
            .status
            .as_ref()
            .unwrap()
            .contains("Upload complete")
    );
    assert_eq!(upload(&app, "upload-approved").join().unwrap(), result);
    // Unrestricted uploads use the same manifest and staging checks,
    // but must not queue an approval even with interactive shell mode enabled.
    app.settings.set("ai_access", "unrestricted".into());
    app.settings.set("ai_interactive", "1".into());
    let automatic = post(
        &app,
        json!({
            "action":"upload", "session_id":new_id, "operation_id":"upload-unrestricted",
            "local":source, "remote":"/fixture/automatic.py"
        }),
    );
    pump(&mut app, &context, |app| {
        assert!(app.agent.pending.is_none());
        automatic.is_finished()
    });
    let result = automatic.join().unwrap();
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["bytes_uploaded"], payload.len() as u64);
    assert_eq!(files.lock().unwrap()["/fixture/automatic.py"], payload);
    pump(&mut app, &context, |app| app.agent.running.is_empty());
    // Verify the identical policy for the tunnel transport.
    let tunnel = app.agent.bridge.as_ref().unwrap().mcp_access();
    let body = json!({
        "action":"upload", "session_id":new_id, "operation_id":"upload-unrestricted-mcp",
        "local":source, "remote":"/fixture/tunnel.py"
    });
    let automatic = std::thread::spawn(move || tunnel.request(body));
    pump(&mut app, &context, |app| {
        assert!(app.agent.pending.is_none());
        automatic.is_finished()
    });
    let result = automatic.join().unwrap();
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(files.lock().unwrap()["/fixture/tunnel.py"], payload);
    pump(&mut app, &context, |app| app.agent.running.is_empty());
    let conflict = upload(&app, "upload-existing");
    pump(&mut app, &context, |app| {
        assert!(app.agent.pending.is_none());
        conflict.is_finished()
    });
    assert_eq!(conflict.join().unwrap()["ok"], false);
    assert_eq!(files.lock().unwrap()["/fixture/sample.py"], payload);
    let project = fixture_root.join("project folder");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::create_dir_all(project.join("empty")).unwrap();
    std::fs::write(project.join("src").join("main.py"), &payload).unwrap();
    std::fs::write(project.join(".hidden"), b"fixture").unwrap();
    std::fs::write(project.join("empty.txt"), b"").unwrap();
    let folder_request = |app: &App, op: &str| {
        post(
            app,
            json!({
                "action":"upload","session_id":new_id,"operation_id":op,
                "local":project.to_str().unwrap(),"remote":"/fixture/project"
            }),
        )
    };
    app.settings.set("ai_interactive", "0".into());
    let folder = folder_request(&app, "upload-folder");
    pump(&mut app, &context, |app| {
        assert!(app.agent.pending.is_none());
        folder.is_finished()
    });
    let result = folder.join().unwrap();
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["bytes_uploaded"], payload.len() as u64 + 7);
    pump(&mut app, &context, |app| app.agent.running.is_empty());
    assert_eq!(
        app.agent.displays[&new_tab].lock().unwrap().progress,
        Some((payload.len() as u64 + 7, payload.len() as u64 + 7))
    );
    {
        let received = files.lock().unwrap();
        assert!(received.contains_key("/fixture/project/"));
        assert!(received.contains_key("/fixture/project/src/"));
        assert!(received.contains_key("/fixture/project/empty/"));
        assert_eq!(received["/fixture/project/src/main.py"], payload);
        assert_eq!(received["/fixture/project/.hidden"], b"fixture");
        assert_eq!(received["/fixture/project/empty.txt"], b"");
    }
    let before = files.lock().unwrap().clone();
    let conflict = folder_request(&app, "upload-folder-existing");
    pump(&mut app, &context, |app| {
        assert!(app.agent.pending.is_none());
        conflict.is_finished()
    });
    assert_eq!(conflict.join().unwrap()["ok"], false);
    assert_eq!(*files.lock().unwrap(), before);
    app.refresh_agent_activity();
    let entries = app.agent.activity.entries();
    for (operation, phase) in [
        ("upload-denied", "failed"),
        ("upload-approved", "completed"),
        ("upload-unrestricted", "completed"),
        ("upload-unrestricted-mcp", "completed"),
        ("upload-existing", "failed"),
        ("upload-folder", "completed"),
        ("upload-folder-existing", "failed"),
    ] {
        let entry = entries
            .iter()
            .find(|entry| entry.operation == operation)
            .unwrap();
        assert_eq!(entry.phase, phase, "{operation}");
        assert!(entry.finished_ms.is_some(), "{operation}");
    }

    // In shell mode a pager may never print the completion marker. Recover
    // that tab after cancellation and keep its ID stable across a later disconnect.
    app.settings.set("ai_interactive", "1".into());
    let first_tab = |app: &App| {
        app.terminals
            .iter()
            .position(|term| term.tab == tab)
            .unwrap()
    };
    assert_eq!(
        submit(&app, "pager-stuck", "pager-fixture", 0)
            .join()
            .unwrap()["status"],
        "queued"
    );
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    app.resolve_agent_permission(true);
    pump(&mut app, &context, |app| {
        app.agent.running_on(Some(id)) && !app.terminals[first_tab(app)].agent_prompt_available()
    });
    assert_eq!(
        cancel(&app, "pager-stuck").join().unwrap()["status"],
        "cancelled"
    );
    pump(&mut app, &context, |app| {
        let term = &app.terminals[first_tab(app)];
        term.agent_prompt_available() && term.endpoint != Some(initial_endpoint)
    });
    let recovered_id = app.terminals[first_tab(&app)].endpoint.unwrap();
    assert_eq!(app.terminals.len(), 2);
    assert!(app.terminals[first_tab(&app)].agent_prompt_available());
    let follow_up = post(
        &app,
        json!({"action":"execute","session_id":id,"command":"echo post recovery"}),
    );
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    app.resolve_agent_permission(true);
    pump(&mut app, &context, |_| follow_up.is_finished());
    assert_eq!(follow_up.join().unwrap()["exit_code"], 7);
    assert_eq!(
        post(&app, json!({"action":"execute","session_id":id,"command":"pager-fixture","operation_id":"pager-timeout","wait_ms":0,"timeout_secs":1})).join().unwrap()["status"],
        "queued"
    );
    pump(&mut app, &context, |app| app.agent.pending.is_some());
    app.resolve_agent_permission(true);
    pump(&mut app, &context, |app| {
        let term = &app.terminals[first_tab(app)];
        term.agent_prompt_available() && term.endpoint != Some(recovered_id)
    });
    assert_eq!(
        poll(&app, "pager-timeout", 0).join().unwrap()["status"],
        "timed_out"
    );
    let after_timeout = app.terminals[first_tab(&app)].endpoint.unwrap();
    assert!(
        app.backend
            .write(after_timeout, b"disconnect-fixture\n".to_vec())
            .is_ok()
    );
    pump(&mut app, &context, |app| app.ssh_retry_pending(tab));
    let discovery = post(&app, json!({"action":"list_sessions"}));
    let during_retry = post(
        &app,
        json!({"action":"execute","session_id":id,"command":"do not replay","operation_id":"during-reconnect"}),
    );
    pump(&mut app, &context, |_| {
        discovery.is_finished() && during_retry.is_finished()
    });
    let listed = discovery.join().unwrap();
    assert_eq!(listed["tabs"][0]["session_id"], id);
    assert_eq!(listed["tabs"][0]["state"], "reconnecting");
    let retry = during_retry.join().unwrap();
    assert_eq!(retry["error_code"], "session_reconnecting");
    assert_eq!(retry["session_id"], id);
    assert_eq!(retry["operation_started"], false);
    pump(&mut app, &context, |app| {
        let term = &app.terminals[first_tab(app)];
        term.agent_prompt_available() && term.endpoint != Some(after_timeout)
    });
    assert_eq!(app.terminals.len(), 2);
    assert_eq!(
        app.terminals
            .iter()
            .find(|term| term.tab == new_tab)
            .unwrap()
            .endpoint,
        Some(new_endpoint)
    );
    assert!(app.splits.contains(tab) && app.splits.contains(new_tab));
    app.settings.set("ai_interactive", "0".into());
    let stable_id = execute(&app, "still the same session");
    pump(&mut app, &context, |_| stable_id.is_finished());
    let result = stable_id.join().unwrap();
    assert_eq!(result["execution_mode"], "background_exec");
    assert_eq!(result["stdout"], "still the same session");
    assert_eq!(result["stderr"], "test stderr");
    assert_eq!(result["exit_code"], 7);
    assert_eq!(
        poll(&app, "during-reconnect", 0).join().unwrap()["error_code"],
        "session_reconnecting"
    );
    if std::env::var_os("RTERM_TEST_PAGER_SHELL").is_some() {
        let pager = execute(&app, PAGER_PROBE);
        pump(&mut app, &context, |_| pager.is_finished());
        let result = pager.join().unwrap();
        assert_eq!(result["execution_mode"], "background_exec", "{result}");
        assert_eq!(result["combined_output"], false);
        assert_eq!(result["exit_code"], 17, "{result}");
        assert_eq!(
            result["stderr"].as_str().unwrap().trim(),
            "pager fixture stderr"
        );
        let lines: Vec<_> = result["stdout"].as_str().unwrap().lines().collect();
        assert_eq!(lines.len(), 100);
        assert!(
            lines
                .iter()
                .enumerate()
                .all(|(i, line)| line.parse() == Ok(i + 1))
        );
    }
    assert!(
        app.backend
            .write(new_endpoint, b"exit-fixture\n".to_vec())
            .is_ok()
    );
    pump(&mut app, &context, |app| {
        app.terminals
            .iter()
            .any(|term| term.tab == new_tab && term.closed)
    });
    assert!(!app.ssh_retry_pending(new_tab));
    let host = crate::host_keys::target("127.0.0.1", port);
    let expected = app.host_trust.store.list().unwrap()[&host].clone();
    let mismatched = crate::host_keys::Key {
        algorithm: expected.algorithm.clone(),
        fingerprint: "SHA256:changed-fixture".into(),
    };
    app.host_trust
        .store
        .change(&host, Some(&expected), Some(mismatched.clone()))
        .unwrap();
    let (endpoint, _) = app.agent_endpoint(id).unwrap();
    app.handle_event(BackendEvent::Closed(endpoint));
    pump(&mut app, &context, |app| app.host_trust.blocking());
    assert!(app.agent_endpoint(id).is_none());
    app.deny_fixture_replacement(&expected);
    assert!(!app.ssh_retry_pending(tab));
    assert!(app.agent_terminal(id).unwrap().closed);
    assert_eq!(app.host_trust.store.list().unwrap()[&host], mismatched);
    app.close_tab(new_tab);
    app.agent.stop();
}

#[test]
fn transcript_renders_control_bytes_literally() {
    let bytes = b"hello\r\n\x1b]52;c;clipboard\x07\x1b[?1049h\x1b[6n";
    let rendered = literal_text(bytes);
    assert!(rendered.starts_with("hello\n"));
    assert!(!rendered.contains('\x1b'));
    assert!(!rendered.contains('\x07'));
    assert!(rendered.contains("52;c;clipboard"));
    assert_eq!(literal_text("こんにちは".as_bytes()), "こんにちは");
    assert_eq!(literal_text("a\u{202e}b".as_bytes()), "a\\u{202e}b");
}

#[test]
fn agent_discovery_and_approval_work_without_any_painted_frame() {
    let context = egui::Context::default();
    let mut app = App::with_settings(&context, Settings::benchmark_defaults(), false);
    app.settings.set("ai_access", "ask".into());
    app.settings.set("ai_interactive", "1".into());
    app.profiles.push(Profile {
        name: "Hidden window fixture".into(),
        connection: Connection::Ssh {
            terminal: "xterm-256color".into(),
            config: crate::models::SshConfig {
                host: "127.0.0.1".into(),
                port: 22,
                user: "fixture".into(),
                password: Some("fixture-secret".into()),
                key_path: None,
                key_name: None,
                vault_pass: None,
                name: None,
                compression: None,
                key_passphrase: None,
                use_agent: false,
                keepalive: 0,
            },
        },
    });
    app.start_agent(&context);
    let post = |app: &App, value| app.agent.bridge.as_ref().unwrap().test_ipc(value);
    let local = post(&app, json!({"action":"list_sessions"}));
    let tunnel = app.agent.bridge.as_ref().unwrap().mcp_access();
    let remote = std::thread::spawn(move || tunnel.request(json!({"action":"list_sessions"})));
    let deadline = Instant::now() + Duration::from_secs(2);
    while !local.is_finished() || !remote.is_finished() {
        let (wake, raise) = eframe::App::native_background_update(&mut app, &context);
        assert!(wake.is_some(), "agent should schedule native wakeups");
        assert!(!raise, "read-only discovery must not steal focus");
        assert!(Instant::now() < deadline, "hidden discovery timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
    let local = local.join().unwrap();
    let remote = remote.join().unwrap();
    assert_eq!(local["ok"], true);
    assert_eq!(remote["ok"], true);
    assert_eq!(local["execution_mode"], "interactive_shell");
    assert_eq!(local["saved_sessions"], remote["saved_sessions"]);
    assert!(!local.to_string().contains("fixture-secret"));
    let profile_id = local["saved_sessions"][0]["profile_id"].as_str().unwrap();
    let approval = post(
        &app,
        json!({
            "action":"open_session", "profile_id":profile_id,
            "operation_id":"hidden-approval", "wait_ms":0
        }),
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let (_, raise) = eframe::App::native_background_update(&mut app, &context);
        if raise {
            assert!(app.agent.pending.is_some());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "hidden approval did not request activation"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!eframe::App::native_background_update(&mut app, &context).1);
    assert!(app.terminals.is_empty(), "activation cannot grant approval");
    assert_eq!(approval.join().unwrap()["ok"], true);
    app.resolve_agent_permission(false);
    assert!(app.terminals.is_empty());
    app.stop_agent();
    let (wake, raise) = eframe::App::native_background_update(&mut app, &context);
    assert!(wake.is_none());
    assert!(!raise);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires a running Wayland desktop; opens and minimizes an isolated fixture window"]
fn native_wayland_minimized_agent_discovery() {
    use winit::application::ApplicationHandler;
    use winit::event::{StartCause, WindowEvent};
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
    use winit::platform::wayland::EventLoopBuilderExtWayland;
    use winit::window::WindowId;

    const INTERVAL: Duration = Duration::from_millis(250);
    // Default regression is short; the same test supports a longer soak run.
    let request_count = std::env::var("RTERM_WAYLAND_PROBE_REQUESTS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(48)
        .clamp(48, 240);

    // No real profiles, credentials, sessions, or tunnel-client are used.
    struct Probe {
        app: App,
        frames: Arc<AtomicUsize>,
        services: Arc<AtomicUsize>,
        minimize_issued: Arc<AtomicBool>,
        minimize_at: Instant,
    }

    impl eframe::App for Probe {
        fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
            // Deliberately never service the app from paint. Some compositors
            // continue to paint thumbnails of minimized windows.
            egui::CentralPanel::default().show(context, |ui| {
                ui.label("RTerm minimized test fixture");
            });
            self.frames.fetch_add(1, Ordering::Relaxed);
            // First let the compositor map and present a visible window.
            // Then minimize it while another redraw is pending. Keep asking
            // for frames to exercise the exact EGL stall seen in the live app.
            if Instant::now() >= self.minimize_at
                && !self.minimize_issued.swap(true, Ordering::Release)
            {
                context.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
            context.request_repaint_after(Duration::from_millis(16));
        }

        fn native_background_update(&mut self, context: &egui::Context) -> (Option<Instant>, bool) {
            let previous = self.app.last_service;
            let result = eframe::App::native_background_update(&mut self.app, context);
            if self.app.last_service != previous {
                self.services.fetch_add(1, Ordering::Relaxed);
            }
            result
        }
    }

    type Report = (
        Vec<serde_json::Value>,
        usize,
        usize,
        usize,
        Duration,
        Duration,
    );
    struct Harness<'a> {
        native: eframe::EframeWinitApplication<'a>,
        receive: std::sync::mpsc::Receiver<Report>,
        report: Option<Report>,
        deadline: Instant,
    }

    impl ApplicationHandler<eframe::UserEvent> for Harness<'_> {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            self.native.resumed(event_loop);
        }

        fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
            self.native.window_event(event_loop, id, event);
        }

        fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
            self.native.new_events(event_loop, cause);
        }

        fn user_event(&mut self, event_loop: &ActiveEventLoop, event: eframe::UserEvent) {
            self.native.user_event(event_loop, event);
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            self.native.about_to_wait(event_loop);
            if let Ok(report) = self.receive.try_recv() {
                self.report = Some(report);
                event_loop.exit();
            } else if Instant::now() >= self.deadline {
                event_loop.exit();
            } else {
                // Only impose a test-wide timeout, not periodic test wakeups.
                // Native service must establish its own shorter deadline.
                match event_loop.control_flow() {
                    ControlFlow::Wait => {
                        event_loop.set_control_flow(ControlFlow::WaitUntil(self.deadline))
                    }
                    ControlFlow::WaitUntil(at) if at > self.deadline => {
                        event_loop.set_control_flow(ControlFlow::WaitUntil(self.deadline));
                    }
                    _ => {}
                }
            }
        }

        fn exiting(&mut self, event_loop: &ActiveEventLoop) {
            self.native.exiting(event_loop);
        }
    }

    let mut builder = EventLoop::<eframe::UserEvent>::with_user_event();
    builder.with_wayland().with_any_thread(true);
    let event_loop = builder.build().expect("Wayland test event loop");
    let (send, receive) = std::sync::mpsc::channel();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RTerm minimized regression fixture")
            .with_inner_size([640.0, 480.0]),
        ..Default::default()
    };
    let native = eframe::create_native(
        "rterm-minimized-fixture",
        options,
        Box::new(move |creation| {
            let mut app =
                App::with_settings(&creation.egui_ctx, Settings::benchmark_defaults(), false);
            app.settings.set("ai_access", "ask".into());
            app.start_agent(&creation.egui_ctx);
            let tunnel = app.agent.bridge.as_ref().unwrap().mcp_access();
            let frames = Arc::new(AtomicUsize::new(0));
            let services = Arc::new(AtomicUsize::new(0));
            let minimize_issued = Arc::new(AtomicBool::new(false));
            let worker_frames = frames.clone();
            let worker_services = services.clone();
            let worker_minimize = minimize_issued.clone();
            std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(5);
                while !worker_minimize.load(Ordering::Acquire) {
                    assert!(Instant::now() < deadline, "minimize command not issued");
                    std::thread::sleep(Duration::from_millis(10));
                }
                std::thread::sleep(Duration::from_millis(800));
                let before = worker_frames.load(Ordering::Relaxed);
                let initial_services = worker_services.load(Ordering::Relaxed);
                let started = Instant::now();
                let mut replies = Vec::new();
                let mut longest = Duration::ZERO;
                for _ in 0..request_count {
                    let request_started = Instant::now();
                    let reply = tunnel.request(json!({"action":"list_sessions"}));
                    longest = longest.max(request_started.elapsed());
                    let succeeded = reply["ok"] == true;
                    replies.push(reply);
                    if !succeeded {
                        break;
                    }
                    std::thread::sleep(INTERVAL);
                }
                let elapsed = started.elapsed();
                let after = worker_frames.load(Ordering::Relaxed);
                let serviced = worker_services.load(Ordering::Relaxed) - initial_services;
                let _ = send.send((replies, before, after, serviced, elapsed, longest));
            });
            Ok(Box::new(Probe {
                app,
                frames,
                services,
                minimize_issued,
                minimize_at: Instant::now() + Duration::from_secs(1),
            }))
        }),
        &event_loop,
    );
    let mut harness = Harness {
        native,
        receive,
        report: None,
        deadline: Instant::now() + INTERVAL * request_count as u32 + Duration::from_secs(30),
    };
    event_loop.run_app(&mut harness).unwrap();
    let (replies, before, after, serviced, elapsed, longest) =
        harness.report.expect("native fixture timed out");
    eprintln!(
        "Wayland minimize requested: {before} to {after} paints, {serviced} native services, {} requests in {elapsed:?}, slowest {longest:?}",
        replies.len()
    );
    assert!(before > 1, "fixture must paint before minimizing");
    assert_eq!(replies.len(), request_count, "{replies:?}");
    assert!(
        replies.iter().all(|reply| reply["ok"] == true),
        "{replies:?}"
    );
    assert!(
        serviced >= request_count,
        "native timer failed to service idle app"
    );
    assert!(
        longest < Duration::from_secs(1),
        "discovery stalled: {longest:?}"
    );
}

#[test]
fn sftp_only_authenticates_without_shell_and_transfers_files() {
    struct FileServer {
        files: FixtureFiles,
        channels: std::collections::HashMap<ChannelId, Channel<server::Msg>>,
        shell_requests: Arc<AtomicUsize>,
    }
    impl server::Handler for FileServer {
        type Error = russh::Error;
        async fn auth_password(&mut self, _: &str, _: &str) -> Result<server::Auth, Self::Error> {
            Ok(server::Auth::Accept)
        }
        async fn channel_open_session(
            &mut self,
            channel: Channel<server::Msg>,
            reply: server::ChannelOpenHandle,
            _: &mut server::Session,
        ) -> Result<(), Self::Error> {
            self.channels.insert(channel.id(), channel);
            reply.accept().await;
            Ok(())
        }
        async fn pty_request(
            &mut self,
            channel: ChannelId,
            _: &str,
            _: u32,
            _: u32,
            _: u32,
            _: u32,
            _: &[(russh::Pty, u32)],
            session: &mut server::Session,
        ) -> Result<(), Self::Error> {
            self.shell_requests.fetch_add(1, Ordering::Relaxed);
            session.channel_failure(channel)
        }
        async fn shell_request(
            &mut self,
            channel: ChannelId,
            session: &mut server::Session,
        ) -> Result<(), Self::Error> {
            self.shell_requests.fetch_add(1, Ordering::Relaxed);
            session.channel_failure(channel)
        }
        async fn subsystem_request(
            &mut self,
            id: ChannelId,
            name: &str,
            session: &mut server::Session,
        ) -> Result<(), Self::Error> {
            if name == "sftp" {
                let channel = self.channels.remove(&id).unwrap();
                session.channel_success(id)?;
                russh_sftp::server::run(
                    channel.into_stream(),
                    TestSftp(self.files.clone(), Default::default()),
                )
                .await;
            } else {
                session.channel_failure(id)?;
            }
            Ok(())
        }
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let files: FixtureFiles = Arc::new(Mutex::new(BTreeMap::from([
        ("/remote.txt".into(), b"download fixture".to_vec()),
        ("/incoming/".into(), Vec::new()),
    ])));
    let shells = Arc::new(AtomicUsize::new(0));
    let port = runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let config = Arc::new(server::Config {
            keys: vec![
                ssh_key::PrivateKey::random(&mut rand::rng(), ssh_key::Algorithm::Ed25519).unwrap(),
            ],
            ..Default::default()
        });
        let files = files.clone();
        let shells = shells.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let handler = FileServer {
                    files: files.clone(),
                    channels: Default::default(),
                    shell_requests: shells.clone(),
                };
                let config = config.clone();
                tokio::spawn(async move {
                    if let Ok(running) = server::run_stream(config, stream, handler).await {
                        let _ = running.await;
                    }
                });
            }
        });
        port
    });
    let context = egui::Context::default();
    let mut app = App::with_settings(&context, Settings::benchmark_defaults(), false);
    app.settings.set("sftp_resume", "0".into());
    let config = crate::models::SshConfig {
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
    let profile = Profile {
        name: "File-only fixture".into(),
        connection: Connection::Sftp { config },
    };
    let encoded = serde_json::to_string(&profile).unwrap();
    assert!(serde_json::from_str::<Profile>(&encoded).unwrap().kind() == Kind::Sftp);
    assert!(
        !serde_json::to_string(&profile.without_credentials())
            .unwrap()
            .contains("\"password\":\"fixture\"")
    );
    app.connect(profile, None);
    let tab = app.active.unwrap();
    assert!(app.active_is_sftp());
    assert!(app.file_workspace_active());
    let pump = |app: &mut App, ready: &dyn Fn(&App) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready(app) {
            app.poll(&context);
            assert!(
                Instant::now() < deadline,
                "SFTP fixture timed out; message {:?}",
                app.message
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    pump(&mut app, &|app| app.host_trust.blocking());
    assert!(app.active_ssh().is_none());
    assert!(app.approve_fixture_host());
    pump(&mut app, &|app| app.active_ssh().is_some());
    assert!(app.terminals[0].sftp_ready);
    assert!(!app.terminals[0].starting());
    assert!(app.agent_sessions().is_empty());
    assert!(app.agent_endpoint(app.ssh_agent_id(tab).unwrap()).is_none());
    assert_eq!(shells.load(Ordering::Relaxed), 0);
    let id = app.active_ssh().unwrap();
    pump(&mut app, &|app| app.can_create_sftp_folder());
    app.new_sftp_folder();
    let draft = app
        .explorer
        .folder_draft
        .as_ref()
        .expect("SFTP inline folder draft");
    let crate::explorer::Destination::Remote(folder_id, directory) = draft.destination.clone()
    else {
        panic!("SFTP folder destination");
    };
    assert_eq!(folder_id, id);
    assert!(draft.name.is_empty());
    assert!(app.dialog.is_none());
    assert!(app.create_sftp_folder(id, &directory, "../escape").is_err());
    assert!(app.create_sftp_folder(id, &directory, " ").is_err());
    assert!(
        app.create_sftp_folder(id, "/wrong-directory", "new")
            .is_err()
    );
    let name = "New fixture folder";
    let path = crate::backend::join_remote(&directory, name);
    let render_folder = |app: &mut App, events| {
        context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| app.explorer_workspace(ui, ui.max_rect()));
            },
        )
    };
    render_folder(&mut app, vec![]);
    render_folder(&mut app, vec![egui::Event::Text(name.into())]);
    assert_eq!(app.explorer.folder_draft.as_ref().unwrap().name, name);
    assert!(!files.lock().unwrap().contains_key(&format!("{path}/")));
    render_folder(
        &mut app,
        vec![egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: Some(egui::Key::Enter),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
    );
    assert!(app.explorer.folder_draft.as_ref().unwrap().pending);
    pump(&mut app, &|app| {
        files.lock().unwrap().contains_key(&format!("{path}/"))
            && app.pending.is_empty()
            && app.can_create_sftp_folder()
    });
    assert!(app.explorer.folder_draft.is_none());
    app.new_sftp_folder();
    app.explorer.folder_draft.as_mut().unwrap().name = name.into();
    app.confirm_folder_draft();
    pump(&mut app, &|app| {
        app.explorer
            .folder_draft
            .as_ref()
            .is_some_and(|draft| draft.error.is_some() && !draft.pending)
    });
    assert!(app.message.is_none());
    assert!(files.lock().unwrap().contains_key(&format!("{path}/")));
    app.explorer.folder_draft.as_mut().unwrap().name = "Retry folder".into();
    app.confirm_folder_draft();
    pump(&mut app, &|app| {
        app.explorer.folder_draft.is_none() && app.pending.is_empty()
    });
    app.toggle_file_workspace();
    assert!(!app.explorer.expanded);
    pump(&mut app, &|app| app.can_create_sftp_folder());
    app.new_sftp_folder();
    assert!(app.explorer.folder_draft.is_some());
    assert!(app.dialog.is_none());
    app.toggle_file_workspace();
    let temp = crate::storage::test_private_tempdir();
    let shared = temp.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
    }
    let source = shared.join("tree");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(source.join("empty")).unwrap();
    std::fs::write(source.join("data.txt"), b"tree fixture").unwrap();
    let request = app.backend.request(Command::Sftp {
        id,
        action: crate::backend::RemoteAction::UploadTree {
            local: source,
            remote: "/incoming/tree".into(),
            batch: Default::default(),
        },
    });
    assert!(
        request
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_ok()
    );
    pump(&mut app, &|_| {
        files
            .lock()
            .unwrap()
            .get("/incoming/tree/data.txt")
            .is_some_and(|data| data == b"tree fixture")
    });
    assert!(files.lock().unwrap().contains_key("/incoming/tree/empty/"));
    let download = temp.path().join("download.txt");
    let request = app.backend.request(Command::Sftp {
        id,
        action: crate::backend::RemoteAction::Download {
            remote: "/remote.txt".into(),
            local: download.clone(),
            batch: Default::default(),
            directory: false,
            options: crate::backend::DownloadOptions {
                parallel: 1,
                max_bytes_per_second: 0,
                timestamps: false,
            },
            resume: None,
        },
    });
    assert!(
        request
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_ok()
    );
    pump(&mut app, &|_| {
        std::fs::read(&download).is_ok_and(|data| data == b"download fixture")
    });
    // Both drag uploads and picker uploads must ask before replacing a
    // destination, and a remembered answer applies only to its own batch.
    use crate::backend::{ConflictBatch, ConflictChoice, RemoteAction};
    let first = shared.join("first.txt");
    let second = shared.join("second.txt");
    std::fs::write(&first, b"new first").unwrap();
    std::fs::write(&second, b"new second").unwrap();
    {
        let mut remote = files.lock().unwrap();
        remote.insert("/incoming/first.txt".into(), b"original first".to_vec());
        remote.insert("/incoming/second.txt".into(), b"original second".to_vec());
    }
    let dispatch = |app: &App, action| {
        assert!(
            app.backend
                .request(Command::Sftp { id, action })
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .is_ok()
        );
    };
    dispatch(
        &app,
        RemoteAction::Upload {
            local: first.clone(),
            remote: "/incoming/shared-fresh.txt".into(),
            resume: None,
            batch: Default::default(),
        },
    );
    pump(&mut app, &|_| {
        files
            .lock()
            .unwrap()
            .get("/incoming/shared-fresh.txt")
            .is_some_and(|data| data == b"new first")
    });
    let batch = ConflictBatch::default();
    dispatch(
        &app,
        RemoteAction::UploadTree {
            local: first.clone(),
            remote: "/incoming/first.txt".into(),
            batch: batch.clone(),
        },
    );
    dispatch(
        &app,
        RemoteAction::Upload {
            local: second.clone(),
            remote: "/incoming/second.txt".into(),
            resume: None,
            batch,
        },
    );
    pump(&mut app, &|app| {
        matches!(app.dialog, Some(Dialog::TransferConflict { .. }))
    });
    assert_eq!(
        files.lock().unwrap()["/incoming/first.txt"],
        b"original first"
    );
    assert_eq!(
        files.lock().unwrap()["/incoming/second.txt"],
        b"original second"
    );
    let Some(Dialog::TransferConflict { mut conflict, .. }) = app.dialog.take() else {
        panic!("Expected conflict")
    };
    assert!(conflict.can_replace);
    conflict.decide(ConflictChoice::Replace, true);
    pump(&mut app, &|_| {
        let remote = files.lock().unwrap();
        remote
            .get("/incoming/first.txt")
            .is_some_and(|data| data == b"new first")
            && remote
                .get("/incoming/second.txt")
                .is_some_and(|data| data == b"new second")
    });
    assert!(app.dialog.is_none());
    pump(&mut app, &|app| {
        app.transfers.values().all(|row| row.expires.is_some())
    });
    assert!(
        files
            .lock()
            .unwrap()
            .keys()
            .all(|path| !path.contains(".rterm-replaced-"))
    );

    // A new batch prompts again; skip must leave the original untouched.
    std::fs::write(&first, b"must not replace").unwrap();
    dispatch(
        &app,
        RemoteAction::UploadTree {
            local: first.clone(),
            remote: "/incoming/first.txt".into(),
            batch: Default::default(),
        },
    );
    pump(&mut app, &|app| {
        matches!(app.dialog, Some(Dialog::TransferConflict { .. }))
    });
    let Some(Dialog::TransferConflict { mut conflict, .. }) = app.dialog.take() else {
        panic!("Expected conflict")
    };
    conflict.decide(ConflictChoice::Skip, false);
    pump(&mut app, &|app| {
        app.transfers
            .values()
            .any(|row| row.text == "Skipped first.txt")
    });
    assert_eq!(files.lock().unwrap()["/incoming/first.txt"], b"new first");

    // Keep both generates a distinct filename without replacing the original.
    dispatch(
        &app,
        RemoteAction::UploadTree {
            local: first.clone(),
            remote: "/incoming/first.txt".into(),
            batch: Default::default(),
        },
    );
    pump(&mut app, &|app| {
        matches!(app.dialog, Some(Dialog::TransferConflict { .. }))
    });
    let Some(Dialog::TransferConflict { mut conflict, .. }) = app.dialog.take() else {
        panic!("Expected conflict")
    };
    conflict.decide(ConflictChoice::KeepBoth, false);
    pump(&mut app, &|_| {
        files
            .lock()
            .unwrap()
            .get("/incoming/first (1).txt")
            .is_some_and(|data| data == b"must not replace")
    });
    assert_eq!(files.lock().unwrap()["/incoming/first.txt"], b"new first");

    // Existing local downloads also require an explicit decision.
    std::fs::write(&download, b"old local data").unwrap();
    dispatch(
        &app,
        RemoteAction::Download {
            remote: "/remote.txt".into(),
            local: download.clone(),
            directory: false,
            options: crate::backend::DownloadOptions {
                parallel: 1,
                max_bytes_per_second: 0,
                timestamps: false,
            },
            resume: None,
            batch: Default::default(),
        },
    );
    pump(&mut app, &|app| {
        matches!(app.dialog, Some(Dialog::TransferConflict { .. }))
    });
    assert_eq!(std::fs::read(&download).unwrap(), b"old local data");
    let Some(Dialog::TransferConflict { mut conflict, .. }) = app.dialog.take() else {
        panic!("Expected conflict")
    };
    conflict.decide(ConflictChoice::Replace, false);
    pump(&mut app, &|_| {
        std::fs::read(&download).is_ok_and(|data| data == b"download fixture")
    });
    pump(&mut app, &|app| {
        app.transfers.values().all(|row| row.expires.is_some())
    });
    assert!(app.transfers.values().all(|row| !row.error));
    app.new_local();
    assert!(!app.file_workspace_active());
    app.activate(tab);
    assert!(app.file_workspace_active());
    assert_eq!(app.active_ssh(), Some(id));
    app.close_tab(tab);
    assert_eq!(app.terminals.len(), 1);
    assert!(!app.file_workspace_active());
    assert_eq!(shells.load(Ordering::Relaxed), 0);
}
