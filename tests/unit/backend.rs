use super::*;
use crate::egui_test::TestContextExt;

#[tokio::test(flavor = "current_thread")]
async fn full_output_queue_yields_to_other_tasks() {
    let context = egui::Context::default();
    let (backend, mut receive) = Backend::new(&context);
    for id in 0..256 {
        backend.events.send_async(BackendEvent::Closed(id)).await;
    }
    let events = backend.events.clone();
    let producer = tokio::spawn(async move {
        events.send_async(BackendEvent::Closed(256)).await;
    });
    tokio::task::yield_now().await;
    assert!(!producer.is_finished());
    for expected in 0..=256 {
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), receive.recv())
            .await
            .unwrap();
        assert!(matches!(event, Some(BackendEvent::Closed(id)) if id == expected));
    }
    producer.await.unwrap();
}

#[test]
fn ssh_input_waits_for_shell_writer_and_preserves_order() {
    use crate::terminal::{Kind, Terminal, TerminalOptions};
    let context = egui::Context::default();
    let (backend, _) = Backend::new(&context);
    let options = TerminalOptions {
        font: 13.0,
        cursor: Default::default(),
        scrollback: 100,
        copy_on_select: false,
        line_numbers: true,
        free_type: false,
    };
    let mut terminal = Terminal::new(1, "SSH startup".into(), Kind::Ssh, &options);
    let id = 124;
    terminal.endpoint = Some(id);
    assert!(terminal.starting());
    // Authentication succeeded, but the writer has not been installed.
    assert!(terminal.send(&backend, b"nlo".to_vec()));
    terminal.service(&backend, &context, false);
    assert!(terminal.send(&backend, b"ad\r".to_vec()));
    let (sender, mut receiver) = async_mpsc::channel(1);
    backend.writers.lock().unwrap().insert(id, sender);
    terminal.service(&backend, &context, false);
    assert!(
        receiver.try_recv().is_err(),
        "wait for the shell-ready reply"
    );
    terminal.ssh_ready(&backend, terminal.size());
    assert!(!terminal.starting());
    assert!(
        matches!(receiver.try_recv(), Ok(TransportMessage::Data(bytes)) if bytes == b"nload\r")
    );
    terminal.service(&backend, &context, false);
    assert!(
        receiver.try_recv().is_err(),
        "do not duplicate buffered input"
    );
    assert!(terminal.send(&backend, b"q".to_vec()));
    assert!(matches!(receiver.try_recv(), Ok(TransportMessage::Data(bytes)) if bytes == b"q"));
    drop(receiver);
    assert!(!terminal.send(&backend, b"x".to_vec()));
}

#[test]
fn fullscreen_viewport_fits_and_retries_remote_resize() {
    use crate::terminal::{Kind, Terminal, TerminalOptions};
    let context = egui::Context::default();
    crate::theme::install_fonts(&context);
    let (backend, _) = Backend::new(&context);
    let (sender, mut receiver) = async_mpsc::channel(1);
    let id = 123;
    backend.writers.lock().unwrap().insert(id, sender.clone());
    let options = TerminalOptions {
        font: 28.0,
        cursor: Default::default(),
        scrollback: 100,
        copy_on_select: false,
        line_numbers: true,
        free_type: false,
    };
    let mut terminal = Terminal::new(1, "Resize".into(), Kind::Ssh, &options);
    terminal.endpoint = Some(id);
    let highlighter = crate::highlight::Highlighter::new(true, "");
    let render = |terminal: &mut Terminal, width: f32, height: f32, time: f64| {
        let _ = context.run_test_pass(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, height),
                )),
                time: Some(time),
                ..Default::default()
            },
            |ctx| {
                ctx.root_ui(|root| {
                    egui::CentralPanel::default().show(root, |ui| {
                        terminal.show(
                            ui,
                            ui.max_rect(),
                            &backend,
                            &options,
                            &highlighter,
                            false,
                            false,
                            false,
                            None,
                            None,
                        );
                    })
                });
            },
        );
        terminal.size()
    };
    let original = render(&mut terminal, 780.0, 730.0, 0.0);
    assert!(original.cols < 80 && original.rows < 24);
    terminal.ssh_ready(&backend, original);
    assert!(receiver.try_recv().is_err());
    // Simulate input occupying the only transport queue slot when nload enters
    // the alternate screen, followed by a window resize before it drains.
    assert!(sender.try_send(TransportMessage::Data(vec![b'x'])).is_ok());
    terminal.receive(b"\x1b[?1049h");
    let fullscreen = render(&mut terminal, 780.0, 730.0, 0.1);
    assert!(fullscreen.cols >= 80 && fullscreen.rows >= 24);
    let resized = render(&mut terminal, 610.0, 450.0, 0.2);
    assert!(resized.cols >= 80 && resized.rows >= 24);
    assert_ne!(fullscreen, resized);
    assert!(matches!(receiver.try_recv(), Ok(TransportMessage::Data(_))));
    terminal.service(&backend, &context, false);
    assert!(matches!(receiver.try_recv(), Ok(TransportMessage::Resize(size)) if size == resized));
    terminal.service(&backend, &context, false);
    assert!(receiver.try_recv().is_err());
    // A split ncurses redraw must not shrink the PTY back to the shell grid.
    terminal.receive(b"\x1b[?1049l");
    assert_eq!(render(&mut terminal, 610.0, 450.0, 0.35), resized);
    assert!(receiver.try_recv().is_err());
    terminal.receive(b"\x1b[?1049h");
    assert_eq!(render(&mut terminal, 610.0, 450.0, 0.55), resized);
    assert!(receiver.try_recv().is_err());
    // Once the app is really done, restore the original shell geometry.
    terminal.receive(b"\x1b[?1049l");
    assert_eq!(render(&mut terminal, 610.0, 450.0, 0.65), resized);
    let restored = render(&mut terminal, 780.0, 730.0, 1.1);
    assert_eq!(restored, original);
    assert!(matches!(receiver.try_recv(), Ok(TransportMessage::Resize(size)) if size == original));
}
