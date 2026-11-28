use eframe::egui::{self, Align, Align2, FontId, Rect, RichText, Sense, Stroke, pos2, vec2};

use crate::{
    app::{App, top_band},
    models::SshKey,
    storage,
    theme::{self, Icon, Theme},
};

use super::{
    Choice, Dialog, footer,
    widgets::{combo, field_label, form_field, toggle},
};

#[derive(Default)]
pub(crate) struct KeyManager {
    pub(crate) open: bool,
    keys: Vec<DiskKey>,
    selected: Option<usize>,
}

#[derive(Clone)]
struct DiskKey {
    name: String,
    path: String,
    algorithm: String,
    public: String,
}

impl KeyManager {
    pub(crate) fn refresh(&mut self) {
        self.keys = storage::list_keys()
            .into_iter()
            .map(|key| DiskKey {
                name: json_text(&key, "name"),
                path: json_text(&key, "path"),
                algorithm: json_text(&key, "type"),
                public: json_text(&key, "public_key"),
            })
            .collect();
        self.selected = None;
    }
}

fn json_text(value: &serde_json::Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().into()
}

pub(crate) enum KeyDialog {
    Generate {
        name: String,
        algorithm: String,
        passphrase: String,
        comment: String,
    },
    Import {
        name: String,
        private: String,
        public: String,
    },
    Delete {
        name: String,
        path: Option<String>,
    },
}

impl KeyDialog {
    pub(super) fn title(&self) -> &'static str {
        match self {
            Self::Generate { .. } => "Generate SSH Key",
            Self::Import { .. } => "Import SSH Key",
            Self::Delete { .. } => "Delete SSH Key",
        }
    }
}

enum KeyAction {
    Generate,
    Import,
    Refresh,
}

impl App {
    pub(crate) fn open_key_generator(&mut self) {
        self.dialog = Some(Dialog::Key(KeyDialog::Generate {
            name: String::new(),
            algorithm: "ed25519".into(),
            passphrase: String::new(),
            comment: String::new(),
        }));
    }

    pub(crate) fn open_key_import(&mut self) {
        self.dialog = Some(Dialog::Key(KeyDialog::Import {
            name: String::new(),
            private: String::new(),
            public: String::new(),
        }));
    }

    pub(crate) fn key_panel(&mut self, ui: &mut egui::Ui, body: Rect) {
        let t = self.theme;
        theme::band(ui, body, t.bg, None);
        let toolbar = top_band(body, 30.0);
        self.key_toolbar(ui, toolbar);

        let content = body.with_min_y(toolbar.bottom());
        // Keep the detail pane usable when both application sidebars are open on a narrow window.
        let (list, details) = if content.width() >= 460.0 {
            let right = content.left() + (content.width() * 0.32).clamp(164.0, 205.0);
            (content.with_max_x(right), content.with_min_x(right + 1.0))
        } else {
            let bottom = content.top() + (content.height() * 0.32).clamp(90.0, 150.0);
            (content.with_max_y(bottom), content.with_min_y(bottom + 1.0))
        };
        let selected = self.key_list(ui, list);
        if content.width() >= 460.0 {
            ui.painter().vline(
                list.right() + 0.5,
                list.y_range(),
                Stroke::new(1.0_f32, t.border),
            );
        } else {
            ui.painter().hline(
                list.x_range(),
                list.bottom() + 0.5,
                Stroke::new(1.0_f32, t.border),
            );
        }

        if let Some(key) = selected {
            let mut pane = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt("key-details")
                    .max_rect(details.shrink(12.0)),
            );
            pane.set_clip_rect(details);
            egui::ScrollArea::vertical().show(&mut pane, |ui| self.key_details(ui, key));
        } else {
            theme::label(
                ui,
                details.shrink(14.0),
                crate::i18n::tr(ui, "Select, generate or import an SSH key."),
                t.chrome_font(12.0),
                t.muted,
                Align2::CENTER_CENTER,
            );
        }
    }

    fn key_toolbar(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let t = self.theme;
        theme::band(ui, rect, t.bar, Some(t.border));
        let full_labels = rect.width() >= 410.0;
        let mut x = rect.left() + 6.0;
        for (index, (action, label, icon)) in [
            (KeyAction::Generate, "Generate", Icon::Add),
            (KeyAction::Import, "Import", Icon::Upload),
            (KeyAction::Refresh, "Refresh Keys", Icon::Refresh),
        ]
        .into_iter()
        .enumerate()
        {
            let title = crate::i18n::tr(ui, label);
            let labeled = full_labels && index < 2;
            let font = t.chrome_font(12.0);
            let width = if labeled {
                theme::text_width(ui, title, font.clone()) + 46.0
            } else {
                28.0
            };
            if index != 0 {
                ui.painter().vline(
                    x + 3.0,
                    (rect.top() + 6.0)..=(rect.bottom() - 6.0),
                    Stroke::new(1.0_f32, t.border),
                );
                x += 10.0;
            }
            let button =
                Rect::from_center_size(pos2(x + width / 2.0, rect.center().y), vec2(width, 24.0));
            x += width;
            let clicked = if labeled {
                let response = ui.interact(
                    button,
                    ui.scope_id().with(("key-action", label)),
                    Sense::click(),
                );
                if response.hovered() {
                    theme::band(ui, button, t.hover, None);
                }
                theme::icon(
                    ui,
                    Rect::from_center_size(
                        pos2(button.left() + 14.0, button.center().y),
                        vec2(14.0, 14.0),
                    ),
                    icon,
                    t.accent,
                );
                theme::label(
                    ui,
                    button
                        .with_min_x(button.left() + 29.0)
                        .with_max_x(button.right() - 12.0),
                    title,
                    font,
                    t.text,
                    Align2::LEFT_CENTER,
                );
                response.clicked()
            } else {
                theme::icon_button(ui, button, label, icon, Theme { dim: t.muted, ..t }).clicked()
            };
            if clicked {
                match action {
                    KeyAction::Generate => self.open_key_generator(),
                    KeyAction::Import => self.open_key_import(),
                    KeyAction::Refresh => self.keys.refresh(),
                }
            }
        }
        let toggle_rect =
            Rect::from_center_size(pos2(rect.right() - 14.0, rect.center().y), vec2(18.0, 18.0));
        let caption = rect
            .with_min_x(toggle_rect.left() - 72.0)
            .with_max_x(toggle_rect.left() - 9.0);
        theme::label(
            ui,
            caption,
            crate::i18n::tr(ui, "SSH Agent"),
            t.chrome_font(11.0),
            t.muted,
            Align2::RIGHT_CENTER,
        );
        let mut control = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("key-agent")
                .max_rect(toggle_rect),
        );
        let mut agent = self.settings.on("ssh_agent");
        if toggle(&mut control, "SSH Agent", &mut agent, t).changed() {
            self.settings
                .set("ssh_agent", if agent { "1" } else { "0" }.into());
            if let Err(error) = self.settings.save() {
                self.fail(error);
            }
        }
    }

    fn key_list(&mut self, ui: &mut egui::Ui, rect: Rect) -> Option<DiskKey> {
        let t = self.theme;
        theme::band(ui, rect, t.panel, None);
        let header = top_band(rect, 23.0);
        theme::band(ui, header, t.bar, Some(t.border));
        theme::label(
            ui,
            header.shrink2(vec2(12.0, 0.0)),
            crate::i18n::tr(ui, "Keys"),
            t.chrome_font(11.0),
            t.muted,
            Align2::LEFT_CENTER,
        );
        let bounds = rect.with_min_y(header.bottom());
        let mut list = ui.new_child(egui::UiBuilder::new().id_salt("key-list").max_rect(bounds));
        list.set_clip_rect(bounds);

        let disk_count = self.keys.keys.len();
        let count = disk_count + self.vault_keys.len();
        if count == 0 {
            list.add_space(5.0);
            list.label(
                RichText::new(crate::i18n::tr(&list, "No SSH keys"))
                    .size(11.0)
                    .color(t.muted),
            );
            return None;
        }

        let mut selected = self
            .keys
            .selected
            .filter(|index| *index < count)
            .unwrap_or(0);
        let entries = self
            .keys
            .keys
            .iter()
            .map(|key| (&key.name, "On disk"))
            .chain(self.vault_keys.iter().map(|key| (&key.name, "In vault")));
        theme::compact_list(bounds.height()).show(&mut list, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (index, (name, location)) in entries.enumerate() {
                let (row, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
                if response.clicked() {
                    selected = index;
                }
                let current = index == selected;
                response.widget_info(|| {
                    egui::WidgetInfo::selected(egui::Role::ListBoxOption, true, current, name)
                });
                if current || response.hovered() || response.has_focus() {
                    ui.painter().rect_filled(row, 0.0, t.hover);
                }
                if current {
                    let marker = Rect::from_min_size(
                        pos2(row.left(), row.top() + 6.0),
                        vec2(2.0, row.height() - 12.0),
                    );
                    ui.painter().rect_filled(marker, 1.0, t.accent);
                }
                let glyph = Rect::from_center_size(
                    pos2(row.left() + 17.0, row.center().y),
                    vec2(15.0, 15.0),
                );
                theme::icon(
                    ui,
                    glyph,
                    Icon::Key,
                    if current { t.accent } else { t.muted },
                );
                let text = row
                    .with_min_x(row.left() + 32.0)
                    .with_max_x(row.right() - 8.0);
                theme::label(
                    ui,
                    top_band(text, 18.0),
                    name,
                    t.chrome_font(12.0),
                    t.text,
                    Align2::LEFT_CENTER,
                );
                theme::label(
                    ui,
                    text.with_min_y(text.top() + 17.0),
                    crate::i18n::tr(ui, location),
                    t.chrome_font(10.0),
                    t.muted,
                    Align2::LEFT_CENTER,
                );
                response.on_hover_text(name);
            }
        });
        self.keys.selected = Some(selected);
        if selected < disk_count {
            self.keys.keys.get(selected).cloned()
        } else {
            self.vault_keys
                .get(selected - disk_count)
                .map(|key| DiskKey {
                    name: key.name.clone(),
                    algorithm: key.key_type.clone(),
                    public: key.public_key.clone(),
                    path: String::new(),
                })
        }
    }

    fn key_details(&mut self, ui: &mut egui::Ui, key: DiskKey) {
        let t = self.theme;
        ui.horizontal(|ui| {
            let (glyph, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
            theme::icon(ui, glyph.shrink(5.0), Icon::Key, t.blue);
            ui.vertical(|ui| {
                ui.add(
                    egui::Label::new(RichText::new(&key.name).font(t.chrome_font(14.0))).truncate(),
                )
                .on_hover_text(&key.name);
                let location = if key.path.is_empty() {
                    crate::i18n::tr(ui, "Encrypted vault")
                } else {
                    &key.path
                };
                ui.add(
                    egui::Label::new(RichText::new(location).size(11.0).color(t.muted)).truncate(),
                )
                .on_hover_text(location);
            });
        });

        ui.add_space(8.0);
        ui.separator();
        key_metadata(ui, t, "Type", &key.algorithm, false);
        if let Ok(public) = ssh_key::PublicKey::from_openssh(&key.public) {
            key_metadata(
                ui,
                t,
                "Fingerprint",
                &public.fingerprint(ssh_key::HashAlg::Sha256).to_string(),
                true,
            );
        }

        ui.add_space(15.0);
        ui.label(
            RichText::new(crate::i18n::tr(ui, "PUBLIC KEY"))
                .font(t.chrome_font(11.0))
                .color(t.blue),
        );
        ui.add_space(3.0);
        if key.public.is_empty() {
            ui.label(RichText::new(crate::i18n::tr(ui, "No public key available")).color(t.muted));
        } else {
            egui::Frame::new()
                .fill(t.panel)
                .stroke(Stroke::new(1.0_f32, t.border))
                .corner_radius(0)
                .inner_margin(8)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    let mut text = egui::text::LayoutJob::simple(
                        key.public.clone(),
                        FontId::monospace(11.0),
                        t.text,
                        ui.available_width(),
                    );
                    text.wrap.break_anywhere = true;
                    ui.add(egui::Label::new(text).selectable(true).wrap());
                });
        }
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(12.0, 8.0);
            ui.spacing_mut().button_padding = vec2(10.0, 4.0);
            if ui
                .add_enabled(
                    !key.public.is_empty(),
                    egui::Button::new(
                        RichText::new(crate::i18n::tr(ui, "Copy Public Key"))
                            .font(t.chrome_font(12.0)),
                    )
                    .min_size(vec2(0.0, 26.0)),
                )
                .clicked()
            {
                ui.ctx().copy_text(key.public);
            }
            if ui
                .add(
                    egui::Button::new(
                        RichText::new(crate::i18n::tr(ui, "Delete Key"))
                            .font(t.chrome_font(12.0))
                            .color(t.red),
                    )
                    .min_size(vec2(0.0, 26.0)),
                )
                .clicked()
            {
                self.dialog = Some(Dialog::Key(KeyDialog::Delete {
                    name: key.name,
                    path: (!key.path.is_empty()).then_some(key.path),
                }));
            }
        });
    }

    pub(super) fn key_dialog(&mut self, ui: &mut egui::Ui, dialog: &mut KeyDialog) -> bool {
        let t = self.theme;
        let (primary, danger) = match dialog {
            KeyDialog::Generate {
                name,
                algorithm,
                passphrase,
                comment,
            } => {
                form_field(ui, t, "Key Name", name, "my_key", false);
                ui.add_space(14.0);
                let label = field_label(ui, t, "Key Type");
                combo(
                    ui,
                    "generate-key-algorithm",
                    algorithm,
                    &[
                        ("ed25519", "Ed25519"),
                        ("rsa-2048", "RSA 2048"),
                        ("rsa-4096", "RSA 4096"),
                        ("ecdsa-p256", "ECDSA P-256"),
                        ("ecdsa-p384", "ECDSA P-384"),
                        ("ecdsa-p521", "ECDSA P-521"),
                    ],
                )
                .labelled_by(label.id);
                ui.add_space(14.0);
                form_field(
                    ui,
                    t,
                    "Passphrase",
                    passphrase,
                    "Enter passphrase (optional)",
                    true,
                );
                ui.add_space(14.0);
                form_field(ui, t, "Comment", comment, "user@hostname", false);
                ("Generate", false)
            }
            KeyDialog::Import {
                name,
                private,
                public,
            } => {
                form_field(ui, t, "Key Name", name, "my_server_key", false);
                ui.add_space(14.0);
                let header = ui
                    .horizontal(|ui| {
                        let label = ui.label(
                            RichText::new(crate::i18n::tr(ui, "Private Key"))
                                .size(11.0)
                                .color(t.muted),
                        );
                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                            let browse = ui.button(crate::i18n::tr(ui, "Browse…")).clicked();
                            (label, browse)
                        })
                        .inner
                    })
                    .inner;
                let (label, browse) = header;
                if browse && let Some(path) = rfd::FileDialog::new().pick_file() {
                    match std::fs::read_to_string(&path) {
                        Ok(content) => {
                            *private = content;
                            if name.is_empty() {
                                *name = path
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .into_owned();
                            }
                        }
                        Err(error) => self.fail(error),
                    }
                }
                ui.add_space(5.0);
                ui.add(
                    egui::TextEdit::multiline(private)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(6)
                        .margin(egui::Margin::same(10))
                        .desired_width(f32::INFINITY),
                )
                .labelled_by(label.id);
                ui.add_space(14.0);
                form_field(
                    ui,
                    t,
                    "Public Key (Optional)",
                    public,
                    "ssh-ed25519 AAAA...",
                    false,
                );
                ("Import", false)
            }
            KeyDialog::Delete { name, .. } => {
                ui.label(crate::i18n::format(
                    ui,
                    "Permanently delete the key “{name}”?",
                    &[("name", name.as_str())],
                ));
                ("Delete", true)
            }
        };
        match footer(ui, t, Some("Cancel"), primary, danger) {
            Choice::Cancel => true,
            Choice::Yes => {
                let result = match dialog {
                    KeyDialog::Generate {
                        name,
                        algorithm,
                        passphrase,
                        comment,
                    } => {
                        let default =
                            format!("id_{}", algorithm.split('-').next().unwrap_or("ed25519"));
                        let name = if name.trim().is_empty() {
                            &default
                        } else {
                            name.trim()
                        };
                        storage::make_key(name, algorithm, passphrase, comment)
                            .and_then(|key| self.persist_key(key))
                    }
                    KeyDialog::Import {
                        name,
                        private,
                        public,
                    } => storage::parse_key(name.trim(), private, public)
                        .and_then(|key| self.persist_key(key)),
                    KeyDialog::Delete {
                        path: Some(path), ..
                    } => storage::delete_key(path),
                    KeyDialog::Delete { name, path: None } => {
                        let previous = self.vault_keys.clone();
                        self.vault_keys.retain(|key| &key.name != name);
                        if let Err(error) = self.save_profiles() {
                            self.vault_keys = previous;
                            Err(error)
                        } else {
                            Ok(())
                        }
                    }
                };
                match result {
                    Ok(()) => {
                        self.keys.refresh();
                        true
                    }
                    Err(error) => {
                        self.fail(error);
                        false
                    }
                }
            }
            Choice::None => false,
        }
    }

    fn persist_key(&mut self, key: SshKey) -> Result<(), String> {
        if self.vault_password.is_none() {
            return storage::write_key(&key);
        }
        if self
            .vault_keys
            .iter()
            .any(|existing| existing.name == key.name)
        {
            return Err("A key with that name already exists".into());
        }
        self.vault_keys.push(key);
        if let Err(error) = self.save_profiles() {
            self.vault_keys.pop();
            return Err(error);
        }
        Ok(())
    }
}

fn key_metadata(ui: &mut egui::Ui, t: Theme, label: &str, value: &str, monospace: bool) {
    ui.horizontal(|ui| {
        let label = crate::i18n::tr(ui, label);
        let width = theme::text_width(ui, label, t.chrome_font(11.0)).max(78.0);
        let (rect, _) = ui.allocate_exact_size(vec2(width, 18.0), Sense::hover());
        theme::label(
            ui,
            rect,
            label,
            t.chrome_font(11.0),
            t.muted,
            Align2::LEFT_CENTER,
        );
        let text = RichText::new(value).size(11.0);
        let text = if monospace { text.monospace() } else { text };
        ui.add(egui::Label::new(text).wrap().selectable(true));
    });
}
