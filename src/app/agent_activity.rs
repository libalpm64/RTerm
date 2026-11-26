use super::*;
use crate::{agent, models::AgentAuditEntry, storage::AGENT_AUDIT_LIMIT};
use std::{
    cell::OnceCell,
    collections::{BTreeSet, VecDeque},
    sync::{Arc, atomic::AtomicBool},
};

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

    #[cfg(test)]
    pub(super) fn saved(&self) -> &VecDeque<AgentAuditEntry> {
        &self.saved
    }

    pub(super) fn load(&mut self, mut entries: VecDeque<AgentAuditEntry>) {
        for entry in &mut entries {
            if entry.finished_ms.is_none() {
                entry.phase = "interrupted".into();
            }
        }
        self.saved = entries;
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
        self.current.truncate(AGENT_AUDIT_LIMIT);
    }

    pub(super) fn retain_command(&mut self, reply: &agent::Reply) {
        if let Some(item) = self.current.iter_mut().find(|item| {
            item.running
                .as_ref()
                .is_some_and(|(current, _)| current.same_operation(reply))
        }) {
            item.save = true;
            self.saved.push_front(item.entry.clone());
            self.saved.truncate(AGENT_AUDIT_LIMIT);
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
            if !matches!(phase, "queued" | "awaiting_approval" | "running") {
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
            _ => return,
        };
        let terminal = self.agent_terminal(session_id);
        let session = self
            .agent
            .activity
            .session_key(terminal.map(|term| term.tab), session_id);
        let session_name = terminal.map_or_else(|| format!("Session {session_id}"), session_label);
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
        egui::Window::new(
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
            let entries = activity.entries();
            viewer.show(
                ui,
                self.theme,
                &entries,
                recording,
                activity.save_error.as_deref(),
            );
        });
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
        if self
            .selected
            .as_ref()
            .is_none_or(|id| !shown.iter().any(|entry| entry.id == *id))
        {
            self.selected = Some(shown[0].id.clone());
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
