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
    tool_result_with_text(structured, text)
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

fn structured_error_result(structured: Value) -> Value {
    let text = render_diagnostic_text(&structured);
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

/// Reads the exact serializer used by CLI JSON errors. This keeps MCP
/// `structuredContent` on the canonical diagnostic path without depending on
/// the diagnostic's concrete Rust representation.
fn stable_pro_error_value(err: &Error) -> Option<Value> {
    let mut encoded = Vec::new();
    if !crate::pro::write_stable_error_json(&mut encoded, err).ok()? {
        return None;
    }
    serde_json::from_slice(&encoded).ok()
}

fn render_diagnostic_text(structured: &Value) -> String {
    let message = structured
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| structured.get("error").and_then(Value::as_str))
        .unwrap_or("ctx blame failed");
    let mut text = single_line(message);
    if let Some(argv) = structured
        .pointer("/next_action/argv")
        .and_then(Value::as_array)
        .filter(|argv| !argv.is_empty())
    {
        text.push_str("\nNext:");
        for argument in argv.iter().filter_map(Value::as_str) {
            text.push(' ');
            text.push_str(&display_argument(argument));
        }
    }
    text
}

fn single_line(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn display_argument(argument: &str) -> String {
    if !argument.is_empty()
        && argument
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&byte))
    {
        argument.to_owned()
    } else {
        serde_json::to_string(argument).unwrap_or_else(|_| "\"<invalid argument>\"".to_owned())
    }
}

pub(super) fn tool_error_result(err: Error) -> Value {
    if let Some(error) = err.downcast_ref::<crate::commands::list::events::EventQueryError>() {
        let structured = crate::commands::list::events::event_query_error_value(error);
        return json!({
            "isError": true,
            "content": [{ "type": "text", "text": error.to_string() }],
            "structuredContent": structured,
        });
    }
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
    if let Some(error) = err.downcast_ref::<ctx_history_refresh::GenerationQueryAuthorityError>() {
        let detail = error.to_string();
        let structured =
            crate::commands::source_index::generation_query_authority_error_json(error);
        return json!({
            "isError": true,
            "content": [{ "type": "text", "text": detail.clone() }],
            "structuredContent": structured,
        });
    }
    if let Some(error) = err.downcast_ref::<ctx_history_index::IndexError>() {
        let error_code = match error {
            ctx_history_index::IndexError::SessionEventCursorGenerationMismatch { .. } => {
                Some("cursor_stale")
            }
            ctx_history_index::IndexError::SessionEventCursorSessionMismatch => {
                Some("cursor_mismatch")
            }
            ctx_history_index::IndexError::InvalidSessionEventCursorSessionIdentity
            | ctx_history_index::IndexError::InvalidSessionEventCursorCoordinate => {
                Some("invalid_cursor")
            }
            _ => None,
        };
        if let Some(error_code) = error_code {
            let detail = error.to_string();
            return json!({
                "isError": true,
                "content": [
                    {
                        "type": "text",
                        "text": detail.clone(),
                    }
                ],
                "structuredContent": {
                    "error": detail.clone(),
                    "error_code": error_code,
                    "detail": detail,
                    "retryable": false,
                },
            });
        }
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
    if let Some(error) = err.downcast_ref::<crate::semantic::SemanticNotReady>() {
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
    if let Some(structured) = stable_pro_error_value(&err) {
        return structured_error_result(structured);
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
    use serde_json::{json, Value};
    use uuid::Uuid;

    use super::{
        error_response, invalid_request_response, invalid_tool_request, stable_pro_error_value,
        structured_error_result, tool_error_result, tool_result,
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
    fn pro_error_structured_content_is_the_cli_json_diagnostic() {
        let error = anyhow::anyhow!("resource_not_found");
        let mut cli_json = Vec::new();
        assert!(crate::pro::write_stable_error_json(&mut cli_json, &error).unwrap());
        let expected: serde_json::Value = serde_json::from_slice(&cli_json).unwrap();

        assert_eq!(stable_pro_error_value(&error), Some(expected.clone()));
        let result = tool_error_result(error);
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"], expected);
        let expected_text = result["structuredContent"]
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| result["structuredContent"]["error"].as_str())
            .unwrap();
        assert!(result["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.starts_with(expected_text)));
    }

    #[test]
    fn diagnostic_text_is_rendered_from_message_and_one_typed_argv_action() {
        let structured = json!({
            "error": "resource_not_found",
            "error_code": "resource_not_found",
            "reason": "target_not_indexed",
            "message": "The current Pro graph does not contain this blame target.",
            "retryable": false,
            "next_action": {
                "kind": "search_core",
                "argv": ["ctx", "search", "src/file with spaces.rs", "--refresh", "off"]
            }
        });
        let result = structured_error_result(structured.clone());

        assert_eq!(result["structuredContent"], structured);
        assert_eq!(result["isError"], true);
        assert_eq!(
            result["content"][0]["text"],
            "The current Pro graph does not contain this blame target.\nNext: ctx search \"src/file with spaces.rs\" --refresh off"
        );
    }

    #[test]
    fn conflicting_attribution_remains_a_successful_tool_result() {
        let structured = json!({
            "target": {"kind": "commit"},
            "outcome": {
                "attribution": "conflicting",
                "coverage": {
                    "unit": "commit_fact",
                    "evaluated": 1,
                    "proven": 0,
                    "possible": 0,
                    "conflicting": 1,
                    "none": 0
                }
            },
            "freshness": {"state": "current"},
            "matches": [],
            "evidence": []
        });
        let result = tool_result(structured.clone());

        assert!(result.get("isError").is_none());
        assert_eq!(result["structuredContent"], structured);
        assert!(result["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("Producer evidence conflicts")));
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

    #[test]
    fn session_cursor_failures_have_stable_typed_tool_errors() {
        let cases = [
            (
                ctx_history_index::IndexError::SessionEventCursorGenerationMismatch {
                    cursor_generation: "old".to_owned(),
                    pinned_generation: "new".to_owned(),
                },
                "cursor_stale",
            ),
            (
                ctx_history_index::IndexError::SessionEventCursorSessionMismatch,
                "cursor_mismatch",
            ),
            (
                ctx_history_index::IndexError::InvalidSessionEventCursorCoordinate,
                "invalid_cursor",
            ),
        ];

        for (error, code) in cases {
            let message = error.to_string();
            let result = tool_error_result(anyhow::Error::new(error));
            assert_eq!(result["isError"], true);
            assert_eq!(result["structuredContent"]["error_code"], code);
            assert_eq!(result["structuredContent"]["retryable"], false);
            assert_eq!(result["structuredContent"]["detail"], message);
            assert_eq!(result["content"][0]["text"], message);
        }
    }
}
