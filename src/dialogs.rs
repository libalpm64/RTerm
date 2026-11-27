use eframe::egui::{self, Align2, Color32, Rect, RichText, Sense, Stroke, pos2, vec2};

use crate::{
    app::App,
    explorer::{FileAction, Target, format_size},
    storage,
    theme::{self, Icon, Theme},
};

mod connect;
mod keys;
mod settings;
mod tunnel;
mod widgets;

use settings::SettingsSection;
use widgets::{DIALOG_INSET, form_field};

pub(crate) use connect::ConnectForm;
pub(crate) use keys::KeyManager;
pub(crate) use settings::Settings;

pub(crate) enum Dialog {
    Connect(Box<ConnectForm>),
    Settings {
        draft: Settings,
        password: String,
        tunnel: tunnel::Draft,
        session_import_summary: Option<crate::session_import::Summary>,
    },
    About,
    Exit,
    ResetVault,
    ImportVault {
        path: std::path::PathBuf,
        password: zeroize::Zeroizing<String>,
        replace: bool,
    },
    VaultNotice {
        imported: bool,
        backup: Option<std::path::PathBuf>,
    },
    DeleteProfile(usize),
    RenameTab {
        tab: u64,
        name: String,
    },
    Files {
        action: FileAction,
        entries: Vec<Target>,
        input: String,
    },
    Download {
        entries: Vec<Target>,
        folder: String,
    },
    TransferConflict {
        conflict: crate::backend::ConflictPrompt,
        remember: bool,
    },
    Key(keys::KeyDialog),
}

impl Dialog {
    pub(crate) fn connect(form: ConnectForm) -> Self {
        Self::Connect(Box::new(form))
    }
    pub(crate) fn settings(settings: &Settings, profile: Option<&crate::tunnel::Profile>) -> Self {
        Self::Settings {
            draft: settings.clone(),
            password: String::new(),
            tunnel: tunnel::Draft::new(profile),
            session_import_summary: None,
        }
    }

    fn title(&self) -> &str {
        match self {
            Self::Connect(form) => form.title(),
            Self::Settings { .. } => "Settings",
            Self::About => "About Rterm",
            Self::Exit => "Exit Rterm?",
            Self::ResetVault => "Reset vault",
            Self::ImportVault { .. } => "Import vault",
            Self::VaultNotice { imported: true, .. } => "Import vault",
            Self::VaultNotice {
                imported: false, ..
            } => "Export vault",
            Self::DeleteProfile(_) => "Delete Session",
            Self::RenameTab { .. } => "Rename Tab",
            Self::Files { action, .. } => match action {
                FileAction::Delete => "Delete",
                FileAction::Move => "Move",
                FileAction::Rename => "Rename",
                FileAction::Info => "Info",
            },
            Self::Download { .. } => "Select download folder",
            Self::TransferConflict { .. } => "Destination already exists",
            Self::Key(key) => key.title(),
        }
    }
}

#[derive(Default)]
pub(crate) struct LockForm {
    password: String,
    visible: bool,
    error: Option<String>,
}

impl App {
    pub(crate) fn show_dialogs(&mut self, context: &egui::Context) {
        let t = self.theme;
        if let Some(mut dialog) = self.dialog.take() {
            let response = modal("rterm-dialog", t).show(context, |ui| {
                let width = if matches!(dialog, Dialog::Settings { .. }) {
                    let available = context.content_rect().width();
                    let room = (available - 2.0 * f32::from(DIALOG_INSET) - 2.0).max(1.0);
                    (available - 60.0).clamp(520.0, 710.0).min(room)
                } else {
                    478.0
                };
                let dismissed = modal_header(ui, dialog.title(), t, width);
                if dismissed {
                    return true;
                }
                if !matches!(dialog, Dialog::Connect(_) | Dialog::Settings { .. }) {
                    ui.add_space(18.0);
                }
                match &mut dialog {
                    Dialog::Connect(form) => {
                        form.show(ui, t, &self.vault_keys, self.settings.on("ssh_agent"));
                        match footer(ui, t, Some("Cancel"), "Connect", false) {
                            Choice::Cancel => true,
                            Choice::Yes => match form.submit(self) {
                                Ok(()) => true,
                                Err(error) => {
                                    form.error = Some(error);
                                    false
                                }
                            },
                            Choice::None => false,
                        }
                    }
                    Dialog::Settings { draft, password, tunnel, session_import_summary } => {
                        let mut connect = false;
                        let existing_vault = storage::vault_exists();
                        let unlocked = self.vault_password.is_some();
                        let tunnel_vault_ready = unlocked && draft.on("vault_enabled");
                        let mut export = false;
                        let mut import = false;
                        let mut import_sessions = false;
                        let mut open_security = false;
                        let can_import_sessions = self.can_import_sessions() && draft.on("vault_enabled");
                        draft.show(ui, t, password, existing_vault, |ui, section| match section {
                            SettingsSection::Update => self.updater.show(ui, t),
                            SettingsSection::Tunnel => {
                                connect = tunnel.show(ui, t, tunnel_vault_ready, self.tunnel_profile.as_ref(), self.openai_tunnel.as_ref());
                            }
                            SettingsSection::VaultBackups => {
                                ui.add_space(16.0);
                                ui.separator();
                                ui.label(crate::i18n::tr(ui, "Encrypted vault backups"));
                                ui.horizontal_wrapped(|ui| {
                                    export = ui.add_enabled(unlocked && existing_vault,
                                        egui::Button::new(crate::i18n::tr(ui, "Export vault"))).clicked();
                                    import = ui.add_enabled(!existing_vault || unlocked,
                                        egui::Button::new(crate::i18n::tr(ui, "Import vault"))).clicked();
                                });
                                if existing_vault && !unlocked {
                                    ui.label(crate::i18n::tr(ui, "Unlock the existing vault to export or replace it."));
                                }
                            }
                            SettingsSection::Import => {
                                ui.label(RichText::new(crate::i18n::tr(ui, "External imports require a vault password.")).color(t.muted));
                                ui.add_space(6.0);
                                ui.horizontal_wrapped(|ui| {
                                    import_sessions = ui.add_enabled(can_import_sessions,
                                        egui::Button::new(crate::i18n::tr(ui, "Import Sessions…"))).clicked();
                                    if !can_import_sessions {
                                        open_security = ui.link(crate::i18n::tr(ui, "Open Security settings")).clicked();
                                    }
                                });
                                if let Some(summary) = session_import_summary {
                                    ui.add_space(6.0);
                                    ui.label(crate::i18n::format(ui, "Detected {format}", &[("format", summary.format)]));
                                    ui.label(crate::i18n::format(ui, "Added {added} • Duplicates {duplicates} • Skipped {skipped}", &[
                                        ("added", &summary.added.to_string()),
                                        ("duplicates", &summary.duplicates.to_string()),
                                        ("skipped", &summary.skipped.to_string()),
                                    ]));
                                }
                            }
                        });
                        if open_security {
                            draft.open_security_page();
                            context.request_repaint();
                        }
                        if import_sessions {
                            match self.import_sessions() {
                                Ok(Some(summary)) => *session_import_summary = Some(summary),
                                Ok(None) => {},
                                Err(error) => self.fail(crate::i18n::tr(ui, &error)),
                            }
                        }
                        if export && let Some(path) = rfd::FileDialog::new()
                            .add_filter("RTerm vault", &["rterm-vault"])
                            .set_file_name("backup.rterm-vault")
                            .save_file() {
                            match self.export_vault(&path) {
                                Ok(()) => self.dialog = Some(Dialog::VaultNotice { imported: false, backup: Some(path) }),
                                Err(error) => { self.fail(error); return false; }
                            }
                            return true;
                        }
                        if import && let Some(path) = rfd::FileDialog::new()
                            .add_filter("RTerm vault", &["rterm-vault", "dat"])
                            .pick_file() {
                            self.dialog = Some(Dialog::ImportVault {
                                path,
                                password: zeroize::Zeroizing::new(String::new()),
                                replace: existing_vault,
                            });
                            return true;
                        }
                        if connect {
                            let result = self.save_settings_dialog(draft.clone(), password, tunnel, context)
                                .and_then(|()| {
                                    tunnel.clear_key();
                                    self.start_agent_with_tunnel(context)
                                });
                            if let Err(error) = result {
                                self.fail(error);
                            }
                            return false;
                        }
                        match footer(ui, t, Some("Cancel"), "Apply", false) {
                            Choice::Yes => {
                                match self.save_settings_dialog(draft.clone(), password, tunnel, context) {
                                    Ok(()) => true,
                                    Err(error) => {
                                        self.fail(error);
                                        false
                                    }
                                }
                            }
                            Choice::Cancel => true,
                            Choice::None => false,
                        }
                    }
                    Dialog::About => about(ui, t),
                    Dialog::Exit => {
                        ui.label(crate::i18n::tr(ui, "Close Rterm and disconnect all sessions?"));
                        match footer(ui, t, Some("Cancel"), "Exit", false) {
                            Choice::Yes => {
                                self.exit(context);
                                true
                            }
                            Choice::Cancel => true,
                            Choice::None => false,
                        }
                    }
                    Dialog::ImportVault { path, password, replace } => {
                        ui.label(path.display().to_string());
                        ui.add_space(12.0);
                        ui.label(crate::i18n::tr(ui, "Import replaces saved sessions and keys. Close other RTerm windows and remote tabs first."));
                        if *replace {
                            ui.colored_label(t.yellow, crate::i18n::tr(ui, "The current encrypted vault will be backed up before replacement."));
                        }
                        ui.add_space(12.0);
                        form_field(ui, t, "Backup password", password, "Password", true);
                        match footer(ui, t, Some("Cancel"), "Import vault", *replace) {
                            Choice::Yes => {
                                if password.is_empty() {
                                    self.fail(crate::i18n::tr(ui, "Enter the backup's master password"));
                                    return false;
                                }
                                match self.import_vault(path, password, *replace) {
                                    Ok(backup) => {
                                        self.dialog = Some(Dialog::VaultNotice { imported: true, backup });
                                        true
                                    }
                                    Err(error) => { self.fail(error); false }
                                }
                            }
                            Choice::Cancel => true,
                            Choice::None => false,
                        }
                    }
                    Dialog::VaultNotice { imported, backup } => {
                        ui.label(crate::i18n::tr(ui, if *imported {
                            "Vault imported. Unlock with the backup's master password."
                        } else {
                            "Encrypted vault exported."
                        }));
                        if let Some(path) = backup {
                            ui.add_space(8.0);
                            if *imported {
                                ui.label(crate::i18n::tr(ui, "Previous vault backup:"));
                            }
                            ui.label(path.display().to_string());
                        }
                        footer(ui, t, None, "OK", false) == Choice::Yes
                    }
                    Dialog::ResetVault => {
                        ui.label(
                            crate::i18n::tr(ui, "Delete the encrypted vault and its saved sessions, keys, and AI command history?"),
                        );
                        match footer(ui, t, Some("Cancel"), "Reset vault", true) {
                            Choice::Yes => {
                                match storage::delete_vault() {
                                    Ok(()) => {
                                        self.locked = false;
                                        self.settings.set("vault_enabled", "0".into());
                                        self.settings.set("ai_audit_vault", "0".into());
                                        self.forget_unlocked_vault();
                                        self.lock_form = LockForm::default();
                                        self.profiles = storage::load_profiles();
                                        self.vault_keys.clear();
                                        self.apply_settings(context);
                                        self.ensure_local();
                                    }
                                    Err(error) => self.fail(error),
                                }
                                true
                            }
                            Choice::Cancel => true,
                            Choice::None => false,
                        }
                    }
                    Dialog::RenameTab { tab, name } => {
                        form_field(ui, t, "Name", name, "Terminal", false);
                        match footer(ui, t, Some("Cancel"), "Rename", false) {
                            Choice::Yes if !name.trim().is_empty() => {
                                self.rename_tab(*tab, name.trim());
                                true
                            }
                            Choice::Cancel => true,
                            _ => false,
                        }
                    }
                    Dialog::DeleteProfile(index) => {
                        if let Some(profile) = self.profiles.get(*index) {
                            ui.label(crate::i18n::format(ui, "Delete the saved session “{name}”?", &[("name", &profile.name)]));
                        }
                        match footer(ui, t, Some("Cancel"), "Delete", true) {
                            Choice::Yes => {
                                if *index < self.profiles.len() {
                                    let prior = self.profiles.remove(*index);
                                    if let Err(error) = self.save_profiles() {
                                        self.profiles.insert(*index, prior);
                                        self.fail(error);
                                    }
                                }
                                true
                            }
                            Choice::Cancel => true,
                            Choice::None => false,
                        }
                    }
                    Dialog::Files {
                        action,
                        entries,
                        input,
                    } => self.file_details(ui, action, entries, input),
                    Dialog::Download { entries, folder } => {
                        ui.label(crate::i18n::format(ui, "Files selected: {count}", &[("count", &entries.len().to_string())]));
                        file_selection_preview(ui, entries);
                        ui.add_space(8.0);
                        form_field(ui, t, "Destination Folder", folder, "Directory path", false);
                        ui.horizontal_wrapped(|ui| {
                            if ui.button(crate::i18n::tr(ui,"Browse…")).clicked()
                                && let Some(path) = rfd::FileDialog::new().set_directory(storage::expand_local_path(folder)).pick_folder() {
                                *folder = path.to_string_lossy().into_owned();
                            }
                            if ui.button(crate::i18n::tr(ui,"Downloads")).clicked() {
                                *folder = storage::default_download_dir().to_string_lossy().into_owned();
                            }
                            if ui.button(crate::i18n::tr(ui,"Current local folder")).clicked() {
                                *folder = self.explorer.local_folder().to_string_lossy().into_owned();
                            }
                        });
                        match footer(ui, t, Some("Cancel"), "Download", false) {
                            Choice::Yes => {
                                let path = storage::expand_local_path(folder.trim());
                                if folder.trim().is_empty() || !path.is_dir() {
                                    self.fail("Choose an existing destination folder");
                                    false
                                } else {
                                    self.download_targets(entries.clone(), path);
                                    true
                                }
                            }
                            Choice::Cancel => true,
                            Choice::None => false,
                        }
                    }
                    Dialog::TransferConflict { conflict, remember } => {
                        use crate::backend::ConflictChoice;
                        if conflict.is_closed() { return true; }
                        ui.label(crate::i18n::tr(ui, "A file or folder with this name already exists at the destination."));
                        ui.add_space(8.0);
                        ui.label(crate::i18n::tr(ui, "Source"));
                        ui.add(egui::Label::new(&conflict.source).wrap());
                        ui.label(crate::i18n::tr(ui, "Destination"));
                        ui.add(egui::Label::new(&conflict.destination).wrap());
                        ui.add_space(12.0);
                        if !conflict.can_replace {
                            ui.label(crate::i18n::tr(ui, "Only regular files can be replaced. Keep both or skip."));
                        }
                        ui.checkbox(remember, crate::i18n::tr(ui, "Always use this action for the current transfer batch"));
                        ui.add_space(12.0);
                        let mut selected = None;
                        ui.horizontal_wrapped(|ui| {
                            if ui.add_enabled(conflict.can_replace, egui::Button::new(crate::i18n::tr(ui, "Replace"))).clicked() {
                                selected = Some(ConflictChoice::Replace);
                            }
                            if ui.button(crate::i18n::tr(ui, "Keep both")).clicked() {
                                selected = Some(ConflictChoice::KeepBoth);
                            }
                            if ui.button(crate::i18n::tr(ui, "Skip")).clicked() {
                                selected = Some(ConflictChoice::Skip);
                            }
                        });
                        if let Some(choice) = selected {
                            conflict.decide(choice, *remember);
                            true
                        } else { false }
                    }
                    Dialog::Key(dialog) => self.key_dialog(ui, dialog),
                }
            });
            if !response.inner && !response.should_close() {
                self.dialog = Some(dialog);
            } else {
                self.focus_terminal_after_dialog();
                context.request_repaint();
            }
        }

        if let Some(error) = self.message.as_deref() {
            let response = modal("rterm-message", t).show(context, |ui| {
                let close = modal_header(ui, "Rterm", t, 478.0);
                ui.add_space(18.0);
                ui.colored_label(t.red, error);
                close || footer(ui, t, None, "OK", false) == Choice::Yes
            });
            if response.inner || response.should_close() {
                self.message = None;
                if self.dialog.is_none() {
                    self.focus_terminal_after_dialog();
                    context.request_repaint();
                }
            }
        }
    }

    fn file_details(
        &mut self,
        ui: &mut egui::Ui,
        action: &FileAction,
        entries: &[Target],
        input: &mut String,
    ) -> bool {
        let mut affirmative = "OK";
        let mut cancel = Some("Cancel");
        let mut danger = false;
        if !matches!(action, FileAction::Info) {
            file_selection_preview(ui, entries);
            ui.add_space(8.0);
        }
        match action {
            FileAction::Info => {
                if let Some(target) = entries.first() {
                    let entry = target.entry();
                    ui.label(RichText::new(&entry.name).strong());
                    ui.label(target.path());
                    ui.label(if entry.directory {
                        crate::i18n::tr(ui, "Folder").to_owned()
                    } else {
                        format!("Size  {}", format_size(entry.size))
                    });
                    if let Some(mode) = entry.permissions {
                        let mut text = String::new();
                        for index in (0..9).rev() {
                            text.push(if mode & (1 << index) == 0 {
                                '-'
                            } else {
                                ['x', 'w', 'r'][index % 3]
                            });
                        }
                        ui.label(format!("Permissions  {text} ({mode:o})"));
                    }
                }
                cancel = None;
            }
            FileAction::Rename => {
                form_field(ui, self.theme, "New Name", input, "Filename", false);
                affirmative = "Rename";
            }
            FileAction::Move => {
                form_field(
                    ui,
                    self.theme,
                    "Destination Folder",
                    input,
                    "Directory path",
                    false,
                );
                if matches!(entries.first(), Some(Target::Local { .. }))
                    && ui.button(crate::i18n::tr(ui, "Browse…")).clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .set_directory(storage::expand_local_path(input))
                        .pick_folder()
                {
                    *input = path.to_string_lossy().into_owned();
                }
                if let Some(Target::Remote { id, path, .. }) = entries.first() {
                    ui.horizontal_wrapped(|ui| {
                        if ui.button(crate::i18n::tr(ui, "Parent Folder")).clicked()
                            && let Some((parent, _)) = input.trim_end_matches('/').rsplit_once('/')
                        {
                            *input = if parent.is_empty() {
                                "/".into()
                            } else {
                                parent.into()
                            };
                        }
                        if ui.button(crate::i18n::tr(ui, "Source folder")).clicked() {
                            *input = path
                                .rsplit_once('/')
                                .map_or(
                                    "/",
                                    |(parent, _)| if parent.is_empty() { "/" } else { parent },
                                )
                                .to_owned();
                        }
                    });
                    let folders = self.remote_folder_suggestions(*id, input);
                    egui::ScrollArea::vertical()
                        .max_height(100.0)
                        .show(ui, |ui| {
                            for name in folders {
                                if ui.selectable_label(false, &name).clicked() {
                                    *input = crate::backend::join_remote(input, &name);
                                }
                            }
                        });
                }
                affirmative = "Move";
            }
            FileAction::Delete => {
                if entries.len() == 1 {
                    ui.label(crate::i18n::format(
                        ui,
                        "Permanently delete “{name}”?",
                        &[("name", &entries[0].entry().name)],
                    ));
                } else {
                    ui.label(crate::i18n::format(
                        ui,
                        "Permanently delete selected items? Count: {count}",
                        &[("count", &entries.len().to_string())],
                    ));
                }
                affirmative = "Delete";
                danger = true;
            }
        }
        match footer(ui, self.theme, cancel, affirmative, danger) {
            Choice::Cancel => true,
            Choice::Yes => {
                if matches!(action, FileAction::Rename | FileAction::Move)
                    && input.trim().is_empty()
                {
                    self.fail("Enter a destination");
                    return false;
                }
                if let Err(error) = self.apply_file_action(*action, entries.to_vec(), input.trim())
                {
                    self.fail(error);
                    return false;
                }
                true
            }
            Choice::None => false,
        }
    }

    pub(crate) fn lock_screen(&mut self, ui: &mut egui::Ui) {
        let t = self.theme;
        let bounds = ui.max_rect();
        theme::band(ui, bounds, t.bg, None);
        let area = Rect::from_center_size(bounds.center(), vec2(320.0, 155.0));
        let title = area.with_max_y(area.top() + 24.0);
        theme::label(
            ui,
            title,
            crate::i18n::tr(ui, "Screen Locked"),
            theme::strong(20.0),
            t.accent,
            Align2::CENTER_CENTER,
        );
        let input = Rect::from_min_size(pos2(area.left(), area.top() + 56.0), vec2(320.0, 40.0));
        let response = ui.put(
            input,
            egui::TextEdit::singleline(&mut self.lock_form.password)
                .password(!self.lock_form.visible)
                .hint_text(crate::i18n::tr(ui, "Password"))
                .font(t.chrome_font(14.0))
                .margin(egui::Margin {
                    left: 12,
                    right: 40,
                    top: 8,
                    bottom: 8,
                }),
        );
        if !response.has_focus() && self.dialog.is_none() && self.message.is_none() {
            response.request_focus();
        }
        let eye = Rect::from_center_size(
            pos2(input.right() - 22.0, input.center().y),
            vec2(26.0, 26.0),
        );
        if theme::icon_button(ui, eye, "Show password", Icon::Eye, t).clicked() {
            self.lock_form.visible = !self.lock_form.visible;
        }
        if self.dialog.is_none() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            let password = std::mem::take(&mut self.lock_form.password);
            match self.unlock(password) {
                Ok(()) => self.lock_form = LockForm::default(),
                Err(error) => self.lock_form.error = Some(error),
            }
        }
        if let Some(error) = &self.lock_form.error {
            theme::label(
                ui,
                Rect::from_min_size(input.left_bottom(), vec2(input.width(), 25.0)),
                error,
                t.chrome_font(12.0),
                t.red,
                Align2::CENTER_CENTER,
            );
        }
        let reset = Rect::from_center_size(
            pos2(area.center().x, input.bottom() + 44.0),
            vec2(100.0, 24.0),
        );
        if theme::button(
            ui,
            reset,
            "reset-vault",
            "Reset vault",
            t.chrome_font(11.0),
            t.dim,
            t,
        )
        .clicked()
        {
            self.dialog = Some(Dialog::ResetVault);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    Yes,
    Cancel,
    None,
}

fn modal(name: &str, t: Theme) -> egui::Modal {
    let frame = egui::Frame::new()
        .fill(t.panel)
        .stroke(Stroke::new(1.0_f32, t.outline))
        .corner_radius(0)
        .inner_margin(egui::Margin::symmetric(DIALOG_INSET, 0))
        .shadow(egui::Shadow::NONE);
    egui::Modal::new(egui::Id::new(name))
        .frame(frame)
        .backdrop_color(Color32::from_black_alpha(125))
}

fn modal_header(ui: &mut egui::Ui, title: &str, t: Theme, width: f32) -> bool {
    ui.set_width(width);
    ui.spacing_mut().item_spacing.y = 0.0;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
    theme::band(
        ui,
        rect.expand2(vec2(f32::from(DIALOG_INSET), 0.0)),
        t.bar,
        None,
    );
    theme::label(
        ui,
        rect.with_max_x(rect.right() - 32.0),
        crate::i18n::tr(ui, title),
        t.chrome_font(13.0),
        t.text,
        Align2::LEFT_CENTER,
    );
    modal_rule(ui, rect.left(), rect.right(), rect.bottom() - 0.5, t);
    let close =
        Rect::from_center_size(pos2(rect.right() - 12.0, rect.center().y), vec2(24.0, 24.0));
    theme::icon_button(ui, close, "Close Dialog", Icon::Close, t).clicked()
}

fn modal_rule(ui: &egui::Ui, left: f32, right: f32, y: f32, t: Theme) {
    let inset = f32::from(DIALOG_INSET);
    ui.painter().hline(
        (left - inset)..=(right + inset),
        y,
        Stroke::new(1.0_f32, t.border),
    );
}

fn footer(
    ui: &mut egui::Ui,
    t: Theme,
    cancel: Option<&str>,
    primary: &str,
    danger: bool,
) -> Choice {
    ui.add_space(8.0);
    let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    modal_rule(ui, line.left(), line.right(), line.center().y, t);
    ui.add_space(8.0);
    let choice = ui
        .allocate_ui_with_layout(
            vec2(ui.available_width(), 28.0),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let fill = if danger { t.red } else { t.accent };
                let text = if fill.intensity() < 0.52 {
                    Color32::WHITE
                } else {
                    Color32::BLACK
                };
                let primary = crate::i18n::tr(ui, primary);
                let primary_width =
                    92.0_f32.max(theme::text_width(ui, primary, t.chrome_font(12.0)) + 18.0);
                let confirm = ui
                    .add_sized(
                        [primary_width, 26.0],
                        egui::Button::new(
                            RichText::new(primary).color(text).font(t.chrome_font(12.0)),
                        )
                        .fill(fill),
                    )
                    .clicked();
                let cancelled = cancel.is_some_and(|label| {
                    let label = crate::i18n::tr(ui, label);
                    let width =
                        84.0_f32.max(theme::text_width(ui, label, t.chrome_font(12.0)) + 18.0);
                    ui.add_sized([width, 26.0], egui::Button::new(label))
                        .clicked()
                });
                if confirm {
                    Choice::Yes
                } else if cancelled {
                    Choice::Cancel
                } else {
                    Choice::None
                }
            },
        )
        .inner;
    ui.add_space(10.0);
    choice
}

fn about(ui: &mut egui::Ui, t: Theme) -> bool {
    ui.horizontal(|ui| {
        let (glyph, _) = ui.allocate_exact_size(vec2(50.0, 50.0), Sense::hover());
        ui.painter().rect_stroke(
            glyph,
            0.0,
            Stroke::new(1.0_f32, t.border),
            egui::StrokeKind::Inside,
        );
        theme::icon(ui, glyph.shrink(9.0), Icon::Terminal, t.accent);
        ui.add_space(8.0);
        ui.vertical(|ui| {
            ui.label(
                RichText::new("RTerm V2")
                    .font(t.chrome_font(20.0))
                    .color(t.text),
            );
            ui.label(
                RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION")))
                    .font(t.chrome_font(11.0))
                    .color(t.muted),
            );
            ui.label(
                RichText::new("Native terminal & remote session manager")
                    .font(t.chrome_font(11.0))
                    .color(t.muted),
            );
        });
    });
    ui.add_space(12.0);
    ui.separator();
    ui.add_space(10.0);
    egui::Grid::new("about-details")
        .num_columns(2)
        .spacing(vec2(18.0, 8.0))
        .show(ui, |ui| {
            for (label, value) in [
                ("Developer", "Libalpm64"),
                ("Platform", std::env::consts::OS),
                ("Architecture", std::env::consts::ARCH),
                ("Terminal engine", "Alacritty"),
                ("Interface", "Rust / egui"),
                ("SSH", "Russh"),
            ] {
                ui.label(RichText::new(label).color(t.muted));
                ui.label(value);
                ui.end_row();
            }
        });
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.hyperlink_to("Project", "https://github.com/libalpm64/RTerm");
        ui.hyperlink_to(
            "Report issue",
            "https://github.com/libalpm64/RTerm/issues/new",
        );
    });
    footer(ui, t, None, "Close", false) == Choice::Yes
}

fn file_selection_preview(ui: &mut egui::Ui, entries: &[Target]) {
    egui::ScrollArea::vertical()
        .max_height(90.0)
        .show(ui, |ui| {
            for target in entries {
                let name = crate::agent_upload::literal_text(target.entry().name.as_bytes());
                let path = crate::agent_upload::literal_text(target.path().as_bytes());
                ui.add(egui::Label::new(RichText::new(name).strong()).truncate())
                    .on_hover_text(path);
            }
        });
}
