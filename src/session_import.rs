use std::{
    collections::HashSet,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use crate::models::{Connection, Profile, SshConfig, TelnetConfig};

const MAX_EXPORT_BYTES: u64 = 16 * 1024 * 1024;
const UNSUPPORTED_FORMAT: &str = "Unrecognized session format. Choose a Netcatty CSV export.";

pub(crate) struct Import {
    pub(crate) format: &'static str,
    pub(crate) profiles: Vec<Profile>,
    pub(crate) skipped: usize,
}

pub(crate) struct Summary {
    pub(crate) format: &'static str,
    pub(crate) added: usize,
    pub(crate) duplicates: usize,
    pub(crate) skipped: usize,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Host<'a> {
    #[serde(rename = "Hostname/IP")]
    hostname: &'a str,
    label: &'a str,
    protocol: &'a str,
    #[serde(default)]
    port: &'a str,
    #[serde(default)]
    username: &'a str,
    #[serde(default)]
    password: &'a str,
    #[serde(default)]
    key_path: &'a str,
    #[serde(default)]
    passphrase: &'a str,
    #[serde(default)]
    proxy: &'a str,
    #[serde(default)]
    proxy_jump: &'a str,
}

impl Host<'_> {
    fn into_profile(self) -> Option<Profile> {
        if !self.proxy_jump.trim().is_empty()
            || (!self.proxy.trim().is_empty() && !self.proxy.trim().eq_ignore_ascii_case("direct"))
        {
            return None;
        }
        let host = self.hostname.trim();
        let host = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        if host.is_empty()
            || host.chars().any(|ch| ch.is_whitespace() || ch.is_control())
            || crate::agent_upload::has_unsafe_text(host)
            || host.contains(['/', '\\', '@', '[', ']'])
            || (host.contains(':')
                && host
                    .split('%')
                    .next()?
                    .parse::<std::net::Ipv6Addr>()
                    .is_err())
        {
            return None;
        }
        let protocol = self.protocol.trim();
        let ssh = protocol.is_empty() || protocol.eq_ignore_ascii_case("ssh");
        if !ssh && !protocol.eq_ignore_ascii_case("telnet") {
            return None;
        }
        let port = match self.port.trim() {
            "" if ssh => 22,
            "" => 23,
            port => port.parse::<u16>().ok().filter(|&port| port != 0)?,
        };
        let user = unguard(self.username).trim();
        if ssh && (user.is_empty() || crate::agent_upload::has_unsafe_text(user)) {
            return None;
        }
        let label = unguard(self.label).trim();
        let name = if label.is_empty() && ssh {
            format!("{user}@{host}")
        } else if label.is_empty() {
            host.to_owned()
        } else {
            label.replace(char::is_control, " ")
        };
        let connection = if ssh {
            let key_path = decode_field(self.key_path.trim(), "__netcatty_csv_keypath_v1__:")
                .filter(|path| !path.contains('\0'))
                .map(|path| {
                    crate::storage::expand_local_path(&path)
                        .to_string_lossy()
                        .into_owned()
                });
            let key_passphrase = key_path.as_ref().and_then(|_| {
                decode_field(self.passphrase, "__netcatty_csv_passphrase_v1__:")
                    .filter(|value| !value.starts_with("enc:v1:"))
            });
            Connection::Ssh {
                config: SshConfig {
                    host: host.into(),
                    port,
                    user: user.into(),
                    password: (!self.password.is_empty() && !self.password.starts_with("enc:v1:"))
                        .then(|| self.password.to_owned()),
                    key_path,
                    key_name: None,
                    vault_pass: None,
                    name: None,
                    compression: None,
                    key_passphrase,
                    use_agent: false,
                    keepalive: 0,
                },
                terminal: "xterm-256color".into(),
            }
        } else {
            Connection::Telnet(TelnetConfig {
                host: host.into(),
                port,
            })
        };
        Some(Profile { name, connection })
    }
}

fn decode_field(value: &str, marker: &str) -> Option<String> {
    if let Some(encoded) = value.strip_prefix(marker) {
        percent_encoding::percent_decode_str(encoded)
            .decode_utf8()
            .ok()
            .filter(|value| !value.is_empty())
            .map(|value| value.into_owned())
    } else {
        (!value.is_empty()).then(|| value.to_owned())
    }
}

pub(crate) fn latest_export(directory: &Path) -> Option<PathBuf> {
    directory
        .read_dir()
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            if !name.starts_with("hosts_export_")
                || !name.ends_with(".csv")
                || !entry.file_type().ok()?.is_file()
            {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            (metadata.len() <= MAX_EXPORT_BYTES)
                .then_some((metadata.modified().ok()?, entry.path()))
        })
        .max()
        .map(|(_, path)| path)
}

// Netcatty prefixes spreadsheet-formula characters in labels and usernames.
fn unguard(value: &str) -> &str {
    value
        .strip_prefix('\'')
        .filter(|value| value.starts_with(['=', '+', '-', '@', '\t', '\r']))
        .unwrap_or(value)
}

pub(crate) fn read(path: &Path) -> Result<Import, String> {
    let metadata = path.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_EXPORT_BYTES {
        return Err("Choose a session export file no larger than 16 MiB.".into());
    }
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    File::open(path)
        .and_then(|file| file.take(MAX_EXPORT_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_EXPORT_BYTES {
        return Err("Choose a session export file no larger than 16 MiB.".into());
    }
    parse(&bytes)
}

fn parse(bytes: &[u8]) -> Result<Import, String> {
    let mut reader = csv::Reader::from_reader(bytes);
    let headers = reader.headers().map_err(|_| UNSUPPORTED_FORMAT)?.clone();
    if !["Hostname/IP", "Protocol", "Label"]
        .iter()
        .all(|required| headers.iter().any(|header| header == *required))
    {
        return Err(UNSUPPORTED_FORMAT.into());
    }
    let mut profiles = Vec::new();
    let mut skipped = 0;
    for record in reader.records() {
        if let Some(profile) = record.ok().and_then(|record| {
            record
                .deserialize::<Host<'_>>(Some(&headers))
                .ok()?
                .into_profile()
        }) {
            profiles.push(profile);
        } else {
            skipped += 1;
        }
    }
    Ok(Import {
        format: "Netcatty CSV",
        profiles,
        skipped,
    })
}

pub(crate) fn append_unique(saved: &mut Vec<Profile>, incoming: Vec<Profile>) -> usize {
    fn identity(profile: &Profile) -> Option<(String, String, Option<String>)> {
        let (host, port, user) = match &profile.connection {
            Connection::Ssh { config, .. } => {
                (config.host.as_str(), config.port, Some(config.user.clone()))
            }
            Connection::Telnet(config) => (config.host.as_str(), config.port, None),
            _ => return None,
        };
        Some((
            profile.name.clone(),
            crate::host_keys::target(host, port),
            user,
        ))
    }

    let mut known: HashSet<_> = saved.iter().filter_map(identity).collect();
    let previous = saved.len();
    saved.extend(
        incoming
            .into_iter()
            .filter(|profile| identity(profile).is_some_and(|key| known.insert(key))),
    );
    saved.len() - previous
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "\u{feff}Groups,Label,Tags,Notes,Hostname/IP,Protocol,Port,Username,Password,KeyPath,Passphrase,Proxy,ProxyJump,LocalShell,SshInputCompatibility\r\n";

    #[test]
    fn netcatty_export_imports_supported_endpoints_and_ssh_credentials() {
        let csv = format!(
            "{HEADER}Team,\"日本語, server\",,\"First line\r\nSecond \"\"quoted\"\" line\",[2001:db8::1],ssh,2222,alice,secret,~/.ssh/id_ed25519,__netcatty_csv_passphrase_v1__:%2Bpass,direct,,,true\r\n\
             ,Console,,,console.example,telnet,,,,,,,,,\r\n\
             ,Jump,,,inside.example,ssh,22,alice,,,,,bastion.example,,true\r\n\
             ,Proxy,,,proxy.example,ssh,22,alice,,,,socks5://proxy.example:1080,,,true\r\n\
             ,Invalid,,,bad.example,ssh,65536,alice,,,,,,,true\r\n\
             ,Local,,,localhost,local,22,,,,,,,,\r\n"
        );
        let imported = parse(csv.as_bytes()).unwrap();
        assert_eq!(imported.skipped, 4);
        assert_eq!(imported.profiles.len(), 2);
        assert_eq!(imported.profiles[0].name, "日本語, server");
        let Connection::Ssh { config, .. } = &imported.profiles[0].connection else {
            panic!("Expected SSH profile");
        };
        assert_eq!(
            (&*config.host, config.port, &*config.user),
            ("2001:db8::1", 2222, "alice")
        );
        assert_eq!(config.password.as_deref(), Some("secret"));
        assert_eq!(config.key_passphrase.as_deref(), Some("+pass"));
        assert_eq!(
            config.key_path.as_deref().map(Path::new),
            Some(crate::storage::home_dir().join(".ssh/id_ed25519").as_path())
        );
        let Connection::Telnet(config) = &imported.profiles[1].connection else {
            panic!("Expected Telnet profile");
        };
        assert_eq!((&*config.host, config.port), ("console.example", 23));
        assert!(parse(b"Host,Name\nexample.test,Example\n").is_err());
    }

    #[test]
    fn repeat_import_matches_canonical_addresses_and_preserves_existing_profiles() {
        let active =
            b"Label,Hostname/IP,Protocol,Port,Username\nProduction,SERVER.example.,ssh,22,alice\n";
        let mut saved = parse(active).unwrap().profiles;
        let Connection::Ssh { config, .. } = &mut saved[0].connection else {
            panic!("Expected SSH profile");
        };
        config.use_agent = true;

        let csv = b"Label,Hostname/IP,Protocol,Username\nProduction,server.example,ssh,alice\nProduction,server.example,ssh,bob\nProduction,server.example,ssh,bob\n";
        assert_eq!(append_unique(&mut saved, parse(csv).unwrap().profiles), 1);
        assert_eq!(append_unique(&mut saved, parse(csv).unwrap().profiles), 0);
        assert_eq!(saved.len(), 2);
        assert!(matches!(&saved[0].connection, Connection::Ssh { config, .. } if config.use_agent));
    }

    #[test]
    fn imported_hosts_reject_bidirectional_format_controls() {
        let csv = "Label,Hostname/IP,Protocol,Username\nSpoofed,example\u{202e}.com,ssh,alice\n";
        let imported = parse(csv.as_bytes()).unwrap();
        assert!(imported.profiles.is_empty());
        assert_eq!(imported.skipped, 1);
    }

    #[test]
    fn detects_exports_by_netcattys_download_filename() {
        let directory = tempfile::tempdir().unwrap();
        let export = directory.path().join("hosts_export_2026-11-26.csv");
        std::fs::write(&export, HEADER).unwrap();
        std::fs::write(directory.path().join("unrelated.csv"), "unrelated").unwrap();
        assert_eq!(latest_export(directory.path()), Some(export));
    }

    #[test]
    fn detects_the_session_format_from_contents_instead_of_the_filename() {
        let directory = tempfile::tempdir().unwrap();
        let renamed = directory.path().join("renamed.data");
        std::fs::write(&renamed, HEADER).unwrap();
        assert_eq!(read(&renamed).unwrap().format, "Netcatty CSV");

        let unrelated = directory.path().join("hosts_export_2026-11-26.csv");
        std::fs::write(&unrelated, "Name,Address\nExample,example.test\n").unwrap();
        assert_eq!(read(&unrelated).err().as_deref(), Some(UNSUPPORTED_FORMAT));
    }
}
