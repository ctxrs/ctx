mod support;

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, ChildStdout, Command as StdCommand, Stdio},
};

use rusqlite::Connection;
use serde_json::{json, Value};
use support::*;

struct WireResponse {
    value: Value,
    wire_bytes: u64,
}

struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl McpClient {
    fn start(temp: &tempfile::TempDir) -> Self {
        let mut command = mcp_command(temp);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn notify(&mut self, message: Value) {
        write_message(&mut self.stdin, &message);
    }

    fn request(&mut self, message: Value) -> WireResponse {
        write_message(&mut self.stdin, &message);
        read_response(&mut self.stdout)
    }

    fn finish(mut self) {
        drop(self.stdin);
        let mut stderr = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        let status = self.child.wait().unwrap();
        assert!(status.success(), "MCP server exited {status}: {stderr}");
    }
}

fn mcp_command(temp: &tempfile::TempDir) -> StdCommand {
    let prepared = ctx(temp);
    let mut command = StdCommand::new(prepared.get_program());
    for (name, value) in prepared.get_envs() {
        match value {
            Some(value) => {
                command.env(name, value);
            }
            None => {
                command.env_remove(name);
            }
        }
    }
    command
        .args(["mcp", "serve"])
        .env("CTX_LOCAL_USAGE_ENABLED", "true");
    command
}

fn write_message(writer: &mut impl Write, message: &Value) {
    serde_json::to_writer(&mut *writer, message).unwrap();
    writer.write_all(b"\n").unwrap();
    writer.flush().unwrap();
}

fn read_response(reader: &mut impl BufRead) -> WireResponse {
    let mut line = Vec::new();
    let read = reader.read_until(b'\n', &mut line).unwrap();
    assert!(read > 0, "MCP server closed stdout before responding");
    assert_eq!(line.last(), Some(&b'\n'), "MCP response was not flushed");
    let value = serde_json::from_slice(&line[..line.len() - 1]).unwrap();
    WireResponse {
        value,
        wire_bytes: u64::try_from(line.len()).unwrap(),
    }
}

fn initialize(id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "mcp-local-usage-v2-test", "version": "0"}
        }
    })
}

fn tool_call(id: &str, name: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments}
    })
}

fn operation_calls(connection: &Connection) -> BTreeMap<String, i64> {
    let mut statement = connection
        .prepare(
            "SELECT operation, SUM(calls) \
             FROM daily_usage WHERE surface = 'mcp' GROUP BY operation ORDER BY operation",
        )
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn classified_calls(
    connection: &Connection,
    operation: &str,
    outcome: &str,
    value_class: &str,
) -> i64 {
    connection
        .query_row(
            "SELECT COALESCE(SUM(calls), 0) FROM daily_usage \
             WHERE surface = 'mcp' AND operation = ?1 AND outcome = ?2 AND value_class = ?3",
            [operation, outcome, value_class],
            |row| row.get(0),
        )
        .unwrap()
}

fn successful_semantic_context_bytes(response: &WireResponse) -> Option<i64> {
    let failed = response.value.get("error").is_some()
        || response
            .value
            .pointer("/result/isError")
            .and_then(Value::as_bool)
            == Some(true);
    if failed {
        return None;
    }
    let structured = response.value.pointer("/result/structuredContent").unwrap();
    Some(i64::try_from(serde_json::to_vec(structured).unwrap().len()).unwrap())
}

fn accumulate_semantic_context(
    response: &WireResponse,
    semantic_context_bytes: &mut i64,
    semantic_context_samples: &mut i64,
) {
    if let Some(bytes) = successful_semantic_context_bytes(response) {
        *semantic_context_bytes += bytes;
        *semantic_context_samples += 1;
    }
}

#[test]
fn delivered_foreground_tools_record_once_with_truthful_classification() {
    let temp = tempdir();
    let (_daemon, _) = import_custom_history_fixture_source_backed(&temp, "basic.jsonl");
    assert!(
        !temp.path().join("usage.sqlite").exists(),
        "fixture import and daemon work must stay outside foreground usage"
    );

    let mut client = McpClient::start(&temp);
    let initialized = client.request(initialize("initialize"));
    assert_eq!(initialized.value["result"]["serverInfo"]["name"], "ctx");
    client.notify(json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }));
    let ping = client.request(json!({
        "jsonrpc": "2.0",
        "id": "ping",
        "method": "ping"
    }));
    assert_eq!(ping.value["result"], json!({}));
    let listed = client.request(json!({
        "jsonrpc": "2.0",
        "id": "tools-list",
        "method": "tools/list"
    }));
    let exposed_tools = listed.value["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        exposed_tools,
        BTreeSet::from([
            "blame",
            "pro_status",
            "search",
            "show_event",
            "show_session",
            "sources",
            "sql",
            "status",
        ])
    );

    let query_marker = "MCP_USAGE_QUERY_MUST_NOT_PERSIST_7f3d";
    let search = client.request(tool_call(
        "search-result",
        "search",
        json!({"query": "parser test", "limit": 5}),
    ));
    let search_content = &search.value["result"]["structuredContent"];
    let results = search_content["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "{:#}", search.value);
    assert_eq!(
        search_content["result_window"],
        json!({"limit": 5, "returned": 1, "more_available": false})
    );
    let mut semantic_search_bytes = serde_json::to_vec(results).unwrap().len() as i64;
    let mut semantic_context_bytes = 0;
    let mut semantic_context_samples = 0;
    accumulate_semantic_context(
        &search,
        &mut semantic_context_bytes,
        &mut semantic_context_samples,
    );
    let mut expected_context_found = i64::try_from(results.len()).unwrap();
    let session_id = results[0]["ctx_session_id"].as_str().unwrap().to_owned();

    let mut delivered_wire_bytes = search.wire_bytes;
    let show_session = client.request(tool_call(
        "show-session",
        "show_session",
        json!({"ctx_session_id": session_id}),
    ));
    assert!(show_session.value["result"].get("isError").is_none());
    accumulate_semantic_context(
        &show_session,
        &mut semantic_context_bytes,
        &mut semantic_context_samples,
    );
    delivered_wire_bytes += show_session.wire_bytes;
    let event_search = client.request(tool_call(
        "search-event-result",
        "search",
        json!({"query": "parser test", "events": true, "limit": 5}),
    ));
    let event_results = event_search.value["result"]["structuredContent"]["results"]
        .as_array()
        .unwrap();
    assert_eq!(event_results.len(), 1, "{:#}", event_search.value);
    semantic_search_bytes += serde_json::to_vec(event_results).unwrap().len() as i64;
    accumulate_semantic_context(
        &event_search,
        &mut semantic_context_bytes,
        &mut semantic_context_samples,
    );
    expected_context_found += i64::try_from(event_results.len()).unwrap();
    let event_id = event_results[0]["ctx_event_id"]
        .as_str()
        .unwrap()
        .to_owned();
    delivered_wire_bytes += event_search.wire_bytes;
    let show_event = client.request(tool_call(
        "show-event",
        "show_event",
        json!({"ctx_event_id": event_id}),
    ));
    assert!(show_event.value["result"].get("isError").is_none());
    accumulate_semantic_context(
        &show_event,
        &mut semantic_context_bytes,
        &mut semantic_context_samples,
    );
    delivered_wire_bytes += show_event.wire_bytes;

    let mut other_responses = Vec::new();
    other_responses.push(client.request(tool_call("status", "status", json!({}))));
    let sources = client.request(tool_call("sources", "sources", json!({})));
    accumulate_semantic_context(
        &sources,
        &mut semantic_context_bytes,
        &mut semantic_context_samples,
    );
    other_responses.push(sources);
    let empty_search = client.request(tool_call(
        "search-empty",
        "search",
        json!({"query": query_marker, "limit": 5}),
    ));
    assert_eq!(
        empty_search.value["result"]["structuredContent"]["results"],
        json!([])
    );
    assert_eq!(
        empty_search.value["result"]["structuredContent"]["result_window"],
        json!({"limit": 5, "returned": 0, "more_available": false})
    );
    accumulate_semantic_context(
        &empty_search,
        &mut semantic_context_bytes,
        &mut semantic_context_samples,
    );
    other_responses.push(empty_search);
    let sql_failure = client.request(tool_call(
        "sql-failure",
        "sql",
        json!({"sql": "SELECT 1 AS one"}),
    ));
    assert_eq!(sql_failure.value["result"]["isError"], true);
    assert_ne!(
        sql_failure.value["result"]["structuredContent"]["error_code"],
        "invalid_request"
    );
    accumulate_semantic_context(
        &sql_failure,
        &mut semantic_context_bytes,
        &mut semantic_context_samples,
    );
    other_responses.push(sql_failure);
    other_responses.push(client.request(tool_call("pro-status", "pro_status", json!({}))));
    let blame = client.request(tool_call(
        "blame",
        "blame",
        json!({"target": {"kind": "commit", "oid": "abc123"}}),
    ));
    accumulate_semantic_context(
        &blame,
        &mut semantic_context_bytes,
        &mut semantic_context_samples,
    );
    other_responses.push(blame);
    delivered_wire_bytes += other_responses
        .iter()
        .map(|response| response.wire_bytes)
        .sum::<u64>();
    client.finish();

    let report = json_output(
        ctx(&temp)
            .args(["stats", "--detail", "--format=json"])
            .env("CTX_LOCAL_USAGE_ENABLED", "true"),
    );
    let expected_context = json!({
        "context_searches": 3,
        "context_found": expected_context_found,
        "context_opened": 2,
        "context_cited_coverage": "unsupported",
        "validated_discoveries": 2,
    });
    assert_eq!(
        report["local_usage"]["summary"]["context"], expected_context,
        "the flushed search and show completions must reach the persisted report"
    );
    assert_eq!(
        report["measured"]["history_retrieval"]["discovery_proxy"],
        expected_context
    );

    let usage_path = temp.path().join("usage.sqlite");
    let connection = Connection::open(&usage_path).unwrap();
    assert_eq!(
        operation_calls(&connection),
        BTreeMap::from([
            ("blame".to_owned(), 1),
            ("pro_status".to_owned(), 1),
            ("search".to_owned(), 3),
            ("show_event".to_owned(), 1),
            ("show_session".to_owned(), 1),
            ("sources".to_owned(), 1),
            ("sql".to_owned(), 1),
            ("status".to_owned(), 1),
        ])
    );
    assert_eq!(
        classified_calls(&connection, "search", "success", "result_bearing"),
        2
    );
    assert_eq!(
        classified_calls(&connection, "search", "success", "empty"),
        1
    );
    assert_eq!(
        classified_calls(&connection, "sql", "failure", "not_applicable"),
        1
    );
    let recorded_wire_bytes: i64 = connection
        .query_row(
            "SELECT SUM(response_bytes) FROM daily_usage WHERE surface = 'mcp'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(recorded_wire_bytes, delivered_wire_bytes as i64);

    let (recorded_context_bytes, context_byte_samples): (i64, i64) = connection
        .query_row(
            "SELECT SUM(context_bytes), SUM(context_byte_samples) \
             FROM daily_usage WHERE surface = 'mcp'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(recorded_context_bytes, semantic_context_bytes);
    assert_eq!(context_byte_samples, semantic_context_samples);
    let (recorded_search_bytes, search_result_byte_samples): (i64, i64) = connection
        .query_row(
            "SELECT SUM(search_result_bytes), SUM(search_result_byte_samples) \
             FROM daily_usage WHERE surface = 'mcp' AND operation = 'search'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(recorded_search_bytes, semantic_search_bytes);
    assert_eq!(search_result_byte_samples, 2);
    assert_ne!(recorded_search_bytes, recorded_wire_bytes);
    let context: (i64, i64, i64, i64) = connection
        .query_row(
            "SELECT SUM(context_searches), SUM(context_found), \
             SUM(context_opened), SUM(validated_discoveries) \
             FROM daily_usage WHERE surface = 'mcp'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        context,
        (3, expected_context_found, 2, 2),
        "search-to-show correlation must persist aggregates only"
    );
    drop(connection);

    let persisted = ["usage.sqlite", "usage.sqlite-wal", "usage.sqlite-shm"]
        .into_iter()
        .filter_map(|name| fs::read(temp.path().join(name)).ok())
        .flatten()
        .collect::<Vec<_>>();
    for forbidden in [
        session_id.as_bytes(),
        event_id.as_bytes(),
        query_marker.as_bytes(),
    ] {
        assert!(
            !persisted
                .windows(forbidden.len())
                .any(|window| window == forbidden),
            "usage storage persisted an MCP query or target identifier"
        );
    }
}

#[test]
fn protocol_control_and_invalid_input_create_no_usage_store() {
    let temp = tempdir();
    let messages = [
        "{not valid json}\n".to_owned(),
        serde_json::to_string(&tool_call("pre-init", "status", json!({}))).unwrap() + "\n",
        serde_json::to_string(&initialize("initialize")).unwrap() + "\n",
        serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
        .unwrap()
            + "\n",
        serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": "ping",
            "method": "ping"
        }))
        .unwrap()
            + "\n",
        serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": "tools-list",
            "method": "tools/list"
        }))
        .unwrap()
            + "\n",
        serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": "missing-name",
            "method": "tools/call",
            "params": {"arguments": {}}
        }))
        .unwrap()
            + "\n",
        serde_json::to_string(&tool_call("unknown", "unknown", json!({}))).unwrap() + "\n",
    ]
    .concat();
    ctx(&temp)
        .args(["mcp", "serve"])
        .env("CTX_LOCAL_USAGE_ENABLED", "true")
        .write_stdin(messages)
        .assert()
        .success();

    assert!(
        !temp.path().join("usage.sqlite").exists(),
        "protocol control, pre-init rejection, invalid JSON, and unknown tools must not count"
    );
}

#[test]
fn delivered_recognized_input_and_tool_failures_each_record_once() {
    let temp = tempdir();
    let mut client = McpClient::start(&temp);
    client.request(initialize("initialize"));

    let arguments_not_object =
        client.request(tool_call("arguments-not-object", "search", json!([])));
    assert_eq!(arguments_not_object.value["error"]["code"], -32602);
    let invalid_search = client.request(tool_call("invalid-search", "search", json!({})));
    assert_eq!(invalid_search.value["result"]["isError"], true);
    assert_eq!(
        invalid_search.value["result"]["structuredContent"]["error_code"],
        "invalid_request"
    );
    let invalid_status = client.request(tool_call(
        "invalid-status",
        "status",
        json!({"unexpected": true}),
    ));
    assert_eq!(invalid_status.value["result"]["isError"], true);
    let invalid_blame = client.request(tool_call(
        "invalid-blame",
        "blame",
        json!({"target": {"kind": "commit", "oid": ""}}),
    ));
    assert_eq!(invalid_blame.value["result"]["isError"], true);
    client.finish();

    let connection = Connection::open(temp.path().join("usage.sqlite")).unwrap();
    assert_eq!(
        operation_calls(&connection),
        BTreeMap::from([
            ("blame".to_owned(), 1),
            ("search".to_owned(), 2),
            ("status".to_owned(), 1),
        ])
    );
    assert_eq!(
        classified_calls(&connection, "search", "failure", "not_applicable"),
        2
    );
    assert_eq!(
        classified_calls(&connection, "status", "failure", "not_applicable"),
        1
    );
    let blame: (String, String, i64) = connection
        .query_row(
            "SELECT target_type, pro_outcome, calls FROM daily_usage \
             WHERE surface = 'mcp' AND operation = 'blame'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(blame, ("not_applicable".to_owned(), "error".to_owned(), 1));
    let recorded_wire_bytes: i64 = connection
        .query_row(
            "SELECT SUM(response_bytes) FROM daily_usage WHERE surface = 'mcp'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        recorded_wire_bytes,
        [
            arguments_not_object,
            invalid_search,
            invalid_status,
            invalid_blame,
        ]
        .iter()
        .map(|response| response.wire_bytes as i64)
        .sum::<i64>()
    );
}

#[cfg(unix)]
#[test]
fn stdout_delivery_failure_does_not_record_the_tool_call() {
    let temp = tempdir();
    let mut child = mcp_command(&temp)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    write_message(&mut stdin, &initialize("initialize"));
    let initialized = read_response(&mut stdout);
    assert_eq!(initialized.value["result"]["serverInfo"]["name"], "ctx");
    drop(stdout);

    write_message(&mut stdin, &tool_call("status", "status", json!({})));
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(
        !output.status.success(),
        "MCP server unexpectedly succeeded after its stdout reader closed"
    );
    assert!(
        !temp.path().join("usage.sqlite").exists(),
        "a tool response that could not be written and flushed must not count"
    );
}
