mod support;

use std::{
    collections::BTreeMap,
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
        Self::start_command(mcp_command(temp))
    }

    fn start_command(mut command: StdCommand) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        Self {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
        }
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

#[cfg(unix)]
#[test]
fn companion_blame_records_only_core_observed_delivery_facts() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempdir();
    let pro = temp.path().join("ctx-pro");
    fs::write(
        &pro,
        b"#!/bin/sh\nif [ \"$1\" = \"--ctx-pro-protocol-v3\" ] && [ \"$2\" = \"handshake\" ]; then\n  printf '{\"protocol_version\":3}\\n'\n  exit 0\nfi\nif [ \"$1\" = \"--ctx-pro-protocol-v3\" ] && [ \"$2\" = \"mcp-serve\" ]; then\n  IFS= read -r request || exit 92\n  printf '{\"jsonrpc\":\"2.0\",\"id\":\"blame\",\"result\":{\"opaque\":true}}\\n'\n  exit 0\nfi\nexit 91\n",
    )
    .unwrap();
    fs::set_permissions(&pro, fs::Permissions::from_mode(0o700)).unwrap();

    let mut command = mcp_command(&temp);
    command.env("CTX_PRO_PATH", &pro);
    let mut client = McpClient::start_command(command);
    client.request(initialize());
    let marker = "PRIVATE_BLAME_TARGET_MUST_NOT_PERSIST_51f2";
    let response = client.request(tool_call(
        "blame",
        "blame",
        json!({"private_target": marker}),
    ));
    assert_eq!(response.value["result"]["opaque"], true);
    client.finish();

    let usage_path = usage_db_path(&temp);
    let connection = Connection::open(&usage_path).unwrap();
    let row: (i64, String, String, i64, i64, i64) = connection
        .query_row(
            "SELECT definition_version, outcome, value_class, calls, result_count, \
                    delivered_output_bytes FROM daily_usage \
             WHERE surface = 'mcp' AND operation = 'blame'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        row,
        (
            3,
            "success".to_owned(),
            "not_applicable".to_owned(),
            1,
            0,
            response.wire_bytes as i64
        )
    );
    drop(connection);

    let persisted = fs::read(usage_path).unwrap();
    assert!(!persisted
        .windows(marker.len())
        .any(|window| window == marker.as_bytes()));
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
        .env("CTX_DAEMON_AUTOSTART_OFF", "1")
        .env("CTX_LOCAL_USAGE_ENABLED", "true");
    command
}

fn usage_db_path(temp: &tempfile::TempDir) -> std::path::PathBuf {
    data_root(temp).join("usage.sqlite")
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

fn initialize() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": "initialize",
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
            "SELECT operation, SUM(calls) FROM daily_usage \
             WHERE surface = 'mcp' GROUP BY operation ORDER BY operation",
        )
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn delivered_search_and_prefix_open_record_once_with_exact_transport_and_context_channels() {
    let temp = tempdir();
    let (_daemon, _) = import_custom_history_fixture_source_backed(&temp, "basic.jsonl");
    assert!(!usage_db_path(&temp).exists());

    let marker = "MCP_USAGE_QUERY_MUST_NOT_PERSIST_7f3d";
    let mut client = McpClient::start(&temp);
    assert_eq!(
        client.request(initialize()).value["result"]["serverInfo"]["name"],
        "ctx"
    );

    let search = client.request(tool_call(
        "search",
        "search",
        json!({"query": "parser test", "limit": 5}),
    ));
    let results = search.value["result"]["structuredContent"]["results"]
        .as_array()
        .unwrap();
    assert_eq!(results.len(), 1, "{:#}", search.value);
    let expected_context_bytes = results
        .iter()
        .map(|result| result["snippet"].as_str().unwrap().len())
        .sum::<usize>();
    let full_session_id = results[0]["ctx_session_id"].as_str().unwrap().to_owned();
    let prefix = full_session_id[..8].to_owned();

    let opened = client.request(tool_call(
        "show-session",
        "show_session",
        json!({"ctx_session_id": prefix}),
    ));
    assert_eq!(
        opened.value["result"]["structuredContent"]["ctx_session_id"],
        full_session_id
    );
    let opened_session = &opened.value["result"]["structuredContent"];
    assert_eq!(opened_session["provider_key"], "demo-agent");
    assert_eq!(opened_session["source_id"], "demo-source");
    assert_eq!(opened_session["provider_session_id"], "demo-session");
    assert!(
        opened_session["events"]
            .as_array()
            .is_some_and(|events| events.iter().all(|event| {
                event["provider_key"] == "demo-agent"
                    && event["source_id"] == "demo-source"
                    && event["provider_session_id"] == "demo-session"
            })),
        "{opened_session:#}"
    );
    let status = client.request(tool_call("status", "status", json!({})));
    let delivered_wire_bytes = search.wire_bytes + opened.wire_bytes + status.wire_bytes;
    client.finish();

    let usage_path = usage_db_path(&temp);
    let connection = Connection::open(&usage_path).unwrap();
    assert_eq!(
        operation_calls(&connection),
        BTreeMap::from([
            ("search".to_owned(), 1),
            ("show_session".to_owned(), 1),
            ("status".to_owned(), 1),
        ])
    );
    let recorded_output: i64 = connection
        .query_row(
            "SELECT SUM(delivered_output_bytes) FROM daily_usage WHERE surface = 'mcp'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(recorded_output, delivered_wire_bytes as i64);

    let search_row: (String, i64, i64, i64) = connection
        .query_row(
            "SELECT context_coverage, result_count, delivered_context_bytes, \
                    matched_normalized_session_bytes \
             FROM daily_usage WHERE surface = 'mcp' AND operation = 'search'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(search_row.0, "complete");
    assert_eq!(search_row.1, 1);
    assert_eq!(search_row.2, expected_context_bytes as i64);
    assert!(search_row.3 >= search_row.2);

    let columns = connection
        .prepare("PRAGMA table_info(daily_usage)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    for forbidden in [
        "query",
        "path",
        "content",
        "session_id",
        "event_id",
        "context_opened",
        "context_cited",
        "validated_discoveries",
        "target_type",
        "pro_outcome",
        "citation_count",
    ] {
        assert!(!columns.iter().any(|column| column == forbidden));
    }
    drop(connection);

    let persisted = ["usage.sqlite", "usage.sqlite-wal", "usage.sqlite-shm"]
        .into_iter()
        .filter_map(|name| fs::read(data_root(&temp).join(name)).ok())
        .flatten()
        .collect::<Vec<_>>();
    for forbidden in [
        marker.as_bytes(),
        prefix.as_bytes(),
        full_session_id.as_bytes(),
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
fn delivered_recognized_failures_record_once_but_protocol_control_does_not() {
    let temp = tempdir();
    let mut client = McpClient::start(&temp);
    client.request(initialize());
    let listed = client.request(json!({
        "jsonrpc": "2.0",
        "id": "tools-list",
        "method": "tools/list"
    }));
    assert!(listed.value["result"]["tools"].is_array());
    let invalid_search = client.request(tool_call("invalid-search", "search", json!({})));
    assert_eq!(invalid_search.value["result"]["isError"], true);
    client.finish();

    let connection = Connection::open(usage_db_path(&temp)).unwrap();
    assert_eq!(
        operation_calls(&connection),
        BTreeMap::from([("search".to_owned(), 1)])
    );
    let failures: (i64, i64) = connection
        .query_row(
            "SELECT SUM(calls), SUM(result_count) \
             FROM daily_usage WHERE outcome = 'failure'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(failures, (1, 0));
    let output_bytes: i64 = connection
        .query_row(
            "SELECT SUM(delivered_output_bytes) FROM daily_usage",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(output_bytes, invalid_search.wire_bytes as i64);
}

#[test]
fn protocol_control_and_invalid_input_create_no_usage_store() {
    let temp = tempdir();
    let messages = [
        "{not valid json}\n".to_owned(),
        serde_json::to_string(&tool_call("pre-init", "status", json!({}))).unwrap() + "\n",
        serde_json::to_string(&initialize()).unwrap() + "\n",
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
        serde_json::to_string(&tool_call("unknown", "unknown", json!({}))).unwrap() + "\n",
    ]
    .concat();
    ctx(&temp)
        .args(["mcp", "serve"])
        .env("CTX_DAEMON_AUTOSTART_OFF", "1")
        .env("CTX_LOCAL_USAGE_ENABLED", "true")
        .write_stdin(messages)
        .assert()
        .success();
    assert!(!usage_db_path(&temp).exists());
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

    write_message(&mut stdin, &initialize());
    assert_eq!(
        read_response(&mut stdout).value["result"]["serverInfo"]["name"],
        "ctx"
    );
    drop(stdout);
    write_message(&mut stdin, &tool_call("status", "status", json!({})));
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(!usage_db_path(&temp).exists());
}
