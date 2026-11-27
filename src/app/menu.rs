use super::*;

#[derive(Clone, Copy)]
enum MenuEntry {
    Item(&'static str, Action),
    Disabled(&'static str, Action),
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
                for entry in &mut items {
                    if let MenuEntry::Item(label, action) = *entry {
                        if matches!(action, Action::FreeType(_)) {
                            *entry = MenuEntry::Item(
                                label,
                                Action::FreeType(self.settings.on("free_type")),
                            );
                        }
                        let disabled = match action {
                            Action::SplitNew(_) => self.show_keys || !self.splits.can_split(),
                            Action::SinglePane => !self.splits.is_split(),
                            _ => false,
                        };
                        if disabled {
                            *entry = MenuEntry::Disabled(label, action);
                        }
                    }
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
            Item(
                "Split Right (New Terminal)",
                Action::SplitNew(splits::Direction::Right),
            ),
            Item(
                "Split Down (New Terminal)",
                Action::SplitNew(splits::Direction::Down),
            ),
            Item("Single Pane", Action::SinglePane),
            Item("Free Type Mode", Action::FreeType(false)),
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
        let (label, item, shortcut, enabled) = match entry {
            MenuEntry::Item(label, action) => (label, action, None, true),
            MenuEntry::Disabled(label, action) => (label, action, None, false),
            MenuEntry::Shortcut(label, action, key) => (label, action, Some(key), true),
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
        if menu_row(ui, t, label, item, shortcut, enabled).clicked() {
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
            MenuEntry::Item(label, _) | MenuEntry::Disabled(label, _) => (*label, 38.0),
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
    enabled: bool,
) -> egui::Response {
    {
        let label = crate::i18n::tr(ui, label);
        let (rect, response) = ui.allocate_exact_size(
            vec2(ui.available_width(), 28.0),
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
        let highlighted = enabled && (response.hovered() || response.has_focus());
        let fg = if !enabled {
            t.dim
        } else if highlighted {
            Color32::WHITE
        } else {
            t.text
        };
        if highlighted {
            theme::band(ui, rect, t.accent, None);
        }
        let glyph = match action {
            Action::New => Some(Icon::Add),
            Action::Connect => Some(Icon::Terminal),
            Action::Settings => Some(Icon::Menu),
            Action::Exit => Some(Icon::Close),
            Action::Copy => Some(Icon::File),
            Action::FreeType(true) => Some(Icon::Check),
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
#[path = "../../tests/unit/app/menu.rs"]
mod tests;
