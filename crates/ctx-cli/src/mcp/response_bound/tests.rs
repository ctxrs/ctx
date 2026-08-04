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
        "payload_type": "session_transcript",
        "ctx_session_id": "018f45d0-0000-7000-8000-000000000001",
        "mode": "log",
        "events": [{
            "ctx_event_id": event_id,
            "text": expanded,
        }],
        "pagination": {
            "limit": 1,
            "returned": 1,
            "has_more": true,
            "next_cursor": "opaque-next-page"
        },
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
fn show_tool_call_detection_covers_both_show_tools() {
    let event = json!({
        "method": "tools/call",
        "params": {"name": "show_event", "arguments": {}},
    });
    assert!(is_show_tool_call(&event));

    assert!(is_show_tool_call(&json!({
        "method": "tools/call",
        "params": {"name": "show_session", "arguments": {}},
    })));

    for message in [
        json!({"method": "tools/call", "params": {"name": "search", "arguments": {}}}),
        json!({"method": "ping"}),
    ] {
        assert!(!is_show_tool_call(&message));
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
fn query_events_detection_and_final_response_bound_are_exact() {
    let call = json!({
        "method": "tools/call",
        "params": {"name": "query_events", "arguments": {}},
    });
    assert!(is_query_events_tool_call(&call));
    assert!(!is_query_events_tool_call(&json!({
        "method": "tools/call",
        "params": {"name": "show_event", "arguments": {}},
    })));

    let response_id = json!(9);
    let exact_attribution = json!({
        "server": "server\n\u{202e}|`[]",
        "tool": "tool\\literal\t*#",
    });
    let response = success_response(
        response_id.clone(),
        tool_result(json!({
            "payload_type": "event_range_page",
            "events": [{
                "text": "x".repeat(2_000),
                "mcp_tool_call": exact_attribution,
            }],
            "next_cursor": "opaque-cursor"
        })),
    );
    let exact = serialized_json_line_bytes(&response).unwrap();
    let mut without_attribution = response.clone();
    without_attribution["result"]["structuredContent"]["events"][0]
        .as_object_mut()
        .unwrap()
        .remove("mcp_tool_call");
    assert!(serialized_json_line_bytes(&without_attribution).unwrap() < exact);
    assert_eq!(
        bound_query_events_mcp_response(response.clone(), response_id.clone(), exact),
        response
    );
    let one_byte_short =
        bound_query_events_mcp_response(response.clone(), response_id.clone(), exact - 1);
    assert_eq!(one_byte_short["result"]["isError"], true);
    assert_eq!(
        one_byte_short["result"]["structuredContent"]["error_code"],
        "output_limit_exceeded"
    );

    let bounded = bound_query_events_mcp_response(response, response_id, TEST_OUTPUT_LIMIT);
    assert_eq!(bounded["result"]["isError"], true);
    assert_eq!(
        bounded["result"]["structuredContent"]["error_code"],
        "output_limit_exceeded"
    );
    assert!(bounded["result"]["structuredContent"]
        .get("next_cursor")
        .is_none());
    assert!(serialized_json_line_bytes(&bounded).unwrap() <= TEST_OUTPUT_LIMIT);
}

#[test]
fn query_events_content_none_overflow_recommends_lower_limit() {
    let response_id = json!("content-none-attribution");
    let attribution = ctx_history_core::McpToolCallAttribution {
        server: "s".repeat(2_000),
        tool: "valid-tool".to_owned(),
    };
    attribution.validate_contract().unwrap();
    let mut event = json!({
        "ctx_event_id": "018f45d0-0000-7000-8000-000000000010",
        "text": null,
        "structured_content": null,
        "content_projection": "none",
    });
    let page_without_attribution = json!({
        "payload_type": "event_range_page",
        "events": [event.clone()],
    });
    let response_without_attribution =
        success_response(response_id.clone(), tool_result(page_without_attribution));
    assert!(
        serialized_json_line_bytes(&response_without_attribution).unwrap() <= TEST_OUTPUT_LIMIT
    );

    event["mcp_tool_call"] = serde_json::to_value(attribution).unwrap();
    let response = success_response(
        response_id.clone(),
        tool_result(json!({
            "payload_type": "event_range_page",
            "events": [event],
        })),
    );
    assert!(serialized_json_line_bytes(&response).unwrap() > TEST_OUTPUT_LIMIT);

    let bounded = bound_query_events_mcp_response(response, response_id, TEST_OUTPUT_LIMIT);
    for advice in [
        bounded["result"]["content"][0]["text"].as_str().unwrap(),
        bounded["result"]["structuredContent"]["recommendation"]
            .as_str()
            .unwrap(),
    ] {
        assert!(advice.contains("lower `limit`"));
        assert!(advice.contains("content=text"));
        assert!(advice.contains("content=none"));
    }
    assert!(serialized_json_line_bytes(&bounded).unwrap() <= TEST_OUTPUT_LIMIT);
}

#[test]
fn query_events_response_bound_counts_full_mcp_exchange_payloads() {
    let response_id = json!("mcp-exchange-bound");
    let response = success_response(
        response_id.clone(),
        tool_result(json!({
            "payload_type": "event_range_page",
            "events": [{
                "ctx_event_id": "018f45d0-0000-7000-8000-000000000010",
                "text": "body",
                "content_projection": "full",
                "mcp_exchange": {
                    "provider_call_id": "call",
                    "response": {
                        "status": "succeeded",
                        "text": {"capture_status": "normalized_body"},
                        "payload": {
                            "capture_status": "present",
                            "value": {
                                "nested": ["雪", null, {"large": "x".repeat(2_000)}]
                            }
                        }
                    }
                }
            }]
        })),
    );
    let actual = serialized_json_line_bytes(&response).unwrap();
    let mut without_exchange = response.clone();
    without_exchange["result"]["structuredContent"]["events"][0]
        .as_object_mut()
        .unwrap()
        .remove("mcp_exchange");
    let without_exchange_bytes = serialized_json_line_bytes(&without_exchange).unwrap();
    assert!(without_exchange_bytes < actual);
    let output_limit = TEST_OUTPUT_LIMIT.max(without_exchange_bytes);
    assert!(output_limit < actual);

    let bounded = bound_query_events_mcp_response(response, response_id, output_limit);
    assert_eq!(bounded["result"]["isError"], true);
    assert_eq!(
        bounded["result"]["structuredContent"]["actual_bytes"],
        actual
    );
    assert!(serialized_json_line_bytes(&bounded).unwrap() <= output_limit);
}

#[test]
fn final_mcp_serialization_is_bounded_after_json_expansion() {
    assert_eq!(
        crate::presentation_limit::MCP_PRESENTATION_MAX_OUTPUT_BYTES,
        8 * 1024 * 1024
    );
    let (response, response_id, event_id) = expanded_response();
    let serialized_bytes = serialized_json_line_bytes(&response).unwrap();
    assert!(serialized_bytes > TEST_OUTPUT_LIMIT);

    let bounded = bound_show_mcp_response(response, response_id.clone(), TEST_OUTPUT_LIMIT);
    let encoded = serde_json::to_string(&bounded).unwrap();
    let decoded: Value = serde_json::from_str(&encoded).unwrap();

    assert_eq!(decoded["id"], response_id);
    assert_eq!(decoded["result"]["isError"], true);
    assert_eq!(
        decoded["result"]["structuredContent"]["error_code"],
        "output_limit_exceeded"
    );
    assert_eq!(
        decoded["result"]["structuredContent"]["ctx_event_id"],
        event_id.to_string()
    );
    assert!(decoded["result"]["structuredContent"]
        .get("pagination")
        .is_none());
    assert!(!decoded["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("opaque-next-page"));
    assert!(serialized_json_line_bytes(&decoded).unwrap() <= TEST_OUTPUT_LIMIT);
}

#[test]
fn final_mcp_serialization_preserves_exact_response_at_the_limit() {
    let (response, response_id, _) = expanded_response();
    let exact_limit = serialized_json_line_bytes(&response).unwrap();
    let bounded = bound_show_mcp_response(response.clone(), response_id, exact_limit);

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
