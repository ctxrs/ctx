use super::*;
use ctx_agent_integrations::tool_backend::{
    ToolBackend, ToolBackendError, ToolExecutionError, ToolOperation, ToolOutcome, UnifiedToolKind,
    UnifiedToolOperation,
};
use ctx_client_observability::analytics as wire;
use std::{
    io::{self, Cursor, Write},
    sync::{Arc, Mutex},
};

#[derive(Debug)]
enum RecordedEvent {
    GraphCompleted(Box<wire::GraphCompletedV1>),
    SiftSummary(Box<wire::SiftSummaryV1>),
    RemoteCompleted(Box<wire::RemoteCompletedV1>),
    History,
}
fn record_batch(events: &mut Vec<RecordedEvent>, batch: &[wire::PublicEventV1]) {
    for event in batch {
        let event = match event {
            wire::PublicEventV1::GraphCompleted(e) => RecordedEvent::GraphCompleted(Box::new(*e)),
            wire::PublicEventV1::SiftSummary(e) => RecordedEvent::SiftSummary(Box::new(*e)),
            wire::PublicEventV1::RemoteCompleted(e) => RecordedEvent::RemoteCompleted(Box::new(*e)),
            wire::PublicEventV1::OperationCompleted(_) => RecordedEvent::History,
            _ => continue,
        };
        events.push(event);
    }
}

fn graph_usage(failed: bool) -> ToolUsageFacts {
    let execution = if failed {
        Err(wire::ProductFailure {
            stage: wire::ProductFailureStage::Execute,
            class: wire::ProductFailureClass::NotFound,
        })
    } else {
        Ok(())
    };
    ToolUsageFacts {
        graph: Some(wire::GraphCompletedV1::new(
            wire::GraphOperation::Show,
            wire::GraphSurface::Mcp,
            wire::ProductCompletion::new(
                Duration::ZERO,
                execution,
                wire::DeliveryEvidence::Unknown,
                wire::ProductOutput::Mcp,
            ),
        )),
        ..Default::default()
    }
}

#[test]
fn encode_failure_removes_sift_savings_without_failing_successful_work() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let collected = events.clone();
    let mut telemetry = McpTelemetry::start(true, move |batch| {
        record_batch(&mut collected.lock().unwrap(), batch);
        Ok(())
    });
    let mut sift = wire::SiftSummaryV1::new(
        wire::SiftOperation::Compact,
        wire::SiftMode::Lossless,
        Duration::ZERO,
    );
    sift.observed = 1;
    sift.complete = 1;
    sift.bytes = Some(wire::PresentedTotals {
        samples: 1,
        input: 100,
        output: 10,
    });
    sift.tokens = Some(wire::PresentedTotals {
        samples: 1,
        input: 20,
        output: 3,
    });
    sift.cohort.entry = Some(wire::SiftEntry::Mcp);
    let usage = ToolUsageFacts {
        sift: Some(sift),
        ..Default::default()
    };
    telemetry.record_response_failure(
        RequestDescriptor::ToolCall {
            operation: McpToolKind::Unified(UnifiedToolKind::OutputCompact),
        },
        Duration::from_millis(3),
        McpErrorClassV1::ResponseSerialize,
        Some(&usage),
    );
    telemetry.stop(
        McpStopReasonV1::ResponseSerializeError,
        Outcome::Failure,
        Duration::ZERO,
    );
    let events = events.lock().unwrap();
    let RecordedEvent::SiftSummary(sample) = events
        .iter()
        .find(|e| matches!(e, RecordedEvent::SiftSummary(_)))
        .unwrap()
    else {
        unreachable!()
    };
    assert_eq!(sample.execution_failed, 0);
    assert_eq!(sample.output_failed, 1);
    assert_eq!(sample.bytes, None);
    assert_eq!(sample.tokens, None);
    assert_eq!(sample.partial, 1);
    assert!(events.iter().all(|e| !matches!(e, RecordedEvent::History)));
}

#[test]
fn remote_errors_and_rejected_remote_requests_are_never_local_history_events() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let collected = events.clone();
    let mut telemetry = McpTelemetry::start(true, move |batch| {
        record_batch(&mut collected.lock().unwrap(), batch);
        Ok(())
    })
    .for_remote();
    let descriptor = RequestDescriptor::ToolCall {
        operation: McpToolKind::Search,
    };
    telemetry.record_delivered(
        descriptor,
        Some(&serde_json::json!({"error":{"code":-32602}})),
        None,
        Duration::ZERO,
    );
    let failure = wire::ProductFailure {
        stage: wire::ProductFailureStage::Execute,
        class: wire::ProductFailureClass::Forbidden,
    };
    let usage = ToolUsageFacts {
        remote: Some(wire::RemoteCompletedV1::new(
            wire::RemoteOperation::Search,
            wire::RemoteSurface::Mcp,
            wire::ProductCompletion::new(
                Duration::ZERO,
                Err(failure),
                wire::DeliveryEvidence::Unknown,
                wire::ProductOutput::Mcp,
            ),
        )),
        ..Default::default()
    };
    telemetry.record_delivered(
        descriptor,
        Some(&serde_json::json!({"result":{"isError":true}})),
        Some(&usage),
        Duration::ZERO,
    );
    telemetry.stop(McpStopReasonV1::Eof, Outcome::Success, Duration::ZERO);
    let events = events.lock().unwrap();
    assert!(events.iter().all(|e| !matches!(e, RecordedEvent::History)));
    let terminals: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let RecordedEvent::RemoteCompleted(remote) = e {
                Some(remote)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(terminals.len(), 1);
    assert_eq!(terminals[0].completion.execution, Err(failure));
    assert_eq!(
        terminals[0].completion.delivery,
        wire::DeliveryEvidence::KnownComplete
    );
}

#[test]
fn flushing_a_replacement_envelope_does_not_claim_delivery_of_the_engine_result() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let collected = events.clone();
    let mut telemetry = McpTelemetry::start(true, move |batch| {
        record_batch(&mut collected.lock().unwrap(), batch);
        Ok(())
    });
    let mut usage = graph_usage(false);
    usage.graph.as_mut().unwrap().completion.delivery = wire::DeliveryEvidence::Failed;
    telemetry.record_delivered(
        RequestDescriptor::ToolCall {
            operation: McpToolKind::Unified(UnifiedToolKind::GraphShow),
        },
        Some(&serde_json::json!({"result":{"isError":true}})),
        Some(&usage),
        Duration::ZERO,
    );
    telemetry.stop(McpStopReasonV1::Eof, Outcome::Success, Duration::ZERO);
    let events = events.lock().unwrap();
    let graph = events
        .iter()
        .find_map(|e| {
            if let RecordedEvent::GraphCompleted(g) = e {
                Some(g)
            } else {
                None
            }
        })
        .unwrap();
    assert!(graph.completion.execution.is_ok());
    assert_eq!(graph.completion.delivery, wire::DeliveryEvidence::Failed);
}

struct Backend {
    usage: ToolUsageFacts,
    failed: bool,
}
impl ToolBackend for Backend {
    fn execute(&self, _: ToolOperation) -> Result<ToolOutcome, ToolExecutionError> {
        panic!("graph must not route to history")
    }
    fn execute_unified(&self, _: UnifiedToolOperation) -> Result<ToolOutcome, ToolExecutionError> {
        if self.failed {
            Err(ToolExecutionError {
                error: Box::new(ToolBackendError::invalid_request("synthetic missing node")),
                usage: Box::new(self.usage),
            })
        } else {
            let mut outcome = ToolOutcome::plain(serde_json::json!({"graph": {"nodes": []}}));
            outcome.usage = self.usage;
            Ok(outcome)
        }
    }
    fn parse_provider(&self, _: &str) -> Option<ctx_history_core::CaptureProvider> {
        None
    }
    fn provider_names(&self) -> Vec<&'static str> {
        vec![]
    }
}
struct Usage;
impl crate::mcp::McpUsagePort for Usage {
    fn record_delivered(
        &mut self,
        _: McpToolKind,
        _: ToolUsageFacts,
        _: &Value,
        _: usize,
        _: Duration,
    ) {
    }
}
struct Writer {
    failure: u8,
    boundary_seen: Arc<Mutex<bool>>,
}
impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.failure == 1 {
            *self.boundary_seen.lock().unwrap() = true;
            Err(io::ErrorKind::BrokenPipe.into())
        } else {
            Ok(bytes.len())
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        *self.boundary_seen.lock().unwrap() = true;
        if self.failure == 2 {
            Err(io::ErrorKind::BrokenPipe.into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn actual_stdio_finalizer_preserves_graph_work_on_write_flush_and_error_envelopes() {
    for work_failed in [false, true] {
        for failure in 0..=2 {
            let events = Arc::new(Mutex::new(Vec::new()));
            let collected = events.clone();
            let boundary_seen = Arc::new(Mutex::new(false));
            let boundary = boundary_seen.clone();
            let telemetry = McpTelemetry::start(true, move |batch| {
                if batch
                    .iter()
                    .any(|e| matches!(e, wire::PublicEventV1::GraphCompleted(_)))
                {
                    assert!(*boundary.lock().unwrap());
                }
                record_batch(&mut collected.lock().unwrap(), batch);
                Ok(())
            });
            let mut input = Cursor::new(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"graph_show\",\"arguments\":{\"symbol\":\"synthetic\"}}}\n");
            let result = crate::mcp::serve_stdio(
                &mut input,
                &mut Writer {
                    failure,
                    boundary_seen,
                },
                crate::mcp::ProductIdentity {
                    name: "test",
                    version: "test",
                },
                &Backend {
                    usage: graph_usage(work_failed),
                    failed: work_failed,
                },
                &crate::mcp::render_generic_tool_text,
                &mut Usage,
                telemetry,
            );
            assert_eq!(result.is_err(), failure != 0);
            let events = events.lock().unwrap();
            let terminals: Vec<_> = events
                .iter()
                .filter_map(|e| {
                    if let RecordedEvent::GraphCompleted(graph) = e {
                        Some(graph)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(terminals.len(), 1);
            assert_eq!(terminals[0].completion.execution.is_err(), work_failed);
            assert_eq!(
                terminals[0].completion.delivery,
                if failure == 0 {
                    wire::DeliveryEvidence::KnownComplete
                } else {
                    wire::DeliveryEvidence::Failed
                }
            );
            assert!(events.iter().all(|e| !matches!(e, RecordedEvent::History)));
        }
    }
}
