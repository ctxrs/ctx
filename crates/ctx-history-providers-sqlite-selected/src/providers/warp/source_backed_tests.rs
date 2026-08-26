#[test]
fn direct_core_projection_is_complete_and_self_contained() {
    let sources = [include_str!("source_backed.rs")];
    let production = sources.join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production.contains("WARP_SOURCE_BACKED_PARSER_REVISION"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("event.lexical_body"));
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
fn root_scope_composes_with_warp_surface_and_preserves_unqualified_identity() {
    use ctx_history_core::{CaptureProvider, SourceAnchor, SourceAnchorScope, SourceKey, TypedKey};

    let selection = super::WarpSourceSelectionV0::new(
        "/tmp/warp-scope-data",
        "/tmp/warp-scope-data/warp.db",
        "stable-client-surface",
    )
    .unwrap();
    let released = SourceKey::derive(
        CaptureProvider::Warp.as_str(),
        crate::WARP_SQLITE_SOURCE_FORMAT,
        super::source_backed::WARP_SOURCE_SCHEMA_VARIANT,
        1,
        SourceAnchor::provider_native(
            super::source_backed::WARP_SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8(selection.surface_key()).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let unqualified =
        super::source_backed::warp_source_key_scoped(&selection, SourceAnchorScope::Unqualified)
            .unwrap();
    assert!(released.exact_descriptor_eq(&unqualified));
    assert_eq!(
        released.identity().encode_canonical().unwrap(),
        unqualified.identity().encode_canonical().unwrap()
    );

    let first = super::source_backed::warp_source_key_scoped(
        &selection,
        SourceAnchorScope::Lineage([0x11; 32]),
    )
    .unwrap();
    let second = super::source_backed::warp_source_key_scoped(
        &selection,
        SourceAnchorScope::Lineage([0x22; 32]),
    )
    .unwrap();
    assert_ne!(
        super::source_backed::warp_session_id(&first, "shared-conversation").unwrap(),
        super::source_backed::warp_session_id(&second, "shared-conversation").unwrap()
    );

    let sibling = super::WarpSourceSelectionV0::new(
        "/tmp/warp-scope-data",
        "/tmp/warp-scope-data/sibling.db",
        "sibling-client-surface",
    )
    .unwrap();
    let sibling = super::source_backed::warp_source_key_scoped(
        &sibling,
        SourceAnchorScope::Lineage([0x11; 32]),
    )
    .unwrap();
    assert_ne!(first.identity(), sibling.identity());
}
