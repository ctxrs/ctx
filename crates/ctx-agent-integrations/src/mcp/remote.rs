//! The remote retained-log surface carries opaque selectors. Their format and
//! authorization belong to the concrete backend, not the MCP protocol crate.

use serde_json::{json, Value};

use super::{
    invalid_tool_request, object_schema, optional_string, optional_transcript_mode, optional_usize,
    search_request, McpToolKind, MCP_PRESENTATION_MAX_OUTPUT_BYTES,
};
use crate::tool_backend::{
    ShowEventRequest, ShowSessionRequest, ToolBackend, ToolBackendError, ToolOperation,
    ToolSearchBackend, ToolTranscriptMode,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryToolSurface {
    Local,
    RemoteLog,
}

const PAGE_LIMIT: usize = 100;
const SESSION_DEFAULT: usize = 50;
const SELECTOR_BYTES: usize = 4096;
const CURSOR_BYTES: usize = 8192;

pub(super) fn allowed_arguments(operation: McpToolKind) -> Option<&'static [&'static str]> {
    match operation {
        McpToolKind::Status => Some(&[]),
        McpToolKind::Search => Some(&["query", "limit", "backend"]),
        McpToolKind::ShowSession => Some(&["ctx_session_id", "mode", "limit", "cursor"]),
        McpToolKind::ShowEvent => Some(&["ctx_event_id", "before", "after", "window"]),
        _ => None,
    }
}

pub(super) fn parse_operation(
    operation: McpToolKind,
    arguments: &Value,
    backend: &impl ToolBackend,
) -> Result<ToolOperation, ToolBackendError> {
    match operation {
        McpToolKind::Status => Ok(ToolOperation::Status),
        McpToolKind::Search => {
            let request = search_request(arguments, backend)?;
            page_limit(request.limit)?;
            if !matches!(request.backend, None | Some(ToolSearchBackend::Lexical)) {
                return Err(invalid_tool_request(
                    "shared search supports only lexical search",
                ));
            }
            Ok(ToolOperation::Search(request))
        }
        McpToolKind::ShowSession => {
            let mode =
                optional_transcript_mode(arguments, "mode")?.unwrap_or(ToolTranscriptMode::Log);
            if mode != ToolTranscriptMode::Log {
                return Err(invalid_tool_request(
                    "shared sessions support mode=log only; lite/full transcript selection is local-only",
                ));
            }
            let limit = optional_usize(arguments, "limit")?.unwrap_or(SESSION_DEFAULT);
            page_limit(limit)?;
            let cursor = optional_string(arguments, "cursor")?;
            if let Some(cursor) = &cursor {
                opaque_value(cursor, "cursor", CURSOR_BYTES)?;
            }
            Ok(ToolOperation::ShowSession(ShowSessionRequest {
                selector: selector(arguments, "ctx_session_id")?,
                mode,
                limit,
                cursor,
                output_limit_bytes: MCP_PRESENTATION_MAX_OUTPUT_BYTES,
            }))
        }
        McpToolKind::ShowEvent => {
            let before = optional_usize(arguments, "before")?.unwrap_or(0);
            let after = optional_usize(arguments, "after")?.unwrap_or(0);
            let window = optional_usize(arguments, "window")?;
            if before != 0 || after != 0 || window.is_some_and(|n| n != 0) {
                return Err(invalid_tool_request(
                    "shared show_event returns one exact event; use its session citation for surrounding events",
                ));
            }
            Ok(ToolOperation::ShowEvent(ShowEventRequest {
                selector: selector(arguments, "ctx_event_id")?,
                before,
                after,
                window,
                output_limit_bytes: MCP_PRESENTATION_MAX_OUTPUT_BYTES,
            }))
        }
        _ => Err(invalid_tool_request(
            "tool is unavailable on this server connection",
        )),
    }
}

fn page_limit(limit: usize) -> Result<(), ToolBackendError> {
    if !(1..=PAGE_LIMIT).contains(&limit) {
        return Err(invalid_tool_request(format!(
            "shared page limit must be 1..{PAGE_LIMIT}"
        )));
    }
    Ok(())
}

fn selector(arguments: &Value, key: &str) -> Result<String, ToolBackendError> {
    let value = optional_string(arguments, key)?
        .ok_or_else(|| invalid_tool_request(format!("{key} is required")))?;
    opaque_value(&value, key, SELECTOR_BYTES)?;
    Ok(value)
}

fn opaque_value(value: &str, key: &str, maximum: usize) -> Result<(), ToolBackendError> {
    if value.is_empty() || value.len() > maximum || !value.is_ascii() {
        return Err(invalid_tool_request(format!(
            "{key} must contain 1..{maximum} ASCII bytes"
        )));
    }
    Ok(())
}

pub(super) fn tool_definitions() -> Vec<Value> {
    [
        (
            "status", "Return coverage for the selected shared collection.",
            object_schema(json!({}), vec![]),
        ),
        (
            "search", "Search shared retained history lexically. Returns event hits with exact event and session citations; no local history is read.",
            object_schema(json!({
                "query": {"type":"string", "minLength":1},
                "limit": {"type":"integer", "minimum":1, "maximum":PAGE_LIMIT, "default":20},
                "backend": {"type":"string", "enum":["lexical"], "default":"lexical"}
            }), vec!["query"]),
        ),
        (
            "show_session", "Return a page of the complete retained event log, including tool activity. Pass session_citation from shared search unchanged. Continue with next_cursor until null; each page rechecks access.",
            object_schema(json!({
                "ctx_session_id": {"type":"string", "minLength":1, "maxLength":SELECTOR_BYTES, "description":"Exact opaque session_citation from shared search."},
                "mode": {"type":"string", "enum":["log"], "default":"log"},
                "limit": {"type":"integer", "minimum":1, "maximum":PAGE_LIMIT, "default":SESSION_DEFAULT},
                "cursor": {"type":"string", "minLength":1, "maxLength":CURSOR_BYTES, "description":"Opaque next_cursor from the preceding page of this exact citation."}
            }), vec!["ctx_session_id"]),
        ),
        (
            "show_event", "Return one exact retained event. Pass citation from shared search unchanged; use session_citation for surrounding events.",
            object_schema(json!({
                "ctx_event_id": {"type":"string", "minLength":1, "maxLength":SELECTOR_BYTES, "description":"Exact opaque citation from shared search."},
                "before": {"type":"integer", "minimum":0, "maximum":0, "default":0},
                "after": {"type":"integer", "minimum":0, "maximum":0, "default":0},
                "window": {"type":"integer", "minimum":0, "maximum":0}
            }), vec!["ctx_event_id"]),
        ),
    ]
    .into_iter()
    .map(|(name, description, schema)| json!({
        "name":name, "description":description, "inputSchema":schema,
        "annotations":{"readOnlyHint":true}
    }))
    .collect()
}
