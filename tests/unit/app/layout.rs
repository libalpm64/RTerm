use super::*;

#[test]
fn expanded_file_workspace_hides_session_tabs_and_restores_them() {
    let context = egui::Context::default();
    let mut app = App::test_fixture(&context);
    let options = app.settings.terminal();
    let names = ["Fixture shell A", "Fixture shell B", "Fixture SFTP"];
    for (index, name) in names.iter().enumerate() {
        let kind = if index == 2 { Kind::Sftp } else { Kind::Local };
        app.terminals.push(Terminal::new(
            index as u64 + 1,
            (*name).into(),
            kind,
            &options,
        ));
    }
    app.activate(1);
    assert!(app.splits.split(1, 2, crate::app::splits::Direction::Right));
    let render = |app: &mut App, width: f32, events: Vec<egui::Event>| {
        context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 800.0))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| app.center(ui, ui.max_rect()));
            },
        )
    };
    let label_rect = |output: &egui::FullOutput, name: &str| {
        output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == name => Some(shape.clip_rect),
            _ => None,
        })
    };
    let check_tabs = |output: &egui::FullOutput, visible: bool| {
        for name in names {
            assert_eq!(label_rect(output, name).is_some(), visible, "{name}");
        }
    };

    check_tabs(&render(&mut app, 1200.0, vec![]), true);
    app.toggle_file_workspace();
    let expanded = render(&mut app, 1200.0, vec![]);
    check_tabs(&expanded, false);
    assert_eq!(label_rect(&expanded, "File Manager").unwrap().top(), 0.0);

    // Exercise the actual restore icon, not just the state toggle.
    let position = pos2(1186.0, 15.0);
    for pressed in [true, false] {
        render(
            &mut app,
            1200.0,
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    assert!(!app.explorer.expanded);
    check_tabs(&render(&mut app, 1200.0, vec![]), true);
    assert_eq!(app.active, Some(1));
    assert!(app.splits.contains(1) && app.splits.contains(2));

    app.activate(3);
    assert!(app.explorer.expanded);
    for width in [1200.0, 350.0] {
        let expanded = render(&mut app, width, vec![]);
        check_tabs(&expanded, false);
        assert_eq!(label_rect(&expanded, "File Manager").unwrap().top(), 0.0);
    }
    app.toggle_file_workspace();
    assert!(app.file_workspace_active());
    let compact = render(&mut app, 1200.0, vec![]);
    check_tabs(&compact, true);
    assert_eq!(label_rect(&compact, "File Manager").unwrap().top(), 23.0);
    assert_eq!(app.active, Some(3));
    assert_eq!(app.terminals.len(), 3);
    assert!(app.splits.contains(1) && app.splits.contains(2));
}

#[test]
fn folder_creation_button_works_in_fullscreen_and_docked_sftp() {
    let context = egui::Context::default();
    let mut app = App::test_fixture(&context);
    let mut terminal = Terminal::new(1, "Fixture SSH".into(), Kind::Ssh, &app.settings.terminal());
    terminal.endpoint = Some(42);
    app.terminals.push(terminal);
    app.active = Some(1);
    app.explorer
        .accept_initial_listing(42, "/fixture".into(), vec![]);
    app.explorer.remote_active = true;
    assert!(app.can_create_sftp_folder());
    for (expanded, sftp_only, pos) in [
        (true, false, pos2(1146.0, 15.0)),
        (true, false, pos2(1166.0, 43.0)),
        (false, false, pos2(166.0, 35.0)),
        (false, true, pos2(1146.0, 15.0)),
        (false, true, pos2(1166.0, 65.0)),
    ] {
        app.explorer.expanded = expanded;
        app.terminals[0].kind = if sftp_only { Kind::Sftp } else { Kind::Ssh };
        app.terminals[0].sftp_ready = sftp_only;
        assert!(app.can_create_sftp_folder());
        let render = |app: &mut App, events| {
            context.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show(ctx, |ui| {
                            if expanded || sftp_only {
                                app.explorer_workspace(ui, ui.max_rect());
                            } else {
                                app.explorer_panel(ui, ui.max_rect().with_max_x(240.0));
                            }
                        });
                },
            )
        };
        let initial = render(&mut app, vec![]);
        if expanded {
            assert!(initial.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Rect(rect) if rect.rect.left() == 598.0 && rect.rect.right() == 602.0
                    && rect.rect.top() == 30.0 && rect.fill == app.theme.bg)));
        }
        for pressed in [true, false] {
            render(
                &mut app,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        let Some(Dialog::NewSftpFolder {
            id,
            directory,
            name,
        }) = app.dialog.take()
        else {
            panic!("folder button must open the dialog, expanded={expanded}");
        };
        assert_eq!(id, 42);
        assert_eq!(directory, "/fixture");
        assert!(name.is_empty());
    }
}

#[test]
fn transfer_card_preserves_file_panes_and_queue_is_opt_in() {
    let context = egui::Context::default();
    let mut app = App::test_fixture(&context);
    app.explorer.expanded = true;
    app.explorer
        .accept_initial_listing(42, "/fixture".into(), vec![]);
    let render = |app: &mut App, events| {
        context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| app.center(ui, ui.max_rect()));
            },
        )
    };
    let footer_y = |out: &egui::FullOutput| {
        let positions: Vec<_> = out
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text().starts_with("Items:") => {
                    Some(text.pos.y)
                }
                _ => None,
            })
            .collect();
        assert_eq!(positions.len(), 2);
        assert_eq!(positions[0], positions[1]);
        positions[0]
    };
    let baseline = footer_y(&render(&mut app, vec![]));
    app.handle_event(BackendEvent::Progress {
        id: "42:upload:fixture".into(),
        text: "Uploading fixture.txt".into(),
        percent: 45.0,
        rate_bps: Some(2.5 * 1024.0 * 1024.0),
        finished: false,
        error: false,
    });
    render(&mut app, vec![]);
    let overlay = render(&mut app, vec![]);
    assert_eq!(footer_y(&overlay), baseline);
    assert!(overlay.shapes.iter().any(|shape| matches!(&shape.shape,
        egui::Shape::Text(text) if text.galley.text().contains("2.5 MiB/s") && text.galley.text().contains("Uploading fixture.txt")
    )));
    let click = |app: &mut App, position| {
        for pressed in [true, false] {
            render(
                app,
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    };
    // The bottom bar is visible even when the queue is closed.
    assert!(overlay.shapes.iter().any(|shape| matches!(&shape.shape,
        egui::Shape::Text(text) if text.galley.text() == "1 Pending"
    )));
    click(&mut app, pos2(55.0, 787.0));
    assert!(app.transfer_queue_open);
    let queue = render(&mut app, vec![]);
    let initial_footer = footer_y(&queue);
    assert!(initial_footer < baseline);
    assert!(queue.shapes.iter().any(|shape| matches!(&shape.shape,
        egui::Shape::Text(text) if text.galley.text() == "Transfer queue"
    )));

    let text_rect = |name: &str| {
        queue
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == name => {
                    Some(Rect::from_min_size(text.pos, text.galley.size()))
                }
                _ => None,
            })
            .unwrap()
    };
    let speed = text_rect("2.5 MiB/s");
    let filename = text_rect("Uploading fixture.txt");
    let progress = text_rect("45%");
    assert!(speed.left() > progress.right() + 12.0);
    assert!(filename.left() >= speed.right() + 10.0);

    // Drag the separator upward, then verify the queue's new height
    // survives both reopening and an idle render.
    let start = pos2(600.0, 774.0 - app.transfer_queue_height - 3.0);
    let end = start - vec2(0.0, 85.0);
    let initial_height = app.transfer_queue_height;
    render(
        &mut app,
        vec![
            egui::Event::PointerMoved(start),
            egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    render(&mut app, vec![egui::Event::PointerMoved(end)]);
    render(
        &mut app,
        vec![egui::Event::PointerButton {
            pos: end,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
    );
    assert!(
        app.transfer_queue_height > initial_height + 50.0,
        "queue height {}",
        app.transfer_queue_height
    );
    let resized_footer = footer_y(&render(&mut app, vec![]));
    assert!(resized_footer < initial_footer - 50.0);
    click(&mut app, pos2(55.0, 787.0));
    assert!(!app.transfer_queue_open);
    assert_eq!(footer_y(&render(&mut app, vec![])), baseline);
    click(&mut app, pos2(1126.0, 15.0));
    assert!(app.transfer_queue_open);
    assert_eq!(footer_y(&render(&mut app, vec![])), resized_footer);

    app.handle_event(BackendEvent::Progress {
        id: "42:upload:failed".into(),
        text: "Failed fixture".into(),
        percent: 0.0,
        rate_bps: None,
        finished: true,
        error: true,
    });
    let result = render(&mut app, vec![]);
    assert!(result.shapes.iter().any(|shape| matches!(&shape.shape,
        egui::Shape::Text(text) if text.galley.text() == "1 Failed"
    )));
    for (id, description, percent, error) in [
        ("complete", "Transferred successful fixture", 100.0, false),
        ("skip", "Skipped fixture", 0.0, false),
    ] {
        app.handle_event(BackendEvent::Progress {
            id: id.into(),
            text: description.into(),
            percent,
            rate_bps: None,
            finished: true,
            error,
        });
    }
    let text = |out: &egui::FullOutput, label: &str| {
        out.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(t) if t.galley.text() == label => {
                Some(Rect::from_min_size(t.pos, t.galley.size()).center())
            }
            _ => None,
        })
    };
    let all = render(&mut app, vec![]);
    assert!(text(&all, "Transferred successful fixture").is_some());
    assert_eq!(contrast_text(app.theme.green), Color32::BLACK);
    assert_eq!(contrast_text(app.theme.hover), Color32::WHITE);
    assert!(all.shapes.iter().any(|shape| matches!(&shape.shape,
        egui::Shape::Text(t) if t.galley.text() == "100%" && t.override_text_color == Some(Color32::BLACK)
    )));
    for (button, filter, visible) in [
        ("1 Failed", TransferFilter::Failed, "Failed fixture"),
        (
            "1 Completed",
            TransferFilter::Completed,
            "Transferred successful fixture",
        ),
        ("1 Skipped", TransferFilter::Skipped, "Skipped fixture"),
        (
            "1 Pending",
            TransferFilter::Pending,
            "Uploading fixture.txt",
        ),
    ] {
        let frame = render(&mut app, vec![]);
        click(&mut app, text(&frame, button).unwrap());
        assert_eq!(app.transfer_filter, filter);
        assert!(app.transfer_queue_open);
        let filtered = render(&mut app, vec![]);
        for name in [
            "Failed fixture",
            "Transferred successful fixture",
            "Skipped fixture",
            "Uploading fixture.txt",
        ] {
            assert_eq!(
                text(&filtered, name).is_some(),
                name == visible,
                "{button}: {name}"
            );
        }
    }
    let frame = render(&mut app, vec![]);
    click(&mut app, text(&frame, "4 All").unwrap());
    assert_eq!(app.transfer_filter, TransferFilter::All);
}

#[cfg(all(windows, feature = "benchmark"))]
#[test]
#[ignore = "requires Neovim on PATH; exercises a real local Windows PTY"]
fn native_neovim_accepts_arrows_escape_search_and_quit() {
    let context = egui::Context::default();
    let mut app = App::with_settings(&context, Settings::benchmark_defaults(), false);
    let mut terminal = Terminal::new(
        1,
        "Editor test".into(),
        Kind::Local,
        &app.settings.terminal(),
    );
    terminal.queue_local("cmd.exe", false);
    app.terminals.push(terminal);
    app.active = Some(1);
    let mut frame = eframe::Frame::_new_kittest();
    let mut render = |app: &mut App, events: Vec<egui::Event>| {
        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1440.0, 900.0))),
            events,
            ..Default::default()
        };
        eframe::App::raw_input_hook(app, &context, &mut raw);
        let _ = context.run(raw, |ctx| eframe::App::update(app, ctx, &mut frame));
    };
    fn wait_for(
        app: &mut App,
        render: &mut impl FnMut(&mut App, Vec<egui::Event>),
        label: &str,
        predicate: impl Fn(&Terminal) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            render(app, vec![]);
            if predicate(&app.terminals[0]) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "Timed out waiting for {label}: {:?}",
                app.terminals[0].status()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let key = |key, repeat| egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat,
        modifiers: egui::Modifiers::NONE,
    };
    let text = |value: &str| egui::Event::Text(value.into());
    wait_for(&mut app, &mut render, "cmd prompt", |term| {
        !term.starting() && term.visible_line_matches(|line| line.ends_with('>'))
    });
    render(
        &mut app,
        vec![
            text("nvim --clean -i NONE -n -c \"call setline(1, 'RTNAVabcdef')\" -c \"normal! 0\""),
            key(Key::Enter, false),
        ],
    );
    wait_for(&mut app, &mut render, "Neovim buffer", |term| {
        term.visible_line_is("RTNAVabcdef")
    });
    render(
        &mut app,
        vec![
            key(Key::ArrowRight, false),
            key(Key::ArrowRight, true),
            key(Key::ArrowRight, true),
            text("i"),
            text("X"),
        ],
    );
    wait_for(
        &mut app,
        &mut render,
        "three right movements and insert",
        |term| term.visible_line_is("RTNXAVabcdef"),
    );
    render(&mut app, vec![key(Key::Escape, false)]);
    wait_for(&mut app, &mut render, "leave insert mode", |term| {
        !term.visible_line_matches(|line| line.contains("-- INSERT --"))
    });
    render(&mut app, vec![text("/abcdef")]);
    wait_for(&mut app, &mut render, "search command", |term| {
        term.visible_line_is("/abcdef")
    });
    render(&mut app, vec![key(Key::Enter, false)]);
    // Search is accepted in normal mode, and command-line entry then quits.
    render(&mut app, vec![text(":q!"), key(Key::Enter, false)]);
    wait_for(&mut app, &mut render, "return to cmd", |term| {
        term.visible_line_matches(|line| line.ends_with('>'))
            && !term.visible_line_is("RTNXAVabcdef")
    });
    render(
        &mut app,
        vec![text("echo RTERM_EDITOR_RETURNED"), key(Key::Enter, false)],
    );
    wait_for(
        &mut app,
        &mut render,
        "shell command after editor exit",
        |term| term.visible_line_is("RTERM_EDITOR_RETURNED"),
    );
    app.terminals[0].mark_closed();
}

#[test]
fn wallpaper_is_exclusive_to_main_terminal_tabs() {
    let context = egui::Context::default();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wallpaper.png");
    image::RgbaImage::from_pixel(64, 32, image::Rgba([32, 128, 255, 255]))
        .save(&path)
        .unwrap();
    let mut settings = Settings::benchmark_defaults();
    settings.set("background_path", path.to_string_lossy().into_owned());
    settings.set("background_enabled", "1".into());
    let mut app = App::with_settings(&context, settings, false);
    assert!(app.wallpaper.is_some());
    let options = app.settings.terminal();
    app.terminals
        .push(Terminal::new(1, "Main".into(), Kind::Local, &options));
    app.active = Some(1);
    app.bottom_shell = Some(Terminal::new(2, "Shell".into(), Kind::Local, &options));
    app.bottom_open = true;
    let mut split = 0.0;
    let output = context.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))),
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let bounds = ui.max_rect();
                split = bounds.bottom() - 200.0;
                app.center(ui, bounds.with_max_y(split));
                app.bottom_panel(ui, bounds.with_min_y(split));
            });
        },
    );
    let primitives = context.tessellate(output.shapes, output.pixels_per_point);
    let images: Vec<_> = primitives
        .iter()
        .filter_map(|primitive| match &primitive.primitive {
            egui::epaint::Primitive::Mesh(mesh)
                if mesh.texture_id != egui::TextureId::default() =>
            {
                Some(mesh.calc_bounds())
            }
            _ => None,
        })
        .collect();
    assert!(!images.is_empty(), "main terminal must retain wallpaper");
    assert!(
        images.iter().all(|bounds| bounds.bottom() <= split),
        "wallpaper must not enter bottom shell"
    );
}

#[test]
fn remote_connection_status_does_not_enter_terminal_grid() {
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    let context = egui::Context::default();
    let mut app = App::with_settings(&context, Settings::benchmark_defaults(), false);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    app.connect(
        Profile {
            name: "Test remote".into(),
            connection: Connection::Telnet(crate::models::TelnetConfig {
                host: "127.0.0.1".into(),
                port: listener.local_addr().unwrap().port(),
            }),
        },
        None,
    );
    let render = |app: &mut App, font: f32| {
        app.settings.set("font_size", font.to_string());
        context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(420.0, 450.0))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    app.center(ui, ui.max_rect());
                });
            },
        )
    };
    for font in [40.0, 12.0, 40.0] {
        let output = render(&mut app, font);
        assert_eq!(app.terminals[0].status().1, "Ln 1, Col 1");
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == "Connecting…")));
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "local test connection timed out");
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("{error}"),
        }
    };
    // Accepted sockets can inherit O_NONBLOCK on BSD/macOS. The
    // listener uses nonblocking accept, but this fixture uses bounded
    // blocking reads and writes on the connected stream.
    stream.set_nonblocking(false).unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(b"READY> ").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.terminals[0].status().1 != "Ln 1, Col 8" {
        app.poll(&context);
        assert!(
            Instant::now() < deadline,
            "first remote prompt missing: {:?}",
            app.terminals[0].status()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let output = render(&mut app, 40.0);
    assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text.contains("READY>"))));
    assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text.contains("Connecting"))));

    // Recreate the focus race: requesting focus again resets egui's
    // EventFilter. Exercise the actual eframe raw hook, not just a steady
    // terminal widget, and check bytes at the other end of the socket.
    let focus = context.memory(|m| m.focused()).expect("terminal focused");
    assert_eq!(context.data(|data| data.get_temp::<u64>(focus)), app.active);
    let key = |key, repeat| egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat,
        modifiers: egui::Modifiers::NONE,
    };
    let mut frame = eframe::Frame::_new_kittest();
    for (alternate, expected) in [
        (false, b"\x1b[C\x1b[C\x1b[C\x1b:q\r\n".as_slice()),
        (true, b"\x1bOC\x1bOC\x1bOC\x1b:q\r\n".as_slice()),
    ] {
        if alternate {
            app.terminals[0].receive(b"\x1b[?1049h\x1b[?1h");
        }
        context.memory_mut(|m| m.request_focus(focus));
        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))),
            events: vec![
                key(Key::ArrowRight, false),
                key(Key::ArrowRight, true),
                key(Key::ArrowRight, true),
                key(Key::Escape, false),
                egui::Event::Text(":".into()),
                egui::Event::Text("q".into()),
                key(Key::Enter, false),
            ],
            ..Default::default()
        };
        eframe::App::raw_input_hook(&mut app, &context, &mut raw);
        assert_eq!(app.terminal_navigation.len(), 4);
        let _ = context.run(raw, |ctx| eframe::App::update(&mut app, ctx, &mut frame));
        let mut got = vec![0; expected.len()];
        stream.read_exact(&mut got).unwrap();
        assert_eq!(got, expected);
        assert_eq!(context.memory(|m| m.focused()), Some(focus));
    }
}

#[test]
fn fullscreen_keyboard_zoom_uses_visible_font_and_keeps_ui_scale() {
    let context = egui::Context::default();
    let mut app = App::with_settings(&context, Settings::benchmark_defaults(), false);
    app.settings.set("font_size", "28".into());
    let options = app.settings.terminal();
    app.terminals
        .push(Terminal::new(1, "nload".into(), Kind::Ssh, &options));
    app.active = Some(1);
    // Mark the fixture as connected so prepare_remote does not replace
    // the grid while measuring the initial (still disconnected) pane.
    app.terminals[0].endpoint = Some(123);
    app.terminals[0].receive(b"\x1b[?1049h\x1b[HRTZOOM");
    let mut frame = eframe::Frame::_new_kittest();
    let modifiers = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    let key = |key, repeat| egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat,
        modifiers,
    };
    let mut render = |app: &mut App, events: Vec<egui::Event>, time: f64| {
        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))),
            time: Some(time),
            modifiers,
            events,
            ..Default::default()
        };
        eframe::App::raw_input_hook(app, &context, &mut raw);
        context.run(raw, |ctx| eframe::App::update(app, ctx, &mut frame))
    };
    render(&mut app, vec![], 0.0);
    let fitted = app.terminals[0].zoom_base(options.font);
    assert!(
        fitted < options.font && fitted > 10.0,
        "fitted font: {fitted}"
    );
    let original_size = app.terminals[0].size();
    let ui_scale = context.zoom_factor();
    let out = render(
        &mut app,
        vec![key(Key::Minus, false), key(Key::Minus, true)],
        0.1,
    );
    assert_eq!(app.settings.terminal().font, fitted - 2.0);
    assert!(app.terminals[0].size().cols > original_size.cols);
    assert!(out.shapes.iter().any(|shape| matches!(&shape.shape,
        egui::Shape::Text(text) if text.galley.text() == "RTZOOM"
            && text.galley.job.sections[0].format.font_id.size == fitted - 2.0
    )));
    assert_eq!(context.zoom_factor(), ui_scale);
    // Zoom above the original automatic fit, without getting capped.
    render(&mut app, vec![key(Key::Equals, false); 3], 0.2);
    assert_eq!(app.settings.terminal().font, fitted + 1.0);
    assert!(app.terminals[0].size().cols < original_size.cols);
    // A curses redraw can briefly return to the primary screen.
    let size = app.terminals[0].size();
    app.terminals[0].receive(b"\x1b[?1049l");
    render(&mut app, vec![], 0.3);
    assert_eq!(app.terminals[0].size(), size);
    app.terminals[0].receive(b"\x1b[?1049h");
    render(&mut app, vec![key(Key::Minus, false)], 0.4);
    assert_eq!(app.settings.terminal().font, fitted);
    render(&mut app, vec![key(Key::Num0, false)], 0.5);
    assert_eq!(app.settings.terminal().font, 13.0);
    assert_eq!(context.zoom_factor(), ui_scale);
}

#[test]
fn fullscreen_wheel_zoom_and_bottom_keyboard_zoom_are_independent() {
    let context = egui::Context::default();
    let mut app = App::with_settings(&context, Settings::benchmark_defaults(), false);
    app.settings.set("font_size", "28".into());
    let options = app.settings.terminal();
    let mut main = Terminal::new(1, "nload".into(), Kind::Ssh, &options);
    main.endpoint = Some(123);
    main.receive(b"\x1b[?1049h\x1b[HRTMAIN");
    app.terminals.push(main);
    app.active = Some(1);
    app.bottom_shell = Some(Terminal::new(
        2,
        "Shell".into(),
        Kind::Local,
        &app.bottom_options(),
    ));
    app.bottom_open = true;
    let mut frame = eframe::Frame::_new_kittest();
    let modifiers = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    let key = |key| egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat: false,
        modifiers,
    };
    let wheel = |position, delta| {
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: vec2(0.0, delta),
                modifiers,
            },
        ]
    };
    let mut render = |app: &mut App, events: Vec<egui::Event>, time: f64| {
        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))),
            time: Some(time),
            modifiers,
            events,
            ..Default::default()
        };
        eframe::App::raw_input_hook(app, &context, &mut raw);
        context.run(raw, |ctx| eframe::App::update(app, ctx, &mut frame))
    };
    render(&mut app, vec![], 0.0);
    let fitted = app.zoom_base(Some(1));
    assert!(fitted < 28.0 && fitted > 8.0);
    let bottom_size = app.bottom_shell.as_ref().unwrap().size();
    let main_size = app.terminals[0].size();
    let out = render(&mut app, wheel(pos2(600.0, 300.0), -1.0), 0.1);
    assert_eq!(app.zoom_preview(1), Some(fitted - 1.0));
    assert_eq!(app.zoom_preview(2), None);
    assert_eq!(app.terminals[0].size(), main_size);
    assert!(out.shapes.iter().any(|shape| matches!(&shape.shape,
        egui::Shape::Text(text) if text.galley.text() == "RTMAIN"
            && text.galley.job.sections[0].format.font_id.size == fitted - 1.0
    )));
    render(&mut app, vec![], 0.2);
    assert_eq!(app.settings.terminal().font, fitted - 1.0);
    assert!(app.terminals[0].size().cols > main_size.cols);
    assert_eq!(app.bottom_shell.as_ref().unwrap().size(), bottom_size);
    let main_size = app.terminals[0].size();
    // Keyboard zoom follows the currently focused terminal.
    app.bottom_request_focus = true;
    render(&mut app, vec![], 0.3);
    assert!(app.bottom_focus);
    let bottom_font = app.bottom_font;
    render(&mut app, vec![key(Key::Minus)], 0.4);
    assert_eq!(app.bottom_font, bottom_font - 1.0);
    assert_eq!(app.terminals[0].size(), main_size);
    let bottom_size = app.bottom_shell.as_ref().unwrap().size();
    render(&mut app, wheel(pos2(600.0, 650.0), -2.0), 0.5);
    assert_eq!(app.zoom_preview(2), Some(bottom_font - 3.0));
    assert_eq!(app.zoom_preview(1), None);
    assert_eq!(app.bottom_shell.as_ref().unwrap().size(), bottom_size);
    render(&mut app, vec![key(Key::Equals)], 0.51);
    assert_eq!(app.bottom_font, bottom_font - 2.0);
    assert_eq!(app.terminals[0].size(), main_size);
    render(&mut app, vec![key(Key::Num0)], 0.6);
    assert_eq!(app.bottom_font, 13.0);
    assert_eq!(app.settings.get("bottom_font_size"), "13");
    assert_eq!(app.settings.terminal().font, fitted - 1.0);
    assert_eq!(app.terminals[0].size(), main_size);
}

#[test]
fn ctrl_wheel_zooms_only_terminal_and_consumes_scroll() {
    let context = egui::Context::default();
    let mut app = App::with_settings(&context, Settings::benchmark_defaults(), false);
    let options = app.settings.terminal();
    app.terminals
        .push(Terminal::new(1, "Main".into(), Kind::Local, &options));
    app.active = Some(1);
    app.bottom_shell = Some(Terminal::new(2, "Shell".into(), Kind::Local, &options));
    app.bottom_open = true;
    let mut frame = eframe::Frame::_new_kittest();
    let render = |app: &mut App,
                  frame: &mut eframe::Frame,
                  pos: egui::Pos2,
                  delta: f32,
                  ctrl: bool,
                  time: f64| {
        let modifiers = egui::Modifiers {
            ctrl,
            command: ctrl,
            ..Default::default()
        };
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))),
                time: Some(time),
                modifiers,
                events: vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Line,
                        delta: vec2(0.0, delta),
                        modifiers,
                    },
                ],
                ..Default::default()
            },
            |ctx| eframe::App::update(app, ctx, frame),
        );
        app.settings.terminal().font
    };
    let original = options.font;
    assert_eq!(
        render(&mut app, &mut frame, pos2(600.0, 300.0), 1.0, true, 0.0),
        original
    );
    assert_eq!(context.input(|input| input.raw_scroll_delta.y), 0.0);
    assert_eq!(
        render(&mut app, &mut frame, pos2(600.0, 300.0), 1.0, false, 0.01),
        original + 1.0
    );
    assert!(context.input(|input| input.raw_scroll_delta.y) > 0.0);
    assert_eq!(
        render(&mut app, &mut frame, pos2(100.0, 300.0), 1.0, true, 0.02),
        original + 1.0
    );
    assert_eq!(
        render(&mut app, &mut frame, pos2(600.0, 650.0), -1.0, true, 0.03),
        original + 1.0
    );
    assert_eq!(
        render(&mut app, &mut frame, pos2(600.0, 650.0), 0.0, true, 0.12),
        original + 1.0
    );
    assert_eq!(app.bottom_font, original - 1.0);
    let size = app.terminals[0].size();
    // Rapid changes which cancel out must never resize the terminal grid.
    for i in 0..20 {
        let delta = if i % 2 == 0 { 5.0 } else { -5.0 };
        assert_eq!(
            render(
                &mut app,
                &mut frame,
                pos2(600.0, 300.0),
                delta,
                true,
                0.13 + f64::from(i) * 0.01
            ),
            original + 1.0
        );
        assert_eq!(app.terminals[0].size(), size);
    }
    assert!(app.pending_wheel_zoom.is_none());
    assert_eq!(
        render(&mut app, &mut frame, pos2(600.0, 300.0), 100.0, true, 0.40),
        original + 1.0
    );
    assert_eq!(
        render(&mut app, &mut frame, pos2(600.0, 300.0), 0.0, true, 0.49),
        32.0
    );
    assert_eq!(
        render(&mut app, &mut frame, pos2(600.0, 300.0), -100.0, true, 0.50),
        32.0
    );
    assert_eq!(
        render(&mut app, &mut frame, pos2(600.0, 300.0), 0.0, false, 0.51),
        8.0
    );
}

#[test]
fn applying_settings_switches_all_supported_languages() {
    let context = egui::Context::default();
    let mut app = App::with_settings(&context, Settings::benchmark_defaults(), false);
    for &(code, _) in crate::i18n::CHOICES
        .iter()
        .filter(|(code, _)| *code != "auto")
    {
        app.settings.set("language", code.into());
        app.apply_settings(&context);
        let expected = if crate::i18n::SCRIPT_LOCALES.contains(&code)
            && crate::i18n::system_font(code).is_none()
        {
            "en"
        } else {
            code
        };
        assert_eq!(crate::i18n::current(&context), expected, "{code}");
        let _ = context.run(Default::default(), |ctx| {
            for key in [
                "Settings",
                "Apply",
                "Cancel",
                "Language",
                "Appearance",
                "Interface Scale",
                "Adjust the size of menus, panels, and text",
            ] {
                let label = crate::i18n::text(ctx, key);
                if expected != "en" {
                    assert_ne!(label, key, "{code}: {key}");
                }
                assert!(
                    ctx.fonts_mut(|fonts| {
                        fonts.has_glyphs(&app.theme.chrome_font(12.0), label)
                    }),
                    "{code}: {label}"
                );
            }
        });
    }
}

#[cfg(target_os = "macos")]
#[test]
fn interface_scale_applies_at_startup_and_when_settings_change() {
    let context = egui::Context::default();
    let mut settings = Settings::benchmark_defaults();
    settings.set("interface_scale", "125".into());
    let mut app = App::with_settings(&context, settings, false);
    let _ = context.run(Default::default(), |_| {});
    assert_eq!(context.zoom_factor(), 1.25);

    app.settings.set("interface_scale", "100".into());
    app.apply_settings(&context);
    let _ = context.run(Default::default(), |_| {});
    assert_eq!(context.zoom_factor(), 1.0);
}

#[test]
fn workspace_and_dialogs_have_unique_widget_ids() {
    let context = egui::Context::default();
    context.enable_accesskit();
    context.options_mut(|options| options.warn_on_id_clash = true);
    let mut app = App::with_settings(&context, Settings::benchmark_defaults(), false);
    let options = app.settings.terminal();
    app.terminals
        .push(Terminal::new(1, "Terminal 1".into(), Kind::Local, &options));
    app.terminals
        .push(Terminal::new(2, "Terminal 2".into(), Kind::Local, &options));
    app.active = Some(1);
    app.bottom_shell = Some(Terminal::new(3, "Shell".into(), Kind::Local, &options));
    app.bottom_open = true;
    let mut frame = eframe::Frame::_new_kittest();
    let check = |app: &mut App, frame: &mut eframe::Frame| {
        for _ in 0..2 {
            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))),
                    ..Default::default()
                },
                |ctx| eframe::App::update(app, ctx, frame),
            );
            for shape in output.shapes {
                if let egui::Shape::Text(text) = &shape.shape {
                    let text = text.galley.text();
                    assert!(
                        !text.contains("Second use of") && !text.contains("Double use of"),
                        "{text}"
                    );
                }
            }
        }
    };
    check(&mut app, &mut frame);
    app.keys.open = true;
    app.show_keys = true;
    check(&mut app, &mut frame);
    app.vault_keys.push(crate::models::SshKey {
        name: "Example key".into(),
        private_key: String::new(),
        public_key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA example".into(),
        key_type: "ed25519".into(),
    });
    check(&mut app, &mut frame);
    for page in 0..7 {
        let mut settings = app.settings.clone();
        settings.test_set_page(page);
        app.dialog = Some(Dialog::settings(&settings, None));
        check(&mut app, &mut frame);
    }
    app.dialog = Some(Dialog::About);
    check(&mut app, &mut frame);
}

#[test]
fn bottom_shell_keeps_its_session_and_restores_focus() {
    let context = egui::Context::default();
    let mut app = App::with_settings(&context, Settings::benchmark_defaults(), false);
    let options = app.settings.terminal();
    app.terminals
        .push(Terminal::new(1, "Main".into(), Kind::Local, &options));
    app.active = Some(1);
    app.bottom_shell = Some(Terminal::new(2, "Shell".into(), Kind::Local, &options));
    app.toggle_bottom_shell();
    assert!(app.bottom_open && app.bottom_focus && app.bottom_request_focus);
    assert_eq!(app.selected().unwrap().tab, 2);
    let mut frame = eframe::Frame::_new_kittest();
    let render = |app: &mut App, frame: &mut eframe::Frame| {
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))),
                ..Default::default()
            },
            |ctx| eframe::App::update(app, ctx, frame),
        );
    };
    render(&mut app, &mut frame);
    let shell_focus = context
        .memory(|memory| memory.focused())
        .expect("shell has focus");
    app.dialog = Some(Dialog::About);
    render(&mut app, &mut frame);
    render(&mut app, &mut frame);
    app.dialog = None;
    app.focus_terminal_after_dialog();
    assert!(app.bottom_request_focus && !app.focus_terminal);
    render(&mut app, &mut frame);
    assert!(
        app.bottom_request_focus,
        "wait for the modal layer to clear"
    );
    render(&mut app, &mut frame);
    assert!(!app.bottom_request_focus);
    assert_eq!(context.memory(|memory| memory.focused()), Some(shell_focus));
    app.toggle_bottom_shell();
    assert!(!app.bottom_open && !app.bottom_focus);
    assert_eq!(app.selected().unwrap().tab, 1);
    app.toggle_bottom_shell();
    assert_eq!(app.selected().unwrap().tab, 2);
    app.activate(1);
    assert!(!app.bottom_focus && !app.bottom_request_focus);
    assert_eq!(app.selected().unwrap().tab, 1);
}
