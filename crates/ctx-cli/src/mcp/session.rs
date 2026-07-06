#[allow(unused_imports)]
use super::*;

pub(crate) const MCP_MAX_SESSION_EVENTS: usize = 200;

pub(crate) fn tool_show_session(arguments: &Value, data_root: &Path) -> Result<Value> {
    let store = open_existing_store(data_root)?;
    let session_id = required_uuid(arguments, "ctx_session_id")?;
    let mode = optional_transcript_mode(arguments, "mode")?.unwrap_or(TranscriptMode::Lite);
    let session = store.get_session(session_id)?;
    let mut events = store.events_for_session_limited(session.id, MCP_MAX_SESSION_EVENTS + 1)?;
    let truncated = events.len() > MCP_MAX_SESSION_EVENTS;
    if truncated {
        events.truncate(MCP_MAX_SESSION_EVENTS);
    }
    let mut value = session_transcript_json(&store, &session, &events, mode, OutputFormat::Json);
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
