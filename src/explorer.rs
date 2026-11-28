use chrono::Datelike;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, Instant},
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
    pub(crate) expanded: bool,
    hover_folder: Option<(Destination, Instant)>,
    split_fraction: f32,
    pub(crate) folder_draft: Option<FolderDraft>,
    folder_serial: u64,
    path_editor_peer: Option<u32>,
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
    path_input: String,
    date_filter: Option<(i32, i32)>,
    date_bounds: Option<(i32, i32)>,
    date_sort: Option<bool>,
    filter_open: bool,
    visible: Vec<usize>,
}

impl<T: PartialEq> Listing<T> {
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
            path_input: String::new(),
            date_filter: None,
            date_bounds: None,
            date_sort: None,
            filter_open: false,
            visible: Vec::new(),
        }
    }

    fn replace(&mut self, path: T, mut entries: Vec<Entry>) {
        entries.retain(|entry| entry.name != "." && entry.name != "..");
        let same_directory = self.path == path;
        self.path = path;
        self.entries = entries;
        self.selected.clear();
        self.error = None;
        if !same_directory {
            self.date_filter = None;
        }
        self.date_bounds =
            self.entries
                .iter()
                .filter_map(modified_day)
                .fold(None, |bounds, day| {
                    Some(bounds.map_or((day, day), |(lo, hi): (i32, i32)| {
                        (lo.min(day), hi.max(day))
                    }))
                });
        if let Some((lo, hi)) = self.date_bounds {
            self.date_filter = self
                .date_filter
                .map(|(start, end)| (start.clamp(lo, hi), end.clamp(lo, hi)));
        } else {
            self.date_filter = None;
        }
        self.sort_entries();
        self.reindex();
    }

    fn sort_entries(&mut self) {
        // Directories stay first for navigation, even with date filtering.
        self.entries.sort_by_cached_key(|entry| {
            let date = match self.date_sort {
                Some(true) => entry.modified.map(|date| -date.timestamp()),
                Some(false) => entry.modified.map(|date| date.timestamp()),
                None => Some(0),
            };
            (
                !entry.directory,
                date.is_none(),
                date.unwrap_or(0),
                entry.name.to_lowercase(),
            )
        });
    }

    fn reindex(&mut self) {
        self.total_bytes = 0;
        self.non_hidden_bytes = 0;
        self.visible.clear();
        self.non_hidden.clear();
        let mut visible_names = HashSet::new();
        for (index, entry) in self.entries.iter().enumerate() {
            if !date_matches(entry, self.date_filter) {
                continue;
            }
            self.visible.push(index);
            if self.selected.contains(&entry.name) {
                visible_names.insert(entry.name.clone());
            }
            if !entry.directory {
                self.total_bytes = self.total_bytes.saturating_add(entry.size);
            }
            if !entry.name.starts_with('.') {
                self.non_hidden.push(index);
                if !entry.directory {
                    self.non_hidden_bytes = self.non_hidden_bytes.saturating_add(entry.size);
                }
            }
        }
        self.selected = visible_names;
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
            expanded: false,
            hover_folder: None,
            split_fraction: 0.5,
            folder_draft: None,
            folder_serial: 0,
            path_editor_peer: None,
        };
        explorer.load_local(storage::home_dir());
        explorer
    }

    pub(crate) fn accept_initial_listing(&mut self, id: u32, path: String, entries: Vec<Entry>) {
        self.cancel_folder_on_side(true);
        self.remote_id = Some(id);
        self.request += 1;
        self.loading = false;
        self.hover_folder = None;
        self.remote = Listing::new(path.clone());
        self.remote.replace(path, entries);
    }

    pub(crate) fn local_folder(&self) -> &Path {
        &self.local.path
    }

    pub(crate) fn refresh_local(&mut self) {
        self.load_local(self.local.path.clone());
    }

    fn cancel_folder_on_side(&mut self, remote: bool) {
        if self
            .folder_draft
            .as_ref()
            .is_some_and(|draft| draft.remote() == remote)
        {
            self.folder_draft = None;
        }
    }

    pub(crate) fn prune_editor_state(&mut self, context: &egui::Context) {
        if self.folder_draft.is_none() {
            reset_editor(context, folder_editor_id());
        }
        if self.path_editor_peer != self.remote_id {
            reset_editor(context, path_editor_id(true));
            self.path_editor_peer = self.remote_id;
        }
    }

    pub(crate) fn folder_editing(&self, context: &egui::Context) -> bool {
        self.folder_draft.as_ref().is_some_and(|draft| {
            !draft.pending && context.memory(|memory| memory.has_focus(draft.editor_id()))
        })
    }

    fn load_local(&mut self, path: PathBuf) {
        let result = std::fs::canonicalize(&path)
            .and_then(|path| list_local(&path).map(|entries| (path, entries)));
        match result {
            Ok((path, entries)) => {
                if path != self.local.path {
                    self.cancel_folder_on_side(false);
                }
                self.local.replace(path, entries);
            }
            Err(error) => {
                self.cancel_folder_on_side(false);
                self.local.error = Some(error.to_string());
            }
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

#[derive(Clone)]
struct DragFiles(Vec<Target>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Destination {
    Local(PathBuf),
    Remote(u32, String),
}

// This is only a visual draft, never a real listing entry or transfer target.
// Commit exactly one MKDIR/create_dir when the user presses Enter.
pub(crate) struct FolderDraft {
    pub(crate) destination: Destination,
    pub(crate) name: String,
    serial: u64,
    focus: bool,
    reset_editor: bool,
    pub(crate) pending: bool,
    pub(crate) error: Option<String>,
}

impl FolderDraft {
    fn remote(&self) -> bool {
        matches!(self.destination, Destination::Remote(..))
    }

    fn editor_id(&self) -> egui::Id {
        // Serial identifies the backend operation, not the widget. Unique
        // widget IDs otherwise leave persistent TextEditState and undo buffers.
        folder_editor_id()
    }
}

fn folder_editor_id() -> egui::Id {
    egui::Id::new("inline-folder-name")
}

fn path_editor_id(remote: bool) -> egui::Id {
    egui::Id::new(("explorer-path", remote))
}

fn reset_editor(context: &egui::Context, id: egui::Id) {
    context.memory_mut(|memory| memory.surrender_focus(id));
    context.data_mut(|data| data.remove::<egui::text_edit::TextEditState>(id));
}

enum FolderEdit {
    None,
    Confirm,
    Cancel,
}

fn can_drop(files: &DragFiles, destination: &Destination) -> bool {
    !files.0.is_empty()
        && files.0.iter().all(|target| match (target, destination) {
            (Target::Local { path, .. }, Destination::Local(folder)) => !folder.starts_with(path),
            (Target::Remote { id, path, .. }, Destination::Remote(peer, folder)) => {
                id == peer && path != folder && !folder.starts_with(&format!("{path}/"))
            }
            _ => true,
        })
}

fn modified_day(entry: &Entry) -> Option<i32> {
    entry
        .modified
        .map(|date| date.date_naive().num_days_from_ce())
}

fn date_matches(entry: &Entry, filter: Option<(i32, i32)>) -> bool {
    entry.directory
        || filter
            .is_none_or(|(lo, hi)| modified_day(entry).is_some_and(|day| (lo..=hi).contains(&day)))
}

fn date_label(day: i32) -> String {
    chrono::NaiveDate::from_num_days_from_ce_opt(day)
        .map_or_else(String::new, |date| date.format("%Y-%m-%d").to_string())
}

fn date_controls<T: PartialEq>(ui: &mut egui::Ui, listing: &mut Listing<T>) {
    let Some((lo, hi)) = listing.date_bounds else {
        ui.label(crate::i18n::tr(ui, "No modification dates"));
        return;
    };
    let mut enabled = listing.date_filter.is_some();
    let mut range = listing.date_filter.unwrap_or((lo, hi));
    ui.checkbox(&mut enabled, crate::i18n::tr(ui, "Date Modified"));
    if enabled {
        // Both handles share the same chronological scale. The selected
        // interval is highlighted, and dragging either endpoint is inclusive.
        let t = crate::theme::Theme::dark();
        let (track, response) = ui.allocate_exact_size(
            vec2(ui.available_width().max(20.0), 28.0),
            Sense::click_and_drag(),
        );
        let track = track.shrink2(vec2(9.0, 10.0));
        let span = (hi - lo).max(1) as f32;
        let x = |day: i32| track.left() + track.width() * (day - lo) as f32 / span;
        let day_at = |pos: f32| {
            (lo + (((pos - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0) * span).round()
                as i32)
                .clamp(lo, hi)
        };
        let handle_id = response.id.with("range-handle");
        if response.drag_started() || response.clicked() {
            let pos = ui
                .input(|input| input.pointer.press_origin())
                .or(response.interact_pointer_pos());
            if let Some(pos) = pos {
                let low = (pos.x - x(range.0)).abs();
                let high = (pos.x - x(range.1)).abs();
                let right = if low == high {
                    pos.x >= x(range.1)
                } else {
                    high < low
                };
                ui.data_mut(|data| data.insert_temp(handle_id, right));
            }
        }
        if (response.dragged() || response.clicked() || response.drag_stopped())
            && let Some(pos) = response.interact_pointer_pos()
        {
            let right = ui
                .data(|data| data.get_temp::<bool>(handle_id))
                .unwrap_or(false);
            if right {
                range.1 = day_at(pos.x).max(range.0);
            } else {
                range.0 = day_at(pos.x).min(range.1);
            }
        }
        ui.painter().rect_filled(track, 2.0, t.hover);
        ui.painter().rect_filled(
            track.with_min_x(x(range.0)).with_max_x(x(range.1)),
            2.0,
            t.accent,
        );
        for day in [range.0, range.1] {
            ui.painter()
                .circle_filled(pos2(x(day), track.center().y), 6.0, t.text);
        }
        ui.horizontal(|ui| {
            ui.label(format!(
                "{} {}",
                crate::i18n::tr(ui, "From"),
                date_label(range.0)
            ));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(format!(
                    "{} {}",
                    crate::i18n::tr(ui, "To"),
                    date_label(range.1)
                ));
            });
        });
        if ui.small_button(crate::i18n::tr(ui, "Reset")).clicked() {
            range = (lo, hi);
        }
    }
    let filter = enabled.then_some(range);
    if filter != listing.date_filter {
        listing.date_filter = filter;
        listing.reindex();
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
            self.explorer.cancel_folder_on_side(true);
            self.explorer.remote = Listing::new(".".into());
            self.explorer.remote_id = id;
            self.explorer.request += 1;
            self.explorer.loading = false;
            self.explorer.hover_folder = None;
        }
        if id.is_some() {
            self.reload_explorer();
        }
    }

    pub(crate) fn refresh_remote_if(&mut self, id: u32) {
        if self.explorer.remote_id == Some(id) && !self.explorer.loading {
            self.load_remote(id, self.explorer.remote.path.clone());
        }
    }

    fn load_remote(&mut self, id: u32, path: String) {
        if self.explorer.remote_id != Some(id) || self.explorer.remote.path != path {
            self.explorer.cancel_folder_on_side(true);
        }
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
                        if app.explorer.remote.path != path {
                            app.explorer.cancel_folder_on_side(true);
                        }
                        app.explorer.remote.replace(path, entries)
                    }
                    Err(error) => {
                        app.explorer.cancel_folder_on_side(true);
                        app.explorer.remote.error = Some(error);
                    }
                    _ => {
                        app.explorer.cancel_folder_on_side(true);
                        app.explorer.remote.error = Some("Invalid SFTP listing".into());
                    }
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

    fn can_create_local_folder(&self) -> bool {
        self.file_interaction_allowed()
            && self.explorer.local.error.is_none()
            && self.explorer.local.path.is_absolute()
    }

    fn begin_folder_draft(&mut self, destination: Destination) {
        if let Some(draft) = &mut self.explorer.folder_draft {
            if draft.pending {
                return;
            }
            if draft.destination == destination {
                draft.focus = true;
                return;
            }
        }
        self.explorer.folder_serial = self.explorer.folder_serial.wrapping_add(1);
        self.explorer.folder_draft = Some(FolderDraft {
            destination,
            name: String::new(),
            serial: self.explorer.folder_serial,
            focus: true,
            reset_editor: true,
            pending: false,
            error: None,
        });
        self.focus_file_editor();
    }

    fn new_local_folder(&mut self) {
        if self.can_create_local_folder() {
            self.begin_folder_draft(Destination::Local(self.explorer.local.path.clone()));
        }
    }

    pub(crate) fn confirm_folder_draft(&mut self) {
        let Some(draft) = self
            .explorer
            .folder_draft
            .as_ref()
            .filter(|draft| !draft.pending)
        else {
            return;
        };
        let destination = draft.destination.clone();
        let name = draft.name.clone();
        let result = match destination {
            Destination::Local(directory) => {
                let result = self.create_local_folder(&directory, &name);
                if result.is_ok() {
                    self.explorer.folder_draft = None;
                }
                result
            }
            Destination::Remote(id, directory) => {
                self.explorer.folder_draft.as_mut().unwrap().pending = true;
                self.create_sftp_folder(id, &directory, &name)
            }
        };
        if let Err(error) = result
            && let Some(draft) = &mut self.explorer.folder_draft
        {
            draft.pending = false;
            draft.focus = true;
            draft.error = Some(error);
        }
    }

    pub(crate) fn create_local_folder(
        &mut self,
        directory: &Path,
        name: &str,
    ) -> Result<(), String> {
        if !self.can_create_local_folder() || self.explorer.local.path != directory {
            return Err("Local destination changed; reopen New Folder".into());
        }
        if name.trim().is_empty() {
            return Err("Enter a folder name".into());
        }
        let name = safe_name(name)?;
        // The listing holds a canonical path. Do not follow a replacement
        // symlink to a different directory after the inline editor has been opened.
        let current = std::fs::canonicalize(directory)
            .map_err(|error| format!("{}: {error}", directory.display()))?;
        if current != directory {
            return Err("Local destination changed; reopen New Folder".into());
        }
        let path = directory.join(name);
        // create_dir is exclusive: existing files, directories and symlinks
        // cannot be replaced. No recursive path creation or shell execution.
        std::fs::create_dir(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        self.explorer.refresh_local();
        Ok(())
    }

    pub(crate) fn can_create_sftp_folder(&self) -> bool {
        self.file_interaction_allowed()
            && !self.explorer.loading
            && self.explorer.remote.error.is_none()
            && self.explorer.remote.path.starts_with('/')
            && self.explorer.remote_id.is_some()
            && self.explorer.remote_id == self.active_ssh()
    }

    pub(crate) fn new_sftp_folder(&mut self) {
        if !self.can_create_sftp_folder() {
            return;
        }
        if let Some(id) = self.explorer.remote_id {
            self.begin_folder_draft(Destination::Remote(id, self.explorer.remote.path.clone()));
            // The main File Manager button must also reveal the editor in
            // compact mode when the Local tab was selected.
            self.explorer.remote_active = true;
        }
    }

    pub(crate) fn create_sftp_folder(
        &mut self,
        id: u32,
        directory: &str,
        name: &str,
    ) -> Result<(), String> {
        if !self.can_create_sftp_folder()
            || self.active_ssh() != Some(id)
            || self.explorer.remote.path != directory
        {
            return Err("SFTP destination changed; reopen New Folder".into());
        }
        if name.trim().is_empty() {
            return Err("Enter a folder name".into());
        }
        safe_name(name)?;
        let serial = self
            .explorer
            .folder_draft
            .as_ref()
            .filter(|draft| {
                draft.pending
                    && draft.destination == Destination::Remote(id, directory.to_owned())
                    && draft.name == name
            })
            .map(|draft| draft.serial);
        self.request(
            Command::Sftp {
                id,
                action: RemoteAction::CreateFolder {
                    directory: directory.to_owned(),
                    name: name.to_owned(),
                },
            },
            move |app, result| {
                let matching = app
                    .explorer
                    .folder_draft
                    .as_ref()
                    .is_some_and(|draft| Some(draft.serial) == serial);
                match result {
                    Ok(Reply::Done) => {
                        if matching {
                            app.explorer.folder_draft = None;
                        }
                        app.refresh_remote_if(id);
                    }
                    result => {
                        let error = match result {
                            Err(error) => error,
                            _ => "Invalid SFTP response".into(),
                        };
                        if matching {
                            let draft = app.explorer.folder_draft.as_mut().unwrap();
                            draft.pending = false;
                            draft.focus = true;
                            draft.error = Some(error);
                        } else {
                            app.fail(error);
                        }
                    }
                }
            },
        );
        Ok(())
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
                            _ => storage::expand_local_path(destination)
                                .join(safe_name(&entry.name)?),
                        };
                        if path == target {
                            continue;
                        }
                        if target.symlink_metadata().is_ok() {
                            return Err("Destination already exists".into());
                        }
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
                            to: join_remote(destination, safe_name(&entry.name)?),
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
        self.explorer.refresh_local();
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
        let batch = crate::backend::ConflictBatch::default();
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
                batch: batch.clone(),
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
        if !self.file_interaction_allowed()
            || self.explorer.loading
            || self.active_ssh() != Some(id)
        {
            return;
        }
        let resume = self
            .settings
            .on("sftp_resume")
            .then(|| self.ssh_transfer_identity(id))
            .flatten();
        let batch = crate::backend::ConflictBatch::default();
        for path in paths {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if let Err(error) = safe_name(name) {
                self.fail(error);
                continue;
            }
            let remote = join_remote(&self.explorer.remote.path, name);
            self.request(
                Command::Sftp {
                    id,
                    action: RemoteAction::Upload {
                        local: path,
                        remote,
                        resume: resume.clone(),
                        batch: batch.clone(),
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

    fn upload_to(&mut self, id: u32, directory: &str, paths: Vec<PathBuf>) {
        if !self.file_interaction_allowed() || self.explorer.loading {
            return;
        }
        if self.active_ssh() != Some(id) {
            self.fail("SSH session closed");
            return;
        }
        let batch = crate::backend::ConflictBatch::default();
        for path in paths {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                self.fail("Invalid filename");
                continue;
            };
            let name = match safe_name(name) {
                Ok(name) => name,
                Err(error) => {
                    self.fail(error);
                    continue;
                }
            };
            self.request(
                Command::Sftp {
                    id,
                    action: RemoteAction::UploadTree {
                        local: path.clone(),
                        remote: join_remote(directory, name),
                        batch: batch.clone(),
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

    fn drop_files(&mut self, files: &DragFiles, destination: Destination) {
        if !self.file_interaction_allowed() {
            return;
        }
        if files.0.is_empty() {
            return;
        }
        match destination {
            Destination::Remote(id, path) if self.active_ssh() == Some(id) => {
                if files.0.iter().all(|target| matches!(target, Target::Local { .. })) {
                    self.upload_to(id, &path, files.0.iter().filter_map(|target| match target {
                        Target::Local { path, .. } => Some(path.clone()),
                        _ => None,
                    }).collect());
                } else if files.0.iter().all(|target| matches!(target, Target::Remote { id: source, .. } if *source == id)) {
                    let entries: Vec<_> = files.0.iter().filter(|target| {
                        let source = target.path();
                        remote_parent(&source) != path && source != path && !path.starts_with(&format!("{source}/"))
                    }).cloned().collect();
                    if !entries.is_empty() { self.dialog = Some(Dialog::Files { action: FileAction::Move, entries, input: path }); }
                }
            }
            Destination::Local(path) => {
                if files.0.iter().all(|target| matches!(target, Target::Remote { id, .. } if Some(*id) == self.active_ssh())) {
                    self.download_targets(files.0.clone(), path);
                } else if files.0.iter().all(|target| matches!(target, Target::Local { .. })) {
                    let entries: Vec<_> = files.0.iter().filter(|target| match target {
                        Target::Local { path: source, .. } => source.parent() != Some(path.as_path()) && !path.starts_with(source),
                        _ => false,
                    }).cloned().collect();
                    if !entries.is_empty() { self.dialog = Some(Dialog::Files { action: FileAction::Move, entries, input: path.to_string_lossy().into_owned() }); }
                }
            }
            _ => {}
        }
    }

    pub(crate) fn explorer_workspace(&mut self, ui: &mut egui::Ui, bounds: Rect) {
        let t = self.theme;
        let header = top_band(bounds, 30.0);
        theme::band(ui, header, t.bar, Some(t.border));
        header_label(
            ui,
            header.with_max_x((icon_slot(header, 3).left() - 4.0).max(header.left() + 21.0)),
            "File Manager",
            t,
        );
        let caption = if self.explorer.expanded {
            "Restore Filer"
        } else {
            "Expand Filer"
        };
        let icon = if self.explorer.expanded {
            Icon::Collapse
        } else {
            Icon::Expand
        };
        if theme::icon_button(ui, icon_slot(header, 0), caption, icon, t).clicked() {
            self.toggle_file_workspace();
        }
        if theme::icon_button(ui, icon_slot(header, 1), "Refresh", Icon::Refresh, t).clicked() {
            self.explorer.refresh_local();
            if let Some(id) = self.active_ssh() {
                self.refresh_remote_if(id);
            }
        }
        if theme::icon_button(ui, icon_slot(header, 3), "Transfer queue", Icon::Menu, t).clicked() {
            self.transfer_queue_open = !self.transfer_queue_open;
            ui.ctx().request_repaint();
        }
        ui.push_id("main-new-folder", |ui| {
            self.new_folder_button(ui, icon_slot(header, 2), "New SFTP Folder", true)
        });
        let body = bounds.with_min_y(header.bottom());
        if !self.explorer.expanded || body.width() < 400.0 {
            ui.push_id("workspace-single", |ui| self.explorer_panel(ui, body));
            return;
        }
        let middle = body.left() + body.width() * self.explorer.split_fraction;
        let divider = Rect::from_min_max(
            pos2(middle - 2.0, body.top()),
            pos2(middle + 2.0, body.bottom()),
        );
        let response = ui
            .interact(
                divider,
                ui.id().with("file-workspace-divider"),
                Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        theme::band(
            ui,
            divider,
            if response.hovered() || response.dragged() {
                t.accent
            } else {
                t.bg
            },
            None,
        );
        if response.dragged() {
            self.explorer.split_fraction = (self.explorer.split_fraction
                + response.drag_delta().x / body.width())
            .clamp(0.25, 0.75);
        }
        self.explorer.remote_active = false;
        ui.push_id("local-workspace", |ui| {
            self.explorer_side(ui, body.with_max_x(divider.left()), false, true)
        });
        self.explorer.remote_active = true;
        ui.push_id("remote-workspace", |ui| {
            self.explorer_side(ui, body.with_min_x(divider.right()), false, true)
        });
    }

    pub(crate) fn explorer_panel(&mut self, ui: &mut egui::Ui, bounds: Rect) {
        let t = self.theme;
        let rect = bounds;
        theme::band(ui, bounds, t.panel, None);
        let header = top_band(rect, 22.0);
        theme::band(ui, header, t.panel, Some(t.border));
        let tabs = header;
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
            self.explorer.folder_draft = None;
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

        self.explorer_side(
            ui,
            rect.with_min_y(tabs.bottom()),
            !self.file_workspace_active(),
            false,
        );
    }

    fn new_folder_button(&mut self, ui: &mut egui::Ui, slot: Rect, label: &str, remote: bool) {
        let t = self.theme;
        let enabled = (if remote {
            self.can_create_sftp_folder()
        } else {
            self.can_create_local_folder()
        }) && self
            .explorer
            .folder_draft
            .as_ref()
            .is_none_or(|draft| !draft.pending);
        if enabled {
            if theme::icon_button(ui, slot, label, Icon::FolderAdd, t).clicked() {
                if remote {
                    self.new_sftp_folder();
                } else {
                    self.new_local_folder();
                }
            }
        } else {
            theme::icon(
                ui,
                Rect::from_center_size(slot.center(), vec2(14.0, 14.0)),
                Icon::FolderAdd,
                t.dim,
            );
            ui.interact(
                slot,
                ui.id()
                    .with(("new-folder-disabled", slot.center().y.to_bits())),
                Sense::hover(),
            )
            .on_hover_text(crate::i18n::tr(
                ui,
                if remote {
                    "Connect and open an SFTP directory first"
                } else {
                    "Open a local directory first"
                },
            ));
        }
    }

    fn explorer_side(&mut self, ui: &mut egui::Ui, rect: Rect, docked: bool, paired: bool) {
        let t = self.theme;
        theme::band(ui, rect, t.panel, None);
        let header = top_band(rect, 26.0);
        theme::band(ui, header, t.panel, Some(t.border));
        let remote = self.explorer.remote_active;
        if !remote || !paired {
            let last_slot = match (remote, docked) {
                (true, true) => 5,
                (true, false) | (false, true) => 4,
                (false, false) => 2,
            };
            let title = header
                .with_max_x((icon_slot(header, last_slot).left() - 4.0).max(header.left() + 30.0));
            header_label(ui, title, if remote { "SFTP" } else { "Local files" }, t);
        }
        if remote && !self.settings.on("sftp_show_hidden") {
            // Hidden selections must not survive into bulk file operations.
            self.explorer
                .remote
                .selected
                .retain(|name| !name.starts_with('.'));
        }
        let mut start = header.bottom();
        if remote {
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
            let status = crate::i18n::tr(ui, status);
            if paired {
                // Keep both paths and file tables at the same height. Reserve
                // room for the five remote toolbar buttons even while loading.
                let title =
                    header.with_max_x((icon_slot(header, 4).left() - 4.0).max(header.left()));
                let painter = ui.painter().with_clip_rect(title);
                painter.circle_filled(pos2(title.left() + 10.0, title.center().y), 3.0, mark);
                if title.width() > 21.0 {
                    theme::label(
                        ui,
                        title.with_min_x(title.left() + 21.0),
                        &format!("SFTP · {status}"),
                        t.chrome_font(11.0),
                        t.text,
                        Align2::LEFT_CENTER,
                    );
                }
                ui.interact(title, ui.id().with("sftp-status"), Sense::hover())
                    .on_hover_text(status);
            } else {
                let band = top_band(rect.with_min_y(start), 36.0);
                theme::band(ui, band, t.panel, Some(t.border));
                ui.painter()
                    .circle_filled(pos2(band.left() + 14.0, band.center().y), 3.0, mark);
                theme::label(
                    ui,
                    band.with_min_x(band.left() + 24.0),
                    status,
                    t.chrome_font(11.0),
                    t.muted,
                    Align2::LEFT_CENTER,
                );
                start = band.bottom();
            }
        }
        if docked
            && theme::icon_button(ui, icon_slot(header, 0), "Hide Filer", Icon::Close, t).clicked()
        {
            self.sidebar = None;
            self.explorer.folder_draft = None;
        }
        if docked
            && theme::icon_button(ui, icon_slot(header, 2), "Expand Filer", Icon::Expand, t)
                .clicked()
        {
            self.toggle_file_workspace();
        }
        if theme::icon_button(
            ui,
            icon_slot(header, if docked { 1 } else { 0 }),
            "Refresh",
            Icon::Refresh,
            t,
        )
        .clicked()
        {
            self.reload_explorer();
        }
        let mode = if remote {
            &mut self.explorer.remote.select_mode
        } else {
            &mut self.explorer.local.select_mode
        };
        if theme::icon_button(
            ui,
            icon_slot(header, if docked { 4 } else { 2 }),
            "Select Mode",
            Icon::Check,
            t,
        )
        .clicked()
        {
            *mode = !*mode;
        }
        let selecting = *mode;
        if remote
            && self.file_interaction_allowed()
            && !self.explorer.loading
            && self.explorer.remote.path.starts_with('/')
            && let Some(id) = self.explorer.remote_id
            && theme::icon_button(
                ui,
                icon_slot(header, if docked { 5 } else { 3 }),
                "Upload File",
                Icon::Upload,
                t,
            )
            .clicked()
            && let Some(files) = rfd::FileDialog::new()
                .set_directory(&self.explorer.local.path)
                .pick_files()
        {
            self.upload_files(id, files);
        }
        if remote
            && self.file_interaction_allowed()
            && !docked
            && self.explorer.remote.path.starts_with('/')
            && let Some(id) = self.explorer.remote_id
            && !self.explorer.loading
            && theme::icon_button(ui, icon_slot(header, 4), "Upload Folder", Icon::Folder, t)
                .clicked()
            && let Some(folder) = rfd::FileDialog::new()
                .set_directory(&self.explorer.local.path)
                .pick_folder()
        {
            let path = self.explorer.remote.path.clone();
            self.upload_to(id, &path, vec![folder]);
        }
        self.new_folder_button(
            ui,
            icon_slot(header, if docked { 3 } else { 1 }),
            if remote {
                "New Folder"
            } else {
                "New Local Folder"
            },
            remote,
        );
        if remote && self.explorer.remote_id.is_none() {
            let message = top_band(rect.with_min_y(start), 28.0).shrink2(vec2(8.0, 0.0));
            theme::label(
                ui,
                message,
                crate::i18n::tr(
                    ui,
                    if self.active_is_sftp() {
                        self.file_connection_label()
                    } else {
                        "No active SSH session"
                    },
                ),
                t.chrome_font(11.0),
                t.muted,
                Align2::LEFT_CENTER,
            );
            return;
        }
        let path_row = top_band(rect.with_min_y(start), 24.0);
        self.explorer_path(ui, path_row);
        let mut list = rect.with_min_y(path_row.bottom());
        let filter_open = if remote {
            self.explorer.remote.filter_open
        } else {
            self.explorer.local.filter_open
        };
        if filter_open {
            let filter = top_band(list, 104.0_f32.min((list.height() - 65.0).max(0.0)));
            theme::band(ui, filter, t.panel, Some(t.border));
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(("date-filter", remote))
                    .max_rect(filter.shrink(6.0)),
            );
            child.set_clip_rect(filter);
            child.spacing_mut().slider_width = (filter.width() - 70.0).max(50.0);
            if remote {
                date_controls(&mut child, &mut self.explorer.remote);
            } else {
                date_controls(&mut child, &mut self.explorer.local);
            }
            list = list.with_min_y(filter.bottom().min(list.bottom()));
        }
        if selecting
            && self.file_interaction_allowed()
            && !(remote && self.explorer.loading)
            && list.height() > 90.0
        {
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
        // One editor per side, independent of session and layout. Reset the
        // remote state (including undo) when switching hosts.
        self.explorer.prune_editor_state(ui.ctx());
        let id = path_editor_id(remote);
        let input = if remote {
            &mut self.explorer.remote.path_input
        } else {
            &mut self.explorer.local.path_input
        };
        if !ui.memory(|memory| memory.has_focus(id)) {
            input.clone_from(&path);
        }
        let output = path_field(ui, body, id, input, t);
        if output.response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            let path = input.clone();
            self.navigate(&path);
        } else if output.response.has_focus()
            && ui.input(|input| input.key_pressed(egui::Key::Escape))
        {
            output.response.surrender_focus();
            *input = path;
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
        let local_path = self.explorer.local.path.clone();
        let remote_path = self.explorer.remote.path.clone();
        let remote_id = self.explorer.remote_id;
        let loading = remote && self.explorer.loading;
        let enabled = self.file_interaction_allowed()
            && !loading
            && ui.memory(|memory| memory.allows_interaction(ui.layer_id()));
        let make_target = |entry: &Entry| -> Option<Target> {
            safe_name(&entry.name).ok()?;
            if remote {
                Some(Target::Remote {
                    id: remote_id?,
                    path: join_remote(&remote_path, &entry.name),
                    entry: entry.clone(),
                })
            } else {
                Some(Target::Local {
                    path: local_path.join(&entry.name),
                    entry: entry.clone(),
                })
            }
        };
        let make_destination = |folder: Option<&str>| -> Option<Destination> {
            if let Some(name) = folder {
                safe_name(name).ok()?;
            }
            if remote {
                if loading || !remote_path.starts_with('/') {
                    return None;
                }
                Some(Destination::Remote(
                    remote_id?,
                    folder.map_or_else(
                        || remote_path.clone(),
                        |name| join_remote(&remote_path, name),
                    ),
                ))
            } else {
                Some(Destination::Local(folder.map_or_else(
                    || local_path.clone(),
                    |name| local_path.join(name),
                )))
            }
        };
        let draft_here = self.explorer.folder_draft.as_ref().is_some_and(|draft| {
            if remote {
                remote_id.is_some_and(|id| {
                    draft.destination == Destination::Remote(id, remote_path.clone())
                })
            } else {
                draft.destination == Destination::Local(local_path.clone())
            }
        });
        let date_sort = if remote {
            self.explorer.remote.date_sort
        } else {
            self.explorer.local.date_sort
        };
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
                if show_hidden {
                    listing.visible.as_slice()
                } else {
                    listing.non_hidden.as_slice()
                },
            )
        } else {
            let listing = &mut self.explorer.local;
            (
                &listing.entries,
                &mut listing.selected,
                &mut listing.select_mode,
                &listing.error,
                listing.total_bytes,
                listing.visible.as_slice(),
            )
        };
        let count = indices.len();
        let mut sort = None;
        let mut filter_toggle = false;
        {
            let header = top_band(bounds, 19.0);
            theme::band(ui, header, t.panel, Some(t.border));
            let wide = header.width() >= 420.0;
            let split = header.left() + header.width() * 0.51;
            let date_start = if wide {
                header.left() + header.width() * 0.68
            } else {
                split
            };
            let date_rect = header
                .with_min_x(date_start.min((header.right() - 24.0).max(header.left())))
                .with_max_x((header.right() - 22.0).max(header.left()));
            let name_response = ui.interact(
                header.with_max_x(split),
                ui.id().with(("sort-name", remote)),
                Sense::click(),
            );
            if name_response.clicked() {
                sort = Some(None);
            }
            let date_response = ui.interact(
                date_rect,
                ui.id().with(("sort-date", remote)),
                Sense::click(),
            );
            if date_response.clicked() {
                sort = Some(Some(true));
            }
            if theme::icon_button(
                ui,
                icon_slot(header, 0),
                "Filter by modified date",
                Icon::Filter,
                t,
            )
            .clicked()
            {
                filter_toggle = true;
            }
            if wide {
                theme::label(
                    ui,
                    header.with_min_x(split + 4.0).with_max_x(date_start),
                    crate::i18n::tr(ui, "Size"),
                    t.chrome_font(10.0),
                    t.text,
                    Align2::LEFT_CENTER,
                );
            }
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
                    date_rect.with_min_x(date_start + 4.0),
                    &format!(
                        "{}{}",
                        crate::i18n::tr(ui, "Date Modified"),
                        match date_sort {
                            Some(true) => " ▼",
                            Some(false) => " ▲",
                            None => "",
                        }
                    ),
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
        let mut upload = None;
        let mut go_parent = false;
        let mut hover_destination = None;
        let mut drop_destination = None;
        let mut payload = None;
        let mut hover_external_folder = None;
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
            go_parent = enabled && response.clicked();
            let parent = if remote {
                navigation_parent(&remote_path).and_then(|parent| {
                    remote_id.map(|id| Destination::Remote(id, parent.to_owned()))
                })
            } else {
                local_path
                    .parent()
                    .map(|path| Destination::Local(path.to_owned()))
            };
            if enabled
                && let Some(destination) = parent
                && let Some(files) = response.dnd_hover_payload::<DragFiles>()
                && can_drop(&files, &destination)
            {
                ui.painter().rect_stroke(
                    row,
                    0.0,
                    Stroke::new(1.0_f32, t.accent),
                    egui::StrokeKind::Inside,
                );
                hover_destination = Some(destination.clone());
                if let Some(files) = response.dnd_release_payload::<DragFiles>() {
                    drop_destination = Some(destination);
                    payload = Some(files);
                }
            }
            response.on_hover_text(crate::i18n::tr(ui, "Go to parent folder"));
        }
        let mut folder_edit = FolderEdit::None;
        if draft_here && let Some(draft) = &mut self.explorer.folder_draft {
            folder_edit = inline_folder_row(&mut child, t, draft, enabled);
        }
        theme::compact_list((bounds.bottom() - child.cursor().top()).max(0.0)).show_rows(
            &mut child,
            18.0,
            count,
            |ui, rows| {
                for row_index in rows {
                    let entry = &entries[indices[row_index]];
                    let (row, response) = ui.allocate_exact_size(
                        vec2(ui.available_width(), 18.0),
                        if enabled {
                            Sense::click_and_drag()
                        } else {
                            Sense::hover()
                        },
                    );
                    if enabled && response.drag_started() {
                        let targets = if selected.contains(&entry.name) {
                            indices
                                .iter()
                                .map(|&i| &entries[i])
                                .filter(|item| selected.contains(&item.name))
                                .filter_map(make_target)
                                .collect()
                        } else {
                            make_target(entry).into_iter().collect()
                        };
                        response.dnd_set_drag_payload(DragFiles(targets));
                    }
                    if enabled
                        && remote
                        && entry.directory
                        && response.contains_pointer()
                        && let Some(Destination::Remote(_, path)) =
                            make_destination(Some(&entry.name))
                    {
                        hover_external_folder = Some(path);
                    }
                    if enabled
                        && entry.directory
                        && response.contains_pointer()
                        && let Some(destination) = make_destination(Some(&entry.name))
                        && let Some(files) = response.dnd_hover_payload::<DragFiles>()
                        && can_drop(&files, &destination)
                    {
                        ui.painter().rect_stroke(
                            row,
                            0.0,
                            Stroke::new(1.0_f32, t.accent),
                            egui::StrokeKind::Inside,
                        );
                        hover_destination = Some(destination.clone());
                        if let Some(files) = response.dnd_release_payload::<DragFiles>() {
                            drop_destination = Some(destination);
                            payload = Some(files);
                        }
                    }
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
                        let start = if row.width() >= 420.0 { 0.68 } else { 0.51 };
                        theme::label(
                            ui,
                            row.with_min_x(bounds.left() + bounds.width() * start + 4.0),
                            &modified.format("%Y/%m/%d %H:%M").to_string(),
                            t.chrome_font(10.0),
                            t.muted,
                            Align2::LEFT_CENTER,
                        );
                    }
                    if row.width() >= 420.0 && !entry.directory {
                        theme::label(
                            ui,
                            row.with_min_x(bounds.left() + bounds.width() * 0.51 + 4.0)
                                .with_max_x(bounds.left() + bounds.width() * 0.68 - 4.0),
                            &format_size(entry.size),
                            t.chrome_font(10.0),
                            t.muted,
                            Align2::LEFT_CENTER,
                        );
                    }
                    if response.clicked() {
                        if !*select_mode && !ui.input(|input| input.modifiers.command) {
                            selected.clear();
                        }
                        if !selected.remove(&entry.name) {
                            selected.insert(entry.name.clone());
                        }
                    }
                    if response.double_clicked()
                        && entry.directory
                        && !*select_mode
                        && safe_name(&entry.name).is_ok()
                    {
                        open = Some(entry.name.clone());
                    }
                    if enabled {
                        response.context_menu(|ui| {
                            if !remote
                                && remote_id.is_some()
                                && ui.button(crate::i18n::tr(ui, "Upload File")).clicked()
                            {
                                upload = Some(entry.clone());
                                ui.close();
                            }
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
                }
            },
        );
        match folder_edit {
            FolderEdit::Confirm => {
                self.confirm_folder_draft();
                ui.ctx().request_repaint();
                return;
            }
            FolderEdit::Cancel => {
                self.explorer.folder_draft = None;
                ui.ctx().request_repaint();
            }
            FolderEdit::None => {}
        }
        if enabled
            && drop_destination.is_none()
            && ui.rect_contains_pointer(bounds)
            && ui.input(|input| input.pointer.any_released())
            && let Some(destination) = make_destination(None)
            && let Some(files) = egui::DragAndDrop::payload::<DragFiles>(ui.ctx())
            && can_drop(&files, &destination)
        {
            drop_destination = Some(destination);
            payload = egui::DragAndDrop::take_payload(ui.ctx());
        }
        if let (Some(files), Some(destination)) = (payload, drop_destination) {
            self.explorer.hover_folder = None;
            self.drop_files(&files, destination);
            return;
        }
        if enabled
            && remote
            && ui.rect_contains_pointer(bounds)
            && let Some(Destination::Remote(id, directory)) = make_destination(None)
        {
            let paths = ui.input_mut(|input| {
                std::mem::take(&mut input.raw.dropped_files)
                    .into_iter()
                    .filter_map(|file| file.path)
                    .collect::<Vec<_>>()
            });
            if !paths.is_empty() {
                let path = hover_external_folder.unwrap_or(directory);
                self.upload_to(id, &path, paths);
            }
        }
        if let Some(destination) = hover_destination {
            let now = Instant::now();
            if self
                .explorer
                .hover_folder
                .as_ref()
                .is_none_or(|(old, _)| old != &destination)
            {
                self.explorer.hover_folder = Some((destination.clone(), now));
            }
            let elapsed = self.explorer.hover_folder.as_ref().unwrap().1.elapsed();
            if elapsed >= Duration::from_millis(650)
                && ui.input(|input| input.pointer.primary_down())
            {
                self.explorer.hover_folder = None;
                match destination {
                    Destination::Local(path) => self.explorer.load_local(path),
                    Destination::Remote(id, path) => self.load_remote(id, path),
                }
            } else {
                ui.ctx()
                    .request_repaint_after(Duration::from_millis(650).saturating_sub(elapsed));
            }
        } else if self
            .explorer
            .hover_folder
            .as_ref()
            .is_some_and(|(destination, _)| {
                matches!(destination, Destination::Remote(..)) == remote
            })
            || !egui::DragAndDrop::has_payload_of_type::<DragFiles>(ui.ctx())
        {
            self.explorer.hover_folder = None;
        }
        if filter_toggle || sort.is_some() {
            fn apply<T: PartialEq>(
                listing: &mut Listing<T>,
                filter_toggle: bool,
                sort: Option<Option<bool>>,
            ) {
                if filter_toggle {
                    listing.filter_open = !listing.filter_open;
                }
                if let Some(sort) = sort {
                    listing.date_sort = sort.map(|_| !listing.date_sort.unwrap_or(false));
                    listing.sort_entries();
                    listing.reindex();
                }
            }
            if remote {
                apply(&mut self.explorer.remote, filter_toggle, sort);
            } else {
                apply(&mut self.explorer.local, filter_toggle, sort);
            }
        }
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
            self.open_download_dialog(vec![target]);
        }
        if let Some(entry) = upload
            && let Some(id) = self.active_ssh()
            && safe_name(&entry.name).is_ok()
        {
            let path = self.explorer.local.path.join(&entry.name);
            let directory = self.explorer.remote.path.clone();
            if directory.starts_with('/') {
                self.upload_to(id, &directory, vec![path]);
            }
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
            Some(SelectionAction::Download) => self.open_download_dialog(targets),
            Some(SelectionAction::Move) => self.file_dialog(FileAction::Move, targets),
            Some(SelectionAction::Delete) => self.file_dialog(FileAction::Delete, targets),
            None => {}
        }
    }

    fn target(&self, entry: Entry) -> Option<Target> {
        safe_name(&entry.name).ok()?;
        if self.explorer.remote_active {
            if self.explorer.loading || !self.explorer.remote.path.starts_with('/') {
                return None;
            }
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

    fn open_download_dialog(&mut self, entries: Vec<Target>) {
        let folder = if self.explorer.expanded {
            self.explorer.local.path.clone()
        } else {
            storage::default_download_dir()
        };
        self.dialog = Some(Dialog::Download {
            entries,
            folder: folder.to_string_lossy().into_owned(),
        });
    }

    pub(crate) fn remote_folder_suggestions(&self, id: u32, path: &str) -> Vec<String> {
        if self.explorer.remote_id != Some(id)
            || self.explorer.remote.path != path
            || self.explorer.loading
        {
            return Vec::new();
        }
        self.explorer
            .remote
            .entries
            .iter()
            .filter(|entry| entry.directory && safe_name(&entry.name).is_ok())
            .take(100)
            .map(|entry| entry.name.clone())
            .collect()
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

// Pin the draft just below the parent row, outside the virtualized scroll
// area. It remains visible even when the directory has thousands of entries.
fn inline_folder_row(
    ui: &mut egui::Ui,
    t: theme::Theme,
    draft: &mut FolderDraft,
    enabled: bool,
) -> FolderEdit {
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    theme::band(ui, row, t.hover, None);
    theme::icon(
        ui,
        Rect::from_center_size(pos2(row.left() + 10.0, row.center().y), vec2(12.0, 12.0)),
        Icon::Folder,
        t.yellow,
    );
    let field = row
        .with_min_x(row.left() + 19.0)
        .with_max_x((row.right() - 4.0).max(row.left() + 20.0));
    if draft.pending {
        theme::label(
            ui,
            field,
            &format!("{}  ·  {}", draft.name, crate::i18n::tr(ui, "Creating…")),
            t.chrome_font(11.0),
            t.muted,
            Align2::LEFT_CENTER,
        );
        return FolderEdit::None;
    }
    if !enabled {
        // A refresh may temporarily disable editing. Do not submit to a
        // directory whose listing has not finished loading.
        return FolderEdit::None;
    }
    let id = draft.editor_id();
    if std::mem::take(&mut draft.reset_editor) {
        reset_editor(ui.ctx(), id);
    }
    let initial = std::mem::take(&mut draft.focus);
    if initial {
        ui.memory_mut(|memory| memory.request_focus(id));
    }
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("folder-name-editor")
            .max_rect(field)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(ui.clip_rect().intersect(field));
    child.visuals_mut().text_cursor.preview = false;
    let response = egui::TextEdit::singleline(&mut draft.name)
        .id(id)
        .font(t.chrome_font(11.0))
        .text_color(t.text)
        .hint_text(crate::i18n::tr(ui, "New Folder"))
        .frame(false)
        .margin(egui::Margin::ZERO)
        .desired_width(field.width())
        .min_size(field.size())
        .vertical_align(egui::Align::Center)
        .show(&mut child)
        .response;
    if initial {
        response.request_focus();
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
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            response.surrender_focus();
            return FolderEdit::Cancel;
        }
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
            response.surrender_focus();
            return FolderEdit::Confirm;
        }
    }
    if !initial && !response.has_focus() {
        return FolderEdit::Cancel;
    }
    FolderEdit::None
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

#[cfg(test)]
#[path = "../tests/unit/explorer.rs"]
mod tests;
