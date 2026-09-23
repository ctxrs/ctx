use std::{fs, io::Cursor, path::Path, time::Duration};

use ctx_agent_application::mcp::{serve_stdio, McpTelemetry, McpUsagePort, ProductIdentity};
use ctx_agent_integrations::{mcp::McpToolKind, tool_backend::ToolUsageFacts};
use graf::{
    model::{Edge, ImportedGraph, Node},
    store::Store,
};
use serde_json::{json, Value};
use tempfile::tempdir;

use crate::tool_backend::LocalToolBackend;

#[derive(Default)]
struct Usage(Vec<McpToolKind>);

impl McpUsagePort for Usage {
    fn record_delivered(
        &mut self,
        operation: McpToolKind,
        _: ToolUsageFacts,
        _: &Value,
        _: usize,
        _: Duration,
    ) {
        self.0.push(operation);
    }
}

fn call(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc":"2.0", "id":id, "method":"tools/call", "params":{"name":name, "arguments":arguments}})
}

fn exchange(backend: &LocalToolBackend, messages: Vec<Value>) -> (Vec<Value>, Usage) {
    let mut input =
        String::from("{\"jsonrpc\":\"2.0\",\"id\":0,\"method\":\"initialize\",\"params\":{}}\n");
    for message in messages {
        input.push_str(&format!("{message}\n"));
    }
    let mut input = Cursor::new(input.into_bytes());
    let mut output = Vec::new();
    let mut usage = Usage::default();
    serve_stdio(
        &mut input,
        &mut output,
        ProductIdentity {
            name: "ctx",
            version: "test",
        },
        backend,
        &super::text::render_tool_text,
        &mut usage,
        McpTelemetry::start(false, |_| panic!("test does not authorize telemetry")),
    )
    .unwrap();
    let lines = String::from_utf8(output).unwrap();
    let responses = lines
        .lines()
        .map(|line| serde_json::from_str(line).expect("stdio must contain only JSON-RPC"))
        .collect();
    (responses, usage)
}

fn node(id: &str, label: &str) -> Node {
    Node {
        id: id.into(),
        label: label.into(),
        kind: "function".into(),
        file: "src/example.rs".into(),
        line: Some(1),
        end_line: Some(3),
        qualified_name: None,
        binding_key: None,
        metadata: json!({}),
    }
}

fn edge(source: &str, target: &str) -> Edge {
    Edge {
        id: format!("{source}->{target}"),
        source: source.into(),
        target: target.into(),
        relation: "calls".into(),
        directed: true,
        file: Some("src/example.rs".into()),
        line: Some(2),
        confidence: "syntactic".into(),
        metadata: json!({}),
    }
}

fn graph_fixture(path: &Path) -> u64 {
    let mut store = Store::create(path).unwrap();
    store
        .import_graph(ImportedGraph {
            nodes: vec![
                node("caller", "caller"),
                node("middle", "middle"),
                node("leaf", "leaf"),
                node("duplicate:a", "duplicate"),
                node("duplicate:b", "duplicate"),
            ],
            edges: vec![edge("caller", "middle"), edge("middle", "leaf")],
            metadata: json!({}),
        })
        .unwrap()
        .generation
}

fn ids(value: &Value) -> Vec<&str> {
    let mut ids: Vec<_> = value["graph"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["id"].as_str().unwrap())
        .collect();
    ids.sort_unstable();
    ids
}

#[test]
fn unified_mcp_protocol_survives_history_failure_and_reads_graph_without_changes() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("history");
    fs::create_dir(&data_root).unwrap();
    fs::write(
        ctx_app_config::AppConfig::config_path(&data_root),
        "[malformed",
    )
    .unwrap();
    let db = temp.path().join("graph.db");
    let generation = graph_fixture(&db);
    let before = fs::read(&db).unwrap();
    let backend = LocalToolBackend::new(data_root).with_graph_db(Ok(Some(db.clone())));
    let (responses, usage) = exchange(
        &backend,
        vec![
            json!({"jsonrpc":"2.0", "id":1, "method":"tools/list"}),
            call(2, "status", json!({})),
            call(3, "graph_query", json!({"query":"caller", "depth":0})),
            call(4, "graph_show", json!({"symbol":"leaf"})),
            call(5, "graph_callers", json!({"symbol":"leaf"})),
            call(6, "graph_callees", json!({"symbol":"caller"})),
            call(7, "graph_impact", json!({"symbol":"leaf"})),
            call(8, "graph_path", json!({"source":"caller", "target":"leaf"})),
            call(9, "graph_stats", json!({})),
            call(10, "graph_show", json!({"symbol":"duplicate"})),
            call(
                11,
                "output_restore",
                json!({"text":"still alive\n", "encoding":"raw"}),
            ),
        ],
    );
    assert_eq!(responses.len(), 12);
    let names: Vec<_> = responses[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    for name in [
        "status",
        "sources",
        "search",
        "show_session",
        "show_event",
        "query_events",
        "blame",
        "graph_query",
        "output_compact",
        "output_restore",
    ] {
        assert!(names.contains(&name));
    }
    assert!(!names.contains(&"run") && !names.contains(&"shell"));
    assert_eq!(responses[2]["result"]["isError"], true);
    for response in &responses[3..10] {
        assert_ne!(response["result"]["isError"], true, "{response}");
        assert!(response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("generation"));
    }
    assert_eq!(
        ids(&responses[3]["result"]["structuredContent"]),
        ["caller"]
    );
    assert_eq!(ids(&responses[4]["result"]["structuredContent"]), ["leaf"]);
    assert_eq!(
        ids(&responses[5]["result"]["structuredContent"]),
        ["leaf", "middle"]
    );
    assert_eq!(
        ids(&responses[6]["result"]["structuredContent"]),
        ["caller", "middle"]
    );
    assert_eq!(
        ids(&responses[7]["result"]["structuredContent"]),
        ["caller", "leaf", "middle"]
    );
    assert_eq!(responses[8]["result"]["structuredContent"]["found"], true);
    assert_eq!(
        responses[9]["result"]["structuredContent"]["generation"],
        generation
    );
    assert_eq!(responses[9]["result"]["structuredContent"]["nodes"], 5);
    assert_eq!(responses[10]["result"]["isError"], true);
    assert!(responses[10]["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("ambiguous"));
    assert_eq!(
        responses[11]["result"]["structuredContent"]["text"],
        "still alive\n"
    );
    assert_eq!(usage.0, [McpToolKind::Status]);
    assert_eq!(fs::read(db).unwrap(), before);
}

#[test]
fn unified_mcp_fresh_root_needs_no_history_or_graph_for_pure_output() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("uncreated-history");
    let backend = LocalToolBackend::new(data_root.clone())
        .with_graph_db(Err("graph discovery failed".into()));
    let original = "synthetic compiler diagnostic: repeated complete line\n".repeat(200);
    let (responses, usage) = exchange(
        &backend,
        vec![
            call(1, "graph_stats", json!({})),
            call(2, "output_compact", json!({"text":original})),
        ],
    );
    assert_eq!(
        responses[1]["result"]["structuredContent"]["error_code"],
        "graph_unavailable"
    );
    let compacted = &responses[2]["result"]["structuredContent"];
    assert!(
        compacted["output_tokens"].as_u64().unwrap() < compacted["input_tokens"].as_u64().unwrap()
    );
    let (restored, restored_usage) = exchange(
        &backend,
        vec![call(
            1,
            "output_restore",
            json!({
                "text":compacted["text"], "encoding":compacted["encoding"],
            }),
        )],
    );
    assert_eq!(restored[1]["result"]["structuredContent"]["text"], original);
    assert!(usage.0.is_empty() && restored_usage.0.is_empty());
    assert!(!data_root.exists());
}

#[test]
fn unified_mcp_status_distinguishes_absence_from_graph_discovery_io_failure() {
    for (selection, expected_status, expected_error) in [
        (
            Ok(None),
            "not_indexed",
            "no graph database found at server startup",
        ),
        (
            Err("synthetic graph discovery I/O failure".to_owned()),
            "unavailable",
            "synthetic graph discovery I/O failure",
        ),
    ] {
        let temp = tempdir().unwrap();
        let data_root = temp.path().join("history");
        ctx_app_config::set_daemon_enabled(&data_root, false).unwrap();
        let backend = LocalToolBackend::new(data_root).with_graph_db(selection);
        let (responses, usage) = exchange(
            &backend,
            vec![
                call(1, "status", json!({})),
                call(2, "graph_stats", json!({})),
                call(
                    3,
                    "output_restore",
                    json!({"text":"after graph failure", "encoding":"raw"}),
                ),
            ],
        );
        assert_eq!(responses.len(), 4);
        let status = &responses[1]["result"];
        assert_ne!(status["isError"], true, "{status}");
        assert_eq!(
            status["structuredContent"]["graph"]["status"],
            expected_status
        );
        assert_eq!(status["structuredContent"]["output"]["status"], "available");
        let stats = &responses[2]["result"];
        assert_eq!(stats["isError"], true);
        assert_eq!(
            stats["structuredContent"]["error_code"],
            "graph_unavailable"
        );
        assert!(stats["structuredContent"]["error"]
            .as_str()
            .unwrap()
            .contains(expected_error));
        assert_eq!(
            responses[3]["result"]["structuredContent"]["text"],
            "after graph failure"
        );
        assert_eq!(usage.0, [McpToolKind::Status]);
    }
}

#[test]
fn unified_mcp_missing_graph_and_invalid_frames_are_tool_local() {
    let temp = tempdir().unwrap();
    let db = temp.path().join("absent/index.db");
    let backend =
        LocalToolBackend::new(temp.path().join("history")).with_graph_db(Ok(Some(db.clone())));
    let (responses, usage) = exchange(
        &backend,
        vec![
            call(1, "graph_stats", json!({})),
            call(
                2,
                "output_restore",
                json!({"encoding":"text-runs-v1", "text":"bad framing"}),
            ),
            call(
                3,
                "output_restore",
                json!({"encoding":"raw", "text":"sift:looks like framing\n"}),
            ),
        ],
    );
    assert_eq!(
        responses[1]["result"]["structuredContent"]["error_code"],
        "graph_unavailable"
    );
    assert_eq!(
        responses[2]["result"]["structuredContent"]["error_code"],
        "output_decode_failed"
    );
    assert_eq!(
        responses[3]["result"]["structuredContent"]["text"],
        "sift:looks like framing\n"
    );
    assert!(usage.0.is_empty());
    assert!(!db.parent().unwrap().exists());
}

#[test]
fn unified_mcp_explicit_database_is_selected_once_without_opening_it() {
    let temp = tempdir().unwrap();
    let explicit = temp.path().join("missing.db");
    assert_eq!(
        super::graph_database_at_startup(Some(explicit.clone())).unwrap(),
        Some(explicit.clone())
    );
    let relative = super::graph_database_at_startup(Some("relative.db".into())).unwrap();
    assert_eq!(
        relative,
        Some(std::env::current_dir().unwrap().join("relative.db"))
    );
    assert!(!explicit.exists());
}

#[test]
fn unified_mcp_restore_rejects_amplification_but_accepts_the_output_boundary() {
    use ctx_agent_integrations::tool_backend::MAX_OUTPUT_TEXT_BYTES;

    let temp = tempdir().unwrap();
    let backend = LocalToolBackend::new(temp.path().join("history"));
    let frame = |count: u64| {
        format!(
            "sift:text-runs-v1 counts repeat exact JSON strings; concatenate\n[[{count},\"x\"]]"
        )
    };
    let (responses, usage) = exchange(
        &backend,
        vec![
            call(
                1,
                "output_restore",
                json!({"encoding":"text-runs-v1", "text":frame(MAX_OUTPUT_TEXT_BYTES as u64)}),
            ),
            call(
                2,
                "output_restore",
                json!({"encoding":"text-runs-v1", "text":frame(MAX_OUTPUT_TEXT_BYTES as u64 + 1)}),
            ),
            call(
                3,
                "output_restore",
                json!({"encoding":"text-runs-v1", "text":frame(u64::MAX)}),
            ),
            call(
                4,
                "output_restore",
                json!({"encoding":"raw", "text":"after oversized output"}),
            ),
        ],
    );
    let boundary = responses[1]["result"]["structuredContent"]["text"]
        .as_str()
        .unwrap();
    assert_eq!(boundary.len(), MAX_OUTPUT_TEXT_BYTES);
    assert!(boundary.bytes().all(|byte| byte == b'x'));
    assert_eq!(
        responses[2]["result"]["structuredContent"]["error_code"],
        "output_limit_exceeded"
    );
    assert_eq!(
        responses[3]["result"]["structuredContent"]["error_code"],
        "output_decode_failed"
    );
    assert_eq!(
        responses[4]["result"]["structuredContent"]["text"],
        "after oversized output"
    );
    assert!(usage.0.is_empty());
}

#[test]
fn unified_mcp_graph_selection_observes_later_committed_generations() {
    let temp = tempdir().unwrap();
    let db = temp.path().join("graph.db");
    let initial = graph_fixture(&db);
    let backend =
        LocalToolBackend::new(temp.path().join("history")).with_graph_db(Ok(Some(db.clone())));
    let (first, _) = exchange(&backend, vec![call(1, "graph_stats", json!({}))]);
    assert_eq!(
        first[1]["result"]["structuredContent"]["generation"],
        initial
    );
    let updated = Store::open(&db)
        .unwrap()
        .refresh_import(ImportedGraph {
            nodes: vec![node("updated", "updated")],
            edges: vec![],
            metadata: json!({}),
        })
        .unwrap()
        .generation;
    let (next, _) = exchange(
        &backend,
        vec![call(1, "graph_query", json!({"query":"updated"}))],
    );
    assert!(updated > initial);
    assert_eq!(
        next[1]["result"]["structuredContent"]["graph"]["generation"],
        updated
    );
    assert_eq!(ids(&next[1]["result"]["structuredContent"]), ["updated"]);
}
