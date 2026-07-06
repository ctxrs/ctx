use super::*;

#[test]
fn local_snippets_preserve_transcript_text() {
    let snippet = display_snippet(
        "token=ghp_1234567890abcdef1234567890abcdef and password=hunter2",
        200,
    );

    assert!(snippet.contains("token=ghp_1234567890abcdef1234567890abcdef"));
    assert!(snippet.contains("password=hunter2"));
    assert!(!snippet.contains("[REDACTED"));
}

#[test]
fn withheld_events_do_not_render_payload_previews() {
    let event = Event {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000010").unwrap(),
        seq: 1,
        history_record_id: None,
        session_id: None,
        run_id: None,
        event_type: EventType::Message,
        role: Some(EventRole::Assistant),
        occurred_at: fixed_time(),
        capture_source_id: None,
        payload: serde_json::json!({"text": "secret payload that must not render"}),
        payload_blob_id: None,
        dedupe_key: None,
        redaction_state: RedactionState::Withheld,
        sync: sync_metadata(),
    };

    let preview = event_preview_text(&event);
    assert_eq!(preview, "raw event payload withheld");
    assert!(!preview.contains("secret payload"));
}

#[test]
fn excluded_provider_session_matches_provider_external_id_for_hits() {
    let filters = excluded_filter(None);
    let hit = HitMetadata {
        provider: Some(CaptureProvider::Codex),
        provider_session_id: Some("provider-session-1".into()),
        ..empty_hit(fixed_time())
    };
    assert!(hit_matches_excluded_provider_session(&hit, &filters));

    let event_hit = EventSearchHit {
        event_id: Uuid::parse_str("018f45d0-0000-7000-8000-000000001001").unwrap(),
        history_record_id: None,
        session_id: None,
        session_parent_session_id: None,
        session_root_session_id: None,
        run_id: None,
        seq: 1,
        event_type: EventType::Message,
        role: Some(EventRole::User),
        occurred_at: fixed_time(),
        preview: "synthetic preview".into(),
        score: 1.0,
        provider: Some(CaptureProvider::Codex),
        session_external_session_id: Some("provider-session-1".into()),
        history_source: None,
        history_source_plugin: None,
        provider_key: None,
        source_id: None,
        source_format: None,
        agent_type: Some(AgentType::Primary),
        session_is_primary: Some(true),
        cwd: None,
        raw_source_path: None,
        cursor: None,
        record_title: None,
        record_kind: None,
        record_workspace: None,
    };
    assert!(event_hit_matches_excluded_provider_session(
        &event_hit, &filters
    ));

    let mut different_provider = event_hit;
    different_provider.provider = Some(CaptureProvider::Claude);
    assert!(!event_hit_matches_excluded_provider_session(
        &different_provider,
        &filters
    ));
}

#[test]
fn excluded_provider_session_matches_parent_and_root_session_tree() {
    let excluded_session_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000001100").unwrap();
    let child_session_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000001101").unwrap();
    let grandchild_session_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000001102").unwrap();
    let filters = excluded_filter(Some(excluded_session_id));

    let parent_hit = HitMetadata {
        session_id: Some(child_session_id),
        parent_session_id: Some(excluded_session_id),
        ..empty_hit(fixed_time())
    };
    assert!(hit_matches_excluded_provider_session(&parent_hit, &filters));

    let root_event_hit = EventSearchHit {
        event_id: Uuid::parse_str("018f45d0-0000-7000-8000-000000001103").unwrap(),
        history_record_id: None,
        session_id: Some(grandchild_session_id),
        session_parent_session_id: Some(child_session_id),
        session_root_session_id: Some(excluded_session_id),
        run_id: None,
        seq: 1,
        event_type: EventType::Message,
        role: Some(EventRole::Assistant),
        occurred_at: fixed_time(),
        preview: "synthetic preview".into(),
        score: 1.0,
        provider: None,
        session_external_session_id: None,
        history_source: None,
        history_source_plugin: None,
        provider_key: None,
        source_id: None,
        source_format: None,
        agent_type: Some(AgentType::Subagent),
        session_is_primary: Some(false),
        cwd: None,
        raw_source_path: None,
        cursor: None,
        record_title: None,
        record_kind: None,
        record_workspace: None,
    };
    assert!(event_hit_matches_excluded_provider_session(
        &root_event_hit,
        &filters
    ));

    let context = RecordContext {
        sessions: vec![Session {
            id: grandchild_session_id,
            history_record_id: None,
            parent_session_id: Some(child_session_id),
            root_session_id: Some(excluded_session_id),
            capture_source_id: None,
            provider: CaptureProvider::Claude,
            external_session_id: Some("different-provider-session".into()),
            external_agent_id: None,
            agent_type: AgentType::Subagent,
            role_hint: None,
            is_primary: false,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: fixed_time(),
            ended_at: None,
            timestamps: timestamps(),
            sync: sync_metadata(),
        }],
        ..RecordContext::default()
    };
    assert!(context_has_excluded_provider_session(&context, &filters));
}
