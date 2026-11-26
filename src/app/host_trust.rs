use super::*;
use crate::host_keys::{Key, Store};
use std::collections::VecDeque;

pub(super) struct Trust {
    pub(super) store: Store,
    pending: VecDeque<Prompt>,
    show_hosts: bool,
}

pub(super) struct Prompt {
    pub(super) tab: u64,
    pub(super) host: String,
    pub(super) profile: Profile,
    pub(super) save: Option<Option<usize>>,
    pub(super) trusted: Option<Key>,
    pub(super) presented: Key,
}

impl Trust {
    pub(super) fn new(persistent: bool) -> Self {
        Self {
            store: Store::new(persistent),
            pending: VecDeque::new(),
            show_hosts: false,
        }
    }

    pub(super) fn remove_tab(&mut self, tab: u64) {
        self.pending.retain(|prompt| prompt.tab != tab);
    }

    pub(super) fn blocking(&self) -> bool {
        !self.pending.is_empty() || self.show_hosts
    }

    pub(super) fn open_hosts(&mut self) {
        self.show_hosts = true;
    }
}

impl App {
    pub(super) fn request_host_trust(&mut self, prompt: Prompt) {
        if self.locked {
            self.finish_host_trust(prompt, false);
        } else {
            self.host_trust.pending.push_back(prompt);
        }
    }

    fn finish_host_trust(&mut self, prompt: Prompt, approve: bool) {
        if !self.terminals.iter().any(|term| term.tab == prompt.tab) {
            return;
        }
        if approve {
            let saved = self.host_trust.store.change(
                &prompt.host,
                prompt.trusted.as_ref(),
                Some(prompt.presented),
            );
            match saved {
                Ok(()) => {
                    self.start_remote_connection(prompt.tab, prompt.profile, prompt.save);
                    return;
                }
                Err(error) => self.fail(error),
            }
        }
        let message = if prompt.trusted.is_some() {
            "SSH host key changed. Connection blocked."
        } else {
            "SSH host key was not trusted. Connection closed."
        };
        self.remote_connection_failed(prompt.tab, message);
    }

    #[cfg(test)]
    pub(super) fn approve_fixture_host(&mut self) -> bool {
        let Some(prompt) = self.host_trust.pending.pop_front() else {
            return false;
        };
        assert!(prompt.trusted.is_none());
        assert!(prompt.presented.fingerprint.starts_with("SHA256:"));
        self.finish_host_trust(prompt, true);
        true
    }

    #[cfg(test)]
    pub(super) fn deny_fixture_replacement(&mut self, expected: &Key) {
        let prompt = self.host_trust.pending.pop_front().unwrap();
        assert!(prompt.trusted.is_some());
        assert_eq!(&prompt.presented, expected);
        self.finish_host_trust(prompt, false);
    }

    pub(super) fn show_host_trust(&mut self, context: &egui::Context) {
        if self.locked {
            for prompt in std::mem::take(&mut self.host_trust.pending) {
                self.finish_host_trust(prompt, false);
            }
            self.host_trust.show_hosts = false;
            return;
        }
        if let Some(prompt) = self.host_trust.pending.front() {
            let changed = prompt.trusted.is_some();
            let response =
                egui::Modal::new(egui::Id::new("ssh-host-verification")).show(context, |ui| {
                    ui.set_width(460.0);
                    ui.heading(crate::i18n::tr(
                        ui,
                        if changed {
                            "SSH host key changed"
                        } else {
                            "Trust SSH host?"
                        },
                    ));
                    ui.label(&prompt.host);
                    if let Some(key) = &prompt.trusted {
                        ui.label(crate::i18n::tr(ui, "Trusted fingerprint"));
                        ui.monospace(&key.fingerprint);
                    }
                    ui.label(crate::i18n::format(
                        ui,
                        "Presented fingerprint ({algorithm})",
                        &[("algorithm", &prompt.presented.algorithm)],
                    ));
                    ui.monospace(&prompt.presented.fingerprint);
                    ui.label(crate::i18n::tr(
                        ui,
                        "Compare the fingerprint with your server or provider before trusting it.",
                    ));
                    ui.horizontal_wrapped(|ui| {
                        if ui.button(crate::i18n::tr(ui, "Cancel")).clicked() {
                            return Some(false);
                        }
                        let label = if changed {
                            "Trust Replacement and Connect"
                        } else {
                            "Trust and Connect"
                        };
                        if ui.button(crate::i18n::tr(ui, label)).clicked() {
                            return Some(true);
                        }
                        None
                    })
                    .inner
                });
            if let Some(approve) = response
                .inner
                .or_else(|| response.should_close().then_some(false))
            {
                let prompt = self.host_trust.pending.pop_front().unwrap();
                self.finish_host_trust(prompt, approve);
                self.focus_terminal_after_dialog();
            }
        } else if self.host_trust.show_hosts {
            let entries = self.host_trust.store.list();
            let response = egui::Modal::new(egui::Id::new("known-ssh-hosts")).show(context, |ui| {
                ui.set_width(470.0);
                ui.heading(crate::i18n::tr(ui, "Known SSH Hosts"));
                let mut forget = None;
                match entries.as_ref() {
                    Ok(entries) if entries.is_empty() => {
                        ui.label(crate::i18n::tr(ui, "No trusted hosts yet."));
                    }
                    Ok(entries) => {
                        egui::ScrollArea::vertical()
                            .max_height(320.0)
                            .show(ui, |ui| {
                                for (host, key) in entries {
                                    ui.horizontal(|ui| {
                                        ui.strong(host);
                                        if ui.small_button(crate::i18n::tr(ui, "Forget")).clicked()
                                        {
                                            forget = Some(host.clone());
                                        }
                                    });
                                    ui.monospace(format!("{} {}", key.algorithm, key.fingerprint));
                                    ui.separator();
                                }
                            });
                    }
                    Err(error) => {
                        ui.colored_label(self.theme.red, error);
                    }
                }
                (ui.button(crate::i18n::tr(ui, "Close")).clicked(), forget)
            });
            if let (Ok(entries), Some(host)) = (&entries, response.inner.1.as_ref())
                && let Err(error) = self.host_trust.store.change(host, entries.get(host), None)
            {
                self.fail(error);
            }
            if response.inner.0 || response.should_close() {
                self.host_trust.show_hosts = false;
                self.focus_terminal_after_dialog();
            }
        }
    }
}
