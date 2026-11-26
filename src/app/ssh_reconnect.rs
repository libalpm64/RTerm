use super::*;

pub(super) struct SshTab {
    pub(super) session_id: u32,
    pub(super) key: Option<crate::host_keys::Key>,
    profile: Profile,
    credentials_available: bool,
    connected_once: bool,
    retry_at: Option<Instant>,
    retry_delay: Duration,
}

impl SshTab {
    pub(super) fn new(profile: Profile) -> Self {
        Self {
            session_id: crate::backend::next_session_id(),
            key: None,
            profile,
            credentials_available: true,
            connected_once: false,
            retry_at: None,
            retry_delay: Duration::from_secs(1),
        }
    }

    pub(super) fn connected(&mut self) {
        self.connected_once = true;
        self.retry_at = None;
        self.retry_delay = Duration::from_secs(1);
    }

    fn schedule(&mut self) {
        if self.connected_once && self.retry_at.is_none() {
            self.retry_at = Some(Instant::now() + self.retry_delay);
            self.retry_delay = (self.retry_delay * 2).min(Duration::from_secs(30));
        }
    }

    pub(super) fn connection_failed(&mut self, error: &str) {
        // Rejected credentials require user attention. SSH connection errors
        // are retried for a tab that authenticated successfully before.
        if error.starts_with("SSH connection") || error == "Backend disconnected" {
            self.schedule();
        }
    }
}

impl App {
    pub(crate) fn ssh_transfer_identity(&self, endpoint: u32) -> Option<String> {
        let tab = self
            .terminals
            .iter()
            .find(|term| term.endpoint == Some(endpoint))?
            .tab;
        let state = self.ssh_tabs.get(&tab)?;
        let Connection::Ssh { config, .. } = &state.profile.connection else {
            return None;
        };
        let key = state.key.as_ref()?;
        Some(format!(
            "{}@{} {}",
            config.user,
            crate::host_keys::target(&config.host, config.port),
            key.fingerprint
        ))
    }

    pub(super) fn ssh_agent_id(&self, tab: u64) -> Option<u32> {
        self.ssh_tabs.get(&tab).map(|state| state.session_id)
    }

    pub(super) fn ssh_connection_state(&self, terminal: &Terminal) -> &'static str {
        if self.ssh_retry_pending(terminal.tab) {
            "reconnecting"
        } else if terminal.closed {
            "closed"
        } else if terminal.starting() {
            if self
                .ssh_tabs
                .get(&terminal.tab)
                .is_some_and(|state| state.connected_once)
            {
                "reconnecting"
            } else {
                "connecting"
            }
        } else {
            "connected"
        }
    }

    pub(super) fn open_ssh_profile(&mut self, profile: Profile) -> u64 {
        let existing = self
            .terminals
            .iter()
            .filter(|term| term.kind == Kind::Ssh)
            .filter(|term| {
                self.ssh_tabs.get(&term.tab).is_some_and(|state| {
                    same_profile(&state.profile, &profile, state.credentials_available)
                })
            })
            .min_by_key(|term| term.closed)
            .map(|term| (term.tab, term.closed));
        if let Some((tab, closed)) = existing {
            if closed {
                let state = self.ssh_tabs.get_mut(&tab).unwrap();
                state.profile = profile;
                state.credentials_available = true;
                self.reconnect_ssh_tab(tab);
            }
            self.activate(tab);
            tab
        } else {
            let tab = self.next_tab;
            self.connect(profile, None);
            tab
        }
    }

    pub(super) fn ssh_transport_closed(&mut self, id: u32, message: &str, retry: bool) -> bool {
        let Some(terminal) = self
            .terminals
            .iter_mut()
            .find(|term| term.kind == Kind::Ssh && term.endpoint == Some(id))
        else {
            return false;
        };
        let tab = terminal.tab;
        terminal.mark_closed();
        terminal.endpoint = None;
        terminal.receive(format!("\r\n\x1b[90m[{message}]\x1b[0m\r\n").as_bytes());
        if let Some(session_id) = self.ssh_agent_id(tab) {
            self.agent.revoke_session(session_id);
        }
        drop(self.backend.request(Command::Disconnect(id)));
        if retry && let Some(state) = self.ssh_tabs.get_mut(&tab) {
            state.schedule();
        }
        if self.active == Some(tab) && self.explorer.remote_active {
            self.activate_remote(None);
        }
        true
    }

    pub(super) fn reconnect_ssh_tab(&mut self, tab: u64) {
        if self.locked {
            return;
        }
        let Some(state) = self
            .ssh_tabs
            .get_mut(&tab)
            .filter(|state| state.credentials_available)
        else {
            return;
        };
        let Some(terminal) = self
            .terminals
            .iter_mut()
            .find(|term| term.kind == Kind::Ssh && term.tab == tab)
        else {
            return;
        };
        if terminal.starting() {
            return;
        }
        if let Some(id) = terminal.endpoint {
            self.agent.revoke_session(state.session_id);
            drop(self.backend.request(Command::Disconnect(id)));
        }
        terminal.begin_ssh_reconnect();
        state.retry_at = None;
        let profile = state.profile.clone();
        self.start_remote_connection(tab, profile, None);
    }

    pub(super) fn ssh_retry_pending(&self, tab: u64) -> bool {
        !self.locked
            && self.settings.on("ssh_auto_reconnect")
            && self
                .ssh_tabs
                .get(&tab)
                .is_some_and(|state| state.credentials_available && state.retry_at.is_some())
    }

    pub(super) fn poll_ssh_reconnect(&mut self, context: &egui::Context) {
        if self.locked || !self.settings.on("ssh_auto_reconnect") {
            return;
        }
        let now = Instant::now();
        let due: Vec<_> = self
            .ssh_tabs
            .iter()
            .filter_map(|(&tab, state)| {
                if !state.credentials_available {
                    return None;
                }
                let at = state.retry_at?;
                if at <= now {
                    Some(tab)
                } else {
                    context.request_repaint_after(at - now);
                    None
                }
            })
            .collect();
        for tab in due {
            self.reconnect_ssh_tab(tab);
        }
    }

    pub(super) fn forget_ssh_credentials(&mut self) {
        for state in self.ssh_tabs.values_mut() {
            state.profile = state.profile.without_credentials();
            state.credentials_available = false;
        }
    }

    pub(super) fn restore_ssh_credentials(&mut self) {
        for state in self
            .ssh_tabs
            .values_mut()
            .filter(|state| !state.credentials_available)
        {
            if let Some(profile) = self
                .profiles
                .iter()
                .find(|profile| same_profile(&state.profile, profile, false))
            {
                state.profile = profile.clone();
                state.credentials_available = true;
            }
        }
    }
}

fn same_profile(a: &Profile, b: &Profile, credentials_available: bool) -> bool {
    if a.name != b.name {
        return false;
    }
    let stripped;
    let b = if credentials_available {
        b
    } else {
        stripped = b.without_credentials();
        &stripped
    };
    match (&a.connection, &b.connection) {
        (
            Connection::Ssh {
                config: left,
                terminal: left_term,
            },
            Connection::Ssh {
                config: right,
                terminal: right_term,
            },
        ) => left == right && left_term == right_term,
        _ => false,
    }
}
