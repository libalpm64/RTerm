use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Key {
    pub(crate) algorithm: String,
    pub(crate) fingerprint: String,
}

impl Key {
    pub(crate) fn presented(key: &russh::keys::PublicKeyOrCertificate) -> Self {
        let key = key.public_key();
        Self {
            algorithm: key.algorithm().to_string(),
            fingerprint: key.fingerprint(ssh_key::HashAlg::Sha256).to_string(),
        }
    }
}

pub(crate) fn target(host: &str, port: u16) -> String {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    if let Ok(address) = host.parse::<std::net::IpAddr>() {
        return format!("[{address}]:{port}");
    }
    format!(
        "[{}]:{port}",
        host.trim_end_matches('.').to_ascii_lowercase()
    )
}

pub(crate) fn display_host(host: &str) -> String {
    host.chars()
        .map(|character| match character {
            '\\' => "\\\\".into(),
            character
                if character.is_ascii_graphic()
                    && !crate::agent_upload::is_unsafe_char(character) =>
            {
                character.to_string()
            }
            character => format!("\\u{{{:04X}}}", character as u32),
        })
        .collect()
}

pub(crate) struct Store {
    path: Option<PathBuf>,
    memory: BTreeMap<String, Key>,
}

impl Store {
    pub(crate) fn new(persistent: bool) -> Self {
        Self {
            path: persistent.then(|| crate::storage::get_config_dir().join("known-hosts.json")),
            memory: BTreeMap::new(),
        }
    }

    pub(crate) fn list(&self) -> Result<BTreeMap<String, Key>, String> {
        let Some(path) = &self.path else {
            return Ok(self.memory.clone());
        };
        match crate::storage::read_private_config_file(path, 16 * 1024 * 1024)
            .map_err(|error| format!("Read known hosts: {error}"))?
        {
            Some(bytes) => {
                serde_json::from_slice(&bytes).map_err(|error| format!("Read known hosts: {error}"))
            }
            None => Ok(BTreeMap::new()),
        }
    }

    pub(crate) fn change(
        &mut self,
        host: &str,
        expected: Option<&Key>,
        key: Option<Key>,
    ) -> Result<(), String> {
        let _lock = self
            .path
            .as_deref()
            .map(crate::storage::lock_storage)
            .transpose()?;
        let mut entries = self.list()?;
        if entries.get(host) != expected {
            return Err("The trusted key changed. Review the current key and try again.".into());
        }
        if let Some(key) = key {
            entries.insert(host.into(), key);
        } else {
            entries.remove(host);
        }
        self.save(entries)
    }

    // Check the entire reviewed selection against the current file before
    // changing anything. A concurrent rotation/removal must not cause us to
    // forget a different key, nor partially apply a multi-host request.
    pub(crate) fn forget_many(&mut self, expected: &BTreeMap<String, Key>) -> Result<(), String> {
        if expected.is_empty() {
            return Ok(());
        }
        let _lock = self
            .path
            .as_deref()
            .map(crate::storage::lock_storage)
            .transpose()?;
        let mut entries = self.list()?;
        if expected
            .iter()
            .any(|(host, key)| entries.get(host) != Some(key))
        {
            return Err(
                "A selected trusted key changed. Review the current keys and try again.".into(),
            );
        }
        for host in expected.keys() {
            entries.remove(host);
        }
        self.save(entries)
    }

    fn save(&mut self, entries: BTreeMap<String, Key>) -> Result<(), String> {
        if let Some(path) = &self.path {
            let bytes = serde_json::to_vec_pretty(&entries).map_err(|error| error.to_string())?;
            crate::storage::write_atomic(path, &bytes)?;
        } else {
            self.memory = entries;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/unit/host_keys.rs"]
mod tests;
