use serde_json::{json, Value};
use uuid::Uuid;

use super::response::{error_response, success_response, tool_error_result};
use crate::complete_content::serialized_json_line_bytes;

pub(super) fn is_complete_content_tool_call(message: &Value) -> bool {
    message.get("method").and_then(Value::as_str) == Some("tools/call")
        && matches!(
            message.pointer("/params/name").and_then(Value::as_str),
            Some("show_session" | "show_event")
        )
        && message
            .pointer("/params/arguments/content")
            .and_then(Value::as_str)
            == Some("complete")
}

pub(super) fn is_blame_tool_call(message: &Value) -> bool {
    message.get("method").and_then(Value::as_str) == Some("tools/call")
        && message.pointer("/params/name").and_then(Value::as_str) == Some("blame")
}

pub(super) fn bound_blame_mcp_response(
    response: Value,
    response_id: Value,
    output_limit_bytes: usize,
) -> Value {
    if serialized_json_line_bytes(&response).is_ok_and(|bytes| bytes <= output_limit_bytes) {
        return response;
    }

    let message = "blame response exceeds the MCP output limit; lower `limit` or use the CLI with `ctx blame ... --format json`";
    let result = json!({
        "isError": true,
        "content": [{
            "type": "text",
            "text": message,
        }],
        "structuredContent": {
            "error": message,
            "error_code": "invalid_response",
            "retryable": true,
        },
    });
    let bounded = success_response(response_id, result);
    if serialized_json_line_bytes(&bounded).is_ok_and(|bytes| bytes <= output_limit_bytes) {
        bounded
    } else {
        error_response(
            Value::Null,
            -32603,
            "Blame response too large",
            Some(json!({ "error": "invalid_response" })),
        )
    }
}

pub(super) fn bound_complete_content_mcp_response(
    response: Value,
    response_id: Value,
    output_limit_bytes: usize,
) -> Value {
    if serialized_json_line_bytes(&response).is_ok_and(|bytes| bytes <= output_limit_bytes) {
        return response;
    }

    let result = match response_complete_content_event_id(&response) {
        Some(event_id) => tool_error_result(anyhow::Error::new(
            ctx_history_capture::complete_content::CompleteContentError::new(
                ctx_history_capture::complete_content::CompleteContentErrorKind::ContentTooLarge,
                event_id,
            ),
        )),
        None => json!({
            "isError": true,
            "content": [{
                "type": "text",
                "text": "complete content response exceeds the serialized MCP output limit",
            }],
            "structuredContent": {
                "error": "content_too_large",
                "error_code": "content_too_large",
                "retryable": false,
            },
        }),
    };
    let bounded = success_response(response_id, result);
    if serialized_json_line_bytes(&bounded).is_ok_and(|bytes| bytes <= output_limit_bytes) {
        bounded
    } else {
        error_response(
            Value::Null,
            -32603,
            "Complete content response too large",
            Some(json!({ "error": "content_too_large" })),
        )
    }
}

fn response_complete_content_event_id(response: &Value) -> Option<Uuid> {
    response
        .pointer("/result/structuredContent/ctx_event_id")
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
        .or_else(|| {
            response
                .pointer("/result/structuredContent/events")
                .and_then(Value::as_array)
                .and_then(|events| events.last())
                .and_then(|event| event.get("ctx_event_id"))
                .and_then(Value::as_str)
                .and_then(|value| Uuid::parse_str(value).ok())
        })
}

#[cfg(test)]
mod tests;
