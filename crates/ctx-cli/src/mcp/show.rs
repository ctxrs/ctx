use std::path::Path;

use anyhow::Result;
use serde_json::{json, Value};

use super::{
    event_window, event_window_json, invalid_tool_request, open_existing_store, optional_string,
    optional_transcript_mode, optional_usize, required_uuid, session_transcript_json, OutputFormat,
    TranscriptMode, MAX_EVENT_WINDOW, MCP_MAX_SESSION_EVENTS,
};
use crate::complete_content::{
    resolve_event_contents, ContentPolicy, MCP_COMPLETE_CONTENT_MAX_OUTPUT_BYTES,
};

pub(super) fn tool_show_session(arguments: &Value, data_root: &Path) -> Result<Value> {
    let session_id = required_uuid(arguments, "ctx_session_id")?;
    let mode = optional_transcript_mode(arguments, "mode")?.unwrap_or(TranscriptMode::Lite);
    let content_policy = optional_content_policy(arguments, "content")?;
    let store = open_existing_store(data_root)?;
    let session = store.get_session(session_id)?;
    let mut events = store.events_for_session_limited(session.id, MCP_MAX_SESSION_EVENTS + 1)?;
    let truncated = events.len() > MCP_MAX_SESSION_EVENTS;
    if truncated {
        events.truncate(MCP_MAX_SESSION_EVENTS);
    }
    let selected = crate::transcript::selected_transcript_events(&events, mode);
    let content = resolve_event_contents(
        &store,
        &selected,
        content_policy,
        MCP_COMPLETE_CONTENT_MAX_OUTPUT_BYTES,
    )?;
    let mut value = session_transcript_json(
        &store,
        &session,
        &events,
        mode,
        OutputFormat::Json,
        &content,
    )?;
    if truncated {
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "truncated".to_owned(),
                json!({
                    "events": true,
                    "max_events": MCP_MAX_SESSION_EVENTS,
                }),
            );
        }
    }
    Ok(value)
}

pub(super) fn tool_show_event(arguments: &Value, data_root: &Path) -> Result<Value> {
    let event_id = required_uuid(arguments, "ctx_event_id")?;
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
    let store = open_existing_store(data_root)?;
    let event = store.get_event(event_id)?;
    let events = event_window(&store, &event, before, after, window)?;
    let selected = events.iter().collect::<Vec<_>>();
    let content = resolve_event_contents(
        &store,
        &selected,
        content_policy,
        MCP_COMPLETE_CONTENT_MAX_OUTPUT_BYTES,
    )?;
    event_window_json(&store, &event, &events, OutputFormat::Json, &content)
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
