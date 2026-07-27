mod support;

use support::*;

#[test]
fn mcp_always_advertises_the_stable_pro_surface() {
    let temp = tempdir();
    let responses = mcp_roundtrip_with_env(
        &temp,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "ctx-pro-test", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list"
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": { "name": "pro_status", "arguments": {} }
            }),
        ],
        &[("CTX_PRO_CHANNEL", "staging")],
    );

    let tools = responses[1]["result"]["tools"].as_array().unwrap();
    for expected in [
        "pro_status",
        "show_resource",
        "locate_resource",
        "blame",
        "timeline",
        "related",
        "facts",
    ] {
        assert!(
            tools.iter().any(|tool| tool["name"] == expected),
            "missing stable Pro tool {expected}"
        );
    }
    assert!(tools.iter().all(|tool| !matches!(
        tool["name"].as_str(),
        Some("materialize" | "pro_install" | "pro_update")
    )));
    for tool in tools.iter().filter(|tool| {
        matches!(
            tool["name"].as_str(),
            Some("show_resource" | "locate_resource" | "blame" | "timeline" | "related" | "facts")
        )
    }) {
        assert_eq!(tool["annotations"]["readOnlyHint"], false);
        assert_eq!(tool["annotations"]["idempotentHint"], true);
    }
    let status = &responses[2]["result"]["structuredContent"];
    assert_eq!(status["installed"], false);
    assert_eq!(status["error_code"], "pro_not_installed");
    assert_eq!(status["access_state"], serde_json::Value::Null);
    assert_eq!(status["refresh_after_unix"], serde_json::Value::Null);
    assert_eq!(status["access_deadline_unix"], serde_json::Value::Null);
    assert_eq!(status["grace_deadline_unix"], serde_json::Value::Null);

    let facts = tools.iter().find(|tool| tool["name"] == "facts").unwrap();
    let target = &facts["inputSchema"]["properties"]["target"]["properties"];
    assert_eq!(
        target["kind"]["enum"],
        serde_json::json!(ctx_pro_host_protocol::ResourceKind::ALL
            .into_iter()
            .map(ctx_pro_host_protocol::ResourceKind::wire_name)
            .collect::<Vec<_>>())
    );
    assert!(target["value"]["description"]
        .as_str()
        .unwrap()
        .contains("Resource value"));
    assert!(target["repository"]["description"]
        .as_str()
        .unwrap()
        .contains("forge:github.com/ctxrs/ctx"));
    assert!(target["line"]["description"]
        .as_str()
        .unwrap()
        .contains("only when kind is file"));
    assert!(facts["inputSchema"]["properties"]["limit"]["description"]
        .as_str()
        .unwrap()
        .contains("Maximum cited records"));
}

#[test]
fn mcp_rejects_line_for_non_file_resources_with_a_typed_error() {
    let temp = tempdir();
    let mut requests = vec![
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "ctx-pro-test", "version": "0" }
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    ];
    for (id, kind) in ["commit", "pull_request", "issue", "session", "run"]
        .into_iter()
        .enumerate()
    {
        requests.push(json!({
            "jsonrpc": "2.0",
            "id": id + 2,
            "method": "tools/call",
            "params": {
                "name": "facts",
                "arguments": {
                    "target": { "kind": kind, "value": "example", "line": 42 }
                }
            }
        }));
    }

    let responses = mcp_roundtrip_with_env(&temp, &requests, &[]);
    for response in &responses[1..] {
        let result = &response["result"];
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"]["error"], "invalid_request");
        assert_eq!(result["structuredContent"]["error_code"], "invalid_request");
        assert_eq!(result["content"][0]["text"], "invalid_request");
    }
}

#[cfg(unix)]
#[test]
fn mcp_locate_resource_uses_the_distinct_locate_operation() {
    let temp = tempdir();
    let helper = temp.path().join("ctx-pro-locate");
    write_locate_helper(&helper);
    let requests = || {
        vec![
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "ctx-pro-test", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "locate_resource",
                    "arguments": {
                        "target": { "kind": "pull_request", "value": "ctxrs/ctx#42" }
                    }
                }
            }),
        ]
    };
    let missing = mcp_roundtrip_with_env(
        &temp,
        &requests(),
        &[("CTX_PRO_HELPER", helper.to_str().unwrap())],
    );
    let missing = &missing[1]["result"];
    assert_eq!(missing["isError"], true);
    assert_eq!(missing["structuredContent"]["error"], "source_unavailable");

    initialize_current_query_store(temp.path());
    let responses = mcp_roundtrip_with_env(
        &temp,
        &requests(),
        &[("CTX_PRO_HELPER", helper.to_str().unwrap())],
    );

    let result = &responses[1]["result"];
    assert_eq!(result["structuredContent"]["payload_type"], "pro_location");
    assert_eq!(
        result["structuredContent"]["target"]["kind"],
        "pull_request"
    );
    assert_eq!(
        result["structuredContent"]["results"][0]["resource"]["kind"],
        "pull_request"
    );
    assert_eq!(
        result["structuredContent"]["results"][0]["summary"],
        "Exact canonical evidence location"
    );
    let text = result["content"][0]["text"].as_str().unwrap();
    assert_eq!(
        text,
        "ctx locate resource\npayload_type: pro_location\nschema_version: 1\ntarget.kind: pull_request\ntarget.value: ctxrs/ctx#42\nstale: false\nresults: 1\ncitations: 1\npagination.truncated: false\n\n1. ctxrs/ctx#42\n   resource.id: pull_request:ctxrs/ctx#42\n   resource.kind: pull_request\n   resource.display: ctxrs/ctx#42\n   summary: Exact canonical evidence location\n   occurred_at_ms: 1\n   facts: 0\n   citations: 1\n   record_citation 1\n      event_id: 00000000-0000-0000-0000-000000000001\n      event_seq: 1\n\nsuggested_next_commands: 2\n1. command: ctx facts pr 'ctxrs/ctx#42'\n2. command: ctx timeline pr 'ctxrs/ctx#42'\n"
    );
}

#[cfg(unix)]
#[test]
fn mcp_preserves_typed_key_store_codes_without_helper_details_or_paths() {
    for error_code in ["key_store_unavailable", "key_store_locked"] {
        let temp = tempdir();
        initialize_empty_store(&temp);
        initialize_pro_installation_identity(temp.path());
        let helper = temp.path().join(format!("ctx-pro-{error_code}"));
        write_startup_error_helper(&helper, error_code);
        let responses = mcp_roundtrip_with_env(
            &temp,
            &[
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "clientInfo": { "name": "ctx-pro-test", "version": "0" }
                    }
                }),
                json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/initialized"
                }),
                json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": { "name": "pro_status", "arguments": {} }
                }),
                json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "tools/call",
                    "params": {
                        "name": "facts",
                        "arguments": {
                            "target": { "kind": "commit", "value": "abc" }
                        }
                    }
                }),
            ],
            &[("CTX_PRO_HELPER", helper.to_str().unwrap())],
        );

        let status = &responses[1]["result"]["structuredContent"];
        assert_eq!(status["installed"], true);
        assert_eq!(status["ready"], false);
        assert_eq!(status["error_code"], error_code);
        assert!(status.get("helper_path").is_none());

        let result = &responses[2]["result"];
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"]["error"], error_code);
        assert_eq!(result["structuredContent"]["error_code"], error_code);
        assert_eq!(result["content"][0]["text"], error_code);

        let serialized = serde_json::to_string(&responses).unwrap();
        assert!(!serialized.contains("private helper detail"));
        assert!(!serialized.contains("/secret/key-store/path"));
        assert!(!serialized.contains(helper.to_str().unwrap()));
        assert!(!serialized.contains("helper_crashed"));
    }
}
