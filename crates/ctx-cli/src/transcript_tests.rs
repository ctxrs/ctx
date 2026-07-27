use super::*;
use chrono::{DateTime, Utc};
use ctx_history_core::{Fidelity, SyncMetadata, SyncState, Visibility};

fn test_event() -> Event {
    Event {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000010").unwrap(),
        seq: 1,
        history_record_id: None,
        session_id: None,
        run_id: None,
        event_type: EventType::Message,
        role: Some(EventRole::User),
        occurred_at: DateTime::parse_from_rfc3339("2026-06-23T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        capture_source_id: None,
        payload: json!({"text": "local show payload should render"}),
        payload_blob_id: None,
        dedupe_key: None,
        sync: SyncMetadata {
            visibility: Visibility::LocalOnly,
            fidelity: Fidelity::Imported,
            sync_state: SyncState::LocalOnly,
            sync_version: 0,
            deleted_at: None,
            metadata: json!({}),
        },
    }
}

#[test]
fn event_content_preserves_payload_text() {
    let event = test_event();

    let content = event_content(&event);
    let preview = event_preview(&event);

    assert!(content.contains("local show payload should render"));
    assert!(preview.contains("local show payload"));
}

fn truncated_content(complete_content_available: bool) -> ResolvedEventContent {
    ResolvedEventContent {
        text: "bounded output preview".to_owned(),
        outcome: crate::complete_content::EventContentOutcome {
            requested: ContentPolicy::Complete,
            complete: false,
            origin: crate::complete_content::ContentOrigin::CtxIndex,
            stored_truncated: true,
            source_verified: false,
        },
        complete_content_available,
    }
}

#[test]
fn policy_bounded_text_guidance_is_terminal() {
    let event = test_event();
    let mut rendered = String::new();
    push_event_text_block(&mut rendered, &event, &truncated_content(false));

    assert!(rendered.contains("content: indexed bounded preview (complete content unavailable)"));
    assert!(!rendered.contains("use --content complete"));
}

#[test]
fn policy_bounded_markdown_guidance_is_terminal() {
    let mut rendered = String::new();
    push_indexed_truncation_markdown(&mut rendered, &truncated_content(false));

    assert_eq!(
        rendered,
        "> Complete content is unavailable beyond this indexed bounded preview.\n\n"
    );
    assert!(!rendered.contains("--content complete"));
}

#[test]
fn recoverable_message_guidance_still_offers_complete_content() {
    let content = truncated_content(true);
    let event = test_event();
    let mut text = String::new();
    push_event_text_block(&mut text, &event, &content);
    let mut markdown = String::new();
    push_indexed_truncation_markdown(&mut markdown, &content);

    assert!(text.contains("use --content complete"));
    assert!(markdown.contains("use `--content complete`"));
}

#[test]
fn locate_reports_complete_content_capability_without_locator_material() {
    use ctx_history_capture::complete_content::{
        CompleteContentBodyDigest, CompleteContentSourceFamily,
    };

    let mut event = test_event();
    let locator = PersistedCompleteContentLocatorV1::new(
        CompleteContentSourceFamily::Fixture,
        "ascii-token",
        b"private-locator",
        "native-1",
        CompleteContentBodyDigest::from_text("private-record"),
        CompleteContentBodyDigest::from_text("private-body"),
    )
    .unwrap();
    event.sync.metadata = json!({
        COMPLETE_CONTENT_LOCATOR_METADATA_KEY: locator.to_metadata_value(),
        "source_record_ordinal": 4,
        "source_record_subrecord_index": 1,
    });
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();

    let value = locate_event_json(&store, &event);
    let encoded = serde_json::to_string(&value).unwrap();

    assert_eq!(value["complete_content"]["available"], true);
    assert_eq!(value["source_record"]["ordinal"], 4);
    for private_key in ["value_hex", "body_sha256", "record_sha256"] {
        assert!(!encoded.contains(private_key));
    }
}
