use chrono::Duration;
use ctx_history_core::{EventRole, EventType};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    model::{saturating_i64, ParsedAuggieEvent, ParsedAuggieSession, ParsedAuggieSource},
    source::AuggieFileStamp,
};
use crate::{
    provider::providers::auggie::{
        auggie_entry_time, auggie_request_text, auggie_response_text, AuggieSessionData,
    },
    CaptureError, ProviderAdapterContext, Result, MAX_PROVIDER_JSONL_LINE_BYTES,
};

pub(super) fn parse_opened_auggie_source(
    before: AuggieFileStamp,
    context: &ProviderAdapterContext,
) -> Result<ParsedAuggieSource> {
    let max_bytes = u64::try_from(MAX_PROVIDER_JSONL_LINE_BYTES).unwrap_or(u64::MAX);
    if before.len > max_bytes {
        return Err(CaptureError::InvalidPayload(format!(
            "Auggie session JSON exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit"
        )));
    }
    let bytes = before.read_all_bounded(MAX_PROVIDER_JSONL_LINE_BYTES)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != before.len {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    if !before.revalidate()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let root = serde_json::from_slice::<Value>(&bytes).map_err(|error| {
        CaptureError::InvalidPayload(format!("invalid Auggie session JSON: {error}"))
    })?;
    let data = AuggieSessionData::parse(&root, context)?;
    let session = ParsedAuggieSession {
        provider_session_id: data.provider_session_id.clone(),
        parent_provider_session_id: data.parent_provider_session_id.clone(),
        root_provider_session_id: data.root_provider_session_id.clone(),
        cwd: data.cwd.clone(),
    };
    let events = parse_events(&data)?;
    Ok(ParsedAuggieSource {
        stamp: before,
        content_digest: Sha256::digest(&bytes).into(),
        session,
        events,
    })
}

fn parse_events(data: &AuggieSessionData<'_>) -> Result<Vec<ParsedAuggieEvent>> {
    let mut events = Vec::new();
    let mut provider_event_index = 0_u64;
    for (chat_index, entry) in data.chat_history.iter().enumerate() {
        let exchange = entry.get("exchange").unwrap_or(entry);
        let base_time = auggie_entry_time(entry, Some(exchange)).unwrap_or_else(|| {
            data.started_at + Duration::milliseconds(saturating_i64(chat_index).saturating_mul(2))
        });
        for (role, message_kind, occurred_at, text) in [
            (
                EventRole::User,
                "request",
                base_time,
                auggie_request_text(exchange),
            ),
            (
                EventRole::Assistant,
                "response",
                base_time + Duration::milliseconds(1),
                auggie_response_text(exchange),
            ),
        ] {
            let Some(text) = text else {
                continue;
            };
            let native_event_id = exchange
                .get("request_id")
                .or_else(|| exchange.get("requestId"))
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .map(|id| format!("{id}:{message_kind}"));
            let provider_event_hash = native_event_id
                .clone()
                .unwrap_or_else(|| format!("chat-{chat_index}:{message_kind}"));
            events.push(ParsedAuggieEvent {
                provider_event_index,
                provider_event_hash,
                event_type: EventType::Message,
                role,
                occurred_at,
                text,
                chat_index,
                message_kind,
                native_event_id,
            });
            provider_event_index =
                provider_event_index
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Auggie provider event index overflowed",
                    ))?;
        }
    }
    Ok(events)
}
