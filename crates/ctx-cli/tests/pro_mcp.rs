mod support;

use support::*;

fn initialize_request() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "ctx-pro-test", "version": "0" }
        }
    })
}

fn initialized_notification() -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    })
}

fn take_status_observation_time(status: &mut Value) -> Value {
    status["daemon"]["supervisor"]["environment_snapshot"]
        .as_object_mut()
        .and_then(|snapshot| snapshot.remove("current_observed_at_ms"))
        .expect("status environment snapshot observation time")
}

fn assert_pro_cli_mcp_status_parity(temp: &TempDir, envs: &[(&str, &str)]) -> (Value, Value) {
    let mut command = ctx(temp);
    command
        .args(["status", "--format=json"])
        .env("CTX_DAEMON_AUTOSTART_OFF", "1");
    for (key, value) in envs {
        command.env(key, value);
    }
    let cli = json_output(&mut command);

    let mut mcp_envs = vec![("CTX_DAEMON_AUTOSTART_OFF", "1")];
    mcp_envs.extend_from_slice(envs);
    let responses = mcp_roundtrip_with_env(
        temp,
        &[
            initialize_request(),
            json!({
                "jsonrpc": "2.0",
                "id": "status",
                "method": "tools/call",
                "params": { "name": "status", "arguments": {} }
            }),
        ],
        &mcp_envs,
    );
    assert_eq!(responses.len(), 2, "{responses:#?}");
    let result = responses[1]["result"].clone();
    assert!(result["isError"].is_null(), "{result:#}");

    let mut comparable_cli = cli.clone();
    let mut comparable_mcp = result["structuredContent"].clone();
    let cli_observed_at = take_status_observation_time(&mut comparable_cli);
    let mcp_observed_at = take_status_observation_time(&mut comparable_mcp);
    assert!(cli_observed_at.is_number(), "{cli:#}");
    assert!(mcp_observed_at.is_number(), "{result:#}");
    assert_eq!(comparable_mcp, comparable_cli, "{result:#}");
    (cli, result)
}

#[test]
fn mcp_advertises_read_only_status_and_locally_mutating_blame() {
    let disclosure = "Blame may perform bounded local catch-up that updates the canonical Core index, writes the encrypted derived Pro graph, and writes the projection acknowledgement. It never writes provider history or repositories.";
    let temp = tempdir();
    let responses = mcp_roundtrip_with_env(
        &temp,
        &[
            initialize_request(),
            initialized_notification(),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list"
            }),
        ],
        &[],
    );
    assert!(responses[0]["result"]["instructions"]
        .as_str()
        .is_some_and(|instructions| instructions.contains(disclosure)));
    let tools = responses[1]["result"]["tools"].as_array().unwrap();
    let blame = tools.iter().find(|tool| tool["name"] == "blame").unwrap();
    let pro_status = tools
        .iter()
        .find(|tool| tool["name"] == "pro_status")
        .unwrap();
    for removed in [
        "show_resource",
        "locate_resource",
        "timeline",
        "related",
        "facts",
    ] {
        assert!(
            tools.iter().all(|tool| tool["name"] != removed),
            "obsolete Pro tool {removed} remained advertised"
        );
    }
    assert_eq!(pro_status["annotations"]["readOnlyHint"], true);
    assert_eq!(blame["annotations"]["readOnlyHint"], false);
    assert_eq!(blame["annotations"]["destructiveHint"], false);
    assert_eq!(blame["annotations"]["idempotentHint"], true);
    assert!(blame["description"]
        .as_str()
        .is_some_and(|description| description.contains(disclosure)));
    assert_eq!(blame["inputSchema"]["properties"]["limit"]["default"], 8);
    assert_eq!(blame["inputSchema"]["properties"]["limit"]["maximum"], 8);
    assert_eq!(
        blame["inputSchema"]["properties"]["target"]["oneOf"]
            .as_array()
            .map(Vec::len),
        Some(3)
    );
}

#[cfg(unix)]
#[test]
fn generic_mcp_status_exactly_matches_cli_json_with_pro_present() {
    let temp = tempdir();
    let helper = temp.path().join("ctx-pro-status");
    write_status_helper(&helper);
    let root = data_root(&temp);
    assert!(!root.exists());

    let (cli, result) = assert_pro_cli_mcp_status_parity(
        &temp,
        &[
            ("CTX_LOCAL_USAGE_ENABLED", "false"),
            ("CTX_PRO_HELPER", helper.to_str().unwrap()),
        ],
    );

    assert_eq!(cli["pro"]["installed"], true, "{cli:#}");
    assert_ne!(cli["pro"]["state"], "not_setup", "{cli:#}");
    assert_eq!(cli["pro"]["payload_type"], "pro_status", "{cli:#}");
    assert_eq!(cli["read_only"], true, "{cli:#}");
    let text = mcp_content_text(&result);
    assert!(!text.contains("pro_status"), "{text}");
    assert!(
        !root.exists(),
        "CLI and generic MCP status must not initialize the data root for Pro status"
    );
}

#[test]
fn mcp_blame_rejects_non_launch_targets_and_invalid_bounds() {
    let temp = tempdir();
    let cases = [
        (
            "issue",
            json!({"target": {"kind": "issue", "selector": "42"}}),
            "target.kind must be file, commit, or pull_request",
        ),
        (
            "numeric-pr-without-repository",
            json!({"target": {"kind": "pull_request", "selector": "42"}}),
            "pull request number requires a repository selector",
        ),
        (
            "zero-pr",
            json!({"target": {"kind": "pull_request", "selector": "0", "repository": "ctxrs/ctx"}}),
            "pull request selector must be a positive decimal number or canonical supported PR URL",
        ),
        (
            "malformed-pr-url",
            json!({"target": {"kind": "pull_request", "selector": "https://gitlab.example.com/a/b/merge_requests/42"}}),
            "pull request selector must be a positive decimal number or canonical supported PR URL",
        ),
        (
            "bad-lines",
            json!({"target": {"kind": "file", "path": "src/lib.rs", "lines": {"start": 9, "end": 2}}}),
            "line range must be positive and inclusive",
        ),
        (
            "limit",
            json!({"target": {"kind": "commit", "oid": "abc"}, "limit": 9}),
            "limit must be between 1 and 8",
        ),
    ];
    let mut requests = vec![initialize_request(), initialized_notification()];
    requests.extend(cases.iter().map(|(id, arguments, _)| {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": "blame", "arguments": arguments }
        })
    }));
    let responses = mcp_roundtrip_with_env(&temp, &requests, &[]);
    for (response, (_, _, expected)) in responses[1..].iter().zip(cases) {
        let result = &response["result"];
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"]["error_code"], "invalid_request");
        assert!(
            result["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains(expected)),
            "{result:#?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn mcp_blame_returns_exact_typed_json_and_complete_text_fallback() {
    let temp = tempdir();
    let root = data_root(&temp);
    initialize_current_query_store(&root);
    let helper = root.join("ctx-pro-blame");
    write_blame_helper(&helper);
    let responses = mcp_roundtrip_with_env(
        &temp,
        &[
            initialize_request(),
            initialized_notification(),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "blame",
                    "arguments": {
                        "target": {"kind": "commit", "oid": "0123456789abcdef"}
                    }
                }
            }),
        ],
        &[("CTX_PRO_HELPER", helper.to_str().unwrap())],
    );
    let result = &responses[1]["result"];
    assert!(result["isError"].is_null(), "{result:#}");
    let structured = &result["structuredContent"];
    assert_eq!(structured["target"]["kind"], "commit");
    assert_eq!(structured["matches"][0]["kind"], "commit");
    assert_eq!(structured["evidence"].as_array().map(Vec::len), Some(1));
    assert!(structured.get("payload_type").is_none());

    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("matches: 1"));
    assert!(text.contains("object.display: session-producer"));
    assert!(text.contains("event_id: 00000000-0000-0000-0000-000000000001"));
    assert!(!text.contains("omitted"));
    assert!(!text.contains("payload_type"));
}

#[cfg(unix)]
#[test]
fn missing_blame_resource_matches_cli_json_error_code() {
    let temp = tempdir();
    let root = data_root(&temp);
    initialize_current_query_store(&root);
    let helper = root.join("ctx-pro-blame-missing-resource");
    write_blame_error_helper(&helper, "resource_not_found");
    let responses = mcp_roundtrip_with_env(
        &temp,
        &[
            initialize_request(),
            initialized_notification(),
            json!({
                "jsonrpc": "2.0",
                "id": "missing-resource",
                "method": "tools/call",
                "params": {
                    "name": "blame",
                    "arguments": {
                        "target": {"kind": "commit", "oid": "0123456789abcdef"}
                    }
                }
            }),
        ],
        &[("CTX_PRO_HELPER", helper.to_str().unwrap())],
    );
    let result = &responses[1]["result"];
    assert_eq!(result["isError"], true, "{result:#}");
    assert_eq!(result["structuredContent"]["error"], "resource_not_found");
    assert_eq!(
        result["structuredContent"]["error_code"],
        "resource_not_found"
    );
    assert_eq!(result["content"][0]["text"], "resource_not_found");
}

#[cfg(unix)]
#[test]
fn mcp_blame_fails_intact_when_helper_page_exceeds_aggregate_cap() {
    let temp = tempdir();
    let root = data_root(&temp);
    initialize_current_query_store(&root);
    let helper = root.join("ctx-pro-blame");
    write_oversized_blame_helper(&helper);
    let responses = mcp_roundtrip_with_env(
        &temp,
        &[
            initialize_request(),
            initialized_notification(),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "blame",
                    "arguments": {
                        "target": {"kind": "commit", "oid": "0123456789abcdef"}
                    }
                }
            }),
        ],
        &[("CTX_PRO_HELPER", helper.to_str().unwrap())],
    );
    let response = &responses[1];
    let result = &response["result"];
    assert_eq!(result["isError"], true, "{response:#}");
    assert_eq!(
        result["structuredContent"]["error_code"],
        "invalid_response"
    );
    assert!(result["content"][0]["text"]
        .as_str()
        .is_some_and(|text| text.contains("lower `limit`")));
    assert!(result["structuredContent"].get("matches").is_none());
    assert!(result["structuredContent"].get("evidence").is_none());
    assert!(result["structuredContent"].get("next").is_none());
    assert!(
        serde_json::to_vec(response).unwrap().len() < 1024 * 1024,
        "oversize replacement escaped the final MCP cap"
    );
}

#[cfg(unix)]
#[test]
fn mcp_pr_activity_does_not_claim_commit_membership() {
    let temp = tempdir();
    let root = data_root(&temp);
    initialize_current_query_store(&root);
    let helper = root.join("ctx-pro-blame");
    write_blame_helper(&helper);
    let responses = mcp_roundtrip_with_env(
        &temp,
        &[
            initialize_request(),
            initialized_notification(),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "blame",
                    "arguments": {
                        "target": {
                            "kind": "pull_request",
                            "selector": "https://gitlab.example.com/ctxrs/ctx/-/merge_requests/42"
                        }
                    }
                }
            }),
        ],
        &[("CTX_PRO_HELPER", helper.to_str().unwrap())],
    );
    let structured = &responses[1]["result"]["structuredContent"];
    assert_eq!(
        structured["matches"][0]["value"]["relationship"]["kind"], "activity",
        "{structured:#}"
    );
    assert!(structured["matches"].as_array().is_some_and(|matches| {
        matches
            .iter()
            .all(|value| value["value"]["relationship"]["kind"] != "commit")
    }));
}
