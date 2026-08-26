use std::{
    io::{self, Cursor, Error, ErrorKind, Write},
    sync::{Arc, Mutex},
    time::Duration,
};

use ctx_agent_integrations::tool_backend::{
    OpaqueMcpDeliveryOutcome, OpaqueMcpProxyError, OpaqueMcpProxyResponse, ToolBackend,
    ToolExecutionError, ToolOperation, ToolOutcome, ToolUsageFacts,
};
use ctx_client_observability::{
    analytics::{Outcome, SearchFailurePhase, SearchTerminalFacts},
    operation_descriptor::{ObservedMcpProductOperation, OperationDescriptor},
};
use ctx_history_core::CaptureProvider;
use serde_json::{json, Value};

use super::*;

#[derive(Clone, Copy)]
enum OutputFailure {
    None,
    Write,
    Flush,
}

struct TracedWriter {
    failure: OutputFailure,
    trace: Arc<Mutex<Vec<&'static str>>>,
    output: Vec<u8>,
}

impl Write for TracedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if matches!(self.failure, OutputFailure::Write) {
            self.trace.lock().unwrap().push("write_failed");
            return Err(Error::new(ErrorKind::BrokenPipe, "test write failure"));
        }
        self.trace.lock().unwrap().push("write");
        self.output.extend_from_slice(bytes);
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

    fn proxy_companion_mcp(
        &self,
        _request: &[u8],
    ) -> Result<OpaqueMcpProxyResponse, OpaqueMcpProxyError> {
        panic!("Core status must not use the companion")
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
        output: Vec::new(),
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
    assert!(!trace.contains(&"submit_pro"), "{trace:?}");

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

struct RecordingProxyBackend {
    request: Mutex<Vec<u8>>,
    response: Vec<u8>,
    completions: Arc<Mutex<Vec<OpaqueMcpDeliveryOutcome>>>,
    trace: Arc<Mutex<Vec<&'static str>>>,
}

impl RecordingProxyBackend {
    fn new(response: Vec<u8>) -> Self {
        Self::with_trace(response, Arc::new(Mutex::new(Vec::new())))
    }

    fn with_trace(response: Vec<u8>, trace: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            request: Mutex::new(Vec::new()),
            response,
            completions: Arc::new(Mutex::new(Vec::new())),
            trace,
        }
    }
}

impl ToolBackend for RecordingProxyBackend {
    fn execute(&self, _operation: ToolOperation) -> Result<ToolOutcome, ToolExecutionError> {
        panic!("companion-owned calls must bypass public tool execution")
    }

    fn proxy_companion_mcp(
        &self,
        request: &[u8],
    ) -> Result<OpaqueMcpProxyResponse, OpaqueMcpProxyError> {
        self.request.lock().unwrap().extend_from_slice(request);
        let completions = Arc::clone(&self.completions);
        let trace = Arc::clone(&self.trace);
        OpaqueMcpProxyResponse::new(self.response.clone(), move |outcome| {
            completions.lock().unwrap().push(outcome);
            trace.lock().unwrap().push(match outcome {
                OpaqueMcpDeliveryOutcome::WrittenAndFlushed => "written_and_flushed",
                OpaqueMcpDeliveryOutcome::OutputFailed => "output_failed",
            });
        })
        .ok_or(OpaqueMcpProxyError::CompanionIncompatible)
    }

    fn parse_provider(&self, _value: &str) -> Option<CaptureProvider> {
        None
    }

    fn provider_names(&self) -> Vec<&'static str> {
        Vec::new()
    }
}

#[test]
fn companion_owned_call_is_proxied_as_exact_opaque_bytes() {
    let request = b" {\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"tools/call\",\"params\":{\"name\":\"blame\",\"arguments\":{\"private\":\"opaque\"}}} \n";
    let initialized = b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n";
    let mut input = Cursor::new([initialized.as_slice(), request.as_slice()].concat());
    let expected_response = b"opaque\xff\n".to_vec();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let backend = RecordingProxyBackend::with_trace(expected_response.clone(), trace.clone());
    let mut output = TracedWriter {
        failure: OutputFailure::None,
        trace: trace.clone(),
        output: Vec::new(),
    };
    let mut usage = TracedUsagePort(Arc::new(Mutex::new(Vec::new())));

    serve_stdio(
        &mut input,
        &mut output,
        ProductIdentity {
            name: "ctx",
            version: "test",
        },
        &backend,
        &render_generic_tool_text,
        &mut usage,
        McpTelemetry::start(false, |_| Ok(())),
    )
    .unwrap();

    assert_eq!(*backend.request.lock().unwrap(), request);
    assert_eq!(output.output, expected_response);
    assert_eq!(
        *backend.completions.lock().unwrap(),
        [OpaqueMcpDeliveryOutcome::WrittenAndFlushed]
    );
    let trace = trace.lock().unwrap();
    let flushed_at = trace.iter().position(|entry| *entry == "flush").unwrap();
    let completed_at = trace
        .iter()
        .position(|entry| *entry == "written_and_flushed")
        .unwrap();
    assert!(flushed_at < completed_at, "{trace:?}");
}

#[test]
fn companion_owned_write_and_flush_failures_complete_as_output_failed_once() {
    let request = b"{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"tools/call\",\"params\":{\"name\":\"blame\",\"arguments\":{}}}\n";
    let initialized = b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n";
    let expected_response = b"opaque\xff\n".to_vec();

    for (failure, expected_reason, failed_label) in [
        (
            OutputFailure::Write,
            McpStopReasonV1::StdoutWriteError,
            "write_failed",
        ),
        (
            OutputFailure::Flush,
            McpStopReasonV1::StdoutFlushError,
            "flush_failed",
        ),
    ] {
        let mut input = Cursor::new([initialized.as_slice(), request.as_slice()].concat());
        let trace = Arc::new(Mutex::new(Vec::new()));
        let backend = RecordingProxyBackend::with_trace(expected_response.clone(), trace.clone());
        let mut output = TracedWriter {
            failure,
            trace: trace.clone(),
            output: Vec::new(),
        };
        let mut usage = TracedUsagePort(Arc::new(Mutex::new(Vec::new())));

        let failure = serve_stdio(
            &mut input,
            &mut output,
            ProductIdentity {
                name: "ctx",
                version: "test",
            },
            &backend,
            &render_generic_tool_text,
            &mut usage,
            McpTelemetry::start(false, |_| Ok(())),
        )
        .unwrap_err();

        assert_eq!(failure.reason, expected_reason);
        assert_eq!(
            *backend.completions.lock().unwrap(),
            [OpaqueMcpDeliveryOutcome::OutputFailed]
        );
        let trace = trace.lock().unwrap();
        let failed_at = trace
            .iter()
            .position(|entry| *entry == failed_label)
            .unwrap();
        let completed_at = trace
            .iter()
            .position(|entry| *entry == "output_failed")
            .unwrap();
        assert!(failed_at < completed_at, "{trace:?}");
    }
}

fn tools_list_with_backend(backend: &impl ToolBackend) -> Value {
    let initialized = b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n";
    let request = b" {\"jsonrpc\":\"2.0\",\"id\":11,\"method\":\"tools/list\"} \n";
    let mut input = Cursor::new([initialized.as_slice(), request.as_slice()].concat());
    let mut output = Vec::new();
    let mut usage = TracedUsagePort(Arc::new(Mutex::new(Vec::new())));
    serve_stdio(
        &mut input,
        &mut output,
        ProductIdentity {
            name: "ctx",
            version: "test",
        },
        backend,
        &render_generic_tool_text,
        &mut usage,
        McpTelemetry::start(false, |_| Ok(())),
    )
    .unwrap();
    serde_json::from_slice(&output).unwrap()
}

fn tool_names(response: &Value) -> Vec<&str> {
    response["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect()
}

#[test]
fn tools_list_appends_private_definitions_without_interpreting_their_fields() {
    let opaque_blame = json!({
        "name": "blame",
        "privateSchema": {"selector": ["opaque", {"nested": true}]},
    });
    let opaque_status = json!({
        "name": "pro_status",
        "privateDescription": "opaque",
    });
    let mut companion_response = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": 11,
        "result": {"tools": [opaque_blame.clone(), opaque_status.clone()]},
    }))
    .unwrap();
    companion_response.push(b'\n');
    let backend = RecordingProxyBackend::new(companion_response);

    let response = tools_list_with_backend(&backend);
    let tools = response["result"]["tools"].as_array().unwrap();
    assert!(tools.contains(&opaque_blame));
    assert!(tools.contains(&opaque_status));
    assert_eq!(
        *backend.request.lock().unwrap(),
        b" {\"jsonrpc\":\"2.0\",\"id\":11,\"method\":\"tools/list\"} \n"
    );
    assert_eq!(
        *backend.completions.lock().unwrap(),
        [OpaqueMcpDeliveryOutcome::WrittenAndFlushed]
    );
}

#[test]
fn tools_list_write_and_flush_failures_complete_as_output_failed_once() {
    let initialized = b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n";
    let request = b"{\"jsonrpc\":\"2.0\",\"id\":11,\"method\":\"tools/list\"}\n";
    let mut companion_response = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": 11,
        "result": {"tools": [{"name": "blame"}]},
    }))
    .unwrap();
    companion_response.push(b'\n');

    for (failure, expected_reason) in [
        (OutputFailure::Write, McpStopReasonV1::StdoutWriteError),
        (OutputFailure::Flush, McpStopReasonV1::StdoutFlushError),
    ] {
        let mut input = Cursor::new([initialized.as_slice(), request.as_slice()].concat());
        let trace = Arc::new(Mutex::new(Vec::new()));
        let backend = RecordingProxyBackend::with_trace(companion_response.clone(), trace.clone());
        let mut output = TracedWriter {
            failure,
            trace,
            output: Vec::new(),
        };
        let mut usage = TracedUsagePort(Arc::new(Mutex::new(Vec::new())));

        let failure = serve_stdio(
            &mut input,
            &mut output,
            ProductIdentity {
                name: "ctx",
                version: "test",
            },
            &backend,
            &render_generic_tool_text,
            &mut usage,
            McpTelemetry::start(false, |_| Ok(())),
        )
        .unwrap_err();

        assert_eq!(failure.reason, expected_reason);
        assert_eq!(
            *backend.completions.lock().unwrap(),
            [OpaqueMcpDeliveryOutcome::OutputFailed]
        );
    }
}

#[test]
fn tools_list_omits_all_private_definitions_when_the_response_fails_closed() {
    let encoded = |tools: Value| {
        let mut response = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 11,
            "result": {"tools": tools},
        }))
        .unwrap();
        response.push(b'\n');
        response
    };
    let invalid_responses = [
        b"not-json\n".to_vec(),
        encoded(json!([{"title": "missing name"}])),
        encoded(json!([{"name": "blame"}, {"name": "blame"}])),
        encoded(json!([{"name": "status"}])),
        encoded(json!([{"name": "unknown-private-tool"}])),
        serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 11,
            "result": {"tools": [{"name": "blame"}]},
        }))
        .unwrap(),
        b"{}\n{}\n".to_vec(),
        vec![b'x'; MCP_MAX_LINE_BYTES + 1],
    ];
    for response in invalid_responses {
        let backend = RecordingProxyBackend::new(response);
        let response = tools_list_with_backend(&backend);
        let names = tool_names(&response);
        assert!(!names.contains(&"blame"), "{names:?}");
        assert!(!names.contains(&"pro_status"), "{names:?}");
        assert!(names.contains(&"status"), "{names:?}");
    }

    for error in [
        OpaqueMcpProxyError::CompanionUnavailable,
        OpaqueMcpProxyError::CompanionIncompatible,
    ] {
        let response = tools_list_with_backend(&FailingProxyBackend(error));
        let names = tool_names(&response);
        assert!(!names.contains(&"blame"), "{names:?}");
        assert!(!names.contains(&"pro_status"), "{names:?}");
        assert!(names.contains(&"status"), "{names:?}");
    }
}

struct FailingProxyBackend(OpaqueMcpProxyError);

impl ToolBackend for FailingProxyBackend {
    fn execute(&self, _operation: ToolOperation) -> Result<ToolOutcome, ToolExecutionError> {
        panic!("companion-owned calls must bypass public tool execution")
    }

    fn proxy_companion_mcp(
        &self,
        _request: &[u8],
    ) -> Result<OpaqueMcpProxyResponse, OpaqueMcpProxyError> {
        Err(self.0)
    }

    fn parse_provider(&self, _value: &str) -> Option<CaptureProvider> {
        None
    }

    fn provider_names(&self) -> Vec<&'static str> {
        Vec::new()
    }
}

#[test]
fn companion_proxy_failures_have_stable_typed_responses() {
    for (error, code, retryable) in [
        (
            OpaqueMcpProxyError::CompanionUnavailable,
            "companion_unavailable",
            true,
        ),
        (
            OpaqueMcpProxyError::CompanionIncompatible,
            "companion_incompatible",
            false,
        ),
    ] {
        let request = b"{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"tools/call\",\"params\":{\"name\":\"pro_status\",\"arguments\":{}}}\n";
        let initialized = b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n";
        let mut input = Cursor::new([initialized.as_slice(), request.as_slice()].concat());
        let mut output = Vec::new();
        let mut usage = TracedUsagePort(Arc::new(Mutex::new(Vec::new())));

        serve_stdio(
            &mut input,
            &mut output,
            ProductIdentity {
                name: "ctx",
                version: "test",
            },
            &FailingProxyBackend(error),
            &render_generic_tool_text,
            &mut usage,
            McpTelemetry::start(false, |_| Ok(())),
        )
        .unwrap();

        let response: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(response["id"], 9);
        assert_eq!(response["result"]["structuredContent"]["error"], code);
        assert_eq!(
            response["result"]["structuredContent"]["retryable"],
            retryable
        );
    }
}
