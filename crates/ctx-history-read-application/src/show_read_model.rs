use std::fmt;

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ctx_history_core::CoreContentPolicyStatus;
use ctx_history_index_query::{
    CopiedEventLineage, CoreEventRecord, IndexError, SessionEventCursor, SessionRecord,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::json::{compact_json, event_copy_json, timestamp_json};
use crate::{copied_lineage_read_model, ShowSessionEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredOutputFormat {
    Text,
    Markdown,
    Json,
    Jsonl,
}

impl StructuredOutputFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Markdown => "markdown",
            Self::Json => "json",
            Self::Jsonl => "jsonl",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredTranscriptMode {
    Full,
    Lite,
    Log,
}

impl StructuredTranscriptMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Lite => "lite",
            Self::Log => "log",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadModelLimitError {
    pub event_id: Uuid,
    pub actual_bytes: usize,
    pub maximum_bytes: usize,
}

impl fmt::Display for ReadModelLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "structured read model through event {} requires {} bytes; the limit is {} bytes",
            self.event_id, self.actual_bytes, self.maximum_bytes
        )
    }
}

impl std::error::Error for ReadModelLimitError {}

pub fn session_transcript_read_model(
    session: &SessionRecord,
    mode: StructuredTranscriptMode,
    format: StructuredOutputFormat,
    rendered: Vec<Value>,
    truncated: bool,
    max_events: Option<usize>,
) -> Value {
    let mut value = compact_json(json!({
        "schema_version": 1,
        "target": "session",
        "payload_type": "session_transcript",
        "ctx_session_id": session.session_id.as_uuid(),
        "provider": session.provider,
        "provider_key": session.provider_key,
        "source_id": session.source_id,
        "provider_session_id": session.provider_session_id,
        "mode": mode.as_str(),
        "format": format.as_str(),
        "session": {
            "id": session.session_id.as_uuid(),
            "item_id": session.session_id.as_uuid(),
            "record_type": "session",
            "ctx_session_id": session.session_id.as_uuid(),
            "provider": session.provider,
            "provider_key": session.provider_key,
            "source_id": session.source_id,
            "provider_session_id": session.provider_session_id,
            "source_format": session.source_format,
            "parent_ctx_session_id": session.parent_session_id.map(|id| id.as_uuid()),
            "root_ctx_session_id": session.root_session_id.map(|id| id.as_uuid()),
            "session_relationship": session.session_relationship,
            "agent_scope": session.agent_scope,
        },
        "truncated": truncated.then(|| json!({
            "events": true,
            "max_events": max_events,
        })),
    }));
    value["events"] = Value::Array(rendered);
    value
}

pub fn event_window_read_model(
    selected: &CoreEventRecord,
    events: &[CoreEventRecord],
    format: StructuredOutputFormat,
    output_limit_bytes: usize,
) -> Result<Value> {
    let references = events.iter().collect::<Vec<_>>();
    let rendered = render_event_read_model_values(&references, output_limit_bytes)?;
    event_window_value(selected, format, rendered)
}

pub fn event_window_with_lineage_read_model(
    selected: &CoreEventRecord,
    events: &[CoreEventRecord],
    copied_lineage: &CopiedEventLineage,
    format: StructuredOutputFormat,
    output_limit_bytes: usize,
) -> Result<Value> {
    let mut value = event_window_read_model(selected, events, format, output_limit_bytes)?;
    value["copied_lineage"] = copied_lineage_read_model(copied_lineage)?;
    Ok(value)
}

pub fn event_window_value(
    selected: &CoreEventRecord,
    format: StructuredOutputFormat,
    rendered: Vec<Value>,
) -> Result<Value> {
    let selected_value = rendered
        .iter()
        .find(|event| {
            event["ctx_event_id"].as_str() == Some(&selected.event_id.as_uuid().to_string())
        })
        .cloned()
        .ok_or_else(|| anyhow!("selected event is absent from its pinned Core event window"))?;
    let mut value = compact_json(json!({
        "schema_version": 1,
        "target": "event",
        "payload_type": "event_window",
        "ctx_event_id": selected.event_id.as_uuid(),
        "ctx_session_id": selected.session_id.as_uuid(),
        "format": format.as_str(),
    }));
    value["event"] = selected_value;
    value["events"] = Value::Array(rendered);
    Ok(value)
}

pub fn render_event_read_model_values(
    events: &[&CoreEventRecord],
    output_limit_bytes: usize,
) -> Result<Vec<Value>> {
    let mut rendered = Vec::with_capacity(events.len());
    let mut serialized_event_bytes = 2_usize;
    for event in events {
        let content = &event.core_record.content;
        let content_bytes = serialized_json_bytes(&content.normalized_body)?
            .saturating_add(serialized_json_bytes(&content.structured_content)?)
            .saturating_add(serialized_json_bytes(&content.activity)?);
        enforce_read_model_limit(
            serialized_event_bytes.saturating_add(content_bytes),
            output_limit_bytes,
            event.event_id.as_uuid(),
        )?;
        let value = render_show_event_read_model(event);
        serialized_event_bytes = serialized_event_bytes
            .saturating_add(usize::from(!rendered.is_empty()))
            .saturating_add(serialized_json_bytes(&value)?);
        enforce_read_model_limit(
            serialized_event_bytes,
            output_limit_bytes,
            event.event_id.as_uuid(),
        )?;
        rendered.push(value);
    }
    Ok(rendered)
}

pub fn render_show_event_read_model(event: &CoreEventRecord) -> Value {
    let content = &event.core_record.content;
    let (policy_status, policy_reason, complete) = match &content.policy_status {
        CoreContentPolicyStatus::Selected => ("selected", None, true),
        CoreContentPolicyStatus::Redacted { reason } => ("redacted", Some(reason.as_str()), false),
        CoreContentPolicyStatus::Omitted { reason } => ("omitted", Some(reason.as_str()), false),
    };
    let (provider_key, source_id) = event
        .custom_source_identity()
        .map_or((None, None), |(provider_key, source_id)| {
            (Some(provider_key), Some(source_id))
        });
    let mut rendered = compact_json(json!({
        "ctx_event_id": event.event_id.as_uuid(),
        "item_id": event.event_id.as_uuid(),
        "record_type": "event",
        "ctx_session_id": event.session_id.as_uuid(),
        "provider": event.provider,
        "provider_key": provider_key,
        "source_id": source_id,
        "provider_session_id": event.provider_session_id,
        "source_format": event.source_format,
        "parent_ctx_session_id": event.parent_session_id.map(|id| id.as_uuid()),
        "root_ctx_session_id": event.root_session_id.map(|id| id.as_uuid()),
        "session_relationship": event.session_relationship,
        "event_copy": event_copy_json(event.event_copy.as_ref()),
        "agent_scope": event.agent_scope,
        "sequence": event.event_sequence,
        "event_type": event.event_type,
        "role": event.role,
        "occurred_at": timestamp_json(event.occurred_at_unix_ms),
        "text": content.normalized_body.as_deref(),
        "structured_content": content.structured_content.as_ref(),
        "content": {
            "complete": complete,
            "policy_status": policy_status,
            "policy_reason": policy_reason,
        },
    }));
    if let Some(activity) = &content.activity {
        rendered["activity"] = json!(activity);
    }
    rendered
}

pub struct SessionPageReadModel {
    pub events: Vec<Value>,
    pub has_more: bool,
    pub next_cursor: Option<SessionEventCursor>,
}

pub fn retain_structured_session_page(
    events: Vec<ShowSessionEvent>,
    query_has_more: bool,
    output_limit_bytes: usize,
) -> Result<SessionPageReadModel> {
    let mut selected = Vec::with_capacity(events.len());
    let mut serialized_event_bytes = 2_usize;
    let mut continuation = None;
    let mut output_truncated = false;
    for selected_event in events {
        let event_id = selected_event.event.event_id.as_uuid();
        let value = render_show_event_read_model(&selected_event.event);
        let candidate_bytes = serialized_event_bytes
            .saturating_add(usize::from(!selected.is_empty()))
            .saturating_add(serialized_json_bytes(&value)?);
        if candidate_bytes > output_limit_bytes {
            if selected.is_empty() {
                enforce_read_model_limit(candidate_bytes, output_limit_bytes, event_id)?;
            }
            output_truncated = true;
            break;
        }
        serialized_event_bytes = candidate_bytes;
        continuation = Some(selected_event.cursor_after);
        selected.push(value);
    }
    let has_more = query_has_more || output_truncated;
    if !has_more {
        continuation = None;
    }
    Ok(SessionPageReadModel {
        events: selected,
        has_more,
        next_cursor: continuation,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn paginated_session_transcript_read_model(
    session: &SessionRecord,
    mode: StructuredTranscriptMode,
    format: StructuredOutputFormat,
    events: Vec<Value>,
    limit: usize,
    has_more: bool,
    next_cursor: Option<&SessionEventCursor>,
) -> Result<Value> {
    let returned = events.len();
    let mut value = session_transcript_read_model(session, mode, format, events, false, None);
    value["pagination"] = compact_json(json!({
        "limit": limit,
        "returned": returned,
        "has_more": has_more,
        "next_cursor": next_cursor.map(encode_session_event_cursor).transpose()?,
    }));
    Ok(value)
}

pub fn encode_session_event_cursor(cursor: &SessionEventCursor) -> Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(cursor)?))
}

pub fn decode_session_event_cursor(encoded: &str) -> Result<SessionEventCursor> {
    let invalid = || anyhow::Error::new(IndexError::InvalidSessionEventCursorCoordinate);
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| invalid())?;
    serde_json::from_slice(&bytes).map_err(|_| invalid())
}

fn serialized_json_bytes<T: serde::Serialize + ?Sized>(value: &T) -> Result<usize> {
    Ok(serde_json::to_vec(value)?.len())
}

fn enforce_read_model_limit(actual: usize, maximum: usize, event_id: Uuid) -> Result<()> {
    if actual > maximum {
        return Err(anyhow::Error::new(ReadModelLimitError {
            event_id,
            actual_bytes: actual,
            maximum_bytes: maximum,
        }));
    }
    Ok(())
}
