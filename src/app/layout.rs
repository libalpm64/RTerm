use super::{menu::terminal_menu, *};

impl App {
    fn tab_bar(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let t = self.theme;

        let tab_height = rect.height() - 1.0;
        theme::band(ui, rect, t.panel, Some(t.border));
        let clip = rect;
        let mut select = None;
        let mut close = None;
        let mut duplicate = None;
        let mut rename = None;
        let mut activity = None;
        let mut reconnect = None;
        let mut close_group = None;
        let mut reorder = None;
        let mut add = false;
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("tabs")
                .max_rect(clip)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        child.set_clip_rect(clip);
        egui::ScrollArea::horizontal()
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(&mut child, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for terminal in &self.terminals {
                        let name_width = ui.fonts_mut(|fonts| {
                            fonts
                                .layout_no_wrap(terminal.name.clone(), t.chrome_font(12.0), t.text)
                                .size()
                                .x
                        });
                        let (_, tab) =
                            ui.allocate_space(vec2((name_width + 50.0).min(233.0), tab_height));
                        let response = ui.interact(
                            tab,
                            ui.id().with(("session-tab", terminal.tab)),
                            Sense::click_and_drag(),
                        );
                        let active = self.active == Some(terminal.tab) && !self.show_keys;
                        let hovered = response.contains_pointer();
                        {
                            ui.painter().rect(
                                tab,
                                egui::CornerRadius {
                                    nw: 0,
                                    ne: 0,
                                    sw: 0,
                                    se: 0,
                                },
                                if active {
                                    t.bg
                                } else if hovered {
                                    t.hover
                                } else {
                                    t.bar
                                },
                                Stroke::new(
                                    1.0_f32,
                                    if active {
                                        t.border
                                    } else {
                                        Color32::TRANSPARENT
                                    },
                                ),
                                egui::StrokeKind::Inside,
                            );
                        }
                        let selected = ui.ctx().animate_bool_with_time(
                            response.id.with("selected"),
                            active,
                            0.075,
                        );
                        if selected > 0.0 {
                            ui.painter().hline(
                                tab.x_range(),
                                tab.top() + 1.0,
                                Stroke::new(2.0_f32, t.accent.gamma_multiply(selected)),
                            );
                        }
                        let color = if terminal.closed {
                            t.dim
                        } else if terminal.starting() || terminal.endpoint.is_none() {
                            t.yellow
                        } else {
                            match terminal.kind {
                                Kind::Local => t.green,
                                Kind::Ssh => t.blue,
                                Kind::Telnet | Kind::Serial => t.purple,
                            }
                        };
                        {
                            theme::icon(
                                ui,
                                Rect::from_center_size(
                                    pos2(tab.left() + 12.0, tab.center().y),
                                    vec2(13.0, 13.0),
                                ),
                                Icon::Terminal,
                                color,
                            );
                        }
                        let title = tab
                            .with_min_x(tab.left() + 24.0)
                            .with_max_x(tab.right() - 22.0);
                        theme::label(
                            ui,
                            title,
                            &terminal.name,
                            t.chrome_font(12.0),
                            if active || hovered { t.text } else { t.muted },
                            Align2::LEFT_CENTER,
                        );
                        if hovered || active {
                            let size = Rect::from_center_size(
                                pos2(tab.right() - 12.0, tab.center().y),
                                vec2(16.0, 18.0),
                            );
                            if ui
                                .push_id(terminal.tab, |ui| {
                                    theme::icon_button(ui, size, "Close tab", Icon::Close, t)
                                })
                                .inner
                                .clicked()
                            {
                                close = Some(terminal.tab);
                            }
                        }
                        if response.clicked() {
                            select = Some(terminal.tab);
                        }
                        if active && self.focus_terminal {
                            response.scroll_to_me(Some(egui::Align::Center));
                        }
                        if response.drag_started() {
                            self.drag_tab = Some(terminal.tab);
                        }
                        if hovered
                            && ui.input(|input| input.pointer.any_released())
                            && let Some(from) = self.drag_tab
                        {
                            reorder = Some((from, terminal.tab));
                        }
                        response.context_menu(|ui| {
                            if ui.button(crate::i18n::tr(ui, "Rename")).clicked() {
                                rename = Some((terminal.tab, terminal.name.clone()));
                                ui.close();
                            }
                            if ui.button(crate::i18n::tr(ui, "Duplicate")).clicked() {
                                duplicate = Some(terminal.tab);
                                ui.close();
                            }
                            if terminal.kind == Kind::Ssh
                                && ui.button(crate::i18n::tr(ui, "AI Activity…")).clicked()
                            {
                                activity = Some(terminal.tab);
                                ui.close();
                            }
                            if terminal.kind == Kind::Ssh
                                && !terminal.starting()
                                && ui.button(crate::i18n::tr(ui, "Reconnect")).clicked()
                            {
                                reconnect = Some(terminal.tab);
                                ui.close();
                            }
                            ui.separator();
                            if ui.button(crate::i18n::tr(ui, "Close")).clicked() {
                                close = Some(terminal.tab);
                                ui.close();
                            }
                            if ui.button(crate::i18n::tr(ui, "Close Others")).clicked() {
                                close_group = Some(Some(terminal.tab));
                                ui.close();
                            }
                            if ui.button(crate::i18n::tr(ui, "Close All")).clicked() {
                                close_group = Some(None);
                                ui.close();
                            }
                        });
                    }
                    if self.keys.open {
                        let (_, rect) = ui.allocate_space(vec2(107.0, tab_height));
                        let response =
                            ui.interact(rect, ui.id().with("ssh-keys-tab"), Sense::click());
                        if self.show_keys || response.hovered() {
                            theme::band(ui, rect, t.bg, None);
                        }
                        if self.show_keys {
                            ui.painter().hline(
                                rect.x_range(),
                                rect.top() + 1.0,
                                Stroke::new(2.0_f32, t.accent),
                            );
                        }
                        let glyph = Rect::from_center_size(
                            pos2(rect.left() + 15.0, rect.center().y),
                            vec2(14.0, 14.0),
                        );
                        theme::icon(ui, glyph, Icon::Key, t.accent);
                        theme::label(
                            ui,
                            rect.shrink2(vec2(23.0, 0.0)),
                            crate::i18n::tr(ui, "SSH Keys"),
                            t.chrome_font(12.0),
                            t.text,
                            Align2::LEFT_CENTER,
                        );
                        if response.clicked() {
                            self.show_keys = true;
                            self.bottom_focus = false;
                            self.bottom_request_focus = false;
                        }
                        let button = Rect::from_center_size(
                            pos2(rect.right() - 12.0, rect.center().y),
                            vec2(18.0, 18.0),
                        );
                        if theme::icon_button(ui, button, "Close SSH Keys", Icon::Close, t)
                            .clicked()
                        {
                            self.keys.open = false;
                            self.show_keys = false;
                            self.focus_terminal = true;
                        }
                    }
                    let (rect, _) = ui.allocate_exact_size(vec2(28.0, tab_height), Sense::hover());
                    if theme::icon_button(ui, rect, "New Session", Icon::Add, t).clicked() {
                        add = true;
                    }
                });
            });
        if ui.input(|input| input.pointer.any_released()) {
            self.drag_tab = None;
        }
        if let Some((from, to)) = reorder
            && let Some(source) = self.terminals.iter().position(|item| item.tab == from)
            && let Some(destination) = self.terminals.iter().position(|item| item.tab == to)
            && source != destination
        {
            let terminal = self.terminals.remove(source);
            self.terminals.insert(destination, terminal);
        }
        if let Some(id) = select {
            self.activate(id);
        }
        if let Some(tab) = activity {
            self.open_agent_activity(Some(tab));
        }
        if let Some(tab) = reconnect {
            self.reconnect_ssh_tab(tab);
        }
        if let Some(id) = close {
            self.close_tab(id);
        }
        if let Some((tab, name)) = rename {
            self.dialog = Some(Dialog::RenameTab { tab, name });
        }
        if let Some(keep) = close_group {
            let closing = self
                .terminals
                .iter()
                .filter_map(|terminal| (Some(terminal.tab) != keep).then_some(terminal.tab))
                .collect::<Vec<_>>();
            for tab in closing {
                self.close_tab(tab);
            }
            self.keys.open = false;
            self.show_keys = false;
        }
        if let Some(id) = duplicate
            && let Some(terminal) = self.terminals.iter().find(|item| item.tab == id)
        {
            if terminal.kind == Kind::Local {
                add = true;
            } else {
                let profile = self
                    .profiles
                    .iter()
                    .find(|profile| {
                        profile.kind() == terminal.kind && profile.name == terminal.name
                    })
                    .cloned();
                if let Some(profile) = profile {
                    self.connect(profile, None);
                } else {
                    self.show_connect();
                }
            }
        }
        if add {
            self.new_local();
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let t = self.theme;
        let rect = rect.shrink2(vec2(4.0, 2.0));
        theme::band(ui, rect, t.bar, Some(t.border));
        theme::icon(
            ui,
            Rect::from_center_size(pos2(rect.left() + 14.0, rect.center().y), vec2(12.0, 12.0)),
            Icon::Play,
            t.muted,
        );
        let name = self
            .terminals
            .iter()
            .find(|item| Some(item.tab) == self.active)
            .map_or("", |item| item.name.as_str());
        let crumbs = format!("shell  ›  sessions  ›  {name}");
        let text = rect
            .with_min_x(rect.left() + 26.0)
            .with_max_x(rect.right() - 86.0);
        theme::label(
            ui,
            text,
            &crumbs,
            t.chrome_font(11.0),
            t.muted,
            Align2::LEFT_CENTER,
        );
        let mut action = None;
        for (index, (title, icon)) in [
            ("Quick Actions", Icon::Menu),
            ("New Session", Icon::Add),
            ("Zoom In", Icon::Zoom),
        ]
        .into_iter()
        .enumerate()
        {
            let area = Rect::from_center_size(
                pos2(rect.right() - 20.0 - index as f32 * 30.0, rect.center().y),
                vec2(24.0, 22.0),
            );
            let response = ui
                .push_id("toolbar", |ui| theme::icon_button(ui, area, title, icon, t))
                .inner;
            match icon {
                Icon::Menu => {
                    action = terminal_menu(egui::Popup::menu(&response), t);
                }
                Icon::Add if response.clicked() => action = Some(Action::New),
                Icon::Zoom if response.clicked() => action = Some(Action::Zoom(1.0)),
                _ => {}
            }
        }
        if let Some(action) = action {
            self.run_action(action, ui.ctx());
        }
    }

    fn status(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let t = self.theme;
        theme::band(ui, rect, t.panel, None);
        ui.painter().hline(
            rect.x_range(),
            rect.top() + 0.5,
            Stroke::new(1.0_f32, t.border),
        );
        let selected = if self.bottom_open && self.bottom_focus {
            self.bottom_shell.as_ref()
        } else {
            self.terminals
                .iter()
                .find(|item| Some(item.tab) == self.active)
        };
        let state = selected.map_or("Ready", |term| {
            if term.kind == Kind::Ssh && self.ssh_connection_state(term) == "reconnecting" {
                "Reconnecting..."
            } else if term.starting() {
                "Connecting…"
            } else if term.closed {
                "Disconnected"
            } else if self.agent.running_on(self.ssh_agent_id(term.tab)) {
                "AI running..."
            } else {
                "Ready"
            }
        });
        let time = chrono::Local::now().format("%m/%d/%Y %H:%M");
        theme::label(
            ui,
            rect.with_max_x(rect.left() + 118.0).shrink2(vec2(8.0, 0.0)),
            crate::i18n::tr(ui, state),
            t.chrome_font(10.0),
            t.muted,
            Align2::LEFT_CENTER,
        );
        let about = rect.with_min_x(rect.right() - 58.0);
        if theme::button(
            ui,
            about,
            "about",
            "Rterm",
            t.chrome_font(11.0),
            t.accent,
            t,
        )
        .clicked()
        {
            self.dialog = Some(Dialog::About);
        }
        let mut right = about.left() - 8.0;
        let mut fields = vec![time.to_string()];
        if self.show_keys && !self.bottom_focus {
            fields.push("SSH Keys".into());
        } else if let Some(term) = selected {
            fields.push(term.name.clone());
            let (dimensions, cursor) = term.status();
            fields.push(cursor);
            fields.push(dimensions);
            fields.push(format!("{} Mode", term.kind.title()));
        }
        for field in fields {
            let font = t.chrome_font(10.0);
            let width = ui.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(field.clone(), font.clone(), t.muted)
                    .size()
                    .x
            }) + 16.0;
            let left = right - width;
            if left < rect.left() + 122.0 {
                break;
            }
            theme::label(
                ui,
                rect.with_min_x(left).with_max_x(right),
                &field,
                font,
                t.muted,
                Align2::CENTER_CENTER,
            );
            right = left;
        }
        ui.ctx().request_repaint_after(Duration::from_secs(1));
    }

    fn bottom_panel(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let t = self.theme;
        theme::band(ui, rect, t.bg, None);
        let header = top_band(rect, 23.0);
        theme::band(ui, header, t.bar, Some(t.border));
        ui.painter().hline(
            header.x_range(),
            header.top() + 0.5,
            Stroke::new(1.0_f32, t.border),
        );
        let tab = header.with_max_x(header.left() + 90.0);
        let response = ui.interact(tab, ui.id().with("bottom-shell-toggle"), Sense::click());
        if self.bottom_open || response.hovered() {
            theme::band(ui, tab, t.hover, None);
        }
        ui.painter().rect_filled(
            Rect::from_center_size(pos2(tab.left() + 12.0, tab.center().y), vec2(5.0, 5.0)),
            0.0,
            t.red,
        );
        theme::label(
            ui,
            tab.with_min_x(tab.left() + 24.0),
            crate::i18n::tr(ui, "Shell"),
            t.chrome_font(11.0),
            t.text,
            Align2::LEFT_CENTER,
        );
        let arrow = if self.bottom_open {
            Icon::Close
        } else {
            Icon::Up
        };
        let toggle =
            theme::icon_button(ui, icon_slot(header, 0), "Toggle Bottom Shell", arrow, t).clicked();
        if response.clicked() || toggle {
            self.toggle_bottom_shell();
            ui.ctx().request_repaint();
            return;
        }
        if !self.bottom_open {
            return;
        }
        let body = rect.with_min_y(header.bottom());
        if let Some(tab) = self.bottom_shell.as_ref().map(|term| term.tab) {
            self.wheel_zoom(ui, body, tab);
        }
        let input = self.dialog.is_none()
            && self.message.is_none()
            && !self.host_trust.blocking()
            && !egui::Popup::is_any_open(ui.ctx())
            && ui.memory(|memory| memory.allows_interaction(ui.layer_id()));
        let options = self.bottom_options();
        let preview = self
            .bottom_shell
            .as_ref()
            .and_then(|term| self.zoom_preview(term.tab));
        if let Some(term) = &mut self.bottom_shell {
            if self.bottom_request_focus && input {
                ui.memory_mut(|memory| memory.request_focus(ui.id().with(("terminal", term.tab))));
                self.bottom_request_focus = false;
            } else if self.bottom_request_focus && self.dialog.is_none() && self.message.is_none() {
                // egui retains the just-closed modal layer for one more frame.
                // Do not consume a focus request until the workspace can interact.
                ui.ctx().request_repaint();
            }
            let (response, history, error) = term.show(
                ui,
                body,
                &self.backend,
                &options,
                &self.highlighter,
                input,
                preview,
                None, // Wallpaper is exclusive to the main terminal tabs.
            );
            if response.has_focus() || response.clicked() {
                self.bottom_focus = true;
            }
            self.remember_history(history);
            if let Some(error) = error {
                self.fail(error);
            }
            if let Some(action) = terminal_menu(egui::Popup::context_menu(&response), t) {
                self.bottom_focus = true;
                self.run_action(action, ui.ctx());
            }
        }
    }

    fn center(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let tabs = top_band(rect, 23.0);
        self.tab_bar(ui, tabs);
        if self.show_keys {
            self.key_panel(ui, rect.with_min_y(tabs.bottom()));

            return;
        }
        let toolbar = top_band(rect.with_min_y(tabs.bottom()), 22.0);
        self.toolbar(ui, toolbar);
        let body = rect.with_min_y(toolbar.bottom());
        if let Some(tab) = self.active {
            self.wheel_zoom(ui, body, tab);
        }
        let options = self.settings.terminal();
        let mut startup_failure = None;
        // All queued local tabs share the measured main pane dimensions, even
        // when the user switches to another tab before a shell starts.
        for term in &mut self.terminals {
            term.prepare_remote(ui, body, &options);
            if let Err(error) = term.prepare_local(ui, body, &options, &self.backend) {
                term.mark_closed();
                term.receive(format!("Could not start the shell: {error}\r\n").as_bytes());
                self.local_profiles.remove(&term.tab);
                startup_failure = Some(error);
            }
        }
        if let Some(error) = startup_failure {
            self.fail(error);
        }
        if let Some(index) = self
            .terminals
            .iter()
            .position(|item| Some(item.tab) == self.active)
        {
            let options = self.settings.terminal();
            let input = self.dialog.is_none()
                && self.message.is_none()
                && !self.host_trust.blocking()
                && !egui::Popup::is_any_open(ui.ctx())
                && ui.memory(|memory| memory.allows_interaction(ui.layer_id()));
            if self.focus_terminal && input {
                ui.memory_mut(|memory| {
                    memory.request_focus(ui.id().with(("terminal", self.terminals[index].tab)))
                });
                self.focus_terminal = false;
                self.bottom_focus = false;
            } else if self.focus_terminal && self.dialog.is_none() && self.message.is_none() {
                ui.ctx().request_repaint();
            }
            let preview = self.zoom_preview(self.terminals[index].tab);
            let (response, history, error) = self.terminals[index].show(
                ui,
                body,
                &self.backend,
                &options,
                &self.highlighter,
                input,
                preview,
                self.wallpaper.as_ref(),
            );
            if self.terminals[index].kind != Kind::Local
                && self.terminals[index].endpoint.is_none()
                && self.terminals[index].starting()
            {
                // A fixed-size UI label is independent of terminal zoom and
                // never enters the grid or the saved command history.
                theme::label(
                    ui,
                    Rect::from_center_size(body.center(), vec2(body.width(), 24.0)),
                    crate::i18n::tr(ui, "Connecting…"),
                    self.theme.chrome_font(12.0),
                    self.theme.muted,
                    Align2::CENTER_CENTER,
                );
            }
            if response.has_focus() || response.clicked() {
                self.bottom_focus = false;
            }
            self.remember_history(history);
            if let Some(error) = error {
                self.fail(error);
            }
            if let Some(action) = terminal_menu(egui::Popup::context_menu(&response), self.theme) {
                self.bottom_focus = false;
                self.run_action(action, ui.ctx());
            }
        } else {
            theme::band(ui, body, self.theme.bg, None);
            let title =
                Rect::from_center_size(body.center() - vec2(0.0, 16.0), vec2(body.width(), 28.0));
            theme::label(
                ui,
                title,
                crate::i18n::tr(ui, "Welcome to Rterm"),
                theme::strong(20.0),
                self.theme.text,
                Align2::CENTER_CENTER,
            );
            theme::label(
                ui,
                title.translate(vec2(0.0, 24.0)),
                crate::i18n::tr(ui, "Create a new session to get started"),
                self.theme.chrome_font(12.0),
                self.theme.muted,
                Align2::CENTER_CENTER,
            );
            let new = Rect::from_center_size(body.center() + vec2(-52.0, 42.0), vec2(98.0, 28.0));
            let connect =
                Rect::from_center_size(body.center() + vec2(45.0, 42.0), vec2(78.0, 28.0));
            if theme::button(
                ui,
                new,
                "empty-new",
                "New Session",
                self.theme.chrome_font(12.0),
                self.theme.accent,
                self.theme,
            )
            .clicked()
            {
                self.new_local();
            }
            if theme::button(
                ui,
                connect,
                "empty-connect",
                "Connect",
                self.theme.chrome_font(12.0),
                self.theme.text,
                self.theme,
            )
            .clicked()
            {
                self.show_connect();
            }
        }
        if !self.transfers.is_empty() {
            self.transfer_overlay(ui, body);
        }
    }

    fn active_sessions_panel(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let t = self.theme;
        theme::band(ui, rect, t.panel, Some(t.border));
        let header = top_band(rect, 22.0);
        theme::band(ui, header, t.panel, Some(t.border));
        header_label(ui, header, "Explorer", t);
        if theme::icon_button(
            ui,
            icon_slot(header, 0),
            "Hide Session Explorer",
            Icon::Close,
            t,
        )
        .clicked()
        {
            self.sidebar = None;
        }
        let list = {
            let search = super::sessions::compact_filter(
                ui,
                rect.with_min_y(header.bottom()),
                &mut self.active_filter,
                "active-session-filter",
                t,
            );
            rect.with_min_y(search.bottom())
        };
        let filter = self.active_filter.to_lowercase();
        let mut selected = None;
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("active-sessions")
                .max_rect(list),
        );
        child.set_clip_rect(list);
        theme::compact_list(list.height()).show(&mut child, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for terminal in &self.terminals {
                if !terminal.name.to_lowercase().contains(&filter) {
                    continue;
                }
                let (row, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 19.0), Sense::click());
                let active = self.active == Some(terminal.tab) && !self.show_keys;
                if active || response.hovered() {
                    theme::band(ui, row, t.hover, None);
                }
                let icon = Rect::from_center_size(
                    pos2(row.left() + 14.0, row.center().y),
                    vec2(12.0, 12.0),
                );
                theme::icon(
                    ui,
                    icon,
                    Icon::Terminal,
                    if terminal.closed { t.dim } else { t.muted },
                );
                theme::label(
                    ui,
                    row.with_min_x(row.left() + 26.0),
                    &terminal.name,
                    t.chrome_font(11.0),
                    t.text,
                    Align2::LEFT_CENTER,
                );
                if response.clicked() {
                    selected = Some(terminal.tab);
                }
            }
        });
        if let Some(tab) = selected {
            self.activate(tab);
        }
    }

    fn transfer_overlay(&self, ui: &mut egui::Ui, body: Rect) {
        let width = (body.width() - 20.0).min(330.0);
        let height = (self.transfers.len().min(5) as f32) * 38.0 + 14.0;
        let area = Rect::from_min_size(
            pos2(body.right() - width - 10.0, body.bottom() - height - 10.0),
            vec2(width, height),
        );
        ui.painter().rect(
            area,
            0.0,
            self.theme.panel,
            Stroke::new(1.0_f32, self.theme.border),
            egui::StrokeKind::Inside,
        );
        for (index, transfer) in self.transfers.values().take(5).enumerate() {
            let line = Rect::from_min_size(
                area.min + vec2(9.0, 6.0 + index as f32 * 38.0),
                vec2(width - 18.0, 22.0),
            );
            let color = if transfer.error {
                self.theme.red
            } else {
                self.theme.accent
            };
            theme::label(
                ui,
                line,
                &transfer.text,
                self.theme.chrome_font(11.0),
                self.theme.text,
                Align2::LEFT_CENTER,
            );
            let track = Rect::from_min_size(line.left_bottom(), vec2(line.width(), 4.0));
            ui.painter().rect_filled(track, 0.0, self.theme.hover);
            ui.painter().rect_filled(
                track.with_max_x(
                    track.left() + track.width() * transfer.percent.clamp(0.0, 100.0) / 100.0,
                ),
                0.0,
                color,
            );
        }
    }
}

impl App {
    fn capture_terminal_navigation(&mut self, context: &egui::Context, raw: &mut egui::RawInput) {
        self.terminal_navigation.clear();
        if !raw.focused
            || self.locked
            || self.dialog.is_some()
            || self.message.is_some()
            || self.agent.blocks_terminal_input()
            || self.host_trust.blocking()
            || egui::Popup::is_any_open(context)
            || raw
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::PointerButton { pressed: true, .. }))
        {
            return;
        }
        let tab = context
            .memory(|memory| memory.focused())
            .and_then(|id| context.data(|data| data.get_temp::<u64>(id)));
        let valid = tab.is_some_and(|tab| {
            (!self.show_keys && self.active == Some(tab))
                || (self.bottom_open
                    && self
                        .bottom_shell
                        .as_ref()
                        .is_some_and(|term| term.tab == tab))
        });
        if !valid {
            return;
        }
        // egui processes focus navigation before App::update, including Escape
        // surrendering focus and Tab moving to a different widget. Stash only
        // terminal navigation events and restore them in original order after
        // begin_pass. This also works on the very first focused frame, when
        // set_focus_lock_filter has not taken effect yet.
        let mut index = 0;
        raw.events.retain(|event| {
            let capture = matches!(
                event,
                egui::Event::Key {
                    key: Key::Tab
                        | Key::ArrowUp
                        | Key::ArrowDown
                        | Key::ArrowLeft
                        | Key::ArrowRight
                        | Key::Escape,
                    ..
                }
            );
            if capture {
                self.terminal_navigation.push((index, event.clone()));
            }
            index += 1;
            !capture
        });
    }

    fn restore_terminal_navigation(&mut self, context: &egui::Context) {
        if !self.terminal_navigation.is_empty() {
            context.input_mut(|input| {
                for (index, event) in self.terminal_navigation.drain(..) {
                    input.events.insert(index.min(input.events.len()), event);
                }
            });
        }
    }
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, context: &egui::Context, raw: &mut egui::RawInput) {
        self.capture_terminal_navigation(context, raw);
    }

    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.restore_terminal_navigation(context);
        self.poll(context);
        self.poll_agent(context);
        if context.input(|input| input.viewport().close_requested()) && !self.exit_confirmed {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.request_exit(context);
        }
        if context.input(|input| !input.events.is_empty()) {
            self.last_activity = Instant::now();
        }
        let timeout = self
            .settings
            .get("lock_timeout")
            .parse::<u64>()
            .unwrap_or(0);
        if timeout > 0 && !self.locked && self.settings.on("vault_enabled") {
            if self.last_activity.elapsed() >= Duration::from_secs(timeout.saturating_mul(60)) {
                self.lock();
            } else {
                context.request_repaint_after(Duration::from_secs(1));
            }
        }
        self.settle_wheel_zoom(context);
        if self.dialog.is_none()
            && self.message.is_none()
            && !self.locked
            && !self.host_trust.blocking()
        {
            self.shortcuts(context);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(self.theme.bg))
            .show(context, |ui| {
                if self.locked {
                    self.lock_screen(ui);
                    return;
                }
                // Match the original content bands and reserve usable terminal width
                // when resizing both sidebars in a narrow window.
                let outer = ui.max_rect();

                let full = outer;
                let menu = top_band(full, 26.0);
                self.menu_bar(ui, menu);
                let status = full.with_min_y(full.bottom() - 23.0);
                let shell_height = if self.bottom_open {
                    self.bottom_height
                        .min((status.top() - menu.bottom()) * 0.48)
                        .max(100.0)
                } else {
                    0.0
                };
                let bottom = full
                    .with_min_y(status.top() - 23.0 - shell_height)
                    .with_max_y(status.top());
                let body = full.with_min_y(menu.bottom()).with_max_y(bottom.top());
                let dock_height = (body.height() * self.dock_fraction)
                    .clamp(90.0, (body.height() - 130.0).max(90.0));
                // Match desktop dock proportions at large window sizes while retaining
                // the existing user-resizable widths at ordinary window sizes.
                let dock_scale = (body.width() / 1440.0).max(1.0);
                let lw = self
                    .sidebar
                    .unwrap_or(0.0)
                    .mul_add(dock_scale, 0.0)
                    .min((body.width() - 300.0).max(0.0));
                let rw = self
                    .sessions_sidebar
                    .unwrap_or(0.0)
                    .mul_add(dock_scale, 0.0)
                    .min((body.width() - lw - 250.0).max(0.0));
                if lw > 0.0 {
                    let rect = body.with_max_x(body.left() + lw);
                    {
                        let sessions = top_band(rect, dock_height);
                        ui.push_id("explorer-dock", |ui| {
                            self.active_sessions_panel(ui, sessions)
                        });
                        let split = rect
                            .with_min_y(sessions.bottom())
                            .with_max_y(sessions.bottom() + 3.0);
                        {
                            let response = ui
                                .interact(split, ui.id().with("dock-split"), Sense::drag())
                                .on_hover_cursor(egui::CursorIcon::ResizeVertical);
                            ui.painter().hline(
                                split.x_range(),
                                split.center().y,
                                Stroke::new(
                                    1.0_f32,
                                    if response.hovered() {
                                        self.theme.accent
                                    } else {
                                        self.theme.border
                                    },
                                ),
                            );
                            if response.dragged() {
                                self.dock_fraction = (self.dock_fraction
                                    + response.drag_delta().y / rect.height().max(1.0))
                                .clamp(0.2, 0.8);
                            }
                        }
                        ui.push_id("filer-dock", |ui| {
                            self.explorer_panel(ui, rect.with_min_y(split.bottom()))
                        });
                    }
                    resize_bar(
                        ui,
                        body.with_min_x(rect.right()).with_max_x(rect.right() + 4.0),
                        &mut self.sidebar,
                        1.0 / dock_scale,
                        self.theme,
                        "resize-left",
                    );
                }
                if rw > 0.0 {
                    let rect = body.with_min_x(body.right() - rw);
                    ui.push_id("session-dock", |ui| self.session_panel(ui, rect));
                    resize_bar(
                        ui,
                        body.with_min_x(rect.left() - 4.0).with_max_x(rect.left()),
                        &mut self.sessions_sidebar,
                        -1.0 / dock_scale,
                        self.theme,
                        "resize-right",
                    );
                }
                self.center(
                    ui,
                    body.with_min_x(body.left() + lw + if lw > 0.0 { 4.0 } else { 0.0 })
                        .with_max_x(body.right() - rw - if rw > 0.0 { 4.0 } else { 0.0 }),
                );
                self.bottom_panel(ui, bottom);
                if self.bottom_open {
                    let split = bottom
                        .with_min_y(bottom.top() - 2.0)
                        .with_max_y(bottom.top() + 2.0);
                    let response = ui
                        .interact(split, ui.id().with("bottom-shell-resize"), Sense::drag())
                        .on_hover_cursor(egui::CursorIcon::ResizeVertical);
                    if response.dragged() {
                        self.bottom_height =
                            (self.bottom_height - response.drag_delta().y).clamp(100.0, 600.0);
                    }
                }
                {
                    self.status(ui, status);
                }
            });
        self.show_dialogs(context);
        self.show_agent_activity(context);
        if self.host_trust.blocking() {
            self.show_host_trust(context);
        } else {
            self.show_agent_dialog(context);
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if self.desktop_shortcut.is_some()
            && self.dialog.is_none()
            && self.message.is_none()
            && !self.locked
        {
            let response =
                egui::Modal::new(egui::Id::new("desktop-shortcut-setup")).show(context, |ui| {
                    ui.heading("RTerm setup");
                    ui.label(crate::i18n::tr(ui, "Create a desktop shortcut?"));
                    ui.horizontal(|ui| {
                        if ui.button(crate::i18n::tr(ui, "No thanks")).clicked() {
                            return Some(false);
                        }
                        if ui.button(crate::i18n::tr(ui, "Create shortcut")).clicked() {
                            return Some(true);
                        }
                        None
                    })
                    .inner
                });
            if let Some(create) = response
                .inner
                .or_else(|| response.should_close().then_some(false))
            {
                if let Some(setup) = self.desktop_shortcut.take() {
                    if let Err(error) = setup.choose(create) {
                        self.message = Some(format!("Desktop shortcut setup: {error}"));
                    }
                }
            }
        }
    }
}

fn resize_bar(
    ui: &mut egui::Ui,
    rect: Rect,
    width: &mut Option<f32>,
    direction: f32,
    theme: Theme,
    id: &str,
) {
    let response = ui
        .interact(rect, ui.id().with(id), Sense::drag())
        .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
    {
        ui.painter().vline(
            rect.center().x,
            rect.y_range(),
            Stroke::new(
                1.0_f32,
                if response.hovered() || response.dragged() {
                    theme.accent
                } else {
                    theme.border
                },
            ),
        );
    }
    if response.dragged()
        && let Some(width) = width
    {
        *width = (*width + response.drag_delta().x * direction).clamp(60.0, 500.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(windows, feature = "benchmark"))]
    #[test]
    #[ignore = "requires Neovim on PATH; exercises a real local Windows PTY"]
    fn native_neovim_accepts_arrows_escape_search_and_quit() {
        let context = egui::Context::default();
        let mut app = App::with_settings(&context, Settings::benchmark_defaults(), false);
        let mut terminal = Terminal::new(
            1,
            "Editor test".into(),
            Kind::Local,
            &app.settings.terminal(),
        );
        terminal.queue_local("cmd.exe", false);
        app.terminals.push(terminal);
        app.active = Some(1);
        let mut frame = eframe::Frame::_new_kittest();
        let mut render = |app: &mut App, events: Vec<egui::Event>| {
            let mut raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1440.0, 900.0))),
                events,
                ..Default::default()
            };
            eframe::App::raw_input_hook(app, &context, &mut raw);
            let _ = context.run(raw, |ctx| eframe::App::update(app, ctx, &mut frame));
        };
        fn wait_for(
            app: &mut App,
            render: &mut impl FnMut(&mut App, Vec<egui::Event>),
            label: &str,
            predicate: impl Fn(&Terminal) -> bool,
        ) {
            let deadline = Instant::now() + Duration::from_secs(12);
            loop {
                render(app, vec![]);
                if predicate(&app.terminals[0]) {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "Timed out waiting for {label}: {:?}",
                    app.terminals[0].status()
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let key = |key, repeat| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat,
            modifiers: egui::Modifiers::NONE,
        };
        let text = |value: &str| egui::Event::Text(value.into());
        wait_for(&mut app, &mut render, "cmd prompt", |term| {
            !term.starting() && term.visible_line_matches(|line| line.ends_with('>'))
        });
        render(
            &mut app,
            vec![
                text(
                    "nvim --clean -i NONE -n -c \"call setline(1, 'RTNAVabcdef')\" -c \"normal! 0\"",
                ),
                key(Key::Enter, false),
            ],
        );
        wait_for(&mut app, &mut render, "Neovim buffer", |term| {
            term.visible_line_is("RTNAVabcdef")
        });
        render(
            &mut app,
            vec![
                key(Key::ArrowRight, false),
                key(Key::ArrowRight, true),
                key(Key::ArrowRight, true),
                text("i"),
                text("X"),
            ],
        );
        wait_for(
            &mut app,
            &mut render,
            "three right movements and insert",
            |term| term.visible_line_is("RTNXAVabcdef"),
        );
        render(&mut app, vec![key(Key::Escape, false)]);
        wait_for(&mut app, &mut render, "leave insert mode", |term| {
            !term.visible_line_matches(|line| line.contains("-- INSERT --"))
        });
        render(&mut app, vec![text("/abcdef")]);
        wait_for(&mut app, &mut render, "search command", |term| {
            term.visible_line_is("/abcdef")
        });
        render(&mut app, vec![key(Key::Enter, false)]);
        // Search is accepted in normal mode, and command-line entry then quits.
        render(&mut app, vec![text(":q!"), key(Key::Enter, false)]);
        wait_for(&mut app, &mut render, "return to cmd", |term| {
            term.visible_line_matches(|line| line.ends_with('>'))
                && !term.visible_line_is("RTNXAVabcdef")
        });
        render(
            &mut app,
            vec![text("echo RTERM_EDITOR_RETURNED"), key(Key::Enter, false)],
        );
        wait_for(
            &mut app,
            &mut render,
            "shell command after editor exit",
            |term| term.visible_line_is("RTERM_EDITOR_RETURNED"),
        );
        app.terminals[0].mark_closed();
    }

    #[test]
    fn wallpaper_is_exclusive_to_main_terminal_tabs() {
        let context = egui::Context::default();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wallpaper.png");
        image::RgbaImage::from_pixel(64, 32, image::Rgba([32, 128, 255, 255]))
            .save(&path)
            .unwrap();
        let mut settings = Settings::benchmark_defaults();
        settings.set("background_path", path.to_string_lossy().into_owned());
        settings.set("background_enabled", "1".into());
        let mut app = App::with_settings(&context, settings, false);
        assert!(app.wallpaper.is_some());
        let options = app.settings.terminal();
        app.terminals
            .push(Terminal::new(1, "Main".into(), Kind::Local, &options));
        app.active = Some(1);
        app.bottom_shell = Some(Terminal::new(2, "Shell".into(), Kind::Local, &options));
        app.bottom_open = true;
        let mut split = 0.0;
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let bounds = ui.max_rect();
                    split = bounds.bottom() - 200.0;
                    app.center(ui, bounds.with_max_y(split));
                    app.bottom_panel(ui, bounds.with_min_y(split));
                });
            },
        );
        let primitives = context.tessellate(output.shapes, output.pixels_per_point);
        let images: Vec<_> = primitives
            .iter()
            .filter_map(|primitive| match &primitive.primitive {
                egui::epaint::Primitive::Mesh(mesh)
                    if mesh.texture_id != egui::TextureId::default() =>
                {
                    Some(mesh.calc_bounds())
                }
                _ => None,
            })
            .collect();
        assert!(!images.is_empty(), "main terminal must retain wallpaper");
        assert!(
            images.iter().all(|bounds| bounds.bottom() <= split),
            "wallpaper must not enter bottom shell"
        );
    }

    #[test]
    fn remote_connection_status_does_not_enter_terminal_grid() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
        };
        let context = egui::Context::default();
        let mut app = App::with_settings(&context, Settings::benchmark_defaults(), false);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        app.connect(
            Profile {
                name: "Test remote".into(),
                connection: Connection::Telnet(crate::models::TelnetConfig {
                    host: "127.0.0.1".into(),
                    port: listener.local_addr().unwrap().port(),
                }),
            },
            None,
        );
        let render = |app: &mut App, font: f32| {
            app.settings.set("font_size", font.to_string());
            context.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(420.0, 450.0))),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        app.center(ui, ui.max_rect());
                    });
                },
            )
        };
        for font in [40.0, 12.0, 40.0] {
            let output = render(&mut app, font);
            assert_eq!(app.terminals[0].status().1, "Ln 1, Col 1");
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == "Connecting…")));
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "local test connection timed out");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("{error}"),
            }
        };
        stream.write_all(b"READY> ").unwrap();
        while app.terminals[0].status().1 != "Ln 1, Col 8" {
            app.poll(&context);
            assert!(
                Instant::now() < deadline,
                "first remote prompt missing: {:?}",
                app.terminals[0].status()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let output = render(&mut app, 40.0);
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text.contains("READY>"))));
        assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text.contains("Connecting"))));

        // Recreate the focus race: requesting focus again resets egui's
        // EventFilter. Exercise the actual eframe raw hook, not just a steady
        // terminal widget, and check bytes at the other end of the socket.
        let focus = context.memory(|m| m.focused()).expect("terminal focused");
        assert_eq!(context.data(|data| data.get_temp::<u64>(focus)), app.active);
        let key = |key, repeat| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat,
            modifiers: egui::Modifiers::NONE,
        };
        let mut frame = eframe::Frame::_new_kittest();
        for (alternate, expected) in [
            (false, b"\x1b[C\x1b[C\x1b[C\x1b:q\r\n".as_slice()),
            (true, b"\x1bOC\x1bOC\x1bOC\x1b:q\r\n".as_slice()),
        ] {
            if alternate {
                app.terminals[0].receive(b"\x1b[?1049h\x1b[?1h");
            }
            context.memory_mut(|m| m.request_focus(focus));
            let mut raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))),
                events: vec![
                    key(Key::ArrowRight, false),
                    key(Key::ArrowRight, true),
                    key(Key::ArrowRight, true),
                    key(Key::Escape, false),
                    egui::Event::Text(":".into()),
                    egui::Event::Text("q".into()),
                    key(Key::Enter, false),
                ],
                ..Default::default()
            };
            eframe::App::raw_input_hook(&mut app, &context, &mut raw);
            assert_eq!(app.terminal_navigation.len(), 4);
            let _ = context.run(raw, |ctx| eframe::App::update(&mut app, ctx, &mut frame));
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut got = vec![0; expected.len()];
            stream.read_exact(&mut got).unwrap();
            assert_eq!(got, expected);
            assert_eq!(context.memory(|m| m.focused()), Some(focus));
        }
    }

    #[test]
    fn fullscreen_keyboard_zoom_uses_visible_font_and_keeps_ui_scale() {
        let context = egui::Context::default();
        let mut app = App::with_settings(&context, Settings::benchmark_defaults(), false);
        app.settings.set("font_size", "28".into());
        let options = app.settings.terminal();
        app.terminals
            .push(Terminal::new(1, "nload".into(), Kind::Ssh, &options));
        app.active = Some(1);
        // Mark the fixture as connected so prepare_remote does not replace
        // the grid while measuring the initial (still disconnected) pane.
        app.terminals[0].endpoint = Some(123);
        app.terminals[0].receive(b"\x1b[?1049h\x1b[HRTZOOM");
        let mut frame = eframe::Frame::_new_kittest();
        let modifiers = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let key = |key, repeat| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat,
            modifiers,
        };
        let mut render = |app: &mut App, events: Vec<egui::Event>, time: f64| {
            let mut raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))),
                time: Some(time),
                modifiers,
                events,
                ..Default::default()
            };
            eframe::App::raw_input_hook(app, &context, &mut raw);
            context.run(raw, |ctx| eframe::App::update(app, ctx, &mut frame))
        };
        render(&mut app, vec![], 0.0);
        let fitted = app.terminals[0].zoom_base(options.font);
        assert!(
            fitted < options.font && fitted > 10.0,
            "fitted font: {fitted}"
        );
        let original_size = app.terminals[0].size();
        let ui_scale = context.zoom_factor();
        let out = render(
            &mut app,
            vec![key(Key::Minus, false), key(Key::Minus, true)],
            0.1,
        );
        assert_eq!(app.settings.terminal().font, fitted - 2.0);
        assert!(app.terminals[0].size().cols > original_size.cols);
        assert!(out.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.text() == "RTZOOM"
                && text.galley.job.sections[0].format.font_id.size == fitted - 2.0
        )));
        assert_eq!(context.zoom_factor(), ui_scale);
        // Zoom above the original automatic fit, without getting capped.
        render(&mut app, vec![key(Key::Equals, false); 3], 0.2);
        assert_eq!(app.settings.terminal().font, fitted + 1.0);
        assert!(app.terminals[0].size().cols < original_size.cols);
        // A curses redraw can briefly return to the primary screen.
        let size = app.terminals[0].size();
        app.terminals[0].receive(b"\x1b[?1049l");
        render(&mut app, vec![], 0.3);
        assert_eq!(app.terminals[0].size(), size);
        app.terminals[0].receive(b"\x1b[?1049h");
        render(&mut app, vec![key(Key::Minus, false)], 0.4);
        assert_eq!(app.settings.terminal().font, fitted);
        render(&mut app, vec![key(Key::Num0, false)], 0.5);
        assert_eq!(app.settings.terminal().font, 13.0);
        assert_eq!(context.zoom_factor(), ui_scale);
    }

    #[test]
    fn fullscreen_wheel_zoom_and_bottom_keyboard_zoom_are_independent() {
        let context = egui::Context::default();
        let mut app = App::with_settings(&context, Settings::benchmark_defaults(), false);
        app.settings.set("font_size", "28".into());
        let options = app.settings.terminal();
        let mut main = Terminal::new(1, "nload".into(), Kind::Ssh, &options);
        main.endpoint = Some(123);
        main.receive(b"\x1b[?1049h\x1b[HRTMAIN");
        app.terminals.push(main);
        app.active = Some(1);
        app.bottom_shell = Some(Terminal::new(
            2,
            "Shell".into(),
            Kind::Local,
            &app.bottom_options(),
        ));
        app.bottom_open = true;
        let mut frame = eframe::Frame::_new_kittest();
        let modifiers = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let key = |key| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        };
        let wheel = |position, delta| {
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: vec2(0.0, delta),
                    modifiers,
                },
            ]
        };
        let mut render = |app: &mut App, events: Vec<egui::Event>, time: f64| {
            let mut raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))),
                time: Some(time),
                modifiers,
                events,
                ..Default::default()
            };
            eframe::App::raw_input_hook(app, &context, &mut raw);
            context.run(raw, |ctx| eframe::App::update(app, ctx, &mut frame))
        };
        render(&mut app, vec![], 0.0);
        let fitted = app.zoom_base(Some(1));
        assert!(fitted < 28.0 && fitted > 8.0);
        let bottom_size = app.bottom_shell.as_ref().unwrap().size();
        let main_size = app.terminals[0].size();
        let out = render(&mut app, wheel(pos2(600.0, 300.0), -1.0), 0.1);
        assert_eq!(app.zoom_preview(1), Some(fitted - 1.0));
        assert_eq!(app.zoom_preview(2), None);
        assert_eq!(app.terminals[0].size(), main_size);
        assert!(out.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.text() == "RTMAIN"
                && text.galley.job.sections[0].format.font_id.size == fitted - 1.0
        )));
        render(&mut app, vec![], 0.2);
        assert_eq!(app.settings.terminal().font, fitted - 1.0);
        assert!(app.terminals[0].size().cols > main_size.cols);
        assert_eq!(app.bottom_shell.as_ref().unwrap().size(), bottom_size);
        let main_size = app.terminals[0].size();
        // Keyboard zoom follows the currently focused terminal.
        app.bottom_request_focus = true;
        render(&mut app, vec![], 0.3);
        assert!(app.bottom_focus);
        let bottom_font = app.bottom_font;
        render(&mut app, vec![key(Key::Minus)], 0.4);
        assert_eq!(app.bottom_font, bottom_font - 1.0);
        assert_eq!(app.terminals[0].size(), main_size);
        let bottom_size = app.bottom_shell.as_ref().unwrap().size();
        render(&mut app, wheel(pos2(600.0, 650.0), -2.0), 0.5);
        assert_eq!(app.zoom_preview(2), Some(bottom_font - 3.0));
        assert_eq!(app.zoom_preview(1), None);
        assert_eq!(app.bottom_shell.as_ref().unwrap().size(), bottom_size);
        render(&mut app, vec![key(Key::Equals)], 0.51);
        assert_eq!(app.bottom_font, bottom_font - 2.0);
        assert_eq!(app.terminals[0].size(), main_size);
        render(&mut app, vec![key(Key::Num0)], 0.6);
        assert_eq!(app.bottom_font, 13.0);
        assert_eq!(app.settings.get("bottom_font_size"), "13");
        assert_eq!(app.settings.terminal().font, fitted - 1.0);
        assert_eq!(app.terminals[0].size(), main_size);
    }

    #[test]
    fn ctrl_wheel_zooms_only_terminal_and_consumes_scroll() {
        let context = egui::Context::default();
        let mut app = App::with_settings(&context, Settings::benchmark_defaults(), false);
        let options = app.settings.terminal();
        app.terminals
            .push(Terminal::new(1, "Main".into(), Kind::Local, &options));
        app.active = Some(1);
        app.bottom_shell = Some(Terminal::new(2, "Shell".into(), Kind::Local, &options));
        app.bottom_open = true;
        let mut frame = eframe::Frame::_new_kittest();
        let render = |app: &mut App,
                      frame: &mut eframe::Frame,
                      pos: egui::Pos2,
                      delta: f32,
                      ctrl: bool,
                      time: f64| {
            let modifiers = egui::Modifiers {
                ctrl,
                command: ctrl,
                ..Default::default()
            };
            let _ = context.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))),
                    time: Some(time),
                    modifiers,
                    events: vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Line,
                            delta: vec2(0.0, delta),
                            modifiers,
                        },
                    ],
                    ..Default::default()
                },
                |ctx| eframe::App::update(app, ctx, frame),
            );
            app.settings.terminal().font
        };
        let original = options.font;
        assert_eq!(
            render(&mut app, &mut frame, pos2(600.0, 300.0), 1.0, true, 0.0),
            original
        );
        assert_eq!(context.input(|input| input.raw_scroll_delta.y), 0.0);
        assert_eq!(
            render(&mut app, &mut frame, pos2(600.0, 300.0), 1.0, false, 0.01),
            original + 1.0
        );
        assert!(context.input(|input| input.raw_scroll_delta.y) > 0.0);
        assert_eq!(
            render(&mut app, &mut frame, pos2(100.0, 300.0), 1.0, true, 0.02),
            original + 1.0
        );
        assert_eq!(
            render(&mut app, &mut frame, pos2(600.0, 650.0), -1.0, true, 0.03),
            original + 1.0
        );
        assert_eq!(
            render(&mut app, &mut frame, pos2(600.0, 650.0), 0.0, true, 0.12),
            original + 1.0
        );
        assert_eq!(app.bottom_font, original - 1.0);
        let size = app.terminals[0].size();
        // Rapid changes which cancel out must never resize the terminal grid.
        for i in 0..20 {
            let delta = if i % 2 == 0 { 5.0 } else { -5.0 };
            assert_eq!(
                render(
                    &mut app,
                    &mut frame,
                    pos2(600.0, 300.0),
                    delta,
                    true,
                    0.13 + f64::from(i) * 0.01
                ),
                original + 1.0
            );
            assert_eq!(app.terminals[0].size(), size);
        }
        assert!(app.pending_wheel_zoom.is_none());
        assert_eq!(
            render(&mut app, &mut frame, pos2(600.0, 300.0), 100.0, true, 0.40),
            original + 1.0
        );
        assert_eq!(
            render(&mut app, &mut frame, pos2(600.0, 300.0), 0.0, true, 0.49),
            32.0
        );
        assert_eq!(
            render(&mut app, &mut frame, pos2(600.0, 300.0), -100.0, true, 0.50),
            32.0
        );
        assert_eq!(
            render(&mut app, &mut frame, pos2(600.0, 300.0), 0.0, false, 0.51),
            8.0
        );
    }

    #[test]
    fn workspace_and_dialogs_have_unique_widget_ids() {
        let context = egui::Context::default();
        context.options_mut(|options| options.warn_on_id_clash = true);
        let mut app = App::with_settings(&context, Settings::benchmark_defaults(), false);
        let options = app.settings.terminal();
        app.terminals
            .push(Terminal::new(1, "Terminal 1".into(), Kind::Local, &options));
        app.terminals
            .push(Terminal::new(2, "Terminal 2".into(), Kind::Local, &options));
        app.active = Some(1);
        app.bottom_shell = Some(Terminal::new(3, "Shell".into(), Kind::Local, &options));
        app.bottom_open = true;
        let mut frame = eframe::Frame::_new_kittest();
        let check = |app: &mut App, frame: &mut eframe::Frame| {
            for _ in 0..2 {
                let output = context.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))),
                        ..Default::default()
                    },
                    |ctx| eframe::App::update(app, ctx, frame),
                );
                for shape in output.shapes {
                    if let egui::Shape::Text(text) = &shape.shape {
                        let text = text.galley.text();
                        assert!(
                            !text.contains("Second use of") && !text.contains("Double use of"),
                            "{text}"
                        );
                    }
                }
            }
        };
        check(&mut app, &mut frame);
        app.keys.open = true;
        app.show_keys = true;
        check(&mut app, &mut frame);
        app.vault_keys.push(crate::models::SshKey {
            name: "Example key".into(),
            private_key: String::new(),
            public_key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA example".into(),
            key_type: "ed25519".into(),
        });
        check(&mut app, &mut frame);
        for page in 0..6 {
            let mut settings = app.settings.clone();
            settings.test_set_page(page);
            app.dialog = Some(Dialog::settings(&settings, None));
            check(&mut app, &mut frame);
        }
        app.dialog = Some(Dialog::About);
        check(&mut app, &mut frame);
    }

    #[test]
    fn bottom_shell_keeps_its_session_and_restores_focus() {
        let context = egui::Context::default();
        let mut app = App::with_settings(&context, Settings::benchmark_defaults(), false);
        let options = app.settings.terminal();
        app.terminals
            .push(Terminal::new(1, "Main".into(), Kind::Local, &options));
        app.active = Some(1);
        app.bottom_shell = Some(Terminal::new(2, "Shell".into(), Kind::Local, &options));
        app.toggle_bottom_shell();
        assert!(app.bottom_open && app.bottom_focus && app.bottom_request_focus);
        assert_eq!(app.selected().unwrap().tab, 2);
        let mut frame = eframe::Frame::_new_kittest();
        let render = |app: &mut App, frame: &mut eframe::Frame| {
            let _ = context.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))),
                    ..Default::default()
                },
                |ctx| eframe::App::update(app, ctx, frame),
            );
        };
        render(&mut app, &mut frame);
        let shell_focus = context
            .memory(|memory| memory.focused())
            .expect("shell has focus");
        app.dialog = Some(Dialog::About);
        render(&mut app, &mut frame);
        render(&mut app, &mut frame);
        app.dialog = None;
        app.focus_terminal_after_dialog();
        assert!(app.bottom_request_focus && !app.focus_terminal);
        render(&mut app, &mut frame);
        assert!(
            app.bottom_request_focus,
            "wait for the modal layer to clear"
        );
        render(&mut app, &mut frame);
        assert!(!app.bottom_request_focus);
        assert_eq!(context.memory(|memory| memory.focused()), Some(shell_focus));
        app.toggle_bottom_shell();
        assert!(!app.bottom_open && !app.bottom_focus);
        assert_eq!(app.selected().unwrap().tab, 1);
        app.toggle_bottom_shell();
        assert_eq!(app.selected().unwrap().tab, 2);
        app.activate(1);
        assert!(!app.bottom_focus && !app.bottom_request_focus);
        assert_eq!(app.selected().unwrap().tab, 1);
    }
}
