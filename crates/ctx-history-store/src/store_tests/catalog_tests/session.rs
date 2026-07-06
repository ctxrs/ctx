#[allow(unused_imports)]
use super::*;

pub(crate) fn session_event(session_id: Uuid, index: u64) -> Event {
    Event {
        id: new_id(),
        seq: index,
        history_record_id: None,
        session_id: Some(session_id),
        run_id: None,
        event_type: EventType::Message,
        role: Some(EventRole::Assistant),
        occurred_at: fixed_time() + chrono::Duration::seconds(index as i64),
        capture_source_id: None,
        payload: serde_json::json!({"index": index}),
        payload_blob_id: None,
        dedupe_key: None,
        redaction_state: RedactionState::LocalPreview,
        sync: sync_metadata(),
    }
}
