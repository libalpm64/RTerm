use super::agent_activity::Activity;
use super::*;
use crate::agent::{self, Action as AgentAction, Bridge, LibraryChange, Mode, Permit, Request};
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
    attention_pending: bool,
    approvals: VecDeque<Request>,
    saved: BTreeMap<String, (usize, blake3::Hash)>,
    library_revision: Option<(String, blake3::Hash)>,
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
            self.attention_pending = true;
            context.request_repaint();
        } else {
            self.approvals.push_back(request);
        }
    }

    // Only surface a live approval, never a discovery or an automatic
    // background operation. The native loop consumes this once, even if
    // no frame is being drawn.
    pub(super) fn take_attention_request(&mut self) -> bool {
        std::mem::take(&mut self.attention_pending)
            && self
                .pending
                .as_ref()
                .is_some_and(|request| request.authorized() && request.deadline > Instant::now())
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
        self.attention_pending = false;
        self.bridge = None;
        self.grants.clear();
        self.tunnel_grants.clear();
        self.saved.clear();
        self.library_revision = None;
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
            .find(|term| term.kind == Kind::Ssh && self.ssh_agent_id(term.tab) == Some(session_id))
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
    fn agent_library_hash(&self) -> Option<blake3::Hash> {
        let bytes = zeroize::Zeroizing::new(
            serde_json::to_vec(&(&self.session_folders, &self.profiles)).ok()?,
        );
        Some(blake3::hash(&bytes))
    }

    fn agent_library_revision(&mut self) -> Option<String> {
        let hash = self.agent_library_hash()?;
        if let Some((id, old)) = &self.agent.library_revision
            && *old == hash
        {
            return Some(id.clone());
        }
        let id = agent::random_id();
        self.agent.library_revision = Some((id.clone(), hash));
        Some(id)
    }

    fn agent_saved_sessions(&mut self) -> Vec<serde_json::Value> {
        let previous: BTreeMap<_, _> = std::mem::take(&mut self.agent.saved)
            .into_iter()
            .map(|(id, (index, hash))| ((index, *hash.as_bytes()), id))
            .collect();
        let mut entries = Vec::new();
        for (index, profile) in self.profiles.iter().enumerate() {
            let Ok(bytes) = serde_json::to_vec(profile) else {
                continue;
            };
            let bytes = zeroize::Zeroizing::new(bytes);
            let fingerprint = blake3::hash(&bytes);
            let id = previous
                .get(&(index, *fingerprint.as_bytes()))
                .cloned()
                .unwrap_or_else(agent::random_id);
            self.agent.saved.insert(id.clone(), (index, fingerprint));
            // Do not expose local shell commands, serial device paths, or credentials.
            let target = profile.network_port().map(|_| profile.subtitle());
            entries.push(json!({"profile_id":id,"name":profile.name,"target":target,
                "folder":profile.folder,"kind":profile.kind().title(),"port":profile.network_port()}));
        }
        entries
    }

    fn agent_profile_index(&self, id: &str) -> Option<usize> {
        let (index, fingerprint) = self.agent.saved.get(id)?;
        let profile = self.profiles.get(*index)?;
        let bytes = zeroize::Zeroizing::new(serde_json::to_vec(profile).ok()?);
        (blake3::hash(&bytes) == *fingerprint).then_some(*index)
    }

    fn agent_saved_profile(&self, id: &str) -> Option<&Profile> {
        let profile = self.profiles.get(self.agent_profile_index(id)?)?;
        (profile.kind() == Kind::Ssh).then_some(profile)
    }

    // Validate the exact discovered library before showing an approval and
    // again at commit. A folder name may have been deleted and reused.
    pub(super) fn agent_library_preview(
        &self,
        revision: &str,
        change: &LibraryChange,
    ) -> Result<String, String> {
        change.validate().map_err(str::to_owned)?;
        if self.locked
            || !self
                .agent
                .library_revision
                .as_ref()
                .is_some_and(|(id, hash)| {
                    id == revision && self.agent_library_hash() == Some(*hash)
                })
        {
            return Err("Saved library changed; list sessions again".into());
        }
        // A pending human connection form can save a snapshot asynchronously.
        // Avoid racing it, even if the connection is not yet authenticated.
        if self.dialog.is_some()
            || self.session_folder_ui.has_draft()
            || !self.local_profiles.is_empty()
            || self.terminals.iter().any(|term| term.starting())
        {
            return Err(
                "Finish the pending dialog, folder editor, or connection before editing saved sessions".into(),
            );
        }
        let profile = |id: &str| {
            self.agent_profile_index(id)
                .map(|i| &self.profiles[i])
                .ok_or("Unknown or changed profile; list sessions again".to_owned())
        };
        let folder = |name: &str| {
            if self.session_folders.contains(name) {
                Ok(())
            } else {
                Err("Folder no longer exists".to_owned())
            }
        };
        let available = |name: &str| {
            if self.session_folders.contains(name) {
                Err("Folder already exists".to_owned())
            } else {
                Ok(())
            }
        };
        let label = |p: &Profile| {
            if p.network_port().is_some() {
                format!("{:?} ({} {})", p.name, p.kind().title(), p.subtitle())
            } else {
                format!("{:?} ({})", p.name, p.kind().title())
            }
        };
        Ok(match change {
            LibraryChange::CreateFolder { name } => {
                available(name)?;
                format!("Create folder {name:?}")
            }
            LibraryChange::RenameFolder { folder: old, name } => {
                folder(old)?;
                if old != name {
                    available(name)?;
                }
                format!("Rename folder {old:?} to {name:?} (all members stay in the folder)")
            }
            LibraryChange::RemoveFolder { folder: old } => {
                folder(old)?;
                format!("Remove folder {old:?} and ungroup its sessions (no sessions deleted)")
            }
            LibraryChange::MoveSession {
                profile_id,
                folder: destination,
            } => {
                let p = profile(profile_id)?;
                if let Some(name) = destination {
                    folder(name)?;
                }
                format!("Move {} from {:?} to {:?}", label(p), p.folder, destination)
            }
            LibraryChange::RenameSession { profile_id, name } => {
                format!("Rename {} to {name:?}", label(profile(profile_id)?))
            }
            LibraryChange::SetPort { profile_id, port } => {
                let p = profile(profile_id)?;
                let old = p.network_port().ok_or("Profile has no network port")?;
                format!(
                    "Change saved connection {} port from {old} to {port}. Existing tabs retain their connection settings.",
                    label(p)
                )
            }
        })
    }

    fn apply_agent_library_change(
        &mut self,
        revision: &str,
        change: &LibraryChange,
    ) -> Result<(), String> {
        self.agent_library_preview(revision, change)?;
        let index = |id: &str| {
            self.agent_profile_index(id)
                .ok_or("Profile changed".to_owned())
        };
        match change {
            LibraryChange::CreateFolder { name } => self.save_session_folder(None, name.clone())?,
            LibraryChange::RenameFolder { folder, name } => {
                self.save_session_folder(Some(folder.clone()), name.clone())?
            }
            LibraryChange::RemoveFolder { folder } => self.remove_session_folder(folder)?,
            LibraryChange::MoveSession { profile_id, folder } => {
                self.move_saved_session_index(index(profile_id)?, folder.clone())?
            }
            LibraryChange::RenameSession { profile_id, name } => {
                self.rename_saved_session(index(profile_id)?, name.clone())?
            }
            LibraryChange::SetPort { profile_id, port } => {
                self.set_saved_session_port(index(profile_id)?, *port)?
            }
        }
        self.agent.library_revision = None;
        Ok(())
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
                    rate_bps: None,
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
                self.agent.attention_pending = self.agent.pending.is_some();
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
                    AgentAction::ManageLibrary {
                        library_revision,
                        change,
                    } => self.agent_library_preview(library_revision, change).is_ok(),
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
            let _ = request
                .reply
                .send(agent::error("Permission request expired or target changed"));
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
                            let saved_profiles = self.agent_saved_sessions();
                            let saved: Vec<_> = saved_profiles
                                .iter()
                                .filter(|p| p["kind"] == "SSH")
                                .collect();
                            let library_revision = self.agent_library_revision();
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
                            let _ = request.reply.send(json!({"ok": true, "sessions": entries, "saved_sessions":saved,"saved_profiles":saved_profiles,"session_folders":self.session_folders,"library_revision":library_revision,"tabs":tabs,"active_session_id":active,"execution_mode":execution_mode}));
                        }
                        AgentAction::ManageLibrary {
                            library_revision,
                            change,
                        } => match self.agent_library_preview(library_revision, change) {
                            Err(error) => {
                                let _ = request.reply.send(agent::error(error));
                            }
                            Ok(_) => {
                                if mode == Mode::Unrestricted {
                                    self.complete_agent_permission(request, true);
                                } else {
                                    self.agent.queue_approval(request, context);
                                }
                            }
                        },
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
                                                || app.agent_mode() == Mode::Disabled
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
                                                    // SFTP has its own channel, not the shared PTY.
                                                    // The explicit upload opt-in plus Unrestricted
                                                    // authorizes dispatch without a per-file prompt.
                                                    // Recheck policy after inspection; Ask still
                                                    // requires approval of this exact manifest.
                                                    if app.agent_mode() == Mode::Unrestricted {
                                                        app.dispatch_agent_upload(request);
                                                    } else {
                                                        app.agent.queue_approval(request, &context);
                                                    }
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
            self.explorer.expanded = false;
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
                "Inspected upload manifest is unavailable; request a new inspection",
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
                        rate_bps: None,
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
        if let AgentAction::ManageLibrary {
            library_revision,
            change,
        } = &request.action
        {
            let preview = self.agent_library_preview(library_revision, change);
            let remaining = request
                .deadline
                .saturating_duration_since(Instant::now())
                .as_secs();
            let response = egui::Modal::new(egui::Id::new("agent-library-approval"))
                .frame(frame)
                .show(context, |ui| {
                    ui.set_width(460.0);
                    ui.heading("AI agent requests a saved-session change");
                    if tunneled {
                        ui.label("Requested through OpenAI Secure MCP Tunnel.");
                    }
                    let valid = preview.is_ok();
                    let message = preview.unwrap_or_else(|error| error);
                    ui.label(literal_text(message.as_bytes()));
                    ui.label(format!("Request expires in {remaining}s"));
                    ui.horizontal(|ui| {
                        if ui.button("Deny").clicked() {
                            return Some(false);
                        }
                        if ui
                            .add_enabled(valid, egui::Button::new("Apply change"))
                            .clicked()
                        {
                            return Some(true);
                        }
                        None
                    })
                    .inner
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
        if let AgentAction::ManageLibrary {
            library_revision,
            change,
        } = &request.action
        {
            if allow
                && !self.locked
                && self.agent.active()
                && self.agent_mode() != Mode::Disabled
                && request.authorized()
                && request.deadline > Instant::now()
            {
                request.reply.commit(&request.active, || {
                    match self.apply_agent_library_change(library_revision, change) {
                        Ok(()) => json!({"ok":true,"rediscover":true}),
                        Err(error) => agent::error(error),
                    }
                });
            } else {
                let _ = request
                    .reply
                    .send(agent::error("User denied library change or access expired"));
            }
            return;
        }

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
#[path = "../../tests/unit/app/agent_ui.rs"]
mod tests;
