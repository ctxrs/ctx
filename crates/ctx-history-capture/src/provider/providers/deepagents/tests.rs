use ctx_history_core::EventType;
use rmpv::{encode::write_value as write_msgpack_value, Value as MsgpackValue};

use super::message::{
    core_eligible, deepagents_decode_msgpack, deepagents_event_type,
    deepagents_messages_from_msgpack_value, DeepAgentsMessage,
};
use crate::CaptureError;

fn message(role: &str, text: &str, status: Option<&str>) -> MsgpackValue {
    let mut fields = vec![
        (
            MsgpackValue::String("type".into()),
            MsgpackValue::String(role.into()),
        ),
        (
            MsgpackValue::String("content".into()),
            MsgpackValue::String(text.into()),
        ),
        (
            MsgpackValue::String("id".into()),
            MsgpackValue::String(format!("{role}-{text}").into()),
        ),
    ];
    if let Some(status) = status {
        fields.push((
            MsgpackValue::String("status".into()),
            MsgpackValue::String(status.into()),
        ));
    }
    MsgpackValue::Map(fields)
}

fn encoded(value: &MsgpackValue) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_msgpack_value(&mut bytes, value).unwrap();
    bytes
}

fn decoded_message(value: MsgpackValue) -> DeepAgentsMessage {
    let mut decoded = deepagents_messages_from_msgpack_value(&value);
    assert_eq!(decoded.rejected_entries, 0);
    assert_eq!(decoded.ignored_entries, 0);
    assert_eq!(decoded.messages.len(), 1);
    decoded.messages.remove(0)
}

#[test]
fn msgpack_requires_eof_and_rejects_trailing_bytes() {
    let mut payload = encoded(&MsgpackValue::Array(vec![message(
        "human",
        "valid prefix",
        None,
    )]));
    payload.push(0xc0);

    let error = deepagents_decode_msgpack(&payload).unwrap_err();
    assert!(matches!(
        error,
        CaptureError::InvalidPayload(ref reason) if reason.contains("trailing bytes")
    ));
}

#[test]
fn source_backed_eligibility_keeps_provider_event_classification() {
    let user = decoded_message(message("human", "question", None));
    assert!(core_eligible(&user));
    assert_eq!(deepagents_event_type(&user), EventType::Message);

    let successful_tool = decoded_message(message("tool", "success output", Some("success")));
    assert!(!core_eligible(&successful_tool));
    assert_eq!(
        deepagents_event_type(&successful_tool),
        EventType::ToolOutput
    );

    let failed_tool = decoded_message(message("tool", "failure output", Some("failed")));
    assert!(core_eligible(&failed_tool));
    assert_eq!(deepagents_event_type(&failed_tool), EventType::ToolOutput);

    let timed_out_tool = decoded_message(message("tool", "timeout output", Some("timed_out")));
    assert!(core_eligible(&timed_out_tool));
    assert_eq!(
        deepagents_event_type(&timed_out_tool),
        EventType::ToolOutput
    );
}
