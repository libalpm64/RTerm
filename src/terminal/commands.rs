use super::*;
use alacritty_terminal::vte::{Parser, Perform};
use std::{collections::BTreeMap, num::NonZeroU64, time::Instant};

#[derive(Default)]
pub(super) struct Commands {
    pub(super) parser: Parser,
    marks: BTreeMap<NonZeroU64, Command>,
    prompt: Option<NonZeroU64>,
    running: Option<NonZeroU64>,
    selected: Option<NonZeroU64>,
    last_id: u64,
}

impl Commands {
    pub(super) fn reset_parser(&mut self) {
        self.parser = Parser::new();
        self.prompt = None;
        self.selected = None;
    }
}

struct Command {
    started: Instant,
    result: Option<(Duration, Option<u8>)>,
}

#[derive(Default)]
pub(super) enum Marker {
    #[default]
    None,
    Prompt,
    Start,
    End(Option<u8>),
}

impl Perform for Marker {
    fn osc_dispatch(&mut self, params: &[&[u8]], _: bool) {
        *self = match params {
            [b"133", b"A", ..] => Self::Prompt,
            [b"133", b"C", ..] => Self::Start,
            [b"133", b"D", status, ..] => Self::End(
                std::str::from_utf8(status)
                    .ok()
                    .and_then(|value| value.parse().ok()),
            ),
            [b"133", b"D"] => Self::End(None),
            _ => return,
        };
    }

    fn terminated(&self) -> bool {
        !matches!(self, Self::None)
    }
}

impl Terminal {
    pub(super) fn flush_sync_timeout(&mut self) {
        if self
            .parser
            .sync_timeout()
            .sync_timeout()
            .is_some_and(|deadline| deadline <= Instant::now())
        {
            self.parser.stop_sync(&mut self.grid);
        }
    }
    pub(super) fn receive_command_markers(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            let mut marker = Marker::None;
            let used = self
                .commands
                .parser
                .advance_until_terminated(&mut marker, data);
            self.parser.advance(&mut self.grid, &data[..used]);
            data = &data[used..];
            if !self.grid.mode().contains(TermMode::ALT_SCREEN)
                && self.parser.sync_timeout().sync_timeout().is_none()
            {
                self.apply_command_marker(marker);
            }
        }
    }

    fn mark_command_row(&mut self) -> NonZeroU64 {
        self.commands.last_id += 1;
        let id = NonZeroU64::new(self.commands.last_id).unwrap();
        let line = self.grid.grid().cursor.point.line;
        self.grid.grid_mut()[line].command_mark = Some(id);
        id
    }

    fn apply_command_marker(&mut self, marker: Marker) {
        match marker {
            Marker::Prompt => {
                self.finish_shell_command(None);
                self.commands.prompt = Some(self.mark_command_row());
            }
            Marker::Start => {
                self.finish_shell_command(None);
                let id = self
                    .commands
                    .prompt
                    .take()
                    .unwrap_or_else(|| self.mark_command_row());
                self.commands.marks.insert(
                    id,
                    Command {
                        started: Instant::now(),
                        result: None,
                    },
                );
                self.commands.running = Some(id);
                while self.commands.marks.len() > self.scrollback.saturating_add(1024) {
                    self.commands.marks.pop_first();
                }
            }
            Marker::End(status) => {
                self.finish_shell_command(status);
                self.commands.prompt = None;
            }
            Marker::None => {}
        }
    }

    pub(super) fn finish_shell_command(&mut self, status: Option<u8>) {
        if let Some(id) = self.commands.running.take()
            && let Some(command) = self.commands.marks.get_mut(&id)
        {
            command.result = Some((command.started.elapsed(), status));
        }
    }

    pub(crate) fn jump_command(&mut self, previous: bool) {
        if self.grid.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        let grid = self.grid.grid();
        let top = self.grid.topmost_line().0;
        let bottom = self.grid.bottommost_line().0;
        let offset = grid.display_offset() as i32;
        let current = self
            .commands
            .selected
            .and_then(|id| (top..=bottom).find(|&row| grid[Line(row)].command_mark == Some(id)));
        let anchor = current.unwrap_or_else(|| {
            if offset == 0 {
                bottom + 1
            } else {
                -offset + i32::from(previous)
            }
        });
        let mark = |row| {
            grid[Line(row)]
                .command_mark
                .filter(|id| self.commands.marks.contains_key(id))
                .map(|id| (row, id))
        };
        let target = if previous {
            (top..anchor).rev().find_map(mark)
        } else {
            ((anchor + 1)..=bottom).find_map(mark)
        };
        if let Some((row, id)) = target {
            self.commands.selected = Some(id);
            self.grid
                .scroll_display(Scroll::Delta((-row).max(0) - offset));
        } else if !previous {
            self.commands.selected = None;
            self.grid.scroll_display(Scroll::Bottom);
        }
    }

    pub(super) fn reset_command_selection(&mut self) {
        self.commands.selected = None;
    }

    pub(super) fn paint_command_mark(&self, ui: &egui::Ui, area: Rect, line: Line) -> bool {
        let Some(id) = self.grid.grid()[line].command_mark else {
            return false;
        };
        let Some(command) = self.commands.marks.get(&id) else {
            return false;
        };
        let theme = crate::theme::Theme::dark();
        let (symbol, color, detail, elapsed) = match command.result {
            Some((elapsed, Some(0))) => ("✓", theme.green, "Exit 0".into(), Some(elapsed)),
            Some((elapsed, Some(code))) => ("×", theme.red, format!("Exit {code}"), Some(elapsed)),
            Some((elapsed, None)) => (
                "?",
                theme.yellow,
                "Exit status unavailable".into(),
                Some(elapsed),
            ),
            None => ("•", theme.accent, "Running".into(), None),
        };
        let painter = ui.painter().with_clip_rect(area);
        if self.commands.selected == Some(id) {
            painter.rect_filled(area, 0.0, theme.hover);
        }
        let time = elapsed.map(super::elapsed_label);
        let label = if area.width() >= 30.0 {
            time.as_ref()
                .map_or_else(|| symbol.to_owned(), |time| format!("{symbol} {time}"))
        } else {
            symbol.into()
        };
        painter.text(
            area.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::monospace((area.height() * 0.75).clamp(7.0, 10.0)),
            color,
        );
        let detail = time.map_or_else(|| detail.clone(), |time| format!("{detail} · {time}"));
        ui.interact(
            area,
            ui.id().with(("command-mark", self.tab, id.get())),
            Sense::hover(),
        )
        .on_hover_text(detail);
        true
    }
}

#[cfg(test)]
mod tests {
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
}
