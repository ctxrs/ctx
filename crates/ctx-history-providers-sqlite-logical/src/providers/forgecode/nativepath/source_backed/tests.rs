#[test]
fn direct_core_projection_is_complete_and_self_contained() {
    let sources = [include_str!("../source_backed.rs")];
    let production = sources.join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production.contains("FORGECODE_SOURCE_BACKED_PARSER_REVISION"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("let lexical_text = retained"));
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
fn root_scope_composes_with_forgecode_authority_and_preserves_unqualified_identity() {
    use ctx_history_core::{SourceAnchor, SourceAnchorScope, SourceKey, TypedKey};

    let data_root = std::path::Path::new("/tmp/forgecode-scope-data");
    let path = "/tmp/forgecode-scope-data/.forge.db";
    let released = SourceKey::derive(
        super::FORGECODE_PROVIDER_ID,
        super::FORGECODE_SQLITE_SOURCE_FORMAT,
        super::FORGECODE_SOURCE_SCHEMA_VARIANT,
        1,
        SourceAnchor::provider_native(
            super::FORGECODE_SELECTED_SOURCE_NAMESPACE,
            TypedKey::utf8(super::FORGECODE_SELECTED_SOURCE_KEY).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let unqualified = super::ForgeCodeSourceSelectionV0::selected(data_root, path)
        .source_key()
        .unwrap();
    assert!(released.exact_descriptor_eq(&unqualified));
    assert_eq!(
        released.identity().encode_canonical().unwrap(),
        unqualified.identity().encode_canonical().unwrap()
    );

    let scoped = |root| {
        super::ForgeCodeSourceSelectionV0::selected_scoped(
            data_root,
            path,
            SourceAnchorScope::Lineage(root),
        )
        .source_key()
        .unwrap()
    };
    let first = scoped([0x11; 32]);
    let second = scoped([0x22; 32]);
    assert_ne!(
        super::forgecode_session_id(&first, "shared-conversation").unwrap(),
        super::forgecode_session_id(&second, "shared-conversation").unwrap()
    );

    let explicit_lineage = [0x33; 32];
    let released_explicit = SourceKey::derive(
        super::FORGECODE_PROVIDER_ID,
        super::FORGECODE_SQLITE_SOURCE_FORMAT,
        super::FORGECODE_SOURCE_SCHEMA_VARIANT,
        1,
        SourceAnchor::CatalogLineage(explicit_lineage),
    )
    .unwrap();
    let unqualified_explicit = super::ForgeCodeSourceSelectionV0::explicit_scoped(
        data_root,
        path,
        explicit_lineage,
        SourceAnchorScope::Unqualified,
    )
    .source_key()
    .unwrap();
    assert!(released_explicit.exact_descriptor_eq(&unqualified_explicit));

    let sibling = super::ForgeCodeSourceSelectionV0::explicit_scoped(
        data_root,
        path,
        explicit_lineage,
        SourceAnchorScope::Lineage([0x11; 32]),
    )
    .source_key()
    .unwrap();
    assert_ne!(first.identity(), sibling.identity());
}

#[test]
fn supported_conversations_are_primary_with_exact_native_content() {
    let selection = super::ForgeCodeSourceSelectionV0::selected(
        std::path::Path::new("/tmp/forgecode-test-data"),
        "/tmp/forgecode-test-data/.forge.db",
    );
    let source = super::ForgeCodeSourceBackedSourceV0 {
        source: selection.source_key().unwrap(),
        canonical_path: "/tmp/forgecode-test-data/.forge.db".into(),
    };
    let row = super::ForgeCodeConversationRow {
        rowid: 1,
        source_record_digest: [1; 32],
        canonical_record_bytes: 128,
        conversation_id: "forgecode-root".to_owned(),
        title: Some("ForgeCode root fixture".to_owned()),
        workspace_id: 7,
        created_at: "2026-08-05T12:00:00Z".to_owned(),
        updated_at: Some("2026-08-05T12:00:01Z".to_owned()),
        context_metadata: serde_json::json!({
            "metadata": {
                "path": "src/context-decoy.rs",
                "branch": "context-decoy"
            }
        }),
        metrics_metadata: None,
    };
    let retained = super::super::source::ForgeCodeRetainedEvent {
        event: super::super::super::event::forgecode_event(
            "forgecode-root",
            &serde_json::json!({
                "message": {
                    "text": {
                        "role": "user",
                        "content": "exact persisted ForgeCode event",
                        "metadata": {
                            "path": "src/body-decoy.rs",
                            "commit": "body-decoy"
                        }
                    }
                }
            }),
            1,
            "2026-08-05T12:00:01Z".parse().unwrap(),
        ),
        provider_event_index: 1,
    };
    let record = super::core_record(&source, &row, retained).unwrap();

    assert_eq!(
        record.agent_scope,
        Some(ctx_history_core::AgentScope::Primary)
    );
    assert_eq!(record.session_relationship, None);
    assert_eq!(record.root_session_id, None);
    assert_eq!(
        record.content.meaningful_text(),
        "exact persisted ForgeCode event"
    );
    assert!(record.native_event_id.is_some());
    let facts = &record.content.activity.as_ref().unwrap().facts;
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].kind, ctx_history_core::LiteralFactKind::Workspace);
    assert_eq!(facts[0].value, "7");
}

#[test]
fn optional_activity_metadata_abstains_without_altering_exact_result_content() {
    let oversized = "x".repeat(64 * 1024 + 1);
    let invalid_call = serde_json::json!({
        "text": {"tool_calls": [{"call_id": oversized, "name": "shell"}]},
    });
    assert_eq!(
        super::forgecode_activity(&invalid_call, ctx_history_core::EventType::ToolCall, 0).unwrap(),
        (None, None, None)
    );

    let invalid_tool = serde_json::json!({
        "text": {"tool_calls": [{
            "call_id": "call-1",
            "name": "x".repeat(64 * 1024 + 1),
        }]},
    });
    assert_eq!(
        super::forgecode_activity(&invalid_tool, ctx_history_core::EventType::ToolCall, 0).unwrap(),
        (None, None, None)
    );

    let exact_body = serde_json::json!({
        "call_id": "call-1",
        "status": "x".repeat(64 * 1024 + 1),
        "output": {"exact": true},
    });
    let output = serde_json::json!({"tool": exact_body.clone()});
    let (call_id, invocation, result) =
        super::forgecode_activity(&output, ctx_history_core::EventType::ToolOutput, 1).unwrap();
    assert_eq!(
        call_id,
        Some(ctx_history_core::TypedKey::Utf8("call-1".to_owned()))
    );
    assert!(invocation.is_none());
    let result = result.unwrap();
    assert_eq!(result.status, None);
    assert_eq!(
        result.structured_content,
        ctx_history_core::ActivityJsonCapture::Present { value: exact_body }
    );
}
