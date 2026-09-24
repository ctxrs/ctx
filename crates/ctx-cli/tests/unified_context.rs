//! Process acceptance for one ctx executable. Fixtures and expected facts are
//! authored here; no Graf/Sift binaries or product-library calls are used.

use assert_cmd::Command;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::ErrorKind,
    net::TcpListener,
    path::{Path, PathBuf},
    process::Output,
    time::Duration,
};
use tempfile::TempDir;

#[path = "unified_context/history_fixture.rs"]
mod history_fixture;
use history_fixture::import_synthetic_history;

#[path = "unified_context/output_hooks.rs"]
mod output_hooks;

const PYTHON_SOURCE: &str = "def kernel():\n    return 41\n\ndef launch():\n    return kernel()\n\ndef idle():\n    return 0\n";
const RUNS_HEADER: &str = "sift:text-runs-v1 counts repeat exact JSON strings; concatenate\n";

struct Sandbox {
    _temp: TempDir,
    root: PathBuf,
    binary: PathBuf,
    network: TcpListener,
}

impl Sandbox {
    fn new() -> Self {
        // cargo_bin honors Bazel's runtime CARGO_BIN_EXE_ctx as well as Cargo's
        // built binary. Resolve before changing cwd; never search the host PATH.
        let command = Command::cargo_bin("ctx").expect("declare the compiled ctx executable");
        let binary = fs::canonicalize(command.get_program()).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        for name in [
            "repo",
            "home",
            "config",
            "data",
            "state",
            "cache",
            "runtime",
            "tmp",
            "empty-path",
            "appdata",
            "localappdata",
            "providers",
        ] {
            fs::create_dir(root.join(name)).unwrap();
        }
        let network = TcpListener::bind("127.0.0.1:0").unwrap();
        network.set_nonblocking(true).unwrap();
        Self {
            _temp: temp,
            root,
            binary,
            network,
        }
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    fn write(&self, relative: &str, bytes: impl AsRef<[u8]>) {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .env_clear()
            .current_dir(self.repo())
            .timeout(Duration::from_secs(30));
        // Windows requires its native system directory even with an empty PATH.
        #[cfg(windows)]
        if let Some(value) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", value);
        }
        for (key, relative) in [
            ("HOME", "home"),
            ("USERPROFILE", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_RUNTIME_DIR", "runtime"),
            ("APPDATA", "appdata"),
            ("LOCALAPPDATA", "localappdata"),
            ("TMPDIR", "tmp"),
            ("TEMP", "tmp"),
            ("TMP", "tmp"),
            ("PATH", "empty-path"),
            ("CTX_DATA_ROOT", "history"),
            ("CTX_RUNTIME_DIR", "runtime"),
            ("CODEX_HOME", "providers/codex"),
            ("CLAUDE_CONFIG_DIR", "providers/claude"),
            ("SIFT_CONFIG_DIR", "output/config"),
            ("SIFT_STATE_DIR", "output/state"),
        ] {
            command.env(key, self.root.join(relative));
        }
        command
            .env("CTX_DAEMON_ENABLED", "false")
            .env("CTX_DAEMON_AUTOSTART_OFF", "1")
            .env("CTX_UPGRADE_AUTO", "off")
            .env("CTX_ANALYTICS_ENABLED", "false")
            .env("CTX_LOCAL_USAGE_ENABLED", "false")
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .env("TZ", "UTC");
        let endpoint = format!("http://{}", self.network.local_addr().unwrap());
        for key in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "CTX_ANALYTICS_ENDPOINT",
        ] {
            command.env(key, &endpoint);
        }
        command
    }

    fn output(&self, args: &[&str], input: &[u8]) -> Output {
        self.command()
            .args(args)
            .write_stdin(input)
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        json_output(self.output(args, b""))
    }

    fn graph(&self, args: &[&str]) -> Value {
        json_output(
            self.command()
                .args(["graph", "--json"])
                .args(args)
                .output()
                .unwrap(),
        )
    }

    fn compact(&self, text: &str) -> Value {
        let request = json!({"version": 1, "text": text});
        json_output(self.output(
            &["sift", "compact", "--protocol=json-v1"],
            format!("{request}\n").as_bytes(),
        ))
    }

    fn assert_restored(&self, representation: &Value, expected: &[u8]) {
        assert_eq!(representation["version"], 1);
        let encoding = representation["encoding"].as_str().unwrap();
        let text = representation["text"].as_str().unwrap();
        let restored = self.output(&["sift", "restore", "--encoding", encoding], text.as_bytes());
        assert_success(&restored);
        assert_eq!(restored.stdout, expected);
    }

    fn protected_state(&self) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        fn visit(base: &Path, path: &Path, result: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
            for entry in fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let relative = path.strip_prefix(base).unwrap();
                // Graph writes and documented output usage/retention are allowed.
                if relative == Path::new("repo/.graf") || relative == Path::new("output/state") {
                    continue;
                }
                let kind = entry.file_type().unwrap();
                assert!(!kind.is_symlink(), "unexpected symlink: {relative:?}");
                if kind.is_dir() {
                    // The output parent can be created for its allowed state.
                    if relative != Path::new("output") {
                        result.insert(relative.to_owned(), None);
                    }
                    visit(base, &path, result);
                } else {
                    assert!(kind.is_file(), "unexpected special file: {relative:?}");
                    result.insert(relative.to_owned(), Some(fs::read(path).unwrap()));
                }
            }
        }
        let mut result = BTreeMap::new();
        visit(&self.root, &self.root, &mut result);
        result
    }

    fn assert_no_connection(&self) {
        match self.network.accept() {
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Ok((_, peer)) => panic!("unexpected network connection from {peer}"),
            Err(error) => panic!("inspect network tripwire: {error}"),
        }
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
}

fn json_output(output: Output) -> Value {
    assert_success(&output);
    serde_json::from_slice(&output.stdout).expect("exactly one JSON value on stdout")
}

fn assert_failure(output: &Output) {
    assert!(!output.status.success(), "unexpected success: {output:?}");
    assert!(output.stdout.is_empty(), "failure contaminated stdout");
    assert!(!output.stderr.is_empty(), "failure omitted its diagnostic");
}

fn labels(graph: &Value) -> Vec<&str> {
    let mut labels = graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["label"].as_str().unwrap())
        .collect::<Vec<_>>();
    labels.sort_unstable();
    labels
}

fn assert_call(graph: &Value, caller: &str, callee: &str) {
    let id = |label| {
        graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["label"] == label)
            .unwrap_or_else(|| panic!("missing {label} in {graph}"))["id"]
            .as_str()
            .unwrap()
    };
    assert!(graph["edges"].as_array().unwrap().iter().any(|edge| {
        edge["source"] == id(caller)
            && edge["target"] == id(callee)
            && edge["relation"] == "calls"
            && edge["directed"] == true
    }));
}

#[test]
fn graph_index_callers_explicit_update_and_delete_use_the_saved_snapshot() {
    let sandbox = Sandbox::new();
    sandbox.write("repo/workflow.py", PYTHON_SOURCE);
    let indexed = sandbox.graph(&["index", "."]);
    assert_eq!(indexed["parsed_files"], 1);
    assert!(sandbox.repo().join(".graf/index.db").is_file());
    let callers = sandbox.graph(&["callers", "kernel"]);
    assert_eq!(labels(&callers), ["kernel", "launch"]);
    assert_call(&callers, "launch", "kernel");
    let impact = sandbox.graph(&["impact", "kernel", "--depth", "2"]);
    assert!(labels(&impact["graph"]).contains(&"launch"));
    assert!(!labels(&impact["graph"]).contains(&"idle"));

    // A source edit alone cannot change reads of the retained graph.
    sandbox.write(
        "repo/workflow.py",
        "def kernel():\n    return 41\n\ndef replacement():\n    return kernel()\n",
    );
    assert_eq!(sandbox.graph(&["callers", "kernel"]), callers);
    let updated = sandbox.graph(&["update"]);
    assert_eq!(updated["parsed_files"], 1);
    assert!(updated["generation"].as_u64().unwrap() > indexed["generation"].as_u64().unwrap());
    let callers = sandbox.graph(&["callers", "kernel"]);
    assert_eq!(labels(&callers), ["kernel", "replacement"]);
    assert_call(&callers, "replacement", "kernel");
    assert!(labels(&sandbox.graph(&["query", "launch", "--depth", "0"])).is_empty());

    // Discovery from a child directory still finds the ancestor .graf/index.db.
    fs::create_dir(sandbox.repo().join("nested")).unwrap();
    let nested = sandbox
        .command()
        .current_dir(sandbox.repo().join("nested"))
        .args(["graph", "callers", "kernel", "--json"])
        .output()
        .unwrap();
    assert_eq!(json_output(nested), callers);
    fs::remove_file(sandbox.repo().join("workflow.py")).unwrap();
    assert_eq!(sandbox.graph(&["callers", "kernel"]), callers);
    let deleted = sandbox.graph(&["update"]);
    assert_eq!(deleted["deleted_files"], 1);
    assert!(labels(&sandbox.graph(&["query", "kernel", "--depth", "0"])).is_empty());
    assert_failure(&sandbox.output(&["graph", "callers", "kernel", "--json"], b""));
    sandbox.assert_no_connection();
    assert!(!sandbox.root.join("history").exists());
}

// Authored schema-v1 interchange, not an export generated by the implementation
// under test. Stable IDs, edge direction and metadata are independent oracles.
fn graf_snapshot() -> Value {
    json!({
        "schema_version": 1, "generation": 12, "kind": "native", "root": "synthetic-source",
        "nodes": [
            {"id":"entry-id", "label":"dispatch", "kind":"function", "file":"app.py",
             "line":1, "end_line":2, "qualified_name":"app.dispatch", "binding_key":"app:dispatch",
             "metadata":{"fixture":"authored", "ordinal":1}},
            {"id":"target-id", "label":"compute", "kind":"function", "file":"app.py",
             "line":4, "end_line":5, "qualified_name":"app.compute", "binding_key":"app:compute",
             "metadata":{"fixture":"authored", "ordinal":2}}
        ],
        "edges": [
            {"id":"call-id", "source":"entry-id", "target":"target-id", "relation":"calls",
             "directed":true, "file":"app.py", "line":2, "confidence":"EXTRACTED",
             "metadata":{"call_site":"direct"}}
        ],
        "metadata":{"fixture":"independent interchange", "revision":7}
    })
}

fn export_snapshot(sandbox: &Sandbox) -> Value {
    // Without --json Graf writes the snapshot itself. With --json it writes an
    // export report containing a content string, a different public contract.
    sandbox.json(&["graph", "export", "snapshot-json"])
}

#[test]
fn existing_graf_snapshot_import_preserves_facts_and_rejected_refresh_is_atomic() {
    let sandbox = Sandbox::new();
    let original = graf_snapshot();
    sandbox.write("repo/saved graph.json", original.to_string());
    let imported = sandbox.graph(&["import", "graf", "saved graph.json"]);
    assert_eq!(imported["kind"], "imported");
    assert_eq!(imported["nodes"], 2);
    assert_eq!(imported["edges"], 1);
    let callers = sandbox.graph(&["callers", "target-id"]);
    assert_eq!(labels(&callers), ["compute", "dispatch"]);
    assert_call(&callers, "dispatch", "compute");
    let before = export_snapshot(&sandbox);
    assert_eq!(before["nodes"], original["nodes"]);
    assert_eq!(before["edges"], original["edges"]);
    assert_eq!(before["kind"], "imported");
    assert_eq!(before["metadata"]["graf_snapshot"]["generation"], 12);
    assert_eq!(
        before["metadata"]["graf_snapshot"]["metadata"],
        original["metadata"]
    );

    // Imported snapshots do not become native indexing roots.
    assert_failure(&sandbox.output(&["graph", "update", "--json"], b""));
    assert_failure(&sandbox.output(&["graph", "import", "graf", "saved graph.json"], b""));
    let mut invalid = original.clone();
    invalid["edges"][0]["target"] = json!("missing-node");
    sandbox.write("repo/broken.json", invalid.to_string());
    assert_failure(&sandbox.output(
        &["graph", "import", "graf", "broken.json", "--refresh"],
        b"",
    ));
    assert_eq!(export_snapshot(&sandbox), before);
    assert_eq!(sandbox.graph(&["callers", "target-id"]), callers);

    let mut replacement = original.clone();
    replacement["nodes"][0]["label"] = json!("dispatch_new");
    sandbox.write("repo/replacement.json", replacement.to_string());
    sandbox.graph(&["import", "graf", "replacement.json", "--refresh"]);
    let after = export_snapshot(&sandbox);
    assert_eq!(after["nodes"], replacement["nodes"]);
    assert_eq!(after["edges"], replacement["edges"]);
    assert!(after["generation"].as_u64().unwrap() > before["generation"].as_u64().unwrap());
    assert_eq!(
        fs::read(sandbox.repo().join("saved graph.json")).unwrap(),
        original.to_string().as_bytes()
    );
    sandbox.assert_no_connection();
}

#[test]
fn graph_missing_database_fails_without_creating_one() {
    let sandbox = Sandbox::new();
    let before = sandbox.protected_state();
    assert_failure(&sandbox.output(
        &["graph", "--db", "absent.db", "callers", "kernel", "--json"],
        b"",
    ));
    assert!(!sandbox.repo().join("absent.db").exists());
    assert!(!sandbox.repo().join(".graf").exists());
    assert_eq!(sandbox.protected_state(), before);
    sandbox.assert_no_connection();
}

fn assert_search_envelope(result: &Value, scope: &str, limit: u64, partial: bool) {
    assert_eq!(result["schema_version"], 1);
    assert_eq!(result["scope"], scope);
    assert_eq!(result["limit_per_scope"], limit);
    assert_eq!(result["partial"], partial);
}

fn assert_unavailable(result: &Value) {
    assert_eq!(result["status"], "unavailable");
    assert!(!result["error"].as_str().unwrap().is_empty());
    assert!(!result["next_action"].as_str().unwrap().is_empty());
    assert!(result.get("result").is_none());
}

fn stable_history_json(mut result: Value) -> Value {
    // Only clock-dependent observations differ between identical snapshot reads.
    result.as_object_mut().unwrap().remove("generated_at");
    result.as_object_mut().unwrap().remove("phase_attribution");
    result["retrieval"]
        .as_object_mut()
        .unwrap()
        .remove("phase_attribution");
    result
}

#[test]
fn imported_history_and_graph_share_all_scope_without_changing_native_citations() {
    let sandbox = Sandbox::new();
    import_synthetic_history(&sandbox);
    sandbox.write("repo/workflow.py", PYTHON_SOURCE);
    sandbox.graph(&["index", "."]);
    let history_args = [
        "search",
        "kernel",
        "--refresh=off",
        "--backend=lexical",
        "--limit=5",
        "--format=json",
    ];
    let native = sandbox.json(&history_args);
    let explicit = json_output(
        sandbox
            .command()
            .args(history_args)
            .arg("--scope=history")
            .output()
            .unwrap(),
    );
    assert_eq!(
        stable_history_json(native.clone()),
        stable_history_json(explicit)
    );
    assert_eq!(native["schema_version"], 2);
    assert_eq!(native["payload_type"], "search_results");
    assert!(native.get("scope").is_none() && native.get("history").is_none());
    assert_eq!(native["results"].as_array().unwrap().len(), 1);
    let hit = &native["results"][0];
    assert_eq!(
        hit["provider_session_id"],
        "019faaaa-0000-7000-8000-000000000123"
    );
    assert!(!hit["citations"].as_array().unwrap().is_empty());
    assert!(hit["ctx_event_id"].is_string() && hit["ctx_session_id"].is_string());
    assert_eq!(hit["citations"][0]["ctx_event_id"], hit["ctx_event_id"]);
    assert_eq!(hit["citations"][0]["ctx_session_id"], hit["ctx_session_id"]);
    let terms = sandbox.json(&[
        "search",
        "synthetic_absent_keyword",
        "--term",
        "kernel",
        "--scope=history",
        "--refresh=off",
        "--backend=lexical",
        "--format=json",
    ]);
    assert_eq!(terms["results"].as_array().unwrap().len(), 1);
    assert_eq!(terms["results"][0]["ctx_session_id"], hit["ctx_session_id"]);
    let before = sandbox.protected_state();
    let graph_bytes = fs::read(sandbox.repo().join(".graf/index.db")).unwrap();

    for graph_missing in [false, true] {
        let mut command = sandbox.command();
        command.args([
            "search",
            "kernel",
            "--scope=all",
            "--limit=5",
            "--format=json",
        ]);
        if graph_missing {
            command.args(["--graph-db", "absent.db"]);
        }
        let combined = json_output(command.output().unwrap());
        assert_search_envelope(&combined, "all", 5, graph_missing);
        assert_eq!(combined["history"]["status"], "ok");
        let history = &combined["history"]["result"];
        for field in [
            "schema_version",
            "payload_type",
            "query",
            "results",
            "result_window",
        ] {
            assert_eq!(
                history[field], native[field],
                "native history field {field} changed"
            );
        }
        assert_eq!(
            history["retrieval"]["generation_id"],
            native["retrieval"]["generation_id"]
        );
        if graph_missing {
            assert_unavailable(&combined["graph"]);
            assert!(!sandbox.repo().join("absent.db").exists());
        } else {
            assert_eq!(combined["graph"]["status"], "ok");
            assert_call(&combined["graph"]["result"]["graph"], "launch", "kernel");
        }
        assert_eq!(sandbox.protected_state(), before);
        assert_eq!(
            fs::read(sandbox.repo().join(".graf/index.db")).unwrap(),
            graph_bytes
        );
    }
    sandbox.assert_no_connection();
}

#[test]
fn docs_help_and_explicit_skill_status_survive_malformed_history() {
    for malformed in [false, true] {
        let sandbox = Sandbox::new();
        if malformed {
            sandbox.write("history/config.toml", b"[broken history configuration\n");
        }
        let before = sandbox.protected_state();
        for args in [
            vec!["docs", "--help"],
            vec!["integrations", "--help"],
            vec!["integrations", "status", "skill", "--help"],
            vec!["sift", "run", "--help"],
            vec!["sift", "compact", "--help"],
            vec!["sift", "restore", "--help"],
            vec!["sift", "recall", "--help"],
            vec!["sift", "--help"],
        ] {
            let output = sandbox.output(&args, b"");
            assert_success(&output);
            assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
        }
        let docs = sandbox.json(&["docs", "show", "cli-reference", "--format=json"]);
        assert_eq!(docs["id"], "cli-reference");
        assert!(docs["body"].as_str().unwrap().contains("ctx search"));
        let status = sandbox.json(&[
            "integrations",
            "status",
            "skill",
            "--agent",
            "codex",
            "--project",
            "--format=json",
        ]);
        assert_eq!(status["scope"], "project");
        assert_eq!(status["results"].as_array().unwrap().len(), 1);
        assert_eq!(status["results"][0]["agent"], "codex");
        assert_eq!(status["results"][0]["status"], "missing");
        assert_eq!(
            sandbox.protected_state(),
            before,
            "inspection installed or repaired state"
        );
        sandbox.assert_no_connection();
    }
}

#[test]
fn output_commands_have_one_public_entry_point() {
    let sandbox = Sandbox::new();
    for command in ["run", "compact", "restore", "recall", "output"] {
        assert_failure(&sandbox.output(&[command, "--help"], b""));
    }
    assert_success(&sandbox.output(&["sift", "--help"], b""));
    sandbox.assert_no_connection();
}

#[test]
fn health_reports_show_independent_components_even_when_history_is_malformed() {
    let sandbox = Sandbox::new();
    for (indexed, malformed) in [(false, false), (true, false), (true, true)] {
        if indexed && !sandbox.repo().join(".graf/index.db").exists() {
            sandbox.write("repo/workflow.py", PYTHON_SOURCE);
            sandbox.graph(&["index", "."]);
        }
        if malformed {
            sandbox.write("history/config.toml", b"[broken history configuration\n");
        }
        let before = sandbox.protected_state();
        for command in ["status", "doctor"] {
            let output = sandbox.output(&[command, "--format=json"], b"");
            let report: Value = if malformed {
                assert_failure(&output);
                serde_json::from_slice(&output.stderr).expect("one JSON history failure on stderr")
            } else {
                json_output(output)
            };
            let schema = match (command, malformed) {
                ("status", true) => 2,
                ("status", false) => 3,
                _ => 1,
            };
            assert_eq!(report["schema_version"], schema);
            assert_eq!(
                report["graph"]["status"],
                if indexed { "readable" } else { "not_indexed" }
            );
            assert_eq!(report["graph"]["read_only"], true);
            assert_eq!(report["graph"]["requires_history"], false);
            assert_eq!(report["graph"]["freshness"], "not_checked");
            assert_eq!(report["output"]["status"], "available");
            assert_eq!(report["output"]["built_in"], true);
            assert_eq!(report["output"]["requires_history"], false);
            if malformed {
                assert_eq!(report["history"]["status"], "unavailable");
                let human = sandbox.output(&[command], b"");
                assert_failure(&human);
                let text = String::from_utf8_lossy(&human.stderr);
                assert!(
                    text.contains("Graph") && text.contains("Command output"),
                    "{text}"
                );
            }
        }
        assert_eq!(sandbox.protected_state(), before);
        sandbox.assert_no_connection();
    }
}

#[test]
fn mcp_output_lifecycle_preserves_authorized_default_telemetry_without_history_setup() {
    assert_mcp_output_lifecycle(false, true);
}

#[test]
fn mcp_output_lifecycle_with_default_observability_preserves_malformed_history() {
    assert_mcp_output_lifecycle(true, true);
}

#[test]
fn mcp_output_lifecycle_with_opt_out_preserves_empty_and_malformed_trees() {
    for malformed in [false, true] {
        assert_mcp_output_lifecycle(malformed, false);
    }
}

fn assert_mcp_output_lifecycle(malformed: bool, default_observability: bool) {
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
    let before = sandbox.protected_state();
    // Keep default observability policy: redirect delivery to the tripwire,
    // but do not opt out, dry-run, or skip real product MCP lifecycle setup.
    let mut command = sandbox.command();
    if default_observability {
        command
            .env_remove("CTX_ANALYTICS_ENABLED")
            .env_remove("CTX_LOCAL_USAGE_ENABLED");
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
        let outbox: Value = serde_json::from_slice(
            &fs::read(sandbox.root.join(device).join("analytics-outbox-v1.json"))
                .expect("authorized lifecycle outbox"),
        )
        .unwrap();
        let mut phases = Vec::new();
        for entry in outbox["entries"].as_array().unwrap() {
            let payload: Value = serde_json::from_str(entry["payload"].as_str().unwrap()).unwrap();
            for event in payload["events"].as_array().unwrap() {
                assert_eq!(
                    event["event_name"], "runtime_observation",
                    "Unified tool recorded as history: {event}"
                );
                assert_eq!(event["surface"], "mcp");
                assert_eq!(event["properties"]["tool_request_count_bucket"], "0");
                phases.push(event["operation"].as_str().unwrap().to_owned());
            }
        }
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

#[test]
fn graph_search_scope_selects_an_explicit_database_and_preserves_native_results() {
    let sandbox = Sandbox::new();
    sandbox.write("repo/saved.json", graf_snapshot().to_string());
    sandbox.graph(&["--db", "selected.db", "import", "graf", "saved.json"]);
    sandbox.write("history/config.toml", b"[broken history configuration\n");
    let native = sandbox.graph(&["--db", "selected.db", "query", "compute", "--limit", "7"]);
    // Native SQLite reads establish WAL locking files before the scoped comparison.
    let before = sandbox.protected_state();
    let result = sandbox.json(&[
        "search",
        "compute",
        "--scope",
        "graph",
        "--graph-db",
        "selected.db",
        "--limit",
        "7",
        "--format=json",
    ]);
    assert_search_envelope(&result, "graph", 7, false);
    assert_eq!(result["graph"]["status"], "ok");
    assert!(result.get("history").is_none());
    // Graf's extended SearchResult carries its GraphResult under `graph`, plus
    // seed and truncation evidence. Do not flatten away the native result.
    assert_eq!(result["graph"]["result"]["graph"], native);
    assert_eq!(result["graph"]["result"]["seeds"], json!(["target-id"]));
    assert_eq!(result["graph"]["result"]["truncation_reasons"], json!([]));
    assert_call(&result["graph"]["result"]["graph"], "dispatch", "compute");
    let term_only = sandbox.output(
        &[
            "search",
            "--term",
            "compute",
            "--scope=graph",
            "--graph-db=selected.db",
            "--limit=7",
            "--format=json",
        ],
        b"",
    );
    assert_failure(&term_only);
    let diagnostic = String::from_utf8_lossy(&term_only.stderr);
    assert!(
        diagnostic.contains("--term") && diagnostic.contains("history"),
        "{diagnostic}"
    );
    assert!(sandbox.protected_state() == before);
    sandbox.assert_no_connection();
}

#[test]
fn all_search_keeps_graph_results_when_history_is_missing_or_malformed() {
    for malformed in [false, true] {
        let sandbox = Sandbox::new();
        sandbox.write("repo/workflow.py", PYTHON_SOURCE);
        sandbox.graph(&["index", "."]);
        if malformed {
            sandbox.write("history/config.toml", b"[broken history configuration\n");
        }
        let before = sandbox.protected_state();
        let result = sandbox.json(&[
            "search",
            "kernel",
            "--scope=all",
            "--limit=5",
            "--format=json",
        ]);
        assert_search_envelope(&result, "all", 5, true);
        assert_unavailable(&result["history"]);
        assert_eq!(result["graph"]["status"], "ok");
        assert_call(&result["graph"]["result"]["graph"], "launch", "kernel");
        assert_eq!(
            sandbox.protected_state(),
            before,
            "combined search refreshed or initialized history"
        );

        // A successful empty graph result is still available, not a missing scope.
        let empty = sandbox.json(&[
            "search",
            "synthetic_absent_symbol",
            "--scope=all",
            "--limit=5",
            "--format=json",
        ]);
        assert_search_envelope(&empty, "all", 5, true);
        assert_eq!(empty["graph"]["status"], "ok");
        assert!(labels(&empty["graph"]["result"]["graph"]).is_empty());
        assert_unavailable(&empty["history"]);
        assert_eq!(sandbox.protected_state(), before);
        sandbox.assert_no_connection();
    }
}

#[test]
fn unavailable_scopes_fail_with_a_machine_readable_envelope_and_no_initialization() {
    let sandbox = Sandbox::new();
    let before = sandbox.protected_state();
    for scope in ["graph", "all"] {
        let output = sandbox.output(
            &[
                "search",
                "kernel",
                "--scope",
                scope,
                "--graph-db",
                "absent.db",
                "--limit=4",
                "--format=json",
            ],
            b"",
        );
        assert!(!output.status.success());
        let result: Value = serde_json::from_slice(&output.stdout)
            .expect("unavailable scopes must retain their JSON envelope on failure");
        assert_search_envelope(&result, scope, 4, false);
        assert_unavailable(&result["graph"]);
        if scope == "all" {
            assert_unavailable(&result["history"]);
        } else {
            assert!(result.get("history").is_none());
        }
        assert_eq!(sandbox.protected_state(), before);
    }
    sandbox.assert_no_connection();
}

#[test]
fn graph_and_all_search_reject_explicit_history_controls_but_accept_literal_queries() {
    let sandbox = Sandbox::new();
    let mut snapshot = graf_snapshot();
    snapshot["nodes"][0]["label"] = json!("--provider");
    sandbox.write("repo/literal.json", snapshot.to_string());
    sandbox.graph(&["import", "graf", "literal.json"]);
    let before = sandbox.protected_state();
    for scope in ["graph", "all"] {
        for rejected in [
            ["--term", "compute"],
            ["--provider", "codex"],
            ["--workspace", "synthetic"],
            ["--file", "app.py"],
            ["--content-scope", "all"],
            ["--refresh", "off"],
            ["--refresh", "background"],
            ["--backend", "lexical"],
            ["--semantic-weight", "0.35"],
        ] {
            let output = sandbox
                .command()
                .args(["search", "compute", "--scope", scope, "--format=json"])
                .args(rejected)
                .output()
                .unwrap();
            assert_failure(&output);
            let diagnostic = String::from_utf8_lossy(&output.stderr);
            assert!(diagnostic.contains("history"), "{diagnostic}");
            assert!(diagnostic.contains(rejected[0]), "{diagnostic}");
        }
        // An escaped positional token is query data; --term itself is now an
        // explicit history-only option, regardless of its flag-looking value.
        let result = sandbox.json(&[
            "search",
            "--scope",
            scope,
            "--format=json",
            "--",
            "--provider",
        ]);
        assert_search_envelope(&result, scope, 20, scope == "all");
        assert_eq!(result["graph"]["status"], "ok");
        assert!(labels(&result["graph"]["result"]["graph"]).contains(&"--provider"));
        let rejected = sandbox.output(
            &[
                "search",
                "--scope",
                scope,
                "--term=--provider",
                "--format=json",
            ],
            b"",
        );
        assert_failure(&rejected);
        let diagnostic = String::from_utf8_lossy(&rejected.stderr);
        assert!(
            diagnostic.contains("--term") && diagnostic.contains("history"),
            "{diagnostic}"
        );
    }
    assert_eq!(sandbox.protected_state(), before);
    sandbox.assert_no_connection();
}

#[test]
fn root_options_preserve_the_separator_before_a_dash_prefixed_output_file() {
    let sandbox = Sandbox::new();
    sandbox.write("repo/--help", b"literal file payload\n");
    sandbox.write("repo/-payload", b"another literal payload\n");
    let before = sandbox.protected_state();
    for (file, expected) in [
        ("--help", b"literal file payload\n".as_slice()),
        ("-payload", b"another literal payload\n".as_slice()),
    ] {
        for prefix in [vec![], vec!["--quiet"], vec!["--color", "never"]] {
            let compact = sandbox
                .command()
                .args(&prefix)
                .args(["sift", "compact", "--", file])
                .output()
                .unwrap();
            assert_success(&compact);
            assert_eq!(compact.stdout, expected);
            let restored = sandbox
                .command()
                .args(&prefix)
                .args(["sift", "restore", "--encoding=raw", "--", file])
                .output()
                .unwrap();
            assert_success(&restored);
            assert_eq!(restored.stdout, expected);
        }
    }
    assert_eq!(sandbox.protected_state(), before);
    sandbox.assert_no_connection();
}

#[test]
fn scoped_snapshot_search_does_not_migrate_a_retired_history_config_setting() {
    let sandbox = Sandbox::new();
    sandbox.write("repo/workflow.py", PYTHON_SOURCE);
    sandbox.graph(&["index", "."]);
    sandbox.write(
        "history/config.toml",
        b"[upgrade]\nallow_rfc2544_fake_ip = true\n",
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Exercise valid retired-control normalization, not permission rejection.
        fs::set_permissions(
            sandbox.root.join("history/config.toml"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let before = sandbox.protected_state();
    for scope in ["graph", "all"] {
        let result = sandbox.json(&["search", "kernel", "--scope", scope, "--format=json"]);
        assert_search_envelope(&result, scope, 20, scope == "all");
        assert_eq!(result["graph"]["status"], "ok");
        if scope == "all" {
            assert_unavailable(&result["history"]);
        }
        assert_eq!(
            sandbox.protected_state(),
            before,
            "snapshot read migrated config"
        );
    }
    sandbox.assert_no_connection();
}

#[test]
fn compact_protocol_and_restore_preserve_complete_text_and_plain_output() {
    let sandbox = Sandbox::new();
    let original = format!(
        "{}last line without LF",
        "stage finished: \"ready\" \\ 雪\r\n".repeat(160)
    );
    let before = sandbox.protected_state();
    let compacted = sandbox.compact(&original);
    assert_ne!(compacted["encoding"], "raw");
    assert!(
        compacted["output_tokens"].as_u64().unwrap() < compacted["input_tokens"].as_u64().unwrap()
    );
    sandbox.assert_restored(&compacted, original.as_bytes());
    let plain = sandbox.output(&["sift", "compact"], original.as_bytes());
    assert_success(&plain);
    assert_eq!(plain.stdout, compacted["text"].as_str().unwrap().as_bytes());
    let short = sandbox.compact("end");
    assert_eq!(short["encoding"], "raw");
    assert_eq!(short["text"], "end");
    sandbox.assert_restored(&short, b"end");
    assert_eq!(sandbox.protected_state(), before);
    sandbox.assert_no_connection();
}

#[test]
fn documented_sift_framing_restores_without_reinterpreting_raw_bytes() {
    let sandbox = Sandbox::new();
    let framed = format!("{RUNS_HEADER}[[3,\"item\\r\\n\"],[1,\"tail\"]]");
    let restored = sandbox.output(
        &["sift", "restore", "--encoding", "text-runs-v1"],
        framed.as_bytes(),
    );
    assert_success(&restored);
    assert_eq!(restored.stdout, b"item\r\nitem\r\nitem\r\ntail");
    for bytes in [framed.as_bytes(), &b"\0\xff\x80\r\ntail"[..]] {
        let raw = sandbox.output(&["sift", "restore", "--encoding=raw", "-"], bytes);
        assert_success(&raw);
        assert_eq!(raw.stdout, bytes);
    }
    let binary = b"\0\xff\x80\r\ntail";
    sandbox.write("repo/-binary input", binary);
    let compacted = sandbox.output(&["sift", "compact", "--", "-binary input"], b"");
    assert_success(&compacted);
    assert_eq!(compacted.stdout, binary);
    for args in [
        vec!["sift", "restore"],
        vec!["sift", "restore", "--encoding=unknown"],
        vec!["sift", "restore", "--encoding=text-runs-v1"],
    ] {
        assert_failure(&sandbox.output(&args, b"not a frame"));
    }
    sandbox.assert_no_connection();
}

#[test]
fn compact_jsonl_rejects_bad_requests_and_continues_with_the_next_line() {
    let sandbox = Sandbox::new();
    let original = "synthetic error remains complete\r\n".repeat(100);
    let request = json!({
        "version":1, "text":original, "is_error":true,
        "complete":false, "tokenizer":"o200k_base"
    });
    let input = format!(
        "broken JSON\n{}\n{}\n{}\n{request}\n{}\n",
        json!({"version": 2, "text": "bad version"}),
        json!({"version": 1, "text": "bad tokenizer", "tokenizer": "unknown"}),
        json!({"version": 1, "text": "bad field", "unexpected": true}),
        json!({"version": 1, "text": "next request"})
    );
    let output = sandbox.output(&["sift", "compact", "--protocol=json-v1"], input.as_bytes());
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout).unwrap();
    let responses: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 6);
    for response in &responses[..4] {
        assert_eq!(response["version"], 1);
        assert!(response["error"].is_string());
        assert!(response.get("text").is_none());
    }
    sandbox.assert_restored(&responses[4], original.as_bytes());
    assert_eq!(responses[5]["text"], "next request");
    assert_eq!(responses[5]["encoding"], "raw");
}

#[test]
fn output_auxiliary_selection_and_unknown_command_keep_their_boundaries() {
    let sandbox = Sandbox::new();
    let selected = sandbox.output(
        &["sift", "read", "--from", "2", "--lines", "1"],
        b"one\r\ntwo\r\nthree",
    );
    assert_success(&selected);
    assert_eq!(selected.stdout, b"two\r\n");
    let selected = sandbox.output(
        &[
            "sift",
            "json",
            "--pointer",
            "/rows",
            "--field",
            "n",
            "--limit",
            "1",
        ],
        br#"{"rows":[{"n":17,"other":false},{"n":29}]}"#,
    );
    assert_eq!(json_output(selected), json!([{"n": 17}]));
    assert_failure(&sandbox.output(&["sift", "read", "--from", "0"], b"one"));
    assert_failure(&sandbox.output(&["sift", "unknown-output-command"], b""));
    sandbox.assert_no_connection();
}

#[cfg(unix)]
#[test]
fn run_preserves_argv_stdin_environment_streams_status_and_single_execution() {
    let sandbox = Sandbox::new();
    for separator in [false, true] {
        let mut command = sandbox.command();
        command.args(["--quiet", "sift", "run", "--raw"]);
        if separator {
            command.arg("--");
        }
        let output = command
            .args(["/bin/echo", "--color", "always", "--quiet"])
            .output()
            .unwrap();
        assert_success(&output);
        assert_eq!(
            output.stdout, b"--color always --quiet\n",
            "separator={separator}"
        );
    }
    // The child is an explicitly requested shell script. The wrapper must not
    // evaluate these argument values as shell source or consume its own flags.
    sandbox.write(
        "repo/child with spaces.sh",
        "printf 'invoked\\n' >> invocations\nprintf '<%s>\\n' \"$@\"\nprintf 'env=%s\\ncwd=%s\\n' \"$CTX_ACCEPTANCE_VALUE\" \"$PWD\"\n/bin/cat\nprintf 'child error without LF' >&2\nexit 37\n",
    );
    let arguments = [
        "space in one arg",
        "",
        "$(touch substituted)",
        "; touch injected",
        "`touch backtick`",
        "*",
        "'quoted'",
        "$HOME",
        "--help",
        "--json",
        "--raw",
    ];
    let input = b"stdin\0\xff\r\nno final newline";
    for flag in ["--raw", "--capture"] {
        let output = sandbox
            .command()
            .env("CTX_ACCEPTANCE_VALUE", "literal value")
            .args(["sift", "run", flag, "--", "/bin/sh", "child with spaces.sh"])
            .args(arguments)
            .write_stdin(input)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(37));
        let mut expected = arguments
            .iter()
            .map(|arg| format!("<{arg}>\n"))
            .collect::<String>()
            .into_bytes();
        expected.extend_from_slice(
            format!("env=literal value\ncwd={}\n", sandbox.repo().display()).as_bytes(),
        );
        expected.extend_from_slice(input);
        assert_eq!(output.stdout, expected);
        assert_eq!(output.stderr, b"child error without LF");
    }
    assert_eq!(
        fs::read(sandbox.repo().join("invocations")).unwrap(),
        b"invoked\ninvoked\n"
    );
    for marker in ["substituted", "injected", "backtick"] {
        assert!(!sandbox.repo().join(marker).exists());
    }
    sandbox.assert_no_connection();
}

#[cfg(unix)]
#[test]
fn capture_compacts_both_streams_and_opt_in_recall_returns_the_originals_once() {
    let sandbox = Sandbox::new();
    sandbox.write("output/config/config.json", br#"{"keep_originals":true}"#);
    sandbox.write("repo/emit.sh", "printf 'invoked\\n' >> invocations\ni=0\nwhile [ \"$i\" -lt 180 ]; do\n  printf 'output checkpoint complete\\r\\n'\n  printf 'error checkpoint retained\\r\\n' >&2\n  i=$((i + 1))\ndone\nexit 23\n");
    let stdout = "output checkpoint complete\r\n".repeat(180);
    let stderr = "error checkpoint retained\r\n".repeat(180);
    let output = sandbox.output(&["sift", "run", "--capture", "--", "/bin/sh", "emit.sh"], b"");
    assert_eq!(output.status.code(), Some(23));
    // Independently expand the documented run grammar instead of asking the
    // same codec to decide whether its own emitted representation is correct.
    for (stream, expected) in [(&output.stdout, &stdout), (&output.stderr, &stderr)] {
        assert!(stream.len() < expected.len());
        let text = std::str::from_utf8(stream).unwrap();
        let runs: Vec<(usize, String)> = serde_json::from_str(
            text.strip_prefix(RUNS_HEADER)
                .expect("documented text-runs frame"),
        )
        .unwrap();
        let expanded = runs
            .iter()
            .map(|(count, text)| text.repeat(*count))
            .collect::<String>();
        assert_eq!(&expanded, expected);
        let restored = sandbox.output(&["sift", "restore", "--encoding=text-runs-v1"], stream);
        assert_success(&restored);
        assert_eq!(restored.stdout, expected.as_bytes());
    }
    let listing = sandbox.output(&["sift", "recall", "--list"], b"");
    assert_success(&listing);
    let listing = String::from_utf8(listing.stdout).unwrap();
    let entries = listing.lines().collect::<Vec<_>>();
    assert_eq!(
        entries.len(),
        1,
        "one capture should yield one retained pair"
    );
    let id = entries[0].split('\t').next().unwrap();
    let recalled = sandbox.output(&["sift", "recall", id], b"");
    assert_success(&recalled);
    assert_eq!(recalled.stdout, stdout.as_bytes());
    let recalled = sandbox.output(&["sift", "recall", id, "--stderr"], b"");
    assert_success(&recalled);
    assert_eq!(recalled.stdout, stderr.as_bytes());
    let selected = sandbox.output(&["sift", "recall", id, "--from", "2", "--lines", "1"], b"");
    assert_success(&selected);
    assert_eq!(selected.stdout, b"output checkpoint complete\r\n");
    assert_failure(&sandbox.output(&["sift", "recall", "00000000000000000000000000000000"], b""));
    assert_eq!(
        fs::read(sandbox.repo().join("invocations")).unwrap(),
        b"invoked\n"
    );
    sandbox.assert_no_connection();
}

#[cfg(unix)]
#[test]
fn run_default_raw_missing_program_and_signal_status_are_preserved() {
    let sandbox = Sandbox::new();
    for args in [
        vec![
            "sift",
            "run",
            "--",
            "/bin/sh",
            "-c",
            "printf tiny; printf err >&2; exit 7",
        ],
        vec![
            "sift",
            "run",
            "--raw",
            "--",
            "/bin/sh",
            "-c",
            "printf tiny; printf err >&2; exit 7",
        ],
        vec![
            "--color",
            "never",
            "sift",
            "run",
            "--raw",
            "--",
            "/bin/sh",
            "-c",
            "printf tiny; printf err >&2; exit 7",
        ],
    ] {
        let output = sandbox.output(&args, b"");
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, b"tiny");
        assert_eq!(output.stderr, b"err");
    }
    // Raw takes precedence even for compressible text, and must preserve binary
    // stderr independently. These bytes would take another path under capture.
    let raw = sandbox.output(
        &[
            "sift", "run", "--raw", "--capture", "--", "/bin/sh", "-c",
            "i=0; while [ \"$i\" -lt 100 ]; do printf 'raw repeated row\\r\\n'; i=$((i + 1)); done; printf '\\377\\000end' >&2",
        ],
        b"",
    );
    assert!(raw.status.success());
    assert_eq!(raw.stdout, "raw repeated row\r\n".repeat(100).as_bytes());
    assert_eq!(raw.stderr, b"\xff\0end");
    let missing = sandbox.output(&["sift", "run", "--", "ctx-synthetic-missing-executable"], b"");
    assert_failure(&missing);
    assert_eq!(missing.status.code(), Some(127));
    let denied = sandbox.output(&["sift", "run", "--", "."], b"");
    assert_failure(&denied);
    assert_eq!(denied.status.code(), Some(126));
    assert_failure(&sandbox.output(&["sift", "run", "--"], b""));
    let signal = sandbox.output(
        &["sift", "run", "--raw", "--", "/bin/sh", "-c", "kill -TERM $$"],
        b"",
    );
    assert_eq!(signal.status.code(), Some(143));
    assert!(signal.stdout.is_empty());
    let originals = sandbox.output(&["sift", "recall", "--list"], b"");
    assert_success(&originals);
    assert!(originals.stdout.is_empty(), "retention must default off");
    sandbox.assert_no_connection();
}

#[test]
fn graph_and_output_bypass_missing_or_malformed_history_without_setup_mutations() {
    for malformed in [false, true] {
        let sandbox = Sandbox::new();
        sandbox.write("repo/workflow.py", PYTHON_SOURCE);
        // Unowned guidance, legacy stores and shell setup must stay untouched.
        for relative in [
            "home/.bashrc",
            "home/.zshrc",
            "providers/codex/skills/ctx/SKILL.md",
            "repo/.claude/settings.json",
            "repo/.cursor/rules/ctx.mdc",
            "repo/.mcp.json",
            "config/sift/config.json",
            "state/sift/originals/sentinel/stdout",
        ] {
            let sentinel = if relative.ends_with(".json") {
                "{\"mcpServers\":{},\"synthetic_user_owned\":\"untouched\"}\n"
            } else {
                "synthetic user-owned sentinel\n"
            };
            sandbox.write(relative, sentinel);
        }
        if malformed {
            sandbox.write("history/config.toml", b"[broken history configuration\n");
        }
        let before = sandbox.protected_state();
        sandbox.graph(&["index", "."]);
        assert_call(&sandbox.graph(&["callers", "kernel"]), "launch", "kernel");
        let compacted = sandbox.compact("history is optional");
        sandbox.assert_restored(&compacted, b"history is optional");
        // Execute the same real ctx as the child: this lane also works without
        // a platform-specific shell, and proves stdin crosses both boundaries.
        for flag in ["--raw", "--capture"] {
            let output = sandbox
                .command()
                .args(["sift", "run", flag, "--"])
                .arg(&sandbox.binary)
                .args(["sift", "restore", "--encoding=raw"])
                .write_stdin(b"through child\0\xff\r\n")
                .output()
                .unwrap();
            assert_success(&output);
            assert_eq!(output.stdout, b"through child\0\xff\r\n");
        }
        let original = sandbox.output(&["sift", "recall", "--list"], b"");
        assert_success(&original);
        assert!(original.stdout.is_empty());
        assert_eq!(
            sandbox.protected_state(),
            before,
            "implicit history, guidance or configuration mutation"
        );
        sandbox.assert_no_connection();
    }
}
