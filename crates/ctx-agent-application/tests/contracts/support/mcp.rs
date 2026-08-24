use std::{
    io::Read,
    process::{Child, Command as StdCommand, Stdio},
    time::{Duration, Instant},
};

use serde_json::Value;
use tempfile::TempDir;

use super::{
    assert_explicit_source_publication, bind_test_ctx_binary, ctx, data_root, json_output,
    terminate_and_reap_test_child,
};

pub(crate) fn assert_mcp_typed_error(
    result: &Value,
    error_code: &str,
    retryable: bool,
    detail_fragment: &str,
) {
    assert_eq!(result["isError"], true, "{result:#}");
    let structured = &result["structuredContent"];
    assert_eq!(structured["error_code"], error_code, "{result:#}");
    assert_eq!(structured["retryable"], retryable, "{result:#}");
    assert!(structured["detail"]
        .as_str()
        .is_some_and(|detail| detail.contains(detail_fragment)));
}

pub(crate) fn assert_status_facts_stay_machine_only(result: &Value) {
    let text = mcp_content_text(result);
    for machine_only in ["local_usage", "automatic_upgrades"] {
        assert!(
            !text.contains(machine_only),
            "MCP status machine fact {machine_only:?} leaked into text content:\n{text}"
        );
    }
}

pub(crate) struct McpSourceRefreshDaemon {
    child: Option<Child>,
}

impl McpSourceRefreshDaemon {
    pub(crate) fn kill_and_wait(&mut self) -> u32 {
        terminate_and_reap_test_child(&mut self.child, "MCP source-refresh daemon")
            .expect("terminate and reap MCP source-refresh daemon")
            .expect("MCP source-refresh daemon")
    }
}

impl Drop for McpSourceRefreshDaemon {
    fn drop(&mut self) {
        if let Err(error) =
            terminate_and_reap_test_child(&mut self.child, "MCP source-refresh daemon")
        {
            if std::thread::panicking() {
                eprintln!("MCP daemon teardown also failed: {error}");
            } else {
                panic!("MCP daemon teardown failed: {error}");
            }
        }
    }
}

pub(crate) fn start_mcp_source_refresh_daemon(temp: &TempDir) -> McpSourceRefreshDaemon {
    let data_root = data_root(temp);
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(
        data_root.join("config.toml"),
        "[daemon]\nenabled = true\nmode = \"full\"\n\n[search]\nsemantic = false\n",
    )
    .unwrap();
    bind_test_ctx_binary(temp);
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
        .current_dir(temp.path())
        .args(["daemon", "run", "--force", "--loop-interval-seconds", "600"])
        .env("CTX_DAEMON_MODE", "full")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = command
        .spawn()
        .unwrap_or_else(|error| panic!("start isolated MCP source-refresh daemon: {error}"));
    let mut daemon = McpSourceRefreshDaemon { child: Some(child) };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(exit) = daemon.child.as_mut().unwrap().try_wait().unwrap() {
            let mut stderr = String::new();
            daemon
                .child
                .as_mut()
                .unwrap()
                .stderr
                .as_mut()
                .unwrap()
                .read_to_string(&mut stderr)
                .unwrap();
            panic!("MCP source-refresh daemon exited before becoming ready ({exit}): {stderr}");
        }
        let status = ctx(temp)
            .args(["daemon", "status", "--format=json"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok());
        if status.as_ref().is_some_and(|status| {
            status["daemon"]["running"] == true
                && status["daemon"]["core_refresh_endpoint"]["available"] == true
        }) {
            return daemon;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for MCP source-refresh daemon readiness: {status:#?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub(crate) fn import_codex_fixture_through_daemon(
    temp: &TempDir,
    fixture: &str,
) -> (McpSourceRefreshDaemon, Value) {
    let daemon = start_mcp_source_refresh_daemon(temp);
    let imported = json_output(ctx(temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        fixture,
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    let source = assert_explicit_source_publication(&imported, "codex", "codex_session_jsonl_tree");
    let current_source_count = imported["totals"]["current_source_count"]
        .as_u64()
        .expect("import totals must report the current source count");
    assert!(current_source_count > 0, "{imported:#}");
    assert_eq!(
        source["current_source_count"], current_source_count,
        "{imported:#}"
    );
    assert!(
        imported["sources"][0]["published_generation"].is_string(),
        "{imported:#}"
    );
    assert_eq!(
        imported["sources"][0]["daemon_request_metadata"]["owner"], "daemon",
        "{imported:#}"
    );
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "MCP fixtures must publish through the source-backed daemon without a Store fallback"
    );
    (daemon, imported)
}

pub(crate) fn import_custom_history_fixture_source_backed(
    temp: &TempDir,
    fixture_name: &str,
) -> (McpSourceRefreshDaemon, Value) {
    let daemon = start_mcp_source_refresh_daemon(temp);
    let source = temp.path().join(fixture_name);
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/custom-history-jsonl")
        .join(fixture_name);
    std::fs::copy(fixture, &source).unwrap();
    let imported = json_output(ctx(temp).args([
        "import",
        "--input-format",
        "ctx-history-jsonl-v2",
        "--path",
        source.to_str().unwrap(),
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(imported["outcome"], "success", "{imported:#}");
    assert_eq!(imported["sources"][0]["provider"], "custom", "{imported:#}");
    assert!(
        imported["sources"][0]["published_generation"].is_string(),
        "{imported:#}"
    );
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "custom fixture import must not create previous-epoch storage"
    );
    (daemon, imported)
}

pub(crate) fn mcp_roundtrip(temp: &TempDir, messages: &[Value]) -> Vec<Value> {
    mcp_roundtrip_with_env(temp, messages, &[])
}

pub(crate) fn mcp_roundtrip_with_env(
    temp: &TempDir,
    messages: &[Value],
    envs: &[(&str, &str)],
) -> Vec<Value> {
    let mut stdin = String::new();
    for message in messages {
        stdin.push_str(&serde_json::to_string(message).unwrap());
        stdin.push('\n');
    }
    let mut command = ctx(temp);
    command.args(["mcp", "serve"]);
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command
        .write_stdin(stdin)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

pub(crate) fn assert_cli_mcp_status_parity(
    temp: &TempDir,
    envs: &[(&str, &str)],
) -> (Value, Value) {
    fn without_observation_time(mut status: Value) -> Value {
        let snapshot = status
            .pointer_mut("/daemon/supervisor/environment_snapshot")
            .and_then(Value::as_object_mut)
            .expect("status must include the daemon environment snapshot");
        let observed_at_ms = snapshot
            .remove("current_observed_at_ms")
            .expect("environment snapshot must include its observation time");
        assert!(
            observed_at_ms.as_u64().is_some(),
            "environment snapshot observation time must be an unsigned integer: {observed_at_ms:#}"
        );
        status
    }

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
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": "init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "ctx-status-parity-test", "version": "0" }
                }
            }),
            serde_json::json!({
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
    assert_eq!(
        without_observation_time(result["structuredContent"].clone()),
        without_observation_time(cli.clone()),
        "{result:#}"
    );
    (cli, result)
}

pub(crate) fn mcp_raw_roundtrip(temp: &TempDir, stdin: String) -> Vec<Value> {
    mcp_raw_roundtrip_bytes(temp, stdin.into_bytes())
}

pub(crate) fn mcp_raw_roundtrip_bytes(temp: &TempDir, stdin: Vec<u8>) -> Vec<Value> {
    let output = ctx(temp)
        .args(["mcp", "serve"])
        .write_stdin(stdin)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

pub(crate) fn mcp_content_text(result: &Value) -> &str {
    result["content"][0]["text"].as_str().unwrap()
}

pub(crate) fn assert_useful_mcp_text<'a>(result: &'a Value, expected: &[&str]) -> &'a str {
    let text = mcp_content_text(result);
    assert!(
        !text.trim_start().starts_with('{'),
        "MCP content text should not be raw JSON:\n{text}"
    );
    assert!(
        !text.contains("ctx returned structured JSON"),
        "MCP content text should not be the old stub:\n{text}"
    );
    for needle in expected {
        assert!(
            text.contains(needle),
            "MCP content text missing {needle:?}:\n{text}"
        );
    }
    text
}
