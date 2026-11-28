use super::*;
use crate::{agent, models::AgentAuditEntry};

use std::{
    cell::OnceCell,
    collections::{BTreeSet, VecDeque},
    sync::{Arc, atomic::AtomicBool},
};

const LIVE_ACTIVITY_LIMIT: usize = 250;

#[derive(Default)]
pub(super) struct Activity {
    run_id: OnceCell<String>,
    current: VecDeque<TrackedEntry>,
    saved: VecDeque<AgentAuditEntry>,
    viewer: Option<Viewer>,
    dirty: BTreeSet<String>,
    flush_requested: bool,
    save_error: Option<String>,
}

struct TrackedEntry {
    entry: AgentAuditEntry,
    running: Option<(agent::Reply, Arc<AtomicBool>)>,
    save: bool,
}

#[derive(Default)]
struct Viewer {
    session: Option<(String, String)>,
    search: String,
    selected: Option<String>,
    deletion: Option<Deletion>,
    delete_error: Option<String>,
}

struct Deletion {
    title: &'static str,
    session: Option<String>,
    ids: BTreeSet<String>,
}

fn deletable(entry: &AgentAuditEntry) -> bool {
    entry.finished_ms.is_some() || entry.phase == "interrupted"
}

impl Activity {
    fn session_key(&self, tab: Option<u64>, session_id: u32) -> String {
        let run = self.run_id.get_or_init(agent::random_id);
        match tab {
            Some(tab) => format!("{run}:tab:{tab}"),
            None => format!("{run}:unknown:{session_id}"),
        }
    }

    pub(super) fn entries(&self) -> Vec<&AgentAuditEntry> {
        let mut seen = BTreeSet::new();
        let mut entries: Vec<_> = self
            .current
            .iter()
            .map(|item| &item.entry)
            .chain(self.saved.iter())
            .filter(|item| seen.insert(item.id.as_str()))
            .collect();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.submitted_ms));
        entries
    }

    pub(super) fn load(&mut self, mut entries: VecDeque<AgentAuditEntry>) {
        let running: BTreeSet<_> = self
            .current
            .iter()
            .filter(|item| item.running.is_some())
            .map(|item| item.entry.id.as_str())
            .collect();
        for entry in &mut entries {
            if entry.finished_ms.is_none() && !running.contains(entry.id.as_str()) {
                entry.phase = "interrupted".into();
            }
        }
        self.saved = entries;
    }

    fn delete_history(
        &mut self,
        reviewed: &BTreeSet<String>,
        persist: impl FnOnce(&BTreeSet<String>) -> Result<VecDeque<AgentAuditEntry>, String>,
    ) -> Result<(), String> {
        // Revalidate at commit. In-flight and newly arrived operations must
        // never be deleted by a stale confirmation.
        let running: BTreeSet<_> = self
            .current
            .iter()
            .filter(|item| item.running.is_some())
            .map(|item| item.entry.id.as_str())
            .collect();
        let ids: BTreeSet<_> = self
            .entries()
            .into_iter()
            .filter(|entry| {
                reviewed.contains(&entry.id)
                    && deletable(entry)
                    && !running.contains(entry.id.as_str())
            })
            .map(|entry| entry.id.clone())
            .collect();
        if ids.is_empty() {
            return Ok(());
        }
        if self.saved.iter().any(|entry| ids.contains(&entry.id)) {
            // Do not alter the visible log unless encrypted storage succeeds.
            let mut saved = persist(&ids)?;
            // Preserve unrelated dirty records even if an earlier write
            // failed (for example, a full vault). Never resurrect deleted IDs.
            let mut dirty: BTreeMap<_, _> = self
                .saved
                .iter()
                .filter(|entry| self.dirty.contains(&entry.id) && !ids.contains(&entry.id))
                .map(|entry| (entry.id.clone(), entry.clone()))
                .collect();
            for entry in &mut saved {
                if let Some(update) = dirty.remove(&entry.id) {
                    *entry = update;
                }
            }
            saved.extend(dirty.into_values());
            saved
                .make_contiguous()
                .sort_by_key(|entry| std::cmp::Reverse(entry.submitted_ms));
            self.load(saved);
        }
        self.current
            .retain(|item| item.running.is_some() || !ids.contains(&item.entry.id));
        self.dirty.retain(|id| !ids.contains(id));
        self.flush_requested = !self.dirty.is_empty();
        if self.dirty.is_empty() {
            self.save_error = None;
        }
        Ok(())
    }

    pub(crate) fn forget_vault(&mut self) {
        self.saved.clear();
        self.current.retain(|item| !item.save);
        self.viewer = None;
        self.dirty.clear();
        self.flush_requested = false;
        self.save_error = None;
    }

    pub(super) fn clear(&mut self) {
        self.forget_vault();
        self.current.clear();
    }

    fn track(&mut self, entry: AgentAuditEntry, reply: agent::Reply, active: Arc<AtomicBool>) {
        self.current.push_front(TrackedEntry {
            entry,
            running: Some((reply, active)),
            save: false,
        });
        self.prune_live();
    }

    fn prune_live(&mut self) {
        // Never evict an in-flight operation before its outcome is known.
        // Persisted history lives in saved; only recent completed live rows
        // need to remain here.
        let mut completed = 0;
        self.current.retain(|item| {
            if item.running.is_some() {
                true
            } else {
                completed += 1;
                completed <= LIVE_ACTIVITY_LIMIT
            }
        });
    }

    pub(super) fn retain_command(&mut self, reply: &agent::Reply) {
        if let Some(item) = self.current.iter_mut().find(|item| {
            item.running
                .as_ref()
                .is_some_and(|(current, _)| current.same_operation(reply))
        }) {
            if item.save {
                return;
            }
            item.save = true;
            self.saved.push_front(item.entry.clone());
            self.dirty.insert(item.entry.id.clone());
            self.flush_requested = true;
        }
    }

    fn refresh(&mut self) {
        for item in &mut self.current {
            let Some((reply, active)) = &item.running else {
                continue;
            };
            let (phase, code, duration, detail) = reply.activity(active);
            if item.entry.phase == phase {
                continue;
            }
            item.entry.phase = phase.into();
            item.entry.exit_code = code;
            item.entry.duration_ms = duration;
            item.entry.detail = detail;
            if !matches!(
                phase,
                "queued" | "preparing_upload" | "awaiting_approval" | "running"
            ) {
                item.entry.finished_ms = Some(now_ms());
                item.running = None;
            }
            if item.save
                && let Some(saved) = self
                    .saved
                    .iter_mut()
                    .find(|entry| entry.id == item.entry.id)
            {
                saved.clone_from(&item.entry);
                self.dirty.insert(item.entry.id.clone());
                self.flush_requested = true;
            }
        }
        self.prune_live();
    }
}

impl App {
    pub(super) fn track_agent_request(
        &mut self,
        request: &agent::Request,
        active: Arc<AtomicBool>,
    ) {
        let (session_id, action, command) = match &request.action {
            agent::Action::Execute {
                session_id,
                command,
            } => (
                *session_id,
                if !self.settings.on("ai_interactive") {
                    "Background command"
                } else {
                    "Interactive command"
                },
                command.clone(),
            ),
            agent::Action::Upload {
                session_id,
                local,
                remote,
            } => (*session_id, "File upload", format!("{local}\n→ {remote}")),
            agent::Action::ManageLibrary {
                library_revision,
                change,
            } => {
                let preview = self
                    .agent_library_preview(library_revision, change)
                    .unwrap_or_else(|error| format!("Rejected library change: {error}"));
                (
                    0,
                    "Saved-session change",
                    super::agent_ui::literal_text(preview.as_bytes()),
                )
            }
            _ => return,
        };
        let terminal = self.agent_terminal(session_id);
        let session = self
            .agent
            .activity
            .session_key(terminal.map(|term| term.tab), session_id);
        let session_name = if session_id == 0 {
            "Saved session library".into()
        } else {
            terminal.map_or_else(|| format!("Session {session_id}"), session_label)
        };
        let entry = AgentAuditEntry {
            id: agent::random_id(),
            operation: request.reply.operation_id().into(),
            session,
            session_name,
            action: if self.agent.is_tunnel_request(request) {
                format!("{action} · OpenAI tunnel")
            } else {
                action.into()
            },
            command,
            submitted_ms: now_ms(),
            finished_ms: None,
            phase: "queued".into(),
            exit_code: None,
            duration_ms: None,
            detail: None,
        };
        self.agent
            .activity
            .track(entry, request.reply.clone(), active);
    }

    pub(super) fn refresh_agent_activity(&mut self) {
        let activity = &mut self.agent.activity;
        activity.refresh();
        if std::mem::take(&mut activity.flush_requested)
            && let Some(key) = &self.vault_key
        {
            let updates = activity
                .saved
                .iter()
                .filter(|entry| activity.dirty.contains(&entry.id))
                .cloned();
            match storage::save_agent_audit(key, updates) {
                Ok(saved) => {
                    activity.load(saved);
                    activity.dirty.clear();
                    activity.save_error = None;
                }
                Err(error) => activity.save_error = Some(error),
            }
        }
    }

    pub(super) fn open_agent_activity(&mut self, tab: Option<u64>) {
        let session = tab
            .and_then(|tab| self.terminals.iter().find(|term| term.tab == tab))
            .map(|term| {
                (
                    self.agent.activity.session_key(Some(term.tab), 0),
                    session_label(term),
                )
            });
        self.agent.activity.viewer = Some(Viewer {
            session,
            ..Default::default()
        });
    }

    pub(super) fn show_agent_activity(&mut self, context: &egui::Context) {
        if self.locked {
            return;
        }
        let Some(mut viewer) = self.agent.activity.viewer.take() else {
            return;
        };
        let mut open = true;
        let recording = self.settings.on("ai_audit_vault") && self.vault_key.is_some();
        let activity = &self.agent.activity;
        let frame = egui::Frame::window(&context.style());
        let frame = frame.inner_margin(egui::Margin {
            top: 2,
            bottom: 2,
            ..frame.inner_margin
        });
        let confirmed = egui::Window::new(
            egui::RichText::new(crate::i18n::text(context, "AI Activity"))
                .font(theme::strong(13.0)),
        )
        .id(egui::Id::new("agent-activity-window"))
        .frame(frame)
        .open(&mut open)
        .default_size(vec2(760.0, 475.0))
        .min_width(590.0)
        .min_height(355.0)
        .show(context, |ui| {
            if viewer.deletion.is_some() {
                viewer.confirm_delete(ui, self.theme)
            } else {
                let entries = activity.entries();
                viewer.show(
                    ui,
                    self.theme,
                    &entries,
                    recording,
                    activity.save_error.as_deref(),
                );
                false
            }
        })
        .and_then(|response| response.inner)
        .unwrap_or(false);
        if open
            && confirmed
            && let Some(deletion) = viewer.deletion.take()
        {
            let key = self.vault_key.as_ref();
            match self.agent.activity.delete_history(&deletion.ids, |ids| {
                let key = key.ok_or("Unlock the vault to delete saved AI history")?;
                storage::delete_agent_audit(key, ids)
            }) {
                Ok(()) => {
                    viewer.selected = None;
                    viewer.delete_error = None;
                }
                Err(error) => viewer.delete_error = Some(error),
            }
        }
        if open {
            self.agent.activity.viewer = Some(viewer);
        }
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn local_time(value: i64) -> String {
    chrono::DateTime::from_timestamp_millis(value).map_or_else(String::new, |time| {
        time.with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()
    })
}

fn session_label(term: &Terminal) -> String {
    let label = term.remote_target.as_ref().map_or_else(
        || term.name.clone(),
        |target| format!("{} · {target}", term.name),
    );
    super::agent_ui::literal_text(label.as_bytes())
}

fn outcome(entry: &AgentAuditEntry, theme: Theme) -> (String, Color32) {
    match entry.phase.as_str() {
        "queued" => ("Queued".into(), theme.muted),
        "preparing_upload" => ("Preparing upload".into(), theme.accent),
        "awaiting_approval" => ("Awaiting approval".into(), theme.yellow),
        "running" => ("Running".into(), theme.accent),
        "completed" => match entry.exit_code {
            Some(code) if code != 0 => (format!("Exit {code}"), theme.red),
            _ => ("Succeeded".into(), theme.green),
        },
        "failed"
            if entry
                .detail
                .as_deref()
                .is_some_and(|detail| detail.starts_with("User denied")) =>
        {
            ("Denied".into(), theme.red)
        }
        "failed" if entry.detail.as_deref() == Some("Agent access revoked") => {
            ("Cancelled".into(), theme.yellow)
        }
        "failed" => ("Failed".into(), theme.red),
        "cancelled" => ("Cancelled".into(), theme.yellow),
        "timed_out" => ("Timed out".into(), theme.red),
        _ => ("Interrupted".into(), theme.yellow),
    }
}

impl Viewer {
    fn review_delete<'a>(
        &mut self,
        title: &'static str,
        session: Option<String>,
        entries: impl Iterator<Item = &'a AgentAuditEntry>,
    ) {
        let ids: BTreeSet<_> = entries
            .filter(|entry| deletable(entry))
            .map(|entry| entry.id.clone())
            .collect();
        if !ids.is_empty() {
            self.deletion = Some(Deletion {
                title,
                session,
                ids,
            });
            self.delete_error = None;
        }
    }

    fn confirm_delete(&mut self, ui: &mut egui::Ui, theme: Theme) -> bool {
        use crate::i18n::{format, tr};
        let Some(deletion) = &self.deletion else {
            return false;
        };
        ui.heading(tr(ui, deletion.title));
        if let Some(session) = &deletion.session {
            ui.label(session);
        }
        ui.label(format(
            ui,
            "Delete {count} AI history entries?",
            &[("count", &deletion.ids.len().to_string())],
        ));
        ui.colored_label(
            theme.yellow,
            tr(
                ui,
                "Deleted entries cannot be restored from the current vault.",
            ),
        );
        ui.label(tr(ui, "Running and pending operations will remain. Existing connections, backups and temporary command output are not affected."));
        ui.separator();
        let mut confirm = false;
        let mut cancel = false;
        ui.horizontal_wrapped(|ui| {
            cancel = ui.button(tr(ui, "Cancel")).clicked();
            confirm = ui
                .button(egui::RichText::new(tr(ui, "Delete")).color(theme.red))
                .clicked();
        });
        if cancel {
            self.deletion = None;
        }
        confirm
    }

    fn show(
        &mut self,
        ui: &mut egui::Ui,
        theme: Theme,
        entries: &[&AgentAuditEntry],
        recording: bool,
        save_error: Option<&str>,
    ) {
        ui.horizontal(|ui| {
            ui.label(crate::i18n::tr(ui, "Session"));
            let mut sessions = BTreeMap::new();
            for entry in entries {
                sessions
                    .entry(&entry.session)
                    .or_insert((&entry.session_name, entry.submitted_ms));
            }
            let label = self
                .session
                .as_ref()
                .map_or(crate::i18n::tr(ui, "All sessions"), |(_, name)| name);
            egui::ComboBox::from_id_salt("audit-session")
                .width(225.0)
                .truncate()
                .selected_text(label)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.session,
                        None,
                        crate::i18n::tr(ui, "All sessions"),
                    );
                    for (id, (name, time)) in sessions {
                        let selected = self
                            .session
                            .as_ref()
                            .is_some_and(|(session, _)| session == id);
                        if ui
                            .selectable_label(selected, format!("{name} · {}", local_time(time)))
                            .clicked()
                        {
                            self.session = Some((id.clone(), name.clone()));
                        }
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text(crate::i18n::tr(ui, "Search commands or operation IDs"))
                    .desired_width(ui.available_width()),
            );
        });
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(crate::i18n::tr(
                    ui,
                    if recording {
                        "Vault logging on"
                    } else {
                        "Vault logging off"
                    },
                ))
                .color(theme.muted),
            );
            if let Some(error) = save_error {
                ui.colored_label(theme.red, "Could not save to vault")
                    .on_hover_text(error);
            }
        });
        ui.separator();

        let query = self.search.trim().to_lowercase();
        let shown: Vec<_> = entries
            .iter()
            .copied()
            .filter(|entry| {
                self.session
                    .as_ref()
                    .is_none_or(|(session, _)| entry.session == *session)
                    && (query.is_empty()
                        || entry.command.to_lowercase().contains(&query)
                        || entry.operation.to_lowercase().contains(&query)
                        || entry.session_name.to_lowercase().contains(&query))
            })
            .collect();
        if self
            .selected
            .as_ref()
            .is_none_or(|id| !shown.iter().any(|entry| entry.id == *id))
        {
            self.selected = shown.first().map(|entry| entry.id.clone());
        }
        let selected = shown
            .iter()
            .copied()
            .find(|entry| self.selected.as_deref() == Some(entry.id.as_str()));
        let session = self.session.clone();
        let session_has_history = session.as_ref().is_some_and(|(id, _)| {
            entries
                .iter()
                .any(|entry| entry.session == *id && deletable(entry))
        });
        let any_history = entries.iter().any(|entry| deletable(entry));
        ui.horizontal_wrapped(|ui| {
            use crate::i18n::tr;
            if ui
                .add_enabled(
                    selected.is_some_and(deletable),
                    egui::Button::new(tr(ui, "Delete Selected")),
                )
                .clicked()
            {
                self.review_delete(
                    "Delete Selected",
                    selected.map(|entry| format!("{} · {}", entry.session_name, entry.operation)),
                    selected.into_iter(),
                );
            }
            if ui
                .add_enabled(
                    session_has_history,
                    egui::Button::new(tr(ui, "Clear Session History")),
                )
                .clicked()
                && let Some((id, name)) = &session
            {
                self.review_delete(
                    "Clear Session History",
                    Some(name.clone()),
                    entries.iter().copied().filter(|entry| entry.session == *id),
                );
            }
            if ui
                .add_enabled(
                    any_history,
                    egui::Button::new(tr(ui, "Clear All AI History")),
                )
                .clicked()
            {
                self.review_delete("Clear All AI History", None, entries.iter().copied());
            }
        });
        if let Some(error) = &self.delete_error {
            ui.colored_label(
                theme.red,
                crate::i18n::tr(ui, "Could not delete AI history"),
            )
            .on_hover_text(error);
        }
        if shown.is_empty() {
            ui.label(crate::i18n::tr(
                ui,
                if entries.is_empty() {
                    "No AI commands or uploads recorded."
                } else {
                    "No activity matches these filters."
                },
            ));
            return;
        }
        let widths = [
            145.0,
            150.0,
            125.0,
            (ui.available_width() - 462.0).max(120.0),
        ];
        ui.horizontal(|ui| {
            for (label, width) in ["Time", "Session", "Status", "Command or action"]
                .into_iter()
                .zip(widths)
            {
                ui.add_sized(
                    [width, 20.0],
                    egui::Label::new(egui::RichText::new(crate::i18n::tr(ui, label)).strong())
                        .truncate(),
                );
            }
        });
        egui::ScrollArea::vertical()
            .id_salt("audit-rows")
            .max_height((ui.available_height() - 145.0).max(100.0))
            .auto_shrink([false, false])
            .show_rows(ui, 23.0, shown.len(), |ui, range| {
                for entry in &shown[range] {
                    let (mut status, color) = outcome(entry, theme);
                    if let Some(duration) = entry.duration_ms {
                        status.push_str(&format!(
                            " · {}",
                            crate::terminal::elapsed_label(Duration::from_millis(duration))
                        ));
                    }
                    let row = ui.horizontal(|ui| {
                        for (text, width) in [
                            egui::RichText::new(local_time(entry.submitted_ms)),
                            egui::RichText::new(&entry.session_name),
                            egui::RichText::new(status).color(color),
                            egui::RichText::new(super::agent_ui::literal_text(
                                entry.command.lines().next().unwrap_or_default().as_bytes(),
                            ))
                            .monospace(),
                        ]
                        .into_iter()
                        .zip(widths)
                        {
                            ui.add_sized(
                                [width, 23.0],
                                egui::Label::new(text).truncate().selectable(false),
                            );
                        }
                    });
                    let response =
                        ui.interact(row.response.rect, ui.id().with(&entry.id), Sense::click());
                    if response.clicked() {
                        self.selected = Some(entry.id.clone());
                    }
                    if self.selected.as_deref() == Some(entry.id.as_str()) {
                        ui.painter().rect_stroke(
                            response.rect,
                            2.0,
                            Stroke::new(1.0_f32, theme.accent),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
            });
        let Some(entry) = shown
            .into_iter()
            .find(|entry| self.selected.as_deref() == Some(entry.id.as_str()))
        else {
            return;
        };
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.strong(&entry.action);
            ui.label(&entry.session_name);
            ui.label(format!("Operation {}", entry.operation));
            if let Some(duration) = entry.duration_ms {
                ui.label(crate::terminal::elapsed_label(Duration::from_millis(
                    duration,
                )));
            }
            if let Some(finished) = entry.finished_ms {
                ui.label(format!("Finished {}", local_time(finished)));
            }
        });
        egui::ScrollArea::vertical()
            .id_salt("audit-details")
            .max_height(128.0)
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(super::agent_ui::literal_text(
                            entry.command.as_bytes(),
                        ))
                        .monospace(),
                    )
                    .selectable(true)
                    .wrap(),
                );
                if let Some(detail) = &entry.detail {
                    ui.label(super::agent_ui::literal_text(detail.as_bytes()));
                }
            });
    }
}

#[cfg(test)]
#[path = "../../tests/unit/app/agent_activity.rs"]
mod test_support;
