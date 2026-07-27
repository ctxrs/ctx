use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventRole, EventType, ProviderEventEnvelope};
use serde_json::{json, Value};

use crate::provider::normalization::{native_event, NativeEventDraft};
use crate::KIRO_SQLITE_SOURCE_FORMAT;

use super::history::KiroConversationRow;

#[allow(clippy::too_many_arguments)]
pub(crate) fn kiro_event(
    row: &KiroConversationRow,
    provider_session_id: &str,
    history_index: usize,
    part_index: u64,
    event_type: EventType,
    role: EventRole,
    occurred_at: DateTime<Utc>,
    text: String,
    entry: &Value,
    tool_uses: Option<Value>,
) -> ProviderEventEnvelope {
    let provider_event_index = history_index
        .saturating_mul(2)
        .saturating_add(part_index as usize) as u64;
    let role_name = match role {
        EventRole::User => "user",
        EventRole::Assistant => "assistant",
        EventRole::System => "system",
        EventRole::Tool => "tool",
        EventRole::Unknown => "unknown",
    };
    native_event(NativeEventDraft {
        provider: CaptureProvider::KiroCli,
        source_format: KIRO_SQLITE_SOURCE_FORMAT,
        provider_session_id: provider_session_id.to_owned(),
        provider_event_index,
        provider_event_hash: Some(format!(
            "{}:{}:{}:{role_name}",
            row.table, provider_session_id, history_index
        )),
        cursor: format!(
            "{}:{}:history:{}:{role_name}",
            row.table, provider_session_id, history_index
        ),
        event_type,
        role: Some(role),
        occurred_at,
        text,
        body: json!({
            "table": row.table,
            "key": row.key,
            "conversation_id": provider_session_id,
            "history_index": history_index,
            "role": role_name,
            "entry": entry,
            "tool_uses": tool_uses,
        }),
        metadata: json!({
            "source": row.table,
            "source_format": KIRO_SQLITE_SOURCE_FORMAT,
            "key": row.key,
            "conversation_id": provider_session_id,
            "history_index": history_index,
            "rowid": row.rowid,
        }),
    })
}
