use super::*;
use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType, Fidelity, SyncMetadata, SyncState, Visibility};

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
fn locate_reports_complete_content_capability_without_locator_material() {
    use ctx_history_capture::complete_content::{
        CompleteContentBodyDigest, CompleteContentSourceFamily, VerifiedContentLocatorV1,
        VerifiedContentLocatorsV1, VerifiedContentRole,
    };
    use ctx_history_core::ContentRef;

    let mut event = test_event();
    let mut address = [0_u8; 16];
    address[8..].copy_from_slice(&1_u64.to_be_bytes());
    let locator = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        "codex.message-body.v1",
        ContentRef::from_bytes(b"private-body").unwrap(),
        CompleteContentSourceFamily::Jsonl,
        "jsonl-range-v1",
        &address,
        "native-1",
        CompleteContentBodyDigest::from_text("private-record"),
    )
    .unwrap();
    let locators = VerifiedContentLocatorsV1::singleton(locator).unwrap();
    event.sync.metadata = json!({
        VERIFIED_CONTENT_LOCATORS_METADATA_KEY: locators.to_metadata_value(),
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
