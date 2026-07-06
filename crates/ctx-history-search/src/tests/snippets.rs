use super::{
    display_snippet, event_preview_text, fixed_time, sync_metadata, Event, EventRole, EventType,
    RedactionState, Uuid,
};

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
