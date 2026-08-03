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
    let parsed_events = parse_events(&data)?;
    Ok(ParsedAuggieSource {
        stamp: before,
        content_digest: Sha256::digest(&bytes).into(),
        session,
        events: parsed_events.events,
        complete_records: parsed_events.complete_records,
        ignored_records: parsed_events.ignored_records,
        rejected_records: parsed_events.rejected_records,
    })
}

struct ParsedAuggieEvents {
    events: Vec<ParsedAuggieEvent>,
    complete_records: u64,
    ignored_records: u64,
    rejected_records: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AuggieCandidate {
    Absent,
    Retained(String),
    Ignored,
    Rejected,
}

fn parse_events(data: &AuggieSessionData<'_>) -> Result<ParsedAuggieEvents> {
    let mut events = Vec::new();
    let mut provider_event_index = 0_u64;
    let mut complete_records = 0_u64;
    let mut ignored_records = 0_u64;
    let mut rejected_records = 0_u64;
    for (chat_index, entry) in data.chat_history.iter().enumerate() {
        let exchange = entry.get("exchange").unwrap_or(entry);
        let base_time = auggie_entry_time(entry, Some(exchange)).unwrap_or_else(|| {
            data.started_at + Duration::milliseconds(saturating_i64(chat_index).saturating_mul(2))
        });
        for (role, message_kind, occurred_at, candidate) in [
            (
                EventRole::User,
                "request",
                base_time,
                auggie_candidate(
                    exchange,
                    &["request_message", "requestMessage"],
                    &["request_nodes", "requestNodes"],
                    auggie_request_text,
                ),
            ),
            (
                EventRole::Assistant,
                "response",
                base_time + Duration::milliseconds(1),
                auggie_candidate(
                    exchange,
                    &["response_text", "responseText"],
                    &["response_nodes", "responseNodes"],
                    auggie_response_text,
                ),
            ),
        ] {
            let text = match candidate {
                AuggieCandidate::Absent => continue,
                AuggieCandidate::Retained(text) => text,
                AuggieCandidate::Ignored => {
                    complete_records = complete_records.saturating_add(1);
                    ignored_records = ignored_records.saturating_add(1);
                    continue;
                }
                AuggieCandidate::Rejected => {
                    complete_records = complete_records.saturating_add(1);
                    rejected_records = rejected_records.saturating_add(1);
                    continue;
                }
            };
            complete_records = complete_records.saturating_add(1);
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
    Ok(ParsedAuggieEvents {
        events,
        complete_records,
        ignored_records,
        rejected_records,
    })
}

fn auggie_candidate(
    exchange: &Value,
    text_fields: &[&str],
    node_fields: &[&str],
    project: fn(&Value) -> Option<String>,
) -> AuggieCandidate {
    if let Some(text) = project(exchange) {
        return AuggieCandidate::Retained(text);
    }
    let fields = text_fields
        .iter()
        .chain(node_fields)
        .filter_map(|field| exchange.get(*field))
        .collect::<Vec<_>>();
    if fields.is_empty() {
        return AuggieCandidate::Absent;
    }
    if fields
        .iter()
        .all(|value| value.as_str().is_some_and(|text| text.trim().is_empty()) || value.is_array())
    {
        AuggieCandidate::Ignored
    } else {
        AuggieCandidate::Rejected
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use serde_json::json;

    use super::*;

    #[test]
    fn unknown_and_malformed_candidates_remain_in_complete_accounting() {
        let chat_history = vec![
            json!({
                "exchange": {
                    "request_nodes": [{
                        "type": 71,
                        "text_node": {"content": "unknown body must not enter Core"}
                    }]
                }
            }),
            json!({"exchange": {"response_nodes": {"future": true}}}),
        ];
        let data = AuggieSessionData {
            provider_session_id: "auggie-accounting".to_owned(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            chat_history: &chat_history,
            started_at: DateTime::<Utc>::UNIX_EPOCH,
            cwd: None,
        };

        let parsed = parse_events(&data).unwrap();
        assert!(parsed.events.is_empty());
        assert_eq!(parsed.complete_records, 2);
        assert_eq!(parsed.ignored_records, 1);
        assert_eq!(parsed.rejected_records, 1);
    }
}
