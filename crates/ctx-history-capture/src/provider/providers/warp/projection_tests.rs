use ctx_history_core::EventType;

use super::super::proto::warp_decode_message;
use super::*;

fn field(field: u32, payload: &[u8]) -> Vec<u8> {
    let mut encoded = vec![u8::try_from(field << 3 | 2).unwrap()];
    encoded.push(u8::try_from(payload.len()).unwrap());
    encoded.extend_from_slice(payload);
    encoded
}

#[test]
fn real_tool_result_without_explicit_outcome_stays_unknown() {
    let tool_result = field(1, &[]);
    let mut encoded = field(1, b"message-1");
    encoded.extend(field(5, &tool_result));
    let message = warp_decode_message(&encoded).unwrap();
    assert_eq!(message.event_type, EventType::ToolOutput);
    let event = warp_message_event(
        "conversation-1",
        "task-1",
        &message,
        0,
        0,
        "2026-07-21T12:00:00Z".parse().unwrap(),
    );
    assert_eq!(event.payload["result_evidence"], Value::Null);
    assert_eq!(event.payload["result_outcome"], Value::Null);
}
