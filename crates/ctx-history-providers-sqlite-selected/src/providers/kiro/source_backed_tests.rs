#[test]
fn direct_core_projection_is_complete_and_self_contained() {
    let sources = [include_str!("source_backed.rs")];
    let production = sources.join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production.contains("KIRO_SOURCE_BACKED_PARSER_REVISION"));
    assert!(production.contains("agent_scope = Some(AgentScope::Primary)"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("let body = complete_text"));
    for removed_api in [
        concat!("Lexical", "Document"),
        concat!("SourceRecord", "Locator"),
        concat!("hyd", "rate_"),
        concat!("resol", "ver"),
    ] {
        assert!(!production.contains(removed_api), "found {removed_api}");
    }
    assert!(!production.contains("body.truncate"));
    assert!(!production.contains("body.chars().take"));
}

#[test]
fn root_scope_separates_identical_kiro_conversations_and_unqualified_is_released() {
    use ctx_history_core::{
        derive_session_id, CaptureProvider, NativeSessionKey, SessionIdentityInput, SourceAnchor,
        SourceAnchorScope, SourceKey, TypedKey,
    };

    let released = SourceKey::derive(
        CaptureProvider::KiroCli.as_str(),
        crate::KIRO_SQLITE_SOURCE_FORMAT,
        super::KIRO_SOURCE_SCHEMA_VARIANT,
        1,
        SourceAnchor::provider_native(
            super::KIRO_SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8(super::KIRO_SOURCE_ANCHOR_KEY).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let unqualified = super::kiro_source_key_scoped(SourceAnchorScope::Unqualified).unwrap();
    assert!(released.exact_descriptor_eq(&unqualified));
    assert_eq!(
        released.identity().encode_canonical().unwrap(),
        unqualified.identity().encode_canonical().unwrap()
    );

    let first = super::kiro_source_key_scoped(SourceAnchorScope::Lineage([0x11; 32])).unwrap();
    let second = super::kiro_source_key_scoped(SourceAnchorScope::Lineage([0x22; 32])).unwrap();
    let native = NativeSessionKey::composite(
        super::KIRO_NATIVE_SESSION_NAMESPACE,
        vec![
            TypedKey::utf8("conversations").unwrap(),
            TypedKey::utf8("shared-row").unwrap(),
            TypedKey::utf8("shared-conversation").unwrap(),
        ],
    )
    .unwrap();
    let session = |source| {
        derive_session_id(SessionIdentityInput {
            source,
            logical_session_kind: super::KIRO_LOGICAL_SESSION_KIND,
            native_session_key: &native,
        })
        .unwrap()
    };
    assert_ne!(session(&first), session(&second));
}

#[cfg(unix)]
#[path = "source_backed/tests/temp_authority.rs"]
mod temp_authority;
