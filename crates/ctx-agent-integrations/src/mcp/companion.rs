use serde_json::Value;

use super::{request_id_is_accepted, McpToolKind, RequestDescriptor};

/// Returns the companion-owned operation only after the generic JSON-RPC
/// request gate has accepted the envelope. Private arguments remain opaque.
pub fn validated_companion_tool_request(
    message: &Value,
    descriptor: RequestDescriptor,
    initialized: bool,
) -> Option<McpToolKind> {
    let object = message.as_object()?;
    if !initialized
        || !request_id_is_accepted(object.get("id"))
        || !object.contains_key("id")
        || object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || object.get("method").and_then(Value::as_str) != Some("tools/call")
        || object
            .get("params")
            .is_some_and(|params| !params.is_object())
    {
        return None;
    }
    match descriptor {
        RequestDescriptor::ToolCall { operation } if operation.is_companion_owned() => {
            Some(operation)
        }
        _ => None,
    }
}
