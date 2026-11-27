use super::agent_activity::Activity;
use super::*;
use crate::agent::{self, Action as AgentAction, Bridge, Mode, Permit, Request};
use crate::backend::{AgentDisplay, AgentTranscript};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Default)]
pub(super) struct State {
    bridge: Option<Bridge>,
    pending: Option<Request>,
    approvals: VecDeque<Request>,
    saved: BTreeMap<String, (usize, blake3::Hash)>,
    grants: BTreeMap<u32, Instant>,
    tunnel_grants: BTreeMap<u32, Instant>,
    running: BTreeMap<u32, Arc<AtomicBool>>,
    upload_summaries: BTreeMap<u32, crate::agent_upload::ManifestSummary>,
    minutes: u32,
    pub(super) activity: Activity,
    displays: BTreeMap<u64, AgentDisplay>,
}

impl State {
    pub(super) fn is_tunnel_request(&self, request: &Request) -> bool {
        self.bridge
            .as_ref()
            .is_some_and(|bridge| !Arc::ptr_eq(&bridge.alive, &request.active))
    }

    pub(super) fn remove_display(&mut self, tab: u64) {
        self.displays.remove(&tab);
    }

    pub(super) fn running_on(&self, session: Option<u32>) -> bool {
        session.is_some_and(|id| self.running.contains_key(&id))
    }

    fn busy(&self, id: u32) -> bool {
        self.running.contains_key(&id)
            || self
                .pending
                .iter()
                .chain(&self.approvals)
                .any(|request| request.authorized() && request.action.session_id() == Some(id))
    }

    fn finish(&mut self, id: u32, alive: &Arc<AtomicBool>) -> bool {
        let current = self
            .running
            .get(&id)
            .is_some_and(|current| Arc::ptr_eq(current, alive));
        if current {
            self.running.remove(&id);
        }
        current
    }

    fn queue_approval(&mut self, request: Request, context: &egui::Context) {
        request.reply.phase("awaiting_approval");
        if self.pending.is_none() {
            self.minutes = 0;
            self.pending = Some(request);
            context.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Informational,
            ));
        } else {
            self.approvals.push_back(request);
        }
    }

    pub(super) fn blocks_terminal_input(&self) -> bool {
        self.pending.is_some()
    }
    pub(super) fn active(&self) -> bool {
        self.bridge
            .as_ref()
            .is_some_and(|bridge| bridge.alive.load(Ordering::Acquire))
    }
    pub(super) fn stop(&mut self) {
        self.bridge = None;
        self.grants.clear();
        self.tunnel_grants.clear();
        self.saved.clear();
        self.upload_summaries.clear();
        for request in self
            .pending
            .take()
            .into_iter()
            .chain(self.approvals.drain(..))
        {
            let _ = request.reply.send(agent::error("Agent access revoked"));
        }
        for alive in self.running.values() {
            alive.store(false, Ordering::Release);
        }
    }
    pub(super) fn revoke_session(&mut self, id: u32) {
        self.grants.remove(&id);
        self.tunnel_grants.remove(&id);
        self.upload_summaries.remove(&id);
        if self
            .pending
            .as_ref()
            .is_some_and(|request| request.action.session_id() == Some(id))
            && let Some(request) = self.pending.take()
        {
            let _ = request.reply.send(agent::error("SSH session closed"));
        }
        self.approvals.retain(|request| {
            if request.action.session_id() == Some(id) {
                let _ = request
                    .reply
                    .clone()
                    .send(agent::error("SSH session closed"));
                false
            } else {
                true
            }
        });
        if let Some(alive) = self.running.get(&id) {
            alive.store(false, Ordering::Release);
        }
    }
}

// Remote data is displayed as literal text, never terminal instructions.
pub(super) fn literal_text(bytes: &[u8]) -> String {
    crate::agent_upload::literal_text(bytes)
}

impl App {
    fn agent_mode(&self) -> Mode {
        Mode::parse(self.settings.get("ai_access"))
    }

    fn agent_command_timeout(&self) -> u64 {
        self.settings
            .get("ai_command_timeout")
            .parse::<u64>()
            .unwrap_or(60)
            .clamp(15, agent::MAX_COMMAND_SECS)
    }

    pub(super) fn start_agent(&mut self, context: &egui::Context) {
        if self.locked || self.agent_mode() == Mode::Disabled {
            self.run_action(Action::AiSettings, context);
            return;
        }
        let result = self.ensure_agent_started(context).and_then(|started| {
            if started && self.settings.on("ai_tunnel_autoconnect") && self.tunnel_profile.is_some()
            {
                self.connect_tunnel(context)
            } else {
                Ok(())
            }
        });
        if let Err(error) = result {
            self.fail(error);
        }
    }

    fn ensure_agent_started(&mut self, context: &egui::Context) -> Result<bool, String> {
        if self.locked || self.agent_mode() == Mode::Disabled {
            return Err("Unlock the vault and choose an enabled Agent Access mode".into());
        }
        if self.agent.active() {
            return Ok(false);
        }
        self.stop_agent();
        self.agent.bridge = Some(
            Bridge::start(context)
                .map_err(|error| format!("Cannot start AI interface: {error}"))?,
        );
        Ok(true)
    }

    pub(crate) fn start_agent_with_tunnel(
        &mut self,
        context: &egui::Context,
    ) -> Result<(), String> {
        self.ensure_agent_started(context)?;
        self.connect_tunnel(context)
    }

    pub(super) fn copy_agent_prompt(&self, context: &egui::Context) {
        if self.agent.active()
            && let Some(bridge) = &self.agent.bridge
        {
            context.copy_text(bridge.prompt(self.agent_command_timeout()));
        }
    }

    pub(crate) fn stop_agent(&mut self) {
        self.stop_tunnel();
        self.agent.stop();
        self.refresh_agent_activity();
    }

    pub(crate) fn stop_tunnel(&mut self) {
        self.openai_tunnel = None;
        self.agent.tunnel_grants.clear();
    }

    pub(crate) fn connect_tunnel(&mut self, context: &egui::Context) -> Result<(), String> {
        if self.locked || self.vault_key.is_none() || !self.settings.on("vault_enabled") {
            return Err("Unlock the encrypted vault to connect the OpenAI tunnel".into());
        }
        if self.agent_mode() == Mode::Disabled || !self.agent.active() {
            return Err("Start Agent before connecting the OpenAI tunnel".into());
        }
        if self
            .openai_tunnel
            .as_ref()
            .is_some_and(|running| matches!(running.status(), crate::tunnel::Status::Failed(_)))
        {
            self.stop_tunnel();
        }
        if self.openai_tunnel.is_none() {
            let profile = self
                .tunnel_profile
                .clone()
                .ok_or("Save an OpenAI tunnel profile in Advanced AI Settings first")?;
            let bridge = self
                .agent
                .bridge
                .as_ref()
                .ok_or("Start Agent before connecting the OpenAI tunnel")?;
            self.openai_tunnel = Some(crate::tunnel::Running::start(
                profile,
                bridge.mcp_access(),
                context,
            )?);
        }
        Ok(())
    }

    pub(super) fn agent_terminal(&self, session_id: u32) -> Option<&Terminal> {
        self.terminals
            .iter()
            .find(|term| self.ssh_agent_id(term.tab) == Some(session_id))
    }

    fn agent_endpoint(&self, session_id: u32) -> Option<(u32, u64)> {
        let term = self.agent_terminal(session_id)?;
        if term.closed || term.starting() {
            return None;
        }
        term.endpoint.map(|id| (id, term.tab))
    }

    fn agent_session_error(&self, session_id: u32) -> serde_json::Value {
        if let Some(term) = self.agent_terminal(session_id) {
            let state = self.ssh_connection_state(term);
            if matches!(state, "connecting" | "reconnecting") {
                let (code, message) = if state == "reconnecting" {
                    ("session_reconnecting", "SSH session is reconnecting")
                } else {
                    ("session_connecting", "SSH session is connecting")
                };
                return json!({"ok":false,"error":message,"error_code":code,
                    "connection_state":state,"session_id":session_id,"tab_id":term.tab,
                    "operation_started":false,"retry_after_ms":500});
            }
        }
        agent::error("Unknown or disconnected SSH session")
    }

    fn agent_sessions(&self) -> Vec<(u32, String)> {
        self.terminals
            .iter()
            .filter_map(|term| {
                if term.kind == Kind::Ssh && !term.closed && !term.starting() {
                    self.ssh_agent_id(term.tab).map(|id| {
                        (
                            id,
                            term.remote_target.as_ref().map_or_else(
                                || term.name.clone(),
                                |target| format!("{} — {}", term.name, target),
                            ),
                        )
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    // Reuse IDs for unchanged profiles across discoveries within one activation.
    // Only public metadata leaves RTerm; credentials remain in the app.
    fn agent_saved_sessions(&mut self) -> Vec<serde_json::Value> {
        let previous = std::mem::take(&mut self.agent.saved);
        let mut entries = Vec::new();
        for (index, profile) in self.profiles.iter().enumerate() {
            if profile.kind() != Kind::Ssh {
                continue;
            }
            let Ok(bytes) = serde_json::to_vec(profile) else {
                continue;
            };
            let fingerprint = blake3::hash(&bytes);
            let id = previous
                .iter()
                .find(|(_, (old_index, old_hash))| *old_index == index && *old_hash == fingerprint)
                .map(|(id, _)| id.clone())
                .unwrap_or_else(agent::random_id);
            self.agent.saved.insert(id.clone(), (index, fingerprint));
            entries.push(json!({"profile_id":id,"name":profile.name,"target":profile.subtitle()}));
        }
        entries
    }

    fn agent_saved_profile(&self, id: &str) -> Option<&Profile> {
        let (index, fingerprint) = self.agent.saved.get(id)?;
        let profile = self.profiles.get(*index)?;
        if profile.kind() != Kind::Ssh {
            return None;
        }
        let bytes = serde_json::to_vec(profile).ok()?;
        (blake3::hash(&bytes) == *fingerprint).then_some(profile)
    }

    pub(super) fn poll_agent(&mut self, context: &egui::Context) {
        self.refresh_agent_activity();
        // Reuse the normal file-transfer overlay; never replace the terminal.
        for (tab, display) in &self.agent.displays {
            let display = display.lock().unwrap();
            if display.status.is_some() {
                continue;
            }
            let percent = display.progress.map_or(0.0, |(sent, total)| {
                if total == 0 {
                    0.0
                } else {
                    100.0 * sent as f32 / total as f32
                }
            });
            self.transfers.insert(
                format!("agent-upload-{tab}"),
                Transfer {
                    text: format!("AI SFTP upload — {:.0}%", percent),
                    percent,
                    expires: None,
                    error: false,
                },
            );
        }
        let mode = self.agent_mode();
        if self.locked || mode == Mode::Disabled {
            self.stop_agent();
            return;
        }
        if !self.agent.active() {
            self.stop_tunnel();
            return;
        }
        let sessions = self.agent_sessions();
        let now = Instant::now();
        self.agent
            .grants
            .retain(|id, expires| *expires > now && sessions.iter().any(|(live, _)| live == id));
        self.agent
            .tunnel_grants
            .retain(|id, expires| *expires > now && sessions.iter().any(|(live, _)| live == id));
        let disconnected: Vec<_> = self
            .agent
            .running
            .keys()
            .copied()
            .filter(|id| !sessions.iter().any(|(live, _)| live == id))
            .collect();
        for id in disconnected {
            self.agent.revoke_session(id);
        }
        loop {
            if self.agent.pending.is_none() {
                self.agent.pending = self.agent.approvals.pop_front();
                self.agent.minutes = 0;
            }
            let Some(request) = &self.agent.pending else {
                break;
            };
            let valid = request.authorized()
                && request.deadline > now
                && match &request.action {
                    AgentAction::Execute { session_id, .. } => {
                        sessions.iter().any(|(id, _)| id == session_id)
                    }
                    AgentAction::Upload { session_id, .. } => {
                        self.settings.on("ai_upload")
                            && sessions.iter().any(|(id, _)| id == session_id)
                    }
                    AgentAction::OpenSession { profile_id } => {
                        self.agent_saved_profile(profile_id).is_some()
                    }
                    AgentAction::ListSessions => false,
                };
            if valid {
                if self.settings.on("ai_interactive")
                    && matches!(request.action, AgentAction::Execute { .. })
                    && let Some(id) = request.action.session_id()
                    && let Some((_, tab)) = self.agent_endpoint(id)
                    && self.active != Some(tab)
                {
                    self.activate(tab);
                }
                break;
            }
            let request = self.agent.pending.take().unwrap();
            if let Some(id) = request.action.session_id() {
                self.agent.upload_summaries.remove(&id);
            }
            let _ = request.reply.send(agent::error(
                "Permission request expired or SSH session closed",
            ));
        }
        for index in 0..16 {
            let next = self
                .agent
                .bridge
                .as_ref()
                .and_then(|bridge| bridge.requests.try_recv().ok());
            let Some(request) = next else {
                break;
            };
            if index == 15 {
                context.request_repaint();
            }
            {
                self.track_agent_request(&request, request.active.clone());
                if !request.authorized() || request.deadline <= now {
                    let _ = request.reply.send(agent::error("Request expired"));
                } else if let Some(id) = request.action.session_id()
                    && self.agent_endpoint(id).is_none()
                {
                    let _ = request.reply.send(self.agent_session_error(id));
                } else if request
                    .action
                    .session_id()
                    .is_some_and(|id| self.agent.busy(id))
                {
                    let _ = request.reply.send(agent::error(
                        "Another operation is active on this SSH session; poll or cancel it first",
                    ));
                } else {
                    match &request.action {
                        AgentAction::ListSessions => {
                            let entries: Vec<_> = sessions
                                .iter()
                                .map(|(id, name)| json!({"session_id": id, "name": name}))
                                .collect();
                            let saved = self.agent_saved_sessions();
                            let background = !self.settings.on("ai_interactive");
                            let tabs: Vec<_> = self.terminals.iter().filter(|term| term.kind == Kind::Ssh).map(|term| {
                                let state = self.ssh_connection_state(term);
                                // This legacy field describes local terminal checks,
                                // not whether the remote shell is actually at a prompt.
                                let auto_execute = mode == Mode::Unrestricted && !background && term.agent_prompt_available();
                                json!({"tab_id":term.tab,"name":term.name,"target":term.remote_target,"state":state,"shell_confirmed":auto_execute,
                                    "session_id":self.ssh_agent_id(term.tab)})
                            }).collect();
                            let active = self
                                .active
                                .and_then(|tab| self.ssh_agent_id(tab))
                                .filter(|id| sessions.iter().any(|(live, _)| live == id));
                            let execution_mode = if background {
                                "background_exec"
                            } else {
                                "interactive_shell"
                            };
                            let _ = request.reply.send(json!({"ok": true, "sessions": entries, "saved_sessions":saved,"tabs":tabs,"active_session_id":active,"execution_mode":execution_mode}));
                        }
                        AgentAction::OpenSession { profile_id } => {
                            if self.agent_saved_profile(profile_id).is_none() {
                                let _ = request.reply.send(agent::error(
                                    "Unknown or changed saved SSH profile; list sessions again",
                                ));
                            } else if self.settings.on("ai_auto_open") {
                                // Use the same validated, cancellation-safe open path
                                // as a manual approval. No command grant is created.
                                self.complete_agent_permission(request, true);
                            } else {
                                self.agent.queue_approval(request, context);
                            }
                        }
                        AgentAction::Upload { .. } => {
                            if !self.settings.on("ai_upload") {
                                let _ = request.reply.send(agent::error(
                                    "AI file uploads are disabled in AI Settings",
                                ));
                            } else {
                                let AgentAction::Upload {
                                    session_id,
                                    local,
                                    remote,
                                } = &request.action
                                else {
                                    unreachable!()
                                };
                                let session_id = *session_id;
                                let permit = Permit {
                                    bridge: request.active.clone(),
                                    request: request.alive.clone(),
                                    deadline: request.deadline,
                                };
                                if !permit.valid() {
                                    let _ = request.reply.send(agent::error(
                                        "Upload inspection expired or was cancelled",
                                    ));
                                } else {
                                    request.reply.phase("preparing_upload");
                                    let alive = request.alive.clone();
                                    self.agent.running.insert(session_id, alive.clone());
                                    let context = context.clone();
                                    self.request(
                                        Command::AgentUploadInspect {
                                            local: local.clone(),
                                            remote: remote.clone(),
                                            permit,
                                        },
                                        move |app, result| {
                                            let current = app.agent.finish(session_id, &alive);
                                            if !current
                                                || !request.authorized()
                                                || request.deadline <= Instant::now()
                                                || !app.agent.active()
                                                || app.locked
                                                || !app.settings.on("ai_upload")
                                                || app.agent_endpoint(session_id).is_none()
                                            {
                                                let _ = request.reply.send(agent::error(
                                                    "Upload inspection expired or SSH access changed",
                                                ));
                                                return;
                                            }
                                            match result {
                                                Ok(BackendReply::AgentUploadSummary(summary)) => {
                                                    app.agent.upload_summaries.insert(session_id, summary);
                                                    app.agent.queue_approval(request, &context);
                                                }
                                                Err(error) => {
                                                    let _ = request.reply.send(agent::error(
                                                        format!("Upload inspection failed: {error}"),
                                                    ));
                                                }
                                                _ => {
                                                    let _ = request.reply.send(agent::error(
                                                        "Unexpected upload inspection result",
                                                    ));
                                                }
                                            }
                                        },
                                    );
                                }
                            }
                        }
                        AgentAction::Execute { session_id, .. } => {
                            let background = !self.settings.on("ai_interactive");
                            let foreground_ready = background
                                || self
                                    .agent_terminal(*session_id)
                                    .is_some_and(Terminal::agent_prompt_available);
                            let grant = if background && mode == Mode::Ask {
                                let grants = if self.agent.is_tunnel_request(&request) {
                                    &self.agent.tunnel_grants
                                } else {
                                    &self.agent.grants
                                };
                                grants.get(session_id).copied()
                            } else {
                                None
                            };
                            if !background
                                && self.agent.pending.is_none()
                                && let Some((_, tab)) = self.agent_endpoint(*session_id)
                            {
                                self.activate(tab);
                            }
                            // Unrestricted background exec is safe to dispatch
                            // automatically. Foreground commands still require
                            // confirmation even when local checks report a prompt;
                            // those checks cannot prove the remote shell is idle.
                            // If local checks fail, reject rather than queue an
                            // approval that the UI cannot safely accept yet.
                            if !foreground_ready {
                                let _ = request.reply.send(agent::error(
                                    "Interactive shell unavailable. Return to an empty prompt or use background execution.",
                                ));
                            } else if (mode == Mode::Unrestricted && background) || grant.is_some()
                            {
                                self.dispatch_agent(request, grant);
                            } else {
                                self.agent.queue_approval(request, context);
                            }
                        }
                    }
                }
            }
        }
        self.refresh_agent_activity();
        if self.agent.pending.is_some()
            || !self.agent.approvals.is_empty()
            || !self.agent.running.is_empty()
            || !self.agent.grants.is_empty()
            || !self.agent.tunnel_grants.is_empty()
        {
            context.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn dispatch_agent(&mut self, request: Request, grant: Option<Instant>) {
        let now = Instant::now();
        let authorized = request.authorized();
        let AgentAction::Execute {
            session_id,
            command,
        } = request.action
        else {
            return;
        };
        if self.locked
            || self.agent_mode() == Mode::Disabled
            || !self.agent.active()
            || !authorized
            || request.deadline <= now
        {
            let _ = request
                .reply
                .send(agent::error("Agent permission expired or session closed"));
            return;
        }
        let Some((endpoint, tab)) = self.agent_endpoint(session_id) else {
            let _ = request.reply.send(self.agent_session_error(session_id));
            return;
        };
        let background = !self.settings.on("ai_interactive");
        if !background {
            if let Err(error) = crate::backend::validate_foreground_command(&command) {
                let _ = request.reply.send(agent::error(error));
                return;
            }
            if !self
                .agent_terminal(session_id)
                .is_some_and(Terminal::agent_prompt_available)
            {
                let _ = request.reply.send(agent::error("Interactive shell unavailable. Return to an empty prompt or use background execution."));
                return;
            }
            self.activate(tab);
        }
        let seconds = request
            .timeout_secs
            .unwrap_or(self.agent_command_timeout())
            .min(self.agent_command_timeout());
        let deadline = now + Duration::from_secs(seconds);
        let permit = Permit {
            bridge: request.active.clone(),
            request: request.alive.clone(),
            deadline: grant.map_or(deadline, |end| end.min(deadline)),
        };
        if !permit.valid() {
            let _ = request.reply.send(agent::error("Agent grant expired"));
            return;
        }
        if !request.reply.start(permit.deadline) {
            let _ = request.reply.send(agent::error(
                "Operation cancelled or expired before execution",
            ));
            return;
        }
        let result_reply = request.reply.clone();
        let alive = request.alive;
        if self.settings.on("ai_audit_vault") && self.vault_key.is_some() {
            self.agent.activity.retain_command(&result_reply);
            self.refresh_agent_activity();
        }
        self.agent.running.insert(session_id, alive.clone());
        let backend_command = if background {
            Command::AgentExec {
                id: endpoint,
                command,
                permit,
                result_reply,
            }
        } else {
            Command::AgentShell {
                id: endpoint,
                command,
                permit,
                result_reply,
            }
        };
        self.request(backend_command, move |app, result| {
            let current = app.agent.finish(session_id, &alive);
            let value = match result {
                Ok(BackendReply::Agent(value)) => value,
                Err(error) => agent::error(error),
                _ => agent::error("Unexpected SSH result"),
            };
            if current
                && !background
                && request.active.load(Ordering::Acquire)
                && value["shell_recovery_required"] == true
                && app.agent_endpoint(session_id) == Some((endpoint, tab))
            {
                app.reconnect_ssh_tab(tab);
            }
            let _ = request.reply.send(value);
        });
    }

    fn dispatch_agent_upload(&mut self, request: Request) {
        let authorized = request.authorized();
        let AgentAction::Upload {
            session_id,
            local,
            remote,
        } = request.action
        else {
            return;
        };
        let approved = self.agent.upload_summaries.remove(&session_id);
        let now = Instant::now();
        if self.locked
            || !self.agent.active()
            || self.agent_mode() == Mode::Disabled
            || !self.settings.on("ai_upload")
            || !authorized
            || request.deadline <= now
            || crate::agent_upload::validate_paths(&local, &remote).is_err()
        {
            let _ = request.reply.send(agent::error(
                "Upload disabled, expired or SSH session closed",
            ));
            return;
        }
        let Some((endpoint, tab)) = self.agent_endpoint(session_id) else {
            let _ = request.reply.send(self.agent_session_error(session_id));
            return;
        };
        let Some(approved) = approved else {
            let _ = request.reply.send(agent::error(
                "Approved upload manifest is unavailable; request a new inspection",
            ));
            return;
        };
        let deadline = now
            + Duration::from_secs(
                request
                    .timeout_secs
                    .unwrap_or(self.agent_command_timeout())
                    .min(self.agent_command_timeout()),
            );
        let permit = Permit {
            bridge: request.active.clone(),
            request: request.alive.clone(),
            deadline,
        };
        if !permit.valid() || !request.reply.start(deadline) {
            let _ = request
                .reply
                .send(agent::error("Upload cancelled or expired"));
            return;
        }
        let alive = request.alive;
        self.agent.running.insert(session_id, alive.clone());
        let display = Arc::new(Mutex::new(AgentTranscript::default()));
        // Transfers always remain visible, independently of background commands.
        self.agent.displays.insert(tab, display.clone());
        self.activate(tab);
        self.request(
            Command::AgentUpload {
                id: endpoint,
                local,
                remote,
                permit,
                approved,
                result_reply: request.reply.clone(),
                display: display.clone(),
            },
            move |app, result| {
                app.agent.finish(session_id, &alive);
                let value = match result {
                    Ok(BackendReply::Agent(value)) => value,
                    Err(error) => agent::error(error),
                    _ => agent::error("Unexpected SFTP result"),
                };
                let status = if value["ok"] == true {
                    format!("Upload complete — {} bytes", value["bytes_uploaded"])
                } else {
                    format!(
                        "Upload failed / cancelled — {}",
                        literal_text(
                            value["error"]
                                .as_str()
                                .unwrap_or("Unknown error")
                                .as_bytes()
                        )
                    )
                };
                display.lock().unwrap().status = Some(status);
                app.transfers.insert(
                    format!("agent-upload-{tab}"),
                    Transfer {
                        text: if value["ok"] == true {
                            "AI SFTP upload complete".into()
                        } else {
                            "AI SFTP upload failed".into()
                        },
                        percent: if value["ok"] == true { 100.0 } else { 0.0 },
                        expires: Some(Instant::now() + Duration::from_secs(4)),
                        error: value["ok"] != true,
                    },
                );
                let _ = request.reply.send(value);
            },
        );
    }

    pub(super) fn show_agent_dialog(&mut self, context: &egui::Context) {
        if self.locked || self.dialog.is_some() || self.message.is_some() {
            return;
        }
        let t = self.theme;
        let frame = egui::Frame::new()
            .fill(t.panel)
            .stroke(Stroke::new(1.0_f32, t.outline))
            .inner_margin(16);
        let Some(request) = &self.agent.pending else {
            return;
        };
        let tunneled = self.agent.is_tunnel_request(request);
        if let AgentAction::OpenSession { profile_id } = &request.action {
            let name = self
                .agent_saved_profile(profile_id)
                .map(|p| format!("{} — {}", p.name, p.subtitle()))
                .unwrap_or_else(|| "Profile changed or removed".into());
            let remaining = request
                .deadline
                .saturating_duration_since(Instant::now())
                .as_secs();
            let response = egui::Modal::new(egui::Id::new("agent-open-approval")).frame(frame).show(context, |ui| {
                ui.set_width(460.0);
                ui.heading("AI agent requests a new SSH tab");
                if tunneled { ui.label("Requested through OpenAI Secure MCP Tunnel."); }
                ui.label(name);
                ui.label("Open this saved connection in RTerm? This does not grant command execution permission.");
                ui.label(format!("Request expires in {remaining}s"));
                ui.horizontal(|ui| {
                    if ui.button("Deny").clicked() { return Some(false); }
                    if ui.button("Open session").clicked() { return Some(true); }
                    None
                }).inner
            });
            if let Some(allow) = response
                .inner
                .or_else(|| response.should_close().then_some(false))
            {
                self.resolve_agent_permission(allow);
                self.focus_terminal_after_dialog();
            }
            return;
        }
        if let AgentAction::Upload {
            session_id,
            local,
            remote,
        } = &request.action
        {
            let summary = self.agent.upload_summaries.get(session_id).cloned();
            let name = self
                .agent_sessions()
                .into_iter()
                .find(|(id, _)| id == session_id)
                .map(|(_, name)| name)
                .unwrap_or_else(|| "Disconnected".into());
            let remaining = request
                .deadline
                .saturating_duration_since(Instant::now())
                .as_secs();
            let response = egui::Modal::new(egui::Id::new("agent-upload-approval")).frame(frame).show(context, |ui| {
                ui.set_width(500.0);
                ui.heading("AI agent requests a file upload");
                if tunneled { ui.label("Requested through OpenAI Secure MCP Tunnel."); }
                ui.label(format!("Server: {}", literal_text(name.as_bytes())));
                ui.colored_label(t.yellow, "Only approve files you intend to share with this server.");
                egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                    ui.label("Local source:");
                    ui.monospace(literal_text(local.as_bytes()));
                    ui.label("Remote destination:");
                    ui.monospace(literal_text(remote.as_bytes()));
                });
                if let Some(summary) = &summary {
                    ui.label(format!(
                        "Manifest: {} files, {} directories, {} bytes total",
                        summary.file_count, summary.directory_count, summary.total_bytes
                    ));
                    if summary.total_bytes >= 1_000_000_000 {
                        ui.colored_label(t.yellow, "Large transfer (1 GB or more). Verify remote storage and bandwidth before approving.");
                    }
                } else {
                    ui.colored_label(t.red, "Upload manifest unavailable. Deny this request and retry.");
                }
                ui.label("Includes hidden files. No overwrite or directory merging. The source manifest is checked again before transfer. Each file is staged remotely and published only when complete. A cancelled multi-file upload may leave completed files, empty directories or hidden staging files.");
                ui.label(format!("Request expires in {remaining}s"));
                ui.horizontal(|ui| {
                    if ui.button("Deny").clicked() { return Some(false); }
                    let label = if summary.as_ref().is_some_and(|summary| summary.total_bytes >= 1_000_000_000) { "Upload large transfer" } else { "Upload" };
                    if ui.add_enabled(summary.is_some(), egui::Button::new(label)).clicked() { return Some(true); }
                    None
                }).inner
            });
            if let Some(allow) = response
                .inner
                .or_else(|| response.should_close().then_some(false))
            {
                self.resolve_agent_permission(allow);
                self.focus_terminal_after_dialog();
            }
            return;
        }
        let AgentAction::Execute {
            session_id,
            command,
        } = &request.action
        else {
            return;
        };
        let id = *session_id;
        let name = self
            .agent_sessions()
            .into_iter()
            .find(|(live, _)| *live == id)
            .map(|(_, name)| name)
            .unwrap_or_else(|| "Disconnected".into());
        let remaining = request
            .deadline
            .saturating_duration_since(Instant::now())
            .as_secs();
        let timeout = request
            .timeout_secs
            .unwrap_or(self.agent_command_timeout())
            .min(self.agent_command_timeout());
        // Escape control and non-ASCII characters to prevent misleading command previews.
        let preview: String = command.chars().flat_map(char::escape_default).collect();
        let background = !self.settings.on("ai_interactive");
        let ready = self
            .agent_terminal(id)
            .is_some_and(|term| Some(term.tab) == self.active && term.agent_prompt_available());
        let response = egui::Modal::new(egui::Id::new("agent-command-approval")).frame(frame).show(context, |ui| {
            ui.set_width(510.0);
            ui.heading("AI agent requests SSH access");
            if tunneled { ui.label("Requested through OpenAI Secure MCP Tunnel."); }
            ui.label(format!("Session: {name} (ID {id})"));
            ui.label(format!("Command timeout: {timeout}s (also limited by your access grant)"));
            ui.colored_label(t.yellow, "Only allow commands you trust. Access uses your existing SSH account.");
            ui.label("Requested command (escaped):");
            egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| { ui.add(egui::Label::new(egui::RichText::new(&preview).monospace()).wrap()); });
            ui.separator();
            if background {
                ui.label("Allow access for:");
                for (minutes, label) in [(0, "This command only"), (5, "5 minutes"), (15, "15 minutes"), (30, "30 minutes")] {
                    ui.radio_value(&mut self.agent.minutes, minutes, label);
                }
                ui.label("Timed access allows further background commands on this session without prompting until expiry. Revoke at any time from the AI menu.");
            } else {
                ui.label("Runs in the actual SSH shell, using its current directory and environment. Confirm that it is at an empty POSIX shell prompt (sh/bash/zsh), not a program or password prompt.");
                if !ready {
                    ui.colored_label(t.yellow, "Terminal has pending input, is not selected, or is in a full-screen program. Deny and return to the shell first.");
                }
            }
            ui.label(format!("Request expires in {remaining}s"));
            ui.horizontal(|ui| {
                if ui.button("Deny").clicked() { return Some(false); }
                if ui.add_enabled(background || ready, egui::Button::new(if background { "Allow" } else { "Approve and Run" })).clicked() { return Some(true); }
                None
            }).inner
        });
        if let Some(allow) = response
            .inner
            .or_else(|| response.should_close().then_some(false))
        {
            self.resolve_agent_permission(allow);
            self.focus_terminal_after_dialog();
        }
    }

    fn resolve_agent_permission(&mut self, allow: bool) {
        let Some(request) = self.agent.pending.take() else {
            return;
        };
        self.complete_agent_permission(request, allow);
    }

    fn complete_agent_permission(&mut self, request: Request, allow: bool) {
        if matches!(&request.action, AgentAction::Upload { .. }) {
            if allow {
                self.dispatch_agent_upload(request);
            } else {
                if let AgentAction::Upload { session_id, .. } = &request.action {
                    self.agent.upload_summaries.remove(session_id);
                }
                let _ = request.reply.send(agent::error("User denied upload"));
            }
            return;
        }
        if let AgentAction::OpenSession { profile_id } = &request.action {
            if allow
                && !self.locked
                && self.agent.active()
                && self.agent_mode() != Mode::Disabled
                && request.authorized()
                && request.deadline > Instant::now()
                && let Some(profile) = self.agent_saved_profile(profile_id).cloned()
            {
                request.reply.commit(&request.active, || {
                    let tab = self.open_ssh_profile(profile);
                    let state = self.terminals.iter().find(|term| term.tab == tab)
                        .map(|term| self.ssh_connection_state(term));
                    json!({"ok":true,"tab_id":tab,"connection_state":state,"session_id":self.ssh_agent_id(tab)})
                });
            } else {
                let _ = request.reply.send(agent::error(
                    "User denied access, profile changed, or request expired",
                ));
            }
            return;
        }
        let AgentAction::Execute { session_id: id, .. } = &request.action else {
            return;
        };
        let id = *id;
        if !allow {
            let _ = request.reply.send(agent::error("User denied command"));
        } else if request.authorized()
            && request.deadline > Instant::now()
            && self.agent.active()
            && (self.agent_mode() == Mode::Ask
                || (self.agent_mode() == Mode::Unrestricted && self.settings.on("ai_interactive")))
            && self.agent_endpoint(id).is_some()
        {
            let grant = (self.agent_mode() == Mode::Ask
                && !self.settings.on("ai_interactive")
                && matches!(self.agent.minutes, 5 | 15 | 30))
            .then(|| Instant::now() + Duration::from_secs(u64::from(self.agent.minutes) * 60));
            if let Some(deadline) = grant {
                let grants = if self.agent.is_tunnel_request(&request) {
                    &mut self.agent.tunnel_grants
                } else {
                    &mut self.agent.grants
                };
                grants.insert(id, deadline);
            }
            self.dispatch_agent(request, grant);
        } else {
            let _ = request.reply.send(agent::error(
                "Command approval expired or SSH access changed",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::{Channel, ChannelId, server};
    use std::sync::atomic::AtomicUsize;

    const PAGER_PROBE: &str = "test ! -t 0 && test ! -t 1 && test ! -t 2 || exit 99\n\
        test \"$PAGER/$GIT_PAGER/$SYSTEMD_PAGER/$MANPAGER\" = cat/cat/cat/cat || exit 98\n\
        seq 1 100 | less || exit 97\n\
        printf 'pager fixture stderr\\n' >&2\n\
        exit 17";

    type FixtureFiles = Arc<Mutex<BTreeMap<String, Vec<u8>>>>;

    struct TestSftp(FixtureFiles);
    impl russh_sftp::server::Handler for TestSftp {
        type Error = russh_sftp::protocol::StatusCode;
        fn unimplemented(&self) -> Self::Error {
            Self::Error::OpUnsupported
        }
        async fn mkdir(
            &mut self,
            id: u32,
            path: String,
            _: russh_sftp::protocol::FileAttributes,
        ) -> Result<russh_sftp::protocol::Status, Self::Error> {
            let mut files = self.0.lock().unwrap();
            if files.contains_key(&path) || files.contains_key(&format!("{path}/")) {
                return Err(Self::Error::Failure);
            }
            files.insert(format!("{path}/"), Vec::new());
            Ok(russh_sftp::protocol::Status {
                id,
                status_code: Self::Error::Ok,
                error_message: "OK".into(),
                language_tag: "en".into(),
            })
        }
        async fn open(
            &mut self,
            id: u32,
            filename: String,
            flags: russh_sftp::protocol::OpenFlags,
            _: russh_sftp::protocol::FileAttributes,
        ) -> Result<russh_sftp::protocol::Handle, Self::Error> {
            use russh_sftp::protocol::OpenFlags;
            assert!(flags.contains(OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::EXCLUDE));
            assert!(!flags.contains(OpenFlags::TRUNCATE));
            let mut files = self.0.lock().unwrap();
            if files.contains_key(&filename) || files.contains_key(&format!("{filename}/")) {
                return Err(Self::Error::Failure);
            }
            files.insert(filename.clone(), Vec::new());
            Ok(russh_sftp::protocol::Handle {
                id,
                handle: filename,
            })
        }
        async fn write(
            &mut self,
            id: u32,
            handle: String,
            offset: u64,
            data: Vec<u8>,
        ) -> Result<russh_sftp::protocol::Status, Self::Error> {
            let mut files = self.0.lock().unwrap();
            let bytes = files.get_mut(&handle).ok_or(Self::Error::Failure)?;
            assert_eq!(offset, bytes.len() as u64);
            bytes.extend_from_slice(&data);
            Ok(russh_sftp::protocol::Status {
                id,
                status_code: Self::Error::Ok,
                error_message: "OK".into(),
                language_tag: "en".into(),
            })
        }
        async fn close(
            &mut self,
            id: u32,
            _: String,
        ) -> Result<russh_sftp::protocol::Status, Self::Error> {
            Ok(russh_sftp::protocol::Status {
                id,
                status_code: Self::Error::Ok,
                error_message: "OK".into(),
                language_tag: "en".into(),
            })
        }
        async fn rename(
            &mut self,
            id: u32,
            old: String,
            new: String,
        ) -> Result<russh_sftp::protocol::Status, Self::Error> {
            let mut files = self.0.lock().unwrap();
            if files.contains_key(&new) || files.contains_key(&format!("{new}/")) {
                return Err(Self::Error::Failure);
            }
            let bytes = files.remove(&old).ok_or(Self::Error::Failure)?;
            files.insert(new, bytes);
            Ok(russh_sftp::protocol::Status {
                id,
                status_code: Self::Error::Ok,
                error_message: "OK".into(),
                language_tag: "en".into(),
            })
        }
        async fn remove(
            &mut self,
            id: u32,
            path: String,
        ) -> Result<russh_sftp::protocol::Status, Self::Error> {
            self.0
                .lock()
                .unwrap()
                .remove(&path)
                .ok_or(Self::Error::Failure)?;
            Ok(russh_sftp::protocol::Status {
                id,
                status_code: Self::Error::Ok,
                error_message: "OK".into(),
                language_tag: "en".into(),
            })
        }
    }
    struct TestServer(
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        FixtureFiles,
        std::collections::HashMap<ChannelId, Channel<server::Msg>>,
        Option<ChannelId>,
        Option<String>,
    );
    impl server::Handler for TestServer {
        type Error = russh::Error;
        async fn auth_password(&mut self, _: &str, _: &str) -> Result<server::Auth, Self::Error> {
            Ok(server::Auth::Accept)
        }
        async fn channel_open_session(
            &mut self,
            channel: Channel<server::Msg>,
            reply: server::ChannelOpenHandle,
            _: &mut server::Session,
        ) -> Result<(), Self::Error> {
            self.3.insert(channel.id(), channel);
            reply.accept().await;
            Ok(())
        }
        async fn subsystem_request(
            &mut self,
            id: ChannelId,
            name: &str,
            session: &mut server::Session,
        ) -> Result<(), Self::Error> {
            if name == "sftp" {
                let channel = self.3.remove(&id).unwrap();
                session.channel_success(id)?;
                russh_sftp::server::run(channel.into_stream(), TestSftp(self.2.clone())).await;
            } else {
                session.channel_failure(id)?;
            }
            Ok(())
        }
        async fn pty_request(
            &mut self,
            channel: ChannelId,
            _: &str,
            _: u32,
            _: u32,
            _: u32,
            _: u32,
            _: &[(russh::Pty, u32)],
            session: &mut server::Session,
        ) -> Result<(), Self::Error> {
            self.3.remove(&channel);
            session.channel_success(channel)
        }
        async fn shell_request(
            &mut self,
            channel: ChannelId,
            session: &mut server::Session,
        ) -> Result<(), Self::Error> {
            self.3.remove(&channel);
            self.4 = Some(channel);
            session.channel_success(channel)?;
            session.data(channel, bytes::Bytes::from_static(b"fixture:/home/test$ "))
        }
        async fn data(
            &mut self,
            channel: ChannelId,
            data: &[u8],
            session: &mut server::Session,
        ) -> Result<(), Self::Error> {
            if self.4 != Some(channel) {
                return Ok(());
            }
            if data == b"disconnect-fixture\n" {
                return session.close(channel);
            }
            if data == b"exit-fixture\n" {
                session.eof(channel)?;
                session.exit_status_request(channel, 0)?;
                return session.close(channel);
            }
            if data.starts_with(b"\x1b[") && data.ends_with(b"R") {
                if let Some(nonce) = self.5.take() {
                    session.data(
                        channel,
                        bytes::Bytes::from_static(b"\r\nforeground result\r\n"),
                    )?;
                    session.data(
                        channel,
                        bytes::Bytes::from(format!("\x1b]777;rt-{nonce};E;7\x07")),
                    )?;
                    session.data(
                        channel,
                        bytes::Bytes::from_static(b"fixture:/home/test$ \x1b[6n"),
                    )?;
                }
                return Ok(());
            }
            // Emulate a POSIX prompt on the existing PTY, without executing
            // any real command on the test machine.
            let text = String::from_utf8_lossy(data);
            if let Some((_, rest)) = text.split_once("]777;rt-")
                && let Some((nonce, _)) = rest.split_once(";B")
            {
                assert!(text.contains("eval"));
                assert_eq!(nonce.len(), 32);
                session.data(channel, bytes::Bytes::copy_from_slice(data))?;
                let begin = format!("\x1b]777;rt-{nonce};B\x07");
                let end = format!("\x1b]777;rt-{nonce};E;7\x07");
                // Fragment markers across SSH packets.
                session.data(
                    channel,
                    bytes::Bytes::copy_from_slice(&begin.as_bytes()[..12]),
                )?;
                session.data(
                    channel,
                    bytes::Bytes::copy_from_slice(&begin.as_bytes()[12..]),
                )?;
                if text.contains("cursor-query-fixture") {
                    self.5 = Some(nonce.to_owned());
                    session.data(channel, bytes::Bytes::from_static(b"\x1b[6n"))?;
                    return Ok(());
                }
                if text.contains("pager-fixture") {
                    session.data(channel, bytes::Bytes::from_static(b"\x1b[?1049h(END)"))?;
                    return Ok(());
                }
                session.data(
                    channel,
                    bytes::Bytes::from_static(b"\r\nforeground result\r\n"),
                )?;
                session.data(
                    channel,
                    bytes::Bytes::copy_from_slice(&end.as_bytes()[..10]),
                )?;
                session.data(
                    channel,
                    bytes::Bytes::copy_from_slice(&end.as_bytes()[10..]),
                )?;
                session.data(channel, bytes::Bytes::from_static(b"fixture:/home/test$ "))?;
            } else {
                session.data(channel, bytes::Bytes::copy_from_slice(data))?;
            }
            Ok(())
        }
        async fn exec_request(
            &mut self,
            channel: ChannelId,
            data: &[u8],
            session: &mut server::Session,
        ) -> Result<(), Self::Error> {
            let body = data
                .strip_prefix(crate::backend::AGENT_PAGER_ENV.as_bytes())
                .expect("agent exec must disable pagers");
            assert!(
                self.3.remove(&channel).is_some(),
                "exec must not request a PTY"
            );
            self.0.fetch_add(1, Ordering::Relaxed);
            session.channel_success(channel)?;
            // A manual run can provide a POSIX shell with less installed.
            // Run only the hardcoded probe, never arbitrary fixture commands.
            if body == PAGER_PROBE.as_bytes()
                && let Some(shell) = std::env::var_os("RTERM_TEST_PAGER_SHELL")
            {
                let script = format!("{}{PAGER_PROBE}", crate::backend::AGENT_PAGER_ENV);
                let output = tokio::time::timeout(
                    Duration::from_secs(3),
                    tokio::process::Command::new(shell)
                        .arg("-c")
                        .arg(script)
                        .stdin(std::process::Stdio::null())
                        .kill_on_drop(true)
                        .output(),
                )
                .await
                .expect("less must exit without a PTY")
                .expect("run the local pager probe");
                session.data(channel, bytes::Bytes::from(output.stdout))?;
                session.extended_data(channel, 1, bytes::Bytes::from(output.stderr))?;
                session.exit_status_request(channel, output.status.code().unwrap() as u32)?;
                session.eof(channel)?;
                return session.close(channel);
            }
            session.data(channel, bytes::Bytes::copy_from_slice(body))?;
            if body == b"wait" {
                return Ok(());
            }
            if body == b"delayed" {
                tokio::time::sleep(Duration::from_millis(120)).await;
            }
            session.extended_data(channel, 1, bytes::Bytes::from_static(b"test stderr"))?;
            session.exit_status_request(channel, 7)?;
            session.eof(channel)?;
            session.close(channel)
        }
        async fn signal(
            &mut self,
            _: ChannelId,
            signal: russh::Sig,
            _: &mut server::Session,
        ) -> Result<(), Self::Error> {
            if matches!(signal, russh::Sig::TERM) {
                self.1.fetch_add(1, Ordering::Relaxed);
            }
            Ok(())
        }
    }

    #[test]
    fn local_agent_approval_expiry_and_ssh_output() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let signals = Arc::new(AtomicUsize::new(0));
        let files: FixtureFiles = Arc::new(Mutex::new(BTreeMap::new()));
        let port = runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let config = Arc::new(server::Config {
                keys: vec![
                    ssh_key::PrivateKey::random(&mut rand::rng(), ssh_key::Algorithm::Ed25519)
                        .unwrap(),
                ],
                ..Default::default()
            });
            let count = count.clone();
            let signals = signals.clone();
            let files = files.clone();
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let config = config.clone();
                    let handler = TestServer(
                        count.clone(),
                        signals.clone(),
                        files.clone(),
                        Default::default(),
                        None,
                        None,
                    );
                    tokio::spawn(async move {
                        if let Ok(running) = server::run_stream(config, stream, handler).await {
                            let _ = running.await;
                        }
                    });
                }
            });
            port
        });
        let context = egui::Context::default();
        let mut app = App::with_settings(&context, Settings::benchmark_defaults(), false);
        assert_eq!(app.agent_mode(), Mode::Disabled);
        assert!(!app.settings.on("ai_auto_open"));
        assert!(!app.settings.on("ai_upload"));
        assert!(!app.settings.on("ai_interactive"));
        app.settings.set("ai_interactive", "1".into());
        assert!(!app.settings.on("ai_audit_vault"));
        app.copy_agent_prompt(&context);
        assert!(!app.agent.active());
        app.start_agent(&context);
        assert!(!app.agent.active());
        app.dialog = None;
        app.settings.set("ai_access", "ask".into());
        app.profiles.push(Profile {
            name: "Test SSH".into(),
            connection: Connection::Ssh {
                terminal: "xterm-256color".into(),
                config: crate::models::SshConfig {
                    host: "127.0.0.1".into(),
                    port,
                    user: "test".into(),
                    password: Some("fixture-secret-not-for-discovery".into()),
                    key_path: None,
                    key_name: None,
                    vault_pass: None,
                    name: None,
                    compression: None,
                    key_passphrase: None,
                    use_agent: false,
                    keepalive: 0,
                },
            },
        });
        app.profiles.push(Profile {
            name: "Local fixture".into(),
            connection: Connection::Local {
                shell: String::new(),
            },
        });
        fn pump(app: &mut App, context: &egui::Context, ready: impl Fn(&App) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !ready(app) {
                app.poll(context);
                app.poll_agent(context);
                assert!(Instant::now() < deadline, "agent test timed out");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        app.start_agent(&context);
        assert!(app.agent.active());
        let prompt = app.agent.bridge.as_ref().unwrap().prompt(120);
        assert!(prompt.contains("rterm-agent sessions"));
        assert!(prompt.contains("rterm-agent execute"));
        assert!(prompt.contains("User-configured maximum command timeout: 120"));
        assert!(
            !prompt.contains(
                &*std::env::current_exe()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .to_string_lossy()
            )
        );
        assert!(!prompt.contains("C:\\Projects"));
        let post = |app: &App, value| app.agent.bridge.as_ref().unwrap().test_ipc(value);
        let invalid = post(&app, json!({"action":"list_sessions","unexpected":true}));
        assert_eq!(invalid.join().unwrap()["ok"], false);
        let invalid = post(&app, json!({"action":"unsupported"}));
        assert_eq!(invalid.join().unwrap()["ok"], false);
        let list = post(&app, json!({"action":"list_sessions"}));
        pump(&mut app, &context, |_| list.is_finished());
        let sessions = list.join().unwrap();
        assert_eq!(sessions["sessions"], json!([]));
        assert_eq!(sessions["saved_sessions"].as_array().unwrap().len(), 1);
        assert!(
            !sessions
                .to_string()
                .contains("fixture-secret-not-for-discovery")
        );
        let profile_id = sessions["saved_sessions"][0]["profile_id"]
            .as_str()
            .unwrap();
        assert_eq!(sessions["protocol_version"], 2);
        let again = post(&app, json!({"action":"list_sessions"}));
        pump(&mut app, &context, |_| again.is_finished());
        assert_eq!(
            again.join().unwrap()["saved_sessions"][0]["profile_id"],
            profile_id
        );
        let open = |app: &App, op: &str| {
            post(
                app,
                json!({"action":"open_session","profile_id":profile_id,"operation_id":op}),
            )
        };
        let denied = open(&app, "open-denied");
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        app.resolve_agent_permission(false);
        assert_eq!(denied.join().unwrap()["ok"], false);
        assert!(app.terminals.is_empty());
        let cancelled = open(&app, "open-cancelled");
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        let cancel = post(
            &app,
            json!({"action":"cancel","operation_id":"open-cancelled"}),
        );
        assert_eq!(cancel.join().unwrap()["status"], "cancelled");
        app.resolve_agent_permission(true);
        assert_eq!(cancelled.join().unwrap()["status"], "cancelled");
        assert!(app.terminals.is_empty());
        let stale = open(&app, "open-stale");
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        app.profiles[0].name = "Changed fixture".into();
        app.resolve_agent_permission(true);
        assert_eq!(stale.join().unwrap()["ok"], false);
        assert!(app.terminals.is_empty());
        app.profiles[0].name = "Test SSH".into();
        // Even unrestricted command mode does not silently start connections.
        app.settings.set("ai_access", "unrestricted".into());
        let approved = open(&app, "open-approved");
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        assert!(app.terminals.is_empty());
        app.resolve_agent_permission(true);
        let opened = approved.join().unwrap();
        assert_eq!(opened["ok"], true);
        assert_eq!(opened["connection_state"], "connecting");
        assert_eq!(app.terminals.len(), 1);
        assert_eq!(app.active, Some(opened["tab_id"].as_u64().unwrap()));
        let duplicate = open(&app, "open-approved");
        assert_eq!(duplicate.join().unwrap(), opened);
        assert_eq!(app.terminals.len(), 1);
        assert!(app.agent.grants.is_empty());
        app.settings.set("ai_access", "ask".into());
        pump(&mut app, &context, |app| app.host_trust.blocking());
        assert!(app.agent_sessions().is_empty());
        assert!(app.approve_fixture_host());
        pump(&mut app, &context, |app| !app.agent_sessions().is_empty());
        let id = app.agent_sessions()[0].0;
        let tab = app.terminals[0].tab;
        let initial_endpoint = app.terminals[0].endpoint.unwrap();
        assert_eq!(opened["session_id"], id);
        assert_ne!(initial_endpoint, id);
        let list = post(&app, json!({"action":"list_sessions"}));
        pump(&mut app, &context, |_| list.is_finished());
        let sessions = list.join().unwrap();
        assert_eq!(sessions["tabs"][0]["tab_id"], opened["tab_id"]);
        assert_eq!(sessions["tabs"][0]["state"], "connected");
        assert_eq!(sessions["sessions"][0]["session_id"], id);
        assert_eq!(sessions["active_session_id"], id);
        let unknown = post(
            &app,
            json!({"action":"execute","session_id":id+100,"command":"noop"}),
        );
        pump(&mut app, &context, |_| unknown.is_finished());
        assert_eq!(unknown.join().unwrap()["ok"], false);
        let execute = |app: &App, command: &str| {
            post(
                app,
                json!({"action":"execute","session_id":id,"command":command}),
            )
        };
        // Unrestricted foreground execution uses the existing PTY from the
        // first command. The actual output reaches the terminal grid.
        app.settings.set("ai_access", "unrestricted".into());
        let list = post(&app, json!({"action":"list_sessions"}));
        pump(&mut app, &context, |_| list.is_finished());
        assert_eq!(list.join().unwrap()["tabs"][0]["shell_confirmed"], true);
        let foreground = execute(&app, "pwd; echo foreground");
        pump(&mut app, &context, |app| {
            assert!(
                app.agent.running.is_empty(),
                "foreground command must wait for approval"
            );
            app.agent.pending.is_some()
        });
        app.resolve_agent_permission(true);
        pump(&mut app, &context, |app| !app.agent.running.is_empty());
        assert_eq!(app.active, Some(tab));
        assert_eq!(
            app.agent.activity.entries()[0].command,
            "pwd; echo foreground"
        );
        assert_eq!(count.load(Ordering::Relaxed), 0);
        pump(&mut app, &context, |_| foreground.is_finished());
        let result = foreground.join().unwrap();
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["execution_mode"], "interactive_shell");
        assert_eq!(result["stdout"], "\r\nforeground result\r\n");
        assert_eq!(result["stderr"], "");
        assert_eq!(result["exit_code"], 7);
        assert_eq!(count.load(Ordering::Relaxed), 0);
        assert_eq!(app.terminals.len(), 1);
        pump(&mut app, &context, |app| app.agent.running.is_empty());
        let entries = app.agent.activity.entries();
        assert_eq!(entries[0].exit_code, Some(7));
        assert_eq!(entries[0].phase, "completed");
        assert!(entries[0].finished_ms.is_some());
        #[cfg(feature = "benchmark")]
        assert!(app.terminals[0].visible_line_matches(|line| line.contains("foreground result")));
        assert!(app.agent.displays.is_empty());
        // Unrestricted foreground execution continues on the same PTY
        // after a nonzero exit and handles automatic cursor reports.
        let query = execute(&app, "echo cursor-query-fixture");
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        app.resolve_agent_permission(true);
        pump(&mut app, &context, |_| query.is_finished());
        let result = query.join().unwrap();
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["exit_code"], 7);
        pump(&mut app, &context, |app| app.agent.running.is_empty());
        let tunnel = app.agent.bridge.as_ref().unwrap().mcp_access();
        let tunnel_command = std::thread::spawn(move || {
            tunnel.request(json!({"action":"execute","session_id":id,"command":"echo tunnel fixture","operation_id":"tunnel-shell-fixture","wait_ms":4000}))
        });
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        assert!(
            app.agent
                .is_tunnel_request(app.agent.pending.as_ref().unwrap())
        );
        app.resolve_agent_permission(true);
        pump(&mut app, &context, |_| tunnel_command.is_finished());
        let result = tunnel_command.join().unwrap();
        assert_eq!(result["execution_mode"], "interactive_shell");
        assert_eq!(result["exit_code"], 7);
        assert!(result.get("output_path").is_none());
        pump(&mut app, &context, |app| app.agent.running.is_empty());
        assert!(
            app.agent.activity.entries()[0]
                .action
                .contains("OpenAI tunnel")
        );
        let followup = execute(&app, "echo followup");
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        app.resolve_agent_permission(true);
        pump(&mut app, &context, |_| followup.is_finished());
        assert_eq!(
            followup.join().unwrap()["execution_mode"],
            "interactive_shell"
        );
        pump(&mut app, &context, |app| app.agent.running.is_empty());
        assert_eq!(count.load(Ordering::Relaxed), 0);
        // Full-screen programs still block dispatch without an approval dialog.
        app.terminals[0].receive(b"\x1b[?1049h");
        let blocked = execute(&app, "echo blocked");
        pump(&mut app, &context, |app| {
            assert!(app.agent.pending.is_none());
            blocked.is_finished()
        });
        let result = blocked.join().unwrap();
        assert_eq!(result["status"], "failed");
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("Interactive shell unavailable")
        );
        app.terminals[0].receive(b"\x1b[?1049l");
        // Returning to the shell after typing still requires approval.
        assert!(app.terminals[0].send(&app.backend, b"\r".to_vec()));
        let resumed = execute(&app, "echo after manual input");
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        app.resolve_agent_permission(true);
        pump(&mut app, &context, |_| resumed.is_finished());
        assert_eq!(
            resumed.join().unwrap()["execution_mode"],
            "interactive_shell"
        );
        pump(&mut app, &context, |app| app.agent.running.is_empty());
        app.settings.set("ai_access", "ask".into());
        let denied = execute(&app, "echo foreground denied");
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        assert_eq!(app.agent.activity.entries()[0].phase, "awaiting_approval");
        app.resolve_agent_permission(false);
        assert_eq!(denied.join().unwrap()["ok"], false);
        app.refresh_agent_activity();
        assert_eq!(
            app.agent.activity.entries()[0].detail.as_deref(),
            Some("User denied command")
        );
        let approved = execute(&app, "echo foreground approved");
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        app.resolve_agent_permission(true);
        pump(&mut app, &context, |_| approved.is_finished());
        assert_eq!(
            approved.join().unwrap()["execution_mode"],
            "interactive_shell"
        );
        pump(&mut app, &context, |app| app.agent.running.is_empty());
        let revoked = execute(&app, "echo foreground disabled");
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        app.settings.set("ai_access", "disabled".into());
        app.poll_agent(&context);
        assert_eq!(revoked.join().unwrap()["ok"], false);
        assert!(!app.agent.active());
        assert!(app.agent.pending.is_none());
        app.agent.activity.clear();
        app.settings.set("ai_access", "ask".into());
        app.settings.set("ai_interactive", "0".into());
        app.start_agent(&context);
        let denied = execute(&app, "denied");
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        assert_eq!(count.load(Ordering::Relaxed), 0);
        app.resolve_agent_permission(false);
        assert_eq!(denied.join().unwrap()["ok"], false);
        assert_eq!(count.load(Ordering::Relaxed), 0);
        assert!(app.agent.displays.is_empty());
        let once = execute(&app, "one command");
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        app.active = None;
        app.resolve_agent_permission(true);
        assert_eq!(app.active, None);
        pump(&mut app, &context, |_| once.is_finished());
        let result = once.join().unwrap();
        assert_eq!(result["stdout"], "one command");
        assert_eq!(result["stderr"], "test stderr");
        assert_eq!(result["exit_code"], 7);
        assert_eq!(result["ok"], true);
        assert_eq!(result["command_success"], false);
        assert_eq!(result["status"], "completed");
        assert_eq!(count.load(Ordering::Relaxed), 1);
        pump(&mut app, &context, |app| app.agent.running.is_empty());
        assert!(app.agent.grants.is_empty());
        let timed = execute(&app, "grant");
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        app.agent.minutes = 5;
        app.resolve_agent_permission(true);
        pump(&mut app, &context, |_| timed.is_finished());
        assert_eq!(timed.join().unwrap()["stdout"], "grant");
        let next = execute(&app, "followup");
        pump(&mut app, &context, |_| next.is_finished());
        assert_eq!(next.join().unwrap()["stdout"], "followup");
        assert_eq!(count.load(Ordering::Relaxed), 3);
        let tunnel = app.agent.bridge.as_ref().unwrap().mcp_access();
        let remote = std::thread::spawn({
            let tunnel = tunnel.clone();
            move || {
                tunnel.request(json!({"action":"execute","session_id":id,"command":"must ask through the tunnel","operation_id":"tunnel-approval-fixture","wait_ms":5000}))
            }
        });
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        assert!(
            app.agent
                .is_tunnel_request(app.agent.pending.as_ref().unwrap())
        );
        app.resolve_agent_permission(false);
        assert_eq!(remote.join().unwrap()["status"], "failed");
        assert_eq!(count.load(Ordering::Relaxed), 3);
        assert_eq!(
            tunnel.request(json!({"action":"history"}))["operations"][0]["operation_id"],
            "tunnel-approval-fixture"
        );
        let local_history = post(&app, json!({"action":"history"})).join().unwrap();
        assert!(
            !local_history
                .to_string()
                .contains("tunnel-approval-fixture")
        );
        tunnel.stop();
        assert_eq!(tunnel.request(json!({"action":"history"}))["ok"], false);
        app.agent
            .grants
            .insert(id, Instant::now() - Duration::from_secs(1));
        let expired = execute(&app, "expired");
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        assert_eq!(count.load(Ordering::Relaxed), 3);
        app.agent.pending.as_mut().unwrap().deadline = Instant::now() - Duration::from_secs(1);
        app.resolve_agent_permission(true);
        assert_eq!(expired.join().unwrap()["ok"], false);
        assert_eq!(count.load(Ordering::Relaxed), 3);
        let running = execute(&app, "wait");
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        app.resolve_agent_permission(true);
        pump(&mut app, &context, |_| count.load(Ordering::Relaxed) == 4);
        assert!(app.agent.displays.is_empty());
        app.agent.stop();
        assert_eq!(running.join().unwrap()["ok"], false);
        assert!(!app.agent.active());
        assert!(app.agent.grants.is_empty());
        assert!(app.agent.activity.saved().is_empty());
        // Revocation completes the client's reply immediately, but the SSH
        // channel must finish cleanup before a new operation can be accepted.
        pump(&mut app, &context, |app| app.agent.running.is_empty());
        app.settings.set("ai_access", "unrestricted".into());
        app.copy_agent_prompt(&context);
        assert!(!app.agent.active());
        app.start_agent(&context);
        assert!(app.agent.active());
        app.settings.set("ai_interactive", "0".into());
        app.agent.remove_display(tab);
        app.active = None;
        let unrestricted = execute(&app, "unrestricted");
        pump(&mut app, &context, |_| unrestricted.is_finished());
        let unrestricted = unrestricted.join().unwrap();
        assert_eq!(unrestricted["stdout"], "unrestricted", "{unrestricted}");
        assert_eq!(count.load(Ordering::Relaxed), 5);
        assert!(app.agent.displays.is_empty());
        assert_eq!(app.active, None);
        // Keep the remaining isolated exec-channel regression tests in
        // background mode; foreground has its own same-PTY assertions.

        // A caller can receive an ID before the UI sees the request, then
        // cancel a pending approval without any new SSH exec.
        app.agent.stop();
        app.settings.set("ai_access", "ask".into());
        app.start_agent(&context);
        let submit = |app: &App, op: &str, cmd: &str, wait: u64| {
            post(
                app,
                json!({"action":"execute","session_id":id,"command":cmd,"operation_id":op,"wait_ms":wait}),
            )
        };
        let poll = |app: &App, op: &str, wait: u64| {
            post(
                app,
                json!({"action":"poll","operation_id":op,"wait_ms":wait}),
            )
        };
        let cancel = |app: &App, op: &str| post(app, json!({"action":"cancel","operation_id":op}));
        let queued = submit(&app, "cancel-before-approval", "never-run", 0)
            .join()
            .unwrap();
        assert_eq!(queued["status"], "queued");
        assert_eq!(queued["operation_id"], "cancel-before-approval");
        assert!(queued["remaining_ms"].as_u64().unwrap() <= 60_000);
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        assert_eq!(
            poll(&app, "cancel-before-approval", 0).join().unwrap()["status"],
            "awaiting_approval"
        );
        assert_eq!(
            poll(&app, "cancel-before-approval", 0).join().unwrap()["hint"],
            "Waiting for approval in RTerm"
        );
        assert_eq!(
            cancel(&app, "cancel-before-approval").join().unwrap()["status"],
            "cancelled"
        );
        app.resolve_agent_permission(true); // a stale click cannot execute it
        assert_eq!(count.load(Ordering::Relaxed), 5);
        assert_eq!(
            poll(&app, "cancel-before-approval", 0).join().unwrap()["status"],
            "cancelled"
        );

        app.agent.stop();
        app.settings.set("ai_access", "unrestricted".into());
        app.start_agent(&context);
        let start = Instant::now();
        app.settings.set("ai_command_timeout", "600".into());
        assert_eq!(
            post(&app, json!({"action":"execute","session_id":id,"command":"wait","operation_id":"stuck","wait_ms":0,"timeout_secs":900})).join().unwrap()["status"],
            "queued"
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        pump(&mut app, &context, |_| count.load(Ordering::Relaxed) == 6);
        let start = Instant::now();
        let pending = poll(&app, "stuck", 150).join().unwrap();
        assert_eq!(pending["status"], "running");
        assert!(pending["remaining_ms"].as_u64().unwrap() > 590_000);
        assert!(pending["remaining_ms"].as_u64().unwrap() <= 600_000);
        assert!(
            start.elapsed() >= Duration::from_millis(130)
                && start.elapsed() < Duration::from_secs(2)
        );
        assert_eq!(
            post(&app, json!({"action":"execute","session_id":id,"command":"wait","operation_id":"stuck","wait_ms":0,"timeout_secs":900})).join().unwrap()["status"],
            "running"
        );
        assert_eq!(
            submit(&app, "stuck", "wait", 0).join().unwrap()["ok"],
            false
        );
        assert_eq!(
            submit(&app, "stuck", "different", 0).join().unwrap()["ok"],
            false
        );
        assert_eq!(
            submit(&app, "another", "nope", 0).join().unwrap()["ok"],
            false
        );
        assert_eq!(count.load(Ordering::Relaxed), 6);
        let long_poll = poll(&app, "stuck", 5000);
        let second_poll = poll(&app, "stuck", 5000);
        std::thread::sleep(Duration::from_millis(40));
        assert!(!long_poll.is_finished());
        let invalid = post(
            &app,
            json!({"action":"cancel","operation_id":"stuck","unexpected":true}),
        );
        assert_eq!(invalid.join().unwrap()["ok"], false);
        assert_eq!(poll(&app, "stuck", 0).join().unwrap()["status"], "running");
        let prior_signals = signals.load(Ordering::Relaxed);
        let start = Instant::now();
        let cancelled = cancel(&app, "stuck").join().unwrap();
        assert_eq!(cancelled["status"], "cancelled");
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_eq!(long_poll.join().unwrap()["status"], "cancelled");
        assert_eq!(second_poll.join().unwrap()["status"], "cancelled");
        assert_eq!(cancel(&app, "stuck").join().unwrap()["status"], "cancelled");
        pump(&mut app, &context, |app| app.agent.running.is_empty());
        pump(&mut app, &context, |_| {
            signals.load(Ordering::Relaxed) > prior_signals
        });

        // A new command works after cancellation; its completion can be
        // observed while the UI is deliberately not being pumped.
        assert_eq!(
            submit(&app, "recovery", "delayed", 0).join().unwrap()["status"],
            "queued"
        );
        pump(&mut app, &context, |app| !app.agent.running.is_empty());
        let result = poll(&app, "recovery", 2000).join().unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(result["stdout"], "delayed");
        assert_eq!(result["exit_code"], 7);
        assert!(result["duration_ms"].as_u64().unwrap() >= 100);
        let page = post(
            &app,
            json!({"action":"poll","operation_id":"recovery","since":0,"wait_ms":0}),
        )
        .join()
        .unwrap();
        assert_eq!(page["output"], "delayedtest stderr");
        assert_eq!(page["next_offset"], 18);
        assert_eq!(page["stdout"], "delayed");
        assert_eq!(page["stderr"], "test stderr");
        assert_eq!(page["combined_output"], false);
        let history = post(&app, json!({"action":"history","limit":1}))
            .join()
            .unwrap();
        assert_eq!(history["operations"][0]["operation_id"], "recovery");
        assert_eq!(history["operations"][0]["exit_code"], 7);
        assert_eq!(count.load(Ordering::Relaxed), 7);
        assert_eq!(
            submit(&app, "recovery", "delayed", 0).join().unwrap(),
            result
        );
        assert_eq!(cancel(&app, "recovery").join().unwrap(), result);
        assert_eq!(count.load(Ordering::Relaxed), 7);
        assert_eq!(poll(&app, "unknown-id", 0).join().unwrap()["ok"], false);
        assert!(poll(&app, "recovery", 30_001).join().unwrap()["ok"] == false);
        pump(&mut app, &context, |app| app.agent.running.is_empty());

        if std::env::var_os("RTERM_AGENT_BENCH").is_some() {
            // Same loopback SSH connection, same command and UI polling cadence
            // for before/after comparisons. No human approval time included.
            fn profile(
                app: &mut App,
                context: &egui::Context,
                body: serde_json::Value,
                label: &str,
            ) {
                let mut samples = Vec::with_capacity(120);
                for index in 0..130 {
                    let start = Instant::now();
                    let task = app.agent.bridge.as_ref().unwrap().test_ipc(body.clone());
                    while !task.is_finished() {
                        app.poll(context);
                        app.poll_agent(context);
                        assert!(start.elapsed() < Duration::from_secs(5));
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    let response = task.join().unwrap();
                    if label == "exec" {
                        assert_eq!(response["stdout"], "benchmark");
                        assert_eq!(response["exit_code"], 7);
                    } else {
                        assert_eq!(response["ok"], true);
                    }
                    if index >= 10 {
                        samples.push(start.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                samples.sort_by(f64::total_cmp);
                let total: f64 = samples.iter().sum();
                println!(
                    "AGENT {label}: n={} median={:.3}ms p95={:.3}ms mean={:.3}ms throughput={:.1}/s",
                    samples.len(),
                    samples[samples.len() / 2],
                    samples[samples.len() * 95 / 100],
                    total / samples.len() as f64,
                    samples.len() as f64 * 1000.0 / total
                );
            }
            profile(
                &mut app,
                &context,
                json!({"action":"list_sessions"}),
                "list",
            );
            profile(
                &mut app,
                &context,
                json!({"action":"execute","session_id":id,"command":"benchmark"}),
                "exec",
            );
        }
        // Auto-open is an independent local preference; it must not
        // implicitly authorize commands or bypass profile validation.
        app.agent.stop();
        app.settings.set("ai_access", "ask".into());
        app.settings.set("ai_auto_open", "1".into());
        app.start_agent(&context);
        let list = post(&app, json!({"action":"list_sessions"}));
        pump(&mut app, &context, |_| list.is_finished());
        let sessions = list.join().unwrap();
        let profile = sessions["saved_sessions"][0]["profile_id"]
            .as_str()
            .unwrap();
        let reuse = post(
            &app,
            json!({"action":"open_session","profile_id":profile,"operation_id":"reuse-tab"}),
        );
        pump(&mut app, &context, |_| reuse.is_finished());
        let reused = reuse.join().unwrap();
        assert_eq!(reused["tab_id"], tab);
        assert_eq!(reused["session_id"], id);
        assert_eq!(reused["connection_state"], "connected");
        assert_eq!(app.terminals.len(), 1);

        let mut second_profile = app.profiles[0].clone();
        second_profile.name = "Second fixture SSH".into();
        app.profiles.push(second_profile);
        let list = post(&app, json!({"action":"list_sessions"}));
        pump(&mut app, &context, |_| list.is_finished());
        let sessions = list.join().unwrap();
        let profile = sessions["saved_sessions"][1]["profile_id"]
            .as_str()
            .unwrap();
        let open_body =
            json!({"action":"open_session","profile_id":profile,"operation_id":"auto-open"});
        let request = post(&app, open_body.clone());
        pump(&mut app, &context, |app| {
            assert!(app.agent.pending.is_none());
            request.is_finished()
        });
        let opened = request.join().unwrap();
        assert_eq!(opened["ok"], true);
        assert_eq!(opened["connection_state"], "connecting");
        let new_tab = opened["tab_id"].as_u64().unwrap();
        assert_eq!(app.active, Some(new_tab));
        assert_eq!(app.terminals.len(), 2);
        assert!(app.agent.grants.is_empty());
        assert_eq!(post(&app, open_body).join().unwrap(), opened);
        assert_eq!(app.terminals.len(), 2);
        pump(&mut app, &context, |app| app.agent_sessions().len() == 2);
        let new_id = opened["session_id"].as_u64().unwrap() as u32;
        let (new_endpoint, _) = app.agent_endpoint(new_id).unwrap();
        let command = post(
            &app,
            json!({"action":"execute","session_id":new_id,"command":"must-ask","operation_id":"auto-open-command"}),
        );
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        app.resolve_agent_permission(false);
        assert_eq!(command.join().unwrap()["ok"], false);

        // Keep one session occupied while executing and discovering through the same bridge.
        app.settings.set("ai_access", "unrestricted".into());
        assert_eq!(
            submit(&app, "fleet-long", "wait", 0).join().unwrap()["status"],
            "queued"
        );
        pump(&mut app, &context, |app| app.agent.running_on(Some(id)));
        let second = post(
            &app,
            json!({"action":"execute","session_id":new_id,"command":"fleet quick","operation_id":"fleet-quick"}),
        );
        let list = post(&app, json!({"action":"list_sessions"}));
        pump(&mut app, &context, |_| {
            second.is_finished() && list.is_finished()
        });
        let result = second.join().unwrap();
        assert_eq!(result["execution_mode"], "background_exec");
        assert_eq!(result["stdout"], "fleet quick");
        assert_eq!(result["stderr"], "test stderr");
        assert_eq!(result["exit_code"], 7);
        assert_eq!(
            list.join().unwrap()["sessions"].as_array().unwrap().len(),
            2
        );
        assert_eq!(
            poll(&app, "fleet-long", 0).join().unwrap()["status"],
            "running"
        );
        let busy = submit(&app, "fleet-conflict", "never run", 0)
            .join()
            .unwrap();
        assert_eq!(busy["ok"], false);
        assert!(busy["error"].as_str().unwrap().contains("SSH session"));
        let tunnel = app.agent.bridge.as_ref().unwrap().mcp_access();
        let cross_transport = std::thread::spawn(move || {
            tunnel.request(json!({"action":"execute","session_id":id,"command":"never run through MCP","operation_id":"cross-transport"}))
        });
        pump(&mut app, &context, |_| cross_transport.is_finished());
        let busy = cross_transport.join().unwrap();
        assert_eq!(busy["ok"], false);
        assert!(busy["error"].as_str().unwrap().contains("SSH session"));
        assert_eq!(
            cancel(&app, "fleet-long").join().unwrap()["status"],
            "cancelled"
        );
        pump(&mut app, &context, |app| app.agent.running.is_empty());
        let recovered = execute(&app, "fleet recovered");
        pump(&mut app, &context, |_| recovered.is_finished());
        assert_eq!(recovered.join().unwrap()["stdout"], "fleet recovered");
        assert_eq!(app.terminals.len(), 2);

        // Unrestricted background access is automatic; foreground shell
        // commands still require approval because prompt detection is local.
        app.agent.stop();
        app.settings.set("ai_access", "unrestricted".into());
        app.settings.set("ai_interactive", "1".into());
        app.start_agent(&context);
        let before_exec = count.load(Ordering::Relaxed);
        for session_id in [id, new_id, id, new_id] {
            let call = post(
                &app,
                json!({"action":"execute","session_id":session_id,"command":"echo multi-session fixture"}),
            );
            pump(&mut app, &context, |app| app.agent.pending.is_some());
            app.resolve_agent_permission(true);
            pump(&mut app, &context, |_| call.is_finished());
            let result = call.join().unwrap();
            assert_eq!(result["ok"], true, "{result}");
            assert_eq!(result["execution_mode"], "interactive_shell");
            pump(&mut app, &context, |app| app.agent.running.is_empty());
        }
        assert_eq!(count.load(Ordering::Relaxed), before_exec);
        let entries = app.agent.activity.entries();
        let session_rows: Vec<_> = entries
            .iter()
            .filter(|entry| entry.command == "echo multi-session fixture")
            .collect();
        assert_eq!(session_rows.len(), 4);
        assert_eq!(
            session_rows
                .iter()
                .map(|entry| &entry.session)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            2
        );
        assert!(app.agent.activity.saved().is_empty());
        app.agent.stop();
        app.settings.set("ai_access", "ask".into());
        app.settings.set("ai_interactive", "0".into());
        app.start_agent(&context);
        let list = post(&app, json!({"action":"list_sessions"}));
        pump(&mut app, &context, |_| list.is_finished());
        let sessions = list.join().unwrap();
        let profile = sessions["saved_sessions"][0]["profile_id"]
            .as_str()
            .unwrap();
        app.profiles[0].name = "Changed again".into();
        let stale = post(
            &app,
            json!({"action":"open_session","profile_id":profile,"operation_id":"auto-open-stale"}),
        );
        pump(&mut app, &context, |_| stale.is_finished());
        assert_eq!(stale.join().unwrap()["ok"], false);
        assert_eq!(app.terminals.len(), 2);
        // File uploads use the actual loopback SFTP subsystem, never a real
        // server or user file. The fixture stores only bytes in memory.
        let fixture_dir = crate::storage::test_private_tempdir();
        #[cfg(unix)]
        let fixture_root = std::fs::canonicalize(fixture_dir.path()).unwrap();
        #[cfg(windows)]
        let fixture_root = fixture_dir.path().to_path_buf();
        let source = fixture_root.join("sample code.py");
        let payload = b"print('fixture upload')\n".repeat(4096);
        std::fs::write(&source, &payload).unwrap();
        let source = source.to_str().unwrap();
        let upload = |app: &App, op: &str| {
            post(
                app,
                json!({
                    "action":"upload","session_id":new_id,"operation_id":op,
                    "local":source,"remote":"/fixture/sample.py"
                }),
            )
        };
        let disabled = upload(&app, "upload-disabled");
        pump(&mut app, &context, |_| disabled.is_finished());
        assert_eq!(disabled.join().unwrap()["ok"], false);
        assert!(files.lock().unwrap().is_empty());
        app.settings.set("ai_upload", "1".into());
        app.settings.set("ai_access", "unrestricted".into());
        let denied = upload(&app, "upload-denied");
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        app.resolve_agent_permission(false);
        assert_eq!(denied.join().unwrap()["ok"], false);
        assert!(files.lock().unwrap().is_empty());
        let approved = upload(&app, "upload-approved");
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        app.resolve_agent_permission(true);
        assert_eq!(app.active, Some(new_tab));
        pump(&mut app, &context, |_| approved.is_finished());
        let result = approved.join().unwrap();
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["bytes_uploaded"], payload.len() as u64);
        assert_eq!(files.lock().unwrap()["/fixture/sample.py"], payload);
        pump(&mut app, &context, |app| app.agent.running.is_empty());
        assert!(
            app.agent.displays[&new_tab]
                .lock()
                .unwrap()
                .status
                .as_ref()
                .unwrap()
                .contains("Upload complete")
        );
        assert_eq!(upload(&app, "upload-approved").join().unwrap(), result);
        let conflict = upload(&app, "upload-existing");
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        app.resolve_agent_permission(true);
        pump(&mut app, &context, |_| conflict.is_finished());
        assert_eq!(conflict.join().unwrap()["ok"], false);
        assert_eq!(files.lock().unwrap()["/fixture/sample.py"], payload);
        let project = fixture_root.join("project folder");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::create_dir_all(project.join("empty")).unwrap();
        std::fs::write(project.join("src").join("main.py"), &payload).unwrap();
        std::fs::write(project.join(".hidden"), b"fixture").unwrap();
        std::fs::write(project.join("empty.txt"), b"").unwrap();
        let folder_request = |app: &App, op: &str| {
            post(
                app,
                json!({
                    "action":"upload","session_id":new_id,"operation_id":op,
                    "local":project.to_str().unwrap(),"remote":"/fixture/project"
                }),
            )
        };
        let folder = folder_request(&app, "upload-folder");
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        app.resolve_agent_permission(true);
        pump(&mut app, &context, |_| folder.is_finished());
        let result = folder.join().unwrap();
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["bytes_uploaded"], payload.len() as u64 + 7);
        pump(&mut app, &context, |app| app.agent.running.is_empty());
        assert_eq!(
            app.agent.displays[&new_tab].lock().unwrap().progress,
            Some((payload.len() as u64 + 7, payload.len() as u64 + 7))
        );
        {
            let received = files.lock().unwrap();
            assert!(received.contains_key("/fixture/project/"));
            assert!(received.contains_key("/fixture/project/src/"));
            assert!(received.contains_key("/fixture/project/empty/"));
            assert_eq!(received["/fixture/project/src/main.py"], payload);
            assert_eq!(received["/fixture/project/.hidden"], b"fixture");
            assert_eq!(received["/fixture/project/empty.txt"], b"");
        }
        let before = files.lock().unwrap().clone();
        let conflict = folder_request(&app, "upload-folder-existing");
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        app.resolve_agent_permission(true);
        pump(&mut app, &context, |_| conflict.is_finished());
        assert_eq!(conflict.join().unwrap()["ok"], false);
        assert_eq!(*files.lock().unwrap(), before);

        // In shell mode a pager may never print the completion marker. Recover
        // that tab after cancellation and keep its ID stable across a later disconnect.
        app.settings.set("ai_interactive", "1".into());
        let first_tab = |app: &App| {
            app.terminals
                .iter()
                .position(|term| term.tab == tab)
                .unwrap()
        };
        assert_eq!(
            submit(&app, "pager-stuck", "pager-fixture", 0)
                .join()
                .unwrap()["status"],
            "queued"
        );
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        app.resolve_agent_permission(true);
        pump(&mut app, &context, |app| {
            app.agent.running_on(Some(id))
                && !app.terminals[first_tab(app)].agent_prompt_available()
        });
        assert_eq!(
            cancel(&app, "pager-stuck").join().unwrap()["status"],
            "cancelled"
        );
        pump(&mut app, &context, |app| {
            let term = &app.terminals[first_tab(app)];
            term.agent_prompt_available() && term.endpoint != Some(initial_endpoint)
        });
        let recovered_id = app.terminals[first_tab(&app)].endpoint.unwrap();
        assert_eq!(app.terminals.len(), 2);
        assert!(app.terminals[first_tab(&app)].agent_prompt_available());
        let follow_up = post(
            &app,
            json!({"action":"execute","session_id":id,"command":"echo post recovery"}),
        );
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        app.resolve_agent_permission(true);
        pump(&mut app, &context, |_| follow_up.is_finished());
        assert_eq!(follow_up.join().unwrap()["exit_code"], 7);
        assert_eq!(
            post(&app, json!({"action":"execute","session_id":id,"command":"pager-fixture","operation_id":"pager-timeout","wait_ms":0,"timeout_secs":1})).join().unwrap()["status"],
            "queued"
        );
        pump(&mut app, &context, |app| app.agent.pending.is_some());
        app.resolve_agent_permission(true);
        pump(&mut app, &context, |app| {
            let term = &app.terminals[first_tab(app)];
            term.agent_prompt_available() && term.endpoint != Some(recovered_id)
        });
        assert_eq!(
            poll(&app, "pager-timeout", 0).join().unwrap()["status"],
            "timed_out"
        );
        let after_timeout = app.terminals[first_tab(&app)].endpoint.unwrap();
        assert!(
            app.backend
                .write(after_timeout, b"disconnect-fixture\n".to_vec())
                .is_ok()
        );
        pump(&mut app, &context, |app| app.ssh_retry_pending(tab));
        let discovery = post(&app, json!({"action":"list_sessions"}));
        let during_retry = post(
            &app,
            json!({"action":"execute","session_id":id,"command":"do not replay","operation_id":"during-reconnect"}),
        );
        pump(&mut app, &context, |_| {
            discovery.is_finished() && during_retry.is_finished()
        });
        let listed = discovery.join().unwrap();
        assert_eq!(listed["tabs"][0]["session_id"], id);
        assert_eq!(listed["tabs"][0]["state"], "reconnecting");
        let retry = during_retry.join().unwrap();
        assert_eq!(retry["error_code"], "session_reconnecting");
        assert_eq!(retry["session_id"], id);
        assert_eq!(retry["operation_started"], false);
        pump(&mut app, &context, |app| {
            let term = &app.terminals[first_tab(app)];
            term.agent_prompt_available() && term.endpoint != Some(after_timeout)
        });
        assert_eq!(app.terminals.len(), 2);
        assert_eq!(
            app.terminals
                .iter()
                .find(|term| term.tab == new_tab)
                .unwrap()
                .endpoint,
            Some(new_endpoint)
        );
        app.settings.set("ai_interactive", "0".into());
        let stable_id = execute(&app, "still the same session");
        pump(&mut app, &context, |_| stable_id.is_finished());
        let result = stable_id.join().unwrap();
        assert_eq!(result["execution_mode"], "background_exec");
        assert_eq!(result["stdout"], "still the same session");
        assert_eq!(result["stderr"], "test stderr");
        assert_eq!(result["exit_code"], 7);
        assert_eq!(
            poll(&app, "during-reconnect", 0).join().unwrap()["error_code"],
            "session_reconnecting"
        );
        if std::env::var_os("RTERM_TEST_PAGER_SHELL").is_some() {
            let pager = execute(&app, PAGER_PROBE);
            pump(&mut app, &context, |_| pager.is_finished());
            let result = pager.join().unwrap();
            assert_eq!(result["execution_mode"], "background_exec", "{result}");
            assert_eq!(result["combined_output"], false);
            assert_eq!(result["exit_code"], 17, "{result}");
            assert_eq!(
                result["stderr"].as_str().unwrap().trim(),
                "pager fixture stderr"
            );
            let lines: Vec<_> = result["stdout"].as_str().unwrap().lines().collect();
            assert_eq!(lines.len(), 100);
            assert!(
                lines
                    .iter()
                    .enumerate()
                    .all(|(i, line)| line.parse() == Ok(i + 1))
            );
        }
        assert!(
            app.backend
                .write(new_endpoint, b"exit-fixture\n".to_vec())
                .is_ok()
        );
        pump(&mut app, &context, |app| {
            app.terminals
                .iter()
                .any(|term| term.tab == new_tab && term.closed)
        });
        assert!(!app.ssh_retry_pending(new_tab));
        let host = crate::host_keys::target("127.0.0.1", port);
        let expected = app.host_trust.store.list().unwrap()[&host].clone();
        let mismatched = crate::host_keys::Key {
            algorithm: expected.algorithm.clone(),
            fingerprint: "SHA256:changed-fixture".into(),
        };
        app.host_trust
            .store
            .change(&host, Some(&expected), Some(mismatched.clone()))
            .unwrap();
        let (endpoint, _) = app.agent_endpoint(id).unwrap();
        app.handle_event(BackendEvent::Closed(endpoint));
        pump(&mut app, &context, |app| app.host_trust.blocking());
        assert!(app.agent_endpoint(id).is_none());
        app.deny_fixture_replacement(&expected);
        assert!(!app.ssh_retry_pending(tab));
        assert!(app.agent_terminal(id).unwrap().closed);
        assert_eq!(app.host_trust.store.list().unwrap()[&host], mismatched);
        app.close_tab(new_tab);
        app.agent.stop();
    }

    #[test]
    fn transcript_renders_control_bytes_literally() {
        let bytes = b"hello\r\n\x1b]52;c;clipboard\x07\x1b[?1049h\x1b[6n";
        let rendered = literal_text(bytes);
        assert!(rendered.starts_with("hello\n"));
        assert!(!rendered.contains('\x1b'));
        assert!(!rendered.contains('\x07'));
        assert!(rendered.contains("52;c;clipboard"));
        assert_eq!(literal_text("こんにちは".as_bytes()), "こんにちは");
        assert_eq!(literal_text("a\u{202e}b".as_bytes()), "a\\u{202e}b");
    }
}
