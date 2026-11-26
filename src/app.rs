use std::{
    collections::BTreeMap,
    sync::mpsc,
    time::{Duration, Instant},
};

use eframe::egui::{self, Align2, Color32, FontId, Key, Rect, Sense, Stroke, pos2, vec2};

use crate::{
    backend::{Backend, BackendEvent, BackendResult, Command, Reply as BackendReply},
    dialogs::{ConnectForm, Dialog, KeyManager, Settings},
    explorer::Explorer,
    highlight::Highlighter,
    models::{Connection, Profile, SshKey},
    storage,
    terminal::{Kind, Terminal},
    theme::{self, Icon, Theme},
};

type Reply = Box<dyn FnOnce(&mut App, BackendResult)>;
struct Pending {
    response: mpsc::Receiver<BackendResult>,
    callback: Reply,
}
struct Transfer {
    text: String,
    percent: f32,
    expires: Option<Instant>,
    error: bool,
}

#[derive(Clone, Copy)]
enum Action {
    New,
    Connect,
    Settings,
    Exit,
    Copy,
    Paste,
    SelectAll,
    Clear,
    JumpCommand(bool),
    Explorer,
    Sessions,
    BottomShell,
    Zoom(f32),
    ResetZoom,
    Keys,
    GenerateKey,
    ImportKey,
    KnownHosts,
    About,
    Docs,
    Issue,
    Lock,
    AiSettings,
    AiStart,
    AiCopyPrompt,
    AiRevoke,
    AiActivity,
    AiTunnelToggle,
}

pub(crate) struct App {
    pub(crate) backend: Backend,
    events: tokio::sync::mpsc::Receiver<BackendEvent>,
    pending: Vec<Pending>,
    local_profiles: BTreeMap<u64, (Profile, Option<usize>)>,
    ssh_tabs: BTreeMap<u64, ssh_reconnect::SshTab>,
    host_trust: host_trust::Trust,
    pub(crate) settings: Settings,
    pub(crate) theme: Theme,
    pub(crate) wallpaper: Option<crate::background::Wallpaper>,
    agent: agent_ui::State,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    desktop_shortcut: Option<crate::desktop_shortcut::Setup>,
    pub(crate) dialog: Option<Dialog>,
    pub(crate) message: Option<String>,
    pub(crate) profiles: Vec<Profile>,
    pub(crate) vault_keys: Vec<SshKey>,
    pub(crate) vault_password: Option<String>,
    vault_key: Option<storage::VaultKey>,
    pub(crate) tunnel_profile: Option<crate::tunnel::Profile>,
    pub(crate) openai_tunnel: Option<crate::tunnel::Running>,
    pub(crate) locked: bool,
    pub(crate) lock_form: crate::dialogs::LockForm,
    pub(crate) keys: KeyManager,
    pub(crate) explorer: Explorer,
    terminals: Vec<Terminal>,
    bottom_shell: Option<Terminal>,
    bottom_open: bool,
    bottom_focus: bool,
    bottom_request_focus: bool,
    bottom_height: f32,
    bottom_font: f32,
    active: Option<u64>,
    next_tab: u64,
    show_keys: bool,
    focus_terminal: bool,
    terminal_navigation: Vec<(usize, egui::Event)>,
    wheel_zoom_remainder: f32,
    wheel_zoom_tab: Option<u64>,
    pending_wheel_zoom: Option<(u64, f32, f64)>,
    pub(crate) sidebar: Option<f32>,
    sessions_sidebar: Option<f32>,
    drag_tab: Option<u64>,
    history: Vec<String>,
    persist_history: bool,
    history_visible: bool,
    filter: String,
    active_filter: String,
    dock_fraction: f32,
    transfers: BTreeMap<String, Transfer>,
    last_activity: Instant,
    exit_confirmed: bool,
    deferred_events: Vec<BackendEvent>,
    highlighter: Highlighter,
}

impl App {
    pub(crate) fn new(context: &egui::Context) -> Self {
        Self::with_settings(context, Settings::load(), true)
    }

    fn with_settings(context: &egui::Context, settings: Settings, restore_sessions: bool) -> Self {
        // RTerm owns terminal zoom; egui must not resize the entire UI.
        context.options_mut(|options| options.zoom_with_keyboard = false);
        crate::i18n::set(
            context,
            if restore_sessions {
                crate::i18n::resolve(settings.get("language"))
            } else {
                "en"
            },
        );
        theme::install_fonts(context);
        let theme = Theme::dark();
        theme.apply(context);
        let (backend, events) = Backend::new(context);
        let locked = restore_sessions && settings.on("vault_enabled") && storage::vault_exists();
        let mut profiles = if locked || !restore_sessions {
            Vec::new()
        } else {
            storage::load_profiles()
        };
        let mut vault_keys = Vec::new();
        if restore_sessions
            && !locked
            && let Ok((vault, _)) = storage::load_vault("")
        {
            if profiles.is_empty() {
                profiles = vault.profiles;
            }
            vault_keys = vault.keys;
        }
        let history = restore_sessions
            .then(|| storage::load_setting("command_history"))
            .flatten()
            .and_then(|saved| serde_json::from_str::<Vec<String>>(&saved).ok())
            .unwrap_or_default()
            .into_iter()
            .take(50)
            .map(|mut line| {
                line.truncate(line.floor_char_boundary(4096));
                line
            })
            .collect();
        let highlighter = Highlighter::new(
            settings.on("semantic_hl"),
            settings.get("highlight_keywords"),
        );
        let (wallpaper, background_error) =
            match crate::background::Wallpaper::from_settings(context, &settings, None) {
                Ok(wallpaper) => (wallpaper, None),
                Err(error) => (None, (!locked).then_some(error)),
            };
        let bottom_font = settings
            .get("bottom_font_size")
            .parse::<u8>()
            .map(|size| f32::from(size.clamp(8, 32)))
            .unwrap_or_else(|_| settings.terminal().font);
        let mut app = Self {
            backend,
            events,
            pending: Vec::new(),
            local_profiles: BTreeMap::new(),
            ssh_tabs: BTreeMap::new(),
            host_trust: host_trust::Trust::new(restore_sessions),
            settings,
            theme,
            wallpaper,
            agent: Default::default(),
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            desktop_shortcut: if restore_sessions {
                crate::desktop_shortcut::Setup::discover().filter(|setup| setup.pending())
            } else {
                None
            },
            dialog: None,
            message: background_error,
            profiles,
            vault_keys,
            vault_password: None,
            vault_key: None,
            tunnel_profile: None,
            openai_tunnel: None,
            locked,
            lock_form: Default::default(),
            keys: KeyManager::default(),
            explorer: Explorer::new(),
            terminals: Vec::new(),
            bottom_shell: None,
            bottom_open: false,
            bottom_focus: false,
            bottom_request_focus: false,
            bottom_height: 180.0,
            bottom_font,
            active: None,
            next_tab: 1,
            show_keys: false,
            focus_terminal: true,
            terminal_navigation: Vec::new(),
            wheel_zoom_remainder: 0.0,
            wheel_zoom_tab: None,
            pending_wheel_zoom: None,
            sidebar: Some(240.0),
            sessions_sidebar: Some(220.0),
            drag_tab: None,
            history,
            persist_history: restore_sessions,
            history_visible: true,
            filter: String::new(),
            active_filter: String::new(),
            dock_fraction: 0.30,
            transfers: BTreeMap::new(),
            last_activity: Instant::now(),
            exit_confirmed: false,
            deferred_events: Vec::new(),
            highlighter,
        };
        if restore_sessions && !locked {
            app.new_local();
        }
        app
    }

    pub(crate) fn request(
        &mut self,
        command: Command,
        callback: impl FnOnce(&mut Self, BackendResult) + 'static,
    ) {
        self.pending.push(Pending {
            response: self.backend.request(command),
            callback: Box::new(callback),
        });
    }

    fn poll(&mut self, context: &egui::Context) {
        let mut completed = Vec::new();
        let mut startup_error = None;
        for terminal in self
            .terminals
            .iter_mut()
            .chain(self.bottom_shell.iter_mut())
        {
            if let Some(result) = terminal.poll_local() {
                match result {
                    Ok(()) => completed.push(terminal.tab),
                    Err(error) => {
                        terminal.mark_closed();
                        terminal
                            .receive(format!("Could not start the shell: {error}\r\n").as_bytes());
                        self.local_profiles.remove(&terminal.tab);
                        startup_error = Some(error);
                    }
                }
            }
            if terminal.launching() {
                context.request_repaint_after(Duration::from_millis(16));
            }
        }
        for tab in completed {
            if let Some((profile, save)) = self.local_profiles.remove(&tab) {
                self.remember_profile(profile, Some(save));
            }
        }
        if let Some(error) = startup_error {
            self.fail(error);
        }
        for pending in std::mem::take(&mut self.pending) {
            let result = match pending.response.try_recv() {
                Ok(response) => response,
                Err(mpsc::TryRecvError::Disconnected) => Err("Backend disconnected".into()),
                Err(mpsc::TryRecvError::Empty) => {
                    self.pending.push(pending);
                    continue;
                }
            };
            (pending.callback)(self, result);
        }
        if !self.pending.is_empty() {
            context.request_repaint_after(Duration::from_millis(25));
        }
        for event in std::mem::take(&mut self.deferred_events) {
            self.handle_event(event);
        }
        self.backend.events.begin_drain();
        let drain_started = Instant::now();
        for index in 0..256 {
            let Ok(event) = self.events.try_recv() else {
                break;
            };
            self.handle_event(event);
            // Leave time for input and painting during continuous remote output.
            if index == 255 || drain_started.elapsed() >= Duration::from_millis(6) {
                context.request_repaint();
                break;
            }
        }
        self.poll_ssh_reconnect(context);
        for terminal in self
            .terminals
            .iter_mut()
            .chain(self.bottom_shell.iter_mut())
        {
            terminal.service(
                &self.backend,
                context,
                self.settings.on("bell_enabled") && !self.locked,
            );
        }
        let now = Instant::now();
        self.transfers
            .retain(|_, transfer| transfer.expires.is_none_or(|time| time > now));
    }

    fn handle_event(&mut self, event: BackendEvent) {
        match event {
            BackendEvent::Data { id, data } => {
                if let Some(terminal) = self
                    .terminals
                    .iter_mut()
                    .chain(self.bottom_shell.iter_mut())
                    .find(|terminal| terminal.endpoint == Some(id))
                {
                    terminal.receive(&data);
                } else if !self.pending.is_empty() && self.deferred_events.len() < 256 {
                    self.deferred_events.push(BackendEvent::Data { id, data });
                }
            }
            BackendEvent::SshShellExited(id) => {
                if !self.ssh_transport_closed(id, "SSH shell exited", false)
                    && !self.pending.is_empty()
                    && self.deferred_events.len() < 256
                {
                    self.deferred_events.push(BackendEvent::SshShellExited(id));
                }
            }
            BackendEvent::Closed(id) => {
                if self.ssh_transport_closed(id, "SSH disconnected", true) {
                    return;
                }
                if let Some(terminal) = self
                    .terminals
                    .iter_mut()
                    .chain(self.bottom_shell.iter_mut())
                    .find(|terminal| terminal.endpoint == Some(id))
                {
                    if terminal.closed {
                        return;
                    }
                    terminal.mark_closed();
                    self.local_profiles.remove(&terminal.tab);
                    terminal.receive(b"\r\n\x1b[90m[Session closed]\x1b[0m\r\n");
                    if terminal.kind != Kind::Local {
                        drop(self.backend.request(Command::Disconnect(id)));
                    }
                } else if !self.pending.is_empty() && self.deferred_events.len() < 256 {
                    self.deferred_events.push(BackendEvent::Closed(id));
                }
            }
            BackendEvent::InputFailed { id, error } => {
                if self.ssh_transport_closed(id, &format!("SSH input failed: {error}"), true) {
                    return;
                }
                if let Some(terminal) = self
                    .terminals
                    .iter_mut()
                    .find(|terminal| terminal.endpoint == Some(id) && !terminal.closed)
                {
                    terminal.mark_closed();
                    // Report a genuine transport failure instead of leaving a
                    // dead writer behind a still-visible interactive prompt.
                    terminal.receive(format!("\r\n\x1b[90m[SSH input failed: {error}; reconnect to resume]\x1b[0m\r\n").as_bytes());
                    drop(self.backend.request(Command::Disconnect(id)));
                }
            }
            BackendEvent::Progress {
                id,
                text,
                percent,
                finished,
                error,
            } => {
                let expires = finished.then(|| Instant::now() + Duration::from_secs(4));
                self.transfers.insert(
                    id,
                    Transfer {
                        text,
                        percent,
                        expires,
                        error,
                    },
                );
            }
            BackendEvent::RemoteChanged(id) => self.refresh_remote_if(id),
        }
    }

    pub(crate) fn fail(&mut self, error: impl ToString) {
        self.message = Some(error.to_string());
    }

    pub(crate) fn save_profiles(&mut self) -> Result<(), String> {
        if let Some(key) = &self.vault_key {
            storage::save_vault(&self.profiles, &self.vault_keys, key, false)
        } else {
            storage::save_profiles(&self.profiles)
        }
    }

    pub(crate) fn create_vault(&mut self, password: &str) -> Result<(), String> {
        let key = storage::VaultKey::derive(password);
        storage::save_vault(&self.profiles, &self.vault_keys, &key, true)?;
        self.vault_key = Some(key);
        self.vault_password = Some(password.into());
        Ok(())
    }

    pub(crate) fn forget_unlocked_vault(&mut self) {
        self.stop_tunnel();
        self.tunnel_profile = None;
        self.vault_key = None;
        self.vault_password = None;
        self.agent.activity.forget_vault();
    }

    pub(crate) fn unlock(&mut self, password: String) -> Result<(), String> {
        let (vault, key) = storage::load_vault(&password)?;
        self.profiles = vault.profiles;
        self.vault_keys = vault.keys;
        self.agent.activity.load(vault.agent_audit);
        self.tunnel_profile = vault.openai_tunnel;
        self.vault_key = Some(key);
        self.vault_password = Some(password);
        self.restore_ssh_credentials();
        self.locked = false;
        self.focus_terminal = true;
        self.last_activity = Instant::now();
        if self.terminals.is_empty() {
            self.new_local();
        }
        Ok(())
    }

    pub(crate) fn lock(&mut self) {
        if !self.settings.on("vault_enabled") || !storage::vault_exists() {
            return;
        }
        self.stop_agent();
        self.dialog = None;
        self.message = None;
        self.keys = KeyManager::default();
        self.forget_unlocked_vault();
        self.agent.activity.clear();
        self.profiles.clear();
        self.vault_keys.clear();
        self.local_profiles.clear();
        self.forget_ssh_credentials();
        self.locked = true;
    }

    pub(crate) fn apply_settings(&mut self, context: &egui::Context) {
        // Any committed AI-policy change invalidates existing credentials/grants.
        self.stop_agent();
        let locale = crate::i18n::resolve(self.settings.get("language"));
        if locale != crate::i18n::current(context) {
            crate::i18n::set(context, locale);
            theme::install_fonts(context);
        }
        self.theme = Theme::dark();
        self.theme.apply(context);
        self.highlighter = Highlighter::new(
            self.settings.on("semantic_hl"),
            self.settings.get("highlight_keywords"),
        );
        if let Err(error) = self.settings.save() {
            self.fail(error);
        }
    }

    pub(crate) fn save_tunnel_profile(
        &mut self,
        profile: Option<crate::tunnel::Profile>,
    ) -> Result<(), String> {
        if self.tunnel_profile == profile {
            return Ok(());
        }
        let key = self
            .vault_key
            .as_ref()
            .ok_or("Unlock the encrypted vault to save tunnel credentials")?;
        storage::save_openai_tunnel(key, profile.clone())?;
        self.stop_tunnel();
        self.tunnel_profile = profile;
        Ok(())
    }

    pub(crate) fn ensure_local(&mut self) {
        if self.terminals.is_empty() {
            self.new_local();
        }
    }
    pub(crate) fn focus_terminal_after_dialog(&mut self) {
        self.bottom_request_focus = self.bottom_open && self.bottom_focus;
        self.focus_terminal = !self.bottom_request_focus && !self.show_keys;
    }
    fn new_local(&mut self) {
        self.connect(
            Profile {
                name: format!("Terminal {}", self.next_tab),
                connection: Connection::Local {
                    shell: String::new(),
                },
            },
            None,
        );
    }

    fn toggle_bottom_shell(&mut self) {
        self.bottom_open = !self.bottom_open;
        if !self.bottom_open {
            self.focus_terminal = true;
            self.bottom_focus = false;
            self.bottom_request_focus = false;
            return;
        }
        if self
            .bottom_shell
            .as_ref()
            .is_none_or(|terminal| terminal.closed)
        {
            let tab = self.next_tab;
            self.next_tab += 1;
            let mut terminal =
                Terminal::new(tab, "Shell".into(), Kind::Local, &self.bottom_options());
            terminal.queue_local("", false);
            self.bottom_shell = Some(terminal);
        }
        self.bottom_focus = true;
        self.bottom_request_focus = true;
        self.focus_terminal = false;
    }

    pub(crate) fn connect(&mut self, profile: Profile, save: Option<Option<usize>>) {
        let tab = self.next_tab;
        self.next_tab += 1;
        let kind = profile.kind();
        let mut terminal =
            Terminal::new(tab, profile.name.clone(), kind, &self.settings.terminal());
        if let Connection::Local { shell } = &profile.connection {
            terminal.queue_local(shell, true);
            if let Some(save) = save {
                self.local_profiles.insert(tab, (profile, save));
            }
            self.terminals.push(terminal);
            self.activate(tab);
            return;
        }
        if kind == Kind::Ssh {
            self.ssh_tabs
                .insert(tab, ssh_reconnect::SshTab::new(profile.clone()));
        }
        terminal.remote_target = Some(profile.subtitle());
        self.terminals.push(terminal);
        self.activate(tab);
        self.start_remote_connection(tab, profile, save);
    }

    fn start_remote_connection(&mut self, tab: u64, profile: Profile, save: Option<Option<usize>>) {
        let mut host_verification = None;
        let (command, terminal_type) = match &profile.connection {
            Connection::Ssh { config, terminal } => {
                let mut config = config.clone();
                if config.keepalive == 0 && self.settings.on("ssh_auto_reconnect") {
                    // Older profiles may have keepalives disabled. Probe idle
                    // connections so a reboot can be detected without typing.
                    config.keepalive = 15;
                }
                if config.key_name.is_some() {
                    config.vault_pass = self.vault_password.clone();
                }
                let host = crate::host_keys::target(&config.host, config.port);
                let trusted = match self.host_trust.store.list() {
                    Ok(mut entries) => entries.remove(&host),
                    Err(error) => {
                        self.remote_connection_failed(tab, &error);
                        return;
                    }
                };
                host_verification = Some((host, trusted.clone()));
                (Command::Ssh { config, trusted }, Some(terminal.clone()))
            }
            Connection::Telnet(config) => (Command::Telnet(config.clone()), None),
            Connection::Serial(config) => (Command::Serial(config.clone()), None),
            Connection::Local { .. } => unreachable!(),
        };
        self.request(command, move |app, response| {
            if let Ok(BackendReply::HostKey(presented)) = response {
                if let Some((host, trusted)) = host_verification
                    && app.terminals.iter().any(|term| term.tab == tab)
                {
                    app.request_host_trust(host_trust::Prompt {
                        tab,
                        host,
                        profile,
                        save,
                        trusted,
                        presented,
                    });
                }
                return;
            }
            let result = response.and_then(BackendReply::session_id);
            let Some(terminal) = app.terminals.iter_mut().find(|item| item.tab == tab) else {
                if let Ok(id) = result {
                    drop(app.backend.request(Command::Disconnect(id)));
                }
                return;
            };
            match result {
                Ok(id) => {
                    terminal.endpoint = Some(id);
                    if let Some((_, key)) = host_verification
                        && let Some(state) = app.ssh_tabs.get_mut(&tab)
                    {
                        state.key = key;
                    }
                    if let Some(terminal_type) = terminal_type {
                        let size = terminal.size();
                        app.request(
                            Command::Shell {
                                id,
                                terminal: terminal_type,
                                size,
                            },
                            move |app, result| {
                                if let Some(terminal) =
                                    app.terminals.iter_mut().find(|item| item.tab == tab)
                                    && terminal.endpoint == Some(id)
                                    && !terminal.closed
                                {
                                    match result {
                                        Ok(_) => {
                                            terminal.ssh_ready(&app.backend, size);
                                            if let Some(state) = app.ssh_tabs.get_mut(&tab) {
                                                state.connected();
                                            }
                                            app.remember_profile(profile, save);
                                            if app.active_ssh() == Some(id)
                                                && app.explorer.remote_active
                                            {
                                                app.activate_remote(Some(id));
                                            }
                                        }
                                        Err(error) => {
                                            app.ssh_transport_closed(
                                                id,
                                                &format!("Shell failed: {error}"),
                                                true,
                                            );
                                        }
                                    }
                                }
                            },
                        );
                    } else {
                        app.remember_profile(profile, save);
                    }
                }
                Err(error) => app.remote_connection_failed(tab, &error),
            }
        });
    }

    fn remote_connection_failed(&mut self, tab: u64, error: &str) {
        if let Some(terminal) = self.terminals.iter_mut().find(|term| term.tab == tab) {
            terminal.mark_closed();
            let error = agent_ui::literal_text(error.as_bytes());
            terminal.receive(format!("Connection failed: {error}\r\n").as_bytes());
        }
        if let Some(state) = self.ssh_tabs.get_mut(&tab) {
            state.connection_failed(error);
        }
    }

    fn remember_profile(&mut self, profile: Profile, save: Option<Option<usize>>) {
        if self.locked {
            return;
        }
        match save {
            Some(Some(index)) if index < self.profiles.len() => self.profiles[index] = profile,
            Some(_) => self.profiles.push(profile),
            None => return,
        }
        if let Err(error) = self.save_profiles() {
            self.fail(error);
        }
    }

    fn activate(&mut self, tab: u64) {
        let old_ssh = self.active_ssh();
        self.active = Some(tab);
        self.show_keys = false;
        self.focus_terminal = true;
        self.bottom_focus = false;
        self.bottom_request_focus = false;
        let new_ssh = self.active_ssh();
        if old_ssh != new_ssh && self.explorer.remote_active {
            self.activate_remote(new_ssh);
        }
    }

    fn close_tab(&mut self, tab: u64) {
        let Some(index) = self
            .terminals
            .iter()
            .position(|terminal| terminal.tab == tab)
        else {
            return;
        };
        let mut terminal = self.terminals.remove(index);
        self.host_trust.remove_tab(tab);
        if let Some(ssh) = self.ssh_tabs.remove(&tab) {
            self.agent.revoke_session(ssh.session_id);
        }
        self.agent.remove_display(tab);
        self.local_profiles.remove(&tab);
        if terminal.kind == Kind::Local {
            terminal.mark_closed();
        }
        if terminal.kind != Kind::Local
            && let Some(id) = terminal.endpoint
        {
            drop(self.backend.request(Command::Disconnect(id)));
        }
        if self.active == Some(tab) {
            self.active = self
                .terminals
                .get(index.min(self.terminals.len().saturating_sub(1)))
                .map(|terminal| terminal.tab);
            self.focus_terminal = true;
            if self.explorer.remote_active {
                self.activate_remote(self.active_ssh());
            }
        }
    }

    pub(crate) fn active_ssh(&self) -> Option<u32> {
        self.terminals
            .iter()
            .find(|terminal| {
                Some(terminal.tab) == self.active && terminal.kind == Kind::Ssh && !terminal.closed
            })
            .and_then(|terminal| terminal.endpoint)
    }

    pub(crate) fn rename_tab(&mut self, id: u64, name: &str) {
        if let Some(terminal) = self
            .terminals
            .iter_mut()
            .find(|terminal| terminal.tab == id)
        {
            terminal.name = name.to_owned();
        }
    }

    fn selected(&mut self) -> Option<&mut Terminal> {
        if self.bottom_open && self.bottom_focus {
            return self.bottom_shell.as_mut();
        }
        self.terminals
            .iter_mut()
            .find(|terminal| Some(terminal.tab) == self.active)
    }

    fn show_connect(&mut self) {
        self.dialog = Some(Dialog::connect(ConnectForm::new(&self.settings)));
    }

    fn run_action(&mut self, action: Action, context: &egui::Context) {
        match action {
            Action::New => self.new_local(),
            Action::Connect => self.show_connect(),
            Action::Settings => {
                self.dialog = Some(Dialog::settings(
                    &self.settings,
                    self.tunnel_profile.as_ref(),
                ))
            }
            Action::Exit => self.request_exit(context),
            Action::Copy => {
                if let Some(terminal) = self.selected() {
                    terminal.copy(context);
                }
            }
            Action::Paste => {
                let text = arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text());
                if let Ok(text) = text
                    && let Some(terminal) = if self.bottom_open && self.bottom_focus {
                        self.bottom_shell.as_ref()
                    } else {
                        self.terminals
                            .iter()
                            .find(|item| Some(item.tab) == self.active)
                    }
                {
                    terminal.paste(&self.backend, &text);
                }
                self.focus_terminal_after_dialog();
            }
            Action::SelectAll => {
                if let Some(terminal) = self.selected() {
                    terminal.select_all();
                }
            }
            Action::Clear => {
                if let Some(terminal) = self.selected() {
                    terminal.clear();
                }
                self.focus_terminal_after_dialog();
            }
            Action::JumpCommand(previous) => {
                if let Some(terminal) = self.selected() {
                    terminal.jump_command(previous);
                }
                self.focus_terminal_after_dialog();
            }
            Action::Explorer => {
                self.sidebar = if self.sidebar.is_some() {
                    None
                } else {
                    Some(240.0)
                }
            }
            Action::Sessions => {
                self.sessions_sidebar = if self.sessions_sidebar.is_some() {
                    None
                } else {
                    Some(220.0)
                }
            }
            Action::BottomShell => self.toggle_bottom_shell(),
            Action::Zoom(delta) => {
                let tab = self.zoom_target();
                self.zoom(tab, self.zoom_base(tab) + delta);
            }
            Action::ResetZoom => self.zoom(self.zoom_target(), 13.0),
            Action::Keys => {
                self.keys.open = true;
                self.show_keys = true;
                self.bottom_focus = false;
                self.keys.refresh();
            }
            Action::GenerateKey => self.open_key_generator(),
            Action::ImportKey => self.open_key_import(),
            Action::KnownHosts => self.host_trust.open_hosts(),
            Action::About => self.dialog = Some(Dialog::About),
            Action::Docs => context.open_url(egui::OpenUrl::new_tab(
                "https://github.com/libalpm64/RTerm#readme",
            )),
            Action::Issue => context.open_url(egui::OpenUrl::new_tab(
                "https://github.com/libalpm64/RTerm/issues/new",
            )),
            Action::Lock => self.lock(),
            Action::AiSettings => {
                let mut draft = self.settings.clone();
                draft.open_ai_page();
                self.dialog = Some(Dialog::settings(&draft, self.tunnel_profile.as_ref()));
            }
            Action::AiStart => self.start_agent(context),
            Action::AiCopyPrompt => self.copy_agent_prompt(context),
            Action::AiRevoke => self.stop_agent(),
            Action::AiActivity => self.open_agent_activity(None),
            Action::AiTunnelToggle => {
                if self.openai_tunnel.is_some() {
                    self.stop_tunnel();
                } else if let Err(error) = self.connect_tunnel(context) {
                    self.fail(error);
                }
            }
        }
    }

    fn bottom_options(&self) -> crate::terminal::TerminalOptions {
        crate::terminal::TerminalOptions {
            font: self.bottom_font,
            ..self.settings.terminal()
        }
    }

    fn zoom_target(&self) -> Option<u64> {
        if self.bottom_open && self.bottom_focus {
            self.bottom_shell.as_ref().map(|term| term.tab)
        } else {
            self.active
        }
    }

    fn zoom_base(&self, tab: Option<u64>) -> f32 {
        if let Some(term) = self
            .bottom_shell
            .as_ref()
            .filter(|term| Some(term.tab) == tab)
        {
            return term.zoom_base(self.bottom_font);
        }
        let configured = self.settings.terminal().font;
        self.terminals
            .iter()
            .find(|term| Some(term.tab) == tab)
            .map_or(configured, |term| term.zoom_base(configured))
    }

    fn zoom_preview(&self, tab: u64) -> Option<f32> {
        self.pending_wheel_zoom
            .filter(|(target, _, _)| *target == tab)
            .map(|(_, size, _)| size)
    }

    fn zoom(&mut self, tab: Option<u64>, size: f32) {
        if self
            .pending_wheel_zoom
            .is_some_and(|(target, _, _)| Some(target) == tab)
        {
            self.pending_wheel_zoom = None;
        }
        let size = size.round().clamp(8.0, 32.0);
        if let Some(terminal) = self
            .bottom_shell
            .as_mut()
            .filter(|term| Some(term.tab) == tab)
        {
            terminal.manual_zoom();
            if self.bottom_font == size {
                return;
            }
            self.bottom_font = size;
            self.settings.set("bottom_font_size", format!("{size}"));
            if self.persist_history
                && let Err(error) =
                    storage::save_settings(&[("bottom_font_size", format!("{size}"))])
            {
                self.fail(error);
            }
            return;
        }
        if tab.is_some() && !self.terminals.iter().any(|term| Some(term.tab) == tab) {
            return;
        }
        // Main tabs share their saved font, but not with the bottom shell.
        for terminal in &mut self.terminals {
            terminal.manual_zoom();
        }
        if size == self.settings.terminal().font {
            return;
        }
        self.settings.set("font_size", format!("{size}"));
        if self.persist_history
            && let Err(error) = storage::save_settings(&[("font_size", format!("{size}"))])
        {
            self.fail(error);
        }
    }

    fn settle_wheel_zoom(&mut self, context: &egui::Context) {
        let Some((tab, size, deadline)) = self.pending_wheel_zoom else {
            return;
        };
        let (now, ctrl, wheel, interaction) = context.input(|input| (
            input.time,
            input.modifiers.ctrl || input.modifiers.mac_cmd,
            input.events.iter().any(|event| matches!(event, egui::Event::MouseWheel { modifiers, delta, .. } if (modifiers.ctrl || modifiers.mac_cmd) && delta.y != 0.0)),
            input.events.iter().any(|event| match event {
                egui::Event::PointerButton { pressed: true, .. } | egui::Event::Key { pressed: true, .. } | egui::Event::Text(_) | egui::Event::Paste(_) | egui::Event::Ime(_) => true,
                egui::Event::MouseWheel { modifiers, delta, .. } => !(modifiers.ctrl || modifiers.mac_cmd) && *delta != egui::Vec2::ZERO,
                _ => false,
            }),
        ));
        if self.locked || self.dialog.is_some() || self.message.is_some() {
            self.pending_wheel_zoom = None;
            self.wheel_zoom_remainder = 0.0;
            return;
        }
        // Commit only a settled gesture. Intermediate grids can trigger a
        // ConPTY/PSReadLine redraw before the previous resize is processed.
        if interaction || (!wheel && (now >= deadline || !ctrl)) {
            self.zoom(Some(tab), size);
        } else {
            context.request_repaint_after(Duration::from_secs_f64((deadline - now).max(0.0)));
        }
    }

    fn wheel_zoom(&mut self, ui: &egui::Ui, rect: Rect, tab: u64) {
        if self.locked
            || self.dialog.is_some()
            || self.message.is_some()
            || egui::Popup::is_any_open(ui.ctx())
            || !ui.rect_contains_pointer(rect)
            || !ui.memory(|memory| memory.allows_interaction(ui.layer_id()))
        {
            return;
        }
        let options = ui.ctx().options(|options| options.input_options);
        let notches = ui.input_mut(|input| {
            let mut notches = 0.0;
            let mut consumed = egui::Vec2::ZERO;
            let page_height = input.viewport_rect().height();
            input.events.retain(|event| {
                let egui::Event::MouseWheel {
                    unit,
                    delta,
                    modifiers,
                } = event
                else {
                    return true;
                };
                if !(modifiers.ctrl || modifiers.mac_cmd) || delta.y == 0.0 {
                    return true;
                }
                let mut points = match unit {
                    egui::MouseWheelUnit::Line => *delta * options.line_scroll_speed,
                    egui::MouseWheelUnit::Point => *delta,
                    egui::MouseWheelUnit::Page => *delta * page_height,
                };
                notches += match unit {
                    egui::MouseWheelUnit::Line | egui::MouseWheelUnit::Page => delta.y,
                    egui::MouseWheelUnit::Point => delta.y / options.line_scroll_speed.max(1.0),
                };
                // Match egui's raw delta conversion so this wheel event cannot
                // also scroll the terminal or send arrows to a curses program.
                let horizontal = modifiers.matches_any(options.horizontal_scroll_modifier);
                let vertical = modifiers.matches_any(options.vertical_scroll_modifier);
                if horizontal && !vertical {
                    points = vec2(points.x + points.y, 0.0);
                }
                if vertical && !horizontal {
                    points = vec2(0.0, points.x + points.y);
                }
                consumed += points;
                false
            });
            input.raw_scroll_delta -= consumed;
            notches
        });
        if notches != 0.0 {
            if self.wheel_zoom_tab != Some(tab) {
                if let Some((previous, size, _)) = self.pending_wheel_zoom.take() {
                    self.zoom(Some(previous), size);
                }
                self.wheel_zoom_remainder = 0.0;
                self.wheel_zoom_tab = Some(tab);
            }
            self.wheel_zoom_remainder += notches;
            let steps = self.wheel_zoom_remainder.trunc();
            self.wheel_zoom_remainder -= steps;
            if steps != 0.0 {
                let current = self.zoom_base(Some(tab));
                let target = (self.zoom_preview(tab).unwrap_or(current) + steps).clamp(8.0, 32.0);
                const QUIET: f64 = 0.075;
                let now = ui.input(|input| input.time);
                self.pending_wheel_zoom = (target != current).then_some((tab, target, now + QUIET));
                // Repaint the zoom preview immediately, including when the
                // gesture reverses back to the committed font size.
                ui.ctx().request_repaint();
                if self.pending_wheel_zoom.is_some() {
                    ui.ctx()
                        .request_repaint_after(Duration::from_secs_f64(QUIET));
                }
            }
        } else if !ui.input(|input| input.modifiers.ctrl || input.modifiers.mac_cmd) {
            self.wheel_zoom_remainder = 0.0;
        }
    }

    fn request_exit(&mut self, context: &egui::Context) {
        if self.settings.on("confirm_exit")
            && (!self.terminals.is_empty() || self.bottom_shell.is_some())
            && !self.locked
        {
            self.dialog = Some(Dialog::Exit);
        } else {
            self.exit(context);
        }
    }

    pub(crate) fn exit(&mut self, context: &egui::Context) {
        self.stop_agent();
        self.exit_confirmed = true;
        if self.settings.on("clear_on_close") {
            self.history.clear();
            if self.persist_history {
                let _ = storage::save_settings(&[("command_history", "[]".into())]);
            }
        }
        context.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn remember_history(&mut self, commands: Vec<String>) {
        if commands.is_empty() {
            return;
        }
        for command in commands {
            self.history.retain(|old| old != &command);
            self.history.insert(0, command);
        }
        self.history.truncate(50);
        if self.persist_history {
            let _ = storage::save_settings(&[(
                "command_history",
                serde_json::to_string(&self.history).unwrap(),
            )]);
        }
    }

    fn shortcuts(&mut self, context: &egui::Context) {
        // Consume every zoom press, including multiple repeats in one frame.
        // Base the first step on the font actually drawn, not the saved size.
        let zooms = context.input_mut(|input| {
            let mut zooms = Vec::new();
            input.events.retain(|event| {
                if let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = event
                    && (modifiers.ctrl || modifiers.mac_cmd || modifiers.command)
                    && !modifiers.alt
                {
                    match key {
                        Key::Plus | Key::Equals => zooms.push(Some(1.0)),
                        Key::Minus => zooms.push(Some(-1.0)),
                        Key::Num0 => zooms.push(None),
                        _ => return true,
                    }
                    return false;
                }
                true
            });
            zooms
        });
        if !zooms.is_empty() {
            let tab = self.zoom_target();
            let mut size = self.zoom_base(tab);
            for delta in zooms {
                size = delta.map_or(13.0, |delta| (size + delta).clamp(8.0, 32.0));
            }
            self.zoom(tab, size);
        }
        let action = context.input_mut(|input| {
            let command = egui::Modifiers::COMMAND;
            if input.consume_key(egui::Modifiers::ALT | egui::Modifiers::SHIFT, Key::ArrowUp) {
                Some(Action::JumpCommand(true))
            } else if input.consume_key(
                egui::Modifiers::ALT | egui::Modifiers::SHIFT,
                Key::ArrowDown,
            ) {
                Some(Action::JumpCommand(false))
            } else if input.consume_key(command, Key::N) {
                Some(Action::New)
            } else {
                None
            }
        });
        if let Some(action) = action {
            self.run_action(action, context);
        }
    }
}

mod agent_activity;
mod agent_ui;
#[cfg(feature = "benchmark")]
pub(crate) mod benchmark;
mod host_trust;
mod layout;
mod menu;
mod sessions;
mod ssh_reconnect;
pub(crate) use sessions::{header_label, icon_slot, top_band};
