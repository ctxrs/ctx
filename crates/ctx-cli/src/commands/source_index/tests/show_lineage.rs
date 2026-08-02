use super::*;

#[test]
fn machine_show_contract_keeps_lineage_in_existing_nested_fields() {
    let mut direct = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 51, 1);
    let parent = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 52, 1);
    let root = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 53, 1);
    direct.parent_session_id = Some(parent.session_id);
    direct.root_session_id = root.session_id;
    direct.agent_type = "subagent".to_owned();
    direct.is_primary = false;
    let event = fixture_core_event(&direct, "nested lineage event");
    let session = SessionRecord::from(&direct);

    let session_value = session_transcript_value(
        &session,
        TranscriptMode::Log,
        OutputFormat::Json,
        vec![render_event_value(&event)],
        false,
        None,
    );
    assert_eq!(
        sorted_json_keys(&session_value),
        vec![
            "ctx_session_id",
            "events",
            "format",
            "mode",
            "payload_type",
            "provider",
            "provider_session_id",
            "schema_version",
            "session",
            "target",
        ]
    );
    assert!(session_value.get("parent_ctx_session_id").is_none());
    assert!(session_value.get("root_ctx_session_id").is_none());
    assert_eq!(
        session_value["session"]["parent_ctx_session_id"],
        parent.session_id.as_uuid().to_string()
    );
    assert_eq!(
        session_value["session"]["root_ctx_session_id"],
        root.session_id.as_uuid().to_string()
    );

    let event_value =
        event_window_value(&event, OutputFormat::Json, vec![render_event_value(&event)]).unwrap();
    assert_eq!(
        sorted_json_keys(&event_value),
        vec![
            "ctx_event_id",
            "ctx_session_id",
            "event",
            "events",
            "format",
            "payload_type",
            "schema_version",
            "target",
        ]
    );
    assert!(event_value.get("parent_ctx_session_id").is_none());
    assert!(event_value.get("root_ctx_session_id").is_none());
    assert_eq!(
        event_value["event"]["parent_ctx_session_id"],
        parent.session_id.as_uuid().to_string()
    );
    assert_eq!(
        event_value["event"]["root_ctx_session_id"],
        root.session_id.as_uuid().to_string()
    );
}
