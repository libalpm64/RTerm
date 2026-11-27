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
        async fn auth_password(&mut self, _: &str, _: &str) -> Result<server::Auth, Self::Error> {
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
    let key = ssh_key::PrivateKey::random(&mut rand::rng(), ssh_key::Algorithm::Ed25519).unwrap();
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
                    ui.memory_mut(|m| m.request_focus(ui.id().with(("terminal", terminal.tab))));
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
