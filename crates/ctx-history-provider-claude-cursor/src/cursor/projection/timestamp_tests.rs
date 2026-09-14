use super::{embedded_cursor_timestamp, CursorNativeEvent, CursorTimestampState};
use crate::cursor::parser::project_cursor_jsonl_record;

fn text_event(role: &str, text: &str) -> CursorNativeEvent {
    let record = serde_json::to_vec(&serde_json::json!({
        "role": role,
        "message": {"content": [{"type": "text", "text": text}]}
    }))
    .unwrap();
    project_cursor_jsonl_record(&record, 0, 0, 0, record.len() as u64)
        .unwrap()
        .unwrap()
        .remove(0)
}

#[test]
fn embedded_timestamp_rejects_invalid_offset_minutes() {
    let event = text_event(
        "user",
        "<timestamp>Thursday, Jul 30, 2026, 11:38 AM (UTC-7:60)</timestamp>",
    );
    assert!(embedded_cursor_timestamp(&event).is_none());
}

#[test]
fn embedded_timestamp_rejects_offset_overflow_without_panicking() {
    let event = text_event(
        "user",
        "<timestamp>Thursday, Jul 30, 2026, 11:38 AM (UTC+596523:20)</timestamp>",
    );
    let parsed = std::panic::catch_unwind(|| embedded_cursor_timestamp(&event));
    assert!(parsed.is_ok(), "untrusted text must not panic the importer");
    assert!(parsed.unwrap().is_none());
}

#[test]
fn undated_user_does_not_inherit_an_earlier_turns_time() {
    let mut state = CursorTimestampState::default();
    let mut dated = [text_event(
        "user",
        "<timestamp>Thursday, Jul 30, 2026, 11:38 AM (UTC-7)</timestamp>",
    )];
    state.apply(&mut dated).unwrap();
    let mut undated = [text_event(
        "user",
        "<user_query>another question</user_query>",
    )];
    state.apply(&mut undated).unwrap();
    assert!(undated[0].occurred_at.is_none());
}

#[test]
fn assistant_text_does_not_supply_a_provider_timestamp() {
    let mut event = [text_event(
        "assistant",
        "<timestamp>Thursday, Jul 30, 2026, 11:38 AM (UTC-7)</timestamp>",
    )];
    CursorTimestampState::default().apply(&mut event).unwrap();
    assert!(event[0].occurred_at.is_none());
}
