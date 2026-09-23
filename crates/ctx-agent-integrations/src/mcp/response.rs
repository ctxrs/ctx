use serde_json::{json, Value};

use super::compact_json;
use crate::tool_backend::{CursorFailureKind, ToolBackendError};

pub(super) fn invalid_tool_request(message: impl Into<String>) -> ToolBackendError {
    ToolBackendError::invalid_request(message)
}

pub(super) fn tool_result_with_text(structured: Value, text: String) -> Value {
    json!({
        "content": [
            {
                "type": "text",
                "text": text,
            }
        ],
        "structuredContent": structured,
    })
}

pub(super) fn tool_error_result(error: ToolBackendError) -> Value {
    let text = error.to_string();
    let structured = match error {
        ToolBackendError::Unified { code, detail } => json!({
            "error": detail,
            "error_code": code.as_str(),
        }),
        ToolBackendError::InvalidRequest { detail } => json!({
            "error": detail,
            "error_code": "invalid_request",
        }),
        ToolBackendError::Blame(error) | ToolBackendError::EventQuery(error) => error.structured,
        ToolBackendError::SourceUnavailable => json!({
            "error": "source_unavailable",
            "error_code": "source_unavailable",
        }),
        ToolBackendError::GenerationAuthority(error) => error.structured,
        ToolBackendError::GenerationChanged => json!({
            "error": "generation_changed/active_generation_race",
            "error_code": "generation_changed",
            "failure_kind": "active_generation_race",
            "detail": "the active searchable generation changed while the command was opening it",
            "retryable": true,
        }),
        ToolBackendError::Cursor { kind, detail } => json!({
            "error": detail.clone(),
            "error_code": match kind {
                CursorFailureKind::Stale => "cursor_stale",
                CursorFailureKind::Mismatch => "cursor_mismatch",
                CursorFailureKind::Invalid => "invalid_cursor",
            },
            "detail": detail,
            "retryable": false,
        }),
        ToolBackendError::OutputLimit {
            event_id,
            actual_bytes,
            maximum_bytes,
        } => json!({
            "error": "output_limit_exceeded",
            "error_code": "output_limit_exceeded",
            "ctx_event_id": event_id,
            "actual_bytes": actual_bytes,
            "maximum_bytes": maximum_bytes,
            "retryable": false,
            "remediation": "reduce the event window or choose a narrower transcript mode",
        }),
        ToolBackendError::SemanticNotReady {
            code,
            detail,
            retryable,
        } => json!({
            "error": text.clone(),
            "error_code": code,
            "detail": detail,
            "retryable": retryable,
        }),
        ToolBackendError::Internal { detail } => json!({ "error": detail }),
    };
    json!({
        "isError": true,
        "content": [
            {
                "type": "text",
                "text": text,
            }
        ],
        "structuredContent": structured,
    })
}

pub(super) fn success_response(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

pub fn error_response(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let error = compact_json(json!({
        "code": code,
        "message": message,
        "data": data,
    }));
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": error,
    })
}

pub(super) fn invalid_request_response(id: Option<&Value>) -> Value {
    let id = match id {
        Some(id @ (Value::String(_) | Value::Number(_))) => id.clone(),
        None | Some(Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_)) => {
            Value::Null
        }
    };
    error_response(id, -32600, "Invalid Request", None)
}

pub(super) fn json_rpc_error(code: i64, message: &str, data: Option<Value>) -> Value {
    compact_json(json!({
        "code": code,
        "message": message,
        "data": data,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        error_response, invalid_request_response, invalid_tool_request, tool_error_result,
    };
    use crate::tool_backend::{CursorFailureKind, ToolBackendError};

    #[test]
    fn generation_change_is_a_retryable_typed_tool_error() {
        let result = tool_error_result(ToolBackendError::GenerationChanged);

        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"]["error"],
            "generation_changed/active_generation_race"
        );
        assert_eq!(
            result["structuredContent"]["error_code"],
            "generation_changed"
        );
        assert_eq!(
            result["structuredContent"]["failure_kind"],
            "active_generation_race"
        );
        assert_eq!(result["structuredContent"]["retryable"], true);
    }

    #[test]
    fn error_response_preserves_required_null_id_while_pruning_optional_data() {
        let response = error_response(serde_json::Value::Null, -32700, "Parse error", None);

        assert!(response.as_object().unwrap().contains_key("id"));
        assert!(response["id"].is_null());
        assert_eq!(response["error"]["code"], -32700);
        assert!(!response["error"].as_object().unwrap().contains_key("data"));
    }

    #[test]
    fn error_response_preserves_string_and_numeric_ids_exactly() {
        let string_id = invalid_request_response(Some(&json!("request-7")));
        let numeric_id = invalid_request_response(Some(&json!(7)));

        assert_eq!(string_id["id"], "request-7");
        assert_eq!(numeric_id["id"], 7);
    }

    #[test]
    fn invalid_request_response_uses_null_for_unknown_or_invalid_ids() {
        let unknown = invalid_request_response(None);
        assert!(unknown.as_object().unwrap().contains_key("id"));
        assert!(unknown["id"].is_null());

        for id in [json!(null), json!(true), json!([])] {
            let response = invalid_request_response(Some(&id));
            assert!(response.as_object().unwrap().contains_key("id"));
            assert!(response["id"].is_null());
        }
    }

    #[test]
    fn invalid_tool_request_preserves_detail_and_adds_stable_error_code() {
        let result = tool_error_result(invalid_tool_request("limit must be an integer"));

        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"]["error"],
            "limit must be an integer"
        );
        assert_eq!(result["structuredContent"]["error_code"], "invalid_request");
        assert_eq!(result["content"][0]["text"], "limit must be an integer");
    }

    #[test]
    fn presentation_output_limit_has_stable_structured_content() {
        let event_id = Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
        let result = tool_error_result(ToolBackendError::OutputLimit {
            event_id,
            actual_bytes: 2048,
            maximum_bytes: 1024,
        });

        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"],
            json!({
                "error": "output_limit_exceeded",
                "error_code": "output_limit_exceeded",
                "ctx_event_id": event_id,
                "actual_bytes": 2048,
                "maximum_bytes": 1024,
                "retryable": false,
                "remediation": "reduce the event window or choose a narrower transcript mode",
            })
        );
        assert_eq!(
            result["structuredContent"]["error_code"],
            "output_limit_exceeded"
        );
        assert_eq!(result["structuredContent"]["retryable"], false);
        assert_eq!(
            result["content"][0]["text"],
            format!(
                "Core content output for ctx event {event_id} requires 2048 bytes; the presentation limit is 1024 bytes"
            )
        );
    }

    #[test]
    fn session_cursor_failures_have_stable_typed_tool_errors() {
        let cases = [
            (
                "cursor generation mismatch",
                CursorFailureKind::Stale,
                "cursor_stale",
            ),
            (
                "cursor session mismatch",
                CursorFailureKind::Mismatch,
                "cursor_mismatch",
            ),
            (
                "invalid cursor coordinate",
                CursorFailureKind::Invalid,
                "invalid_cursor",
            ),
        ];

        for (message, kind, code) in cases {
            let result = tool_error_result(ToolBackendError::Cursor {
                kind,
                detail: message.to_owned(),
            });
            assert_eq!(result["isError"], true);
            assert_eq!(result["structuredContent"]["error_code"], code);
            assert_eq!(result["structuredContent"]["retryable"], false);
            assert_eq!(result["structuredContent"]["detail"], message);
            assert_eq!(result["content"][0]["text"], message);
        }
    }
}
