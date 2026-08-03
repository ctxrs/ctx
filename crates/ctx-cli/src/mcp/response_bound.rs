use serde_json::{json, Value};
use uuid::Uuid;

use super::response::{error_response, success_response, tool_error_result};
use crate::presentation_limit::serialized_json_line_bytes;

pub(super) fn is_show_tool_call(message: &Value) -> bool {
    message.get("method").and_then(Value::as_str) == Some("tools/call")
        && matches!(
            message.pointer("/params/name").and_then(Value::as_str),
            Some("show_session" | "show_event")
        )
}

pub(super) fn is_blame_tool_call(message: &Value) -> bool {
    message.get("method").and_then(Value::as_str) == Some("tools/call")
        && message.pointer("/params/name").and_then(Value::as_str) == Some("blame")
}

pub(super) fn is_query_events_tool_call(message: &Value) -> bool {
    message.get("method").and_then(Value::as_str) == Some("tools/call")
        && message.pointer("/params/name").and_then(Value::as_str) == Some("query_events")
}

pub(super) fn bound_query_events_mcp_response(
    response: Value,
    response_id: Value,
    output_limit_bytes: usize,
) -> Value {
    let actual_bytes = serialized_json_line_bytes(&response).unwrap_or(usize::MAX);
    if actual_bytes <= output_limit_bytes {
        return response;
    }

    let message = "query_events response exceeds the MCP output limit; retry with content=text or content=none";
    let result = json!({
        "isError": true,
        "content": [{ "type": "text", "text": message }],
        "structuredContent": {
            "error": message,
            "error_code": "output_limit_exceeded",
            "actual_bytes": actual_bytes,
            "maximum_bytes": output_limit_bytes,
            "retryable": true,
            "recommendation": "retry with content=text or content=none",
        },
    });
    let bounded = success_response(response_id, result);
    if serialized_json_line_bytes(&bounded).is_ok_and(|bytes| bytes <= output_limit_bytes) {
        bounded
    } else {
        error_response(
            Value::Null,
            -32603,
            "query_events response too large",
            Some(json!({ "error": "output_limit_exceeded" })),
        )
    }
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

pub(super) fn bound_show_mcp_response(
    response: Value,
    response_id: Value,
    output_limit_bytes: usize,
) -> Value {
    if serialized_json_line_bytes(&response).is_ok_and(|bytes| bytes <= output_limit_bytes) {
        return response;
    }

    let result = match response_show_event_id(&response) {
        Some(event_id) => tool_error_result(anyhow::Error::new(
            crate::presentation_limit::PresentationOutputLimitError {
                event_id,
                actual_bytes: serialized_json_line_bytes(&response).unwrap_or(usize::MAX),
                maximum_bytes: output_limit_bytes,
            },
        )),
        None => json!({
            "isError": true,
            "content": [{
                "type": "text",
                "text": "show response exceeds the serialized MCP output limit",
            }],
            "structuredContent": {
                "error": "output_limit_exceeded",
                "error_code": "output_limit_exceeded",
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
            "Show response too large",
            Some(json!({ "error": "output_limit_exceeded" })),
        )
    }
}

fn response_show_event_id(response: &Value) -> Option<Uuid> {
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
