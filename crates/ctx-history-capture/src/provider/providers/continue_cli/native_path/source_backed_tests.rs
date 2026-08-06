#[test]
fn direct_core_projection_is_complete_and_self_contained() {
    let sources = [include_str!("source_backed.rs")];
    let production = sources.join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production.contains("CONTINUE_SOURCE_BACKED_PARSER_REVISION"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("event.search_text.clone()"));
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
fn supported_sessions_remain_root_unknown_with_exact_native_content() {
    use super::super::normalize::{
        ContinueEventIdentity, ContinueEventKind, ContinueEventRole, ContinueEventRow,
        ContinueSessionIdentity, ContinueSessionRow,
    };

    let session_identity = ContinueSessionIdentity("continue-root".to_owned());
    let source = super::continue_source_key(&session_identity.0).unwrap();
    let session_id = super::continue_session_id(&source, &session_identity.0).unwrap();
    let session = ContinueSessionRow {
        identity: session_identity.clone(),
        title: Some("Continue root fixture".to_owned()),
        started_at: Some("2026-08-05T12:00:00Z".parse().unwrap()),
        workspace_directory: Some("/workspace/continue".to_owned()),
        mode: Some("chat".to_owned()),
        chat_model_title: Some("test-model".to_owned()),
        usage: None,
        index_metadata: None,
        metadata_json: "{}".to_owned(),
        metadata_hash: "continue-test-metadata".to_owned(),
    };
    let event = ContinueEventRow {
        identity: ContinueEventIdentity {
            session: session_identity,
            history_ordinal: 3,
        },
        native_item_id: Some("continue-event-3".to_owned()),
        kind: ContinueEventKind::Message,
        role: ContinueEventRole::User,
        occurred_at: Some("2026-08-05T12:00:01Z".parse().unwrap()),
        search_text: "exact persisted Continue event".to_owned(),
        calls: Vec::new().into_boxed_slice(),
        file_touches: Vec::new().into_boxed_slice(),
    };
    let record = super::project_bound_event(&source, session_id, [3; 32], &session, event).unwrap();

    assert_eq!(
        record.session_relationship,
        ctx_history_core::SessionRelationshipKind::Root
    );
    assert_eq!(record.event_origin, ctx_history_core::EventOrigin::Unknown);
    assert!(record.is_primary);
    assert_eq!(
        record.content.meaningful_text(),
        "exact persisted Continue event"
    );
    assert!(record.native_event_id.is_some());
}
