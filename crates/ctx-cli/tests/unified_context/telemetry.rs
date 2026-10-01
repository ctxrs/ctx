use super::*;
use std::time::Instant;

fn state(sandbox: &Sandbox) -> PathBuf {
    if cfg!(target_os = "macos") {
        sandbox.root.join("home/Library/Application Support/ctx")
    } else if cfg!(windows) {
        sandbox.root.join("localappdata/ctx")
    } else {
        sandbox.root.join("state/ctx")
    }
}

fn enabled(sandbox: &Sandbox, sink: &Path) -> Command {
    let mut command = sandbox.command();
    command.env("CTX_ANALYTICS_ENABLED", "true").env(
        "CTX_ANALYTICS_ENDPOINT",
        url::Url::from_file_path(sink).unwrap().as_str(),
    );
    command
}

fn sink_events(sink: &Path) -> Vec<Value> {
    fs::read_to_string(sink)
        .unwrap()
        .lines()
        .flat_map(|line| {
            let batch: Value = serde_json::from_str(line).unwrap();
            batch["events"].as_array().unwrap().clone()
        })
        .collect()
}

#[test]
fn standalone_graph_delivers_private_safe_terminal_without_history_or_daemon() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "repo/private_canary.py",
        "def private_canary_definition():\n    return 1\n",
    );
    let sink = sandbox.root.join("telemetry.jsonl");
    let result = enabled(&sandbox, &sink)
        .args(["graph", "--json", "index", "."])
        .output()
        .unwrap();
    json_output(result);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !fs::read_to_string(&sink).is_ok_and(|body| !body.is_empty() && body.ends_with('\n'))
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    let events = sink_events(&sink);
    let graph = events
        .iter()
        .find(|event| event["operation"] == "graph")
        .unwrap();
    assert_eq!(graph["outcome"], "success");
    assert_eq!(graph["properties"]["graph_operation"], "index");
    let delivered = fs::read_to_string(&sink).unwrap();
    assert!(!delivered.contains("private_canary"));
    assert!(!delivered.contains(&sandbox.root.to_string_lossy().to_string()));
    assert!(!sandbox.root.join("history/search").exists());
    assert!(!sandbox.root.join("history/daemon").exists());
    sandbox.assert_no_connection();
}

#[test]
fn standalone_sift_window_materializes_once_and_observed_opt_out_purges_it() {
    let sandbox = Sandbox::new();
    let sink = sandbox.root.join("telemetry.jsonl");
    fs::create_dir_all(state(&sandbox)).unwrap();
    // An unfamiliar launch hint suppresses only automatic delivery; exercise
    // the actual outbox-only child entry point deterministically below.
    fs::write(
        state(&sandbox).join("analytics-launch-v1.json"),
        br#"{"schema_version":2,"next_allowed_at":0}"#,
    )
    .unwrap();
    let request = json!({"version":1,"text":"private_sift_canary\n".repeat(40)});
    for _ in 0..3 {
        json_output(
            enabled(&sandbox, &sink)
                .args(["sift", "compact", "--protocol=json-v1"])
                .write_stdin(format!("{request}\n"))
                .output()
                .unwrap(),
        );
    }
    let identity: Value =
        serde_json::from_slice(&fs::read(sandbox.root.join("history/install.json")).unwrap())
            .unwrap();
    let owner = identity["install_id"].as_str().unwrap();
    let drain = || {
        enabled(&sandbox, &sink)
            .arg("--ctx-analytics-drain-v1")
            .arg(sandbox.root.join("history"))
            .arg(owner)
            .output()
            .unwrap()
    };
    assert_success(&drain());
    let first = fs::read(&sink).unwrap();
    assert_success(&drain());
    assert_eq!(fs::read(&sink).unwrap(), first);
    let events = sink_events(&sink);
    assert!(events
        .iter()
        .any(|event| event["operation"] == "sift_summary"));
    assert!(!String::from_utf8(first)
        .unwrap()
        .contains("private_sift_canary"));
    json_output(
        enabled(&sandbox, &sink)
            .args(["sift", "compact", "--protocol=json-v1"])
            .write_stdin(format!("{request}\n"))
            .output()
            .unwrap(),
    );
    let stopped = sandbox
        .command()
        .env("CTX_ANALYTICS_ENABLED", "false")
        .arg("--ctx-analytics-drain-v1")
        .arg(sandbox.root.join("history"))
        .arg(owner)
        .output()
        .unwrap();
    assert_success(&stopped);
    let before = fs::read(&sink).unwrap();
    assert_success(&drain());
    assert_eq!(fs::read(&sink).unwrap(), before);
    assert!(!sandbox.root.join("history/search").exists());
    assert!(!sandbox.root.join("history/daemon").exists());
}

pub(super) fn assert_mcp_output_lifecycle(malformed: bool, default_observability: bool) {
    let sandbox = Sandbox::new();
    if malformed {
        sandbox.write("history/config.toml", b"[broken history configuration\n");
    }
    let original = "mcp synthetic complete diagnostic\n".repeat(120);
    let frame = format!("{RUNS_HEADER}[[3,\"preserved\\r\\n\"],[1,\"tail\"]]");
    let messages = [
        json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
                "protocolVersion":"2025-11-25", "capabilities":{},
                "clientInfo":{"name":"synthetic-acceptance", "version":"0"}}}),
        json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"}),
        json!({"jsonrpc":"2.0", "id":3, "method":"tools/call", "params":{
                "name":"output_compact", "arguments":{"text":original}}}),
        json!({"jsonrpc":"2.0", "id":4, "method":"tools/call", "params":{
                "name":"output_restore", "arguments":{"encoding":"text-runs-v1", "text":frame}}}),
    ];
    let input = messages
        .iter()
        .map(|message| format!("{message}\n"))
        .collect::<String>();
    let sink = sandbox.root.join("telemetry.jsonl");
    if default_observability && !malformed {
        fs::create_dir_all(state(&sandbox)).unwrap();
        // Defer automatic delivery only; inspect the real outbox-only child after EOF.
        fs::write(
            state(&sandbox).join("analytics-launch-v1.json"),
            br#"{"schema_version":2,"next_allowed_at":0}"#,
        )
        .unwrap();
    }
    let before = sandbox.protected_state();
    // Keep default consent and the real MCP lifecycle. The child uses an isolated sink.
    let mut command = sandbox.command();
    if default_observability {
        command
            .env_remove("CTX_ANALYTICS_ENABLED")
            .env_remove("CTX_LOCAL_USAGE_ENABLED")
            .env(
                "CTX_ANALYTICS_ENDPOINT",
                url::Url::from_file_path(&sink).unwrap().as_str(),
            );
    }
    let output = command
        .args(["mcp", "serve", "--graph-db", "absent.db"])
        .write_stdin(input)
        .output()
        .unwrap(); // Closing stdin exercises EOF shutdown.
    assert_success(&output);
    let text = std::str::from_utf8(&output.stdout).unwrap();
    let responses = text
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).expect("MCP stdout must contain only JSON-RPC")
        })
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 4, "{text}");
    for (index, response) in responses.iter().enumerate() {
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], index + 1);
        assert!(response.get("error").is_none(), "{response}");
        assert_ne!(response["result"]["isError"], true, "{response}");
    }
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-11-25");
    let tools = responses[1]["result"]["tools"].as_array().unwrap();
    for name in ["output_compact", "output_restore"] {
        assert!(tools.iter().any(|tool| tool["name"] == name));
    }
    let compact = &responses[2]["result"]["structuredContent"];
    assert_eq!(compact["encoding"], "text-runs-v1");
    assert!(compact["output_tokens"].as_u64().unwrap() < compact["input_tokens"].as_u64().unwrap());
    let runs: Vec<(usize, String)> = serde_json::from_str(
        compact["text"]
            .as_str()
            .unwrap()
            .strip_prefix(RUNS_HEADER)
            .expect("documented run framing"),
    )
    .unwrap();
    let expanded = runs
        .iter()
        .map(|(count, text)| text.repeat(*count))
        .collect::<String>();
    assert_eq!(expanded, original);
    let restored = &responses[3]["result"]["structuredContent"];
    assert_eq!(restored["encoding"], "raw");
    assert_eq!(
        restored["text"],
        "preserved\r\npreserved\r\npreserved\r\ntail"
    );
    if default_observability && !malformed {
        let identity: Value =
            serde_json::from_slice(&fs::read(sandbox.root.join("history/install.json")).unwrap())
                .unwrap();
        let owner = identity["install_id"].as_str().unwrap();
        assert_success(
            &enabled(&sandbox, &sink)
                .arg("--ctx-analytics-drain-v1")
                .arg(sandbox.root.join("history"))
                .arg(owner)
                .output()
                .unwrap(),
        );
    }
    sandbox.assert_no_connection();
    let after = sandbox.protected_state();
    if default_observability && !malformed {
        let device = Path::new(if cfg!(windows) {
            "localappdata/ctx"
        } else if cfg!(target_os = "macos") {
            "home/Library/Application Support/ctx"
        } else {
            "state/ctx"
        });
        let permitted = [
            PathBuf::from("history/install.json"),
            device.join("device.json"),
            device.join("device.lock"),
            device.join("analytics-outbox-v1.uploader.lock"),
            device.join(format!(
                "analytics-summary-{}.json",
                serde_json::from_slice::<Value>(
                    &fs::read(sandbox.root.join("history/install.json")).unwrap()
                )
                .unwrap()["install_id"]
                    .as_str()
                    .unwrap()
            )),
            PathBuf::from("telemetry.jsonl"),
            device.join("analytics-outbox-v1.json"),
            device.join("analytics-outbox-v1.lock"),
            device.join("execution-capabilities-v1.claim"),
            device.join("execution-capabilities-v1.reported"),
        ];
        for (path, bytes) in &before {
            assert_eq!(
                after.get(path),
                Some(bytes),
                "existing MCP state changed: {path:?}"
            );
        }
        for (path, bytes) in &after {
            assert!(
                before.contains_key(path)
                    || permitted
                        .iter()
                        .any(|allowed| path == allowed
                            || (bytes.is_none() && allowed.starts_with(path))),
                "MCP created non-lifecycle state: {path:?}"
            );
        }
        let mut phases = Vec::new();
        let mut sift_operations = Vec::new();
        for event in sink_events(&sink) {
            assert_eq!(
                event["event_name"], "runtime_observation",
                "Unified tool recorded as history: {event}"
            );
            assert_eq!(event["surface"], "mcp");
            match event["operation"].as_str().unwrap() {
                "sift_summary" => sift_operations.push(
                    event["properties"]["sift_operation"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                ),
                operation => {
                    let requests = match operation {
                        "initialized" => "0",
                        "stopped" => "2-5",
                        other => panic!("unexpected MCP lifecycle operation: {other}"),
                    };
                    assert_eq!(event["properties"]["tool_request_count_bucket"], requests);
                    phases.push(operation.to_owned());
                }
            }
        }
        sift_operations.sort();
        assert_eq!(sift_operations, ["compact", "restore"]);
        assert!(!fs::read_to_string(&sink)
            .unwrap()
            .contains("mcp synthetic complete diagnostic"));
        phases.sort();
        assert_eq!(phases, ["initialized", "stopped"]);
    } else {
        assert_eq!(
            after, before,
            "opt-out/malformed MCP lifecycle mutated isolated state"
        );
    }
    assert!(
        !sandbox.root.join("output").exists(),
        "pure MCP output created output state"
    );
    assert!(
        !sandbox.repo().join(".graf").exists(),
        "MCP created a graph"
    );
    if !malformed && !default_observability {
        assert!(
            !sandbox.root.join("history").exists(),
            "MCP created the history root"
        );
    }
}
