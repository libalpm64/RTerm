use super::*;
use std::time::Instant;

#[test]
fn terminal_keeps_focus_for_tab_and_repeating_arrows() {
    let context = egui::Context::default();
    let (backend, _) = Backend::new(&context);
    let options = TerminalOptions {
        font: 13.0,
        cursor: CursorStyle::default(),
        scrollback: 100,
        copy_on_select: false,
        line_numbers: false,
    };
    let mut term = Terminal::new(1, "Test".into(), Kind::Local, &options);
    let (input, receive) = mpsc::sync_channel(128);
    let (_result, startup) = mpsc::channel();
    term.startup = Some(LocalStartup {
        result: startup,
        input,
        size: term.size,
    });
    term.endpoint = Some(1);
    let highlighter = Highlighter::new(false, "");
    let render = |term: &mut Terminal, events: Vec<egui::Event>, focus: bool| {
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = ui.button("Above terminal");
                    let id = ui.id().with(("terminal", term.tab));
                    if focus {
                        ui.memory_mut(|m| m.request_focus(id));
                    }
                    let area = Rect::from_min_size(pos2(0.0, 80.0), vec2(600.0, 400.0));
                    term.show(ui, area, &backend, &options, &highlighter, true, None, None);
                    let _ = ui.interact(
                        Rect::from_min_size(pos2(0.0, 490.0), vec2(100.0, 24.0)),
                        ui.id().with("below"),
                        Sense::click(),
                    );
                });
            },
        );
    };
    render(&mut term, vec![], true);
    render(&mut term, vec![], false);
    let key = |key, repeat, shift| egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat,
        modifiers: egui::Modifiers {
            shift,
            ..Default::default()
        },
    };
    for (event, expected) in [
        (key(Key::Tab, false, false), &b"\t"[..]),
        (key(Key::Tab, false, true), &b"\x1b[Z"[..]),
        (key(Key::ArrowUp, false, false), &b"\x1b[A"[..]),
        (key(Key::ArrowUp, true, false), &b"\x1b[A"[..]),
        (key(Key::ArrowUp, true, false), &b"\x1b[A"[..]),
        (key(Key::ArrowDown, true, false), &b"\x1b[B"[..]),
        (key(Key::ArrowLeft, false, false), &b"\x1b[D"[..]),
        (key(Key::Escape, false, false), &b"\x1b"[..]),
    ] {
        render(&mut term, vec![event], false);
        assert_eq!(receive.try_recv().unwrap_or_default(), expected);
    }
}

#[test]
fn input_bursts_are_batched_and_retried_in_order() {
    let context = egui::Context::default();
    let (backend, _) = Backend::new(&context);
    let options = TerminalOptions {
        font: 13.0,
        cursor: CursorStyle::default(),
        scrollback: 100,
        copy_on_select: false,
        line_numbers: false,
    };
    let mut term = Terminal::new(1, "Test".into(), Kind::Local, &options);
    let (input, receive) = mpsc::sync_channel(1);
    let (_result, startup) = mpsc::channel();
    input.send(b"existing".to_vec()).unwrap();
    term.startup = Some(LocalStartup {
        result: startup,
        input,
        size: term.size,
    });
    term.endpoint = Some(1);
    let highlighter = Highlighter::new(false, "");
    let events = (0..500).map(|_| egui::Event::Text("x".into())).collect();
    let _ = context.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.memory_mut(|m| m.request_focus(ui.id().with(("terminal", term.tab))));
                let (_, _, error) = term.show(
                    ui,
                    ui.max_rect(),
                    &backend,
                    &options,
                    &highlighter,
                    true,
                    None,
                    None,
                );
                assert!(error.is_none());
            });
        },
    );
    assert_eq!(&*term.pending_input.borrow(), &vec![b'x'; 500]);
    assert!(term.send(&backend, b"tail\r".to_vec()));
    assert_eq!(receive.try_recv().unwrap(), b"existing");
    term.service(&backend, &context, false);
    let mut expected = vec![b'x'; 500];
    expected.extend(b"tail\r");
    assert_eq!(receive.try_recv().unwrap(), expected);
    assert!(term.pending_input.borrow().is_empty());
}

#[test]
fn ascii_runs_match_terminal_cell_pitch() {
    let context = egui::Context::default();
    crate::theme::install_fonts(&context);
    let ascii: String = (b' '..=b'~').map(char::from).collect();
    for scale in [1.0, 1.25, 1.5, 2.0] {
        context.set_pixels_per_point(scale);
        let _ = context.run(egui::RawInput::default(), |ctx| {
            let pixels_per_point = ctx.pixels_per_point();
            ctx.fonts_mut(|fonts| {
                for size in [10.0, 13.0, 18.0, 26.0] {
                    let font = FontId::monospace(size);
                    let pitch = fonts.glyph_width(&font, 'M');
                    let galley = fonts.layout_no_wrap(ascii.clone(), font, Color32::WHITE);
                    assert_eq!(galley.rows.len(), 1);
                    for (column, glyph) in galley.rows[0].glyphs.iter().enumerate() {
                        let error = (glyph.pos.x - column as f32 * pitch).abs() * pixels_per_point;
                        assert!(error <= 1.1, "ASCII pitch error {error} at column {column}, font {size}, scale {scale}");
                    }
                }
            });
        });
    }
}

#[test]
#[ignore = "manual scrollback CPU profile"]
fn scrollback_frame_profile() {
    let context = egui::Context::default();
    crate::theme::install_fonts(&context);
    crate::theme::Theme::dark().apply(&context);
    let (backend, _) = Backend::new(&context);
    let options = TerminalOptions {
        font: 13.0,
        cursor: CursorStyle::default(),
        scrollback: 100_000,
        copy_on_select: false,
        line_numbers: true,
    };
    let mut terminal = Terminal::new(1, "Profile".into(), Kind::Ssh, &options);
    let render = |terminal: &mut Terminal, highlighter: &Highlighter, scroll: bool| {
        let start = Instant::now();
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1450.0, 1000.0))),
                events: if scroll {
                    vec![
                        egui::Event::PointerMoved(pos2(600.0, 400.0)),
                        egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Point,
                            delta: vec2(0.0, 120.0),
                            modifiers: egui::Modifiers::NONE,
                        },
                    ]
                } else {
                    vec![]
                },
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    terminal.show(
                        ui,
                        ui.max_rect(),
                        &backend,
                        &options,
                        highlighter,
                        true,
                        None,
                        None,
                    );
                });
            },
        );
        let shapes = output.shapes.len();
        let _ = context.tessellate(output.shapes, output.pixels_per_point);
        (start.elapsed().as_secs_f64() * 1000.0, shapes)
    };
    render(&mut terminal, &Highlighter::new(true, ""), false);
    let line = b"2026-11-25 12:34:56 INFO ssh connection accepted from 192.0.2.1 /var/log/example.log request completed successfully\r\n";
    for _ in 0..101_000 {
        terminal.receive(line);
    }
    for (mode, highlighter) in [
        ("enabled", Highlighter::new(true, "")),
        ("disabled", Highlighter::new(false, "")),
    ] {
        terminal.grid.scroll_display(Scroll::Bottom);
        render(&mut terminal, &highlighter, false);
        let mut samples = Vec::with_capacity(60);
        let mut shapes = 0;
        for frame in 0..70 {
            let (ms, count) = render(&mut terminal, &highlighter, true);
            if frame >= 10 {
                samples.push(ms);
                shapes += count;
            }
        }
        assert!(terminal.grid.grid().display_offset() > 0);
        samples.sort_by(f64::total_cmp);
        println!(
            "SCROLL_PROFILE mode={mode} median_ms={:.3} p95_ms={:.3} avg_shapes={} history={} offset={}",
            samples[samples.len() / 2],
            samples[samples.len() * 95 / 100],
            shapes / samples.len(),
            terminal.grid.grid().history_size(),
            terminal.grid.grid().display_offset()
        );
    }
}

#[test]
fn zoom_preview_scales_text_without_resizing_grid() {
    let context = egui::Context::default();
    crate::theme::install_fonts(&context);
    let (backend, _) = Backend::new(&context);
    let mut options = TerminalOptions {
        font: 13.0,
        cursor: Default::default(),
        scrollback: 1000,
        copy_on_select: false,
        line_numbers: true,
    };
    let highlighter = Highlighter::new(false, "");
    let mut terminal = Terminal::new(1, "Preview".into(), Kind::Local, &options);
    let render = |terminal: &mut Terminal, options: &TerminalOptions, preview: Option<f32>| {
        context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(760.0, 680.0))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    terminal.show(
                        ui,
                        ui.max_rect(),
                        &backend,
                        options,
                        &highlighter,
                        false,
                        preview,
                        None,
                    );
                });
            },
        )
    };
    render(&mut terminal, &options, None);
    terminal.receive(b"RTZ_MARKER\r\nPS> ");
    let original_size = terminal.size;
    let original_cursor = terminal.grid.grid().cursor.point;
    let original_history = terminal.grid.grid().history_size();
    for font in [14.0, 18.0, 28.0, 8.0, 23.0, 13.0] {
        let frame = render(&mut terminal, &options, Some(font));
        assert!(frame.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "RTZ_MARKER" && text.galley.job.sections[0].format.font_id.size == font)
        }), "preview must paint the requested font on this frame: {font}");
        assert_eq!(terminal.size, original_size);
        assert_eq!(terminal.grid.grid().cursor.point, original_cursor);
        assert_eq!(terminal.grid.grid().history_size(), original_history);
    }
    options.font = 23.0;
    render(&mut terminal, &options, None);
    assert_ne!(terminal.size, original_size);
    assert_eq!(terminal.grid.grid().cursor.point, original_cursor);
    assert_eq!(terminal.grid.grid().history_size(), original_history);
}

#[test]
fn resize_round_trip_preserves_shell_lines() {
    let context = egui::Context::default();
    crate::theme::install_fonts(&context);
    let (backend, _) = Backend::new(&context);
    let mut options = TerminalOptions {
        font: 13.0,
        cursor: Default::default(),
        scrollback: 1000,
        copy_on_select: false,
        line_numbers: true,
    };
    let highlighter = Highlighter::new(false, "");
    let mut terminal = Terminal::new(1, "Resize".into(), Kind::Local, &options);
    let render = |terminal: &mut Terminal, options: &TerminalOptions| {
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(760.0, 680.0))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    terminal.show(
                        ui,
                        ui.max_rect(),
                        &backend,
                        options,
                        &highlighter,
                        false,
                        None,
                        None,
                    );
                });
            },
        );
    };
    render(&mut terminal, &options);
    terminal.receive(b"\x1b[2J\x1b[3J\x1b[HLocal Terminal\r\nPS C:\\Users\\example> ");
    let initial =
        terminal.grid.grid().history_size() as i32 + terminal.grid.grid().cursor.point.line.0;
    for _ in 0..20 {
        for font in [32.0, 8.0, 24.0, 13.0] {
            options.font = font;
            render(&mut terminal, &options);
        }
    }
    let final_line =
        terminal.grid.grid().history_size() as i32 + terminal.grid.grid().cursor.point.line.0;
    assert_eq!(final_line, initial);
}

#[test]
fn native_pty_connects_shell_input_and_output() {
    let context = egui::Context::default();
    crate::theme::install_fonts(&context);
    let (backend, mut events) = Backend::new(&context);
    let shell = if cfg!(windows) { "cmd.exe" } else { "/bin/sh" };
    let options = TerminalOptions {
        font: 13.0,
        cursor: CursorStyle::default(),
        scrollback: 100,
        copy_on_select: false,
        line_numbers: false,
    };
    let mut terminal = Terminal::new(1, "Test".into(), Kind::Local, &options);
    terminal.queue_local(shell, false);
    assert!(terminal.endpoint.is_none());
    let _ = context.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(620.0, 300.0))),
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                terminal
                    .prepare_local(ui, ui.max_rect(), &options, &backend)
                    .unwrap();
            });
        },
    );
    assert_ne!(terminal.size, PtySize::default());
    assert_eq!(terminal.startup.as_ref().unwrap().size, terminal.size);
    assert_eq!(terminal.grid.grid().history_size(), 0);
    assert_eq!(terminal.grid.grid().cursor.point.line, Line(0));
    let deadline = Instant::now() + Duration::from_secs(12);
    while terminal.starting() {
        if let Some(result) = terminal.poll_local() {
            result.unwrap();
            break;
        }
        assert!(Instant::now() < deadline, "PTY startup timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
    let pty = terminal.local.as_ref().unwrap();
    let actual = pty.master.get_size().unwrap();
    assert_eq!(
        (actual.cols, actual.rows),
        (terminal.size.cols, terminal.size.rows)
    );
    pty.master
        .resize(PtySize {
            cols: 100,
            rows: 35,
            ..PtySize::default()
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut output = Vec::new();
    let mut command_sent = false;
    while Instant::now() < deadline {
        let event = match events.try_recv() {
            Ok(event) => event,
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            Err(_) => break,
        };
        match event {
            BackendEvent::Data { id, data } if terminal.endpoint == Some(id) => {
                output.extend_from_slice(&data);
                terminal.receive(&data);
                // On Windows, ConPTY waits for a cursor-position report before
                // launching its shell. Exercise the same parser/reply path as the UI.
                terminal.service(&backend, &context, false);
                if !command_sent && terminal.grid.grid().cursor.point.column.0 > 0 {
                    assert!(terminal.send(&backend, b"echo RTERM_PTY_TEST_RESPONSE\r".to_vec()));
                    command_sent = true;
                }
                // ConPTY may insert OSC title updates between the newline
                // and the response. Match the rendered line, not raw bytes
                // (nor the echoed command, which contains "echo ").
                if command_sent && terminal.visible_line_is("RTERM_PTY_TEST_RESPONSE") {
                    return;
                }
            }
            BackendEvent::Closed(id) if terminal.endpoint == Some(id) => break,
            _ => {}
        }
    }
    panic!(
        "PTY did not return the marker: {}",
        String::from_utf8_lossy(&output)
    );
}
