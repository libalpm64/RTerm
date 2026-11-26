#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SshConfig {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) user: String,
    pub(crate) password: Option<String>,
    pub(crate) key_path: Option<String>,
    #[serde(default)]
    pub(crate) key_name: Option<String>,
    #[serde(default)]
    pub(crate) vault_pass: Option<String>,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) compression: Option<bool>,
    #[serde(default)]
    pub(crate) key_passphrase: Option<String>,
    #[serde(default)]
    pub(crate) use_agent: bool,
    #[serde(default)]
    pub(crate) keepalive: u64,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub(crate) struct TelnetConfig {
    pub(crate) host: String,
    pub(crate) port: u16,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub(crate) struct SerialConfig {
    pub(crate) port: String,
    pub(crate) baud: u32,
    #[serde(default = "default_serial_bits")]
    pub(crate) bits: u8,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct SshKey {
    pub(crate) name: String,
    pub(crate) private_key: String,
    pub(crate) public_key: String,
    #[serde(default)]
    pub(crate) key_type: String,
}

fn default_serial_bits() -> u8 {
    8
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum Connection {
    Local { shell: String },
    Ssh { config: SshConfig, terminal: String },
    Telnet(TelnetConfig),
    Serial(SerialConfig),
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Profile {
    pub(crate) name: String,
    pub(crate) connection: Connection,
}

impl Profile {
    pub(crate) fn kind(&self) -> crate::terminal::Kind {
        use crate::terminal::Kind;
        match self.connection {
            Connection::Local { .. } => Kind::Local,
            Connection::Ssh { .. } => Kind::Ssh,
            Connection::Telnet(_) => Kind::Telnet,
            Connection::Serial(_) => Kind::Serial,
        }
    }

    pub(crate) fn subtitle(&self) -> String {
        match &self.connection {
            Connection::Local { shell } if !shell.is_empty() => shell.clone(),
            Connection::Local { .. } => "Local Shell".into(),
            Connection::Ssh { config, .. } => {
                format!("{}@{}:{}", config.user, config.host, config.port)
            }
            Connection::Telnet(config) => format!("{}:{}", config.host, config.port),
            Connection::Serial(config) => format!("{} @ {}", config.port, config.baud),
        }
    }

    pub(crate) fn without_credentials(&self) -> Self {
        let mut profile = self.clone();
        if let Connection::Ssh { config, .. } = &mut profile.connection {
            config.password = None;
            config.key_passphrase = None;
            config.vault_pass = None;
        }
        profile
    }
}

impl From<SshConfig> for Profile {
    fn from(mut config: SshConfig) -> Self {
        let name = config
            .name
            .take()
            .unwrap_or_else(|| format!("{}@{}", config.user, config.host));
        config.vault_pass = None;
        Self {
            name,
            connection: Connection::Ssh {
                config,
                terminal: "xterm-256color".into(),
            },
        }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct AgentAuditEntry {
    pub(crate) id: String,
    pub(crate) operation: String,
    pub(crate) session: String,
    pub(crate) session_name: String,
    pub(crate) action: String,
    pub(crate) command: String,
    pub(crate) submitted_ms: i64,
    pub(crate) finished_ms: Option<i64>,
    pub(crate) phase: String,
    pub(crate) exit_code: Option<i64>,
    #[serde(default)]
    pub(crate) duration_ms: Option<u64>,
    pub(crate) detail: Option<String>,
}
