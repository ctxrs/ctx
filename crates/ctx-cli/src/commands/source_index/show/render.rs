use anyhow::{anyhow, Result};
use ctx_history_core::CoreContentPolicyStatus;
use ctx_history_index::{CoreEventRecord, SessionRecord};
use serde_json::{json, Value};

use crate::{
    output::{compact_json, OutputFormat},
    presentation_limit::{enforce_presentation_output_limit, serialized_json_bytes},
    transcript::TranscriptMode,
};

use super::super::render::timestamp_json;

#[allow(clippy::too_many_arguments)]
pub(in crate::commands::source_index) fn session_transcript_value(
    session: &SessionRecord,
    mode: TranscriptMode,
    format: OutputFormat,
    rendered: Vec<Value>,
    truncated: bool,
    max_events: Option<usize>,
) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "target": "session",
        "payload_type": "session_transcript",
        "ctx_session_id": session.session_id.as_uuid(),
        "provider": session.provider,
        "provider_session_id": session.provider_session_id,
        "mode": mode.as_str(),
        "format": format.as_str(),
        "session": {
            "id": session.session_id.as_uuid(),
            "item_id": session.session_id.as_uuid(),
            "record_type": "session",
            "ctx_session_id": session.session_id.as_uuid(),
            "provider": session.provider,
            "provider_session_id": session.provider_session_id,
            "source_format": session.source_format,
            "parent_ctx_session_id": session.parent_session_id.map(|id| id.as_uuid()),
            "root_ctx_session_id": session.root_session_id.as_uuid(),
            "branch": session.branch,
            "agent_type": session.agent_type,
            "is_primary": session.is_primary,
            "workspace": session.workspace,
            "cwd": session.cwd,
        },
        "events": rendered,
        "truncated": truncated.then(|| json!({
            "events": true,
            "max_events": max_events,
        })),
    }))
}

pub(super) fn event_window_json(
    selected: &CoreEventRecord,
    events: &[CoreEventRecord],
    format: OutputFormat,
    output_limit_bytes: usize,
) -> Result<Value> {
    let references = events.iter().collect::<Vec<_>>();
    let rendered = render_event_values(&references, output_limit_bytes)?;
    event_window_value(selected, format, rendered)
}

pub(in crate::commands::source_index) fn event_window_value(
    selected: &CoreEventRecord,
    format: OutputFormat,
    rendered: Vec<Value>,
) -> Result<Value> {
    let selected_value = rendered
        .iter()
        .find(|event| {
            event["ctx_event_id"].as_str() == Some(&selected.event_id.as_uuid().to_string())
        })
        .cloned()
        .ok_or_else(|| anyhow!("selected event is absent from its pinned Core event window"))?;
    Ok(compact_json(json!({
        "schema_version": 1,
        "target": "event",
        "payload_type": "event_window",
        "ctx_event_id": selected.event_id.as_uuid(),
        "ctx_session_id": selected.session_id.as_uuid(),
        "format": format.as_str(),
        "event": selected_value,
        "events": rendered,
    })))
}

pub(in crate::commands::source_index) fn render_event_values(
    events: &[&CoreEventRecord],
    output_limit_bytes: usize,
) -> Result<Vec<Value>> {
    let mut rendered = Vec::with_capacity(events.len());
    let mut serialized_event_bytes = 2_usize;
    for event in events {
        let content = &event.core_record.content;
        let content_bytes = serialized_json_bytes(&content.normalized_body)?
            .saturating_add(serialized_json_bytes(&content.structured_content)?);
        enforce_presentation_output_limit(
            serialized_event_bytes.saturating_add(content_bytes),
            output_limit_bytes,
            event.event_id.as_uuid(),
        )?;

        let value = render_event_value(event);
        serialized_event_bytes = serialized_event_bytes
            .saturating_add(usize::from(!rendered.is_empty()))
            .saturating_add(serialized_json_bytes(&value)?);
        enforce_presentation_output_limit(
            serialized_event_bytes,
            output_limit_bytes,
            event.event_id.as_uuid(),
        )?;
        rendered.push(value);
    }
    Ok(rendered)
}

pub(in crate::commands::source_index) fn render_event_value(event: &CoreEventRecord) -> Value {
    let content = &event.core_record.content;
    let (policy_status, policy_reason, complete) = match &content.policy_status {
        CoreContentPolicyStatus::Selected => ("selected", None, true),
        CoreContentPolicyStatus::Redacted { reason } => ("redacted", Some(reason.as_str()), false),
        CoreContentPolicyStatus::Omitted { reason } => ("omitted", Some(reason.as_str()), false),
    };
    compact_json(json!({
        "ctx_event_id": event.event_id.as_uuid(),
        "item_id": event.event_id.as_uuid(),
        "record_type": "event",
        "ctx_session_id": event.session_id.as_uuid(),
        "provider": event.provider,
        "provider_session_id": event.provider_session_id,
        "source_format": event.source_format,
        "parent_ctx_session_id": event.parent_session_id.map(|id| id.as_uuid()),
        "root_ctx_session_id": event.root_session_id.as_uuid(),
        "branch": event.branch,
        "agent_type": event.agent_type,
        "is_primary": event.is_primary,
        "sequence": event.event_sequence,
        "event_type": event.event_type,
        "role": event.role,
        "occurred_at": timestamp_json(event.occurred_at_unix_ms),
        "workspace": event.workspace,
        "cwd": event.cwd,
        "touched_files": event.touched_files,
        "text": content.normalized_body.as_deref(),
        "structured_content": content.structured_content.as_ref(),
        "content": {
            "complete": complete,
            "policy_status": policy_status,
            "policy_reason": policy_reason,
        },
    }))
}
