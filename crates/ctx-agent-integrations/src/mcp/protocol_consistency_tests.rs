use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::tool_backend::{ToolExecutionError, UnifiedToolOperation};

#[derive(Default)]
struct RecordingBackend(AtomicUsize);

impl ToolBackend for RecordingBackend {
    fn execute(&self, _: ToolOperation) -> Result<ToolOutcome, ToolExecutionError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(ToolOutcome::plain(json!({"accepted": true})))
    }

    fn execute_unified(&self, _: UnifiedToolOperation) -> Result<ToolOutcome, ToolExecutionError> {
        self.execute(ToolOperation::Status)
    }

    fn parse_provider(&self, _: &str) -> Option<ctx_history_core::CaptureProvider> {
        None
    }

    fn provider_names(&self) -> Vec<&'static str> {
        Vec::new()
    }
}

fn request(backend: &RecordingBackend, method: &str, params: Value) -> Value {
    let message = json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params});
    let handled = handle_protocol_message(
        message.clone(),
        RequestDescriptor::from_message(&message),
        &mut true,
        McpServerIdentity {
            name: "ctx",
            version: "test",
        },
        backend,
        Value::to_string,
    );
    if params["name"] == "graph_stats" {
        assert!(
            handled.usage.is_none(),
            "graph calls have no history usage authority"
        );
    }
    let encoded = encode_response_line(&handled.value.unwrap()).unwrap();
    let response: Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 7);
    response
}

#[test]
fn history_and_graph_share_argument_envelopes_and_keep_semantic_errors() {
    let backend = RecordingBackend::default();
    for name in ["status", "graph_stats"] {
        for arguments in [json!([]), Value::Null, json!("invalid"), json!(42)] {
            let response = request(
                &backend,
                "tools/call",
                json!({"name": name, "arguments": arguments}),
            );
            assert_eq!(response["error"]["code"], -32602, "{response}");
            assert_eq!(response["error"]["message"], "Invalid params");
            assert_eq!(
                response["error"]["data"]["error"],
                "tools/call params.arguments must be an object"
            );
            assert!(response.get("result").is_none());
        }
        let response = request(
            &backend,
            "tools/call",
            json!({"name": name, "arguments": {"unknown": true}}),
        );
        assert!(response.get("error").is_none());
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(
            response["result"]["structuredContent"]["error_code"],
            "invalid_request"
        );
    }
    assert_eq!(backend.0.load(Ordering::Relaxed), 0);
    for name in ["status", "graph_stats"] {
        for params in [
            json!({"name": name, "arguments": {}}),
            json!({"name": name}),
        ] {
            let response = request(&backend, "tools/call", params);
            assert!(response.get("error").is_none());
            assert_eq!(response["result"]["structuredContent"]["accepted"], true);
        }
    }
    assert_eq!(backend.0.load(Ordering::Relaxed), 4);
    assert_eq!(request(&backend, "ping", json!({}))["result"], json!({}));
}

#[test]
fn show_event_discovery_and_validation_agree_at_window_boundary() {
    let backend = RecordingBackend::default();
    let response = request(&backend, "tools/list", json!({}));
    let schema = &response["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "show_event")
        .unwrap()["inputSchema"];
    for key in ["before", "after", "window"] {
        assert_eq!(schema["properties"][key]["minimum"], 0);
        assert_eq!(schema["properties"][key]["maximum"], 50);
        for value in [0, 50, 51] {
            let mut arguments = json!({"ctx_event_id": "11111111-2222-4333-8444-555555555555"});
            arguments[key] = json!(value);
            let response = request(
                &backend,
                "tools/call",
                json!({"name": "show_event", "arguments": arguments}),
            );
            assert!(response.get("error").is_none());
            if value == 51 {
                assert_eq!(response["result"]["isError"], true);
                assert_eq!(
                    response["result"]["structuredContent"]["error_code"],
                    "invalid_request"
                );
                assert_eq!(
                    response["result"]["structuredContent"]["error"],
                    "show_event before/after/window must be 50 or less"
                );
            } else {
                assert_eq!(response["result"]["structuredContent"]["accepted"], true);
            }
        }
    }
    assert_eq!(backend.0.load(Ordering::Relaxed), 6);
}
