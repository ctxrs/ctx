//! Verified message-body recovery from structured provider snapshots.

use std::{path::Path, time::Instant};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventType};
use serde_json::Value;

use crate::complete_content::{
    CompleteContentError, CompleteContentErrorKind, CompleteContentHashAuthority,
    CompleteContentResolver, CompleteContentSourceFamily, CompleteMessage, CompleteMessageRequest,
    SourceVerification, COMPLETE_CONTENT_MAX_BODY_BYTES,
};
use crate::compute_payload_hash;
use crate::provider::normalization::{provider_block_text, provider_message_id};
use crate::provider::providers::{
    auggie::{auggie_request_text, auggie_response_text},
    codebuddy::{codebuddy_decoded_message, codebuddy_message_text},
    continue_cli::{continue_history_item_event, continue_history_item_text},
    openhands::decode_openhands_event_value,
    task_json::{
        task_json_event, task_json_event_text, task_json_event_type, task_json_provider,
        TaskJsonEventInput,
    },
};

use super::{
    contracts::{structured_source_format, StructuredLocator},
    source_access::{
        error, parse_bounded_json, task_json_records, validate_json_shape,
        whole_json_path_candidate, StructuredSourceSnapshot, TaskJsonRecord,
    },
    verification::{digest_bytes, validate_request_batch, ResolutionBudget},
    StructuredCompleteContentResolver,
};

impl CompleteContentResolver for StructuredCompleteContentResolver {
    fn family(&self) -> CompleteContentSourceFamily {
        CompleteContentSourceFamily::Structured
    }

    fn supports(&self, provider: CaptureProvider, source_format: &str) -> bool {
        structured_source_format(provider, source_format)
    }

    fn resolve(
        &self,
        requests: &[CompleteMessageRequest],
    ) -> std::result::Result<Vec<CompleteMessage>, CompleteContentError> {
        let Some(first) = requests.first() else {
            return Ok(Vec::new());
        };
        if !CompleteContentResolver::supports(self, first.provider, &first.source_format) {
            return Err(error(first, CompleteContentErrorKind::HydrationUnsupported));
        }
        validate_request_batch(requests)?;
        let locators = requests
            .iter()
            .map(StructuredLocator::for_request)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let deadline = Instant::now() + self.bounds.deadline;
        let mut budget = ResolutionBudget::new(self.bounds, deadline);
        let snapshot = first.source_access.structured_snapshot(first.event_id)?;
        snapshot.validate_bounds(self.bounds, first)?;
        let mut output = Vec::with_capacity(requests.len());
        for (request, locator) in requests.iter().zip(&locators) {
            budget.check(request)?;
            let resolved = resolve_one(request, locator, snapshot, &mut budget)?;
            output.push(CompleteMessage::verified(
                request,
                resolved.text,
                SourceVerification::VERIFIED,
            )?);
        }
        Ok(output)
    }
}
#[derive(Debug)]
struct ResolvedMessage {
    text: String,
    provider_hash: Option<String>,
    fallback_hash: Option<String>,
    native_id: String,
}

fn resolve_one(
    request: &CompleteMessageRequest,
    locator: &StructuredLocator,
    snapshot: &StructuredSourceSnapshot,
    budget: &mut ResolutionBudget,
) -> std::result::Result<ResolvedMessage, CompleteContentError> {
    debug_assert_eq!(request.provider, locator.provider);
    let resolved = match request.provider {
        CaptureProvider::Auggie => {
            resolve_whole_json(request, locator, snapshot, budget, auggie_message)
        }
        CaptureProvider::Continue => {
            resolve_whole_json(request, locator, snapshot, budget, continue_message)
        }
        CaptureProvider::RovoDev => {
            resolve_whole_json(request, locator, snapshot, budget, rovodev_message)
        }
        CaptureProvider::OpenHands => resolve_openhands(request, locator, snapshot, budget),
        CaptureProvider::Cline | CaptureProvider::RooCode => {
            resolve_task_json(request, locator, snapshot, budget)
        }
        CaptureProvider::CodeBuddy => resolve_codebuddy(request, locator, snapshot, budget),
        _ => Err(error(
            request,
            CompleteContentErrorKind::HydrationUnsupported,
        )),
    }?;
    if resolved.native_id != locator.native_id {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let observed_hash = match request.expected_hash_authority {
        CompleteContentHashAuthority::ProviderSupplied => resolved.provider_hash.as_ref(),
        CompleteContentHashAuthority::NormalizedPayloadFallback => resolved.fallback_hash.as_ref(),
    };
    if observed_hash.map(String::as_str) != Some(request.expected_provider_event_hash.as_str()) {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    Ok(resolved)
}

type WholeJsonExtractor = fn(
    &CompleteMessageRequest,
    &StructuredLocator,
    &Path,
    &Value,
) -> std::result::Result<ResolvedMessage, CompleteContentError>;

fn resolve_whole_json(
    request: &CompleteMessageRequest,
    locator: &StructuredLocator,
    snapshot: &StructuredSourceSnapshot,
    budget: &mut ResolutionBudget,
    extract: WholeJsonExtractor,
) -> std::result::Result<ResolvedMessage, CompleteContentError> {
    let mut saw_candidate = false;
    for file in snapshot.files() {
        let path = file.path();
        if !whole_json_path_candidate(request.provider, path) {
            continue;
        }
        saw_candidate = true;
        let bytes = file.bytes();
        if bytes.len() > COMPLETE_CONTENT_MAX_BODY_BYTES {
            return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
        }
        if digest_bytes(bytes) != locator.record_digest.as_str() {
            continue;
        }
        let value = parse_bounded_json(request, bytes, budget)?;
        return extract(request, locator, path, &value);
    }
    Err(error(
        request,
        if saw_candidate {
            CompleteContentErrorKind::SourceChanged
        } else {
            CompleteContentErrorKind::SourceMissing
        },
    ))
}

fn auggie_message(
    request: &CompleteMessageRequest,
    locator: &StructuredLocator,
    _path: &Path,
    session: &Value,
) -> std::result::Result<ResolvedMessage, CompleteContentError> {
    if locator.ordinal != 0 {
        return Err(error(
            request,
            CompleteContentErrorKind::SourceRecordMissing,
        ));
    }
    let history = session
        .get("chatHistory")
        .or_else(|| session.get("chat_history"))
        .and_then(Value::as_array)
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let mut current = 0_u32;
    for (chat_index, entry) in history.iter().enumerate() {
        let exchange = entry.get("exchange").unwrap_or(entry);
        for (label, text) in [
            ("request", auggie_request_text(exchange)),
            ("response", auggie_response_text(exchange)),
        ] {
            let Some(text) = text else { continue };
            if current == locator.subrecord {
                let native_id = exchange
                    .get("request_id")
                    .or_else(|| exchange.get("requestId"))
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| format!("{value}:{label}"))
                    .unwrap_or_else(|| format!("chat-{chat_index}:{label}"));
                return Ok(ResolvedMessage {
                    text,
                    provider_hash: Some(native_id.clone()),
                    fallback_hash: None,
                    native_id,
                });
            }
            current = current.saturating_add(1);
        }
    }
    Err(error(
        request,
        CompleteContentErrorKind::SourceRecordMissing,
    ))
}

fn continue_message(
    request: &CompleteMessageRequest,
    locator: &StructuredLocator,
    _path: &Path,
    session: &Value,
) -> std::result::Result<ResolvedMessage, CompleteContentError> {
    if locator.ordinal != 0 {
        return Err(error(
            request,
            CompleteContentErrorKind::SourceRecordMissing,
        ));
    }
    let history = session
        .get("history")
        .and_then(Value::as_array)
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let provider_session_id = request
        .provider_session_id
        .as_deref()
        .ok_or_else(|| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
    let (history_item_index, item, provider_hash) = history
        .iter()
        .enumerate()
        .find_map(|(history_item_index, item)| {
            let provider_hash = item
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned);
            let native_id = provider_hash
                .clone()
                .unwrap_or_else(|| format!("history:{provider_session_id}:{history_item_index}"));
            (native_id == locator.native_id).then_some((history_item_index, item, provider_hash))
        })
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let text = continue_history_item_text(item)
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let provider_event_index = u64::try_from(history_item_index)
        .ok()
        .and_then(|index| index.checked_add(1))
        .ok_or_else(|| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
    let event = continue_history_item_event(
        provider_session_id,
        item,
        provider_event_index,
        DateTime::<Utc>::UNIX_EPOCH,
    );
    if event.event_type != EventType::Message {
        return Err(error(
            request,
            CompleteContentErrorKind::HydrationUnsupported,
        ));
    }
    let fallback_hash = compute_payload_hash(&event.payload).ok();
    let native_id = provider_hash
        .clone()
        .unwrap_or_else(|| format!("history:{provider_session_id}:{history_item_index}"));
    Ok(ResolvedMessage {
        text,
        provider_hash,
        fallback_hash,
        native_id,
    })
}

fn rovodev_message(
    request: &CompleteMessageRequest,
    locator: &StructuredLocator,
    _path: &Path,
    session: &Value,
) -> std::result::Result<ResolvedMessage, CompleteContentError> {
    if locator.ordinal != 0 {
        return Err(error(
            request,
            CompleteContentErrorKind::SourceRecordMissing,
        ));
    }
    let messages = session
        .get("message_history")
        .or_else(|| session.pointer("/session_context/message_history"))
        .or_else(|| session.get("messages"))
        .or_else(|| session.pointer("/conversation/messages"))
        .and_then(Value::as_array)
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let message = messages
        .get(locator.subrecord as usize)
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let text = provider_block_text(message)
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let native_id = provider_message_id(message, u64::from(locator.subrecord));
    Ok(ResolvedMessage {
        text,
        provider_hash: Some(native_id.clone()),
        fallback_hash: None,
        native_id,
    })
}

fn resolve_openhands(
    request: &CompleteMessageRequest,
    locator: &StructuredLocator,
    snapshot: &StructuredSourceSnapshot,
    budget: &mut ResolutionBudget,
) -> std::result::Result<ResolvedMessage, CompleteContentError> {
    if locator.subrecord != 0 {
        return Err(error(
            request,
            CompleteContentErrorKind::SourceRecordMissing,
        ));
    }
    let mut saw_candidate = false;
    for file in snapshot.files() {
        let path = file.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json")
            || !path
                .components()
                .any(|part| part.as_os_str() == "v1_conversations")
        {
            continue;
        }
        saw_candidate = true;
        let bytes = file.bytes();
        if bytes.len() > COMPLETE_CONTENT_MAX_BODY_BYTES {
            return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
        }
        if digest_bytes(bytes) != locator.record_digest.as_str() {
            continue;
        }
        let value = serde_json::from_slice::<Value>(bytes)
            .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
        validate_json_shape(request, &value, budget, 0)?;
        let decoded = decode_openhands_event_value(path, value).map_err(|decode_error| {
            error(
                request,
                if decode_error.is_too_large() {
                    CompleteContentErrorKind::ContentTooLarge
                } else {
                    CompleteContentErrorKind::ContentVerificationFailed
                },
            )
        })?;
        if decoded.event_type() != EventType::Message {
            return Err(error(
                request,
                CompleteContentErrorKind::HydrationUnsupported,
            ));
        }
        let native_id = decoded.event_id().to_owned();
        return Ok(ResolvedMessage {
            text: decoded.text().to_owned(),
            provider_hash: Some(native_id.clone()),
            fallback_hash: None,
            native_id,
        });
    }
    Err(error(
        request,
        if saw_candidate {
            CompleteContentErrorKind::SourceChanged
        } else {
            CompleteContentErrorKind::SourceMissing
        },
    ))
}

fn resolve_codebuddy(
    request: &CompleteMessageRequest,
    locator: &StructuredLocator,
    snapshot: &StructuredSourceSnapshot,
    budget: &mut ResolutionBudget,
) -> std::result::Result<ResolvedMessage, CompleteContentError> {
    if locator.subrecord != 0 {
        return Err(error(
            request,
            CompleteContentErrorKind::SourceRecordMissing,
        ));
    }
    let mut saw_candidate = false;
    for file in snapshot.files() {
        let path = file.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json")
            || path
                .parent()
                .and_then(Path::file_name)
                .and_then(|v| v.to_str())
                != Some("messages")
        {
            continue;
        }
        saw_candidate = true;
        let bytes = file.bytes();
        if digest_bytes(bytes) != locator.record_digest.as_str() {
            continue;
        }
        let raw = parse_bounded_json(request, bytes, budget)?;
        let decoded = codebuddy_decoded_message(&raw);
        let text = codebuddy_message_text(&decoded, &raw);
        let message_id = path
            .file_stem()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
        let session_id = request
            .provider_session_id
            .as_deref()
            .ok_or_else(|| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
        let native_id = format!("{session_id}:{message_id}");
        return Ok(ResolvedMessage {
            text,
            provider_hash: Some(native_id.clone()),
            fallback_hash: None,
            native_id,
        });
    }
    Err(error(
        request,
        if saw_candidate {
            CompleteContentErrorKind::SourceChanged
        } else {
            CompleteContentErrorKind::SourceMissing
        },
    ))
}

fn resolve_task_json(
    request: &CompleteMessageRequest,
    locator: &StructuredLocator,
    snapshot: &StructuredSourceSnapshot,
    budget: &mut ResolutionBudget,
) -> std::result::Result<ResolvedMessage, CompleteContentError> {
    if locator.subrecord != 0 {
        return Err(error(
            request,
            CompleteContentErrorKind::SourceRecordMissing,
        ));
    }
    let spec = task_json_provider(request.provider);
    let mut saw_candidate = false;
    for file in snapshot.files() {
        let path = file.path();
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let source = if file_name == spec.api_file {
            "api_conversation_history"
        } else if file_name == spec.ui_file {
            "ui_messages"
        } else if spec.fallback_api_file == Some(file_name) {
            "claude_messages"
        } else {
            continue;
        };
        saw_candidate = true;
        let bytes = file.bytes();
        for record in task_json_records(request, bytes, budget)? {
            let TaskJsonRecord {
                native_index,
                bytes: record_bytes,
                value: raw,
            } = record;
            if digest_bytes(record_bytes) != locator.record_digest.as_str() {
                continue;
            }
            let event_type = task_json_event_type(&raw, source);
            if event_type != EventType::Message {
                return Err(error(
                    request,
                    CompleteContentErrorKind::HydrationUnsupported,
                ));
            }
            let text = task_json_event_text(&raw, source, event_type);
            let task_id = request.provider_session_id.as_deref().ok_or_else(|| {
                error(request, CompleteContentErrorKind::ContentVerificationFailed)
            })?;
            let event = task_json_event(
                spec,
                task_id,
                TaskJsonEventInput {
                    source,
                    native_index,
                    raw,
                },
                locator.ordinal as usize,
                DateTime::<Utc>::UNIX_EPOCH,
            );
            let native_id = event.provider_event_hash.clone().ok_or_else(|| {
                error(request, CompleteContentErrorKind::ContentVerificationFailed)
            })?;
            return Ok(ResolvedMessage {
                text,
                provider_hash: Some(native_id.clone()),
                fallback_hash: None,
                native_id,
            });
        }
    }
    Err(error(
        request,
        if saw_candidate {
            CompleteContentErrorKind::SourceChanged
        } else {
            CompleteContentErrorKind::SourceMissing
        },
    ))
}
