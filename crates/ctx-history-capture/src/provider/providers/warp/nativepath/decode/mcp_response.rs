use ctx_history_core::{
    McpFailureKind, McpJsonCapture, McpTerminalResponseContent, McpTerminalStatus, McpTextCapture,
};
use serde_json::{json, Value};

use super::protobuf::{decode_last_nested_text_occurrences, validate_string_fields_occurrences};
use super::{
    select_message_oneof, validate_message_payload, WarpSelectedMessage, WarpValidatedString,
};
use crate::{CaptureError, OutputOutcome, Result};

use super::super::super::wire::{WarpWireCursor, WarpWireValue};

pub(super) struct WarpDecodedMcpToolResult {
    pub(super) outcome: OutputOutcome,
    pub(super) body: Option<String>,
    pub(super) response: McpTerminalResponseContent,
}

pub(super) fn decode_mcp_tool_result_response(
    payloads: &[Vec<u8>],
) -> Result<Option<WarpDecodedMcpToolResult>> {
    let mut selected = None;
    let mut payload_complete = true;
    for payload in payloads {
        let mut cursor = WarpWireCursor::new(payload);
        while let Some(field) = cursor.next()? {
            match (field.number, field.value) {
                (number @ (1 | 2), WarpWireValue::LengthDelimited(value)) => {
                    validate_message_payload(value)?;
                    select_message_oneof(&mut selected, number, value);
                }
                _ => payload_complete = false,
            }
        }
    }
    let Some(selected) = selected else {
        return Ok(None);
    };
    let (outcome, body, status, failure_kind, mut payload) = match selected.field {
        1 => {
            let body = decode_mcp_success_text_occurrences(&selected.payloads)?;
            (
                OutputOutcome::Success,
                body,
                McpTerminalStatus::Succeeded,
                None,
                decode_success_payload(&selected.payloads)?,
            )
        }
        2 => {
            let body = decode_last_nested_text_occurrences(&selected.payloads, 1)?;
            (
                OutputOutcome::Failure,
                body,
                McpTerminalStatus::Failed,
                Some(McpFailureKind::Unknown),
                decode_error_payload(&selected.payloads)?,
            )
        }
        _ => {
            return Err(CaptureError::SystemInvariant(
                "Warp MCP result selected an unclassified oneof arm",
            ))
        }
    };
    let text = retained_text_capture(body.as_deref());
    if !payload_complete {
        payload = McpJsonCapture::Unavailable;
    }
    Ok(Some(WarpDecodedMcpToolResult {
        outcome,
        body,
        response: McpTerminalResponseContent {
            status,
            failure_kind,
            duration_ns: None,
            text,
            payload,
        },
    }))
}

fn retained_text_capture(body: Option<&str>) -> McpTextCapture {
    if body.is_some_and(|body| !body.trim().is_empty()) {
        McpTextCapture::NormalizedBody
    } else {
        McpTextCapture::Absent
    }
}

fn decode_success_payload(payloads: &[Vec<u8>]) -> Result<McpJsonCapture> {
    let mut results = Vec::new();
    let mut complete = true;
    for payload in payloads {
        let mut cursor = WarpWireCursor::new(payload);
        while let Some(field) = cursor.next()? {
            match (field.number, field.value) {
                (1, WarpWireValue::LengthDelimited(result)) => {
                    let (value, value_complete) = decode_result_payload(result)?;
                    complete &= value_complete;
                    if let Some(value) = value {
                        results.push(value);
                    }
                }
                _ => complete = false,
            }
        }
    }
    Ok(if complete {
        McpJsonCapture::Present {
            value: json!({"success": {"results": results}}),
        }
    } else {
        McpJsonCapture::Unavailable
    })
}

fn decode_error_payload(payloads: &[Vec<u8>]) -> Result<McpJsonCapture> {
    let mut message = WarpValidatedString::default();
    let mut complete = true;
    for payload in payloads {
        let mut cursor = WarpWireCursor::new(payload);
        while let Some(field) = cursor.next()? {
            match (field.number, field.value) {
                (1, WarpWireValue::LengthDelimited(value)) => message.observe(value),
                _ => complete = false,
            }
        }
    }
    let message = message
        .into_optional("CallMCPToolResult.Error.message")?
        .unwrap_or_default();
    Ok(if complete {
        McpJsonCapture::Present {
            value: json!({"error": {"message": message}}),
        }
    } else {
        McpJsonCapture::Unavailable
    })
}

fn decode_result_payload(data: &[u8]) -> Result<(Option<Value>, bool)> {
    let mut selected = None;
    let mut complete = true;
    let mut cursor = WarpWireCursor::new(data);
    while let Some(field) = cursor.next()? {
        match (field.number, field.value) {
            (number @ (1..=3), WarpWireValue::LengthDelimited(value)) => {
                validate_message_payload(value)?;
                select_message_oneof(&mut selected, number, value);
            }
            _ => complete = false,
        }
    }
    let Some(selected) = selected else {
        return Ok((None, false));
    };
    let (kind, value, value_complete) = match selected.field {
        1 => {
            let (value, complete) = decode_text_payload(&selected.payloads)?;
            ("text", Some(value), complete)
        }
        2 => ("image", None, false),
        3 => {
            let (value, complete) = decode_resource_payload(&selected.payloads)?;
            ("resource", value, complete)
        }
        _ => {
            return Err(CaptureError::SystemInvariant(
                "Warp MCP content selected an unclassified oneof arm",
            ))
        }
    };
    Ok((
        value.map(|value| json!({(kind): value})),
        complete && value_complete,
    ))
}

fn decode_text_payload(payloads: &[Vec<u8>]) -> Result<(Value, bool)> {
    let mut text = WarpValidatedString::default();
    let mut complete = true;
    for payload in payloads {
        let mut cursor = WarpWireCursor::new(payload);
        while let Some(field) = cursor.next()? {
            match (field.number, field.value) {
                (1, WarpWireValue::LengthDelimited(value)) => text.observe(value),
                _ => complete = false,
            }
        }
    }
    let text = text
        .into_optional("CallMCPToolResult.Success.Result.Text.text")?
        .unwrap_or_default();
    Ok((json!({"text": text}), complete))
}

fn decode_resource_payload(payloads: &[Vec<u8>]) -> Result<(Option<Value>, bool)> {
    let mut selected = None;
    let mut uri = WarpValidatedString::default();
    let mut complete = true;
    for payload in payloads {
        let mut cursor = WarpWireCursor::new(payload);
        while let Some(field) = cursor.next()? {
            match (field.number, field.value) {
                (1, WarpWireValue::LengthDelimited(value)) => uri.observe(value),
                (number @ (2 | 3), WarpWireValue::LengthDelimited(value)) => {
                    validate_message_payload(value)?;
                    select_message_oneof(&mut selected, number, value);
                }
                _ => complete = false,
            }
        }
    }
    let Some(selected) = selected else {
        return Ok((None, false));
    };
    let (kind, value, value_complete) = match selected {
        WarpSelectedMessage { field: 2, payloads } => {
            let (value, complete) = decode_resource_text_payload(&payloads)?;
            ("text", value, complete)
        }
        WarpSelectedMessage {
            field: 3,
            payloads: _,
        } => return Ok((None, false)),
        _ => {
            return Err(CaptureError::SystemInvariant(
                "Warp MCP resource selected an unclassified oneof arm",
            ))
        }
    };
    let uri = uri
        .into_optional("MCPResourceContent.uri")?
        .unwrap_or_default();
    Ok((
        Some(json!({"uri": uri, (kind): value})),
        complete && value_complete,
    ))
}

fn decode_resource_text_payload(payloads: &[Vec<u8>]) -> Result<(Value, bool)> {
    validate_string_fields_occurrences(payloads, &[1, 2], "MCPResourceContent.Text")?;
    let mut content = WarpValidatedString::default();
    let mut mime_type = WarpValidatedString::default();
    let mut complete = true;
    for payload in payloads {
        let mut cursor = WarpWireCursor::new(payload);
        while let Some(field) = cursor.next()? {
            match (field.number, field.value) {
                (1, WarpWireValue::LengthDelimited(value)) => content.observe(value),
                (2, WarpWireValue::LengthDelimited(value)) => mime_type.observe(value),
                _ => complete = false,
            }
        }
    }
    let content = content
        .into_optional("MCP resource content")?
        .unwrap_or_default();
    let mime_type = mime_type
        .into_optional("MCP resource MIME type")?
        .unwrap_or_default();
    Ok((
        json!({"content": content, "mime_type": mime_type}),
        complete,
    ))
}

fn decode_mcp_success_text_occurrences(payloads: &[Vec<u8>]) -> Result<Option<String>> {
    let mut parts = Vec::new();
    for payload in payloads {
        let mut cursor = WarpWireCursor::new(payload);
        while let Some(field) = cursor.next()? {
            let (1, WarpWireValue::LengthDelimited(result)) = (field.number, field.value) else {
                continue;
            };
            if let Some(text) = decode_mcp_result_content_text(result)? {
                if !text.trim().is_empty() {
                    parts.push(text);
                }
            }
        }
    }
    Ok((!parts.is_empty()).then(|| parts.join("\n")))
}

fn decode_mcp_result_content_text(data: &[u8]) -> Result<Option<String>> {
    let mut cursor = WarpWireCursor::new(data);
    let mut selected = None;
    while let Some(field) = cursor.next()? {
        if let (number @ (1..=3), WarpWireValue::LengthDelimited(value)) =
            (field.number, field.value)
        {
            validate_message_payload(value)?;
            select_message_oneof(&mut selected, number, value);
        }
    }
    let Some(selected) = selected else {
        return Ok(None);
    };
    match selected.field {
        1 => decode_last_nested_text_occurrences(&selected.payloads, 1),
        2 => Ok(None),
        3 => decode_mcp_resource_text_occurrences(&selected.payloads),
        _ => Err(CaptureError::SystemInvariant(
            "Warp MCP content selected an unclassified oneof arm",
        )),
    }
}

fn decode_mcp_resource_text_occurrences(payloads: &[Vec<u8>]) -> Result<Option<String>> {
    let mut selected = None;
    let mut uri = WarpValidatedString::default();
    for payload in payloads {
        let mut cursor = WarpWireCursor::new(payload);
        while let Some(field) = cursor.next()? {
            match (field.number, field.value) {
                (1, WarpWireValue::LengthDelimited(value)) => uri.observe(value),
                (number @ (2 | 3), WarpWireValue::LengthDelimited(value)) => {
                    validate_message_payload(value)?;
                    select_message_oneof(&mut selected, number, value);
                }
                _ => {}
            }
        }
    }
    match selected {
        Some(WarpSelectedMessage { field: 2, payloads }) => {
            let _ = uri.into_optional("MCP resource URI")?;
            decode_last_nested_text_occurrences(&payloads, 1)
        }
        Some(WarpSelectedMessage {
            field: 3,
            payloads: _,
        }) => Ok(None),
        None => Ok(None),
        Some(_) => Err(CaptureError::SystemInvariant(
            "Warp MCP resource selected an unclassified oneof arm",
        )),
    }
}
