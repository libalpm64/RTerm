//! xterm mouse tracking. Applications own their input when they request
//! DECSET 1000/1002/1003; Shift remains an escape hatch for local selection.
use super::*;
use egui::{Event, Modifiers, PointerButton, Pos2};

#[derive(Default)]
pub(super) struct Mouse {
    buttons: u8,
    last_cell: Option<(usize, usize)>,
    pub(super) suppress_menu: bool,
}

fn button_code(button: PointerButton) -> Option<u8> {
    match button {
        PointerButton::Primary => Some(0),
        PointerButton::Middle => Some(1),
        PointerButton::Secondary => Some(2),
        _ => None,
    }
}

pub(super) fn report(
    mode: TermMode,
    code: u8,
    col: usize,
    row: usize,
    release: bool,
    modifiers: Modifiers,
) -> Option<Vec<u8>> {
    let flags =
        4 * u8::from(modifiers.shift) + 8 * u8::from(modifiers.alt) + 16 * u8::from(modifiers.ctrl);
    if mode.contains(TermMode::SGR_MOUSE) {
        return Some(
            format!(
                "\x1b[<{};{};{}{}",
                code + flags,
                col + 1,
                row + 1,
                if release { 'm' } else { 'M' }
            )
            .into_bytes(),
        );
    }
    let button = if release { 3 } else { code };
    let mut bytes = vec![0x1b, b'[', b'M', 32 + button + flags];
    for value in [col + 33, row + 33] {
        if mode.contains(TermMode::UTF8_MOUSE) {
            if value > 2047 {
                return None;
            }
            let mut buf = [0; 4];
            bytes.extend(
                char::from_u32(value as u32)?
                    .encode_utf8(&mut buf)
                    .as_bytes(),
            );
        } else {
            bytes.push(u8::try_from(value).ok()?);
        }
    }
    Some(bytes)
}

impl Terminal {
    pub(crate) fn context_menu<'a>(&self, response: &'a egui::Response) -> egui::Popup<'a> {
        let popup = egui::Popup::context_menu(response);
        // Preserve a Shift-right-click popup once opened, but do not open a
        // second menu over the application's own secondary-button handling.
        if self.mouse.suppress_menu {
            popup.open_memory(None)
        } else {
            popup
        }
    }

    pub(super) fn application_mouse(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        rect: Rect,
        cell: Vec2,
        backend: &Backend,
    ) -> bool {
        let mode = *self.grid.mode();
        self.mouse.suppress_menu = false;
        if !mode.intersects(TermMode::MOUSE_MODE) || self.grid.grid().display_offset() != 0 {
            self.mouse = Mouse::default();
            return false;
        }
        let modifiers = ui.input(|input| input.modifiers);
        let over = response.hovered() && ui.rect_contains_pointer(rect);
        let dragging = self.mouse.buttons != 0;
        if modifiers.shift && !dragging {
            return false;
        }
        self.mouse.suppress_menu = response.secondary_clicked() && !modifiers.shift;
        self.free_cancel_drag();
        self.drag_anchor = None;
        let size = self.size;
        let locate = |pos: Pos2| {
            let delta = pos - rect.min;
            (
                (delta.x / cell.x)
                    .floor()
                    .clamp(0.0, size.cols.saturating_sub(1) as f32) as usize,
                (delta.y / cell.y)
                    .floor()
                    .clamp(0.0, size.rows.saturating_sub(1) as f32) as usize,
            )
        };
        let mut output = Vec::new();
        let mut emit = |code, (col, row), release, mods| {
            if let Some(bytes) = report(mode, code, col, row, release, mods) {
                output.extend(bytes);
            }
        };
        for event in ui.input(|input| input.events.clone()) {
            match event {
                Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers,
                } => {
                    let Some(code) = button_code(button) else {
                        continue;
                    };
                    let bit = 1 << code;
                    let position = locate(pos);
                    if pressed
                        && !modifiers.shift
                        && rect.contains(pos)
                        && (over
                            || response.is_pointer_button_down_on()
                            || response.clicked_by(button))
                    {
                        self.mouse.buttons |= bit;
                        self.mouse.last_cell = Some(position);
                        emit(code, position, false, modifiers);
                    } else if !pressed && self.mouse.buttons & bit != 0 {
                        self.mouse.buttons &= !bit;
                        emit(code, position, true, modifiers);
                        if button == PointerButton::Secondary {
                            self.mouse.suppress_menu = true;
                        }
                    }
                }
                Event::PointerMoved(pos) => {
                    let held = self.mouse.buttons != 0;
                    if (over || held)
                        && (mode.contains(TermMode::MOUSE_MOTION)
                            || (held && mode.contains(TermMode::MOUSE_DRAG)))
                    {
                        let position = locate(pos);
                        if self.mouse.last_cell != Some(position) {
                            let code = if held {
                                self.mouse.buttons.trailing_zeros() as u8
                            } else {
                                3
                            };
                            emit(code + 32, position, false, modifiers);
                            self.mouse.last_cell = Some(position);
                        }
                    }
                }
                _ => {}
            }
        }
        // The pane may have been temporarily hidden or blocked by a modal
        // during release. Once interactive again, avoid leaving the program
        // with a permanently pressed mouse button.
        for (button, code) in [
            (PointerButton::Primary, 0),
            (PointerButton::Middle, 1),
            (PointerButton::Secondary, 2),
        ] {
            let bit = 1 << code;
            if self.mouse.buttons & bit != 0 && !ui.input(|input| input.pointer.button_down(button))
            {
                self.mouse.buttons &= !bit;
                if let Some(position) = self.mouse.last_cell {
                    emit(code, position, true, modifiers);
                }
            }
        }
        if over && !modifiers.shift {
            self.scroll_remainder += crate::theme::raw_scroll_delta(ui.ctx()).y / cell.y;
            let lines = self.scroll_remainder.trunc() as i32;
            self.scroll_remainder -= lines as f32;
            if let Some(pos) = ui.input(|input| input.pointer.hover_pos()) {
                for _ in 0..lines.unsigned_abs().min(32) {
                    emit(
                        if lines > 0 { 64 } else { 65 },
                        locate(pos),
                        false,
                        modifiers,
                    );
                }
            }
        }
        if !output.is_empty() {
            self.send(backend, output);
            self.free_type.caret = None;
        }
        // A click-drag in the application must never create a terminal text
        // selection, even when a button is released outside the viewport.
        !modifiers.shift || dragging
    }
}

#[cfg(test)]
#[path = "../../tests/unit/terminal/mouse.rs"]
mod tests;
