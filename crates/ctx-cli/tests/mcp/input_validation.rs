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
        ("bad-sql", "sql", json!({}), "sql is required"),
        (
            "bad-show",
            "show_event",
            json!({"ctx_event_id": "not-a-uuid"}),
            "invalid ctx_event_id",
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
        (
            "bad-pro-target-kind",
            "blame",
            json!({"target": {"kind": "unknown", "oid": "abc123"}}),
            "target.kind must be file, commit, or pull_request",
        ),
        (
            "bad-pro-target-argument",
            "blame",
            json!({
                "target": {"kind": "commit", "oid": "abc123", "unexpected": true}
            }),
            "unknown target argument unexpected",
        ),
        (
            "bad-pro-argument",
            "blame",
            json!({
                "target": {"kind": "commit", "oid": "abc123"},
                "unexpected": true
            }),
            "unknown argument unexpected",
        ),
        (
            "bad-pro-limit",
            "blame",
            json!({"target": {"kind": "commit", "oid": "abc123"}, "limit": 0}),
            "limit must be between 1 and 8",
        ),
        (
            "bad-pro-cursor",
            "blame",
            json!({"target": {"kind": "commit", "oid": "abc123"}, "cursor": ""}),
            "cursor must contain 1 to",
        ),
        (
            "bad-pro-cursor-encoding",
            "blame",
            json!({"target": {"kind": "commit", "oid": "abc123"}, "cursor": "é"}),
            "cursor must contain 1 to",
        ),
        (
            "bad-pro-selector",
            "blame",
            json!({"target": {"kind": "pull_request", "selector": "0", "repository": "ctxrs/ctx"}}),
            "pull request selector must be a positive decimal number",
        ),
        (
            "bad-pro-lines",
            "blame",
            json!({"target": {"kind": "file", "path": "src/lib.rs", "lines": {"start": 4, "end": 2}}}),
            "line range must be positive and inclusive",
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
                "query": "valid input reaches the source-backed index",
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
    assert!(index_failure["structuredContent"]["error_code"].is_null());
    assert!(index_failure["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("the source-backed index does not exist; retry with daemon refresh enabled"));

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
