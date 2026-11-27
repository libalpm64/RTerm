use super::widgets::{form_field, input};
use crate::{theme::Theme, tunnel};
use eframe::egui;
use zeroize::{Zeroize, Zeroizing};

pub(crate) struct Draft {
    id: String,
    client: String,
    key: Zeroizing<String>,
    replacing_key: bool,
    remove: bool,
    export_error: Option<String>,
}

impl Draft {
    pub(super) fn new(profile: Option<&tunnel::Profile>) -> Self {
        Self {
            id: profile.map_or_else(String::new, |profile| profile.id.clone()),
            client: profile.map_or_else(String::new, |profile| {
                profile.client.to_string_lossy().into_owned()
            }),
            key: Zeroizing::new(String::new()),
            replacing_key: false,
            remove: false,
            export_error: None,
        }
    }

    pub(super) fn profile(
        &self,
        saved: Option<&tunnel::Profile>,
    ) -> Result<Option<tunnel::Profile>, String> {
        if self.remove
            || (saved.is_none()
                && self.id.trim().is_empty()
                && self.key.trim().is_empty()
                && self.client.trim().is_empty())
        {
            return Ok(None);
        }
        let api_key = if self.key.trim().is_empty() || (saved.is_some() && !self.replacing_key) {
            saved
                .map(|profile| profile.api_key.clone())
                .ok_or("Enter the restricted tunnel runtime API key")?
        } else {
            self.key.trim().to_owned()
        };
        let profile = tunnel::Profile {
            id: self.id.trim().into(),
            api_key,
            client: self.client.trim().into(),
        };
        profile.validate()?;
        Ok(Some(profile))
    }

    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        theme: Theme,
        unlocked: bool,
        saved: Option<&tunnel::Profile>,
        running: Option<&tunnel::Running>,
    ) -> bool {
        ui.horizontal_wrapped(|ui| {
            ui.hyperlink_to("Platform Tunnels", tunnel::PLATFORM_TUNNELS);
            ui.hyperlink_to("API keys", tunnel::PLATFORM_API_KEYS);
            ui.hyperlink_to("Client downloads (manual setup)", tunnel::DOWNLOAD);
            ui.hyperlink_to("ChatGPT Plugins", tunnel::CHATGPT_PLUGINS);
            ui.hyperlink_to("Guide", tunnel::GUIDE);
        });
        if !unlocked {
            ui.label(crate::i18n::tr(
                ui,
                "Unlock the vault to configure the tunnel.",
            ));
            return false;
        }
        ui.add_space(6.0);
        ui.add_enabled_ui(!self.remove, |ui| {
            form_field(ui, theme, "Tunnel ID", &mut self.id, "tunnel_…", false);
            let key_label = ui
                .horizontal(|ui| {
                    let label = ui
                        .label(
                            egui::RichText::new("Runtime API key")
                                .size(11.0)
                                .color(theme.muted),
                        )
                        .on_hover_text("Use a restricted key with Tunnels Read and Use.");
                    if saved.is_some() {
                        let action = if self.replacing_key {
                            "Cancel replacement"
                        } else {
                            "Replace"
                        };
                        if ui.small_button(action).clicked() {
                            if self.replacing_key {
                                self.clear_key();
                            } else {
                                self.replacing_key = true;
                            }
                        }
                    }
                    label.id
                })
                .inner;
            if saved.is_some() && !self.replacing_key {
                let mut mask = "********".to_owned();
                ui.add_enabled_ui(false, |ui| {
                    input(ui, theme, &mut mask, "", false).labelled_by(key_label)
                });
            } else {
                input(ui, theme, &mut self.key, "Restricted runtime key", true)
                    .labelled_by(key_label);
            }
            let client_label = ui
                .horizontal(|ui| {
                    let label = ui.label(
                        egui::RichText::new("Tunnel client")
                            .size(11.0)
                            .color(theme.muted),
                    );
                    if !self.client.is_empty() && ui.small_button("Use Automatic").clicked() {
                        self.client.clear();
                    }
                    if ui.small_button(crate::i18n::tr(ui, "Browse…")).clicked()
                        && let Some(path) = rfd::FileDialog::new()
                            .set_title("Select OpenAI tunnel-client")
                            .pick_file()
                    {
                        self.client = path.to_string_lossy().into_owned();
                    }
                    label.id
                })
                .inner;
            input(
                ui,
                theme,
                &mut self.client,
                "Automatic (bundled client)",
                false,
            )
            .labelled_by(client_label);
            ui.label(
                "RTerm installers include the tunnel client. Leave this blank for automatic setup.",
            );
        });
        let has_credentials = !self.id.trim().is_empty()
            && if saved.is_some() && !self.replacing_key {
                true
            } else {
                !self.key.trim().is_empty()
            };
        ui.add_space(6.0);
        let connect = ui
            .horizontal_wrapped(|ui| {
                let connect = ui
                    .add_enabled(
                        !self.remove && has_credentials,
                        egui::Button::new("Apply and Connect"),
                    )
                    .on_disabled_hover_text("Enter the tunnel credentials.")
                    .clicked();
                match running.map(tunnel::Running::status) {
                    Some(tunnel::Status::Ready) => {
                        ui.colored_label(theme.green, "Tunnel ready");
                    }
                    Some(tunnel::Status::Connecting) => {
                        ui.label("Connecting…");
                    }
                    Some(tunnel::Status::Reconnecting) => {
                        ui.label("Reconnecting…");
                    }
                    Some(tunnel::Status::Failed(error)) => {
                        ui.colored_label(theme.red, error);
                    }
                    None if saved.is_some() => {
                        ui.label("Tunnel stopped");
                    }
                    None => {}
                }
                connect
            })
            .inner;
        if let Some(saved) = saved {
            ui.horizontal_wrapped(|ui| {
                if ui.small_button("Copy setup").clicked() {
                    ui.ctx().copy_text(saved.setup());
                }
                if ui.small_button("Export…").clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .set_title("Export RTerm ChatGPT setup")
                        .set_file_name("rterm-chatgpt-setup.txt")
                        .save_file()
                {
                    self.export_error = std::fs::write(path, saved.setup())
                        .err()
                        .map(|err| err.to_string());
                }
            });
            ui.checkbox(&mut self.remove, "Remove saved profile on Apply");
            if let Some(error) = &self.export_error {
                ui.colored_label(theme.red, format!("Could not export setup. {error}"));
            }
        }
        connect
    }

    pub(super) fn clear_key(&mut self) {
        self.key.zeroize();
        self.replacing_key = false;
    }
}

#[cfg(test)]
#[path = "../../tests/unit/dialogs/tunnel.rs"]
mod tests;
