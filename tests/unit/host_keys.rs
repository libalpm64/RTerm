use super::*;

#[test]
fn known_hosts_persist_and_require_the_expected_key_for_rotation() {
    let temp = crate::storage::test_private_tempdir();
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

#[test]
fn host_display_escapes_unicode_and_directional_controls() {
    assert_eq!(
        display_host("exаmple\u{202e}.com"),
        "ex\\u{0430}mple\\u{202E}.com"
    );
}

#[test]
fn batch_forget_checks_all_expected_keys_and_preserves_other_writes() {
    let temp = crate::storage::test_private_tempdir();
    let path = temp.path().join("known-hosts.json");
    let open = || Store {
        path: Some(path.clone()),
        memory: BTreeMap::new(),
    };
    let original: BTreeMap<_, _> = (0..10_000)
        .map(|i| {
            (
                target(&format!("host-{i:05}.example"), 22),
                Key {
                    algorithm: "ssh-ed25519".into(),
                    fingerprint: format!("SHA256:fixture-{i:05}"),
                },
            )
        })
        .collect();
    let mut store = open();
    store.save(original.clone()).unwrap();
    let mut selected: BTreeMap<_, _> = original
        .iter()
        .step_by(10)
        .map(|(host, key)| (host.clone(), key.clone()))
        .collect();
    let host = selected.keys().next().unwrap().clone();
    let replacement = Key {
        algorithm: "ssh-ed25519".into(),
        fingerprint: "SHA256:replacement".into(),
    };
    open()
        .change(&host, selected.get(&host), Some(replacement.clone()))
        .unwrap();
    let before = store.list().unwrap();
    assert!(store.forget_many(&selected).is_err());
    assert_eq!(store.list().unwrap(), before, "no partial removal");
    open().change(&host, Some(&replacement), None).unwrap();
    let before = store.list().unwrap();
    assert!(store.forget_many(&selected).is_err());
    assert_eq!(store.list().unwrap(), before);
    selected.remove(&host);
    let unrelated = target("newly-trusted.example", 2222);
    open()
        .change(&unrelated, None, Some(replacement.clone()))
        .unwrap();
    store.forget_many(&selected).unwrap();
    let entries = open().list().unwrap();
    assert_eq!(entries.len(), original.len() - selected.len());
    assert_eq!(entries[&unrelated], replacement);
    assert!(selected.keys().all(|host| !entries.contains_key(host)));
    for (host, key) in before {
        if !selected.contains_key(&host) {
            assert_eq!(entries[&host], key);
        }
    }
}

#[cfg(unix)]
#[test]
fn known_hosts_rejects_symlinks_and_hardlinks() {
    use std::os::unix::fs::symlink;

    let dir = crate::storage::test_private_tempdir();
    let path = dir.path().join("known-hosts.json");
    let target = dir.path().join("trusted.json");
    std::fs::write(&target, b"{}").unwrap();
    symlink(&target, &path).unwrap();
    let store = Store {
        path: Some(path.clone()),
        memory: BTreeMap::new(),
    };
    assert!(store.list().is_err());

    std::fs::remove_file(&path).unwrap();
    std::fs::hard_link(&target, &path).unwrap();
    assert!(store.list().is_err());
}
