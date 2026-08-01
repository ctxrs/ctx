use std::fmt;

use anyhow::Error;
use serde_json::{json, Value};

use super::{compact_json, render_tool_text};

#[derive(Debug)]
pub(super) struct InvalidToolRequest {
    message: String,
}

impl fmt::Display for InvalidToolRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for InvalidToolRequest {}

pub(super) fn invalid_tool_request(message: impl Into<String>) -> Error {
    Error::new(InvalidToolRequest {
        message: message.into(),
    })
}

pub(super) fn tool_result(structured: Value) -> Value {
    let text = render_tool_text(&structured);
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

pub(super) fn tool_error_result(err: Error) -> Value {
    if crate::commands::source_index::is_active_generation_race(&err) {
        let structured = crate::commands::source_index::active_generation_race_error_json();
        return json!({
            "isError": true,
            "content": [
                {
                    "type": "text",
                    "text": "History changed while ctx was opening the searchable generation. Retry the same request.",
                }
            ],
            "structuredContent": structured,
        });
    }
    if let Some(error) =
        err.downcast_ref::<crate::presentation_limit::PresentationOutputLimitError>()
    {
        let structured = crate::presentation_limit::presentation_output_limit_error_json(error);
        return json!({
            "isError": true,
            "content": [
                {
                    "type": "text",
                    "text": error.to_string(),
                }
            ],
            "structuredContent": structured,
        });
    }
    if let Some(error) = err.downcast_ref::<crate::semantic::SourceBackedSemanticNotReady>() {
        let structured = error.structured();
        return json!({
            "isError": true,
            "content": [
                {
                    "type": "text",
                    "text": error.to_string(),
                }
            ],
            "structuredContent": structured,
        });
    }
    if let Some(error) = err.downcast_ref::<InvalidToolRequest>() {
        let message = error.to_string();
        return json!({
            "isError": true,
            "content": [
                {
                    "type": "text",
                    "text": message.clone(),
                }
            ],
            "structuredContent": {
                "error": message,
                "error_code": "invalid_request",
            }
        });
    }
    if let Some(error_code) = crate::pro::stable_error_code(&err) {
        let diagnostic = crate::pro::stable_error_diagnostic(&err).unwrap_or(error_code);
        return json!({
            "isError": true,
            "content": [
                {
                    "type": "text",
                    "text": diagnostic,
                }
            ],
            "structuredContent": {
                "error": diagnostic,
                "error_code": error_code,
            }
        });
    }
    let error = err.to_string();
    json!({
        "isError": true,
        "content": [
            {
                "type": "text",
                "text": error.clone(),
            }
        ],
        "structuredContent": {
            "error": error,
        }
    })
}

pub(super) fn success_response(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

pub(super) fn error_response(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
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
    use ctx_history_index::IndexError;
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        error_response, invalid_request_response, invalid_tool_request, tool_error_result,
    };

    #[test]
    fn generation_change_is_a_retryable_typed_tool_error() {
        let error = anyhow::Error::new(IndexError::ConcurrentGenerationChange)
            .context("opening the searchable generation");
        let result = tool_error_result(error);

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
        let error = crate::presentation_limit::PresentationOutputLimitError {
            event_id,
            actual_bytes: 2048,
            maximum_bytes: 1024,
        };
        let result = tool_error_result(anyhow::Error::new(error.clone()));

        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"],
            crate::presentation_limit::presentation_output_limit_error_json(&error)
        );
        assert_eq!(
            result["structuredContent"]["error_code"],
            "output_limit_exceeded"
        );
        assert_eq!(result["structuredContent"]["retryable"], false);
        assert_eq!(result["content"][0]["text"], error.to_string());
    }
}
