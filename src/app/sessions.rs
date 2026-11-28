use super::*;

#[derive(Default)]
pub(super) struct FolderUi {
    collapsed: BTreeSet<String>,
    draft: Option<FolderDraft>,
}

struct FolderDraft {
    original: Option<String>,
    name: String,
    focus: bool,
    error: Option<String>,
}

const FOLDER_EDITOR_ID: &str = "session-folder-editor";

impl FolderUi {
    pub(super) fn has_draft(&self) -> bool {
        self.draft.is_some()
    }

    pub(super) fn editing(&self, context: &egui::Context) -> bool {
        self.draft.is_some() && context.memory(|m| m.has_focus(egui::Id::new(FOLDER_EDITOR_ID)))
    }

    pub(super) fn cancel(&mut self, context: &egui::Context) {
        self.draft = None;
        let id = egui::Id::new(FOLDER_EDITOR_ID);
        context.memory_mut(|m| m.surrender_focus(id));
        context.data_mut(|data| data.remove::<egui::text_edit::TextEditState>(id));
    }
}

#[derive(Clone)]
struct SessionDrag {
    index: usize,
    fingerprint: blake3::Hash,
}

impl SessionDrag {
    fn new(index: usize, profile: &Profile) -> Option<Self> {
        let bytes = zeroize::Zeroizing::new(serde_json::to_vec(profile).ok()?);
        Some(Self {
            index,
            fingerprint: blake3::hash(&bytes),
        })
    }

    fn valid(&self, profiles: &[Profile]) -> bool {
        profiles
            .get(self.index)
            .and_then(|p| Self::new(self.index, p))
            .is_some_and(|current| current.fingerprint == self.fingerprint)
    }
}

enum SessionAction {
    Connect(usize),
    Edit(usize),
    Delete(usize),
    AddIn(String),
    Rename(String),
    RemoveFolder(String),
    Move(SessionDrag, Option<String>),
    FinishFolder,
    CancelFolder,
}

impl App {
    fn begin_session_folder(&mut self, original: Option<String>) {
        self.session_folder_ui.draft = Some(FolderDraft {
            name: original.clone().unwrap_or_default(),
            original,
            focus: true,
            error: None,
        });
        self.filter.clear();
        self.focus_file_editor();
    }

    // Only saved metadata is changed; preserve profile ordering, credentials,
    // active tabs and pending connection indices. Roll back on a failed save.
    fn change_session_folders(
        &mut self,
        change: impl FnOnce(&mut BTreeSet<String>, &mut [Profile]),
    ) -> Result<(), String> {
        self.change_saved_metadata(change, |app| {
            // Fixtures must never write the user's session library.
            if app.persist_history {
                app.save_profiles()
            } else {
                Ok(())
            }
        })
    }

    fn change_saved_metadata(
        &mut self,
        change: impl FnOnce(&mut BTreeSet<String>, &mut [Profile]),
        save: impl FnOnce(&mut Self) -> Result<(), String>,
    ) -> Result<(), String> {
        if self.locked {
            return Err("Unlock the vault first".into());
        }
        let old_folders = self.session_folders.clone();
        let previous: Vec<_> = self
            .profiles
            .iter()
            .map(|p| (p.folder.clone(), p.name.clone(), p.network_port()))
            .collect();
        change(&mut self.session_folders, &mut self.profiles);
        if let Err(error) = save(self) {
            self.session_folders = old_folders;
            for (profile, (folder, name, port)) in self.profiles.iter_mut().zip(previous) {
                profile.folder = folder;
                profile.name = name;
                if let Some(port) = port {
                    profile.set_network_port(port);
                }
            }
            return Err(error);
        }
        Ok(())
    }

    fn finish_session_folder(&mut self) -> Result<(), String> {
        let draft = self
            .session_folder_ui
            .draft
            .as_ref()
            .ok_or("No folder editor")?;
        let name = draft.name.trim().to_owned();
        let original = draft.original.clone();
        self.save_session_folder(original, name)
    }

    pub(super) fn save_session_folder(
        &mut self,
        original: Option<String>,
        name: String,
    ) -> Result<(), String> {
        if self.locked {
            return Err("Unlock the vault first".into());
        }
        if !storage::valid_session_folder(&name) {
            return Err("Enter a folder name (1–128 characters)".into());
        }
        if original.as_ref() == Some(&name) {
            return Ok(());
        }
        if self.session_folders.contains(&name) {
            return Err("A folder with that name already exists".into());
        }
        if original
            .as_ref()
            .is_some_and(|old| !self.session_folders.contains(old))
        {
            return Err("Folder no longer exists".into());
        }
        self.change_session_folders(|folders, profiles| {
            if let Some(old) = &original {
                folders.remove(old);
                for profile in profiles {
                    if profile.folder.as_ref() == Some(old) {
                        profile.folder = Some(name.clone());
                    }
                }
            }
            folders.insert(name.clone());
        })?;
        if let Some(old) = original {
            self.session_folder_ui.collapsed.remove(&old);
        }
        self.session_folder_ui.collapsed.remove(&name);
        Ok(())
    }

    fn move_saved_session(
        &mut self,
        drag: SessionDrag,
        folder: Option<String>,
    ) -> Result<(), String> {
        if !drag.valid(&self.profiles) {
            return Err("Session changed; try again".into());
        }
        self.move_saved_session_index(drag.index, folder)
    }

    pub(super) fn move_saved_session_index(
        &mut self,
        index: usize,
        folder: Option<String>,
    ) -> Result<(), String> {
        if self.locked || index >= self.profiles.len() {
            return Err("Session unavailable".into());
        }
        if folder
            .as_ref()
            .is_some_and(|name| !self.session_folders.contains(name))
        {
            return Err("Folder no longer exists".into());
        }
        if self.profiles[index].folder == folder {
            return Ok(());
        }
        self.change_session_folders(|_, profiles| profiles[index].folder = folder.clone())?;
        if let Some(folder) = folder {
            self.session_folder_ui.collapsed.remove(&folder);
        }
        Ok(())
    }

    pub(super) fn rename_saved_session(
        &mut self,
        index: usize,
        name: String,
    ) -> Result<(), String> {
        if self.locked || !storage::valid_session_folder(&name) {
            return Err("Invalid name or locked vault".into());
        }
        if index >= self.profiles.len() {
            return Err("Session unavailable".into());
        }
        self.change_session_folders(|_, profiles| profiles[index].name = name)
    }

    pub(super) fn set_saved_session_port(&mut self, index: usize, port: u16) -> Result<(), String> {
        if self.locked || port == 0 {
            return Err("Invalid port or locked vault".into());
        }
        self.profiles
            .get(index)
            .ok_or("Session unavailable")?
            .network_port()
            .ok_or("Profile has no network port")?;
        self.change_session_folders(|_, profiles| profiles[index].set_network_port(port))
    }

    pub(super) fn remove_session_folder(&mut self, name: &str) -> Result<(), String> {
        // Folder deletion is organizational only. Never delete its sessions.
        self.change_session_folders(|folders, profiles| {
            folders.remove(name);
            for profile in profiles {
                if profile.folder.as_deref() == Some(name) {
                    profile.folder = None;
                }
            }
        })?;
        self.session_folder_ui.collapsed.remove(name);
        Ok(())
    }
}

impl App {
    pub(super) fn session_panel(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let t = self.theme;
        theme::band(ui, rect, t.panel, None);

        let header = top_band(rect, 22.0);
        let title =
            header.with_max_x((icon_slot(header, 1).left() - 2.0).max(header.left() + 34.0));
        let drop = ui.interact(title, ui.id().with("session-root-drop"), Sense::hover());
        if self.file_interaction_allowed() && drop.dnd_hover_payload::<SessionDrag>().is_some() {
            theme::band(ui, title, t.hover, None);
        }
        header_label(ui, title, "Session", t);
        let drop = drop.on_hover_text(crate::i18n::tr(ui, "Drop here to ungroup a session"));
        if self.file_interaction_allowed()
            && let Some(drag) = drop.dnd_release_payload::<SessionDrag>()
            && let Err(error) = self.move_saved_session((*drag).clone(), None)
        {
            self.fail(error);
        }
        if self.file_interaction_allowed()
            && theme::icon_button(ui, icon_slot(header, 1), "New Folder", Icon::FolderAdd, t)
                .clicked()
        {
            self.begin_session_folder(None);
        }
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
        let enabled = self.file_interaction_allowed();
        let filter = self.filter.to_lowercase();
        let mut action = None;
        let mut groups: BTreeMap<Option<&str>, Vec<usize>> = BTreeMap::new();
        for (index, profile) in self.profiles.iter().enumerate() {
            let folder = profile.folder.as_deref();
            if filter.is_empty()
                || profile.name.to_lowercase().contains(&filter)
                || profile.subtitle().to_lowercase().contains(&filter)
                || folder.is_some_and(|name| name.to_lowercase().contains(&filter))
            {
                groups.entry(folder).or_default().push(index);
            }
        }
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("saved-sessions")
                .max_rect(list),
        );
        child.set_clip_rect(list);
        theme::compact_list(list.height()).show(&mut child, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let mut matches = 0;
            if enabled
                && self
                    .session_folder_ui
                    .draft
                    .as_ref()
                    .is_some_and(|draft| draft.original.is_none())
            {
                matches += 1;
                folder_editor(
                    ui,
                    t,
                    self.session_folder_ui.draft.as_mut().unwrap(),
                    &mut action,
                );
            }
            for folder in &self.session_folders {
                let entries = groups
                    .get(&Some(folder.as_str()))
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                if !filter.is_empty()
                    && entries.is_empty()
                    && !folder.to_lowercase().contains(&filter)
                {
                    continue;
                }
                matches += 1;
                if enabled
                    && self
                        .session_folder_ui
                        .draft
                        .as_ref()
                        .is_some_and(|draft| draft.original.as_ref() == Some(folder))
                {
                    folder_editor(
                        ui,
                        t,
                        self.session_folder_ui.draft.as_mut().unwrap(),
                        &mut action,
                    );
                } else {
                    let expanded =
                        !self.session_folder_ui.collapsed.contains(folder) || !filter.is_empty();
                    let (row, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
                    let response = ui
                        .interact(
                            row,
                            ui.id().with(("session-folder", folder)),
                            Sense::click(),
                        )
                        .on_hover_text(folder);
                    if response.hovered()
                        || (enabled && response.dnd_hover_payload::<SessionDrag>().is_some())
                    {
                        theme::band(ui, row, t.hover, None);
                    }
                    let center = pos2(row.left() + 9.0, row.center().y);
                    let arrow = if expanded {
                        vec![
                            center + vec2(-3.0, -2.0),
                            center + vec2(0.0, 2.0),
                            center + vec2(3.0, -2.0),
                        ]
                    } else {
                        vec![
                            center + vec2(-2.0, -3.0),
                            center + vec2(2.0, 0.0),
                            center + vec2(-2.0, 3.0),
                        ]
                    };
                    ui.painter()
                        .add(egui::Shape::line(arrow, Stroke::new(1.0_f32, t.muted)));
                    theme::icon(
                        ui,
                        Rect::from_center_size(
                            pos2(row.left() + 23.0, row.center().y),
                            vec2(14.0, 14.0),
                        ),
                        Icon::Folder,
                        t.yellow,
                    );
                    theme::label(
                        ui,
                        row.with_min_x(row.left() + 36.0),
                        folder,
                        t.chrome_font(11.0),
                        t.text,
                        Align2::LEFT_CENTER,
                    );
                    if enabled
                        && response.clicked()
                        && filter.is_empty()
                        && !self.session_folder_ui.collapsed.remove(folder)
                    {
                        self.session_folder_ui.collapsed.insert(folder.clone());
                    }
                    if enabled {
                        if let Some(drag) = response.dnd_release_payload::<SessionDrag>() {
                            action =
                                Some(SessionAction::Move((*drag).clone(), Some(folder.clone())));
                        }
                        response.context_menu(|ui| {
                            for (label, item) in [
                                ("Add Session", SessionAction::AddIn(folder.clone())),
                                ("Rename", SessionAction::Rename(folder.clone())),
                                ("Remove Folder", SessionAction::RemoveFolder(folder.clone())),
                            ] {
                                if ui.button(crate::i18n::tr(ui, label)).clicked() {
                                    action = Some(item);
                                    ui.close();
                                }
                            }
                        });
                    }
                }
                if !self.session_folder_ui.collapsed.contains(folder) || !filter.is_empty() {
                    for &index in entries {
                        session_row(
                            ui,
                            t,
                            index,
                            &self.profiles[index],
                            true,
                            enabled,
                            &self.session_folders,
                            &mut action,
                        );
                    }
                }
            }
            // Give the root a real drop target even if every session belongs
            // to a folder. The title bar remains a shortcut when scrolled.
            let show_root = !self.session_folders.is_empty()
                && (filter.is_empty()
                    || groups.contains_key(&None)
                    || egui::DragAndDrop::has_payload_of_type::<SessionDrag>(ui.ctx()));
            if show_root {
                let (row, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
                root_drop_target(ui, row, t, enabled, &mut action, true);
            }
            if let Some(entries) = groups.get(&None) {
                matches += entries.len();
                for &index in entries {
                    session_row(
                        ui,
                        t,
                        index,
                        &self.profiles[index],
                        false,
                        enabled,
                        &self.session_folders,
                        &mut action,
                    );
                }
            }
            // The visible empty area below all rows also belongs to the root,
            // not the last expanded folder. Never extend the scroll content
            // beyond the visible viewport just to provide a drop target.
            if !self.session_folders.is_empty() {
                let height =
                    (ui.clip_rect().bottom().min(list.bottom()) - ui.cursor().top()).max(0.0);
                if height > 0.0 {
                    let (blank, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
                    root_drop_target(ui, blank, t, enabled, &mut action, false);
                }
            }
            if matches == 0 {
                theme::label(
                    ui,
                    Rect::from_min_size(list.min, vec2(list.width(), 36.0)),
                    crate::i18n::tr(
                        ui,
                        if self.profiles.is_empty() && self.session_folders.is_empty() {
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
        if !enabled {
            return;
        }
        let Some(action) = action else {
            return;
        };
        match action {
            SessionAction::Connect(index) => {
                let profile = self.profiles[index].clone();
                if let Connection::Ssh { config, .. } | Connection::Sftp { config } =
                    &profile.connection
                    && !config.use_agent
                    && config.password.is_none()
                    && config.key_path.is_none()
                    && config.key_name.is_none()
                {
                    self.dialog = Some(Dialog::connect(ConnectForm::editing(
                        &profile,
                        index,
                        &self.settings,
                    )));
                } else if profile.kind().is_ssh() {
                    self.open_ssh_profile(profile);
                } else {
                    self.connect(profile, None);
                }
            }
            SessionAction::Edit(index) => {
                self.dialog = Some(Dialog::connect(ConnectForm::editing(
                    &self.profiles[index],
                    index,
                    &self.settings,
                )))
            }
            SessionAction::Delete(index) => self.dialog = Some(Dialog::DeleteProfile(index)),
            SessionAction::AddIn(folder) => {
                self.dialog = Some(Dialog::connect(
                    ConnectForm::new(&self.settings).in_folder(folder),
                ))
            }
            SessionAction::Rename(folder) => self.begin_session_folder(Some(folder)),
            SessionAction::RemoveFolder(folder) => {
                if let Err(error) = self.remove_session_folder(&folder) {
                    self.fail(error);
                }
            }
            SessionAction::Move(drag, folder) => {
                if let Err(error) = self.move_saved_session(drag, folder) {
                    self.fail(error);
                }
            }
            SessionAction::FinishFolder => match self.finish_session_folder() {
                Ok(()) => self.session_folder_ui.cancel(ui.ctx()),
                Err(error) => {
                    if let Some(draft) = &mut self.session_folder_ui.draft {
                        draft.error = Some(error);
                        draft.focus = true;
                    }
                }
            },
            SessionAction::CancelFolder => self.session_folder_ui.cancel(ui.ctx()),
        }
        ui.ctx().request_repaint();
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

fn folder_editor(
    ui: &mut egui::Ui,
    t: Theme,
    draft: &mut FolderDraft,
    action: &mut Option<SessionAction>,
) {
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    theme::band(ui, row, t.hover, None);
    theme::icon(
        ui,
        Rect::from_center_size(pos2(row.left() + 23.0, row.center().y), vec2(14.0, 14.0)),
        Icon::Folder,
        t.yellow,
    );
    let id = egui::Id::new(FOLDER_EDITOR_ID);
    let initial = std::mem::take(&mut draft.focus);
    if initial {
        ui.ctx()
            .data_mut(|data| data.remove::<egui::text_edit::TextEditState>(id));
        ui.memory_mut(|m| m.request_focus(id));
    }
    let field = row
        .with_min_x(row.left() + 36.0)
        .with_max_x((row.right() - 4.0).max(row.left() + 37.0));
    let mut editor = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("session-folder-field")
            .max_rect(field)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    editor.set_clip_rect(ui.clip_rect().intersect(field));
    editor.visuals_mut().text_cursor.preview = false;
    let response = egui::TextEdit::singleline(&mut draft.name)
        .id(id)
        .font(t.chrome_font(11.0))
        .hint_text(crate::i18n::tr(ui, "New Folder"))
        .margin(egui::Margin::ZERO)
        .frame(false)
        .desired_width(field.width())
        .min_size(field.size())
        .vertical_align(egui::Align::Center)
        .show(&mut editor)
        .response;
    if initial {
        response.request_focus();
        ui.scroll_to_rect(row, None);
    }
    if response.changed() {
        draft.error = None;
    }
    ui.painter().rect_stroke(
        field,
        0.0,
        Stroke::new(
            1.0_f32,
            if draft.error.is_some() {
                t.red
            } else {
                t.accent
            },
        ),
        egui::StrokeKind::Inside,
    );
    if let Some(error) = &draft.error {
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
        theme::label(
            ui,
            rect.shrink2(vec2(4.0, 0.0)),
            error,
            t.chrome_font(10.0),
            t.red,
            Align2::LEFT_CENTER,
        );
        response.on_hover_text(error);
    }
    if !initial && (response.has_focus() || response.lost_focus()) {
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Escape)) {
            *action = Some(SessionAction::CancelFolder);
            return;
        }
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Enter)) {
            *action = Some(SessionAction::FinishFolder);
            return;
        }
    }
    if !initial && !response.has_focus() {
        *action = Some(SessionAction::CancelFolder);
    }
}

fn root_drop_target(
    ui: &mut egui::Ui,
    rect: Rect,
    t: Theme,
    enabled: bool,
    action: &mut Option<SessionAction>,
    label: bool,
) {
    let response = ui.interact(rect, ui.id().with(("session-root", label)), Sense::hover());
    if enabled && response.dnd_hover_payload::<SessionDrag>().is_some() {
        theme::band(ui, rect, t.hover, None);
        if let Some(drag) = response.dnd_release_payload::<SessionDrag>() {
            *action = Some(SessionAction::Move((*drag).clone(), None));
        }
    }
    if label {
        theme::label(
            ui,
            rect.with_min_x(rect.left() + 12.0),
            crate::i18n::tr(ui, "Ungrouped"),
            t.chrome_font(10.0),
            t.muted,
            Align2::LEFT_CENTER,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn session_row(
    ui: &mut egui::Ui,
    t: Theme,
    index: usize,
    profile: &Profile,
    indented: bool,
    enabled: bool,
    folders: &BTreeSet<String>,
    action: &mut Option<SessionAction>,
) {
    let detail = profile.subtitle();
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    let response = ui
        .interact(
            row,
            ui.id().with(("saved-session-row", index)),
            Sense::click_and_drag(),
        )
        .on_hover_text(&detail);
    if response.contains_pointer() {
        theme::band(ui, row, t.hover, None);
    }
    let indent = if indented { 20.0 } else { 0.0 };
    theme::icon(
        ui,
        Rect::from_center_size(
            pos2(row.left() + 16.0 + indent, row.center().y),
            vec2(16.0, 16.0),
        ),
        profile.kind().icon(t).0,
        profile.kind().icon(t).1,
    );
    let text = row
        .with_min_x(row.left() + 32.0 + indent)
        .with_max_x((row.right() - 40.0).max(row.left() + 33.0 + indent));
    theme::label(
        ui,
        text,
        &profile.name,
        t.chrome_font(11.0),
        t.text,
        Align2::LEFT_CENTER,
    );
    if !enabled {
        return;
    }
    if !indented && let Some(drag) = response.dnd_release_payload::<SessionDrag>() {
        *action = Some(SessionAction::Move((*drag).clone(), None));
    }
    if response.double_clicked() {
        *action = Some(SessionAction::Connect(index));
    }
    if response.drag_started()
        && let Some(drag) = SessionDrag::new(index, profile)
    {
        response.dnd_set_drag_payload(drag);
    }
    response.context_menu(|ui| {
        for (label, item) in [
            ("Connect", SessionAction::Connect(index)),
            ("Edit", SessionAction::Edit(index)),
            ("Delete", SessionAction::Delete(index)),
        ] {
            if ui.button(crate::i18n::tr(ui, label)).clicked() {
                *action = Some(item);
                ui.close();
            }
        }
        if !folders.is_empty() {
            ui.menu_button(crate::i18n::tr(ui, "Move to Folder"), |ui| {
                let root = ui.add_enabled(
                    profile.folder.is_some(),
                    egui::Button::new(crate::i18n::tr(ui, "Ungrouped")),
                );
                if root.clicked()
                    && let Some(drag) = SessionDrag::new(index, profile)
                {
                    *action = Some(SessionAction::Move(drag, None));
                    ui.close();
                }
                for folder in folders {
                    if ui
                        .add_enabled(
                            profile.folder.as_ref() != Some(folder),
                            egui::Button::new(folder),
                        )
                        .clicked()
                        && let Some(drag) = SessionDrag::new(index, profile)
                    {
                        *action = Some(SessionAction::Move(drag, Some(folder.clone())));
                        ui.close();
                    }
                }
            });
        }
    });
    if response.contains_pointer() {
        ui.push_id(index, |ui| {
            if theme::icon_button(ui, icon_slot(row, 1), "Edit Session", Icon::Edit, t).clicked() {
                *action = Some(SessionAction::Edit(index));
            }
            if theme::icon_button(ui, icon_slot(row, 0), "Delete Session", Icon::Trash, t).clicked()
            {
                *action = Some(SessionAction::Delete(index));
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

#[cfg(test)]
#[path = "../../tests/unit/app/sessions.rs"]
mod tests;
