use serde_json::{json, Value};

use rusqlite::Connection;

use super::{
    normalization::{
        goose_normalized_result_content, goose_output_projection, normalize_goose_native_output,
    },
    schema::GooseNativeSchema,
    stream::{
        goose_fetch_native_message_page, GooseMessageCellDisposition, GooseNativePageLimits,
        GooseNativeRowKeyset, GOOSE_NATIVE_DEFAULT_PAGE_BYTES,
    },
};

#[test]
fn goose_result_content_is_unbounded_ordered_and_does_not_search_wrappers() {
    let long = "x".repeat(16_037);
    let content = json!([
        {"type": "text", "output": "not a result"},
        {"type": "toolResponse", "toolResult": long.clone(), "result": "lower priority"},
        [{"type": "toolResponse", "content": ["second", 2]}],
        {"type": "wrapper", "content": {"type": "toolResponse", "result": "not discovered"}}
    ]);

    assert_eq!(
        goose_normalized_result_content(&content),
        Some(format!("{long}\nsecond\n2"))
    );
    assert_eq!(
        goose_normalized_result_content(&json!({
            "wrapper": {"type": "toolResponse", "result": "not discovered"}
        })),
        None
    );
}

#[test]
fn goose_output_body_and_outcome_use_the_same_direct_tool_responses() {
    let content = json!([
        {
            "type": "toolResponse",
            "toolCallId": "call-1",
            "toolResult": "exact failure body",
            "exitCode": 9,
            "durationMs": 42
        },
        {
            "type": "wrapper",
            "content": {
                "type": "toolResponse",
                "toolResult": "must not affect body or outcome",
                "success": true
            }
        }
    ]);

    let output = goose_output_projection(&content);
    assert_eq!(
        goose_normalized_result_content(&content).as_deref(),
        Some("exact failure body")
    );
    assert_eq!(output.call_id.as_deref(), Some("call-1"));
    assert_eq!(output.outcome.outcome, crate::OutputOutcome::Failure);
    assert_eq!(output.outcome.exit_code, Some(9));
    assert_eq!(output.outcome.duration_ms, Some(42));
}

#[test]
fn source_parser_keeps_success_failure_unknown_and_large_result_bodies() {
    let ordered = scan_only_output(json!([
        {
            "type": "toolResponse",
            "toolCallId": "ordered-call",
            "toolResult": "first"
        },
        [{
            "type": "toolResponse",
            "toolCallId": "ordered-call",
            "toolResult": "second"
        }]
    ]));
    let ordered = normalize_goose_native_output(&ordered)
        .unwrap()
        .expect("nested direct Goose results");
    assert_eq!(ordered.searchable_text, "first\nsecond");

    for (status_field, status_value, expected_disposition, expected_outcome) in [
        (
            "status",
            json!("success"),
            GooseMessageCellDisposition::OutputSuccess,
            "success",
        ),
        (
            "isError",
            json!(true),
            GooseMessageCellDisposition::OutputFailure,
            "failure",
        ),
        (
            "status",
            json!("future_state"),
            GooseMessageCellDisposition::OutputUnknown,
            "unknown",
        ),
    ] {
        let mut response = serde_json::Map::from_iter([
            ("type".to_owned(), json!("toolResponse")),
            ("toolCallId".to_owned(), json!("call-1")),
            ("toolResult".to_owned(), json!("complete native result")),
        ]);
        response.insert(status_field.to_owned(), status_value);
        let scanned = scan_only_output(Value::Array(vec![Value::Object(response)]));
        assert_eq!(scanned.disposition, expected_disposition);
        let event = normalize_goose_native_output(&scanned)
            .unwrap()
            .expect("selected Goose result");
        assert_eq!(event.searchable_text, "complete native result");
        assert_eq!(event.content["result_outcome"], expected_outcome);
        assert_eq!(event.content["call_id"], "call-1");
        assert!(!event.content.to_string().contains("complete native result"));
    }

    let large = format!(
        "goose-large-head-{}-goose-large-tail",
        "x".repeat(8 * 1024 * 1024)
    );
    let scanned = scan_only_output(json!([{
        "type": "toolResponse",
        "toolCallId": "large-call",
        "toolResult": large,
        "status": "success"
    }]));
    let event = normalize_goose_native_output(&scanned)
        .unwrap()
        .expect("large Goose result");
    assert!(event.searchable_text.len() > GOOSE_NATIVE_DEFAULT_PAGE_BYTES as usize);
    assert!(event.searchable_text.starts_with("goose-large-head-"));
    assert!(event.searchable_text.ends_with("-goose-large-tail"));

    let status_only = scan_only_output(json!([{
        "type": "toolResponse",
        "status": "failed"
    }]));
    assert!(normalize_goose_native_output(&status_only)
        .unwrap()
        .is_none());
}

fn scan_only_output(content: Value) -> super::stream::GooseScannedMessage {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "create table sessions (id text primary key);
             create table messages (
                 id integer primary key,
                 message_id text,
                 session_id text not null,
                 role text not null,
                 content_json text not null
             );
             create table schema_version (version integer not null);
             insert into schema_version values (14);
             insert into sessions values ('session-1');",
        )
        .unwrap();
    connection
        .execute(
            "insert into messages (id, message_id, session_id, role, content_json)
             values (1, 'message-1', 'session-1', 'tool', ?1)",
            [content.to_string()],
        )
        .unwrap();
    let schema = GooseNativeSchema::probe(&connection).unwrap();
    let mut rows = goose_fetch_native_message_page(
        &connection,
        &schema,
        GooseNativeRowKeyset::Unstarted,
        GooseNativePageLimits::default(),
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    rows.remove(0)
}

mod source_backed;
