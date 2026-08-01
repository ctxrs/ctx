use ctx_history_core::EventType;
use serde_json::{json, Value};

use super::*;

const LEGACY_CANONICAL_TEXT_BOUND: usize = 16_000;

struct TestPolicyEvent {
    payload: Value,
}

fn test_native_event(event_type: EventType, text: &str, body: Value) -> TestPolicyEvent {
    let retained_text = provider_policy_event_text(event_type, text, &body);
    let retained_body = provider_policy_body(event_type, &body);
    TestPolicyEvent {
        payload: json!({
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": provider_result_identifier_evidence(event_type, text, &body),
            "result_outcome": provider_result_outcome_evidence(event_type, &body),
            "source_format": "test_provider",
            "body": retained_body,
        }),
    }
}

#[test]
fn result_evidence_abstains_on_unknown_without_omitting_result_content() {
    let unknown = test_native_event(
        EventType::ToolOutput,
        "UNKNOWN_RESULT_NARRATIVE",
        json!({"call_id": "unknown-call", "output": "UNKNOWN_RESULT_NARRATIVE"}),
    );
    assert_eq!(unknown.payload["result_outcome"], Value::Null);
    assert_eq!(unknown.payload["text_retention"]["mode"], "complete");
    assert_eq!(unknown.payload["text"], "UNKNOWN_RESULT_NARRATIVE");
    assert_eq!(
        unknown.payload["body"]["output"],
        "UNKNOWN_RESULT_NARRATIVE"
    );

    let failed = test_native_event(
        EventType::ToolOutput,
        "FAILED_RESULT_DIAGNOSTIC",
        json!({
            "call_id": "failed-call",
            "exit_code": 2,
            "output": "FAILED_RESULT_DIAGNOSTIC",
        }),
    );
    assert_eq!(failed.payload["result_outcome"], "failure");
    assert_eq!(failed.payload["text"], "FAILED_RESULT_DIAGNOSTIC");
    assert_eq!(failed.payload["body"]["output"], "FAILED_RESULT_DIAGNOSTIC");
}

#[test]
fn result_outcome_requires_one_bounded_explicit_consistent_signal() {
    let event = |body| test_native_event(EventType::ToolOutput, "", body);
    assert_eq!(
        event(json!({"call_id": "unknown"})).payload["result_outcome"],
        Value::Null,
    );
    assert_eq!(
        event(json!({"call_id": "success", "exit_code": 0})).payload["result_outcome"],
        "success"
    );
    assert_eq!(
        event(json!({"call_id": "failure", "success": false})).payload["result_outcome"],
        "failure"
    );
    assert_eq!(
        event(json!({"call_id": "truncated", "truncated": true})).payload["result_outcome"],
        Value::Null
    );
    assert_eq!(
        event(json!({
            "results": [
                {"call_id": "one", "success": true},
                {"call_id": "two", "exit_code": 1},
            ]
        }))
        .payload["result_outcome"],
        Value::Null,
    );
}

#[test]
fn result_evidence_distinguishes_git_commit_summaries_from_other_oids() {
    let produced = test_native_event(
        EventType::CommandOutput,
        "[main 0123456789ab] add bounded evidence",
        json!({
            "call_id": "commit-call",
            "exit_code": 0,
            "output": "[main 0123456789ab] add bounded evidence",
        }),
    );
    assert_eq!(
        produced.payload["result_evidence"],
        json!([
            {"kind": "call_id", "value": "commit-call"},
            {"kind": "git_commit_summary_id", "value": "0123456789ab"},
        ])
    );

    let referenced = test_native_event(
        EventType::CommandOutput,
        "inspected 0123456789abcdef0123456789abcdef01234567",
        json!({
            "call_id": "show-call",
            "exit_code": 0,
            "output": "inspected 0123456789abcdef0123456789abcdef01234567",
        }),
    );
    assert_eq!(
        referenced.payload["result_evidence"],
        json!([
            {"kind": "call_id", "value": "show-call"},
            {"kind": "git_oid", "value": "0123456789abcdef0123456789abcdef01234567"},
        ])
    );

    let saturated_call_ids = (0..MAX_RESULT_EVIDENCE_IDENTIFIERS)
        .map(|index| json!({"tool_call_id": format!("call-{index}")}))
        .collect::<Vec<_>>();
    let saturated = provider_result_identifier_evidence(
        EventType::CommandOutput,
        "[main 0123456789ab] must not exceed the evidence bound",
        &json!({"success": true, "results": saturated_call_ids}),
    );
    assert_eq!(
        saturated.as_array().map(Vec::len),
        Some(MAX_RESULT_EVIDENCE_IDENTIFIERS)
    );
}

#[test]
fn native_event_retains_complete_message_tool_arguments_results_and_patches() {
    let message = format!(
        "{}MESSAGE_TAIL_ORACLE",
        "message-content-".repeat(LEGACY_CANONICAL_TEXT_BOUND / 8)
    );
    let arguments = format!(
        "{}ARGUMENT_TAIL_ORACLE",
        "structured-tool-argument-".repeat(LEGACY_CANONICAL_TEXT_BOUND / 12)
    );
    let result = format!(
        "{}RESULT_TAIL_ORACLE",
        "successful-command-output-".repeat(LEGACY_CANONICAL_TEXT_BOUND / 12)
    );
    let patch = "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-old\n+new\n*** End Patch";

    for event in [
        test_native_event(EventType::Message, &message, json!({"content": message})),
        test_native_event(
            EventType::ToolCall,
            &arguments,
            json!({"tool_name": "Edit", "arguments": arguments, "patch": patch}),
        ),
        test_native_event(
            EventType::CommandOutput,
            &result,
            json!({"exit_code": 0, "stdout": result, "diff": patch}),
        ),
    ] {
        assert_eq!(
            event.payload["text_retention"],
            json!({
                "mode": "complete",
                "limit_chars": null,
                "truncated": false,
                "omission_policy": "none",
                "omission_applied": false,
            })
        );
        let rendered = event.payload.to_string();
        assert!(!rendered.contains("field_retention"));
        assert!(!rendered.contains("provider_truncation"));
    }

    let message_event =
        test_native_event(EventType::Message, &message, json!({"content": message}));
    assert!(message_event.payload["text"]
        .as_str()
        .unwrap()
        .ends_with("MESSAGE_TAIL_ORACLE"));
    let tool_event = test_native_event(
        EventType::ToolCall,
        &arguments,
        json!({"arguments": arguments, "patch": patch}),
    );
    assert!(tool_event.payload["body"]["arguments"]
        .as_str()
        .unwrap()
        .ends_with("ARGUMENT_TAIL_ORACLE"));
    assert_eq!(tool_event.payload["body"]["patch"], patch);
    let result_event = test_native_event(
        EventType::CommandOutput,
        &result,
        json!({"exit_code": 0, "stdout": result, "diff": patch}),
    );
    assert!(result_event.payload["body"]["stdout"]
        .as_str()
        .unwrap()
        .ends_with("RESULT_TAIL_ORACLE"));
    assert_eq!(result_event.payload["body"]["diff"], patch);
}

#[test]
fn result_evidence_rejects_unsafe_call_ids_without_affecting_complete_content() {
    let unsafe_call_ids = provider_result_identifier_evidence(
        EventType::ToolOutput,
        "",
        &json!({
            "tool_use_id": "secret token with spaces",
            "tool_call_id": "x".repeat(MAX_RESULT_EVIDENCE_CALL_ID_CHARS + 1),
            "success": true,
        }),
    );
    assert!(unsafe_call_ids.is_null());
}
