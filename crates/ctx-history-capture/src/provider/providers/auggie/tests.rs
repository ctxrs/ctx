use ctx_history_core::{EventRole, EventType};
use serde_json::{json, Value};

use super::*;
use crate::{CaptureError, ProviderImportSummary};

const TOOL_OUTPUT_BODY: &str = "AUGGIE_TOOL_OUTPUT_BODY_MUST_NOT_ENTER_CORE";
const UNKNOWN_BODY: &str = "AUGGIE_UNKNOWN_BODY_MUST_NOT_ENTER_CORE_OR_PRO";
const NUMERIC_BODY: &str = "AUGGIE_NUMERIC_BODY_MUST_NOT_ENTER_CORE_OR_PRO";

#[test]
fn parse_error_classification_only_localizes_invalid_payloads() {
    let mut summary = ProviderImportSummary::default();
    native_path::record_auggie_source_parse_error(
        &mut summary,
        7,
        CaptureError::InvalidPayload("deterministic malformed record".to_owned()),
    )
    .unwrap();
    assert_eq!(summary.failed, 1);
    assert_eq!(summary.failures[0].line, 7);

    assert!(matches!(
        native_path::record_auggie_source_parse_error(
            &mut summary,
            8,
            CaptureError::SourceChangedDuringCapture,
        ),
        Err(CaptureError::SourceChangedDuringCapture)
    ));
    assert!(matches!(
        native_path::record_auggie_source_parse_error(
            &mut summary,
            9,
            CaptureError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected source I/O failure",
            )),
        ),
        Err(CaptureError::Io(_))
    ));
    assert!(matches!(
        native_path::record_auggie_source_parse_error(
            &mut summary,
            10,
            CaptureError::SystemInvariant("injected invariant failure"),
        ),
        Err(CaptureError::SystemInvariant("injected invariant failure"))
    ));
    assert_eq!(summary.failed, 1);
}

#[test]
fn certified_node_text_requires_an_exact_native_text_shape() {
    let exact = json!([
        {"text_node": {"content": "legacy text"}},
        {"type": 0, "text_node": {"content": "snake text"}},
        {"type": 0, "textNode": {"content": "camel text"}},
    ]);
    assert_eq!(
        auggie_nodes_text(Some(&exact)),
        Some("legacy text\nsnake text\ncamel text".to_owned())
    );

    for rejected in [
        json!([{"type": "text", "text_node": {"content": "unknown string kind"}}]),
        json!([{"type": 71, "text_node": {"content": NUMERIC_BODY}}]),
        json!([{"type": 0, "content": "generic content"}]),
        json!([{"type": 0, "text_node": {"content": 71}}]),
        json!([{
            "type": 0,
            "text_node": {"content": "apparently text"},
            "output": TOOL_OUTPUT_BODY,
        }]),
    ] {
        assert_eq!(auggie_nodes_text(Some(&rejected)), None);
    }

    assert_eq!(auggie_request_text(&json!({"message": UNKNOWN_BODY})), None);
    assert_eq!(
        auggie_response_text(&json!({"response": UNKNOWN_BODY})),
        None
    );
}

#[test]
fn completed_message_metadata_does_not_invent_a_tool_result() {
    let entry = json!({"completed": true, "source": "agent"});
    let exchange = json!({"request_id": "request-1"});
    let event = auggie_event(AuggieEventInput {
        provider_session_id: "session-1",
        provider_event_index: 0,
        chat_index: 0,
        role: EventRole::Assistant,
        label: "response",
        occurred_at: "2026-07-21T00:00:00Z".parse().unwrap(),
        text: "created commit 0123456789abcdef0123456789abcdef01234567".to_owned(),
        entry: &entry,
        exchange: &exchange,
        raw_source_path: "/tmp/auggie/session.json",
    });

    assert_eq!(event.event_type, EventType::Message);
    assert_eq!(event.payload["result_outcome"], Value::Null);
    assert_eq!(event.payload["result_evidence"], Value::Null);
}
