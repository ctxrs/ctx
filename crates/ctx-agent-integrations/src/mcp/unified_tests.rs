use std::sync::Mutex;

use serde_json::{json, Value};

use super::*;
use crate::{
    mcp::{
        handle_protocol_message, McpServerIdentity, RequestDescriptor,
        MCP_PRESENTATION_MAX_OUTPUT_BYTES,
    },
    tool_backend::{ToolExecutionError, ToolOperation},
};

struct HistoryOnly;

impl ToolBackend for HistoryOnly {
    fn execute(&self, _: ToolOperation) -> Result<ToolOutcome, ToolExecutionError> {
        panic!("new tools must not enter the history backend")
    }
    fn parse_provider(&self, _: &str) -> Option<ctx_history_core::CaptureProvider> {
        None
    }
    fn provider_names(&self) -> Vec<&'static str> {
        Vec::new()
    }
}

#[derive(Default)]
struct RecordingBackend(Mutex<Vec<UnifiedToolOperation>>);

impl ToolBackend for RecordingBackend {
    fn execute(&self, _: ToolOperation) -> Result<ToolOutcome, ToolExecutionError> {
        panic!("new tools must not enter the history backend")
    }
    fn execute_unified(
        &self,
        operation: UnifiedToolOperation,
    ) -> Result<ToolOutcome, ToolExecutionError> {
        self.0.lock().unwrap().push(operation);
        let mut result = ToolOutcome::plain(json!({"accepted": true}));
        result.text = Some("accepted".to_owned());
        Ok(result)
    }
    fn parse_provider(&self, _: &str) -> Option<ctx_history_core::CaptureProvider> {
        None
    }
    fn provider_names(&self) -> Vec<&'static str> {
        Vec::new()
    }
}

fn call(backend: &impl ToolBackend, name: &str, arguments: Value) -> Value {
    let request = json!({"jsonrpc":"2.0", "id":7, "method":"tools/call", "params":{"name":name, "arguments":arguments}});
    let handled = handle_protocol_message(
        request.clone(),
        RequestDescriptor::from_message(&request),
        &mut true,
        McpServerIdentity {
            name: "ctx",
            version: "test",
        },
        backend,
        |_| panic!("backend owns the new tools' text projection"),
    );
    let usage = handled
        .usage
        .as_ref()
        .expect("unified tools retain their carrier");
    assert!(matches!(usage.operation, McpToolKind::Unified(_)));
    assert!(
        usage.facts.search.is_none()
            && usage.facts.search_execution.is_none()
            && usage.facts.blame.is_none(),
        "graph/output has no history usage authority"
    );
    let response = handled.value.unwrap();
    assert_eq!(response["id"], 7);
    assert!(
        response.get("error").is_none(),
        "expected a tool-local response: {response}"
    );
    response["result"].clone()
}

#[test]
fn unified_tools_are_closed_read_only_schemas_without_execution_or_paths() {
    let tools = definitions();
    assert_eq!(tools.len(), 9);
    for definition in tools {
        assert_eq!(definition["annotations"]["readOnlyHint"], true);
        assert_eq!(definition["annotations"]["openWorldHint"], false);
        assert_eq!(definition["inputSchema"]["additionalProperties"], false);
        let properties = definition["inputSchema"]["properties"].as_object().unwrap();
        for forbidden in [
            "path", "db", "graph_db", "project", "provider", "command", "shell",
        ] {
            assert!(!properties.contains_key(forbidden));
        }
        let name = definition["name"].as_str().unwrap();
        assert_eq!(
            crate::mcp::McpToolKind::from_tool_name(Some(name)).tool_name(),
            name
        );
    }
}

#[test]
fn unrelated_backends_return_typed_unsupported_without_history_observation() {
    let result = call(&HistoryOnly, "graph_stats", json!({}));
    assert_eq!(result["isError"], true);
    assert_eq!(
        result["structuredContent"]["error_code"],
        "unsupported_tool"
    );
    assert!(result["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("does not support"));
}

#[test]
fn parses_bounded_graph_and_pure_output_requests() {
    let backend = RecordingBackend::default();
    let cases = [
        (
            "graph_query",
            json!({"query":"main", "limit":200, "depth":6, "direction":"incoming", "relation":"calls"}),
        ),
        ("graph_show", json!({"symbol":"node:main"})),
        ("graph_callers", json!({"symbol":"main"})),
        ("graph_callees", json!({"symbol":"main"})),
        ("graph_impact", json!({"symbol":"main"})),
        ("graph_path", json!({"source":"main", "target":"leaf"})),
        ("graph_stats", json!({})),
        ("output_compact", json!({"text":""})),
        (
            "output_restore",
            json!({"text":"unchanged", "encoding":"raw"}),
        ),
    ];
    for (name, args) in cases {
        assert_eq!(
            call(&backend, name, args)["structuredContent"]["accepted"],
            true
        );
    }
    let recorded = backend.0.lock().unwrap();
    assert_eq!(recorded.len(), 9);
    assert!(
        matches!(&recorded[0], UnifiedToolOperation::Graph(GraphOperation::Query { options, .. })
        if options.depth == 6 && options.limit == 200 && options.direction == GraphDirection::Incoming && options.relation.as_deref() == Some("calls"))
    );
    assert!(
        matches!(&recorded[5], UnifiedToolOperation::Graph(GraphOperation::Path { options, .. })
        if options.depth == 6 && options.direction == GraphDirection::Outgoing)
    );
}

#[test]
fn rejects_paths_invalid_types_and_utf8_byte_overflow_before_backend() {
    let cases = [
        ("graph_stats", json!({"graph_db":"elsewhere.db"})),
        ("graph_query", json!({"query":"main", "limit":0})),
        ("graph_query", json!({"query":"main", "limit":201})),
        ("graph_query", json!({"query":"main", "depth":7})),
        ("graph_query", json!({"query":"main", "depth":-1})),
        ("graph_query", json!({"query":"main", "limit":1.5})),
        (
            "graph_query",
            json!({"query":"main", "direction":"sideways"}),
        ),
        ("graph_query", json!({"query":"main", "relation":" "})),
        ("graph_query", json!({"query":"é".repeat(513)})),
        ("graph_show", json!({"symbol":" "})),
        (
            "graph_callers",
            json!({"symbol":"main", "direction":"outgoing"}),
        ),
        ("graph_path", json!({"source":"main"})),
        ("output_compact", json!({"text":7})),
        (
            "output_compact",
            json!({"text":"é".repeat(MAX_OUTPUT_INPUT_BYTES / 2 + 1)}),
        ),
        (
            "output_compact",
            json!({"text":"data", "provider":"remote"}),
        ),
        ("output_restore", json!({"text":"data"})),
        ("output_restore", json!({"text":"data", "encoding":"infer"})),
    ];
    for (name, args) in cases {
        let result = call(&HistoryOnly, name, args);
        assert_eq!(result["isError"], true, "{name}");
        assert_eq!(
            result["structuredContent"]["error_code"], "invalid_request",
            "{name}"
        );
    }
    assert!(parse(
        UnifiedToolKind::OutputCompact,
        &json!({"text":"x".repeat(MAX_OUTPUT_INPUT_BYTES)})
    )
    .is_ok());
}

#[test]
fn response_bound_counts_json_escapes_and_preserves_id_and_errors() {
    let id = json!("\\".repeat(4096));
    let oversized = success_response(
        id.clone(),
        tool_result_with_text(
            json!({}),
            "\0".repeat(MCP_PRESENTATION_MAX_OUTPUT_BYTES / 6),
        ),
    );
    assert!(serialized_json_line_bytes(&oversized).unwrap() > MCP_PRESENTATION_MAX_OUTPUT_BYTES);
    let bounded = bound_response(oversized, id.clone(), &mut None);
    assert_eq!(bounded["id"], id);
    assert_eq!(bounded["result"]["isError"], true);
    assert_eq!(
        bounded["result"]["structuredContent"]["error_code"],
        "output_limit_exceeded"
    );
    assert!(serialized_json_line_bytes(&bounded).unwrap() < MCP_PRESENTATION_MAX_OUTPUT_BYTES);
    let ordinary = success_response(json!(1), json!({"ok":true}));
    assert_eq!(
        bound_response(ordinary.clone(), json!(1), &mut None),
        ordinary
    );
}

#[test]
fn known_unified_parse_failures_keep_closed_facts_without_entering_a_backend() {
    use ctx_client_observability::analytics as a;
    for (name, arguments) in [
        ("graph_path", json!({"source":"synthetic"})),
        ("graph_stats", json!([])),
        (
            "output_restore",
            json!({"text":"synthetic", "encoding":"unknown"}),
        ),
    ] {
        let request = json!({"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{"name":name,"arguments":arguments}});
        let handled = handle_protocol_message(
            request.clone(),
            RequestDescriptor::from_message(&request),
            &mut true,
            McpServerIdentity {
                name: "ctx",
                version: "test",
            },
            &HistoryOnly,
            |_| panic!("invalid input has no renderer"),
        );
        let facts = handled.usage.unwrap().facts;
        if name.starts_with("graph") {
            let graph = facts.graph.unwrap();
            assert_eq!(
                graph.completion.execution.unwrap_err().stage,
                a::ProductFailureStage::Parse
            );
            assert_eq!(
                graph.details.failure.unwrap().kind,
                a::GraphFailureKind::InvalidInput
            );
            assert_eq!(graph.completion.delivery, a::DeliveryEvidence::Unknown);
        } else {
            let sift = facts.sift.unwrap();
            assert_eq!(sift.execution_failed, 1);
            assert_eq!(sift.tokens, None);
        }
    }
}

#[test]
fn bounded_response_preserves_successful_graph_work_and_clears_sift_candidates() {
    use ctx_client_observability::analytics as a;
    let mut sift = a::SiftSummaryV1::new(
        a::SiftOperation::Compact,
        a::SiftMode::Lossless,
        std::time::Duration::ZERO,
    );
    sift.observed = 1;
    sift.complete = 1;
    sift.bytes = Some(a::PresentedTotals {
        samples: 1,
        input: 10,
        output: 1,
    });
    sift.tokens = sift.bytes;
    let mut usage = Some(McpUsage {
        operation: McpToolKind::Unified(UnifiedToolKind::GraphStats),
        facts: ToolUsageFacts {
            graph: Some(a::GraphCompletedV1::new(
                a::GraphOperation::Stats,
                a::GraphSurface::Mcp,
                a::ProductCompletion::new(
                    std::time::Duration::ZERO,
                    Ok(()),
                    a::DeliveryEvidence::Unknown,
                    a::ProductOutput::Mcp,
                ),
            )),
            sift: Some(sift),
            ..Default::default()
        },
    });
    let oversized = success_response(
        json!(1),
        tool_result_with_text(json!({}), "x".repeat(MCP_PRESENTATION_MAX_OUTPUT_BYTES)),
    );
    let bounded = bound_response(oversized, json!(1), &mut usage);
    assert_eq!(bounded["result"]["isError"], true);
    let facts = usage.unwrap().facts;
    assert!(facts.graph.unwrap().completion.execution.is_ok());
    assert_eq!(
        facts.graph.unwrap().completion.delivery,
        a::DeliveryEvidence::Failed
    );
    let sift = facts.sift.unwrap();
    assert_eq!(sift.execution_failed, 0);
    assert_eq!(sift.output_failed, 1);
    assert_eq!(sift.bytes, None);
    assert_eq!(sift.tokens, None);
    assert_eq!(sift.partial, 1);
    assert!(sift.into_event().is_some());
}
