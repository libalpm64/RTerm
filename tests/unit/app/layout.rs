use super::*;

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
