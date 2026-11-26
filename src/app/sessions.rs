use super::*;

impl App {
    pub(super) fn session_panel(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let t = self.theme;
        theme::band(ui, rect, t.panel, None);

        let header = top_band(rect, 22.0);
        header_label(ui, header, "Session", t);
        if theme::icon_button(ui, icon_slot(header, 0), "Add Session", Icon::Add, t).clicked() {
            self.show_connect();
        }
        let search = compact_filter(
            ui,
            rect.with_min_y(header.bottom()),
            &mut self.filter,
            "saved-session-filter",
            t,
        );
        let header_height = 22.0;
        let history_height = if self.history_visible {
            (rect.height() * 0.18)
                .min(108.0)
                .min((rect.bottom() - search.bottom() - header_height).max(0.0))
        } else {
            0.0
        };
        let history_header = rect
            .with_min_y(rect.bottom() - history_height - header_height)
            .with_max_y(rect.bottom() - history_height);
        let list = rect
            .with_min_y(search.bottom())
            .with_max_y(history_header.top());
        self.saved_sessions(ui, list);
        self.history_panel(ui, history_header, rect.bottom());
    }

    fn saved_sessions(&mut self, ui: &mut egui::Ui, list: Rect) {
        let t = self.theme;

        let filter = self.filter.to_lowercase();
        let mut connect = None;
        let mut edit = None;
        let mut delete = None;
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("saved-sessions")
                .max_rect(list),
        );
        child.set_clip_rect(list);
        theme::compact_list(list.height()).show(&mut child, |ui| {
            let mut matches = 0;
            ui.spacing_mut().item_spacing.y = 0.0;
            for (index, profile) in self.profiles.iter().enumerate() {
                let detail = profile.subtitle();
                if !profile.name.to_lowercase().contains(&filter)
                    && !detail.to_lowercase().contains(&filter)
                {
                    continue;
                }
                matches += 1;
                let height = 20.0;
                let (row, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
                let response = response.on_hover_text(&detail);
                if response.contains_pointer() {
                    theme::band(ui, row, t.hover, None);
                }
                theme::icon(
                    ui,
                    Rect::from_center_size(
                        pos2(row.left() + 16.0, row.center().y),
                        vec2(16.0, 16.0),
                    ),
                    Icon::Terminal,
                    t.accent,
                );
                let text = row
                    .with_min_x(row.left() + 32.0)
                    .with_max_x((row.right() - 40.0).max(row.left() + 33.0));
                theme::label(
                    ui,
                    text,
                    &profile.name,
                    t.chrome_font(11.0),
                    t.text,
                    Align2::LEFT_CENTER,
                );

                if response.double_clicked() {
                    connect = Some(index);
                }
                response.context_menu(|ui| {
                    if ui.button(crate::i18n::tr(ui, "Connect")).clicked() {
                        connect = Some(index);
                        ui.close();
                    }
                    if ui.button(crate::i18n::tr(ui, "Edit")).clicked() {
                        edit = Some(index);
                        ui.close();
                    }
                    if ui.button(crate::i18n::tr(ui, "Delete")).clicked() {
                        delete = Some(index);
                        ui.close();
                    }
                });
                if response.contains_pointer() {
                    ui.push_id(index, |ui| {
                        if theme::icon_button(ui, icon_slot(row, 1), "Edit Session", Icon::Edit, t)
                            .clicked()
                        {
                            edit = Some(index);
                        }
                        if theme::icon_button(
                            ui,
                            icon_slot(row, 0),
                            "Delete Session",
                            Icon::Trash,
                            t,
                        )
                        .clicked()
                        {
                            delete = Some(index);
                        }
                    });
                }
            }
            if matches == 0 {
                theme::label(
                    ui,
                    Rect::from_min_size(list.min, vec2(list.width(), 36.0)),
                    crate::i18n::tr(
                        ui,
                        if self.profiles.is_empty() {
                            "No saved sessions"
                        } else {
                            "No matching sessions"
                        },
                    ),
                    t.chrome_font(11.0),
                    t.dim,
                    Align2::LEFT_TOP,
                );
            }
        });
        if let Some(index) = connect {
            let profile = self.profiles[index].clone();
            if let Connection::Ssh { config, .. } = &profile.connection
                && !config.use_agent
                && config.password.is_none()
                && config.key_path.is_none()
                && config.key_name.is_none()
            {
                edit = Some(index);
            } else if profile.kind() == Kind::Ssh {
                self.open_ssh_profile(profile);
            } else {
                self.connect(profile, None);
            }
        }
        if let Some(index) = edit {
            self.dialog = Some(Dialog::connect(ConnectForm::editing(
                &self.profiles[index],
                index,
                &self.settings,
            )));
        }
        if let Some(index) = delete {
            self.dialog = Some(Dialog::DeleteProfile(index));
        }
    }

    fn history_panel(&mut self, ui: &mut egui::Ui, header: Rect, bottom: f32) {
        let t = self.theme;
        ui.painter().hline(
            header.x_range(),
            header.top() + 0.5,
            Stroke::new(1.0_f32, t.border),
        );
        let button = header.with_max_x(header.right() - 24.0);
        if ui
            .interact(button, ui.id().with("history-header"), Sense::click())
            .clicked()
        {
            self.history_visible = !self.history_visible;
        }
        header_label(ui, button, "Command History", t);
        if theme::icon_button(ui, icon_slot(header, 0), "Clear History", Icon::Trash, t).clicked() {
            self.history.clear();
            if self.persist_history {
                let _ = storage::save_settings(&[("command_history", "[]".into())]);
            }
        }
        if header.bottom() >= bottom {
            return;
        }
        let bounds = header.with_min_y(header.bottom()).with_max_y(bottom);
        let mut child = ui.new_child(egui::UiBuilder::new().id_salt("history").max_rect(bounds));
        child.set_clip_rect(bounds);
        theme::compact_list(bounds.height()).show(&mut child, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if self.history.is_empty() {
                theme::label(
                    ui,
                    bounds,
                    crate::i18n::tr(ui, "No command history"),
                    t.chrome_font(11.0),
                    t.dim,
                    Align2::CENTER_CENTER,
                );
            }
            for command in &self.history {
                let (row, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::click());
                let response = response.on_hover_text(command);
                if response.hovered() {
                    theme::band(ui, row, t.hover, None);
                }
                ui.painter()
                    .circle_filled(pos2(row.left() + 10.0, row.center().y), 2.0, t.dim);
                theme::label(
                    ui,
                    row.with_min_x(row.left() + 20.0),
                    command,
                    FontId::monospace(11.0),
                    t.muted,
                    Align2::LEFT_CENTER,
                );
                if response.clicked()
                    && let Some(terminal) = self
                        .terminals
                        .iter()
                        .find(|item| Some(item.tab) == self.active)
                {
                    terminal.paste(&self.backend, command);
                    terminal.send(&self.backend, b"\r".to_vec());
                    self.focus_terminal = true;
                }
            }
        });
    }
}

pub(super) fn compact_filter(
    ui: &mut egui::Ui,
    rect: Rect,
    value: &mut String,
    id: &str,
    t: Theme,
) -> Rect {
    let search = top_band(rect, 17.0);
    ui.put(
        search.shrink2(vec2(4.0, 0.0)),
        egui::TextEdit::singleline(value)
            .id_salt(id)
            .hint_text(egui::RichText::new(crate::i18n::tr(ui, "Filter")).color(t.dim))
            .font(t.chrome_font(10.0))
            .frame(false)
            .margin(egui::Margin::symmetric(1, 0)),
    );
    ui.painter().hline(
        search.x_range(),
        search.bottom() - 0.5,
        Stroke::new(1.0_f32, t.border),
    );
    search
}

pub(crate) fn top_band(rect: Rect, height: f32) -> Rect {
    rect.with_max_y(rect.top() + height)
}

pub(crate) fn header_label(ui: &egui::Ui, rect: Rect, text: &str, t: Theme) {
    theme::label(
        ui,
        rect.with_min_x(rect.left() + 21.0)
            .with_max_x(rect.right() - 8.0),
        crate::i18n::tr(ui, text),
        t.chrome_font(11.0),
        t.text,
        Align2::LEFT_CENTER,
    );
    {
        let marker = match text {
            "Explorer" => t.red,
            "Filer" => t.purple,
            "Session" => t.accent,
            _ => t.blue,
        };
        ui.painter().rect_filled(
            Rect::from_center_size(pos2(rect.left() + 10.0, rect.center().y), vec2(5.0, 5.0)),
            0.0,
            marker,
        );
    }
}

pub(crate) fn icon_slot(rect: Rect, right_index: usize) -> Rect {
    Rect::from_center_size(
        pos2(
            rect.right() - 14.0 - right_index as f32 * 20.0,
            rect.center().y,
        ),
        vec2(18.0, 20.0),
    )
}
