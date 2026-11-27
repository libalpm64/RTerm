use super::*;

fn terminal() -> Terminal {
    Terminal::new(
        1,
        String::new(),
        Kind::Local,
        &TerminalOptions {
            font: 12.0,
            cursor: Default::default(),
            scrollback: 100,
            copy_on_select: false,
            line_numbers: true,
        },
    )
}

#[test]
fn command_markers_survive_scrollback_and_resize() {
    let mut term = terminal();
    term.receive(b"\x1b]13");
    term.receive(b"3;A\x07$ first\x1b]133;C\x07\r\nresult\x1b]133;D;17\x1b\\\r\n");
    term.receive(b"\x1b]133;A\x07$ second\x1b]133;C\x07\r\nresult\x1b]133;D;0\x07\r\n");
    for _ in 0..32 {
        term.receive(b"output\r\n");
    }
    term.jump_command(true);
    let second = term.commands.selected.unwrap();
    assert_eq!(term.commands.marks[&second].result.unwrap().1, Some(0));
    assert!(term.grid.grid().display_offset() > 0);
    term.jump_command(true);
    let first = term.commands.selected.unwrap();
    assert_eq!(term.commands.marks[&first].result.unwrap().1, Some(17));
    term.grid.resize(Size(PtySize {
        cols: 40,
        rows: 24,
        pixel_width: 0,
        pixel_height: 0,
    }));
    term.jump_command(false);
    assert_eq!(term.commands.selected, Some(second));
    term.clear();
    term.jump_command(true);
    assert!(term.commands.selected.is_none());
}

#[test]
fn terminal_service_recovers_unfinished_synchronized_frames() {
    let mut term = terminal();
    let context = egui::Context::default();
    let (backend, _) = Backend::new(&context);
    term.receive(b"\x1b[?2026hA");
    assert_eq!(term.grid.grid()[Line(0)][Column(0)].c, ' ');
    term.receive(b"B\x1b[?2026l");
    assert_eq!(term.grid.grid()[Line(0)][Column(1)].c, 'B');
    term.receive(b"\x1b[?2026hC");
    assert_eq!(term.grid.grid()[Line(0)][Column(2)].c, ' ');
    let timeout = term.parser.sync_timeout().sync_timeout().unwrap();
    std::thread::sleep(
        timeout.saturating_duration_since(Instant::now()) + Duration::from_millis(5),
    );
    term.service(&backend, &context, false);
    assert_eq!(term.grid.grid()[Line(0)][Column(2)].c, 'C');
    assert!(term.parser.sync_timeout().sync_timeout().is_none());
}
