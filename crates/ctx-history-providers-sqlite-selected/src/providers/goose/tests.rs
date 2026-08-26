use ctx_history_core::{ActivityJsonCapture, ActivityTextCapture, TypedKey};
use serde_json::{json, Value};

use rusqlite::Connection;

use super::{
    normalization::{goose_normalized_result_content, normalize_goose_native_output},
    schema::GooseNativeSchema,
    stream::{
        goose_fetch_native_message_page, GooseMessageCellDisposition, GooseNativePageLimits,
        GooseNativeRowKeyset, GOOSE_NATIVE_DEFAULT_PAGE_BYTES,
    },
};

#[test]
fn root_scope_separates_identical_goose_sessions_and_unqualified_is_released() {
    use ctx_history_core::{CaptureProvider, SourceAnchor, SourceAnchorScope, SourceKey};

    let released = SourceKey::derive(
        CaptureProvider::Goose.as_str(),
        crate::GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        super::source_backed::GOOSE_SOURCE_SCHEMA_VARIANT,
        1,
        SourceAnchor::provider_native(
            super::source_backed::GOOSE_SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8(super::source_backed::GOOSE_SOURCE_ANCHOR_KEY).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let unqualified =
        super::source_backed::goose_source_key_scoped(SourceAnchorScope::Unqualified).unwrap();
    assert!(released.exact_descriptor_eq(&unqualified));
    assert_eq!(
        released.identity().encode_canonical().unwrap(),
        unqualified.identity().encode_canonical().unwrap()
    );

    let first =
        super::source_backed::goose_source_key_scoped(SourceAnchorScope::Lineage([0x11; 32]))
            .unwrap();
    let second =
        super::source_backed::goose_source_key_scoped(SourceAnchorScope::Lineage([0x22; 32]))
            .unwrap();
    assert_ne!(
        super::source_backed::goose_session_id(&first, "shared-session").unwrap(),
        super::source_backed::goose_session_id(&second, "shared-session").unwrap()
    );
}

#[test]
fn goose_result_content_is_unbounded_ordered_and_does_not_search_wrappers() {
    let long = "x".repeat(16_037);
    let content = json!([
        {"type": "text", "output": "not a result"},
        {"type": "toolResponse", "toolResult": long.clone()},
        [{"type": "toolResponse", "content": ["second", 2]}],
        {"type": "wrapper", "content": {"type": "toolResponse", "result": "not discovered"}}
    ]);

    assert_eq!(
        goose_normalized_result_content(&content),
        Some(format!("{long}\nsecond\n2"))
    );
    assert_eq!(
        goose_normalized_result_content(&json!([{
            "type": "toolResponse",
            "toolResult": "one",
            "result": "two"
        }])),
        None
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
    let scanned = scan_only_output(json!([{
        "type": "toolResponse",
        "toolCallId": "call-1",
        "toolResult": "exact failure body",
        "status": "provider-failure",
        "exitCode": 9,
        "durationMs": 42
    }]));
    let event = normalize_goose_native_output(&scanned)
        .unwrap()
        .expect("selected Goose result");
    let (call_id, invocation, result) = super::source_backed::goose_activity(&event, None).unwrap();
    assert_eq!(event.searchable_text, "exact failure body");
    assert_eq!(call_id, Some(TypedKey::utf8("call-1").unwrap()));
    assert!(invocation.is_none());
    let result = result.unwrap();
    assert_eq!(result.status.as_deref(), Some("provider-failure"));
    assert_eq!(result.text, ActivityTextCapture::NormalizedBody);
    assert!(matches!(
        result.structured_content,
        ActivityJsonCapture::Omitted { ref reason, .. }
            if reason == "normalized_body_authoritative"
    ));
}

#[test]
fn goose_conflicting_id_name_and_argument_aliases_never_choose_an_occurrence() {
    let event = |request: Value| {
        let scanned = scan_only_output(json!([
            request,
            {
                "type": "toolResponse",
                "toolCallId": "call-1",
                "toolResult": "exact result"
            }
        ]));
        normalize_goose_native_output(&scanned)
            .unwrap()
            .expect("ambiguous Goose record remains retained")
    };

    let conflicting_id = event(json!({
        "type": "toolRequest",
        "toolCallId": "call-1",
        "id": "call-2",
        "toolCall": {"id": "call-1", "name": "exact_tool", "arguments": {"x": 1}}
    }));
    assert_eq!(
        super::source_backed::goose_activity(&conflicting_id, None).unwrap(),
        (None, None, None)
    );

    let conflicting_name = event(json!({
        "type": "toolRequest",
        "toolCallId": "call-1",
        "toolCall": {
            "id": "call-1",
            "name": "first_tool",
            "tool": "second_tool",
            "arguments": {"x": 1}
        }
    }));
    let (call_id, invocation, result) =
        super::source_backed::goose_activity(&conflicting_name, None).unwrap();
    assert_eq!(call_id, Some(TypedKey::Utf8("call-1".to_owned())));
    assert!(invocation.is_none());
    assert!(result.is_some());

    let conflicting_arguments = event(json!({
        "type": "toolRequest",
        "toolCallId": "call-1",
        "toolCall": {
            "id": "call-1",
            "name": "exact_tool",
            "arguments": {"x": 1},
            "input": {"x": 2}
        }
    }));
    let (_, invocation, _) =
        super::source_backed::goose_activity(&conflicting_arguments, None).unwrap();
    assert_eq!(
        invocation.unwrap().arguments,
        ActivityJsonCapture::Unavailable
    );
}

#[test]
fn source_parser_keeps_exact_statuses_and_large_result_bodies() {
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

    for (status_field, status_value) in [
        ("status", json!("success")),
        ("isError", json!(true)),
        ("status", json!("future_state")),
    ] {
        let mut response = serde_json::Map::from_iter([
            ("type".to_owned(), json!("toolResponse")),
            ("toolCallId".to_owned(), json!("call-1")),
            ("toolResult".to_owned(), json!("complete native result")),
        ]);
        response.insert(status_field.to_owned(), status_value.clone());
        let scanned = scan_only_output(Value::Array(vec![Value::Object(response)]));
        assert_eq!(scanned.disposition, GooseMessageCellDisposition::ToolOutput);
        let event = normalize_goose_native_output(&scanned)
            .unwrap()
            .expect("selected Goose result");
        assert_eq!(event.searchable_text, "complete native result");
        assert_eq!(event.content[0]["toolCallId"], "call-1");
        assert_eq!(event.content[0][status_field], status_value);
        assert_eq!(event.content[0]["toolResult"], "complete native result");
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

#[test]
fn duplicate_goose_selectors_reject_or_retain_only_raw_lexical_evidence() {
    let duplicate_type = scan_only_output_raw(
        r#"[{"type":"text","type":"toolResponse","toolCallId":"call-1","toolResult":"body"}]"#,
    );
    assert_eq!(
        duplicate_type.disposition,
        GooseMessageCellDisposition::DuplicateBlockType
    );
    assert!(duplicate_type.content_json.is_none());

    let duplicate_result = scan_only_output_raw(
        r#"[{"type":"toolResponse","toolCallId":"call-1","toolResult":"first","toolResult":"second"}]"#,
    );
    assert_eq!(
        duplicate_result.disposition,
        GooseMessageCellDisposition::ToolOutput
    );
    let event = normalize_goose_native_output(&duplicate_result)
        .unwrap()
        .expect("duplicate payload remains bounded lexical evidence");
    assert!(event.semantic_capture_ambiguous);
    assert!(event.searchable_text.contains("\"toolResult\":\"first\""));
    assert!(event.searchable_text.contains("\"toolResult\":\"second\""));
    assert_eq!(
        super::source_backed::goose_activity(&event, None).unwrap(),
        (None, None, None)
    );
}

fn scan_only_output(content: Value) -> super::stream::GooseScannedMessage {
    scan_only_output_raw(&content.to_string())
}

fn scan_only_output_raw(content: &str) -> super::stream::GooseScannedMessage {
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
             insert into schema_version values (15);
             insert into sessions values ('session-1');",
        )
        .unwrap();
    connection
        .execute(
            "insert into messages (id, message_id, session_id, role, content_json)
             values (1, 'message-1', 'session-1', 'tool', ?1)",
            [content],
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

#[test]
fn repeated_message_ids_across_sessions_remain_exact_native_identities() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "create table sessions (
                 id text primary key,
                 parent_session_id text
             );
             create table messages (
                 id integer primary key,
                 message_id text,
                 session_id text not null,
                 role text not null,
                 content_json text not null
             );
             create table schema_version (version integer not null);
             insert into schema_version values (15);
             insert into sessions values ('parent', null), ('child', 'parent');
             insert into messages values
                 (1, 'copied-message', 'parent', 'user', '[{\"type\":\"text\",\"text\":\"parent\"}]'),
                 (2, 'copied-message', 'child', 'user', '[{\"type\":\"text\",\"text\":\"child\"}]');",
        )
        .unwrap();
    let schema = GooseNativeSchema::probe(&connection).unwrap();
    let rows = goose_fetch_native_message_page(
        &connection,
        &schema,
        GooseNativeRowKeyset::Unstarted,
        GooseNativePageLimits::default(),
    )
    .unwrap();

    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| !row.identity_degraded));
    assert!(rows
        .iter()
        .all(|row| row.provider_message_identity.as_deref() == Some("copied-message")));
    assert!(rows
        .iter()
        .all(|row| row.native_identity.contains("copied-message")));
}

#[test]
fn missing_and_colliding_message_ids_do_not_invent_copy_authority() {
    let missing = super::stream::goose_native_message_identity(None, 0, 7);
    assert!(missing.identity_degraded);
    assert_eq!(missing.provider_message_identity, None);
    assert!(missing
        .native_identity
        .starts_with("goose-message-identity-v1:messages-id:"));

    let collision =
        super::stream::goose_native_message_identity(Some("provider-message".to_owned()), 2, 8);
    assert!(collision.identity_degraded);
    assert_eq!(
        collision.provider_message_identity.as_deref(),
        Some("provider-message")
    );
    assert!(collision
        .native_identity
        .starts_with("goose-message-identity-v1:messages-id:"));
}

mod source_backed;
