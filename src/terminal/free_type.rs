//! Mouse editing is limited to a known, echoed, single-line command. A PTY
//! has no generic API for editing another program's buffer; never write to
//! the screen grid or guess a cursor movement through arbitrary output.
use super::*;

#[derive(Default)]
pub(super) struct FreeType {
    pub(super) caret: Option<usize>,
    drag: Option<SelectionDrag>,
}

struct SelectionDrag {
    text: String,
    range: Range<usize>,
}

#[derive(Clone, Copy)]
pub(super) struct EditableLine {
    row: Line,
    start: usize,
    pub(super) caret: usize,
    len: usize,
}

impl EditableLine {
    fn target(self, point: Point, side: Side) -> Option<usize> {
        (point.line == self.row).then(|| {
            (point.column.0 + usize::from(side == Side::Right))
                .saturating_sub(self.start)
                .min(self.len)
        })
    }

    fn selection(
        self,
        selection: alacritty_terminal::selection::SelectionRange,
    ) -> Option<Range<usize>> {
        if selection.is_block
            || selection.start.line != self.row
            || selection.end.line != self.row
            || selection.start.column.0 < self.start
            || selection.end.column.0 >= self.start + self.len
        {
            return None;
        }
        Some(selection.start.column.0 - self.start..selection.end.column.0 - self.start + 1)
    }
}

pub(super) fn plain_text(text: &str) -> bool {
    text.bytes().all(|byte| (b' '..=b'~').contains(&byte))
}

fn move_caret(bytes: &mut Vec<u8>, from: usize, to: usize, mode: TermMode, kind: Kind) {
    let key = if from > to {
        Key::ArrowLeft
    } else {
        Key::ArrowRight
    };
    if let Some(sequence) = key_bytes(key, egui::Modifiers::NONE, mode, kind) {
        bytes.extend(sequence.repeat(from.abs_diff(to)));
    }
}

impl Terminal {
    pub(super) fn editable_line(&self) -> Option<EditableLine> {
        let mode = *self.grid.mode();
        let grid = self.grid.grid();
        let text = &self.input_line;
        // Bracketed paste is a useful indication of an interactive line
        // editor, not proof of a shell. Only edit a command we actually typed
        // and can verify on screen. Unknown applications/output are untouched.
        if self.closed
            || !mode.contains(TermMode::BRACKETED_PASTE)
            || mode.intersects(TermMode::ALT_SCREEN | TermMode::MOUSE_MODE)
            || grid.display_offset() != 0
            || text.is_empty()
            || text.len() >= 4096
            || !mode.contains(TermMode::SHOW_CURSOR)
            || !plain_text(text)
            || text.trim_end().is_empty()
            || grid.cursor.input_needs_wrap
            || !self.pending_input.borrow().is_empty()
        {
            return None;
        }
        let point = grid.cursor.point;
        let row = &grid[point.line];
        let end =
            (0..usize::from(self.size.cols)).rfind(|&column| row[Column(column)].c != ' ')? + 1;
        let start = end.checked_sub(text.trim_end_matches(' ').len())?;
        let limit = start.checked_add(text.len())?;
        if limit >= usize::from(self.size.cols)
            || point.column.0 < start
            || point.column.0 > limit
            || self
                .free_type
                .caret
                .is_some_and(|caret| caret != point.column.0 - start)
            || !text.bytes().enumerate().all(|(i, byte)| {
                let cell = &row[Column(start + i)];
                cell.c == char::from(byte)
                    && !cell.flags.intersects(
                        Flags::HIDDEN
                            | Flags::WIDE_CHAR
                            | Flags::WIDE_CHAR_SPACER
                            | Flags::WRAPLINE,
                    )
                    && cell.zerowidth().is_none()
            })
        {
            return None;
        }
        Some(EditableLine {
            row: point.line,
            start,
            caret: point.column.0 - start,
            len: text.len(),
        })
    }

    pub(super) fn free_click(&mut self, point: Point, side: Side, backend: &Backend) {
        let Some(line) = self.editable_line() else {
            return;
        };
        let Some(target) = line.target(point, side) else {
            return;
        };
        let mut bytes = Vec::new();
        move_caret(&mut bytes, line.caret, target, *self.grid.mode(), self.kind);
        if bytes.is_empty() || self.send(backend, bytes) {
            self.free_type.caret = Some(target);
        }
    }

    pub(super) fn free_drag_begin(&mut self, point: Point) -> bool {
        self.free_type.drag = None;
        let Some(line) = self.editable_line() else {
            return false;
        };
        let Some(selection) = self
            .grid
            .selection
            .as_ref()
            .and_then(|s| s.to_range(&self.grid))
        else {
            return false;
        };
        if !selection.contains(point) {
            return false;
        }
        let Some(range) = line.selection(selection) else {
            return false;
        };
        self.free_type.drag = Some(SelectionDrag {
            text: self.input_line.clone(),
            range,
        });
        true
    }

    pub(super) fn free_dragging(&self) -> bool {
        self.free_type.drag.is_some()
    }

    pub(super) fn free_cancel_drag(&mut self) {
        self.free_type.drag = None;
    }

    pub(super) fn free_drag_drop(&mut self, point: Point, side: Side, backend: &Backend) {
        let Some(drag) = self.free_type.drag.take() else {
            return;
        };
        let Some(line) = self.editable_line() else {
            return;
        };
        let Some(target) = line.target(point, side) else {
            return;
        };
        if drag.text != self.input_line || (drag.range.start..=drag.range.end).contains(&target) {
            return;
        }
        let mode = *self.grid.mode();
        let mut bytes = Vec::new();
        let count = drag.range.len();
        let mut updated = drag.text;
        let moved = updated[drag.range.clone()].to_owned();
        move_caret(&mut bytes, line.caret, drag.range.end, mode, self.kind);
        bytes.extend(std::iter::repeat_n(0x7f, count));
        updated.replace_range(drag.range.clone(), "");
        let destination = if target > drag.range.end {
            target - count
        } else {
            target
        };
        move_caret(&mut bytes, drag.range.start, destination, mode, self.kind);
        // All bytes are printable ASCII; never paste control characters or a
        // newline from scrollback into the shell.
        bytes.extend(self.paste_bytes(&moved));
        updated.insert_str(destination, &moved);
        if self.send(backend, bytes) {
            self.input_line = updated;
            self.free_type.caret = Some(destination + count);
            self.grid.selection = None;
        }
    }

    pub(super) fn free_delete_selection(&mut self, bytes: &mut Vec<u8>) -> Option<usize> {
        let line = self.editable_line()?;
        let selection = self.grid.selection.as_ref()?.to_range(&self.grid)?;
        let range = line.selection(selection)?;
        move_caret(bytes, line.caret, range.end, *self.grid.mode(), self.kind);
        bytes.extend(std::iter::repeat_n(0x7f, range.len()));
        self.input_line.replace_range(range.clone(), "");
        self.grid.selection = None;
        Some(range.start)
    }
}

#[cfg(test)]
#[path = "../../tests/unit/terminal/free_type.rs"]
mod tests;
