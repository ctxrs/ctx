use std::{
    fs,
    io::{Cursor, Write},
    sync::{Condvar, Mutex},
};

use serde_json::{json, Map};
use tempfile::TempDir;

use super::*;
use crate::analytics::{
    pro_operation_event, OperationPayloadV1, ProHostOperationV1, ProStatusTelemetryV1,
    ProSurfaceV1, RuntimeObservationKindV1,
};

fn private_tempdir() -> TempDir {
    let root = tempfile::tempdir().unwrap();
    ctx_history_core::platform_security::restrict_private_directory(root.path()).unwrap();
    root
}

fn test_event() -> PublicEventV1 {
    operation_event(
        McpOperationV1::tool_call(McpToolV1::Status),
        Outcome::Success,
        Duration::ZERO,
    )
}

fn test_pro_event() -> PublicEventV1 {
    pro_operation_event(
        ProHostOperationV1::Status(ProStatusTelemetryV1::new(ProSurfaceV1::Mcp)),
        Outcome::Success,
        Duration::ZERO,
    )
}

struct TraceWriter {
    trace: Arc<Mutex<Vec<&'static str>>>,
}

impl Write for TraceWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.trace.lock().unwrap().push("response_write");
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.trace.lock().unwrap().push("response_flush");
        Ok(())
    }
}

struct FailingFlushWriter;

impl Write for FailingFlushWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::other("test flush failure"))
    }
}

#[test]
fn housekeeping_is_coalesced_into_lifecycle_counts() {
    let mut lifecycle = McpLifecycle::new();
    let success = json!({"jsonrpc": "2.0", "id": 1, "result": {}});
    let initialized = lifecycle
        .record_delivered(
            RequestDescriptor::Initialize,
            Some(&success),
            Duration::ZERO,
        )
        .unwrap();
    assert!(matches!(initialized, PublicEventV1::RuntimeObservation(_)));
    assert!(lifecycle
        .record_delivered(RequestDescriptor::Ping, Some(&success), Duration::ZERO)
        .is_none());
    assert!(lifecycle
        .record_delivered(RequestDescriptor::ToolsList, Some(&success), Duration::ZERO)
        .is_none());
    assert!(lifecycle
        .record_delivered(
            RequestDescriptor::InitializedNotification,
            None,
            Duration::ZERO,
        )
        .is_none());
    assert!(lifecycle
        .record_delivered(RequestDescriptor::UnknownNotification, None, Duration::ZERO,)
        .is_none());

    let stopped = RuntimeObservationV1::mcp(
        McpRuntimeObservationV1::stopped(
            lifecycle.initialized,
            McpStopReasonV1::Eof,
            lifecycle.counts,
        ),
        Outcome::Success,
        Duration::ZERO,
    );
    let RuntimeObservationKindV1::Mcp(observation) = stopped.kind else {
        panic!("expected MCP lifecycle observation");
    };
    let mut properties = Map::new();
    observation.insert_properties(&mut properties);
    assert_eq!(properties["ping_count_bucket"], "1");
    assert_eq!(properties["tools_list_count_bucket"], "1");
    assert_eq!(properties["initialized_notification_count_bucket"], "1");
    assert_eq!(properties["unknown_notification_count_bucket"], "1");
}

#[test]
fn malformed_and_tool_requests_get_typed_terminal_events() {
    let mut lifecycle = McpLifecycle::new();
    let malformed = json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": {"code": -32700, "message": "sensitive parser output"}
    });
    let event = lifecycle
        .record_delivered(
            RequestDescriptor::InvalidJson,
            Some(&malformed),
            Duration::ZERO,
        )
        .unwrap();
    let PublicEventV1::OperationCompleted(event) = event else {
        panic!("expected operation event");
    };
    let mut properties = Map::new();
    let crate::analytics::OperationPayloadV1::Mcp(operation) = event.payload else {
        panic!("expected MCP operation");
    };
    operation.insert_properties(&mut properties);
    assert_eq!(properties["error_class"], "invalid_json");
    assert!(!serde_json::to_string(&properties)
        .unwrap()
        .contains("sensitive parser output"));

    let invalid_without_id = lifecycle
        .record_delivered(
            RequestDescriptor::UnknownNotification,
            Some(&json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": {"code": -32600, "message": "Invalid Request"}
            })),
            Duration::ZERO,
        )
        .unwrap();
    assert!(matches!(
        invalid_without_id,
        PublicEventV1::OperationCompleted(_)
    ));

    let response = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {"structuredContent": {"results": []}}
    });
    let event = lifecycle
        .record_delivered(
            RequestDescriptor::ToolCall {
                tool: McpToolV1::Search,
            },
            Some(&response),
            Duration::ZERO,
        )
        .unwrap();
    assert!(matches!(event, PublicEventV1::OperationCompleted(_)));
}

#[test]
fn pro_tools_do_not_derive_product_result_dimensions() {
    let response = json!({
        "result": {
            "structuredContent": {
                "results": [1, 2, 3],
                "pagination": {"truncated": true}
            }
        }
    });
    for tool in [McpToolV1::Blame, McpToolV1::ProStatus] {
        assert_eq!(
            result_metadata(tool, &response),
            McpResultMetadataV1::default()
        );
    }
}

#[test]
fn response_flush_precedes_one_local_blame_increment_and_remote_submissions() {
    let temp = private_tempdir();
    fs::write(
        temp.path().join("config.toml"),
        "analytics.enabled = true\n",
    )
    .unwrap();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let observed_trace = Arc::clone(&trace);
    let mut sender =
        AsyncMcpSender::start_with(temp.path().to_path_buf(), 4, Arc::new(|_, _, _| Ok(())));
    sender.submit_observer = Some(Arc::new(move |event| {
        let label = match event {
            PublicEventV1::OperationCompleted(event) => match &event.payload {
                OperationPayloadV1::Mcp(_) => "submit_mcp",
                OperationPayloadV1::ProHost(_) => "submit_pro",
                _ => "submit_other",
            },
            _ => "submit_other",
        };
        observed_trace.lock().unwrap().push(label);
    }));
    let mut telemetry = McpTelemetry {
        state: McpTelemetryState::Enabled {
            sender,
            lifecycle: McpLifecycle::new(),
        },
    };
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "blame",
            "arguments": {
                "target": {"kind": "commit", "oid": "abc1234"}
            }
        }
    });
    let mut stdin = Cursor::new(format!("{request}\n").into_bytes());
    let mut stdout = TraceWriter {
        trace: Arc::clone(&trace),
    };
    let mut initialized = true;
    let mut usage_recorder = crate::local_usage::McpUsageRecorder::start(temp.path().to_path_buf());
    usage_recorder.set_test_trace(Arc::clone(&trace));

    let result = super::super::serve_stdio_loop(
        temp.path(),
        &mut stdin,
        &mut stdout,
        &mut initialized,
        &mut telemetry,
        &mut usage_recorder,
    );
    assert!(result.is_ok());
    telemetry.stop(McpStopReasonV1::Eof, Outcome::Success, Duration::ZERO);

    let trace = trace.lock().unwrap();
    let position = |label| trace.iter().position(|entry| *entry == label).unwrap();
    assert!(position("response_write") < position("response_flush"));
    assert!(position("response_flush") < position("local_usage"));
    assert!(position("local_usage") < position("submit_mcp"));
    assert!(position("local_usage") < position("submit_pro"));
    assert!(position("response_flush") < position("submit_mcp"));
    assert!(position("response_flush") < position("submit_pro"));
    assert_eq!(
        trace
            .iter()
            .filter(|entry| **entry == "local_usage")
            .count(),
        1
    );
    drop(trace);

    let report = crate::local_usage::read_report(temp.path(), true, true);
    let definitions = report.definitions.unwrap();
    let current = definitions
        .iter()
        .find(|definition| definition.definition_version == crate::local_usage::DEFINITION_VERSION)
        .unwrap();
    let summary = &current.summary;
    assert_eq!(summary.calls, 1);
    assert_eq!(summary.pro_blame.requests, 1);
    let detail = &current.by_operation[0];
    assert_eq!(detail.surface, "mcp");
    assert_eq!(detail.operation, "blame");
    assert_eq!(detail.calls, 1);
}

#[test]
fn failed_response_flush_does_not_record_local_usage() {
    let temp = private_tempdir();
    fs::write(
        temp.path().join("config.toml"),
        "analytics.enabled = false\n",
    )
    .unwrap();
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "status", "arguments": {}}
    });
    let mut stdin = Cursor::new(format!("{request}\n").into_bytes());
    let mut stdout = FailingFlushWriter;
    let mut initialized = true;
    let mut telemetry = McpTelemetry::start(temp.path().to_path_buf());
    let mut usage_recorder = crate::local_usage::McpUsageRecorder::start(temp.path().to_path_buf());

    let failure = super::super::serve_stdio_loop(
        temp.path(),
        &mut stdin,
        &mut stdout,
        &mut initialized,
        &mut telemetry,
        &mut usage_recorder,
    )
    .unwrap_err();
    assert_eq!(failure.reason, McpStopReasonV1::StdoutFlushError);
    assert!(!temp.path().join("usage.sqlite").exists());
}

#[test]
fn local_usage_descriptor_is_created_only_after_protocol_and_dispatch_validation() {
    let temp = private_tempdir();
    let excluded = [
        json!([]),
        json!({"jsonrpc": "1.0", "id": 1, "method": "tools/call", "params": {"name": "status"}}),
        json!({"jsonrpc": "2.0", "id": true, "method": "tools/call", "params": {"name": "status"}}),
        json!({"jsonrpc": "2.0", "method": "tools/call", "params": {"name": "status"}}),
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "unknown"}}),
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {}}),
    ];
    for message in excluded {
        let mut initialized = true;
        let (_, invocation) =
            super::super::handle_message(message.clone(), temp.path(), &mut initialized);
        assert!(invocation.is_none(), "{message}");
    }

    let pre_init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "status", "arguments": {}}
    });
    let (_, invocation) = super::super::handle_message(pre_init, temp.path(), &mut false);
    assert!(invocation.is_none());

    let recognized_argument_error = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": "search", "arguments": {"unknown": "content"}}
    });
    let (_, invocation) =
        super::super::handle_message(recognized_argument_error, temp.path(), &mut true);
    assert!(invocation.is_some());

    let invalid_target = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "blame",
            "arguments": {"target": {"kind": "commit", "oid": ""}}
        }
    });
    let (_, invocation) = super::super::handle_message(invalid_target, temp.path(), &mut true);
    assert_eq!(
        invocation.unwrap().target_type_for_test(),
        crate::local_usage::TargetType::NotApplicable
    );

    let valid_target = json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "blame",
            "arguments": {"target": {"kind": "commit", "oid": "abc1234"}}
        }
    });
    let (_, invocation) = super::super::handle_message(valid_target, temp.path(), &mut true);
    assert_eq!(
        invocation.unwrap().target_type_for_test(),
        crate::local_usage::TargetType::Commit
    );
}

#[test]
fn disabled_start_creates_no_sender_thread_and_stays_noop() {
    let temp = private_tempdir();
    let config_path = temp.path().join("config.toml");
    fs::write(&config_path, "analytics.enabled = false\n").unwrap();
    let mut telemetry = McpTelemetry::start(temp.path().to_path_buf());
    assert!(matches!(&telemetry.state, McpTelemetryState::Disabled));

    fs::write(&config_path, "analytics.enabled = true\n").unwrap();
    telemetry.record_delivered(RequestDescriptor::Ping, None, Duration::ZERO);
    assert!(matches!(&telemetry.state, McpTelemetryState::Disabled));
    telemetry.stop(McpStopReasonV1::Eof, Outcome::Success, Duration::ZERO);
}

#[test]
fn enabled_start_honors_later_dynamic_opt_out() {
    let temp = private_tempdir();
    let config_path = temp.path().join("config.toml");
    fs::write(&config_path, "analytics.enabled = true\n").unwrap();
    let calls = Arc::new(AtomicU64::new(0));
    let observed = Arc::clone(&calls);
    let sender = AsyncMcpSender::start_with(
        temp.path().to_path_buf(),
        2,
        Arc::new(move |_, _, _| {
            observed.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }),
    );
    let telemetry = McpTelemetry {
        state: McpTelemetryState::Enabled {
            sender,
            lifecycle: McpLifecycle::new(),
        },
    };
    fs::write(&config_path, "analytics.enabled = false\n").unwrap();
    telemetry.submit_pro_event(test_pro_event());
    telemetry.stop(McpStopReasonV1::Eof, Outcome::Success, Duration::ZERO);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[test]
fn mixed_mcp_and_pro_queue_pressure_is_bounded_and_counted() {
    let temp = private_tempdir();
    let gate = Arc::new((Mutex::new((false, false)), Condvar::new()));
    let dispatch_gate = Arc::clone(&gate);
    let sender = AsyncMcpSender::start_with(
        temp.path().to_path_buf(),
        1,
        Arc::new(move |_, _, _| {
            let (lock, wake) = &*dispatch_gate;
            let mut state = lock.lock().unwrap();
            state.0 = true;
            wake.notify_all();
            while !state.1 {
                state = wake.wait(state).unwrap();
            }
            Ok(())
        }),
    );
    sender.try_submit(test_event());
    {
        let (lock, wake) = &*gate;
        let mut state = lock.lock().unwrap();
        while !state.0 {
            state = wake.wait(state).unwrap();
        }
    }
    let mut telemetry = McpTelemetry {
        state: McpTelemetryState::Enabled {
            sender,
            lifecycle: McpLifecycle::new(),
        },
    };
    telemetry.record_delivered(
        RequestDescriptor::ToolCall {
            tool: McpToolV1::Status,
        },
        Some(&json!({"result": {"structuredContent": {}}})),
        Duration::ZERO,
    );
    telemetry.submit_pro_event(test_pro_event());
    let McpTelemetryState::Enabled { sender, .. } = &telemetry.state else {
        panic!("telemetry should be enabled");
    };
    assert_eq!(sender.dropped_count(), 1);
    {
        let (lock, wake) = &*gate;
        lock.lock().unwrap().1 = true;
        wake.notify_all();
    }
    telemetry.stop(McpStopReasonV1::Eof, Outcome::Success, Duration::ZERO);
}

#[test]
fn dispatch_failure_is_best_effort() {
    let temp = private_tempdir();
    let calls = Arc::new(AtomicU64::new(0));
    let observed = Arc::clone(&calls);
    let mut sender = AsyncMcpSender::start_with(
        temp.path().to_path_buf(),
        2,
        Arc::new(move |_, _, _| {
            observed.fetch_add(1, Ordering::Relaxed);
            Err(())
        }),
    );
    sender.try_submit(test_event());
    sender.shutdown(Duration::from_secs(1));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(sender.dropped_count(), 0);
}
