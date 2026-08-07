use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use super::{
    invalid_tool_request, optional_string, optional_transcript_mode, optional_usize,
    TranscriptMode, MAX_EVENT_WINDOW, MCP_DEFAULT_SESSION_PAGE_LIMIT, MCP_MAX_SESSION_CURSOR_BYTES,
    MCP_MAX_SESSION_PAGE_LIMIT,
};
use crate::presentation_limit::MCP_PRESENTATION_MAX_OUTPUT_BYTES;

pub(super) fn tool_show_session(arguments: &Value, data_root: &Path) -> Result<(Value, Value)> {
    let session_id = optional_string(arguments, "ctx_session_id")?
        .ok_or_else(|| invalid_tool_request("ctx_session_id is required"))?;
    validate_ctx_id(&session_id, "ctx_session_id", "session")?;
    let mode = optional_transcript_mode(arguments, "mode")?.unwrap_or(TranscriptMode::Lite);
    let limit = optional_usize(arguments, "limit")?.unwrap_or(MCP_DEFAULT_SESSION_PAGE_LIMIT);
    if !(1..=MCP_MAX_SESSION_PAGE_LIMIT).contains(&limit) {
        return Err(invalid_tool_request(format!(
            "limit must be between 1 and {MCP_MAX_SESSION_PAGE_LIMIT}"
        )));
    }
    let cursor = optional_session_cursor(arguments)?;
    crate::commands::source_index::mcp_show_session_with_compact(
        data_root,
        &session_id,
        mode,
        limit,
        cursor.as_deref(),
        MCP_PRESENTATION_MAX_OUTPUT_BYTES,
    )
}

fn optional_session_cursor(arguments: &Value) -> Result<Option<String>> {
    let cursor = optional_string(arguments, "cursor")?;
    match cursor {
        Some(value)
            if value.is_empty()
                || value.len() > MCP_MAX_SESSION_CURSOR_BYTES
                || !value.is_ascii() =>
        {
            Err(invalid_tool_request(format!(
                "cursor must contain 1 to {MCP_MAX_SESSION_CURSOR_BYTES} ASCII bytes"
            )))
        }
        value => Ok(value),
    }
}

pub(super) fn tool_show_event(arguments: &Value, data_root: &Path) -> Result<(Value, Value)> {
    let event_id = optional_string(arguments, "ctx_event_id")?
        .ok_or_else(|| invalid_tool_request("ctx_event_id is required"))?;
    validate_ctx_id(&event_id, "ctx_event_id", "event")?;
    let before = optional_usize(arguments, "before")?.unwrap_or(0);
    let after = optional_usize(arguments, "after")?.unwrap_or(0);
    let window = optional_usize(arguments, "window")?;
    if before > MAX_EVENT_WINDOW
        || after > MAX_EVENT_WINDOW
        || window.is_some_and(|window| window > MAX_EVENT_WINDOW)
    {
        return Err(invalid_tool_request(format!(
            "show_event before/after/window must be {MAX_EVENT_WINDOW} or less"
        )));
    }
    crate::commands::source_index::mcp_show_event_with_compact(
        data_root,
        &event_id,
        before,
        after,
        window,
        MCP_PRESENTATION_MAX_OUTPUT_BYTES,
    )
}

fn validate_ctx_id(id: &str, argument: &str, kind: &str) -> Result<()> {
    if uuid::Uuid::parse_str(id.trim()).is_ok() {
        return Ok(());
    }
    crate::transcript::normalize_uuid_prefix(id, kind)
        .map(|_| ())
        .map_err(|error| invalid_tool_request(format!("invalid {argument}: {error}")))
}
