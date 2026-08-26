mod support;

use std::fs;

use serde_json::{json, Value};

use support::*;

fn telemetry_roundtrip(
    temp: &tempfile::TempDir,
    data_root: &std::path::Path,
    events_path: &std::path::Path,
    stdin: Vec<u8>,
) -> Vec<Value> {
    let output = ctx(temp)
        .args(["mcp", "serve"])
        .env("CTX_DATA_ROOT", data_root)
        .env("CTX_ANALYTICS_ENABLED", "true")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(events_path))
        .env("CTX_DAEMON_ENABLED", "false")
        .env("CTX_UPGRADE_AUTO", "off")
        .write_stdin(stdin)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("MCP stdout must contain only JSON-RPC"))
        .collect()
}

fn push_message(stdin: &mut Vec<u8>, message: Value) {
    serde_json::to_writer(&mut *stdin, &message).unwrap();
    stdin.push(b'\n');
}

#[test]
fn mcp_telemetry_is_content_free_coalesced_and_stdout_pure() {
    let temp = tempdir();
    let data_root = temp.path().join("data");
    let events_path = temp.path().join("mcp-analytics.jsonl");
    fs::create_dir_all(&data_root).unwrap();

    let client_sentinel = "PRIVATE_CLIENT_NAME_SENTINEL";
    let query_sentinel = "PRIVATE_QUERY_SQL_TARGET_SENTINEL";
    let notification_sentinel = "notifications/PRIVATE_NOTIFICATION_SENTINEL";
    let tool_sentinel = "PRIVATE_TOOL_NAME_SENTINEL";
    let malformed_sentinel = "PRIVATE_MALFORMED_SENTINEL";
    let mut stdin = Vec::new();
    push_message(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": "PRIVATE_JSON_RPC_ID_SENTINEL",
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": client_sentinel, "version": "secret-version"}
            }
        }),
    );
    push_message(
        &mut stdin,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    push_message(
        &mut stdin,
        json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
    );
    push_message(
        &mut stdin,
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}),
    );
    push_message(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {"name": "status", "arguments": {}}
        }),
    );
    push_message(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {"name": "sources", "arguments": {}}
        }),
    );
    push_message(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": {"name": "search", "arguments": {"query": query_sentinel}}
        }),
    );
    push_message(
        &mut stdin,
        json!({"jsonrpc": "2.0", "method": notification_sentinel}),
    );
    push_message(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": tool_sentinel,
                "arguments": {"target": query_sentinel}
            }
        }),
    );
    stdin.extend_from_slice(
        format!("{{\"jsonrpc\":\"2.0\",\"id\":8,\"{malformed_sentinel}\"\n").as_bytes(),
    );

    let responses = telemetry_roundtrip(&temp, &data_root, &events_path, stdin);
    assert_eq!(responses.len(), 8);
    assert!(responses
        .iter()
        .all(|response| response.get("jsonrpc") == Some(&json!("2.0"))));

    let payloads = read_analytics_events(&events_path);
    let events = payloads
        .iter()
        .flat_map(|payload| payload["events"].as_array().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 7, "unexpected MCP telemetry: {events:#?}");
    assert!(events.iter().all(|event| event["surface"] == "mcp"));
    assert_eq!(
        events
            .iter()
            .filter(|event| event["event_name"] == "runtime_observation")
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["event_name"] == "operation_completed")
            .count(),
        5
    );
    let operations = events
        .iter()
        .filter(|event| event["event_name"] == "operation_completed")
        .map(|event| event["operation"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        operations,
        ["status", "sources", "search", "unknown", "missing"]
    );
    assert!(!operations.contains(&"ping"));
    assert!(!operations.contains(&"tools_list"));
    let search = events
        .iter()
        .find(|event| event["operation"] == "search")
        .unwrap();
    assert_eq!(search["properties"]["search_output_served"], true);
    assert_eq!(search["properties"]["search_failure_phase"], "refresh");
    assert_eq!(search["properties"]["search_refresh_status"], "failed");

    let allowed_operation_properties = [
        "acceleration_candidate",
        "available_parallelism_bucket",
        "capability_snapshot_schema",
        "column_count_bucket",
        "cpu_vector_tier",
        "error_class",
        "error_layer",
        "events_truncated",
        "host_memory_bucket",
        "method",
        "query_duration_bucket",
        "refresh_duration_bucket",
        "response_bound",
        "result_count_bucket",
        "result_truncated",
        "rows_truncated",
        "search_backend_effective",
        "search_backend_requested",
        "search_candidate_core_bytes_decoded_bucket",
        "search_candidate_literal_root_family_count_bucket",
        "search_candidate_pool_truncated",
        "search_candidate_records_decoded_bucket",
        "search_candidate_rows_total_bucket",
        "search_candidate_session_count_bucket",
        "search_copy_cluster_availability",
        "search_diversification_changed_final_top_n",
        "search_diversification_status",
        "search_failure_phase",
        "search_final_candidate_pool_bucket",
        "search_largest_literal_root_candidate_share_bucket",
        "search_largest_session_candidate_share_bucket",
        "search_literal_root_candidate_coverage_bucket",
        "search_literal_root_concentration_availability",
        "search_output_duration_bucket",
        "search_output_served",
        "search_provider_copy_candidate_count_bucket",
        "search_provider_copy_candidate_share_bucket",
        "search_query_execution_count_bucket",
        "search_refresh_status",
        "search_refresh_source_count_bucket",
        "search_retrieval_round_count_bucket",
        "search_stop_reason",
        "tool",
        "values_truncated",
        "zero_result",
    ];
    let allowed_runtime_properties = [
        "acceleration_candidate",
        "available_parallelism_bucket",
        "capability_snapshot_schema",
        "cpu_vector_tier",
        "host_memory_bucket",
        "initialized",
        "initialized_notification_count_bucket",
        "malformed_request_count_bucket",
        "ping_count_bucket",
        "request_count_bucket",
        "stop_reason",
        "telemetry_dropped_count_bucket",
        "tool_failure_count_bucket",
        "tool_request_count_bucket",
        "tools_list_count_bucket",
        "unknown_notification_count_bucket",
    ];
    for event in &events {
        let properties = event["properties"].as_object().unwrap();
        let allowed = if event["event_name"] == "operation_completed" {
            &allowed_operation_properties[..]
        } else {
            &allowed_runtime_properties[..]
        };
        for key in properties.keys() {
            assert!(
                allowed.contains(&key.as_str()),
                "unexpected MCP property {key}: {event:#?}"
            );
        }
    }
    let stopped = events
        .iter()
        .find(|event| event["operation"] == "stopped")
        .unwrap();
    assert_eq!(stopped["properties"]["ping_count_bucket"], "1");
    assert_eq!(stopped["properties"]["tools_list_count_bucket"], "1");
    assert_eq!(
        stopped["properties"]["initialized_notification_count_bucket"],
        "1"
    );
    assert_eq!(
        stopped["properties"]["unknown_notification_count_bucket"],
        "1"
    );
    assert_no_json_string_contains(
        &Value::Array(payloads),
        &[
            client_sentinel,
            query_sentinel,
            notification_sentinel,
            tool_sentinel,
            malformed_sentinel,
            "PRIVATE_JSON_RPC_ID_SENTINEL",
            "secret-version",
        ],
    );
}

#[test]
fn mcp_opt_out_creates_no_identity_or_sink_output() {
    let temp = tempdir();
    let data_root = temp.path().join("data");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let events_path = temp.path().join("disabled-analytics.jsonl");
    fs::create_dir_all(&data_root).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::write(data_root.join("config.toml"), "analytics.enabled = false\n").unwrap();

    let mut stdin = Vec::new();
    push_message(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2025-11-25", "capabilities": {}}
        }),
    );
    push_message(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "status", "arguments": {}}
        }),
    );

    let output = ctx(&temp)
        .args(["mcp", "serve"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env("CTX_ANALYTICS_ENABLED", "true")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .write_stdin(stdin)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(String::from_utf8(output).unwrap().lines().count(), 2);
    assert!(!events_path.exists());
    assert!(!expected_device_path(&home, &state).exists());
}
