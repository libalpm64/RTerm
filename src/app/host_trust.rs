use super::{known_hosts::KnownHosts, *};
use crate::host_keys::{Key, Store};
use std::collections::VecDeque;

pub(super) struct Trust {
    pub(super) store: Store,
    pending: VecDeque<Prompt>,
    hosts: Option<KnownHosts>,
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
            hosts: None,
        }
    }

    pub(super) fn remove_tab(&mut self, tab: u64) {
        self.pending.retain(|prompt| prompt.tab != tab);
    }

    pub(super) fn blocking(&self) -> bool {
        !self.pending.is_empty() || self.hosts.is_some()
    }

    pub(super) fn open_hosts(&mut self) {
        if self.hosts.is_none() {
            self.hosts = Some(KnownHosts::new(&self.store));
        }
    }
}

impl App {
    pub(super) fn request_host_trust(&mut self, prompt: Prompt) {
        if self.locked {
            self.finish_host_trust(prompt, false);
        } else {
            if let Some(hosts) = &mut self.host_trust.hosts {
                hosts.cancel_confirmation();
            }
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
                    if let Some(hosts) = &mut self.host_trust.hosts {
                        hosts.reload(&self.host_trust.store);
                    }
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

    pub(super) fn show_host_trust(&mut self, context: &egui::Context) {
        if self.locked {
            for prompt in std::mem::take(&mut self.host_trust.pending) {
                self.finish_host_trust(prompt, false);
            }
            self.host_trust.hosts = None;
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
                    ui.label(crate::host_keys::display_host(&prompt.host));
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
        } else if let Some(hosts) = &mut self.host_trust.hosts
            && hosts.show(context, &mut self.host_trust.store, self.theme)
        {
            self.host_trust.hosts = None;
            self.focus_terminal_after_dialog();
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/app/host_trust.rs"]
mod test_support;
