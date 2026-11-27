use super::*;
use egui::{Event, PointerButton, Pos2, RawInput};

#[test]
fn tree_rejects_duplicates_and_collapses() {
    let mut splits = Splits::default();
    assert!(!splits.split(1, 1, Direction::Right));
    assert!(splits.split(1, 2, Direction::Right));
    assert!(!splits.split(99, 3, Direction::Down));
    assert!(!splits.split(1, 2, Direction::Down));
    assert!(splits.split(1, 3, Direction::Down));
    assert!(splits.split(2, 4, Direction::Down));
    assert!(!splits.can_split());
    assert!(!splits.split(4, 5, Direction::Right));
    splits.select(Some(1), 2);
    assert!((1..=4).all(|id| splits.contains(id)));
    splits.select(Some(2), 5);
    assert!(!splits.contains(2));
    assert!([1, 3, 4, 5].into_iter().all(|id| splits.contains(id)));
    assert_eq!(splits.remove(3), Some(1));
    assert_eq!(splits.remove(1), Some(5));
    assert_eq!(splits.remove(4), Some(5));
    assert!(!splits.is_split());
    assert!(splits.can_split());
}

fn pointer(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

#[test]
fn geometry_drag_and_narrow_window_restore() {
    let context = egui::Context::default();
    let mut splits = Splits::default();
    assert!(splits.split(1, 2, Direction::Right));
    assert!(splits.split(1, 3, Direction::Down));
    assert!(splits.split(2, 4, Direction::Down));
    let large = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
    let mut render = |rect: Rect, events| {
        let mut panes = Vec::new();
        let _ = context.run(
            RawInput {
                screen_rect: Some(large),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| {
                        panes = splits.layout(ui, rect, Some(4), Theme::dark(), true);
                    });
            },
        );
        panes
    };
    let original = render(large, vec![]);
    assert_eq!(
        original.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![1, 3, 2, 4]
    );
    for (i, (_, rect)) in original.iter().enumerate() {
        assert!(large.contains_rect(*rect));
        assert!(rect.width() >= MIN_PANE.x && rect.height() >= MIN_PANE.y);
        for (_, other) in &original[..i] {
            assert!(!rect.intersects(*other));
        }
    }
    let divider = pos2(400.0, 100.0);
    render(large, vec![Event::PointerMoved(divider)]);
    render(large, vec![pointer(divider, true)]);
    let moved = pos2(550.0, 100.0);
    render(large, vec![Event::PointerMoved(moved)]);
    render(large, vec![pointer(moved, false)]);
    let resized = render(large, vec![]);
    assert!(
        resized[0].1.width() > original[0].1.width() + 100.0,
        "{resized:?}"
    );
    let narrow = large.with_max_x(200.0);
    assert_eq!(render(narrow, vec![]), vec![(4, narrow)]);
    assert_eq!(render(large, vec![]), resized);
}

fn fixture(context: &egui::Context, count: u64) -> App {
    let mut app = App::with_settings(context, Settings::benchmark_defaults(), false);
    app.sidebar = None;
    app.sessions_sidebar = None;
    for id in 1..=count {
        app.terminals.push(Terminal::new(
            id,
            format!("Fixture {id}"),
            Kind::Local,
            &app.settings.terminal(),
        ));
    }
    app.next_tab = count + 1;
    app.activate(1);
    app
}

#[test]
fn hiding_and_selecting_keep_sessions_alive() {
    let context = egui::Context::default();
    let mut app = fixture(&context, 5);
    app.split_existing(2, Direction::Right);
    app.split_existing(3, Direction::Down);
    app.split_existing(4, Direction::Right);
    assert_eq!(app.active, Some(4));
    let next = app.next_tab;
    app.split_new(Direction::Down);
    assert_eq!(app.next_tab, next);
    assert_eq!(app.terminals.len(), 5);
    app.activate(5);
    assert!(!app.splits.contains(4));
    assert!(app.splits.contains(5));
    app.hide_pane(2);
    assert_eq!(app.active, Some(5));
    assert_eq!(app.terminals.len(), 5);
    app.close_tab(5);
    assert_eq!(app.active, Some(1));
    assert!(app.splits.contains(1) && app.splits.contains(3));
    app.hide_pane(1);
    assert_eq!(app.active, Some(3));
    assert!(!app.splits.is_split());
    app.split_existing(4, Direction::Right);
    app.single_pane();
    assert_eq!(app.active, Some(4));
    assert_eq!(app.terminals.len(), 4);
    assert!(!app.splits.is_split());
    app.split_new(Direction::Down);
    assert!(app.splits.contains(next));
    assert_eq!(app.active, Some(next));
    assert_eq!(app.terminals.len(), 5);
}

#[cfg(unix)]
#[test]
fn real_pty_input_is_exclusive_across_panes_and_bottom_shell() {
    let context = egui::Context::default();
    let mut app = fixture(&context, 2);
    for term in &mut app.terminals {
        term.queue_local("/bin/sh", false);
    }
    let mut bottom = Terminal::new(
        3,
        "Bottom fixture".into(),
        Kind::Local,
        &app.bottom_options(),
    );
    bottom.queue_local("/bin/sh", false);
    app.bottom_shell = Some(bottom);
    app.bottom_open = true;
    app.next_tab = 4;
    app.split_existing(2, Direction::Right);
    let mut frame = eframe::Frame::_new_kittest();
    let mut render = |app: &mut App, events| {
        let mut raw = RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))),
            events,
            ..Default::default()
        };
        eframe::App::raw_input_hook(app, &context, &mut raw);
        let _ = context.run(raw, |ctx| eframe::App::update(app, ctx, &mut frame));
    };
    fn wait_for(
        app: &mut App,
        render: &mut impl FnMut(&mut App, Vec<Event>),
        check: impl Fn(&App) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            render(app, vec![]);
            if check(app) {
                return;
            }
            assert!(Instant::now() < deadline, "PTY fixture did not respond");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    wait_for(&mut app, &mut render, |app| {
        app.terminals
            .iter()
            .chain(app.bottom_shell.iter())
            .all(|term| !term.starting() && term.endpoint.is_some())
    });
    let enter = Event::Key {
        key: Key::Enter,
        physical_key: Some(Key::Enter),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    let send =
        |render: &mut dyn FnMut(&mut App, Vec<Event>), app: &mut App, pos: Pos2, command: &str| {
            render(app, vec![Event::PointerMoved(pos)]);
            // Text and Enter deliberately arrive in the same frame as the press.
            render(
                app,
                vec![
                    pointer(pos, true),
                    Event::Text(command.into()),
                    enter.clone(),
                ],
            );
            render(app, vec![pointer(pos, false)]);
        };
    send(
        &mut render,
        &mut app,
        pos2(220.0, 180.0),
        "echo RTERM_SPLIT_LEFT",
    );
    assert_eq!(app.active, Some(1));
    send(
        &mut render,
        &mut app,
        pos2(920.0, 180.0),
        "echo RTERM_SPLIT_RIGHT",
    );
    assert_eq!(app.active, Some(2));
    send(
        &mut render,
        &mut app,
        pos2(400.0, 735.0),
        "echo RTERM_SPLIT_BOTTOM",
    );
    assert!(app.bottom_focus);
    send(
        &mut render,
        &mut app,
        pos2(220.0, 180.0),
        "echo RTERM_SPLIT_RETURN",
    );
    assert!(!app.bottom_focus);
    wait_for(&mut app, &mut render, |app| {
        app.terminals[0].visible_line_is("RTERM_SPLIT_LEFT")
            && app.terminals[0].visible_line_is("RTERM_SPLIT_RETURN")
            && app.terminals[1].visible_line_is("RTERM_SPLIT_RIGHT")
            && app
                .bottom_shell
                .as_ref()
                .unwrap()
                .visible_line_is("RTERM_SPLIT_BOTTOM")
    });
    let markers = [
        "RTERM_SPLIT_LEFT",
        "RTERM_SPLIT_RIGHT",
        "RTERM_SPLIT_BOTTOM",
        "RTERM_SPLIT_RETURN",
    ];
    for (i, term) in app
        .terminals
        .iter()
        .chain(app.bottom_shell.iter())
        .enumerate()
    {
        for (j, marker) in markers.iter().enumerate() {
            assert_eq!(
                term.visible_line_is(marker),
                i == j || (i == 0 && j == 3),
                "pane {i}: {marker}"
            );
        }
    }
    let before = app.terminals[0].size();
    // Drag divider to increase the first terminal's width.
    let divider = pos2(600.0, 250.0);
    render(&mut app, vec![Event::PointerMoved(divider)]);
    render(&mut app, vec![pointer(divider, true)]);
    render(&mut app, vec![Event::PointerMoved(pos2(760.0, 250.0))]);
    render(&mut app, vec![pointer(pos2(760.0, 250.0), false)]);
    render(&mut app, vec![]);
    assert!(app.terminals[0].size().cols > before.cols);
    assert!(app.terminals[0].size().cols > app.terminals[1].size().cols);
    let command = "stty size; echo RTERM_SPLIT_RESIZED";
    // No intervening pane click: the divider must preserve the keyboard owner.
    assert_eq!(app.active, Some(1));
    render(&mut app, vec![Event::Text(command.into()), enter.clone()]);
    let size = app.terminals[0].size();
    wait_for(&mut app, &mut render, |app| {
        app.terminals[0].visible_line_is("RTERM_SPLIT_RESIZED")
    });
    assert!(app.terminals[0].visible_line_is(&format!("{} {}", size.rows, size.cols)));
    // A header or gutter click used to lose focus when egui registered
    // the smaller text-grid widget. Type only after release and idle frames.
    let click = |render: &mut dyn FnMut(&mut App, Vec<Event>), app: &mut App, pos: Pos2| {
        render(app, vec![Event::PointerMoved(pos)]);
        render(app, vec![pointer(pos, true)]);
        render(app, vec![pointer(pos, false)]);
        render(app, vec![]);
    };
    click(&mut render, &mut app, pos2(900.0, 81.0));
    assert_eq!(app.active, Some(2));
    render(
        &mut app,
        vec![Event::Text("echo RTERM_HEADER_FOCUS".into()), enter.clone()],
    );
    wait_for(&mut app, &mut render, |app| {
        app.terminals[1].visible_line_is("RTERM_HEADER_FOCUS")
    });
    click(&mut render, &mut app, pos2(20.0, 185.0));
    assert_eq!(app.active, Some(1));
    render(
        &mut app,
        vec![Event::Text("echo RTERM_GUTTER_FOCUS".into()), enter.clone()],
    );
    wait_for(&mut app, &mut render, |app| {
        app.terminals[0].visible_line_is("RTERM_GUTTER_FOCUS")
    });
    assert!(!app.terminals[0].visible_line_is("RTERM_HEADER_FOCUS"));
    assert!(!app.terminals[1].visible_line_is("RTERM_GUTTER_FOCUS"));
    for term in app.terminals.iter_mut().chain(app.bottom_shell.iter_mut()) {
        term.mark_closed();
    }
}

#[test]
fn header_remove_does_not_focus_or_close_inactive_session() {
    let context = egui::Context::default();
    let mut app = fixture(&context, 2);
    app.split_existing(2, Direction::Right);
    let mut frame = eframe::Frame::_new_kittest();
    let mut render = |app: &mut App, events| {
        let mut raw = RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))),
            events,
            ..Default::default()
        };
        eframe::App::raw_input_hook(app, &context, &mut raw);
        let _ = context.run(raw, |ctx| eframe::App::update(app, ctx, &mut frame));
    };
    render(&mut app, vec![]);
    // Left pane header's remove button, with right pane focused.
    let pos = pos2(586.0, 81.0);
    render(&mut app, vec![Event::PointerMoved(pos)]);
    render(&mut app, vec![pointer(pos, true)]);
    assert_eq!(app.active, Some(2));
    render(&mut app, vec![pointer(pos, false)]);
    assert_eq!(app.active, Some(2));
    assert!(!app.splits.is_split());
    assert_eq!(app.terminals.len(), 2);
}

#[test]
fn dragging_tabs_to_panes_swaps_or_replaces_existing_sessions() {
    let context = egui::Context::default();
    let mut app = fixture(&context, 3);
    app.split_existing(2, Direction::Right);
    let mut frame = eframe::Frame::_new_kittest();
    let mut render = |app: &mut App, events| {
        let mut raw = RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))),
            events,
            ..Default::default()
        };
        eframe::App::raw_input_hook(app, &context, &mut raw);
        context.run(raw, |ctx| eframe::App::update(app, ctx, &mut frame))
    };
    let output = render(&mut app, vec![]);
    let label_center = |output: &egui::FullOutput, label: &str| {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == label && text.pos.y < 50.0 => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .unwrap()
    };
    let a = label_center(&output, "Fixture 1");
    let c = label_center(&output, "Fixture 3");
    let drag = |render: &mut dyn FnMut(&mut App, Vec<Event>) -> egui::FullOutput,
                app: &mut App,
                from: Pos2,
                to: Pos2| {
        render(app, vec![Event::PointerMoved(from)]);
        render(app, vec![pointer(from, true)]);
        render(app, vec![Event::PointerMoved(to)]);
        assert!(app.drag_tab.is_some(), "tab must own the drag");
        render(app, vec![pointer(to, false)]);
        render(app, vec![]);
    };
    drag(&mut render, &mut app, a, pos2(920.0, 180.0));
    assert_eq!(app.active, Some(1));
    assert_eq!(app.splits.root.as_ref().unwrap().first(), 2);
    assert!(app.splits.contains(1) && app.splits.contains(2));
    drag(&mut render, &mut app, c, pos2(220.0, 180.0));
    assert_eq!(app.active, Some(3));
    assert_eq!(app.splits.root.as_ref().unwrap().first(), 3);
    assert!(app.splits.contains(1) && app.splits.contains(3));
    assert!(!app.splits.contains(2));
    assert_eq!(app.terminals.len(), 3);
    assert_eq!(app.next_tab, 4);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires a native Wayland desktop; renders isolated synthetic split terminals"]
fn native_split_preview() {
    use winit::platform::wayland::EventLoopBuilderExtWayland;
    struct Preview {
        app: App,
        frames: u32,
    }
    impl eframe::App for Preview {
        fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
            if let Some(image) = ctx.input(|input| {
                input.events.iter().find_map(|event| match event {
                    Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            }) {
                image::save_buffer(
                    "target/linux-freetype-fixture-preview.png",
                    image.as_raw(),
                    image.width() as u32,
                    image.height() as u32,
                    image::ColorType::Rgba8,
                )
                .unwrap();
                self.app.exit_confirmed = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            self.app.update(ctx, frame);
            if self.frames == 2 {
                for term in &mut self.app.terminals {
                    term.receive(
                        b"\x1b[1;32mLocal Terminal\x1b[0m\r\n\x1b[1;32mfixture@fedora:~\x1b[0m$ ",
                    );
                }
            }
            self.frames += 1;
            if self.frames == 6 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
            ctx.request_repaint_after(Duration::from_millis(60));
        }
    }
    eframe::run_native(
        "rterm-split-fixture",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("RTerm synthetic split fixture")
                .with_inner_size([1440.0, 900.0]),
            event_loop_builder: Some(Box::new(|builder| {
                builder.with_wayland().with_any_thread(true);
            })),
            ..Default::default()
        },
        Box::new(move |cc| {
            let mut app = fixture(&cc.egui_ctx, 2);
            // Reproduce a large-font, side-by-side layout, with no real
            // local file listing or session credentials in the screenshot.
            app.settings.set("font_size", "24".into());
            app.settings.set("cursor_blink", "0".into());
            app.terminals[0].name = "Terminal 1".into();
            app.terminals[1].name = "Terminal 2".into();
            app.split_existing(2, Direction::Right);
            Ok(Box::new(Preview { app, frames: 0 }))
        }),
    )
    .unwrap();
    assert!(std::path::Path::new("target/linux-freetype-fixture-preview.png").is_file());
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "native Wayland reproduction of the shell startup/split cursor"]
fn native_bash_split_cursor_probe() {
    use winit::platform::wayland::EventLoopBuilderExtWayland;
    struct Probe {
        app: App,
        phase: u8,
        settled: Option<Instant>,
        deadline: Instant,
        result: std::sync::Arc<std::sync::Mutex<Option<bool>>>,
    }
    impl eframe::App for Probe {
        fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut RawInput) {
            self.app.raw_input_hook(ctx, raw);
        }
        fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
            let prompt_ready = |term: &Terminal| {
                !term.starting()
                    && term.visible_line_matches(|line| line.ends_with('$') || line.ends_with('#'))
            };
            let ready = self.app.terminals.iter().all(prompt_ready);
            if self.phase == 0 && ready {
                self.settled.get_or_insert_with(Instant::now);
                if self.settled.unwrap().elapsed() > Duration::from_millis(300) {
                    println!("INITIAL {:?}", self.app.terminals[0].status());
                    self.app.split_new(Direction::Right);
                    self.phase = 1;
                    self.settled = None;
                }
            } else if self.phase == 1 && ready {
                self.settled.get_or_insert_with(Instant::now);
                if self.settled.unwrap().elapsed() > Duration::from_millis(350) {
                    for term in &self.app.terminals {
                        println!("SPLIT {} {:?}", term.tab, term.status());
                    }
                    self.app.activate(1);
                    ctx.input_mut(|input| input.events.push(Event::Text("abc".into())));
                    self.phase = 2;
                }
            } else if self.phase == 2
                && self.app.terminals[0]
                    .visible_line_matches(|line| line.ends_with("$ abc") || line.ends_with("# abc"))
            {
                println!("LEFT INPUT {:?}", self.app.terminals[0].status());
                self.app.activate(2);
                ctx.input_mut(|input| input.events.push(Event::Text("xyz".into())));
                self.phase = 3;
            } else if self.phase == 3
                && self.app.terminals[1]
                    .visible_line_matches(|line| line.ends_with("$ xyz") || line.ends_with("# xyz"))
            {
                println!("RIGHT INPUT {:?}", self.app.terminals[1].status());
                self.phase = 4;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
            if let Some(image) = ctx.input(|input| {
                input.events.iter().find_map(|event| match event {
                    Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            }) {
                image::save_buffer(
                    "target/linux-native-bash-split-probe.png",
                    image.as_raw(),
                    image.width() as u32,
                    image.height() as u32,
                    image::ColorType::Rgba8,
                )
                .unwrap();
                *self.result.lock().unwrap() = Some(self.phase == 4);
                self.app.exit_confirmed = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            if Instant::now() >= self.deadline && self.phase != 5 {
                println!(
                    "TIMEOUT phase={} {:?}",
                    self.phase,
                    self.app
                        .terminals
                        .iter()
                        .map(Terminal::status)
                        .collect::<Vec<_>>()
                );
                self.phase = 5;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
            self.app.update(ctx, frame);
            ctx.request_repaint_after(Duration::from_millis(40));
        }
    }
    let result = std::sync::Arc::new(std::sync::Mutex::new(None));
    let result_for_app = result.clone();
    eframe::run_native(
        "rterm-bash-split-probe",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("RTerm isolated Bash split diagnostic")
                .with_inner_size([1293.0, 900.0]),
            event_loop_builder: Some(Box::new(|builder| {
                builder.with_wayland().with_any_thread(true);
            })),
            ..Default::default()
        },
        Box::new(move |cc| {
            let mut settings = Settings::benchmark_defaults();
            // Read only display options from the live installation, not profiles,
            // credentials, file history or agent configuration.
            for key in [
                "font_size",
                "line_numbers",
                "cursor_style",
                "cursor_blink",
                "free_type",
                "semantic_hl",
                "scrollback",
            ] {
                if let Some(value) = storage::load_setting(key) {
                    settings.set(key, value);
                }
            }
            println!(
                "Display font={} line_numbers={}",
                settings.get("font_size"),
                settings.get("line_numbers")
            );
            let mut app = App::with_settings(&cc.egui_ctx, settings, false);
            app.sidebar = None;
            app.sessions_sidebar = None;
            app.new_local();
            Ok(Box::new(Probe {
                app,
                phase: 0,
                settled: None,
                deadline: Instant::now() + Duration::from_secs(10),
                result: result_for_app,
            }))
        }),
    )
    .unwrap();
    assert_eq!(*result.lock().unwrap(), Some(true));
}

#[cfg(target_os = "linux")]
#[test]
fn bash_prompt_remains_editable_when_splitting_during_startup() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let wrapper = directory.path().join("bash-fixture");
    std::fs::write(&wrapper, r#"#!/bin/sh
export INPUTRC=/dev/null HISTFILE=/dev/null
export PS1='\[\033[0m\033[1m\033[32m\]guest@local-fedora\[\033[0m\]:\[\033[1m\033[32m\]~\[\033[0m\]$ '
exec /bin/bash --noprofile --norc -i
"#).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    for (initial_width, delay) in [
        (800.0, 0),
        (800.0, 1),
        (900.0, 5),
        (1200.0, 20),
        (1293.0, 100),
    ] {
        let context = egui::Context::default();
        let mut app = App::with_settings(&context, Settings::benchmark_defaults(), false);
        app.settings.set("font_size", "20".into());
        app.settings.set("line_numbers", "1".into());
        let mut frame = eframe::Frame::_new_kittest();
        let profile = |name: &str| Profile {
            name: name.into(),
            connection: Connection::Local {
                shell: wrapper.to_str().unwrap().into(),
            },
        };
        app.connect(profile("Left fixture"), None);
        let mut render = |app: &mut App, width: f32, events: Vec<Event>| {
            let mut raw = RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 800.0))),
                events,
                ..Default::default()
            };
            eframe::App::raw_input_hook(app, &context, &mut raw);
            let _ = context.run(raw, |ctx| eframe::App::update(app, ctx, &mut frame));
        };
        render(&mut app, initial_width, vec![]);
        std::thread::sleep(Duration::from_millis(delay));
        app.sidebar = None;
        render(&mut app, 1293.0, vec![]);
        app.sessions_sidebar = None;
        app.splits.split(1, 2, Direction::Right);
        app.connect(profile("Right fixture"), None);
        let prompt = "guest@local-fedora:~$";
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            render(&mut app, 1293.0, vec![]);
            if app
                .terminals
                .iter()
                .all(|term| !term.starting() && term.visible_line_is(prompt))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "initial width {initial_width}, delay {delay}, {:?}",
                app.terminals
                    .iter()
                    .map(Terminal::status)
                    .collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        for _ in 0..15 {
            render(&mut app, 1293.0, vec![]);
            std::thread::sleep(Duration::from_millis(5));
        }
        for (tab, marker) in [(1, "abc"), (2, "xyz")] {
            app.activate(tab);
            render(&mut app, 1293.0, vec![Event::Text(marker.into())]);
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                render(&mut app, 1293.0, vec![]);
                if app.terminals[(tab - 1) as usize].visible_line_is(&format!("{prompt} {marker}"))
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "typing in pane {tab}: width {initial_width}, delay {delay}, {:?}",
                    app.terminals
                        .iter()
                        .map(Terminal::status)
                        .collect::<Vec<_>>()
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        for term in &mut app.terminals {
            term.mark_closed();
        }
    }
}
