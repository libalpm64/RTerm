use super::*;

#[test]
fn tunnel_draft_reuses_saved_key_until_explicitly_replaced() {
    let original = "sk-fixture-saved-runtime-key";
    let replacement = "sk-fixture-new-runtime-key";
    let saved = tunnel::Profile {
        id: "tunnel_0123456789abcdef0123456789abcdef".into(),
        api_key: original.into(),
        client: Default::default(),
    };
    let mut draft = Draft::new(Some(&saved));
    assert!(draft.key.is_empty());
    assert_eq!(
        draft.profile(Some(&saved)).unwrap().unwrap().api_key,
        original
    );

    draft.replacing_key = true;
    *draft.key = replacement.into();
    let updated = draft.profile(Some(&saved)).unwrap().unwrap();
    assert_eq!(updated.api_key, replacement);

    draft.clear_key();
    assert!(!draft.replacing_key);
    assert!(draft.key.is_empty());
    assert_eq!(
        draft.profile(Some(&updated)).unwrap().unwrap().api_key,
        replacement
    );
}
