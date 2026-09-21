use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use ctx_history_source_sqlite::sqlite_logical_record_digest_bytes;
use serde_json::{json, Value};

use crate::normalization::{provider_nonnegative_i64_to_u64, provider_required_timestamp_seconds};
use crate::{record_evidence::RecordDigest, Result, HERMES_SQLITE_SOURCE_FORMAT};
use ctx_history_capture_model::normalization::{
    provider_json_text, provider_role, provider_value_text,
};

mod layout;
pub mod source_backed;
mod sqlite;

#[cfg(feature = "test-support")]
pub(crate) use sqlite::{
    exact_message_query_counters, exact_message_spool_counters, reset_exact_message_query_counters,
};

use self::layout::{HermesMessageRow, HermesSqliteValue};

pub(super) const HERMES_CAPTURE_REVISION: u32 = 2;
pub(super) const HERMES_POLICY_REVISION: u32 = 6;

#[derive(Clone, Debug)]
#[allow(
    dead_code,
    reason = "provider-native evidence retained for schema compatibility"
)]
pub(super) struct HermesNativeEvent {
    pub(super) provider_event_index: u64,
    pub(super) cursor: String,
    pub(super) event_type: EventType,
    pub(super) role: Option<EventRole>,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) payload: Value,
    pub(super) metadata: Value,
    pub(super) complete_text: String,
}

pub(in crate::provider) fn hermes_native_event(
    row: &HermesMessageRow,
    source_record_ordinal: u64,
) -> Result<HermesNativeEvent> {
    let content = hermes_decode_content(row.content.as_deref());
    let text = hermes_normalized_result_content(&row.role, &content)
        .or_else(|| provider_value_text(&content))
        .unwrap_or_else(|| {
            row.tool_name
                .as_ref()
                .map(|name| format!("tool: {name}"))
                .unwrap_or_else(|| format!("Hermes {}", row.role))
        });
    let occurred_at =
        provider_required_timestamp_seconds(row.timestamp, "Hermes message timestamp")?;
    let event_type = hermes_event_type(row);
    let body = json!({
            "message_id": row.id,
            "role": row.role,
            "content": content,
            "tool_call_id": row.tool_call_id,
            "tool_calls": row.tool_calls.as_deref().map(provider_json_text),
            "tool_name": row.tool_name,
            "status": row.finish_reason,
            "reasoning": row.reasoning,
            "reasoning_content": row.reasoning_content,
            "reasoning_details": row.reasoning_details.as_deref().map(provider_json_text),
            "codex_reasoning_items": row.codex_reasoning_items.as_deref().map(provider_json_text),
            "codex_message_items": row.codex_message_items.as_deref().map(provider_json_text),
    });
    Ok(HermesNativeEvent {
        provider_event_index: provider_nonnegative_i64_to_u64(row.id, "Hermes message id")?,
        cursor: format!("messages:id:{}", row.id),
        event_type,
        role: Some(provider_role(Some(&row.role))),
        occurred_at,
        payload: json!({
            "text": text,
            "source_format": HERMES_SQLITE_SOURCE_FORMAT,
            "body": body,
        }),
        metadata: json!({
            "source": "hermes_state_db",
            "source_format": HERMES_SQLITE_SOURCE_FORMAT,
            "message_id": row.id,
            "platform_message_id": row.platform_message_id,
            "token_count": row.token_count,
            "finish_reason": row.finish_reason,
            "observed": row.observed != 0,
            "active": row.active != 0,
            "compacted": row.compacted != 0,
            "source_record_ordinal": source_record_ordinal,
            "source_record_subrecord_index": 0,
        }),
        complete_text: text,
    })
}

/// Hermes rows are digested with the shared provider logical-row function, so
/// the persisted evidence stays byte-comparable with every other SQLite
/// provider rather than depending on a private re-encode.
fn hermes_layout_record_digest(values: &[HermesSqliteValue]) -> RecordDigest {
    RecordDigest::from_sha256(sqlite_logical_record_digest_bytes(values))
}

pub(crate) fn hermes_decode_content(raw: Option<&str>) -> Value {
    let Some(raw) = raw else {
        return Value::Null;
    };
    if let Some(json) = raw.strip_prefix("\0json:") {
        return provider_json_text(json);
    }
    Value::String(raw.to_owned())
}

/// Returns the complete normalized result body for one Hermes tool-role row.
///
/// Hermes owns the `content` column as the result body, so no nested field-name
/// search is needed. The caller owns any byte bound.
pub(crate) fn hermes_normalized_result_content(role: &str, content: &Value) -> Option<String> {
    (role == "tool")
        .then(|| provider_value_text(content))
        .flatten()
}

fn hermes_event_type(row: &HermesMessageRow) -> EventType {
    if row.role == "tool" {
        EventType::ToolOutput
    } else if row
        .tool_calls
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || row
            .tool_name
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    {
        EventType::ToolCall
    } else {
        EventType::Message
    }
}

#[cfg(test)]
mod tests;
