use std::io;

use serde_json::{json, Value};
use uuid::Uuid;

use super::response::{error_response, success_response, tool_error_result};

#[derive(Default)]
struct SerializedByteCounter {
    bytes: usize,
}

impl io::Write for SerializedByteCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buffer.len());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn serialized_json_line_bytes(value: &Value) -> serde_json::Result<usize> {
    let mut counter = SerializedByteCounter::default();
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.bytes.saturating_add(1))
}

#[cfg(test)]
pub(super) fn is_show_tool_call(message: &Value) -> bool {
    message.get("method").and_then(Value::as_str) == Some("tools/call")
        && matches!(
            message.pointer("/params/name").and_then(Value::as_str),
            Some("show_session" | "show_event")
        )
}

#[cfg(test)]
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

    let message = "query_events response exceeds the MCP output limit; lower `limit` and retry with content=text or content=none";
    let result = json!({
        "isError": true,
        "content": [{ "type": "text", "text": message }],
        "structuredContent": {
            "error": message,
            "error_code": "output_limit_exceeded",
            "actual_bytes": actual_bytes,
            "maximum_bytes": output_limit_bytes,
            "retryable": true,
            "recommendation": "lower `limit` and retry with content=text or content=none",
        },
    });
    let mut bounded = success_response(response_id, result);
    if serialized_json_line_bytes(&bounded).is_ok_and(|bytes| bytes <= output_limit_bytes) {
        bounded
    } else {
        let response_id = bounded
            .get_mut("id")
            .map(Value::take)
            .unwrap_or(Value::Null);
        bounded_protocol_error(
            response_id,
            output_limit_bytes,
            -32603,
            "query_events response too large",
            json!({ "error": "output_limit_exceeded" }),
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
        Some(event_id) => tool_error_result(crate::tool_backend::ToolBackendError::OutputLimit {
            event_id,
            actual_bytes: serialized_json_line_bytes(&response).unwrap_or(usize::MAX),
            maximum_bytes: output_limit_bytes,
        }),
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
    let mut bounded = success_response(response_id, result);
    if serialized_json_line_bytes(&bounded).is_ok_and(|bytes| bytes <= output_limit_bytes) {
        bounded
    } else {
        let response_id = bounded
            .get_mut("id")
            .map(Value::take)
            .unwrap_or(Value::Null);
        bounded_protocol_error(
            response_id,
            output_limit_bytes,
            -32603,
            "Show response too large",
            json!({ "error": "output_limit_exceeded" }),
        )
    }
}

fn bounded_protocol_error(
    response_id: Value,
    output_limit_bytes: usize,
    code: i64,
    message: &str,
    data: Value,
) -> Value {
    let mut bounded = error_response(response_id, code, message, Some(data));
    if serialized_json_line_bytes(&bounded).is_ok_and(|bytes| bytes <= output_limit_bytes) {
        return bounded;
    }

    bounded
        .get_mut("error")
        .and_then(Value::as_object_mut)
        .and_then(|error| error.remove("data"));
    // Every accepted request ID is capped so this smallest correlated error
    // fits the production response bounds. Correlation is never discarded to
    // satisfy an ad hoc lower internal limit.
    bounded
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
