use serde_json::{json, Value};

use super::*;
use crate::provider::providers::zed::thread::ZedThreadRow;
use crate::MAX_PROVIDER_SQLITE_VALUE_BYTES;

type EventSnapshot = (Value, String);

fn row(thread: Value) -> ZedThreadRow {
    ZedThreadRow {
        rowid: 7,
        id: "zed-decoder-parity".to_owned(),
        parent_id: None,
        folder_paths: None,
        folder_paths_order: None,
        summary: "decoder parity".to_owned(),
        updated_at: "2026-07-21T12:00:00Z".to_owned(),
        data_type: "json".to_owned(),
        data: serde_json::to_vec(&thread).unwrap(),
        created_at: None,
    }
}

fn snapshot(decoded: ZedDecodedEvent<'_>) -> EventSnapshot {
    (
        serde_json::to_value(decoded.event).unwrap(),
        decoded.complete_text,
    )
}

fn live_decode(row: &ZedThreadRow) -> std::result::Result<Vec<EventSnapshot>, String> {
    let decoded = decode_zed_thread_events(row).map_err(|error| error.to_string())?;
    decoded
        .events(&row.id)
        .map(|event| event.map(snapshot).map_err(|error| error.to_string()))
        .collect()
}

fn recovery_decode(row: &ZedThreadRow) -> std::result::Result<Vec<EventSnapshot>, String> {
    let decoded = decode_zed_thread_events(row).map_err(|error| error.to_string())?;
    let mut events = Vec::new();
    for event_index in 0.. {
        let Some(event) = decoded
            .event_at(&row.id, event_index)
            .map_err(|error| error.to_string())?
        else {
            break;
        };
        events.push(snapshot(event));
    }
    Ok(events)
}

fn assert_live_recovery_parity(row: &ZedThreadRow) -> Vec<EventSnapshot> {
    let live = live_decode(row).unwrap();
    assert_eq!(recovery_decode(row).unwrap(), live);
    live
}

fn assert_live_recovery_error_parity(row: &ZedThreadRow, expected: &str) {
    let live = live_decode(row).unwrap_err();
    let recovery = recovery_decode(row).unwrap_err();
    assert_eq!(recovery, live);
    assert!(live.contains(expected), "unexpected error: {live}");
}

#[test]
fn authoritative_decoder_valid_payload_has_live_recovery_parity() {
    let row = row(json!({
        "updated_at": "2026-07-21T13:00:00Z",
        "messages": [
            {"User": {"content": [{"Text": "hello from Zed"}]}},
            {"Agent": {
                "content": [
                    {"Text": "working"},
                    {"ToolUse": {
                        "id": "call-1",
                        "name": "edit_file",
                        "input": {"path": "src/main.rs"}
                    }}
                ],
                "tool_results": {
                    "call-1": {
                        "id": "call-1",
                        "tool_name": "edit_file",
                        "content": [{"Text": "done"}],
                        "is_error": false
                    }
                }
            }},
            {"Compaction": {"Summary": "compact summary"}}
        ]
    }));

    let snapshots = assert_live_recovery_parity(&row);
    assert_eq!(snapshots.len(), 4);
    let event_types = snapshots
        .iter()
        .map(|(event, _)| event["event_type"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        event_types,
        ["message", "tool_call", "tool_output", "summary"]
    );
    assert_eq!(snapshots[0].1, "hello from Zed");
    assert_eq!(
        snapshots[1].1,
        "working\ntool call: edit_file\ntool input: present"
    );
    assert_eq!(snapshots[2].1, "tool result: edit_file\ndone");
    assert_eq!(snapshots[3].1, "compact summary");
    assert_eq!(snapshots[1].0["provider_event_index"], 2);
    assert_eq!(snapshots[2].0["provider_event_index"], 3);
    assert_eq!(
        snapshots[1].0["cursor"],
        "thread:zed-decoder-parity:message:1:tool_call"
    );
    assert_eq!(
        snapshots[2].0["cursor"],
        "thread:zed-decoder-parity:message:1:tool_output"
    );
    assert_eq!(
        snapshots[1].0["metadata"]["provider_event_identity_index"],
        1
    );
    assert_eq!(
        snapshots[2].0["metadata"]["provider_event_identity_index"],
        1_000_001
    );
    assert_eq!(snapshots[0].0["occurred_at"], "2026-07-21T13:00:00Z");
}

#[test]
fn authoritative_decoder_malformed_payload_fails_closed_with_parity() {
    for (thread, expected) in [
        (
            json!({"title": "missing messages"}),
            "missing DbThread.messages array",
        ),
        (
            json!({"messages": [{"User": []}]}),
            "Zed User message 0 must contain an object",
        ),
        (
            json!({"messages": [{"Agent": {"content": {"Text": "wrong"}}}]}),
            "Zed Agent message 0 content must be an array",
        ),
        (
            json!({"messages": [null]}),
            "Zed message 0 is not a nonempty externally tagged value",
        ),
    ] {
        assert_live_recovery_error_parity(&row(thread), expected);
    }
}

#[test]
fn authoritative_decoder_oversized_payload_fails_closed_with_parity() {
    let mut encoded = row(json!({"messages": []}));
    encoded.data = vec![b' '; MAX_PROVIDER_SQLITE_VALUE_BYTES + 1];
    assert_live_recovery_error_parity(&encoded, "exceeds 16777216 encoded bytes");

    let expanded = vec![b' '; MAX_PROVIDER_SQLITE_VALUE_BYTES + 1];
    let mut compressed = row(json!({"messages": []}));
    compressed.data_type = "zstd".to_owned();
    compressed.data = zstd::stream::encode_all(expanded.as_slice(), 0).unwrap();
    assert!(compressed.data.len() < MAX_PROVIDER_SQLITE_VALUE_BYTES);
    assert_live_recovery_error_parity(&compressed, "exceeds 16777216 decompressed bytes");

    let messages = vec![Value::String("Resume".to_owned()); ZED_MAX_MESSAGES_PER_THREAD + 1];
    let too_many = row(json!({"messages": messages}));
    assert!(too_many.data.len() < MAX_PROVIDER_SQLITE_VALUE_BYTES);
    assert_live_recovery_error_parity(&too_many, "exceeds 65536 messages");

    let mut nested = Value::String("FutureLeaf".to_owned());
    for _ in 0..ZED_MAX_MESSAGE_JSON_DEPTH {
        nested = json!({"Future": nested});
    }
    let too_deep = row(json!({"messages": [nested]}));
    assert_live_recovery_error_parity(&too_deep, "exceeds JSON depth 64");
}

#[test]
fn authoritative_decoder_edge_payload_preserves_forward_compatible_semantics() {
    let empty = row(json!({"messages": []}));
    assert!(assert_live_recovery_parity(&empty).is_empty());

    let row = row(json!({
        "messages": [
            "Resume",
            {"FutureVariant": {"detail": "future detail"}},
            {"Agent": {"content": [{"ToolUse": {"name": "read_file"}}]}},
            {"Agent": {"tool_results": {
                "result": {"tool_name": "read_file", "output": "edge output"}
            }}}
        ]
    }));
    let snapshots = assert_live_recovery_parity(&row);
    let event_types = snapshots
        .iter()
        .map(|(event, _)| event["event_type"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        event_types,
        ["message", "notice", "tool_call", "tool_output"]
    );
    assert_eq!(snapshots[0].1, "[resume]");
    assert_eq!(snapshots[2].0["metadata"]["event_suffix"], "message");
    assert_eq!(snapshots[3].0["metadata"]["event_suffix"], "message");
    assert_eq!(snapshots[3].1, "tool result: read_file\nedge output");
}

#[test]
fn result_profile_returns_explicit_fields_without_display_labels() {
    let message = json!({"Agent": {"tool_results": {
        "call-1": {
            "tool_name": "shell",
            "is_error": true,
            "content": [
                {"Text": "first"},
                {"Image": {"source": "ignored"}}
            ],
            "output": "second"
        }
    }}});
    assert_eq!(
        zed_result_content(&message).as_deref(),
        Some("first\nsecond")
    );

    let label_only = json!({"Agent": {"tool_results": {
        "call-1": {"tool_name": "shell", "is_error": false}
    }}});
    assert_eq!(zed_result_content(&label_only), None);
    assert_eq!(
        zed_result_content(&json!({"User": {"content": [{"Text": "not a result"}]}})),
        None
    );
}
