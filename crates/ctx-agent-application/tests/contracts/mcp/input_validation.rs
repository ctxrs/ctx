use super::*;

#[test]
fn mcp_tool_input_validation_returns_stable_invalid_request_and_server_recovers() {
    let temp = tempdir();
    let cases = [
        (
            "bad-search-limit",
            "search",
            json!({"query": "onboarding", "limit": "five"}),
            "limit must be a non-negative integer",
        ),
        (
            "bad-show",
            "show_event",
            json!({"ctx_event_id": "not-a-uuid"}),
            "invalid ctx_event_id",
        ),
        (
            "bad-show-session-limit-type",
            "show_session",
            json!({
                "ctx_session_id": "00000000-0000-0000-0000-000000000000",
                "limit": "two"
            }),
            "limit must be a non-negative integer",
        ),
        (
            "bad-show-session-limit-zero",
            "show_session",
            json!({
                "ctx_session_id": "00000000-0000-0000-0000-000000000000",
                "limit": 0
            }),
            "limit must be between 1 and 4096",
        ),
        (
            "bad-show-session-limit-large",
            "show_session",
            json!({
                "ctx_session_id": "00000000-0000-0000-0000-000000000000",
                "limit": 4097
            }),
            "limit must be between 1 and 4096",
        ),
        (
            "bad-show-session-cursor-type",
            "show_session",
            json!({
                "ctx_session_id": "00000000-0000-0000-0000-000000000000",
                "cursor": 7
            }),
            "cursor must be a string",
        ),
        (
            "bad-show-session-cursor-empty",
            "show_session",
            json!({
                "ctx_session_id": "00000000-0000-0000-0000-000000000000",
                "cursor": ""
            }),
            "cursor must contain 1 to 4096 ASCII bytes",
        ),
        (
            "bad-show-session-cursor-encoding",
            "show_session",
            json!({
                "ctx_session_id": "00000000-0000-0000-0000-000000000000",
                "cursor": "é"
            }),
            "cursor must contain 1 to 4096 ASCII bytes",
        ),
        (
            "removed-show-content",
            "show_event",
            json!({
                "ctx_event_id": "00000000-0000-0000-0000-000000000000",
                "content": "complete"
            }),
            "unknown argument content",
        ),
        (
            "removed-include-subagents",
            "search",
            json!({
                "query": "onboarding",
                "include_subagents": true
            }),
            "unknown argument include_subagents",
        ),
        (
            "unreleased-source-scopes-spelling",
            "search",
            json!({
                "query": "onboarding",
                "scopes": ["work"]
            }),
            "unknown argument scopes",
        ),
        (
            "bad-since",
            "search",
            json!({"query": "onboarding", "since": "yesterday"}),
            "invalid --since value",
        ),
        (
            "bad-event-type",
            "search",
            json!({"query": "onboarding", "event_type": "not-an-event"}),
            "invalid EventType value",
        ),
        (
            "bad-history-source",
            "search",
            json!({
                "query": "onboarding",
                "provider": "custom",
                "history_source": "missing-separator"
            }),
            "--history-source expects plugin/source or provider_key/source_id",
        ),
        (
            "bad-source-id",
            "search",
            json!({"query": "onboarding", "source_id": " "}),
            "--source-id cannot be empty",
        ),
        (
            "bad-source-provider",
            "search",
            json!({
                "query": "onboarding",
                "provider": "codex",
                "provider_key": "custom-provider"
            }),
            "custom history source filters can only be combined with --provider custom",
        ),
    ];

    let mut requests = vec![json!({
        "jsonrpc": "2.0",
        "id": "init",
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "ctx-test", "version": "0" }
        }
    })];
    requests.extend(cases.iter().map(|(id, name, arguments, _)| {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments
            }
        })
    }));
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": "index-failure",
        "method": "tools/call",
        "params": {
            "name": "search",
            "arguments": {
                "query": "valid input reaches the Core index",
                "provider": "roo_code"
            }
        }
    }));
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": "status",
        "method": "tools/call",
        "params": {
            "name": "status",
            "arguments": {}
        }
    }));

    let responses = mcp_roundtrip(&temp, &requests);

    for (offset, (id, _, _, detail)) in cases.iter().enumerate() {
        let result = &responses[offset + 1]["result"];
        assert_eq!(responses[offset + 1]["id"], *id);
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"]["error_code"], "invalid_request");
        assert!(
            result["structuredContent"]["error"]
                .as_str()
                .unwrap()
                .contains(detail),
            "{id}: {result:#?}"
        );
        assert!(mcp_content_text(result).contains(detail));
    }

    let index_failure = &responses[cases.len() + 1]["result"];
    assert_eq!(index_failure["isError"], true);
    assert_eq!(
        index_failure["structuredContent"],
        json!({
            "error": "source_unavailable",
            "error_code": "source_unavailable",
        })
    );
    assert_eq!(mcp_content_text(index_failure), "source_unavailable");

    let recovered = &responses[cases.len() + 2]["result"];
    assert!(recovered["isError"].is_null());
    assert_eq!(recovered["structuredContent"]["schema_version"], 2);
    assert_eq!(recovered["structuredContent"]["read_only"], true);
}

#[test]
fn mcp_object_argument_errors_are_typed_but_non_objects_remain_json_rpc_invalid_params() {
    let temp = tempdir();
    let responses = mcp_roundtrip(
        &temp,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": "init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "ctx-test", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "unknown-argument",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "onboarding",
                        "refresh": "wait"
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "non-object",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": []
                }
            }),
        ],
    );

    let tool_error = &responses[1]["result"];
    assert_eq!(tool_error["isError"], true);
    assert_eq!(
        tool_error["structuredContent"]["error_code"],
        "invalid_request"
    );
    assert!(tool_error["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("unknown argument refresh"));

    let error = &responses[2]["error"];
    assert_eq!(error["code"], -32602);
    assert!(error["data"]["error"]
        .as_str()
        .unwrap()
        .contains("params.arguments must be an object"));
}
