use std::{
    io::{self, Cursor, Error, ErrorKind, Write},
    sync::{Arc, Mutex},
    time::Duration,
};

use ctx_agent_integrations::tool_backend::{
    ToolBackend, ToolExecutionError, ToolOperation, ToolOutcome, ToolUsageFacts,
};
use ctx_client_observability::{
    analytics::{Outcome, SearchFailurePhase, SearchTerminalFacts},
    operation_descriptor::{ObservedMcpProductOperation, OperationDescriptor},
};
use ctx_history_core::CaptureProvider;
use serde_json::{json, Value};

use super::*;

#[derive(Clone, Copy, Debug)]
enum OutputFailure {
    None,
    Write,
    Flush,
}

struct TracedWriter {
    failure: OutputFailure,
    trace: Arc<Mutex<Vec<&'static str>>>,
    bytes: Vec<u8>,
}

impl Write for TracedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if matches!(self.failure, OutputFailure::Write) {
            self.trace.lock().unwrap().push("write_failed");
            return Err(Error::new(ErrorKind::BrokenPipe, "test write failure"));
        }
        self.trace.lock().unwrap().push("write");
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if matches!(self.failure, OutputFailure::Flush) {
            self.trace.lock().unwrap().push("flush_failed");
            return Err(Error::new(ErrorKind::BrokenPipe, "test flush failure"));
        }
        self.trace.lock().unwrap().push("flush");
        Ok(())
    }
}

struct TestBackend;

impl ToolBackend for TestBackend {
    fn execute(&self, _operation: ToolOperation) -> Result<ToolOutcome, ToolExecutionError> {
        Ok(ToolOutcome::plain(json!({"payload_type": "status"})))
    }

    fn parse_provider(&self, _value: &str) -> Option<CaptureProvider> {
        None
    }

    fn provider_names(&self) -> Vec<&'static str> {
        Vec::new()
    }
}

struct TracedUsagePort(Arc<Mutex<Vec<&'static str>>>);

type ResponseRun = (
    Result<(), McpServeFailure>,
    Vec<&'static str>,
    Vec<(Outcome, SearchTerminalFacts)>,
);

impl McpUsagePort for TracedUsagePort {
    fn record_delivered(
        &mut self,
        _operation: McpToolKind,
        _usage: ToolUsageFacts,
        _response: &Value,
        _encoded_response_bytes: usize,
        _duration: Duration,
    ) {
        self.0.lock().unwrap().push("local_usage");
    }
}

fn run_one_response(failure: OutputFailure, tool: &str, arguments: Value) -> ResponseRun {
    let request = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": tool,
        "method": "tools/call",
        "params": {"name": tool, "arguments": arguments}
    }))
    .unwrap();
    let initialized = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }))
    .unwrap();
    let mut input = Cursor::new([initialized, vec![b'\n'], request, vec![b'\n']].concat());
    let trace = Arc::new(Mutex::new(Vec::new()));
    let mut output = TracedWriter {
        failure,
        trace: trace.clone(),
        bytes: Vec::new(),
    };
    let delivery_trace = trace.clone();
    let search_events = Arc::new(Mutex::new(Vec::new()));
    let recorded_search_events = search_events.clone();
    let telemetry = McpTelemetry::start(true, move |events| {
        let mut trace = delivery_trace.lock().unwrap();
        for event in events {
            let label = match event {
                ctx_client_observability::analytics::PublicEventV1::OperationCompleted(event) => {
                    match &event.descriptor {
                        OperationDescriptor::Mcp(operation) => {
                            if operation.product_operation()
                                == Some(ObservedMcpProductOperation::Search)
                            {
                                if let Some(search) = operation.result().search {
                                    recorded_search_events
                                        .lock()
                                        .unwrap()
                                        .push((event.outcome, search));
                                }
                            }
                            "submit_mcp"
                        }
                        _ => continue,
                    }
                }
                _ => continue,
            };
            trace.push(label);
        }
        Ok(())
    });
    let mut usage = TracedUsagePort(trace.clone());
    let result = serve_stdio(
        &mut input,
        &mut output,
        ProductIdentity {
            name: "ctx",
            version: "test",
        },
        &TestBackend,
        &render_generic_tool_text,
        &mut usage,
        telemetry,
    );
    let trace = trace.lock().unwrap().clone();
    let search_events = search_events.lock().unwrap().clone();
    (result, trace, search_events)
}

#[test]
fn response_flush_precedes_the_one_usage_commit_and_post_flush_telemetry() {
    let (delivered, trace, _) = run_one_response(OutputFailure::None, "status", json!({}));
    assert!(delivered.is_ok());
    assert_eq!(
        trace
            .iter()
            .filter(|entry| **entry == "local_usage")
            .count(),
        1
    );
    let flushed_at = trace.iter().position(|entry| *entry == "flush").unwrap();
    let recorded_at = trace
        .iter()
        .position(|entry| *entry == "local_usage")
        .unwrap();
    assert!(flushed_at < recorded_at, "{trace:?}");
    let submitted_at = trace
        .iter()
        .position(|entry| *entry == "submit_mcp")
        .unwrap();
    assert!(recorded_at < submitted_at, "{trace:?}");

    for failure in [OutputFailure::Write, OutputFailure::Flush] {
        let (result, trace, _) = run_one_response(failure, "status", json!({}));
        assert!(matches!(
            result.unwrap_err().reason,
            McpStopReasonV1::StdoutWriteError | McpStopReasonV1::StdoutFlushError
        ));
        assert!(!trace.contains(&"local_usage"), "{trace:?}");
    }
}

#[test]
fn early_search_validation_failure_keeps_preparation_and_actual_delivery_facts() {
    let (result, _, events) = run_one_response(OutputFailure::None, "search", json!({}));

    assert!(result.is_ok());
    let (outcome, search) = events.into_iter().next().expect("search terminal event");
    assert_eq!(outcome, Outcome::Failure);
    assert_eq!(
        search.health.failure_phase,
        Some(SearchFailurePhase::Preparation)
    );
    assert_eq!(search.output_served, Some(true));
    assert!(search.output_duration.is_some());
}

#[test]
fn json_rpc_search_validation_failure_keeps_preparation_and_delivery_facts_without_result() {
    let (result, _, events) = run_one_response(OutputFailure::None, "search", json!("invalid"));

    assert!(result.is_ok());
    let (outcome, search) = events.into_iter().next().expect("search terminal event");
    assert_eq!(outcome, Outcome::Failure);
    assert_eq!(
        search.health.failure_phase,
        Some(SearchFailurePhase::Preparation)
    );
    assert_eq!(search.output_served, Some(true));
    assert!(search.output_duration.is_some());
}

#[test]
fn search_write_and_flush_failures_are_unserved_output_failures() {
    for failure in [OutputFailure::Write, OutputFailure::Flush] {
        let (result, trace, events) =
            run_one_response(failure, "search", json!({"query": "needle"}));

        assert!(result.is_err());
        assert!(!trace.contains(&"local_usage"), "{trace:?}");
        let (outcome, search) = events.into_iter().next().expect("search terminal event");
        assert_eq!(outcome, Outcome::Failure);
        assert_eq!(
            search.health.failure_phase,
            Some(SearchFailurePhase::Output)
        );
        assert_eq!(search.output_served, Some(false));
        assert!(search.output_duration.is_some());
    }
}

#[test]
fn search_serialization_failure_is_unserved_with_render_phase() {
    let mut usage = Some(McpUsage {
        operation: McpToolKind::Search,
        facts: ToolUsageFacts::search_preparation(),
    });

    mark_search_failure(&mut usage, ToolSearchFailurePhase::Render, None);

    let search = usage
        .unwrap()
        .facts
        .search_execution
        .expect("search terminal facts");
    assert_eq!(search.failure_phase, Some(ToolSearchFailurePhase::Render));
    assert_eq!(search.output_served, Some(false));
    assert_eq!(search.output_duration, None);
}

#[test]
fn malformed_input_recovers_with_exact_json_rpc_parse_error() {
    let mut input =
        Cursor::new(b"\xff\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n".to_vec());
    let mut output = Vec::new();
    let mut usage = TracedUsagePort(Arc::new(Mutex::new(Vec::new())));
    let result = serve_stdio(
        &mut input,
        &mut output,
        ProductIdentity {
            name: "ctx",
            version: "test",
        },
        &TestBackend,
        &render_generic_tool_text,
        &mut usage,
        McpTelemetry::start(false, |_| Ok(())),
    );
    assert!(result.is_ok());
    let lines = String::from_utf8(output).unwrap();
    assert!(lines.contains("MCP message is not valid UTF-8"));
    assert!(lines.contains("\"id\":null"));
    assert!(lines.contains("\"id\":1"));
}

// Authored transport fixtures: the final backend supplies both renderings and
// terminal query facts. These do not stand in for attribution engine tests.
struct NativeBlameBackend(ctx_client_observability::analytics::BlameTerminalFacts);

impl ToolBackend for NativeBlameBackend {
    fn execute(&self, operation: ToolOperation) -> Result<ToolOutcome, ToolExecutionError> {
        let ToolOperation::Blame {
            target,
            limit,
            cursor,
        } = operation
        else {
            panic!("native Blame dispatch expected");
        };
        assert!(matches!(
            target,
            ctx_agent_integrations::tool_backend::BlameTarget::File { .. }
        ));
        assert_eq!(limit, 8);
        assert_eq!(cursor.as_deref(), Some("authored-continuation"));
        Ok(ToolOutcome {
            structured: json!({"fixture":"native-blame","evidence":[{"citation":"ctx show event authored-event"}]}),
            text: Some("Authored cited result\nctx show event authored-event\n".into()),
            compact: None,
            usage: ToolUsageFacts {
                blame: Some(self.0),
                ..ToolUsageFacts::default()
            },
        })
    }
    fn parse_provider(&self, _: &str) -> Option<CaptureProvider> {
        None
    }
    fn provider_names(&self) -> Vec<&'static str> {
        Vec::new()
    }
}

#[derive(Default)]
struct BlameUsage(
    Vec<(
        usize,
        ctx_client_observability::analytics::BlameTerminalFacts,
    )>,
);

impl McpUsagePort for BlameUsage {
    fn record_delivered(
        &mut self,
        operation: McpToolKind,
        usage: ToolUsageFacts,
        _: &Value,
        bytes: usize,
        _: Duration,
    ) {
        assert_eq!(operation, McpToolKind::Blame);
        self.0
            .push((bytes, usage.blame.expect("same native query facts")));
    }
}

#[test]
fn native_blame_delivers_both_renderings_and_one_terminal_with_exact_output_facts() {
    use ctx_client_observability::analytics::{
        BlameFreshness, BlameRequestKind, BlameResultFacts, BlameResultState, BlameTargetKind,
        BlameTerminalFacts,
    };

    for state in [
        BlameResultState::Proven,
        BlameResultState::Possible,
        BlameResultState::Conflicting,
        BlameResultState::None,
    ] {
        for failure in [
            OutputFailure::None,
            OutputFailure::Write,
            OutputFailure::Flush,
        ] {
            let mut facts = BlameTerminalFacts::new(BlameTargetKind::File);
            facts.request_kind = Some(BlameRequestKind::Continuation);
            facts.query_duration = Some(Duration::from_millis(3));
            facts.result = Some(BlameResultFacts {
                state,
                evaluated: 2,
                freshness: BlameFreshness::StaleCommitted,
                has_more: true,
            });
            let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"blame","arguments":{"target":{"kind":"file","path":"authored.rs"},"cursor":"authored-continuation"}}});
            let mut input = Cursor::new(format!(
                "{{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}}\n{request}\n"
            ));
            let mut output = TracedWriter {
                failure,
                trace: Arc::new(Mutex::new(Vec::new())),
                bytes: Vec::new(),
            };
            let events = Arc::new(Mutex::new(Vec::new()));
            let recorded = events.clone();
            let telemetry = McpTelemetry::start(true, move |batch| {
                for event in batch {
                    if let ctx_client_observability::analytics::PublicEventV1::OperationCompleted(
                        event,
                    ) = event
                    {
                        if let OperationDescriptor::Mcp(operation) = &event.descriptor {
                            if operation.product_operation()
                                == Some(ObservedMcpProductOperation::Blame)
                            {
                                recorded.lock().unwrap().push((
                                    event.outcome,
                                    operation.result().blame.expect("native Blame facts"),
                                ));
                            }
                        }
                    }
                }
                Ok(())
            });
            let mut usage = BlameUsage::default();
            let result = serve_stdio(
                &mut input,
                &mut output,
                ProductIdentity {
                    name: "ctx",
                    version: "test",
                },
                &NativeBlameBackend(facts),
                &|_: &Value| -> String { panic!("native backend already rendered text") },
                &mut usage,
                telemetry,
            );
            let events = events.lock().unwrap();
            assert_eq!(events.len(), 1);
            let (outcome, observed) = events[0];
            assert_eq!(
                observed.result, facts.result,
                "output failure must preserve the query result"
            );
            assert_eq!(observed.query_duration, facts.query_duration);
            assert_eq!(observed.request_kind, facts.request_kind);
            match failure {
                OutputFailure::None => {
                    result.unwrap();
                    assert_eq!(outcome, Outcome::Success);
                    assert_eq!(observed.output_served, Some(true));
                    assert_eq!(observed.failure, None);
                    assert_eq!(usage.0, vec![(output.bytes.len(), observed)]);
                    let response: Value = serde_json::from_slice(&output.bytes).unwrap();
                    assert_eq!(
                        response["result"]["content"][0]["text"],
                        "Authored cited result\nctx show event authored-event\n"
                    );
                    assert_eq!(
                        response["result"]["structuredContent"]["evidence"][0]["citation"],
                        "ctx show event authored-event"
                    );
                }
                OutputFailure::Write | OutputFailure::Flush => {
                    assert!(result.is_err());
                    assert!(
                        usage.0.is_empty(),
                        "undelivered response cannot count local usage"
                    );
                    assert_eq!(outcome, Outcome::Failure);
                    assert_eq!(observed.output_served, Some(false));
                    assert_eq!(observed.failure.unwrap().phase, BlameFailurePhase::Output);
                }
            }
        }
    }
}
