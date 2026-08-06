use chrono::{DateTime, SecondsFormat, Utc};
use ctx_history_core::CoreContentPolicyStatus;
use ctx_history_index::CoreEventRecord;
use serde_json::{json, Value};

use super::{EventContentProjection, EventQueryError, EVENT_QUERY_SCHEMA_VERSION};

pub(crate) fn render_event(
    event: &CoreEventRecord,
    projection: EventContentProjection,
) -> std::result::Result<Value, EventQueryError> {
    let record = &event.core_record;
    let content = &record.content;
    let (policy_status, policy_reason, complete) = match &content.policy_status {
        CoreContentPolicyStatus::Selected => ("selected", None, true),
        CoreContentPolicyStatus::Redacted { reason } => ("redacted", Some(reason.as_str()), false),
        CoreContentPolicyStatus::Omitted { reason } => ("omitted", Some(reason.as_str()), false),
    };
    let text = (projection != EventContentProjection::None)
        .then_some(content.normalized_body.as_ref())
        .flatten();
    let structured_content = (projection == EventContentProjection::Full)
        .then_some(content.structured_content.as_ref())
        .flatten();
    let mcp_exchange = (projection == EventContentProjection::Full)
        .then_some(content.mcp_exchange.as_ref())
        .flatten();
    let occurred_at = format_timestamp(event.occurred_at_unix_ms);
    let mut rendered = json!({
        "schema_version": EVENT_QUERY_SCHEMA_VERSION,
        "record_version": record.record_version,
        "ctx_event_id": event.event_id.as_uuid(),
        "ctx_source_id": event.source.identity().as_uuid(),
        "ctx_session_id": event.session_id.as_uuid(),
        "parent_ctx_session_id": event.parent_session_id.map(|id| id.as_uuid()),
        "root_ctx_session_id": event.root_session_id.as_uuid(),
        "session_relationship": event.session_relationship,
        "event_origin": crate::commands::source_index::event_origin_json(&event.event_origin),
        "occurred_at": occurred_at,
        "occurred_at_ms": event.occurred_at_unix_ms,
        "sequence": event.event_sequence,
        "provider": event.provider,
        "source_format": event.source_format,
        "source": event.source,
        "provider_session_id": event.provider_session_id,
        "native_event_id": event.native_event_id,
        "branch": event.branch,
        "agent_type": event.agent_type,
        "agent_scope": if event.is_primary { "primary" } else { "subagent" },
        "is_primary": event.is_primary,
        "event_type": event.event_type,
        "role": event.role,
        "workspace": event.workspace,
        "cwd": event.cwd,
        "touched_files": event.touched_files,
        "parser_revision": record.parser_revision,
        "normalization_revision": record.normalization_revision,
        "text": text,
        "structured_content": structured_content,
        "content": {
            "complete": complete,
            "policy_revision": content.policy_revision,
            "policy_status": policy_status,
            "policy_reason": policy_reason,
        },
        "citations": [{
            "target_type": "event",
            "ctx_event_id": event.event_id.as_uuid(),
            "ctx_session_id": event.session_id.as_uuid(),
            "label": event.event_type,
            "time": occurred_at,
            "provider": event.provider,
            "session_id": event.provider_session_id,
            "event_seq": event.event_sequence,
        }],
        "metadata": record.metadata,
        "repository_candidate_evidence": record.repository_candidate_evidence,
        "repository_bindings": record.repository_bindings,
        "repository_abstentions": record.repository_abstentions,
        "repository_file_invocation_evidence": record.repository_file_invocation_evidence,
        "repository_file_observations": record.repository_file_observations,
        "repository_vcs_observations": record.repository_vcs_observations,
        "content_projection": projection.as_str(),
    });
    if let Some(mcp_exchange) = mcp_exchange {
        rendered["mcp_exchange"] = serde_json::to_value(mcp_exchange)?;
    }
    crate::commands::mcp_tool_call::insert_mcp_tool_call(&mut rendered, record);
    Ok(rendered)
}

pub(super) fn format_timestamp(value: Option<i64>) -> Option<String> {
    value
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Millis, true))
}
