use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use super::{
    invalid_tool_request, optional_string, optional_transcript_mode, optional_usize,
    TranscriptMode, MAX_EVENT_WINDOW, MCP_MAX_SESSION_EVENTS,
};
use crate::complete_content::{ContentPolicy, MCP_COMPLETE_CONTENT_MAX_OUTPUT_BYTES};

pub(super) fn tool_show_session(arguments: &Value, data_root: &Path) -> Result<Value> {
    let session_id = optional_string(arguments, "ctx_session_id")?
        .ok_or_else(|| invalid_tool_request("ctx_session_id is required"))?;
    validate_ctx_id(&session_id, "ctx_session_id", "session")?;
    let mode = optional_transcript_mode(arguments, "mode")?.unwrap_or(TranscriptMode::Lite);
    let content_policy = optional_content_policy(arguments, "content")?;
    crate::commands::source_index::mcp_show_session(
        data_root,
        &session_id,
        mode,
        content_policy,
        MCP_MAX_SESSION_EVENTS,
        MCP_COMPLETE_CONTENT_MAX_OUTPUT_BYTES,
    )
}

pub(super) fn tool_show_event(arguments: &Value, data_root: &Path) -> Result<Value> {
    let event_id = optional_string(arguments, "ctx_event_id")?
        .ok_or_else(|| invalid_tool_request("ctx_event_id is required"))?;
    validate_ctx_id(&event_id, "ctx_event_id", "event")?;
    let before = optional_usize(arguments, "before")?.unwrap_or(0);
    let after = optional_usize(arguments, "after")?.unwrap_or(0);
    let window = optional_usize(arguments, "window")?;
    let content_policy = optional_content_policy(arguments, "content")?;
    if before > MAX_EVENT_WINDOW
        || after > MAX_EVENT_WINDOW
        || window.is_some_and(|window| window > MAX_EVENT_WINDOW)
    {
        return Err(invalid_tool_request(format!(
            "show_event before/after/window must be {MAX_EVENT_WINDOW} or less"
        )));
    }
    crate::commands::source_index::mcp_show_event(
        data_root,
        &event_id,
        before,
        after,
        window,
        content_policy,
        MCP_COMPLETE_CONTENT_MAX_OUTPUT_BYTES,
    )
}

fn optional_content_policy(arguments: &Value, key: &str) -> Result<ContentPolicy> {
    match optional_string(arguments, key)?.as_deref() {
        None | Some("indexed") => Ok(ContentPolicy::Indexed),
        Some("complete") => Ok(ContentPolicy::Complete),
        Some(_) => Err(invalid_tool_request(
            "content must be one of indexed, complete",
        )),
    }
}

fn validate_ctx_id(id: &str, argument: &str, kind: &str) -> Result<()> {
    if uuid::Uuid::parse_str(id.trim()).is_ok() {
        return Ok(());
    }
    crate::transcript::normalize_uuid_prefix(id, kind)
        .map(|_| ())
        .map_err(|error| invalid_tool_request(format!("invalid {argument}: {error}")))
}
