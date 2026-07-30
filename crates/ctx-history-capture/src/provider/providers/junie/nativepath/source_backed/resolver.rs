use std::sync::Arc;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, EventHydrationRequest, EventIdentityInput, HydratedProviderRecord,
    HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy, NativeItemKey,
    NativeRecordCoordinate, PositionStability, SourceAnchor, SourceRecordLocator, TypedKey,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    common::io::OpenedProviderSourceFile,
    provider::source_backed::family::jsonl::JsonlFamilyHydrator, CaptureError,
};

use super::super::projection::{
    SourceBackedTarget, MAX_RECORD_SET_ENTRIES, RECORD_SET_DIGEST_DOMAIN,
};
use super::{
    JunieBinding, LOGICAL_EVENT_KIND, NATIVE_EVENT_POSITION_KIND, RECORD_SET_COORDINATE_KIND,
    RELATIVE_EVENTS_FILE, SOURCE_ANCHOR_NAMESPACE, SOURCE_SCHEMA_VARIANT,
    UNAVAILABLE_COORDINATE_NAMESPACE, USER_PROMPT_COORDINATE_KIND,
};
use crate::{
    provider::providers::junie::{
        assistant::{
            junie_buffer_result_text, junie_merge_buffered_agent_event,
            junie_step_output_projection, JunieAssistantBuffer, JunieStepAgg,
        },
        MAX_JUNIE_TRANSIENT_TURN_BYTES,
    },
    JUNIE_SESSION_EVENTS_SOURCE_FORMAT, MAX_PROVIDER_JSONL_LINE_BYTES,
};

pub(super) struct JunieHydrator {
    source: ctx_history_core::SourceKey,
    binding: JunieBinding,
    source_file: Arc<OpenedProviderSourceFile>,
}

impl JunieHydrator {
    pub(super) fn new(
        source: ctx_history_core::SourceKey,
        binding: JunieBinding,
        source_file: Arc<OpenedProviderSourceFile>,
    ) -> Self {
        Self {
            source,
            binding,
            source_file,
        }
    }
}

impl JsonlFamilyHydrator for JunieHydrator {
    fn hydrate(
        &mut self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        request
            .locator()
            .validate_contract()
            .map_err(|_| failure(HydrationFailureKind::InvalidLocator))?;
        validate_locator_source(request.locator(), &self.source, &self.binding)?;
        validate_revision_policy(request.locator(), self.binding.source_revision_digest)?;
        let exact_text = match request.locator().coordinate() {
            NativeRecordCoordinate::Jsonl {
                byte_offset,
                byte_length,
                physical_ordinal,
                native_session_key,
                native_event_key,
            } => {
                let event_sequence = request_event_sequence(native_event_key)?;
                let expected_event_key = TypedKey::composite(vec![
                    TypedKey::utf8(USER_PROMPT_COORDINATE_KIND)
                        .map_err(|_| failure(HydrationFailureKind::InvalidLocator))?,
                    TypedKey::U64(event_sequence),
                ])
                .map_err(|_| failure(HydrationFailureKind::InvalidLocator))?;
                if native_session_key.as_ref()
                    != Some(&TypedKey::Utf8(self.binding.provider_session_id.clone()))
                    || native_event_key.as_ref() != Some(&expected_event_key)
                {
                    return Err(failure(HydrationFailureKind::InvalidLocator));
                }
                validate_event_identity(request, &self.source, &self.binding, event_sequence)?;
                let payload = read_payload(&self.source_file, *byte_offset, *byte_length)?;
                if Sha256::digest(&payload).as_slice() != request.locator().record_digest() {
                    return Err(failure(HydrationFailureKind::StaleRecordEvidence));
                }
                replay_user_prompt(*physical_ordinal, &payload)?
            }
            NativeRecordCoordinate::TreeRecord {
                relative_file_key,
                record_coordinate,
            } => {
                if relative_file_key != &TypedKey::Utf8(RELATIVE_EVENTS_FILE.to_owned()) {
                    return Err(failure(HydrationFailureKind::InvalidLocator));
                }
                let (event_sequence, target, entries) = decode_record_set(record_coordinate)?;
                validate_event_identity(request, &self.source, &self.binding, event_sequence)?;
                let values = read_record_set(
                    &self.source_file,
                    &entries,
                    request.locator().record_digest(),
                )?;
                replay_record_set(&target, &values)?
            }
            NativeRecordCoordinate::ProviderNative {
                namespace,
                coordinate,
            } if namespace == UNAVAILABLE_COORDINATE_NAMESPACE => {
                let (target, event_sequence) = decode_unavailable_coordinate(coordinate)?;
                validate_event_identity(request, &self.source, &self.binding, event_sequence)?;
                if &super::unavailable_digest(event_sequence, &target)
                    != request.locator().record_digest()
                {
                    return Err(failure(HydrationFailureKind::StaleRecordEvidence));
                }
                return Err(HydrationFailure {
                    kind: HydrationFailureKind::UnsupportedParserRevision,
                    detail: format!(
                        "Junie exact reopening requires at most {MAX_RECORD_SET_ENTRIES} source records"
                    ),
                });
            }
            _ => return Err(failure(HydrationFailureKind::InvalidLocator)),
        };
        Ok(HydratedProviderRecord {
            event_id: request.event_id(),
            provider_bytes: exact_text.into_bytes(),
        })
    }
}

fn validate_revision_policy(
    locator: &SourceRecordLocator,
    current_revision_digest: [u8; 32],
) -> Result<(), HydrationFailure> {
    let expected = locator
        .certified_source_revision_digest()
        .copied()
        .ok_or_else(|| failure(HydrationFailureKind::InvalidLocator))?;
    match locator.revision_policy() {
        LocatorRevisionPolicy::StableRecordEvidence => Ok(()),
        LocatorRevisionPolicy::ExactSourceRevision if expected == current_revision_digest => Ok(()),
        LocatorRevisionPolicy::ExactSourceRevision => {
            Err(failure(HydrationFailureKind::StaleSourceEvidence))
        }
    }
}

fn validate_event_identity(
    request: &EventHydrationRequest,
    source: &ctx_history_core::SourceKey,
    binding: &JunieBinding,
    event_sequence: u64,
) -> Result<(), HydrationFailure> {
    let native_item_key = NativeItemKey::certified_position(
        NATIVE_EVENT_POSITION_KIND,
        TypedKey::U64(event_sequence),
        PositionStability::AppendStable,
    )
    .map_err(|_| failure(HydrationFailureKind::InvalidLocator))?;
    let expected = derive_event_id(EventIdentityInput {
        source,
        session_id: binding.session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map_err(|_| failure(HydrationFailureKind::InvalidLocator))?;
    if expected != request.event_id() {
        return Err(failure(HydrationFailureKind::InvalidLocator));
    }
    Ok(())
}

fn validate_locator_source(
    locator: &SourceRecordLocator,
    source: &ctx_history_core::SourceKey,
    binding: &JunieBinding,
) -> Result<(), HydrationFailure> {
    if locator.source().provider() != ctx_history_core::CaptureProvider::Junie.as_str()
        || locator.source().source_format() != JUNIE_SESSION_EVENTS_SOURCE_FORMAT
        || locator.source().schema_variant() != SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
        || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_none()
        || !locator.source().exact_descriptor_eq(source)
    {
        return Err(failure(HydrationFailureKind::InvalidLocator));
    }
    let SourceAnchor::ProviderNative { namespace, key } = locator.source().anchor() else {
        return Err(failure(HydrationFailureKind::InvalidLocator));
    };
    if namespace != SOURCE_ANCHOR_NAMESPACE
        || key != &TypedKey::Utf8(binding.provider_session_id.clone())
    {
        return Err(failure(HydrationFailureKind::InvalidLocator));
    }
    Ok(())
}

fn request_event_sequence(native_event_key: &Option<TypedKey>) -> Result<u64, HydrationFailure> {
    let Some(TypedKey::Composite(parts)) = native_event_key else {
        return Err(failure(HydrationFailureKind::InvalidLocator));
    };
    let [TypedKey::Utf8(kind), TypedKey::U64(sequence)] = parts.as_slice() else {
        return Err(failure(HydrationFailureKind::InvalidLocator));
    };
    if kind != USER_PROMPT_COORDINATE_KIND {
        return Err(failure(HydrationFailureKind::InvalidLocator));
    }
    Ok(*sequence)
}

fn read_payload(
    file: &OpenedProviderSourceFile,
    byte_offset: u64,
    byte_length: u64,
) -> Result<Vec<u8>, HydrationFailure> {
    if byte_length == 0 || byte_length > MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2) as u64 {
        return Err(failure(HydrationFailureKind::InvalidLocator));
    }
    let length =
        usize::try_from(byte_length).map_err(|_| failure(HydrationFailureKind::InvalidLocator))?;
    let record = file
        .read_exact_range(
            byte_offset,
            length,
            MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2),
        )
        .map_err(|error| match error {
            CaptureError::InvalidPayload(_) => failure(HydrationFailureKind::MissingRecord),
            _ => failure(HydrationFailureKind::TemporarilyUnavailable),
        })?;
    Ok(strip_jsonl_ending(&record).to_vec())
}

#[derive(Debug)]
struct RecordSetEntry {
    ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
    payload_digest: [u8; 32],
}

fn decode_record_set(
    coordinate: &TypedKey,
) -> Result<(u64, SourceBackedTarget, Vec<RecordSetEntry>), HydrationFailure> {
    let TypedKey::Composite(parts) = coordinate else {
        return Err(failure(HydrationFailureKind::InvalidLocator));
    };
    let [TypedKey::Utf8(kind), TypedKey::U64(event_sequence), target, TypedKey::Composite(encoded)] =
        parts.as_slice()
    else {
        return Err(failure(HydrationFailureKind::InvalidLocator));
    };
    if kind != RECORD_SET_COORDINATE_KIND {
        return Err(failure(HydrationFailureKind::UnsupportedParserRevision));
    }
    if encoded.is_empty() || encoded.len() > MAX_RECORD_SET_ENTRIES {
        return Err(failure(HydrationFailureKind::InvalidLocator));
    }
    let target = decode_target(target)?;
    let mut entries = Vec::with_capacity(encoded.len());
    for encoded in encoded {
        let TypedKey::Composite(parts) = encoded else {
            return Err(failure(HydrationFailureKind::InvalidLocator));
        };
        let [TypedKey::U64(ordinal), TypedKey::U64(start), TypedKey::U64(end), TypedKey::Bytes(digest)] =
            parts.as_slice()
        else {
            return Err(failure(HydrationFailureKind::InvalidLocator));
        };
        let payload_digest = digest
            .as_slice()
            .try_into()
            .map_err(|_| failure(HydrationFailureKind::InvalidLocator))?;
        if start >= end
            || entries.last().is_some_and(|prior: &RecordSetEntry| {
                prior.ordinal >= *ordinal || prior.byte_end_exclusive > *start
            })
        {
            return Err(failure(HydrationFailureKind::InvalidLocator));
        }
        entries.push(RecordSetEntry {
            ordinal: *ordinal,
            byte_start: *start,
            byte_end_exclusive: *end,
            payload_digest,
        });
    }
    Ok((*event_sequence, target, entries))
}

fn decode_target(target: &TypedKey) -> Result<SourceBackedTarget, HydrationFailure> {
    let TypedKey::Composite(parts) = target else {
        return Err(failure(HydrationFailureKind::InvalidLocator));
    };
    let [TypedKey::U64(tag), TypedKey::U64(first), TypedKey::U64(second)] = parts.as_slice() else {
        return Err(failure(HydrationFailureKind::InvalidLocator));
    };
    match (*tag, *first, *second) {
        (1, 0, 0) => Ok(SourceBackedTarget::UserPrompt),
        (2, 0, 0) => Ok(SourceBackedTarget::AssistantMessage),
        (3, first, 0) => Ok(SourceBackedTarget::StepCall {
            step_order: u32::try_from(first)
                .map_err(|_| failure(HydrationFailureKind::InvalidLocator))?,
        }),
        (4, first, 0) => Ok(SourceBackedTarget::StepOutput {
            step_order: u32::try_from(first)
                .map_err(|_| failure(HydrationFailureKind::InvalidLocator))?,
        }),
        (5, first, second) => Ok(SourceBackedTarget::FileChange {
            step_order: u32::try_from(first)
                .map_err(|_| failure(HydrationFailureKind::InvalidLocator))?,
            change_index: u32::try_from(second)
                .map_err(|_| failure(HydrationFailureKind::InvalidLocator))?,
        }),
        _ => Err(failure(HydrationFailureKind::InvalidLocator)),
    }
}

fn read_record_set(
    file: &OpenedProviderSourceFile,
    entries: &[RecordSetEntry],
    expected_digest: &[u8; 32],
) -> Result<Vec<(u64, Value)>, HydrationFailure> {
    let total_bytes = entries.iter().try_fold(0_u64, |total, entry| {
        total.checked_add(entry.byte_end_exclusive.saturating_sub(entry.byte_start))
    });
    if total_bytes.is_none_or(|bytes| bytes > MAX_JUNIE_TRANSIENT_TURN_BYTES as u64) {
        return Err(failure(HydrationFailureKind::InvalidLocator));
    }
    let mut aggregate = Sha256::new();
    aggregate.update(RECORD_SET_DIGEST_DOMAIN);
    aggregate.update((entries.len() as u64).to_be_bytes());
    let mut values = Vec::with_capacity(entries.len());
    for entry in entries {
        let payload = read_payload(
            file,
            entry.byte_start,
            entry.byte_end_exclusive.saturating_sub(entry.byte_start),
        )?;
        let observed: [u8; 32] = Sha256::digest(&payload).into();
        if observed != entry.payload_digest {
            return Err(failure(HydrationFailureKind::StaleRecordEvidence));
        }
        aggregate.update(entry.ordinal.to_be_bytes());
        aggregate.update(entry.byte_start.to_be_bytes());
        aggregate.update(entry.byte_end_exclusive.to_be_bytes());
        aggregate.update(observed);
        values.push((
            entry.ordinal,
            serde_json::from_slice(&payload)
                .map_err(|_| failure(HydrationFailureKind::UnsupportedParserRevision))?,
        ));
    }
    if &<[u8; 32]>::from(aggregate.finalize()) != expected_digest {
        return Err(failure(HydrationFailureKind::StaleRecordEvidence));
    }
    Ok(values)
}

fn replay_user_prompt(_ordinal: u64, payload: &[u8]) -> Result<String, HydrationFailure> {
    let value: Value = serde_json::from_slice(payload)
        .map_err(|_| failure(HydrationFailureKind::UnsupportedParserRevision))?;
    if value.get("kind").and_then(Value::as_str) != Some("UserPromptEvent") {
        return Err(failure(HydrationFailureKind::UnsupportedParserRevision));
    }
    value
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| failure(HydrationFailureKind::MissingRecord))
}

fn replay_record_set(
    target: &SourceBackedTarget,
    values: &[(u64, Value)],
) -> Result<String, HydrationFailure> {
    let mut buffer = JunieAssistantBuffer::default();
    for (ordinal, value) in values {
        if value.get("kind").and_then(Value::as_str) != Some("SessionA2uxEvent") {
            return Err(failure(HydrationFailureKind::UnsupportedParserRevision));
        }
        let agent = value
            .get("event")
            .and_then(|event| event.get("agentEvent"))
            .ok_or_else(|| failure(HydrationFailureKind::UnsupportedParserRevision))?;
        let occurred_at = value
            .get("timestampMs")
            .and_then(Value::as_i64)
            .and_then(DateTime::<Utc>::from_timestamp_millis)
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
        if !junie_merge_buffered_agent_event(
            &mut buffer,
            agent,
            ordinal.saturating_add(1),
            occurred_at,
        ) {
            return Err(failure(HydrationFailureKind::UnsupportedParserRevision));
        }
    }
    match target {
        SourceBackedTarget::UserPrompt => Err(failure(HydrationFailureKind::InvalidLocator)),
        SourceBackedTarget::AssistantMessage => {
            let text = junie_buffer_result_text(&buffer);
            (!text.is_empty())
                .then_some(text)
                .ok_or_else(|| failure(HydrationFailureKind::MissingRecord))
        }
        SourceBackedTarget::StepCall { step_order } => {
            Ok(step_call_text(step_by_order(&buffer, *step_order)?))
        }
        SourceBackedTarget::StepOutput { step_order } => {
            junie_step_output_projection(step_by_order(&buffer, *step_order)?)
                .map(|output| output.details.to_owned())
                .ok_or_else(|| failure(HydrationFailureKind::MissingRecord))
        }
        SourceBackedTarget::FileChange {
            step_order,
            change_index,
        } => {
            let change = step_by_order(&buffer, *step_order)?
                .changes
                .get(*change_index as usize)
                .ok_or_else(|| failure(HydrationFailureKind::MissingRecord))?;
            let path = change
                .get("afterRelativePath")
                .and_then(Value::as_str)
                .or_else(|| change.get("beforeRelativePath").and_then(Value::as_str))
                .filter(|path| !path.trim().is_empty())
                .ok_or_else(|| failure(HydrationFailureKind::MissingRecord))?;
            Ok(format!("Edit: {path}"))
        }
    }
}

fn step_by_order(
    buffer: &JunieAssistantBuffer,
    step_order: u32,
) -> Result<&JunieStepAgg, HydrationFailure> {
    let step_id = buffer
        .step_ids_in_order
        .get(step_order as usize)
        .ok_or_else(|| failure(HydrationFailureKind::MissingRecord))?;
    buffer
        .steps
        .get(step_id)
        .ok_or_else(|| failure(HydrationFailureKind::MissingRecord))
}

fn step_call_text(step: &JunieStepAgg) -> String {
    if let Some(command) = &step.command {
        format!("Bash: {command}")
    } else {
        step.label.clone().unwrap_or_else(|| {
            if step.files.is_some() {
                "View files".to_owned()
            } else {
                "Junie tool step".to_owned()
            }
        })
    }
}

fn decode_unavailable_coordinate(
    coordinate: &TypedKey,
) -> Result<(TypedKey, u64), HydrationFailure> {
    let TypedKey::Composite(parts) = coordinate else {
        return Err(failure(HydrationFailureKind::InvalidLocator));
    };
    let [target, TypedKey::U64(event_sequence)] = parts.as_slice() else {
        return Err(failure(HydrationFailureKind::InvalidLocator));
    };
    decode_target(target)?;
    Ok((target.clone(), *event_sequence))
}

fn strip_jsonl_ending(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn failure(kind: HydrationFailureKind) -> HydrationFailure {
    HydrationFailure {
        kind,
        detail: "Junie source-backed locator could not be verified".to_owned(),
    }
}

pub(super) fn unavailable(error: impl std::fmt::Display) -> HydrationFailure {
    HydrationFailure {
        kind: HydrationFailureKind::TemporarilyUnavailable,
        detail: error.to_string(),
    }
}
