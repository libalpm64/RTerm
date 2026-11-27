use super::*;

fn fixture() -> (Terminal, Backend, mpsc::Receiver<Vec<u8>>) {
    let context = egui::Context::default();
    let (backend, _) = Backend::new(&context);
    let options = TerminalOptions {
        font: 13.0,
        cursor: CursorStyle::default(),
        scrollback: 100,
        copy_on_select: false,
        line_numbers: false,
        free_type: true,
    };
    let mut term = Terminal::new(1, "Editable fixture".into(), Kind::Local, &options);
    let (input, receiver) = mpsc::sync_channel(128);
    let (_result, startup) = mpsc::channel();
    term.startup = Some(LocalStartup {
        result: startup,
        input,
        size: term.size,
    });
    term.endpoint = Some(1);
    term.receive(b"\x1b[?2004hfixture$ echo ABC DEF");
    term.input_line = "echo ABC DEF".into();
    (term, backend, receiver)
}

#[test]
fn verified_echo_supports_click_move_and_selection_reordering() {
    let (mut term, backend, receiver) = fixture();
    let line = term.editable_line().unwrap();
    assert_eq!(line.start, 9);
    assert_eq!(line.caret, 12);
    term.free_click(Point::new(Line(0), Column(14)), Side::Left, &backend);
    assert_eq!(receiver.try_recv().unwrap(), b"\x1b[D".repeat(7));
    assert_eq!(term.free_type.caret, Some(5));
    // Simulate the editor's response to the seven left-arrow presses.
    term.receive(b"\x1b[7D");
    let mut selection = Selection::new(
        SelectionType::Simple,
        Point::new(Line(0), Column(14)),
        Side::Left,
    );
    selection.update(Point::new(Line(0), Column(16)), Side::Right);
    term.grid.selection = Some(selection);
    assert!(term.free_drag_begin(Point::new(Line(0), Column(15))));
    term.free_drag_drop(Point::new(Line(0), Column(21)), Side::Left, &backend);
    let mut expected = b"\x1b[C".repeat(3);
    expected.extend([0x7f; 3]);
    expected.extend(b"\x1b[C".repeat(4));
    expected.extend(b"\x1b[200~ABC\x1b[201~");
    assert_eq!(receiver.try_recv().unwrap(), expected);
    assert_eq!(term.input_line, "echo  DEFABC");
    assert_eq!(term.free_type.caret, Some(12));
    assert!(term.grid.selection.is_none());
}

#[test]
fn unverified_output_passwords_wrapped_lines_and_application_modes_are_not_edited() {
    let (mut term, backend, receiver) = fixture();
    term.input_line = "not actually echoed".into();
    assert!(term.editable_line().is_none());
    term.free_click(Point::new(Line(0), Column(14)), Side::Left, &backend);
    assert!(receiver.try_recv().is_err());
    term.input_line = "echo ABC DEF".into();
    term.receive(b"\x1b[?1000h");
    assert!(term.editable_line().is_none());
    term.receive(b"\x1b[?1000l\x1b[?1049h");
    assert!(term.editable_line().is_none());
    term.receive(b"\x1b[?1049l\x1b[?2004l");
    assert!(term.editable_line().is_none());
}

#[cfg(target_os = "linux")]
struct NativeFixture {
    // Keep the wrapper alive until the shell exits.
    _dir: tempfile::TempDir,
    context: egui::Context,
    backend: Backend,
    events: tokio::sync::mpsc::Receiver<BackendEvent>,
    term: Terminal,
    options: TerminalOptions,
    highlighter: Highlighter,
    width: f32,
    bounds: Rect,
    cell: Vec2,
}

#[cfg(target_os = "linux")]
impl NativeFixture {
    fn new(script: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let wrapper = dir.path().join("fixture-shell");
        std::fs::write(&wrapper, script).unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let context = egui::Context::default();
        crate::theme::install_fonts(&context);
        let (backend, events) = Backend::new(&context);
        let options = TerminalOptions {
            font: 24.0,
            cursor: CursorStyle::default(),
            scrollback: 100,
            copy_on_select: false,
            line_numbers: true,
            free_type: true,
        };
        let mut term = Terminal::new(1, "Native fixture".into(), Kind::Local, &options);
        term.queue_local(wrapper.to_str().unwrap(), true);
        Self {
            _dir: dir,
            context,
            backend,
            events,
            term,
            options,
            highlighter: Highlighter::new(false, ""),
            width: 1200.0,
            bounds: Rect::NOTHING,
            cell: Vec2::ZERO,
        }
    }

    fn render(&mut self, events: Vec<egui::Event>) {
        if let Some(result) = self.term.poll_local() {
            result.unwrap();
        }
        while let Ok(event) = self.events.try_recv() {
            match event {
                BackendEvent::Data { id, data } if self.term.endpoint == Some(id) => {
                    self.term.receive(&data)
                }
                BackendEvent::Closed(id) if self.term.endpoint == Some(id) => {
                    self.term.mark_closed()
                }
                _ => {}
            }
        }
        self.term.service(&self.backend, &self.context, false);
        let _ = self.context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(self.width, 600.0))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| {
                        let (response, _, error) = self.term.show(
                            ui,
                            ui.max_rect(),
                            &self.backend,
                            &self.options,
                            &self.highlighter,
                            true,
                            true,
                            true,
                            None,
                            None,
                        );
                        assert!(error.is_none(), "{error:?}");
                        assert!(response.has_focus());
                        let layout = terminal_viewport(
                            ui,
                            ui.max_rect(),
                            &self.options,
                            self.term.fullscreen_layout,
                            true,
                        );
                        self.bounds = layout.rect;
                        self.cell = layout.cell;
                    });
            },
        );
    }

    fn wait(&mut self, check: impl Fn(&Terminal) -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        loop {
            self.render(vec![]);
            if check(&self.term) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "native fixture timed out, status {:?}",
                self.term.status()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn pos(&self, row: usize, col: f32) -> egui::Pos2 {
        self.bounds.min + vec2(col * self.cell.x, (row as f32 + 0.5) * self.cell.y)
    }

    fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn click(&mut self, pos: egui::Pos2) {
        self.render(vec![egui::Event::PointerMoved(pos)]);
        self.render(vec![Self::button(pos, true)]);
        self.render(vec![Self::button(pos, false)]);
    }

    fn drag(&mut self, from: egui::Pos2, to: egui::Pos2) {
        self.render(vec![egui::Event::PointerMoved(from)]);
        self.render(vec![Self::button(from, true)]);
        self.render(vec![egui::Event::PointerMoved(to)]);
        self.render(vec![Self::button(to, false)]);
    }
}

#[cfg(target_os = "linux")]
impl Drop for NativeFixture {
    fn drop(&mut self) {
        self.term.mark_closed();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn native_bash_free_type_click_select_move_and_replace() {
    let mut f = NativeFixture::new(
        r#"#!/bin/sh
export INPUTRC=/dev/null HISTFILE=/dev/null
export PS1='\[\033[32m\]fixture@fedora:~$\[\033[0m\] '
exec /bin/bash --noprofile --norc -i
"#,
    );
    let prompt = "fixture@fedora:~$ ";
    f.wait(|term| !term.starting() && term.visible_line_is(prompt.trim_end()));
    assert_eq!(f.term.grid.grid().cursor.point.column.0, prompt.len());
    // A new split changes an already-running shell's terminal width.
    f.width = 650.0;
    f.wait(|term| term.size.cols < 50);
    // Allow the shell's resize redraw to arrive before typing.
    for _ in 0..20 {
        f.render(vec![]);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        f.term.grid.grid().cursor.point.column.0,
        prompt.len(),
        "cursor after split resize"
    );
    f.render(vec![egui::Event::Text("echo ABC DEF".into())]);
    f.wait(|term| term.editable_line().is_some_and(|line| line.caret == 12));
    let row = f.term.grid.grid().cursor.point.line.0 as usize;
    let start = f.term.editable_line().unwrap().start;
    f.click(f.pos(row, (start + 8) as f32 + 0.1));
    f.wait(|term| term.editable_line().is_some_and(|line| line.caret == 8));
    f.render(vec![egui::Event::Text("X".into())]);
    f.wait(|term| term.visible_line_is("fixture@fedora:~$ echo ABCX DEF"));
    assert_eq!(f.term.input_line, "echo ABCX DEF");
    // Select ABCX then drag it to the end of the command.
    f.drag(
        f.pos(row, (start + 5) as f32 + 0.1),
        f.pos(row, (start + 8) as f32 + 0.9),
    );
    assert_eq!(f.term.grid.selection_to_string().as_deref(), Some("ABCX"));
    f.drag(
        f.pos(row, (start + 6) as f32 + 0.1),
        f.pos(row, (start + 13) as f32 + 0.1),
    );
    f.wait(|term| term.visible_line_is("fixture@fedora:~$ echo  DEFABCX"));
    assert_eq!(f.term.input_line, "echo  DEFABCX");
    f.drag(
        f.pos(row, (start + 6) as f32 + 0.1),
        f.pos(row, (start + 8) as f32 + 0.9),
    );
    assert_eq!(f.term.grid.selection_to_string().as_deref(), Some("DEF"));
    f.render(vec![egui::Event::Text("GHI".into())]);
    f.wait(|term| term.visible_line_is("fixture@fedora:~$ echo  GHIABCX"));
    assert!(
        !f.term.visible_line_is("GHIABCX"),
        "editing must not execute a command"
    );
    f.render(vec![egui::Event::Key {
        key: Key::Enter,
        physical_key: Some(Key::Enter),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }]);
    f.wait(|term| term.visible_line_is("GHIABCX"));
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires Neovim on PATH"]
fn native_neovim_mouse_click_and_drag() {
    let mut f = NativeFixture::new(
        r#"#!/bin/sh
exec nvim -u NONE -i NONE -n -c 'set mouse=a nonumber norelativenumber laststatus=0' -c "call setline(1, 'abcdefghijklmno')" -c 'normal! 0'
"#,
    );
    f.wait(|term| {
        term.grid.mode().contains(TermMode::ALT_SCREEN)
            && term.grid.mode().intersects(TermMode::MOUSE_MODE)
            && term.visible_line_is("abcdefghijklmno")
    });
    f.click(f.pos(0, 6.2));
    f.wait(|term| term.grid.grid().cursor.point.column.0 == 6);
    // Start the drag away from the previous click so Neovim does not
    // interpret the second press as a double-click selecting a whole word.
    f.drag(f.pos(0, 2.2), f.pos(0, 10.2));
    f.wait(|term| term.grid.grid().cursor.point.column.0 == 10);
    assert!(
        f.term.grid.selection.is_none(),
        "editor owns its own mouse selection"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn bash_startup_at_split_width_keeps_prompt_cursor_and_first_input() {
    for width in [650.0, 1293.0] {
        let mut f = NativeFixture::new(
            r#"#!/bin/sh
export INPUTRC=/dev/null HISTFILE=/dev/null
export PS1='\[\033[0m\033[1m\033[32m\]guest@local-fedora\[\033[0m\]:\[\033[1m\033[32m\]~\[\033[0m\]$ '
exec /bin/bash --noprofile --norc -i
"#,
        );
        f.width = width;
        f.options.font = 22.0;
        let prompt = "guest@local-fedora:~$ ";
        f.wait(|term| !term.starting() && term.visible_line_is(prompt.trim_end()));
        assert_eq!(
            f.term.grid.grid().cursor.point.column.0,
            prompt.len(),
            "initial size {:?}",
            f.term.size()
        );
        f.width = 650.0;
        for _ in 0..40 {
            f.render(vec![]);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            f.term.grid.grid().cursor.point.column.0,
            prompt.len(),
            "resized size {:?}",
            f.term.size()
        );
        for c in "abc123XYZ".chars() {
            f.render(vec![egui::Event::Text(c.to_string())]);
            let expected = format!(
                "{prompt}{}",
                &"abc123XYZ"[.."abc123XYZ".find(c).unwrap() + 1]
            );
            f.wait(|term| term.visible_line_is(&expected));
            assert_eq!(f.term.grid.grid().cursor.point.column.0, expected.len());
        }
    }
}

// Temporary diagnostic, uses the actual configured interactive startup.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "diagnostic for local Bash startup configuration"]
fn native_configured_bash_prompt_probe() {
    for width in [650.0, 1293.0] {
        let mut f = NativeFixture::new(
            r#"#!/bin/sh
export HISTFILE=/dev/null HISTSIZE=0 HISTFILESIZE=0
exec /bin/bash -i
"#,
        );
        f.width = width;
        f.options.font = 22.0;
        f.wait(|term| {
            !term.starting()
                && term.grid.mode().contains(TermMode::BRACKETED_PASTE)
                && term.visible_line_matches(|line| line.ends_with("$") || line.ends_with("#"))
        });
        for _ in 0..40 {
            f.render(vec![]);
            std::thread::sleep(Duration::from_millis(5));
        }
        let check = |term: &Terminal, label: &str| {
            let grid = term.grid.grid();
            let p = grid.cursor.point;
            let row = &grid[p.line];
            let text: String = (0..term.size.cols)
                .map(|c| row[Column(c as usize)].c)
                .collect();
            println!(
                "{label}: cols={} cursor={:?} last_nonblank={} prompt_ends={}",
                term.size.cols,
                p,
                text.trim_end().chars().count(),
                text.trim_end().ends_with('$')
            );
            assert_eq!(p.column.0, text.trim_end().chars().count() + 1);
        };
        check(&f.term, "startup");
        f.width = 650.0;
        for _ in 0..40 {
            f.render(vec![]);
            std::thread::sleep(Duration::from_millis(5));
        }
        check(&f.term, "split");
        f.render(vec![egui::Event::Text("rtprobe".into())]);
        f.wait(|term| {
            term.visible_line_matches(|line| {
                line.ends_with("$ rtprobe") || line.ends_with("# rtprobe")
            })
        });
        assert_eq!(
            f.term.grid.grid().cursor.point.column.0,
            (0..f.term.size.cols)
                .map(|c| f.term.grid.grid()[f.term.grid.grid().cursor.point.line]
                    [Column(c as usize)]
                .c)
                .collect::<String>()
                .trim_end()
                .len()
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires Bash 5.3 patch 20 / Readline 8.3 patch 6; older shells emit incorrect cursor movement after widening"]
fn bash_prompt_survives_repeated_resolution_changes() {
    let mut f = NativeFixture::new(
        r#"#!/bin/sh
export INPUTRC=/dev/null HISTFILE=/dev/null
export PS1='\[\033[0m\033[1m\033[32m\]guest@local-fedora\[\033[0m\]:\[\033[1m\033[32m\]~\[\033[0m\]$ '
exec /bin/bash --noprofile --norc -i
"#,
    );
    let prompt = "guest@local-fedora:~$ ";
    f.options.font = 20.0;
    f.wait(|term| !term.starting() && term.visible_line_is(prompt.trim_end()));
    for (width, scale, delay) in [
        (650.0, 1.0, 30),
        (300.0, 1.0, 50),
        (650.0, 1.0, 30),
        (480.0, 1.25, 0),
        (120.0, 2.0, 0),
        (650.0, 1.0, 30),
        (300.0, 1.0, 0),
        (500.0, 1.5, 0),
        (650.0, 1.0, 30),
        (400.0, 1.0, 100),
        (650.0, 1.0, 30),
    ] {
        println!(
            "RESIZE {width} scale {scale} delay {delay} before {:?}",
            f.term.status()
        );
        f.width = width;
        f.context.set_pixels_per_point(scale);
        f.render(vec![]);
        std::thread::sleep(Duration::from_millis(delay));
        if width == 650.0 {
            for _ in 0..25 {
                f.render(vec![]);
                std::thread::sleep(Duration::from_millis(5));
            }
            let p = f.term.grid.grid().cursor.point;
            assert!(
                f.term.visible_line_is(prompt.trim_end()),
                "prompt after resize {width} scale {scale}: {:?}",
                f.term.status()
            );
            assert_eq!(
                p.column.0,
                prompt.len(),
                "cursor after resize {width} scale {scale}: {:?}",
                f.term.status()
            );
            f.render(vec![egui::Event::Text("x".into())]);
            f.wait(|term| term.visible_line_is(&format!("{prompt}x")));
            f.render(vec![egui::Event::Key {
                key: Key::Backspace,
                physical_key: Some(Key::Backspace),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]);
            f.wait(|term| term.visible_line_is(prompt.trim_end()));
        }
    }
}
