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
        match std::fs::read(path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|error| format!("Read known hosts: {error}"))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(format!("Read known hosts: {error}")),
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
mod tests {
    use super::*;

    #[test]
    fn known_hosts_persist_and_require_the_expected_key_for_rotation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("known-hosts.json");
        let open = || Store {
            path: Some(path.clone()),
            memory: BTreeMap::new(),
        };
        let host = target("SERVER.Example.", 2222);
        assert_eq!(host, target("server.example", 2222));
        assert_ne!(host, target("server.example", 22));
        assert_eq!(target("[::1]", 22), target("0:0:0:0:0:0:0:1", 22));
        let key = Key {
            algorithm: "ssh-ed25519".into(),
            fingerprint: "SHA256:fixture".into(),
        };
        open().change(&host, None, Some(key.clone())).unwrap();
        let mut reloaded = open();
        let mut next = key.clone();
        next.fingerprint = "SHA256:replacement".into();
        assert!(reloaded.change(&host, None, Some(next.clone())).is_err());
        assert_eq!(reloaded.list().unwrap()[&host], key);
        reloaded
            .change(&host, Some(&key), Some(next.clone()))
            .unwrap();
        assert_eq!(open().list().unwrap()[&host], next);
        reloaded.change(&host, Some(&next), None).unwrap();
        assert!(open().list().unwrap().is_empty());
    }
}
