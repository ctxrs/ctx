#[cfg(test)]
use std::path::Path;

use ctx_history_core::{
    BatchHydrationRequest, BatchHydrationResult, HydratedProviderRecord, HydrationFailure,
    HydrationFailureKind, LocatorRevisionPolicy, NativeRecordCoordinate, SourceAnchor,
    SourceRecordLocator, TypedKey,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    auggie_source_key, discover_auggie_source_backed_unfenced, owns_auggie_source,
    revalidate_auggie_tree, AuggieHydratedSourceRecord, AuggieSourceBackedError,
    AuggieSourceBackedResult, AuggieSourceBackedRoot,
};
#[cfg(test)]
use crate::CaptureError;
use crate::{
    provider::{
        providers::auggie::{auggie_request_text, auggie_response_text},
        source_backed::hydration_failure,
    },
    MAX_PROVIDER_JSONL_LINE_BYTES,
};

/// Rehydrates one exact message from one explicitly supplied document path.
#[cfg(test)]
pub(super) fn hydrate_auggie_source_backed(
    path: &Path,
    locator: &SourceRecordLocator,
) -> AuggieSourceBackedResult<AuggieHydratedSourceRecord> {
    locator.validate_contract()?;
    let stamp = super::AuggieFileStamp::observe(path)?;
    let (digest, root) = read_auggie_document(&stamp)?;
    drop(stamp);
    hydrate_auggie_value(&root, digest, locator)
}

pub(super) fn hydrate_auggie_group_with_observer(
    root: &AuggieSourceBackedRoot,
    request: &BatchHydrationRequest,
    mut observe_parse: impl FnMut(),
) -> Result<BatchHydrationResult, HydrationFailure> {
    if request.is_empty() {
        return BatchHydrationResult::new(Vec::new())
            .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error));
    }
    let expected_source = request
        .events()
        .first()
        .map(|event| event.locator().source().clone())
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Auggie hydration group was unexpectedly empty",
            )
        })?;
    if request.events().iter().any(|event| {
        !event
            .locator()
            .source()
            .exact_descriptor_eq(&expected_source)
    }) {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Auggie hydration group spans more than one exact source",
        ));
    }
    let inventory = discover_auggie_source_backed_unfenced(root)
        .map_err(|error| hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error))?;
    let tree = inventory.into_complete_tree().ok_or_else(|| {
        hydration_failure(
            HydrationFailureKind::TemporarilyUnavailable,
            "Auggie selected route inventory is temporarily unavailable",
        )
    })?;
    let mut hydrated = None;
    for leaf in &tree.leaves {
        let provider_bytes = {
            #[cfg(test)]
            let open_guard = tree.authority.track_open();
            let stamp = match tree.authority.open_leaf(&leaf.provider_leaf) {
                Ok(stamp) => stamp,
                Err(_) => continue,
            };
            let bytes = match stamp.read_all_bounded(MAX_PROVIDER_JSONL_LINE_BYTES) {
                Ok(bytes) if leaf.provider_leaf.matches(&stamp) => bytes,
                Ok(_) | Err(_) => continue,
            };
            drop(stamp);
            #[cfg(test)]
            drop(open_guard);
            bytes
        };
        observe_parse();
        let root_value = match serde_json::from_slice::<Value>(&provider_bytes) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let Some(native_session_id) = auggie_value_session_id(&root_value) else {
            continue;
        };
        let candidate_source = auggie_source_key(native_session_id)
            .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?;
        if !candidate_source.exact_descriptor_eq(&expected_source) {
            continue;
        }
        if hydrated.is_some() {
            return Err(hydration_failure(
                HydrationFailureKind::StaleSourceEvidence,
                "more than one Auggie document owns the requested source",
            ));
        }
        let document_digest: [u8; 32] = Sha256::digest(&provider_bytes).into();
        let records = request
            .events()
            .iter()
            .map(|event| {
                let record = hydrate_auggie_value(&root_value, document_digest, event.locator())
                    .map_err(auggie_hydration_failure)?;
                Ok(HydratedProviderRecord {
                    event_id: event.event_id(),
                    provider_bytes: record.provider_bytes,
                })
            })
            .collect::<Result<Vec<_>, HydrationFailure>>()?;
        hydrated = Some(records);
    }
    let terminal_tree_fingerprint = revalidate_auggie_tree(&tree)
        .map_err(|error| hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error))?;
    if terminal_tree_fingerprint != tree.tree_fingerprint {
        return Err(hydration_failure(
            HydrationFailureKind::TemporarilyUnavailable,
            "Auggie source tree changed during grouped hydration",
        ));
    }
    let records = hydrated.ok_or_else(|| {
        hydration_failure(
            HydrationFailureKind::MissingRecord,
            "the exact Auggie source document is absent",
        )
    })?;
    BatchHydrationResult::new(records)
        .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))
}

#[cfg(test)]
fn read_auggie_document(
    stamp: &super::AuggieFileStamp,
) -> AuggieSourceBackedResult<([u8; 32], Value)> {
    let maximum = u64::try_from(MAX_PROVIDER_JSONL_LINE_BYTES).unwrap_or(u64::MAX);
    if stamp.len > maximum {
        return Err(CaptureError::InvalidPayload(format!(
            "Auggie session JSON exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit"
        ))
        .into());
    }
    let bytes = stamp.read_all_bounded(MAX_PROVIDER_JSONL_LINE_BYTES)?;
    let digest = Sha256::digest(&bytes).into();
    let root = serde_json::from_slice(&bytes)?;
    Ok((digest, root))
}

fn hydrate_auggie_value(
    root: &Value,
    document_digest: [u8; 32],
    locator: &SourceRecordLocator,
) -> AuggieSourceBackedResult<AuggieHydratedSourceRecord> {
    let (expected_session_id, expected_event_key, chat_index, message_kind, json_pointer) =
        validate_auggie_locator(locator)?;
    if locator.certified_source_revision_digest() != Some(&document_digest) {
        return Err(AuggieSourceBackedError::SourceRevisionChanged);
    }
    if locator.record_digest() != &document_digest {
        return Err(AuggieSourceBackedError::LocatorDigestMismatch);
    }
    if auggie_value_session_id(root) != Some(expected_session_id.as_str()) {
        return Err(AuggieSourceBackedError::LocatorRecordMissing);
    }
    let expected_pointer = auggie_message_pointer(root, chat_index)?;
    if json_pointer != expected_pointer {
        return Err(AuggieSourceBackedError::InvalidLocator);
    }
    let exchange = root
        .pointer(json_pointer)
        .ok_or(AuggieSourceBackedError::LocatorRecordMissing)?;
    let actual_event_key = auggie_native_event_key(exchange, chat_index, message_kind);
    if actual_event_key != expected_event_key {
        return Err(AuggieSourceBackedError::LocatorRecordMissing);
    }
    let decoded_display_text = match message_kind {
        "request" => auggie_request_text(exchange),
        "response" => auggie_response_text(exchange),
        _ => return Err(AuggieSourceBackedError::InvalidLocator),
    }
    .ok_or(AuggieSourceBackedError::LocatorRecordMissing)?;
    Ok(AuggieHydratedSourceRecord {
        provider_bytes: decoded_display_text.as_bytes().to_vec(),
        decoded_display_text,
    })
}

fn auggie_hydration_failure(error: AuggieSourceBackedError) -> HydrationFailure {
    let kind = match &error {
        AuggieSourceBackedError::InvalidLocator
        | AuggieSourceBackedError::Projection(_)
        | AuggieSourceBackedError::Resolver(_) => HydrationFailureKind::InvalidLocator,
        AuggieSourceBackedError::LocatorRecordMissing
        | AuggieSourceBackedError::DuplicateEventIdentity(_)
        | AuggieSourceBackedError::MissingLexicalText => HydrationFailureKind::MissingRecord,
        AuggieSourceBackedError::SourceRevisionChanged
        | AuggieSourceBackedError::LocatorDigestMismatch => {
            HydrationFailureKind::StaleRecordEvidence
        }
        AuggieSourceBackedError::Io(_)
        | AuggieSourceBackedError::Capture(_)
        | AuggieSourceBackedError::Json(_) => HydrationFailureKind::TemporarilyUnavailable,
    };
    hydration_failure(kind, error)
}

fn auggie_value_session_id(root: &Value) -> Option<&str> {
    root.get("sessionId")
        .or_else(|| root.get("session_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn validate_auggie_locator(
    locator: &SourceRecordLocator,
) -> AuggieSourceBackedResult<(String, String, u64, &str, &str)> {
    let source = locator.source();
    if !owns_auggie_source(source)
        || locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
    {
        return Err(AuggieSourceBackedError::InvalidLocator);
    }
    let SourceAnchor::ProviderNative { namespace, key } = source.anchor() else {
        return Err(AuggieSourceBackedError::InvalidLocator);
    };
    let TypedKey::Utf8(native_session_id) = key else {
        return Err(AuggieSourceBackedError::InvalidLocator);
    };
    if namespace != super::AUGGIE_SOURCE_ANCHOR_NAMESPACE {
        return Err(AuggieSourceBackedError::InvalidLocator);
    }
    let NativeRecordCoordinate::Document {
        object_key,
        json_pointer: Some(json_pointer),
    } = locator.coordinate()
    else {
        return Err(AuggieSourceBackedError::InvalidLocator);
    };
    let TypedKey::Composite(parts) = object_key else {
        return Err(AuggieSourceBackedError::InvalidLocator);
    };
    let [TypedKey::Utf8(event_key), TypedKey::U64(chat_index), TypedKey::Utf8(message_kind)] =
        parts.as_slice()
    else {
        return Err(AuggieSourceBackedError::InvalidLocator);
    };
    if !matches!(message_kind.as_str(), "request" | "response") {
        return Err(AuggieSourceBackedError::InvalidLocator);
    }
    Ok((
        native_session_id.clone(),
        event_key.clone(),
        *chat_index,
        message_kind,
        json_pointer,
    ))
}

fn auggie_message_pointer(root: &Value, chat_index: u64) -> AuggieSourceBackedResult<String> {
    let chat_index =
        usize::try_from(chat_index).map_err(|_| AuggieSourceBackedError::InvalidLocator)?;
    let (history_key, entries) =
        if let Some(entries) = root.get("chatHistory").and_then(Value::as_array) {
            ("chatHistory", entries)
        } else if let Some(entries) = root.get("chat_history").and_then(Value::as_array) {
            ("chat_history", entries)
        } else {
            return Err(AuggieSourceBackedError::LocatorRecordMissing);
        };
    let entry = entries
        .get(chat_index)
        .ok_or(AuggieSourceBackedError::LocatorRecordMissing)?;
    Ok(if entry.get("exchange").is_some() {
        format!("/{history_key}/{chat_index}/exchange")
    } else {
        format!("/{history_key}/{chat_index}")
    })
}

fn auggie_native_event_key(exchange: &Value, chat_index: u64, message_kind: &str) -> String {
    exchange
        .get("request_id")
        .or_else(|| exchange.get("requestId"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(|request_id| format!("{request_id}:{message_kind}"))
        .unwrap_or_else(|| format!("chat-{chat_index}:{message_kind}"))
}
