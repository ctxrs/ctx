use serde_json::{json, Value};

use super::*;
use crate::mcp::response::{success_response, tool_result};

const TEST_OUTPUT_LIMIT: usize = 1_024;

fn expanded_response() -> (Value, Value, Uuid) {
    let event_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000010").unwrap();
    let response_id = json!("request-\"\\\n\u{0001}-雪");
    let expanded = "\"\\\n\r\t\u{0000}\u{001f}雪".repeat(200);
    let structured = json!({
        "schema_version": 1,
        "payload_type": "event_window",
        "ctx_event_id": event_id,
        "content_policy": "complete",
        "event": {
            "ctx_event_id": event_id,
            "text": expanded,
            "source": {
                "path": "C:\\\\Users\\\"agent\"\\history\n雪.jsonl",
                "cursor": "line:\"7\"\\next\u{0001}",
            },
            "citation": {
                "source_record_ordinal": 7,
                "provider_event_hash": "sha256:\"quoted\"\\hash",
            },
        },
        "events": [{"ctx_event_id": event_id}],
    });
    let response = success_response(response_id.clone(), tool_result(structured));
    (response, response_id, event_id)
}

fn expanded_blame_response() -> (Value, Value, Value) {
    let response_id = json!("blame-request");
    let expanded = "\"\\\n\r\t\u{0000}\u{001f}雪".repeat(200);
    let structured = json!({
        "target": {
            "kind": "commit",
            "commit": {"id": "commit:abc", "kind": "commit", "display": "abc"},
            "repository": {"id": "repo:ctx", "kind": "repository", "display": "ctxrs/ctx"}
        },
        "git_snapshot": null,
        "matches": [{
            "kind": "commit",
            "value": {
                "fact_id": "fact:1",
                "fact_type": "git.commit.produced",
                "predicate": "produced_by",
                "subject": {"id": "commit:abc", "kind": "commit", "display": "abc"},
                "object": {"id": "session:large", "kind": "session", "display": expanded},
                "fact_occurred_at_ms": null,
                "confidence": "explicit",
                "state": "asserted",
                "direct_actor": null,
                "owning_root": null,
                "evidence_numbers": [1]
            }
        }],
        "evidence": [{
            "number": 1,
            "citation": {"event_id": "33333333-3333-4333-8333-333333333333"}
        }],
        "next": {"cursor": "real-helper-cursor", "reason": "more_matches"}
    });
    let response = success_response(response_id.clone(), tool_result(structured.clone()));
    (response, response_id, structured)
}

#[test]
fn complete_tool_call_detection_is_narrow() {
    let complete = json!({
        "method": "tools/call",
        "params": {"name": "show_event", "arguments": {"content": "complete"}},
    });
    assert!(is_complete_content_tool_call(&complete));

    for message in [
        json!({"method": "tools/call", "params": {"name": "show_event", "arguments": {"content": "indexed"}}}),
        json!({"method": "tools/call", "params": {"name": "search", "arguments": {"content": "complete"}}}),
        json!({"method": "ping"}),
    ] {
        assert!(!is_complete_content_tool_call(&message));
    }

    let blame = json!({
        "method": "tools/call",
        "params": {"name": "blame", "arguments": {"target": {"kind": "commit", "oid": "abc"}}},
    });
    assert!(is_blame_tool_call(&blame));
    assert!(!is_blame_tool_call(&json!({
        "method": "tools/call",
        "params": {"name": "search", "arguments": {}},
    })));
}

#[test]
fn final_mcp_serialization_is_bounded_after_json_expansion() {
    let (response, response_id, event_id) = expanded_response();
    let serialized_bytes = serialized_json_line_bytes(&response).unwrap();
    assert!(serialized_bytes > TEST_OUTPUT_LIMIT);

    let bounded =
        bound_complete_content_mcp_response(response, response_id.clone(), TEST_OUTPUT_LIMIT);
    let encoded = serde_json::to_string(&bounded).unwrap();
    let decoded: Value = serde_json::from_str(&encoded).unwrap();

    assert_eq!(decoded["id"], response_id);
    assert_eq!(decoded["result"]["isError"], true);
    assert_eq!(
        decoded["result"]["structuredContent"]["error_code"],
        "content_too_large"
    );
    assert_eq!(
        decoded["result"]["structuredContent"]["ctx_event_id"],
        event_id.to_string()
    );
    assert!(serialized_json_line_bytes(&decoded).unwrap() <= TEST_OUTPUT_LIMIT);
}

#[test]
fn final_mcp_serialization_preserves_exact_response_at_the_limit() {
    let (response, response_id, _) = expanded_response();
    let exact_limit = serialized_json_line_bytes(&response).unwrap();
    let bounded = bound_complete_content_mcp_response(response.clone(), response_id, exact_limit);

    assert_eq!(bounded, response);
    assert_eq!(serialized_json_line_bytes(&bounded).unwrap(), exact_limit);
}

#[test]
fn blame_response_bound_counts_structured_content_and_text_expansion() {
    let (response, response_id, structured) = expanded_blame_response();
    let structured_only = success_response(
        response_id.clone(),
        json!({"structuredContent": structured}),
    );
    let output_limit = serialized_json_line_bytes(&structured_only).unwrap() + 1;
    assert!(serialized_json_line_bytes(&response).unwrap() > output_limit);

    let bounded = bound_blame_mcp_response(response, response_id, output_limit);

    assert_eq!(bounded["result"]["isError"], true);
    assert_eq!(
        bounded["result"]["structuredContent"]["error_code"],
        "invalid_response"
    );
    assert!(bounded["result"]["content"][0]["text"]
        .as_str()
        .is_some_and(|text| text.contains("lower `limit`")));
    assert!(bounded["result"]["structuredContent"].get("next").is_none());
    assert!(serialized_json_line_bytes(&bounded).unwrap() <= output_limit);
}

#[test]
fn blame_response_preserves_the_exact_typed_result_at_the_boundary() {
    let (response, response_id, _) = expanded_blame_response();
    let exact_limit = serialized_json_line_bytes(&response).unwrap();

    let bounded = bound_blame_mcp_response(response.clone(), response_id, exact_limit);

    assert_eq!(bounded, response);
    assert_eq!(serialized_json_line_bytes(&bounded).unwrap(), exact_limit);
}
