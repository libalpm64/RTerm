use eframe::egui::{self, Align, RichText, Sense, vec2};

use crate::{
    app::App,
    models::{Connection, Profile, SerialConfig, SshConfig, SshKey, TelnetConfig},
    storage,
    terminal::Kind,
    theme::{self, Icon, Theme},
};

use super::{
    Settings,
    settings::{BAUD_RATES, DATA_BITS},
    widgets::{
        CONTROL_HEIGHT, body_scroll, combo, dialog_tabs, field_label, form_field, input,
        number_value, select, toggle,
    },
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Authentication {
    Password,
    Key,
    Agent,
}

pub(crate) struct ConnectForm {
    kind: Kind,
    name: String,
    shell: String,
    host: String,
    ssh_port: String,
    telnet_port: String,
    username: String,
    password: String,
    authentication: Authentication,
    key_path: String,
    key_name: Option<String>,
    key_passphrase: String,
    terminal: String,
    serial_port: String,
    baud: String,
    bits: String,
    save: bool,
    index: Option<usize>,
    disk_keys: Vec<(String, String)>,
    ports: Vec<String>,
    pub(super) error: Option<String>,
}

impl ConnectForm {
    pub(crate) fn new(settings: &Settings) -> Self {
        Self {
            kind: Kind::Local,
            name: String::new(),
            shell: String::new(),
            host: String::new(),
            ssh_port: settings.get("ssh_default_port").to_owned(),
            telnet_port: "23".into(),
            username: "root".into(),
            password: String::new(),
            authentication: Authentication::Password,
            key_path: String::new(),
            key_name: None,
            key_passphrase: String::new(),
            terminal: "xterm-256color".into(),
            serial_port: String::new(),
            baud: settings.get("serial_default_baud").into(),
            bits: settings.get("serial_default_bits").into(),
            save: true,
            index: None,
            disk_keys: Vec::new(),
            ports: Vec::new(),
            error: None,
        }
    }

    pub(crate) fn editing(profile: &Profile, index: usize, settings: &Settings) -> Self {
        let mut form = Self::new(settings);
        form.kind = profile.kind();
        form.name.clone_from(&profile.name);
        form.index = Some(index);
        match &profile.connection {
            Connection::Local { shell } => form.shell.clone_from(shell),
            Connection::Ssh { config, .. } | Connection::Sftp { config } => {
                form.host.clone_from(&config.host);
                form.ssh_port = config.port.to_string();
                form.username.clone_from(&config.user);
                form.password = config.password.clone().unwrap_or_default();
                if let Connection::Ssh { terminal, .. } = &profile.connection {
                    form.terminal.clone_from(terminal);
                }
                form.key_name.clone_from(&config.key_name);
                form.key_path = config.key_path.clone().unwrap_or_default();
                form.key_passphrase = config.key_passphrase.clone().unwrap_or_default();
                form.authentication = if config.use_agent {
                    Authentication::Agent
                } else if config.key_path.is_some() || config.key_name.is_some() {
                    Authentication::Key
                } else {
                    Authentication::Password
                };
                form.refresh_keys();
            }
            Connection::Telnet(config) => {
                form.host.clone_from(&config.host);
                form.telnet_port = config.port.to_string();
            }
            Connection::Serial(config) => {
                form.serial_port.clone_from(&config.port);
                form.baud = config.baud.to_string();
                form.bits = config.bits.to_string();
                form.refresh_ports();
            }
        }
        form
    }

    pub(super) fn title(&self) -> &'static str {
        if self.index.is_some() {
            "Edit Session"
        } else {
            "Connect"
        }
    }

    pub(super) fn submit(&self, app: &mut App) -> Result<(), String> {
        let connection = match self.kind {
            Kind::Local => Connection::Local {
                shell: self.shell.clone(),
            },
            Kind::Ssh | Kind::Sftp => {
                let host = required(&self.host, "Host")?;
                let user = required(&self.username, "Username")?;
                if self.authentication == Authentication::Key
                    && self.key_name.is_none()
                    && self.key_path.trim().is_empty()
                {
                    return Err("Select an SSH key".into());
                }
                let config = SshConfig {
                    host,
                    port: number_value("Port", &self.ssh_port, 1..=65535)? as u16,
                    user,
                    password: (self.authentication == Authentication::Password)
                        .then(|| self.password.clone()),
                    key_path: (self.authentication == Authentication::Key
                        && self.key_name.is_none())
                    .then(|| self.key_path.clone()),
                    key_name: if self.authentication == Authentication::Key {
                        self.key_name.clone()
                    } else {
                        None
                    },
                    key_passphrase: (!self.key_passphrase.is_empty()
                        && self.authentication == Authentication::Key)
                        .then(|| self.key_passphrase.clone()),
                    vault_pass: None,
                    name: None,
                    compression: Some(app.settings.on("ssh_compression")),
                    keepalive: app.settings.get("ssh_keepalive").parse().unwrap_or(0),
                    use_agent: self.authentication == Authentication::Agent,
                };
                if self.kind == Kind::Sftp {
                    Connection::Sftp { config }
                } else {
                    Connection::Ssh {
                        config,
                        terminal: self.terminal.clone(),
                    }
                }
            }
            Kind::Telnet => Connection::Telnet(TelnetConfig {
                host: required(&self.host, "Host")?,
                port: number_value("Port", &self.telnet_port, 1..=65535)? as u16,
            }),
            Kind::Serial => Connection::Serial(SerialConfig {
                port: required(&self.serial_port, "Serial port")?,
                baud: self.baud.parse().unwrap_or(115200),
                bits: self.bits.parse().unwrap_or(8),
            }),
        };
        let name = if self.name.trim().is_empty() {
            match &connection {
                Connection::Local { .. } => "Local Shell".into(),
                Connection::Ssh { config, .. } | Connection::Sftp { config } => {
                    format!("{}@{}", config.user, config.host)
                }
                Connection::Telnet(config) => config.host.clone(),
                Connection::Serial(config) => config.port.clone(),
            }
        } else {
            self.name.trim().into()
        };
        app.connect(
            Profile { name, connection },
            self.save.then_some(self.index),
        );
        Ok(())
    }

    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        t: Theme,
        keys: &[SshKey],
        agent_enabled: bool,
    ) {
        let previous = Kind::ALL
            .iter()
            .position(|kind| *kind == self.kind)
            .unwrap_or_default();
        let labels = Kind::ALL.map(Kind::title);
        let selected = dialog_tabs(ui, "connect-tabs", &labels, previous, t);
        if selected != previous {
            self.kind = Kind::ALL[selected];
            self.error = None;
            if self.kind == Kind::Serial {
                self.refresh_ports();
            }
        }

        body_scroll(ui, ("connect-body", selected)).show(ui, |ui| {
            ui.add_space(18.0);
            let placeholder = match self.kind {
                Kind::Local => "Local Shell",
                Kind::Ssh | Kind::Sftp => "My Server",
                Kind::Telnet => "Telnet Session",
                Kind::Serial => "Serial Session",
            };
            form_field(ui, t, "Session Name", &mut self.name, placeholder, false);
            ui.add_space(FIELD_GAP);
            match self.kind {
                Kind::Local => self.show_local(ui, t),
                Kind::Ssh | Kind::Sftp => self.show_ssh(ui, t, keys, agent_enabled),
                Kind::Telnet => field_pair(
                    ui,
                    PORT_WIDTH,
                    |ui| {
                        form_field(
                            ui,
                            t,
                            "Host",
                            &mut self.host,
                            "Hostname or IP address",
                            false,
                        )
                    },
                    |ui| form_field(ui, t, "Port", &mut self.telnet_port, "", false),
                ),
                Kind::Serial => self.show_serial(ui, t),
            }
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                ui.label(crate::i18n::tr(ui, "Save Session"));
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    toggle(ui, "Save Session", &mut self.save, t);
                });
            });
            if let Some(error) = &self.error {
                ui.add_space(8.0);
                ui.colored_label(t.red, error);
            }
        });
    }

    fn show_local(&mut self, ui: &mut egui::Ui, t: Theme) {
        let label = field_label(ui, t, "Shell");
        let choices: &[(&str, &str)] = if cfg!(windows) {
            &[
                ("", "Default"),
                ("powershell.exe", "Windows PowerShell"),
                ("pwsh.exe", "PowerShell"),
                ("cmd.exe", "Command Prompt"),
            ]
        } else {
            &[
                ("", "Default"),
                ("/bin/bash", "bash"),
                ("/bin/zsh", "zsh"),
                ("/bin/sh", "sh"),
            ]
        };
        combo(ui, "shell", &mut self.shell, choices).labelled_by(label.id);
    }

    fn show_ssh(&mut self, ui: &mut egui::Ui, t: Theme, keys: &[SshKey], agent_enabled: bool) {
        field_pair(
            ui,
            PORT_WIDTH,
            |ui| {
                form_field(
                    ui,
                    t,
                    "Host",
                    &mut self.host,
                    "Hostname or IP address",
                    false,
                )
            },
            |ui| form_field(ui, t, "Port", &mut self.ssh_port, "", false),
        );
        ui.add_space(FIELD_GAP);
        if self.kind == Kind::Sftp {
            form_field(ui, t, "Username", &mut self.username, "Username", false);
        } else {
            field_pair(
                ui,
                174.0,
                |ui| form_field(ui, t, "Username", &mut self.username, "Username", false),
                |ui| {
                    let label = field_label(ui, t, "Terminal Type");
                    combo(
                        ui,
                        "term",
                        &mut self.terminal,
                        &[
                            ("xterm-256color", "xterm-256color"),
                            ("xterm", "xterm"),
                            ("vt100", "vt100"),
                        ],
                    )
                    .labelled_by(label.id);
                },
            );
        }
        ui.add_space(FIELD_GAP);
        let label = field_label(ui, t, "Authentication");
        let method = match self.authentication {
            Authentication::Password => "Password",
            Authentication::Key => "SSH Key",
            Authentication::Agent => "SSH Agent",
        };
        let mut refresh = false;
        select(ui, "authentication", crate::i18n::tr(ui, method), |ui| {
            ui.selectable_value(
                &mut self.authentication,
                Authentication::Password,
                crate::i18n::tr(ui, "Password"),
            );
            refresh = ui
                .selectable_value(
                    &mut self.authentication,
                    Authentication::Key,
                    crate::i18n::tr(ui, "SSH Key"),
                )
                .changed();
            ui.add_enabled_ui(agent_enabled, |ui| {
                ui.selectable_value(
                    &mut self.authentication,
                    Authentication::Agent,
                    crate::i18n::tr(ui, "SSH Agent"),
                );
            });
        })
        .response
        .labelled_by(label.id);
        if refresh {
            self.refresh_keys();
        }

        ui.add_space(FIELD_GAP);
        match self.authentication {
            Authentication::Password => {
                form_field(ui, t, "Password", &mut self.password, "Password", true)
            }
            Authentication::Key => {
                let label = field_label(ui, t, "SSH Key");
                let selected = self
                    .key_name
                    .clone()
                    .unwrap_or_else(|| self.key_path.clone());
                select(
                    ui,
                    "key-select",
                    if selected.is_empty() {
                        crate::i18n::tr(ui, "Select a key…")
                    } else {
                        &selected
                    },
                    |ui| {
                        for key in keys {
                            if ui
                                .selectable_label(
                                    self.key_name.as_deref() == Some(key.name.as_str()),
                                    format!("{} (vault)", key.name),
                                )
                                .clicked()
                            {
                                self.key_name = Some(key.name.clone());
                                self.key_path.clear();
                            }
                        }
                        for (name, path) in &self.disk_keys {
                            if ui.selectable_label(&self.key_path == path, name).clicked() {
                                self.key_name = None;
                                self.key_path.clone_from(path);
                            }
                        }
                        if ui.button(crate::i18n::tr(ui, "Browse…")).clicked()
                            && let Some(path) = rfd::FileDialog::new().pick_file()
                        {
                            self.key_name = None;
                            self.key_path = path.to_string_lossy().into_owned();
                        }
                    },
                )
                .response
                .labelled_by(label.id);
                ui.add_space(FIELD_GAP);
                form_field(
                    ui,
                    t,
                    "Key Passphrase",
                    &mut self.key_passphrase,
                    "Optional",
                    true,
                );
            }
            Authentication::Agent => {
                ui.label(
                    RichText::new(crate::i18n::tr(ui, "Use keys from the system SSH agent"))
                        .small()
                        .color(t.muted),
                );
            }
        }
    }

    fn show_serial(&mut self, ui: &mut egui::Ui, t: Theme) {
        let label = field_label(ui, t, "Serial Port");
        let mut refresh = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let width = ui.available_width();
            ui.allocate_ui_with_layout(
                vec2(width - 150.0, CONTROL_HEIGHT),
                egui::Layout::top_down(Align::Min),
                |ui| {
                    input(ui, t, &mut self.serial_port, "COM3 or /dev/ttyUSB0", false)
                        .labelled_by(label.id);
                },
            );
            ui.allocate_ui_with_layout(
                vec2(100.0, CONTROL_HEIGHT),
                egui::Layout::top_down(Align::Min),
                |ui| {
                    select(ui, "serial-ports", crate::i18n::tr(ui, "Ports"), |ui| {
                        for port in &self.ports {
                            ui.selectable_value(&mut self.serial_port, port.clone(), port);
                        }
                        if self.ports.is_empty() {
                            ui.label(crate::i18n::tr(ui, "No serial ports found"));
                        }
                    });
                },
            );
            let (slot, _) =
                ui.allocate_exact_size(vec2(CONTROL_HEIGHT, CONTROL_HEIGHT), Sense::hover());
            refresh =
                theme::icon_button(ui, slot, "Refresh serial ports", Icon::Refresh, t).clicked();
        });
        if refresh {
            self.refresh_ports();
        }

        ui.add_space(FIELD_GAP);
        field_pair(
            ui,
            132.0,
            |ui| {
                let label = field_label(ui, t, "Baud Rate");
                combo(ui, "baud", &mut self.baud, &BAUD_RATES).labelled_by(label.id);
            },
            |ui| {
                let label = field_label(ui, t, "Data Bits");
                combo(ui, "bits", &mut self.bits, &DATA_BITS).labelled_by(label.id);
            },
        );
    }

    fn refresh_ports(&mut self) {
        self.ports = serialport::available_ports()
            .unwrap_or_default()
            .into_iter()
            .map(|port| port.port_name)
            .collect();
    }

    fn refresh_keys(&mut self) {
        self.disk_keys = storage::list_keys()
            .into_iter()
            .map(|value| {
                (
                    value["name"].as_str().unwrap_or_default().to_owned(),
                    value["path"].as_str().unwrap_or_default().to_owned(),
                )
            })
            .collect();
    }
}

fn required(value: &str, label: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        Err(format!("{label} is required"))
    } else if crate::agent_upload::has_unsafe_text(value) {
        Err(format!(
            "{label} contains unsupported control or direction characters"
        ))
    } else {
        Ok(value.into())
    }
}

const FIELD_GAP: f32 = 14.0;
const PORT_WIDTH: f32 = 110.0;

fn field_pair(
    ui: &mut egui::Ui,
    right_width: f32,
    left: impl FnOnce(&mut egui::Ui),
    right: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal_top(|ui| {
        let gap = 12.0;
        ui.spacing_mut().item_spacing.x = gap;
        let left_width = ui.available_width() - right_width - gap;
        let column = egui::Layout::top_down(Align::Min);
        ui.allocate_ui_with_layout(vec2(left_width, 0.0), column, left);
        ui.allocate_ui_with_layout(vec2(right_width, 0.0), column, right);
    });
}
