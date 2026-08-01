use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};

use super::history::KiroConversationRow;

pub(super) fn kiro_native_event(
    row: &KiroConversationRow,
    provider_session_id: &str,
    history_index: usize,
    part_index: u64,
    event_type: EventType,
    role: EventRole,
    occurred_at: DateTime<Utc>,
) -> KiroNativeEvent {
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
    KiroNativeEvent {
        provider_event_index,
        cursor: format!(
            "{}:{}:history:{}:{role_name}",
            row.table, provider_session_id, history_index
        ),
        event_type,
        role: Some(role),
        occurred_at,
    }
}

pub(crate) struct KiroNativeEvent {
    pub(crate) provider_event_index: u64,
    pub(crate) cursor: String,
    pub(crate) event_type: EventType,
    pub(crate) role: Option<EventRole>,
    pub(crate) occurred_at: DateTime<Utc>,
}
