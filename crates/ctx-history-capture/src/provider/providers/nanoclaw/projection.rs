use chrono::Utc;
use ctx_history_core::{EventRole, EventType};
use serde_json::{json, Value};

use crate::provider::normalization::{
    provider_capped_json, provider_json_text, provider_policy_body, provider_policy_event_text,
    provider_result_identifier_evidence, provider_result_outcome_evidence,
    provider_timestamp_millis, provider_value_text, text_id_index,
};
use crate::{fnv1a64, NANOCLAW_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS};

use super::rows::{NanoClawMessageRow, NanoClawSessionRow};

#[derive(Debug)]
pub(crate) struct NanoClawCoreEvent {
    pub(crate) provider_event_index: u64,
    pub(crate) provider_event_hash: String,
    pub(crate) cursor: String,
    pub(crate) event_type: EventType,
    pub(crate) role: Option<EventRole>,
    pub(crate) occurred_at: chrono::DateTime<Utc>,
    pub(crate) payload: Value,
    pub(crate) metadata: Value,
}

/// Provider-owned normalization shared by NativePath Core and exact
/// complete-content hydration.
pub(super) fn nanoclaw_core_event(
    session: &NanoClawSessionRow,
    message: &NanoClawMessageRow,
    seq: Option<u64>,
    fallback: chrono::DateTime<Utc>,
) -> (NanoClawCoreEvent, String) {
    let occurred_at = provider_timestamp_millis(message.timestamp, fallback);
    let content = message
        .content
        .as_deref()
        .map(provider_json_text)
        .unwrap_or(Value::Null);
    let text = provider_value_text(&content).unwrap_or_else(|| {
        format!(
            "NanoClaw {}",
            message.kind.as_deref().unwrap_or(message.source)
        )
    });
    let event_index = nanoclaw_event_index(message, seq);
    let role = if message.source == "inbound" {
        Some(EventRole::User)
    } else {
        Some(EventRole::Assistant)
    };
    let event_type = EventType::Message;
    let body = json!({
        "message_id": message.id,
        "seq": message.seq,
        "kind": message.kind,
        "content": content,
        "status": message.status,
        "in_reply_to": message.in_reply_to,
        "platform_id": message.platform_id,
        "channel_type": message.channel_type,
        "thread_id": message.thread_id,
        "trigger": message.trigger,
        "source_session_id": message.source_session_id,
        "on_wake": message.on_wake,
    });
    let retained_text = provider_policy_event_text(event_type, &text, &body);
    let retained_body = provider_policy_body(event_type, &body);
    let result_evidence = provider_result_identifier_evidence(event_type, &text, &body);
    let result_outcome = provider_result_outcome_evidence(event_type, &body);
    let event = NanoClawCoreEvent {
        provider_event_index: event_index,
        provider_event_hash: format!("{}:{}", message.source, message.id),
        cursor: format!(
            "{}:{}:{}",
            message.source,
            session.id,
            message.seq.unwrap_or_default()
        ),
        event_type,
        role,
        occurred_at,
        payload: json!({
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": result_evidence,
            "result_outcome": result_outcome,
            "source_format": NANOCLAW_SOURCE_FORMAT,
            "body": provider_capped_json(&retained_body, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": format!("nanoclaw_{}", message.source),
            "source_format": NANOCLAW_SOURCE_FORMAT,
            "message_id": message.id,
            "seq": message.seq,
        }),
    };
    (event, text)
}

pub(super) fn nanoclaw_event_index(message: &NanoClawMessageRow, seq: Option<u64>) -> u64 {
    if let Some(seq) = seq {
        let source_bucket = if message.source == "outbound" {
            500_000
        } else {
            0
        };
        let row_bucket = fnv1a64(format!("{}:{}", message.source, message.id).as_bytes()) % 500_000;
        return seq
            .saturating_mul(1_000_000)
            .saturating_add(source_bucket)
            .saturating_add(row_bucket);
    }
    text_id_index(&format!("{}:{}", message.source, message.id), 2_000_000_000)
}
