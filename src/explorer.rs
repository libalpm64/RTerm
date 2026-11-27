use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use eframe::egui::{self, Align2, FontId, Rect, Sense, Stroke, pos2, vec2};

use crate::{
    app::{App, header_label, icon_slot, top_band},
    backend::{Command, DownloadOptions, Entry, RemoteAction, Reply, join_remote, safe_name},
    dialogs::Dialog,
    storage,
    theme::{self, Icon},
};

pub(crate) struct Explorer {
    pub(crate) remote_active: bool,
    local: Listing<PathBuf>,
    remote: Listing<String>,
    remote_id: Option<u32>,
    loading: bool,
    request: u64,
    path_input: String,
}

struct Listing<T> {
    path: T,
    entries: Vec<Entry>,
    total_bytes: u64,
    non_hidden: Vec<usize>,
    non_hidden_bytes: u64,
    selected: HashSet<String>,
    select_mode: bool,
    error: Option<String>,
}

impl<T> Listing<T> {
    fn new(path: T) -> Self {
        Self {
            path,
            entries: Vec::new(),
            total_bytes: 0,
            non_hidden: Vec::new(),
            non_hidden_bytes: 0,
            selected: HashSet::new(),
            select_mode: false,
            error: None,
        }
    }

    fn replace(&mut self, path: T, mut entries: Vec<Entry>) {
        entries.retain(|entry| entry.name != "." && entry.name != "..");
        // Compute case-folded names once instead of allocating in every comparison.
        entries.sort_by_cached_key(|entry| (!entry.directory, entry.name.to_lowercase()));
        self.total_bytes = entries
            .iter()
            .filter(|entry| !entry.directory)
            .map(|entry| entry.size)
            .fold(0_u64, u64::saturating_add);
        self.non_hidden.clear();
        self.non_hidden_bytes = 0;
        for (index, entry) in entries.iter().enumerate() {
            if !entry.name.starts_with('.') {
                self.non_hidden.push(index);
                if !entry.directory {
                    self.non_hidden_bytes = self.non_hidden_bytes.saturating_add(entry.size);
                }
            }
        }
        self.path = path;
        self.entries = entries;
        self.selected.clear();
        self.error = None;
    }
}

impl Explorer {
    pub(crate) fn new() -> Self {
        let mut explorer = Self {
            remote_active: false,
            local: Listing::new(storage::home_dir()),
            remote: Listing::new(".".into()),
            remote_id: None,
            loading: false,
            request: 0,
            path_input: String::new(),
        };
        explorer.load_local(storage::home_dir());
        explorer
    }

    fn load_local(&mut self, path: PathBuf) {
        match list_local(&path) {
            Ok(entries) => self.local.replace(path, entries),
            Err(error) => self.local.error = Some(error.to_string()),
        }
    }
}

#[derive(Clone)]
pub(crate) enum Target {
    Local { path: PathBuf, entry: Entry },
    Remote { id: u32, path: String, entry: Entry },
}

impl Target {
    pub(crate) fn entry(&self) -> &Entry {
        match self {
            Self::Local { entry, .. } | Self::Remote { entry, .. } => entry,
        }
    }

    pub(crate) fn path(&self) -> String {
        match self {
            Self::Local { path, .. } => path.display().to_string(),
            Self::Remote { path, .. } => path.clone(),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum FileAction {
    Delete,
    Rename,
    Move,
    Info,
}
enum SelectionAction {
    Download,
    Move,
    Delete,
}

impl App {
    pub(crate) fn activate_remote(&mut self, id: Option<u32>) {
        if self.explorer.remote_id != id {
            self.explorer.remote = Listing::new(".".into());
            self.explorer.remote_id = id;
            self.explorer.request += 1;
            self.explorer.loading = false;
        }
        if id.is_some() {
            self.reload_explorer();
        }
    }

    pub(crate) fn refresh_remote_if(&mut self, id: u32) {
        if self.explorer.remote_active && self.explorer.remote_id == Some(id) {
            self.load_remote(id, self.explorer.remote.path.clone());
        }
    }

    fn load_remote(&mut self, id: u32, path: String) {
        self.explorer.loading = true;
        self.explorer.request += 1;
        let request = self.explorer.request;
        self.request(
            Command::Sftp {
                id,
                action: RemoteAction::List(path),
            },
            move |app, result| {
                if request != app.explorer.request {
                    return;
                }
                app.explorer.loading = false;
                match result {
                    Ok(Reply::Files { path, entries }) => {
                        app.explorer.remote.replace(path, entries)
                    }
                    Err(error) => app.explorer.remote.error = Some(error),
                    _ => app.explorer.remote.error = Some("Invalid SFTP listing".into()),
                }
            },
        );
    }

    fn reload_explorer(&mut self) {
        if self.explorer.remote_active {
            if let Some(id) = self.explorer.remote_id {
                self.load_remote(id, self.explorer.remote.path.clone());
            }
        } else {
            self.explorer.load_local(self.explorer.local.path.clone());
        }
    }

    fn navigate(&mut self, path: &str) {
        if self.explorer.remote_active {
            if let Some(id) = self.explorer.remote_id {
                self.load_remote(id, path.to_owned());
            }
        } else {
            self.explorer.load_local(storage::expand_local_path(path));
        }
    }

    pub(crate) fn apply_file_action(
        &mut self,
        action: FileAction,
        entries: Vec<Target>,
        destination: &str,
    ) -> Result<(), String> {
        for item in entries {
            match item {
                Target::Local { path, entry } => match action {
                    FileAction::Delete => {
                        let result = if entry.directory {
                            std::fs::remove_dir_all(&path)
                        } else {
                            std::fs::remove_file(&path)
                        };
                        result.map_err(|error| format!("{}: {error}", path.display()))?;
                    }
                    FileAction::Rename | FileAction::Move => {
                        let target = match action {
                            FileAction::Rename => path.with_file_name(safe_name(destination)?),
                            _ => storage::expand_local_path(destination).join(&entry.name),
                        };
                        std::fs::rename(&path, &target)
                            .map_err(|error| format!("{}: {error}", path.display()))?;
                    }
                    FileAction::Info => {}
                },
                Target::Remote { id, path, entry } => {
                    let remote = match action {
                        FileAction::Delete => RemoteAction::Delete {
                            path,
                            directory: entry.directory,
                        },
                        FileAction::Rename => RemoteAction::Rename {
                            to: join_remote(remote_parent(&path), safe_name(destination)?),
                            from: path,
                        },
                        FileAction::Move => RemoteAction::Rename {
                            to: join_remote(destination, &entry.name),
                            from: path,
                        },
                        FileAction::Info => continue,
                    };
                    self.request(Command::Sftp { id, action: remote }, move |app, result| {
                        if let Err(error) = result {
                            app.fail(error);
                        }
                        app.refresh_remote_if(id);
                    });
                }
            }
        }
        if !self.explorer.remote_active {
            self.reload_explorer();
        }
        Ok(())
    }

    pub(crate) fn download_targets(&mut self, targets: Vec<Target>, folder: PathBuf) {
        let options = DownloadOptions {
            parallel: self.settings.get("sftp_concurrent").parse().unwrap_or(3),
            max_bytes_per_second: self
                .settings
                .get("sftp_max_speed")
                .parse::<u64>()
                .unwrap_or(0)
                .saturating_mul(1024),
            timestamps: self.settings.on("sftp_timestamps"),
        };
        for target in targets {
            let Target::Remote { id, path, entry } = target else {
                continue;
            };
            let name = match safe_name(&entry.name) {
                Ok(name) => name,
                Err(error) => {
                    self.fail(error);
                    continue;
                }
            };
            let action = RemoteAction::Download {
                remote: path,
                local: folder.join(name),
                directory: entry.directory,
                options,
                resume: (!entry.directory && self.settings.on("sftp_resume"))
                    .then(|| self.ssh_transfer_identity(id))
                    .flatten(),
            };
            self.request(Command::Sftp { id, action }, |app, result| {
                if let Err(error) = result {
                    app.fail(error);
                }
            });
        }
    }

    pub(crate) fn upload_files(&mut self, id: u32, paths: Vec<PathBuf>) {
        let resume = self
            .settings
            .on("sftp_resume")
            .then(|| self.ssh_transfer_identity(id))
            .flatten();
        for path in paths {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let remote = join_remote(&self.explorer.remote.path, name);
            self.request(
                Command::Sftp {
                    id,
                    action: RemoteAction::Upload {
                        local: path,
                        remote,
                        resume: resume.clone(),
                    },
                },
                |app, result| {
                    if let Err(error) = result {
                        app.fail(error);
                    }
                },
            );
        }
    }

    pub(crate) fn explorer_panel(&mut self, ui: &mut egui::Ui, bounds: Rect) {
        let t = self.theme;
        let rect = bounds;
        theme::band(ui, bounds, t.panel, None);
        let header = top_band(rect, 22.0);
        theme::band(ui, header, t.panel, Some(t.border));
        header_label(ui, header, "Filer", t);
        let tabs = top_band(rect.with_min_y(header.bottom()), 22.0);
        theme::band(ui, tabs, t.bar, Some(t.border));
        let local = ui.interact(
            tabs.with_max_x(tabs.center().x),
            ui.id().with(("explorer-tab", 0)),
            Sense::click(),
        );
        let sftp = ui.interact(
            tabs.with_min_x(tabs.center().x),
            ui.id().with(("explorer-tab", 1)),
            Sense::click(),
        );
        let show_remote = if local.clicked() {
            false
        } else if sftp.clicked() {
            true
        } else {
            self.explorer.remote_active
        };
        if show_remote != self.explorer.remote_active {
            self.explorer.remote_active = show_remote;
            if show_remote {
                self.activate_remote(self.active_ssh());
            } else {
                self.reload_explorer();
            }
        }
        theme::paint_tab(
            ui,
            &local,
            crate::i18n::tr(ui, "Local"),
            !show_remote,
            t.chrome_font(11.0),
            t,
        );
        theme::paint_tab(ui, &sftp, "SFTP", show_remote, t.chrome_font(11.0), t);

        let remote = self.explorer.remote_active;
        if remote && !self.settings.on("sftp_show_hidden") {
            // Hidden selections must not survive into bulk file operations.
            self.explorer
                .remote
                .selected
                .retain(|name| !name.starts_with('.'));
        }
        let mut start = tabs.bottom();
        if remote {
            let band = top_band(rect.with_min_y(start), 36.0);
            theme::band(ui, band, t.panel, Some(t.border));
            let (status, mark) = if self.explorer.loading {
                (
                    if self.explorer.remote.path == "." {
                        "Connecting…"
                    } else {
                        "Loading folder..."
                    },
                    t.yellow,
                )
            } else if self.explorer.remote_id.is_some() && self.explorer.remote.error.is_none() {
                ("Connected", t.green)
            } else {
                ("Not connected", t.dim)
            };
            ui.painter()
                .circle_filled(pos2(band.left() + 14.0, band.center().y), 3.0, mark);
            theme::label(
                ui,
                band.with_min_x(band.left() + 24.0),
                crate::i18n::tr(ui, status),
                t.chrome_font(11.0),
                t.muted,
                Align2::LEFT_CENTER,
            );
            start = band.bottom();
        }
        if theme::icon_button(ui, icon_slot(header, 0), "Hide Filer", Icon::Close, t).clicked() {
            self.sidebar = None;
        }
        if theme::icon_button(ui, icon_slot(header, 1), "Refresh", Icon::Refresh, t).clicked() {
            self.reload_explorer();
        }
        let mode = if remote {
            &mut self.explorer.remote.select_mode
        } else {
            &mut self.explorer.local.select_mode
        };
        if theme::icon_button(ui, icon_slot(header, 2), "Select Mode", Icon::Check, t).clicked() {
            *mode = !*mode;
        }
        let selecting = *mode;
        if remote
            && let Some(id) = self.explorer.remote_id
            && theme::icon_button(ui, icon_slot(header, 3), "Upload File", Icon::Upload, t)
                .clicked()
            && let Some(files) = rfd::FileDialog::new().pick_files()
        {
            self.upload_files(id, files);
        }
        if remote && self.explorer.remote_id.is_none() {
            let message = top_band(rect.with_min_y(start), 28.0).shrink2(vec2(8.0, 0.0));
            theme::label(
                ui,
                message,
                crate::i18n::tr(ui, "No active SSH session"),
                t.chrome_font(11.0),
                t.muted,
                Align2::LEFT_CENTER,
            );
            return;
        }
        let path_row = top_band(rect.with_min_y(start), 24.0);
        self.explorer_path(ui, path_row);
        let mut list = rect.with_min_y(path_row.bottom());
        if selecting {
            let bar = list.with_min_y(list.bottom() - 56.0);
            list = list.with_max_y(bar.top());
            self.bulk_bar(ui, bar);
        }
        self.explorer_files(ui, list);
    }

    fn explorer_path(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let t = self.theme;
        theme::band(ui, rect, t.panel, Some(t.border));
        let remote = self.explorer.remote_active;
        let path = if remote {
            self.explorer.remote.path.clone()
        } else {
            self.explorer.local.path.display().to_string()
        };
        let body = rect
            .with_min_x(rect.left() + 8.0)
            .with_max_x((rect.right() - 25.0).max(rect.left() + 9.0));
        // Each listing has a stable editor identity. Switching connections
        // cannot leave a draft from the previous remote host in the field.
        let id = ui.make_persistent_id((
            "explorer-path",
            remote,
            remote.then_some(self.explorer.remote_id),
        ));
        if !ui.memory(|memory| memory.has_focus(id)) {
            self.explorer.path_input.clone_from(&path);
        }
        let output = path_field(ui, body, id, &mut self.explorer.path_input, t);
        if output.response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            let path = self.explorer.path_input.clone();
            self.navigate(&path);
        } else if output.response.has_focus()
            && ui.input(|input| input.key_pressed(egui::Key::Escape))
        {
            output.response.surrender_focus();
            self.explorer.path_input = path;
            ui.ctx().request_repaint();
        }
        if theme::icon_button(ui, icon_slot(rect, 0), "Parent Folder", Icon::Up, t).clicked() {
            self.navigate_parent();
        }
    }

    fn navigate_parent(&mut self) {
        if self.explorer.remote_active {
            if let Some(parent) = navigation_parent(&self.explorer.remote.path).map(str::to_owned) {
                self.navigate(&parent);
            }
        } else if let Some(parent) = self.explorer.local.path.parent().map(Path::to_owned) {
            self.explorer.load_local(parent);
        }
    }

    fn explorer_files(&mut self, ui: &mut egui::Ui, mut bounds: Rect) {
        let t = self.theme;
        let remote = self.explorer.remote_active;
        let has_parent = if remote {
            navigation_parent(&self.explorer.remote.path).is_some()
        } else {
            self.explorer.local.path.parent().is_some()
        };
        let show_hidden = !remote || self.settings.on("sftp_show_hidden");
        let (entries, selected, select_mode, error, total, indices) = if remote {
            let listing = &mut self.explorer.remote;
            (
                &listing.entries,
                &mut listing.selected,
                &mut listing.select_mode,
                &listing.error,
                if show_hidden {
                    listing.total_bytes
                } else {
                    listing.non_hidden_bytes
                },
                (!show_hidden).then_some(listing.non_hidden.as_slice()),
            )
        } else {
            let listing = &mut self.explorer.local;
            (
                &listing.entries,
                &mut listing.selected,
                &mut listing.select_mode,
                &listing.error,
                listing.total_bytes,
                None,
            )
        };
        let count = indices.map_or(entries.len(), |indices| indices.len());
        {
            let header = top_band(bounds, 19.0);
            theme::band(ui, header, t.panel, Some(t.border));
            let split = header.left() + header.width() * 0.51;
            theme::label(
                ui,
                header.with_max_x(split).shrink2(vec2(4.0, 0.0)),
                crate::i18n::tr(ui, "Name"),
                t.chrome_font(10.0),
                t.text,
                Align2::LEFT_CENTER,
            );
            if header.width() >= 150.0 {
                ui.painter()
                    .vline(split, header.y_range(), Stroke::new(1.0_f32, t.border));
                theme::label(
                    ui,
                    header.with_min_x(split + 4.0),
                    crate::i18n::tr(ui, "Date Modified"),
                    t.chrome_font(10.0),
                    t.text,
                    Align2::LEFT_CENTER,
                );
            }
            let footer = bounds.with_min_y(bounds.bottom() - 22.0);
            theme::band(ui, footer, t.panel, None);
            ui.painter().hline(
                footer.x_range(),
                footer.top() + 0.5,
                Stroke::new(1.0_f32, t.border),
            );
            theme::label(
                ui,
                footer.shrink2(vec2(4.0, 0.0)),
                &crate::i18n::format(
                    ui,
                    "Items: {count}  {size}",
                    &[("count", &count.to_string()), ("size", &format_size(total))],
                ),
                t.chrome_font(10.0),
                t.muted,
                Align2::LEFT_CENTER,
            );
            bounds = bounds
                .with_min_y(header.bottom())
                .with_max_y(footer.top().max(header.bottom()));
        }
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(("explorer-files", remote))
                .max_rect(bounds),
        );
        child.set_clip_rect(bounds);
        let mut open = None;
        let mut action = None;
        let mut download = None;
        let mut go_parent = false;
        child.spacing_mut().item_spacing.y = 0.0;
        if let Some(error) = error {
            child.colored_label(t.red, error.as_str());
        }
        if has_parent {
            // A synthetic navigation row, never a selectable or mutable file.
            let (row, response) =
                child.allocate_exact_size(vec2(child.available_width(), 18.0), Sense::click());
            if response.hovered() {
                theme::band(&child, row, t.hover, None);
            }
            theme::icon(
                &child,
                Rect::from_center_size(pos2(row.left() + 10.0, row.center().y), vec2(12.0, 12.0)),
                Icon::Folder,
                t.yellow,
            );
            theme::label(
                &child,
                row.with_min_x(row.left() + 19.0),
                "...",
                FontId::monospace(11.0),
                t.text,
                Align2::LEFT_CENTER,
            );
            go_parent = response.clicked();
            response.on_hover_text(crate::i18n::tr(ui, "Go to parent folder"));
        }
        theme::compact_list((bounds.bottom() - child.cursor().top()).max(0.0)).show_rows(
            &mut child,
            18.0,
            count,
            |ui, rows| {
                for row_index in rows {
                    let entry = &entries[indices.map_or(row_index, |indices| indices[row_index])];
                    let (row, response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::click());
                    let checked = selected.contains(&entry.name);
                    if checked || response.hovered() {
                        theme::band(ui, row, t.hover, None);
                    }
                    let color = if checked {
                        t.accent
                    } else if response.hovered() {
                        t.text
                    } else {
                        t.muted
                    };
                    let icon = if checked && *select_mode {
                        Icon::Check
                    } else if entry.directory {
                        Icon::Folder
                    } else {
                        Icon::File
                    };
                    theme::icon(
                        ui,
                        Rect::from_center_size(
                            pos2(row.left() + 10.0, row.center().y),
                            vec2(12.0, 12.0),
                        ),
                        icon,
                        if entry.directory && !checked {
                            t.yellow
                        } else if entry.directory || checked {
                            t.accent
                        } else {
                            t.muted
                        },
                    );
                    let name = {
                        row.with_min_x(row.left() + 19.0)
                            .with_max_x(if row.width() >= 150.0 {
                                bounds.left() + bounds.width() * 0.51 - 3.0
                            } else {
                                row.right()
                            })
                    };
                    let display_name = crate::agent_upload::literal_text(entry.name.as_bytes());
                    theme::label(
                        ui,
                        name,
                        &display_name,
                        FontId::monospace(11.0),
                        color,
                        Align2::LEFT_CENTER,
                    );
                    if row.width() >= 150.0
                        && let Some(modified) = entry.modified
                    {
                        theme::label(
                            ui,
                            row.with_min_x(bounds.left() + bounds.width() * 0.51 + 4.0),
                            &modified.format("%Y/%m/%d %H:%M").to_string(),
                            t.chrome_font(10.0),
                            t.muted,
                            Align2::LEFT_CENTER,
                        );
                    }
                    if response.clicked() {
                        if !*select_mode {
                            selected.clear();
                        }
                        if !selected.remove(&entry.name) {
                            selected.insert(entry.name.clone());
                        }
                    }
                    if response.double_clicked() && entry.directory && !*select_mode {
                        open = Some(entry.name.clone());
                    }
                    response.context_menu(|ui| {
                        if remote && ui.button(crate::i18n::tr(ui, "Download")).clicked() {
                            download = Some(entry.clone());
                            ui.close();
                        }
                        for (title, label) in [
                            ("Rename", FileAction::Rename),
                            ("Move", FileAction::Move),
                            ("Info", FileAction::Info),
                            ("Delete", FileAction::Delete),
                        ] {
                            if ui.button(crate::i18n::tr(ui, title)).clicked() {
                                action = Some((label, entry.clone()));
                                ui.close();
                            }
                        }
                        if ui.button(crate::i18n::tr(ui, "Select Mode")).clicked() {
                            *select_mode = !*select_mode;
                            ui.close();
                        }
                    });
                }
            },
        );
        if go_parent {
            self.navigate_parent();
            return;
        }
        if let Some(name) = open {
            let path = if remote {
                join_remote(&self.explorer.remote.path, &name)
            } else {
                self.explorer
                    .local
                    .path
                    .join(&name)
                    .to_string_lossy()
                    .into_owned()
            };
            self.navigate(&path);
        }
        if let Some((action, entry)) = action
            && let Some(target) = self.target(entry)
        {
            self.file_dialog(action, vec![target]);
        }
        if let Some(entry) = download
            && let Some(target) = self.target(entry)
        {
            self.dialog = Some(Dialog::Download(vec![target]));
        }
    }

    fn bulk_bar(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let t = self.theme;
        let remote = self.explorer.remote_active;
        let (entries, selected) = if remote {
            (
                &self.explorer.remote.entries,
                &self.explorer.remote.selected,
            )
        } else {
            (&self.explorer.local.entries, &self.explorer.local.selected)
        };
        let targets = entries
            .iter()
            .filter(|item| {
                !remote || self.settings.on("sftp_show_hidden") || !item.name.starts_with('.')
            })
            .filter(|item| selected.contains(&item.name))
            .filter_map(|item| self.target(item.clone()))
            .collect::<Vec<_>>();
        theme::band(ui, rect, t.panel, Some(t.border));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("bulk-actions")
                .max_rect(rect.shrink2(vec2(4.0, 2.0))),
        );
        child.set_clip_rect(rect);
        let count = crate::i18n::format(
            &child,
            "Files selected: {count}",
            &[("count", &targets.len().to_string())],
        );
        child
            .add(egui::Label::new(egui::RichText::new(&count).small()).truncate())
            .on_hover_text(count);
        let action = child
            .horizontal_wrapped(|ui| {
                let mut action = None;
                ui.add_enabled_ui(!targets.is_empty(), |ui| {
                    if remote && ui.small_button(crate::i18n::tr(ui, "Download")).clicked() {
                        action = Some(SelectionAction::Download);
                    }
                    if ui.small_button(crate::i18n::tr(ui, "Move")).clicked() {
                        action = Some(SelectionAction::Move);
                    }
                    if ui.small_button(crate::i18n::tr(ui, "Delete")).clicked() {
                        action = Some(SelectionAction::Delete);
                    }
                });
                action
            })
            .inner;
        match action {
            Some(SelectionAction::Download) => self.dialog = Some(Dialog::Download(targets)),
            Some(SelectionAction::Move) => self.file_dialog(FileAction::Move, targets),
            Some(SelectionAction::Delete) => self.file_dialog(FileAction::Delete, targets),
            None => {}
        }
    }

    fn target(&self, entry: Entry) -> Option<Target> {
        if self.explorer.remote_active {
            let id = self.explorer.remote_id?;
            let path = join_remote(&self.explorer.remote.path, &entry.name);
            Some(Target::Remote { id, path, entry })
        } else {
            Some(Target::Local {
                path: self.explorer.local.path.join(&entry.name),
                entry,
            })
        }
    }

    fn file_dialog(&mut self, action: FileAction, entries: Vec<Target>) {
        let input = match action {
            FileAction::Rename => entries[0].entry().name.clone(),
            FileAction::Move if self.explorer.remote_active => self.explorer.remote.path.clone(),
            FileAction::Move => short_local(&self.explorer.local.path),
            _ => String::new(),
        };
        self.dialog = Some(Dialog::Files {
            action,
            entries,
            input,
        });
    }
}

fn list_local(path: &Path) -> std::io::Result<Vec<Entry>> {
    const MAX_DIRECTORY_ENTRIES: usize = 100_000;
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(path)?.flatten() {
        if entries.len() == MAX_DIRECTORY_ENTRIES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Directory exceeds {MAX_DIRECTORY_ENTRIES} entries"),
            ));
        }
        let metadata = entry.metadata().ok();
        #[cfg(unix)]
        let permissions = {
            use std::os::unix::fs::PermissionsExt;
            metadata
                .as_ref()
                .map(|value| value.permissions().mode() & 0o777)
        };
        #[cfg(not(unix))]
        let permissions = None;
        entries.push(Entry {
            name: entry.file_name().to_string_lossy().into_owned(),
            directory: entry.file_type().is_ok_and(|kind| kind.is_dir()),
            size: metadata.as_ref().map_or(0, |value| value.len()),
            permissions,
            modified: metadata
                .and_then(|value| value.modified().ok())
                .map(chrono::DateTime::<chrono::Local>::from),
        });
    }
    Ok(entries)
}

fn short_local(path: &Path) -> String {
    if let Ok(suffix) = path.strip_prefix(storage::home_dir()) {
        if suffix.as_os_str().is_empty() {
            "~".into()
        } else {
            format!("~/{}", suffix.display())
        }
    } else {
        path.display().to_string()
    }
}

// Keep the address field in its own fixed rectangle. A long path must never
// grow the enclosing dock, and hover/focus must not swap widget layouts.
fn path_field(
    ui: &mut egui::Ui,
    rect: Rect,
    id: egui::Id,
    text: &mut String,
    t: theme::Theme,
) -> egui::text_edit::TextEditOutput {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("path-field")
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(ui.clip_rect().intersect(rect));
    child.visuals_mut().text_cursor.preview = false;
    egui::TextEdit::singleline(text)
        .id(id)
        .font(t.chrome_font(11.0))
        .text_color(t.text)
        .frame(false)
        .margin(egui::Margin::ZERO)
        .desired_width(rect.width())
        .min_size(rect.size())
        .vertical_align(egui::Align::Center)
        .show(&mut child)
}

fn remote_parent(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit_once('/')
        .map_or(
            "/",
            |(parent, _)| if parent.is_empty() { "/" } else { parent },
        )
}

// Only show the synthetic row for an already resolved, non-root directory.
fn navigation_parent(path: &str) -> Option<&str> {
    (path.starts_with('/') && !path.trim_end_matches('/').is_empty()).then(|| remote_parent(path))
}

#[test]
fn path_field_keeps_geometry_on_hover_and_focus() {
    let context = egui::Context::default();
    let theme = theme::Theme::dark();
    theme.apply(&context);
    theme::install_fonts(&context);
    let rect = Rect::from_min_size(pos2(20.0, 40.0), vec2(180.0, 24.0));
    let id = egui::Id::new("test-path");
    let mut path = "/home/user".to_owned();
    let render = |path: &mut String, events: Vec<egui::Event>| {
        let mut geometry = None;
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let before = ui.min_rect();
                    let output = path_field(ui, rect, id, path, theme);
                    assert_eq!(
                        ui.min_rect(),
                        before,
                        "path field must not expand its parent"
                    );
                    geometry = Some((
                        output.response.rect,
                        output.galley_pos,
                        output.text_clip_rect,
                    ));
                });
            },
        );
        geometry.unwrap()
    };
    let baseline = render(&mut path, vec![]);
    assert_eq!(baseline.0, rect);
    assert_eq!(baseline.2, rect);
    let hover = rect.center();
    assert_eq!(
        render(&mut path, vec![egui::Event::PointerMoved(hover)]),
        baseline
    );
    assert_eq!(
        render(&mut path, vec![egui::Event::PointerMoved(rect.right_top())]),
        baseline
    );
    context.memory_mut(|memory| memory.request_focus(id));
    assert_eq!(render(&mut path, vec![]), baseline);
    context.memory_mut(|memory| memory.surrender_focus(id));
    path = "/very/long/remote/directory/that/must/not/expand/the/location/bar".into();
    let long = render(&mut path, vec![]);
    assert_eq!(long.0, rect);
    assert_eq!(long.2, rect);
    assert_eq!(
        render(&mut path, vec![egui::Event::PointerMoved(hover)]),
        long
    );
}

#[test]
fn hidden_entries_can_be_filtered_without_reloading() {
    let entry = |name: &str, directory, size| Entry {
        name: name.into(),
        directory,
        size,
        permissions: None,
        modified: None,
    };
    let mut listing = Listing::new(".".to_owned());
    listing.replace(
        "/home/user".into(),
        vec![
            entry(".", true, 0),
            entry("..", true, 0),
            entry(".config", true, 0),
            entry(".bashrc", false, 20),
            entry("Documents", true, 0),
            entry("report.txt", false, 10),
        ],
    );
    assert_eq!(listing.entries.len(), 4);
    assert_eq!(listing.total_bytes, 30);
    assert_eq!(listing.non_hidden_bytes, 10);
    assert_eq!(
        listing
            .non_hidden
            .iter()
            .map(|&i| listing.entries[i].name.as_str())
            .collect::<Vec<_>>(),
        vec!["Documents", "report.txt"]
    );
    assert_eq!(navigation_parent(&listing.path), Some("/home"));
    listing.replace("/".into(), vec![entry(".hidden", false, 5)]);
    assert!(listing.non_hidden.is_empty());
    assert_eq!(listing.non_hidden_bytes, 0);
    assert_eq!(listing.total_bytes, 5);
}

#[test]
fn parent_navigation_is_separate_from_real_files() {
    let entry = |name: &str, directory| Entry {
        name: name.into(),
        directory,
        size: 10,
        permissions: None,
        modified: None,
    };
    let mut listing = Listing::new(".".to_owned());
    listing.replace(
        "/home/user/".into(),
        vec![
            entry(".", true),
            entry("..", true),
            entry("file", false),
            entry("Zfolder", true),
        ],
    );
    assert_eq!(navigation_parent(&listing.path), Some("/home"));
    assert_eq!(navigation_parent("/home"), Some("/"));
    assert_eq!(navigation_parent("/"), None);
    assert_eq!(navigation_parent("."), None);
    assert_eq!(
        listing
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Zfolder", "file"]
    );
    assert_eq!(listing.total_bytes, 10);
    assert!(listing.selected.is_empty());
}

pub(crate) fn format_size(size: u64) -> String {
    let mut scaled = size as f64;
    let mut unit = "B";
    for suffix in ["K", "M", "G", "T"] {
        if scaled < 1024.0 {
            break;
        }
        scaled /= 1024.0;
        unit = suffix;
    }
    if unit == "B" {
        format!("{size}B")
    } else {
        format!("{scaled:.1}{unit}")
    }
}
