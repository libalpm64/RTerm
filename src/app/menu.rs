use super::*;

#[derive(Clone, Copy)]
enum MenuEntry {
    Item(&'static str, Action),
    Shortcut(&'static str, Action, Key),
    Divider,
}

impl App {
    pub(super) fn menu_bar(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let t = self.theme;
        theme::band(ui, rect, t.bg, Some(t.border));
        let mut x = rect.left();
        let mut action = None;
        for name in ["File", "Edit", "View", "Session", "SSH Keys", "Help", "AI"] {
            let title = crate::i18n::tr(ui, name);
            let font = t.chrome_font(12.0);
            let width = theme::text_width(ui, title, font.clone()) + 14.0;
            let inset = 1.0;
            let bounds = Rect::from_min_size(
                pos2(x, rect.top() + inset),
                vec2(width, rect.height() - inset * 2.0),
            );
            x += width + 0.0;
            let response = ui.interact(bounds, ui.id().with(("menu", name)), Sense::click());
            response
                .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, title));
            let popup = styled_menu(egui::Popup::menu(&response), t).show(|ui| {
                let mut items = menu_entries(name, self.agent.active()).to_vec();
                if name == "AI" && self.agent.active() && self.tunnel_profile.is_some() {
                    let label = if self.openai_tunnel.is_some() {
                        "Stop OpenAI Tunnel"
                    } else {
                        "Connect OpenAI Tunnel"
                    };
                    items.push(MenuEntry::Item(label, Action::AiTunnelToggle));
                }
                if name == "Session" && self.settings.on("vault_enabled") {
                    items.push(MenuEntry::Item("Lock", Action::Lock));
                }
                action = dropdown(ui, t, &items);
            });

            let highlighted = popup.is_some() || response.hovered() || response.has_focus();
            if highlighted {
                ui.painter().rect_filled(bounds, 0.0, t.accent);
            }
            theme::label(
                ui,
                bounds,
                title,
                font,
                if highlighted {
                    Color32::WHITE
                } else if name == "AI" && self.agent.active() {
                    if self.settings.get("ai_access") == "unrestricted" {
                        t.yellow
                    } else {
                        t.green
                    }
                } else {
                    t.muted
                },
                Align2::CENTER_CENTER,
            );
        }
        if let Some(action) = action {
            self.run_action(action, ui.ctx());
        }
    }
}

fn menu_entries(name: &str, agent_active: bool) -> &'static [MenuEntry] {
    use MenuEntry::{Divider, Item, Shortcut};
    match name {
        "File" => &[
            Shortcut("New Session", Action::New, Key::N),
            Item("Connect…", Action::Connect),
            Divider,
            Item("Settings", Action::Settings),
            Divider,
            Item("Exit", Action::Exit),
        ],
        "Edit" => &[
            Item("Copy", Action::Copy),
            Item("Paste", Action::Paste),
            Item("Select All", Action::SelectAll),
            Divider,
            Item("Clear Terminal", Action::Clear),
        ],
        "View" => &[
            Item("Toggle Explorer", Action::Explorer),
            Item("Toggle Sessions", Action::Sessions),
            Item("Bottom Shell", Action::BottomShell),
            Divider,
            Shortcut("Zoom In", Action::Zoom(1.0), Key::Plus),
            Shortcut("Zoom Out", Action::Zoom(-1.0), Key::Minus),
            Shortcut("Reset Zoom", Action::ResetZoom, Key::Num0),
        ],
        "Session" => &[
            Item("Previous Command", Action::JumpCommand(true)),
            Item("Next Command", Action::JumpCommand(false)),
            Divider,
            Item("Clear Terminal", Action::Clear),
            Item("Reset Font Size", Action::ResetZoom),
        ],
        "SSH Keys" => &[
            Item("Manage SSH Keys", Action::Keys),
            Item("Known Hosts…", Action::KnownHosts),
            Divider,
            Item("Generate SSH Key…", Action::GenerateKey),
            Item("Import SSH Key…", Action::ImportKey),
        ],
        "AI" if agent_active => &[
            Item("Stop Agent", Action::AiRevoke),
            Item("Copy Agent Instructions", Action::AiCopyPrompt),
            Divider,
            Item("AI Activity…", Action::AiActivity),
            Item("AI Settings…", Action::AiSettings),
        ],
        "AI" => &[
            Item("Start Agent", Action::AiStart),
            Divider,
            Item("AI Activity…", Action::AiActivity),
            Item("AI Settings…", Action::AiSettings),
        ],
        _ => &[
            Item("About Rterm", Action::About),
            Item("Documentation", Action::Docs),
            Item("Report Issue", Action::Issue),
        ],
    }
}

pub(super) fn terminal_menu(popup: egui::Popup<'_>, theme: Theme) -> Option<Action> {
    use MenuEntry::{Divider, Item};

    styled_menu(popup, theme)
        .show(|ui| {
            dropdown(
                ui,
                theme,
                &[
                    Item("Copy", Action::Copy),
                    Item("Paste", Action::Paste),
                    Divider,
                    Item("Previous Command", Action::JumpCommand(true)),
                    Item("Next Command", Action::JumpCommand(false)),
                    Divider,
                    Item("Clear Terminal", Action::Clear),
                    Item("Reset Font Size", Action::ResetZoom),
                ],
            )
        })
        .and_then(|popup| popup.inner)
}

fn styled_menu(popup: egui::Popup<'_>, t: Theme) -> egui::Popup<'_> {
    {
        popup.width(270.0).frame(
            egui::Frame::new()
                .fill(t.bar)
                .stroke(Stroke::new(1.0_f32, t.border))
                .corner_radius(0)
                .inner_margin(0),
        )
    }
}

fn dropdown(ui: &mut egui::Ui, t: Theme, entries: &[MenuEntry]) -> Option<Action> {
    let width = menu_width(ui, t, entries);
    ui.set_min_width(width);
    ui.spacing_mut().item_spacing.y = 0.0;
    ui.spacing_mut().button_padding = vec2(10.0, 3.0);
    // Use the popup's left-aligned layout. add_sized would center the label in each row.
    let mut action = None;
    for &entry in entries {
        let (label, item, shortcut) = match entry {
            MenuEntry::Item(label, action) => (label, action, None),
            MenuEntry::Shortcut(label, action, key) => (label, action, Some(key)),
            MenuEntry::Divider => {
                let (line, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                ui.painter().hline(
                    line.shrink2(vec2(0.0, 0.0)).x_range(),
                    line.center().y,
                    Stroke::new(1.0_f32, t.border),
                );
                continue;
            }
        };
        if menu_row(ui, t, label, item, shortcut).clicked() {
            action = Some(item);
            ui.close();
        }
    }
    action
}

fn menu_width(ui: &mut egui::Ui, t: Theme, entries: &[MenuEntry]) -> f32 {
    let mut width = 270.0_f32;
    for entry in entries {
        let (label, reserve) = match entry {
            MenuEntry::Item(label, _) => (*label, 38.0),
            MenuEntry::Shortcut(label, _, _) => (*label, 116.0),
            MenuEntry::Divider => continue,
        };
        let translated = crate::i18n::tr(ui, label);
        let measured = theme::text_width(ui, translated, t.chrome_font(12.0));
        width = width.max(measured + reserve + 4.0);
    }
    width.min(ui.ctx().content_rect().width().max(1.0))
}

fn menu_row(
    ui: &mut egui::Ui,
    t: Theme,
    label: &str,
    action: Action,
    shortcut: Option<Key>,
) -> egui::Response {
    {
        let label = crate::i18n::tr(ui, label);
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
        let highlighted = response.hovered() || response.has_focus();
        let fg = if highlighted { Color32::WHITE } else { t.text };
        if highlighted {
            theme::band(ui, rect, t.accent, None);
        }
        let glyph = match action {
            Action::New => Some(Icon::Add),
            Action::Connect => Some(Icon::Terminal),
            Action::Settings => Some(Icon::Menu),
            Action::Exit => Some(Icon::Close),
            Action::Copy => Some(Icon::File),
            Action::Paste => Some(Icon::Upload),
            Action::Clear => Some(Icon::Trash),
            Action::Keys => Some(Icon::Key),
            Action::GenerateKey => Some(Icon::Add),
            Action::ImportKey => Some(Icon::Upload),
            Action::Zoom(_) => Some(Icon::Zoom),
            _ => None,
        };
        if let Some(glyph) = glyph {
            theme::icon(
                ui,
                Rect::from_center_size(pos2(rect.left() + 13.0, rect.center().y), vec2(14.0, 14.0)),
                glyph,
                fg,
            );
        }
        let shortcut_text = shortcut.map(|key| {
            ui.ctx()
                .format_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, key))
        });
        theme::label(
            ui,
            rect.with_min_x(rect.left() + 28.0)
                .with_max_x(rect.right() - if shortcut_text.is_some() { 88.0 } else { 10.0 }),
            label,
            t.chrome_font(12.0),
            fg,
            Align2::LEFT_CENTER,
        );
        if let Some(text) = shortcut_text {
            theme::label(
                ui,
                rect.with_min_x(rect.right() - 83.0)
                    .with_max_x(rect.right() - 9.0),
                &text,
                t.chrome_font(12.0),
                fg,
                Align2::LEFT_CENTER,
            );
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, PointerButton, RawInput, Shape};

    #[test]
    fn terminal_context_menu_aligns_actions_and_uses_the_full_row() {
        for locale in ["en", "de", "ru"] {
            check_terminal_context_menu(locale);
        }
    }

    fn check_terminal_context_menu(locale: &'static str) {
        let context = egui::Context::default();
        crate::i18n::set(&context, locale);
        theme::install_fonts(&context);
        let theme = Theme::dark();
        theme.apply(&context);
        let mut selected = None;
        let mut frame = |events| {
            context.run(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(640.0, 480.0))),
                    events,
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        let response = ui.allocate_response(vec2(500.0, 400.0), Sense::click());
                        if let Some(action) =
                            terminal_menu(egui::Popup::context_menu(&response), theme)
                        {
                            selected = Some(action);
                        }
                    });
                },
            )
        };
        let pointer = |pos, button, pressed| Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let anchor = pos2(120.0, 140.0);
        frame(vec![Event::PointerMoved(anchor)]);
        frame(vec![pointer(anchor, PointerButton::Secondary, true)]);
        frame(vec![pointer(anchor, PointerButton::Secondary, false)]);
        let popup = frame(vec![]);

        let rows = ["Copy", "Paste", "Clear Terminal", "Reset Font Size"].map(|label| {
            let label = crate::i18n::text(&context, label);
            popup
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    Shape::Text(text) if text.galley.text() == label => {
                        let rect = Rect::from_min_size(text.pos, text.galley.size());
                        assert!(
                            rect.right() <= shape.clip_rect.right() + 1.0,
                            "{locale}: {label} clipped"
                        );
                        Some(rect)
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{label} is missing from the terminal context menu"))
        });
        assert!(
            rows.iter()
                .all(|row| (row.left() - rows[0].left()).abs() < 1.0),
            "menu rows are not aligned: {rows:?}"
        );

        let hit = pos2(rows[3].left() + 145.0, rows[3].center().y);
        frame(vec![Event::PointerMoved(hit)]);
        frame(vec![pointer(hit, PointerButton::Primary, true)]);
        frame(vec![pointer(hit, PointerButton::Primary, false)]);
        assert!(matches!(selected, Some(Action::ResetZoom)));
    }
}
