use super::*;

#[test]
fn direct_core_projection_uses_neutral_v3_content() {
    let production = [
        include_str!("../../source_backed.rs"),
        include_str!("../projection.rs"),
    ]
    .join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("CoreActivity"));
    assert!(production.contains("ActivityJsonCapture"));
    assert!(production.contains("ProviderDeclaredFact"));
    assert!(production.contains("omit_structured_content_if_aggregate_exceeds_limit"));
    for forbidden in [
        concat!("Repository", "Attributor"),
        concat!("repository_", "bindings"),
        concat!("repository_", "abstentions"),
        concat!("result_", "outcome"),
        concat!("file_", "touches"),
    ] {
        assert!(!production.contains(forbidden), "found {forbidden}");
    }
}

#[test]
fn exact_provider_strings_are_not_trimmed_by_helpers() {
    assert_eq!(
        nonempty("  /literal/path  ".to_owned()).as_deref(),
        Some("  /literal/path  ")
    );
    assert_eq!(nonempty(String::new()), None);
}

#[test]
fn only_payload_content_size_failures_are_record_local() {
    assert!(record_local_core_projection_failure(
        &CoreRecordError::FieldTooLarge {
            field: "selected_content",
            actual: ctx_history_core::MAX_CORE_CONTENT_BYTES + 1,
            maximum: ctx_history_core::MAX_CORE_CONTENT_BYTES,
        }
    ));
    assert!(!record_local_core_projection_failure(
        &CoreRecordError::InvalidIdentityRelationship
    ));
    assert!(!record_local_core_projection_failure(
        &CoreRecordError::InvalidSessionRelationship
    ));
    assert!(!record_local_core_projection_failure(
        &CoreRecordError::InvalidActivity
    ));
}

#[test]
fn schema_transitions_separate_session_ids_without_weakening_provenance() {
    let dialect = &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT;
    let legacy_source = source_key_scoped(
        dialect,
        OpenCodeNativeSchemaFamily::MessagePart,
        SourceAnchorScope::Unqualified,
    )
    .unwrap();
    let released = ctx_history_core::derive_native_session_id(
        &legacy_source,
        "opencode-family-session",
        "opencode-family.session-id",
        TypedKey::utf8("same-session").unwrap(),
    )
    .unwrap();
    assert_eq!(
        session_id(&legacy_source, "same-session")
            .unwrap()
            .encode_canonical()
            .unwrap(),
        released.encode_canonical().unwrap()
    );
    let mut compact = std::collections::BTreeSet::new();
    for family in [
        OpenCodeNativeSchemaFamily::MessagePart,
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        OpenCodeNativeSchemaFamily::SessionMessageSynthesizedSeq,
        OpenCodeNativeSchemaFamily::SessionEntry,
        OpenCodeNativeSchemaFamily::LegacyMessage,
    ] {
        let source = source_key_scoped(dialect, family, SourceAnchorScope::Unqualified).unwrap();
        assert_eq!(source.identity(), legacy_source.identity());
        let session = session_id(&source, "same-session").unwrap();
        assert!(compact.insert(session.as_uuid()));
        assert_eq!(
            session.encode_canonical().unwrap(),
            session_id(&source, "same-session")
                .unwrap()
                .encode_canonical()
                .unwrap()
        );
        let item =
            NativeItemKey::native_id("message", TypedKey::utf8("message-1").unwrap()).unwrap();
        derive_event_id(EventIdentityInput {
            source: &source,
            session_id: session,
            logical_item_kind: "message",
            native_item_key: &item,
            subrecord_selector: None,
        })
        .unwrap();
        if family != OpenCodeNativeSchemaFamily::MessagePart {
            assert!(
                derive_event_id(EventIdentityInput {
                    source: &source,
                    session_id: released,
                    logical_item_kind: "message",
                    native_item_key: &item,
                    subrecord_selector: None,
                })
                .is_err(),
                "old provenance must still be rejected by the new descriptor"
            );
        }
    }
}
