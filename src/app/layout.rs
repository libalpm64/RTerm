use super::{menu::terminal_menu, *};

fn contrast_text(background: Color32) -> Color32 {
    let linear = |channel: u8| {
        let channel = f32::from(channel) / 255.0;
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.2126 * linear(background.r())
        + 0.7152 * linear(background.g())
        + 0.0722 * linear(background.b());
    if luminance > 0.179 {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

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
        let mut split = None;
        let mut hide = None;
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
                        if !active && !self.show_keys && self.splits.contains(terminal.tab) {
                            ui.painter().hline(
                                tab.x_range(),
                                tab.bottom() - 1.0,
                                Stroke::new(1.0_f32, t.accent.gamma_multiply(0.55)),
                            );
                        }
                        let (glyph, protocol_color) = terminal.kind.icon(t);
                        let color = if terminal.closed {
                            t.dim
                        } else {
                            protocol_color
                        };
                        theme::icon(
                            ui,
                            Rect::from_center_size(
                                pos2(tab.left() + 12.0, tab.center().y),
                                vec2(13.0, 13.0),
                            ),
                            glyph,
                            color,
                        );
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
                            if !self.show_keys
                                && !self.active_is_sftp()
                                && terminal.kind != Kind::Sftp
                                && self.active != Some(terminal.tab)
                                && !self.splits.contains(terminal.tab)
                                && self.splits.can_split()
                                && self.active.is_some()
                            {
                                for (label, direction) in [
                                    ("Split Right", splits::Direction::Right),
                                    ("Split Down", splits::Direction::Down),
                                ] {
                                    if ui.button(crate::i18n::tr(ui, label)).clicked() {
                                        split = Some((terminal.tab, direction));
                                        ui.close();
                                    }
                                }
                            }
                            if self.splits.contains(terminal.tab)
                                && ui
                                    .button(crate::i18n::tr(ui, "Remove from Split"))
                                    .clicked()
                            {
                                hide = Some(terminal.tab);
                                ui.close();
                            }
                            if terminal.kind == Kind::Ssh
                                && ui.button(crate::i18n::tr(ui, "AI Activity…")).clicked()
                            {
                                activity = Some(terminal.tab);
                                ui.close();
                            }
                            if terminal.kind.is_ssh()
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
        if ui.input(|input| input.pointer.any_released()) && ui.rect_contains_pointer(rect) {
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
        if let Some((tab, direction)) = split {
            self.split_existing(tab, direction);
        }
        if let Some(tab) = hide {
            self.hide_pane(tab);
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
            if term.kind.is_ssh() && self.ssh_connection_state(term) == "reconnecting" {
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
            if !self.file_workspace_active() {
                let (dimensions, cursor) = term.status();
                fields.push(cursor);
                fields.push(dimensions);
            }
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
        let input = self.dialog.is_none()
            && self.message.is_none()
            && !self.host_trust.blocking()
            && !self.agent.blocks_terminal_input()
            && !egui::Popup::is_any_open(ui.ctx())
            && ui.memory(|memory| memory.allows_interaction(ui.layer_id()));
        if input && let Some(tab) = self.bottom_shell.as_ref().map(|term| term.tab) {
            self.wheel_zoom(ui, body, tab);
        }
        let options = self.bottom_options();
        let preview = self
            .bottom_shell
            .as_ref()
            .and_then(|term| self.zoom_preview(term.tab));
        if let Some(term) = &mut self.bottom_shell {
            let request_focus = self.bottom_request_focus && input;
            if request_focus {
                self.bottom_request_focus = false;
                self.bottom_focus = true;
                self.focus_terminal = false;
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
                input && self.bottom_focus,
                input,
                request_focus,
                preview,
                None, // Wallpaper is exclusive to the main terminal tabs.
            );
            let action = terminal_menu(term.context_menu(&response), t);
            self.remember_history(history);
            if let Some(error) = error {
                self.fail(error);
            }
            if let Some(action) = action {
                self.bottom_focus = true;
                self.run_action(action, ui.ctx());
            }
        }
    }

    fn center(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let body = if self.file_workspace_active() && self.explorer.expanded {
            rect
        } else {
            let tabs = top_band(rect, 23.0);
            self.tab_bar(ui, tabs);
            rect.with_min_y(tabs.bottom())
        };
        if self.show_keys {
            if ui.input(|input| input.pointer.any_released()) {
                self.drag_tab = None;
            }
            self.key_panel(ui, body);

            return;
        }
        if self.file_workspace_active() {
            self.focus_terminal = false;
            self.drag_tab = None;
            let queue_bar = body.with_min_y((body.bottom() - 26.0).max(body.top()));
            let workspace = body.with_max_y(queue_bar.top());
            let max_height = (workspace.height() * 0.68).max(0.0);
            let min_height = 96.0_f32.min(max_height);
            let queue_height = self.transfer_queue_height.clamp(min_height, max_height);
            let reserve = if self.transfer_queue_open {
                queue_height
            } else {
                0.0
            };
            let divider_height = if reserve > 0.0 {
                6.0_f32.min(workspace.height() - reserve)
            } else {
                0.0
            };
            let files = workspace.with_max_y(workspace.bottom() - reserve - divider_height);
            self.explorer_workspace(ui, files);
            if reserve > 0.0 {
                let divider = workspace
                    .with_min_y(files.bottom())
                    .with_max_y(files.bottom() + divider_height);
                let response = ui
                    .interact(
                        divider,
                        ui.id().with("transfer-queue-divider"),
                        Sense::drag(),
                    )
                    .on_hover_cursor(egui::CursorIcon::ResizeVertical)
                    .on_hover_text(crate::i18n::tr(ui, "Drag to resize transfer queue"));
                theme::band(
                    ui,
                    divider,
                    if response.hovered() || response.dragged() {
                        self.theme.accent
                    } else {
                        self.theme.bg
                    },
                    None,
                );
                ui.painter().hline(
                    (divider.center().x - 20.0)..=(divider.center().x + 20.0),
                    divider.center().y,
                    Stroke::new(1.0_f32, self.theme.muted),
                );
                if response.dragged() {
                    self.transfer_queue_height =
                        (queue_height - response.drag_delta().y).clamp(min_height, max_height);
                    ui.ctx().request_repaint();
                }
                self.transfer_queue(ui, workspace.with_min_y(divider.bottom()));
            } else if !self.transfer_queue_open {
                // Float above the file-list footer, never reserve space for transfers.
                self.transfer_overlay(
                    ui,
                    files.with_max_y((files.bottom() - 24.0).max(files.top())),
                );
            }
            self.transfer_queue_bar(ui, queue_bar);
            return;
        }
        let toolbar = top_band(body, 22.0);
        self.toolbar(ui, toolbar);
        let body = rect.with_min_y(toolbar.bottom());
        let input = self.dialog.is_none()
            && self.message.is_none()
            && !self.host_trust.blocking()
            && !self.agent.blocks_terminal_input()
            && !egui::Popup::is_any_open(ui.ctx())
            && ui.memory(|memory| memory.allows_interaction(ui.layer_id()));
        let mut panes = self.splits.layout(ui, body, self.active, self.theme, input);
        if self.splits.divider_interacted {
            self.focus_terminal_after_dialog();
        }
        // A tab dropped on an existing pane replaces it, or swaps the two
        // visible sessions. Neither case creates another transport.
        if ui.input(|input| input.pointer.any_released())
            && let Some(tab) = self.drag_tab.take()
            && input
            && self
                .terminals
                .iter()
                .any(|term| term.tab == tab && term.kind != Kind::Sftp)
            && let Some(&(target, _)) = panes
                .iter()
                .find(|(_, rect)| ui.rect_contains_pointer(*rect))
        {
            let swap = self.splits.contains(tab);
            self.splits.place(target, tab);
            self.activate(tab);
            for (id, _) in &mut panes {
                if *id == target {
                    *id = tab;
                } else if swap && *id == tab {
                    *id = target;
                }
            }
        }
        let split = panes.len() > 1;
        let header = |rect: Rect| top_band(rect, 22.0);
        let hide_button = |rect: Rect| {
            let bar = header(rect);
            Rect::from_min_size(
                pos2(bar.right() - 24.0, bar.top()),
                vec2(24.0, bar.height()),
            )
        };
        let terminal_rect = |rect: Rect| {
            if split {
                rect.with_min_y(header(rect).bottom())
            } else {
                rect
            }
        };
        // Decide the keyboard owner before drawing any terminal. In particular,
        // a click and text arriving in one frame must not be handled by two PTYs.
        if input
            && ui.input(|input| input.pointer.any_pressed() || input.pointer.any_click())
            && let Some(&(tab, _)) = panes.iter().find(|(_, rect)| {
                ui.rect_contains_pointer(*rect)
                    && !(split && ui.rect_contains_pointer(hide_button(*rect)))
            })
        {
            self.activate(tab);
        }
        if input {
            for &(tab, rect) in &panes {
                if ui.rect_contains_pointer(terminal_rect(rect)) {
                    self.wheel_zoom(ui, terminal_rect(rect), tab);
                    break;
                }
            }
        }
        let options = self.settings.terminal();
        let mut startup_failure = None;
        // Start queued transports at the actual pane size. Hidden tabs still
        // start at the normal workspace size, as before.
        for term in &mut self.terminals {
            let bounds = panes
                .iter()
                .find(|(tab, _)| *tab == term.tab)
                .map_or(body, |(_, rect)| terminal_rect(*rect));
            term.prepare_remote(ui, bounds, &options);
            if let Err(error) = term.prepare_local(ui, bounds, &options, &self.backend) {
                term.mark_closed();
                term.receive(format!("Could not start the shell: {error}\r\n").as_bytes());
                self.local_profiles.remove(&term.tab);
                startup_failure = Some(error);
            }
        }
        if let Some(error) = startup_failure {
            self.fail(error);
        }
        if !panes.is_empty() {
            let input = input && self.message.is_none();
            let request_focus = self.focus_terminal && input && self.active.is_some();
            if request_focus {
                self.focus_terminal = false;
                self.bottom_focus = false;
            } else if self.focus_terminal && self.dialog.is_none() && self.message.is_none() {
                ui.ctx().request_repaint();
            }
            let keyboard_owner = self.active.filter(|_| !self.bottom_focus);
            let mut action = None;
            let mut hide = None;
            let clip = ui.clip_rect();
            for (tab, pane) in panes {
                let Some(index) = self.terminals.iter().position(|term| term.tab == tab) else {
                    continue;
                };
                ui.set_clip_rect(clip.intersect(pane));
                let bounds = terminal_rect(pane);
                if split {
                    let bar = header(pane);
                    let focused = self.active == Some(tab) && !self.bottom_focus;
                    theme::band(
                        ui,
                        bar,
                        if focused {
                            self.theme.hover
                        } else {
                            self.theme.bar
                        },
                        Some(self.theme.border),
                    );
                    if focused {
                        ui.painter().hline(
                            bar.x_range(),
                            bar.top() + 0.5,
                            Stroke::new(2.0_f32, self.theme.accent),
                        );
                    }
                    let name =
                        crate::agent_upload::literal_text(self.terminals[index].name.as_bytes());
                    theme::label(
                        ui,
                        bar.with_min_x(bar.left() + 8.0)
                            .with_max_x(bar.right() - 26.0),
                        &name,
                        self.theme.chrome_font(11.0),
                        if focused {
                            self.theme.text
                        } else {
                            self.theme.muted
                        },
                        Align2::LEFT_CENTER,
                    );
                    let button = hide_button(pane);
                    if input {
                        let response = ui
                            .push_id(("hide-pane", tab), |ui| {
                                theme::icon_button(
                                    ui,
                                    button,
                                    "Remove from Split",
                                    Icon::Close,
                                    self.theme,
                                )
                            })
                            .inner;
                        if response.clicked() {
                            hide = Some(tab);
                        }
                    } else {
                        theme::icon(
                            ui,
                            Rect::from_center_size(button.center(), vec2(14.0, 14.0)),
                            Icon::Close,
                            self.theme.dim,
                        );
                    }
                }
                let preview = self.zoom_preview(tab);
                let (response, history, error) = self.terminals[index].show(
                    ui,
                    bounds,
                    &self.backend,
                    &options,
                    &self.highlighter,
                    input && self.message.is_none() && keyboard_owner == Some(tab),
                    input && self.message.is_none(),
                    request_focus && keyboard_owner == Some(tab),
                    preview,
                    self.wallpaper.as_ref(),
                );
                if self.terminals[index].kind != Kind::Local
                    && self.terminals[index].endpoint.is_none()
                    && self.terminals[index].starting()
                {
                    theme::label(
                        ui,
                        Rect::from_center_size(bounds.center(), vec2(bounds.width(), 24.0)),
                        crate::i18n::tr(ui, "Connecting…"),
                        self.theme.chrome_font(12.0),
                        self.theme.muted,
                        Align2::CENTER_CENTER,
                    );
                }
                ui.set_clip_rect(clip);
                self.remember_history(history);
                if let Some(error) = error {
                    self.fail(error);
                }
                if self.active == Some(tab)
                    && let Some(selected) =
                        terminal_menu(self.terminals[index].context_menu(&response), self.theme)
                {
                    action = Some(selected);
                }
            }
            if let Some(tab) = hide {
                self.hide_pane(tab);
            }
            if let Some(action) = action {
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
                    terminal.kind.icon(t).0,
                    if terminal.closed {
                        t.dim
                    } else {
                        terminal.kind.icon(t).1
                    },
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

    fn transfer_queue_bar(&mut self, ui: &mut egui::Ui, bounds: Rect) {
        let t = self.theme;
        theme::band(ui, bounds, t.bar, Some(t.border));
        let arrow = if self.transfer_queue_open {
            "▼"
        } else {
            "▲"
        };
        let caption = format!("{arrow}  {}", crate::i18n::tr(ui, "Transfer queue"));
        let font = t.chrome_font(11.0);
        let toggle_width =
            (theme::text_width(ui, &caption, font.clone()) + 24.0).min(bounds.width());
        let toggle = bounds.with_max_x(bounds.left() + toggle_width);
        let response = ui
            .interact(
                toggle,
                ui.id().with("transfer-queue-toggle"),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.hovered() {
            theme::band(ui, toggle, t.hover, None);
        }
        theme::label(
            ui,
            toggle,
            &caption,
            font.clone(),
            t.text,
            Align2::CENTER_CENTER,
        );
        if response
            .on_hover_text(crate::i18n::tr(
                ui,
                if self.transfer_queue_open {
                    "Hide transfer queue"
                } else {
                    "Show transfer queue"
                },
            ))
            .clicked()
        {
            self.transfer_queue_open = !self.transfer_queue_open;
            ui.ctx().request_repaint();
        }
        let rest = bounds.with_min_x(toggle.right());
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("queue-filter-buttons")
                .max_rect(rest),
        );
        child.set_clip_rect(rest);
        egui::ScrollArea::horizontal()
            .id_salt("queue-filter-scroll")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;
                    for (filter, label) in TransferFilter::OPTIONS {
                        let count = self
                            .transfers
                            .values()
                            .filter(|row| filter.matches(row))
                            .count();
                        let label = format!("{count} {}", crate::i18n::tr(ui, label));
                        let selected = self.transfer_queue_open && self.transfer_filter == filter;
                        let color = if filter == TransferFilter::Failed && count > 0 {
                            t.red
                        } else {
                            t.text
                        };
                        let width = theme::text_width(ui, &label, font.clone()) + 18.0;
                        let response = ui.add_sized(
                            [width, 24.0],
                            egui::Button::new(
                                egui::RichText::new(label).font(font.clone()).color(color),
                            )
                            .selected(selected)
                            .fill(if selected {
                                t.hover
                            } else {
                                t.bar
                            }),
                        );
                        if response.clicked() {
                            self.transfer_filter = filter;
                            self.transfer_queue_open = true;
                            ui.ctx().request_repaint();
                        }
                    }
                });
            });
    }

    fn transfer_queue(&mut self, ui: &mut egui::Ui, bounds: Rect) {
        let t = self.theme;
        theme::band(ui, bounds, t.panel, Some(t.border));
        let header = top_band(bounds, 26.0);
        theme::band(ui, header, t.bar, Some(t.border));
        theme::label(
            ui,
            header
                .with_min_x(header.left() + 8.0)
                .with_max_x((header.right() - 64.0).max(header.left())),
            crate::i18n::tr(ui, "Transfer queue"),
            t.chrome_font(11.0),
            t.text,
            Align2::LEFT_CENTER,
        );
        if theme::icon_button(ui, icon_slot(header, 0), "Close", Icon::Close, t).clicked() {
            self.transfer_queue_open = false;
            ui.ctx().request_repaint();
        }
        if theme::icon_button(
            ui,
            icon_slot(header, 1).translate(vec2(-6.0, 0.0)),
            "Clear completed transfers",
            Icon::Trash,
            t,
        )
        .clicked()
        {
            self.transfers.retain(|_, row| row.expires.is_none());
        }
        let body = bounds
            .with_min_y(header.bottom().min(bounds.bottom()))
            .shrink2(vec2(8.0, 0.0));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("transfer-queue-list")
                .max_rect(body),
        );
        child.set_clip_rect(body);
        let mut rows: Vec<_> = self
            .transfers
            .iter()
            .filter(|(_, row)| self.transfer_filter.matches(row))
            .collect();
        rows.sort_by_key(|(_, row)| row.expires);
        egui::ScrollArea::vertical()
            .max_height(body.height())
            .show(&mut child, |ui| {
                if rows.is_empty() {
                    ui.label(crate::i18n::tr(ui, "No matching transfers"));
                }
                for (key, row) in rows {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        let (state, color) = if row.error {
                            ("Failed", t.red)
                        } else if row.text.starts_with("Skipped ") {
                            ("Skipped", t.muted)
                        } else if row.expires.is_some() {
                            ("Finished", t.green)
                        } else if row.text.starts_with("Queued ") {
                            ("Queued", t.muted)
                        } else if row.text.starts_with("Waiting ") {
                            ("Waiting", t.yellow)
                        } else {
                            ("Active", t.accent)
                        };
                        let direction = if key.contains(":download:") {
                            "Download"
                        } else {
                            "Upload"
                        };
                        ui.add_sized(
                            [80.0, 20.0],
                            egui::Label::new(crate::i18n::tr(ui, direction)),
                        );
                        ui.add_sized(
                            [82.0, 20.0],
                            egui::Label::new(
                                egui::RichText::new(crate::i18n::tr(ui, state)).color(color),
                            ),
                        );
                        let (track, _) = ui.allocate_exact_size(vec2(105.0, 16.0), Sense::hover());
                        ui.painter().rect_filled(track, 2.0, t.hover);
                        let fraction = (row.percent / 100.0).clamp(0.0, 1.0);
                        let filled = track.with_max_x(track.left() + track.width() * fraction);
                        if fraction > 0.0 {
                            ui.painter().rect_filled(filled, 2.0, color);
                        }
                        ui.painter().rect_stroke(
                            track,
                            2.0,
                            Stroke::new(1.0_f32, t.border),
                            egui::StrokeKind::Inside,
                        );
                        // Use the same centered galley across both halves of
                        // the progress bar, with contrasting text on each.
                        let painter = ui.painter();
                        let galley = painter.layout_no_wrap(
                            format!("{:.0}%", row.percent),
                            t.chrome_font(10.0),
                            t.text,
                        );
                        let position = Align2::CENTER_CENTER
                            .align_size_within_rect(galley.size(), track)
                            .min;
                        let unfilled = track.with_min_x(filled.right());
                        if unfilled.width() > 0.0 {
                            painter
                                .with_clip_rect(unfilled)
                                .galley_with_override_text_color(
                                    position,
                                    galley.clone(),
                                    contrast_text(t.hover),
                                );
                        }
                        if filled.width() > 0.0 {
                            painter
                                .with_clip_rect(filled)
                                .galley_with_override_text_color(
                                    position,
                                    galley,
                                    contrast_text(color),
                                );
                        }
                        let speed = row.rate_bps.map_or_else(
                            || "—".into(),
                            |rate| format!("{}/s", crate::backend::transfer_bytes(rate)),
                        );
                        ui.add_sized(
                            [100.0, 20.0],
                            egui::Label::new(egui::RichText::new(speed).color(t.muted)),
                        );
                        ui.add(egui::Label::new(&row.text).truncate())
                            .on_hover_text(&row.text);
                    });
                }
            });
    }

    fn transfer_overlay(&self, ui: &mut egui::Ui, body: Rect) {
        let now = Instant::now();
        let visible: Vec<_> = self
            .transfers
            .values()
            .filter(|transfer| transfer.expires.is_none_or(|time| time > now))
            .take(5)
            .collect();
        if visible.is_empty() || body.width() < 80.0 || body.height() < 62.0 {
            return;
        }
        let width = (body.width() - 20.0).min(330.0);
        let height =
            (visible.len().min(((body.height() - 24.0) / 38.0) as usize) as f32) * 38.0 + 14.0;
        if height < 52.0 {
            return;
        }
        let area = Rect::from_min_size(
            pos2(body.right() - width - 10.0, body.bottom() - height - 10.0),
            vec2(width, height),
        );
        egui::Area::new(ui.id().with("transfer-card"))
            .order(egui::Order::Foreground)
            .fixed_pos(area.min)
            .default_size(area.size())
            .constrain(false)
            .show(ui.ctx(), |ui| {
                ui.set_min_size(area.size());
                ui.set_max_size(area.size());
                ui.set_clip_rect(area);
                ui.painter().rect(
                    area,
                    0.0,
                    self.theme.panel,
                    Stroke::new(1.0_f32, self.theme.border),
                    egui::StrokeKind::Inside,
                );
                for (index, transfer) in visible
                    .into_iter()
                    .take(((height - 14.0) / 38.0) as usize)
                    .enumerate()
                {
                    let line = Rect::from_min_size(
                        area.min + vec2(9.0, 6.0 + index as f32 * 38.0),
                        vec2(width - 18.0, 22.0),
                    );
                    let color = if transfer.error {
                        self.theme.red
                    } else {
                        self.theme.accent
                    };
                    let label = if let Some(rate) = transfer.rate_bps {
                        format!(
                            "{}/s  ·  {}",
                            crate::backend::transfer_bytes(rate),
                            transfer.text
                        )
                    } else {
                        transfer.text.clone()
                    };
                    theme::label(
                        ui,
                        line,
                        &label,
                        self.theme.chrome_font(11.0),
                        self.theme.text,
                        Align2::LEFT_CENTER,
                    );
                    let track = Rect::from_min_size(line.left_bottom(), vec2(line.width(), 4.0));
                    ui.painter().rect_filled(track, 0.0, self.theme.hover);
                    ui.painter().rect_filled(
                        track.with_max_x(
                            track.left()
                                + track.width() * transfer.percent.clamp(0.0, 100.0) / 100.0,
                        ),
                        0.0,
                        color,
                    );
                }
            });
    }
}

impl App {
    fn capture_terminal_navigation(&mut self, context: &egui::Context, raw: &mut egui::RawInput) {
        self.terminal_navigation.clear();
        if !raw.focused
            || self.file_workspace_active()
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
        let tab = context.memory(|memory| memory.focused()).and_then(|id| {
            context
                .data(|data| data.get_temp::<(egui::Id, u64)>(crate::terminal::terminal_focus_id()))
                .filter(|(owner, _)| *owner == id)
                .map(|(_, tab)| tab)
        });
        let valid = tab.is_some_and(|tab| {
            (!self.show_keys && !self.bottom_focus && self.active == Some(tab))
                || (self.bottom_open
                    && self.bottom_focus
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

impl App {
    fn service_backend_and_agent(&mut self, context: &egui::Context) {
        self.last_service = Instant::now();
        self.poll(context);
        self.poll_agent(context);
    }

    // Keep inactivity-based revocation independent of drawing and enforce it
    // before servicing any queued request, not after accepting that request.
    fn enforce_idle_lock(&mut self, context: &egui::Context) -> Option<Instant> {
        let timeout = self
            .settings
            .get("lock_timeout")
            .parse::<u64>()
            .unwrap_or(0);
        if timeout == 0 || self.locked || !self.settings.on("vault_enabled") {
            return None;
        }
        let deadline = self
            .last_activity
            .checked_add(Duration::from_secs(timeout.saturating_mul(60)))?;
        if Instant::now() >= deadline {
            self.lock();
            if self.locked {
                context.request_repaint();
                return None;
            }
            // A configured vault may have been removed externally.
            return Some(Instant::now() + Duration::from_secs(1));
        }
        Some(deadline)
    }
}

impl eframe::App for App {
    fn native_background_update(&mut self, context: &egui::Context) -> (Option<Instant>, bool) {
        // This hook runs on the same native thread as update(), with no
        // concurrent access to profiles, terminals, approvals or vault keys.
        // Visible frames already service the queue; only fill in missed frames.
        const INTERVAL: Duration = Duration::from_millis(50);
        let idle_wake = self.enforce_idle_lock(context);
        if (self.agent.active() || self.openai_tunnel.is_some())
            && self.last_service.elapsed() >= INTERVAL
        {
            self.service_backend_and_agent(context);
        }
        let agent_wake = (self.agent.active() || self.openai_tunnel.is_some())
            .then(|| self.last_service + INTERVAL);
        let next = idle_wake.into_iter().chain(agent_wake).min();
        (next, self.agent.take_attention_request())
    }

    fn raw_input_hook(&mut self, context: &egui::Context, raw: &mut egui::RawInput) {
        self.capture_terminal_navigation(context, raw);
    }

    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.restore_terminal_navigation(context);
        if context.input(|input| !input.events.is_empty()) {
            self.last_activity = Instant::now();
        }
        self.enforce_idle_lock(context);
        self.service_backend_and_agent(context);
        if !self.file_interaction_allowed()
            || (self.show_keys || (!self.file_workspace_active() && self.sidebar.is_none()))
        {
            self.explorer.folder_draft = None;
        }
        if context.input(|input| input.viewport().close_requested()) && !self.exit_confirmed {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.request_exit(context);
        }
        self.settle_wheel_zoom(context);
        if self.dialog.is_none()
            && self.message.is_none()
            && !self.locked
            && !self.host_trust.blocking()
            && !self.agent.blocks_terminal_input()
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
                if self.file_workspace_active() {
                    self.center(ui, full.with_min_y(menu.bottom()).with_max_y(status.top()));
                    self.status(ui, status);
                    return;
                }
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
                                        self.theme.bg
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
                if self.bottom_open
                    && self.dialog.is_none()
                    && self.message.is_none()
                    && !self.host_trust.blocking()
                    && !self.agent.blocks_terminal_input()
                    && !egui::Popup::is_any_open(context)
                    && ui.memory(|memory| memory.allows_interaction(ui.layer_id()))
                    && (self.bottom_request_focus
                        || (ui.input(|input| {
                            input.pointer.any_pressed() || input.pointer.any_click()
                        }) && ui.rect_contains_pointer(bottom.with_min_y(bottom.top() + 23.0))))
                {
                    // Resolve both requested and pointer-driven bottom focus
                    // before any main terminal can process this frame's keys.
                    self.bottom_focus = true;
                    self.bottom_request_focus = true;
                    self.focus_terminal = false;
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
                && let Some(setup) = self.desktop_shortcut.take()
                && let Err(error) = setup.choose(create)
            {
                self.message = Some(format!("Desktop shortcut setup: {error}"));
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
                    theme.bg
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
#[path = "../../tests/unit/app/layout.rs"]
mod tests;
