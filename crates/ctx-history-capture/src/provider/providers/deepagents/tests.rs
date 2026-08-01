use ctx_history_core::EventType;
use rmpv::{encode::write_value as write_msgpack_value, Value as MsgpackValue};

use super::message::{
    core_eligible, deepagents_decode_msgpack, deepagents_event_type,
    deepagents_messages_from_msgpack_value, DeepAgentsMessage,
};
use super::source::deepagents_write_candidate_page;
use crate::CaptureError;
use rusqlite::Connection;

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
            MsgpackValue::String(format!("{role}-id").into()),
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
    assert!(core_eligible(&successful_tool));
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

#[test]
fn duplicate_projected_msgpack_keys_are_rejected() {
    let duplicate_content = MsgpackValue::Map(vec![
        (
            MsgpackValue::String("type".into()),
            MsgpackValue::String("tool".into()),
        ),
        (
            MsgpackValue::String("content".into()),
            MsgpackValue::String("first".into()),
        ),
        (
            MsgpackValue::String("content".into()),
            MsgpackValue::String("second".into()),
        ),
    ]);
    let decoded = deepagents_messages_from_msgpack_value(&duplicate_content);
    assert!(decoded.messages.is_empty());
    assert_eq!(decoded.rejected_entries, 1);
}

#[test]
fn write_candidate_larger_than_native_page_target_is_hydrated() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "create table writes (
                thread_id text not null,
                checkpoint_ns text not null,
                checkpoint_id text not null,
                task_id text not null,
                idx integer not null,
                channel text not null,
                type text,
                value blob not null
            );",
        )
        .unwrap();
    let body = format!(
        "deepagents-large-head-{}-deepagents-large-tail",
        "x".repeat(8 * 1024 * 1024)
    );
    let payload = encoded(&message("tool", &body, Some("success")));
    assert!(payload.len() > 8 * 1024 * 1024);
    connection
        .execute(
            "insert into writes (
                thread_id, checkpoint_ns, checkpoint_id, task_id, idx, channel, type, value
             ) values ('thread', '', 'checkpoint', 'task', 0, 'messages', 'msgpack', ?1)",
            [&payload],
        )
        .unwrap();
    let candidates = deepagents_write_candidate_page(&connection, None, 8).unwrap();
    assert_eq!(candidates.len(), 1);
    assert!(candidates[0].key.is_some());
    assert!(candidates[0].rejection_reason.is_none());
    assert_eq!(candidates[0].value.as_deref(), Some(payload.as_slice()));
}
