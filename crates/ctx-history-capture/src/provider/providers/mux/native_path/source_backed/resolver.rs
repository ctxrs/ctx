use std::sync::Arc;

use ctx_history_core::{
    derive_event_id, CaptureProvider, EventHydrationRequest, EventIdentityInput,
    HydratedProviderRecord, HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy,
    NativeItemKey, NativeRecordCoordinate, SourceRecordLocator, TypedKey,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::{
        normalization::provider_value_text,
        providers::mux::normalization::{
            mux_event_id, mux_event_text, mux_event_type, mux_output_projection,
            mux_partial_event_index, mux_result_content, MuxOutputOutcome,
        },
        source_backed::family::jsonl::{observe_opened_file, JsonlFamilyHydrator},
    },
    MAX_PROVIDER_JSONL_LINE_BYTES, MUX_SOURCE_FORMAT,
};

use super::{
    bound_stream, open_verified, MuxBinding, MuxStreamKind, LOGICAL_EVENT_KIND,
    SOURCE_SCHEMA_VARIANT,
};

const NATIVE_ITEM_NAMESPACE: &str = "mux.record";
const PROVIDER_NATIVE_LOCATOR_NAMESPACE: &str = "mux.logical-record.v2";
const PARTIAL_NATIVE_ORDINAL: u64 = 1_u64 << 63;
const MAX_ORDINAL: u64 = (1_u64 << 47) - 1;

pub(super) struct MuxHydrator {
    source: ctx_history_core::SourceKey,
    authority: Arc<ProviderSourceRoot>,
    binding: MuxBinding,
    primary: Arc<OpenedProviderSourceFile>,
    secondary_partial: Option<Arc<OpenedProviderSourceFile>>,
    metadata: Option<Arc<OpenedProviderSourceFile>>,
}

impl MuxHydrator {
    pub(super) fn new(
        source: ctx_history_core::SourceKey,
        authority: Arc<ProviderSourceRoot>,
        binding: MuxBinding,
        primary: Arc<OpenedProviderSourceFile>,
    ) -> Result<Self, HydrationFailure> {
        let secondary_partial = if binding.primary_stream == MuxStreamKind::Chat {
            binding
                .partial
                .as_ref()
                .map(|bound| open_verified(&authority, bound).map_err(unavailable))
                .transpose()?
        } else {
            None
        };
        let metadata = binding
            .metadata_file
            .as_ref()
            .map(|bound| open_verified(&authority, bound).map_err(unavailable))
            .transpose()?;
        Ok(Self {
            source,
            authority,
            binding,
            primary,
            secondary_partial,
            metadata,
        })
    }

    fn stream_file(
        &self,
        stream: MuxStreamKind,
    ) -> Result<&OpenedProviderSourceFile, HydrationFailure> {
        if stream == self.binding.primary_stream {
            return Ok(self.primary.as_ref());
        }
        match stream {
            MuxStreamKind::Partial => self.secondary_partial.as_deref(),
            MuxStreamKind::Chat => None,
        }
        .ok_or_else(|| {
            failure(
                HydrationFailureKind::MissingRecord,
                "Mux locator stream is absent from the bound session",
            )
        })
    }
}

impl JsonlFamilyHydrator for MuxHydrator {
    fn hydrate(
        &mut self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        validate_locator(request.locator(), &self.source)?;
        let coordinate = decode_mux_coordinate(request.locator())?;
        if coordinate.stream_kind.is_partial()
            && request.locator().certified_source_revision_digest()
                != Some(&self.binding.source_revision_digest)
        {
            return Err(failure(
                HydrationFailureKind::StaleSourceEvidence,
                "Mux partial snapshot revision changed",
            ));
        }
        let payload = read_mux_payload(self.stream_file(coordinate.stream_kind)?, &coordinate)?;
        if Sha256::digest(&payload).as_slice() != request.locator().record_digest() {
            return Err(failure(
                HydrationFailureKind::StaleRecordEvidence,
                "Mux source record digest changed",
            ));
        }
        let value = serde_json::from_slice::<Value>(&payload)
            .map_err(|error| failure(HydrationFailureKind::UnsupportedParserRevision, error))?;
        if !value.is_object() {
            return Err(failure(
                HydrationFailureKind::UnsupportedParserRevision,
                "Mux native record is not an object",
            ));
        }
        validate_native_identity(
            &self.source,
            &self.binding,
            request,
            &coordinate,
            &payload,
            &value,
        )?;
        Ok(HydratedProviderRecord {
            event_id: request.event_id(),
            provider_bytes: mux_exact_logical_content(&value)?.into_bytes(),
        })
    }

    fn finish(&mut self) -> Result<(), HydrationFailure> {
        for (stream, opened) in [(MuxStreamKind::Partial, self.secondary_partial.as_ref())] {
            if let Some(opened) = opened {
                let bound = bound_stream(&self.binding, stream).map_err(unavailable)?;
                let path = self.authority.named_path().join(&bound.relative_path);
                if observe_opened_file(&path, opened.as_ref()).map_err(unavailable)?
                    != bound.observation
                {
                    return Err(failure(
                        HydrationFailureKind::StaleRecordEvidence,
                        "Mux auxiliary stream changed during grouped hydration",
                    ));
                }
            }
        }
        if let (Some(opened), Some(bound)) = (&self.metadata, &self.binding.metadata_file) {
            let path = self.authority.named_path().join(&bound.relative_path);
            if observe_opened_file(&path, opened.as_ref()).map_err(unavailable)?
                != bound.observation
            {
                return Err(failure(
                    HydrationFailureKind::StaleSourceEvidence,
                    "Mux metadata changed during grouped hydration",
                ));
            }
        }
        self.authority.revalidate().map_err(unavailable)
    }
}

#[derive(Debug)]
struct MuxLogicalRecordCoordinate {
    stream_kind: MuxStreamKind,
    byte_start: u64,
    byte_end_exclusive: u64,
    source_record_ordinal: u64,
    event_sequence: u64,
    native_record_id: String,
}

fn validate_locator(
    locator: &SourceRecordLocator,
    source: &ctx_history_core::SourceKey,
) -> Result<(), HydrationFailure> {
    locator
        .validate_contract()
        .map_err(|error| failure(HydrationFailureKind::InvalidLocator, error))?;
    if locator.source().provider() != CaptureProvider::Mux.as_str()
        || locator.source().source_format() != MUX_SOURCE_FORMAT
        || locator.source().schema_variant() != SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
        || locator.certified_source_revision_digest().is_none()
        || !locator.source().exact_descriptor_eq(source)
    {
        return Err(failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator source descriptor is invalid",
        ));
    }
    let coordinate = decode_mux_coordinate(locator)?;
    let expected_policy = if coordinate.stream_kind.is_partial() {
        LocatorRevisionPolicy::ExactSourceRevision
    } else {
        LocatorRevisionPolicy::StableRecordEvidence
    };
    if locator.revision_policy() != expected_policy {
        return Err(failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator revision policy does not match its stream",
        ));
    }
    Ok(())
}

fn decode_mux_coordinate(
    locator: &SourceRecordLocator,
) -> Result<MuxLogicalRecordCoordinate, HydrationFailure> {
    let NativeRecordCoordinate::ProviderNative {
        namespace,
        coordinate,
    } = locator.coordinate()
    else {
        return Err(failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator is not provider-native",
        ));
    };
    if namespace != PROVIDER_NATIVE_LOCATOR_NAMESPACE {
        return Err(failure(
            if namespace.starts_with("mux.") {
                HydrationFailureKind::UnsupportedParserRevision
            } else {
                HydrationFailureKind::InvalidLocator
            },
            "Mux locator namespace is unsupported",
        ));
    }
    let TypedKey::Composite(parts) = coordinate else {
        return Err(failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator coordinate is malformed",
        ));
    };
    let [TypedKey::U64(version), TypedKey::U64(tag), TypedKey::U64(byte_start), TypedKey::U64(byte_end_exclusive), TypedKey::U64(source_record_ordinal), TypedKey::U64(event_sequence), TypedKey::Utf8(native_record_id)] =
        parts.as_slice()
    else {
        return Err(failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator coordinate is malformed",
        ));
    };
    if *version != 2 {
        return Err(failure(
            HydrationFailureKind::UnsupportedParserRevision,
            "Mux locator parser revision is unsupported",
        ));
    }
    let stream_kind = match *tag {
        1 => MuxStreamKind::Chat,
        2 => MuxStreamKind::Partial,
        _ => {
            return Err(failure(
                HydrationFailureKind::InvalidLocator,
                "Mux locator stream tag is invalid",
            ))
        }
    };
    if byte_start >= byte_end_exclusive
        || native_record_id.is_empty()
        || (stream_kind.is_partial() && (*byte_start != 0 || *source_record_ordinal != 0))
        || (!stream_kind.is_partial() && event_sequence != source_record_ordinal)
    {
        return Err(failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator coordinate is internally inconsistent",
        ));
    }
    Ok(MuxLogicalRecordCoordinate {
        stream_kind,
        byte_start: *byte_start,
        byte_end_exclusive: *byte_end_exclusive,
        source_record_ordinal: *source_record_ordinal,
        event_sequence: *event_sequence,
        native_record_id: native_record_id.clone(),
    })
}

fn read_mux_payload(
    source: &OpenedProviderSourceFile,
    coordinate: &MuxLogicalRecordCoordinate,
) -> Result<Vec<u8>, HydrationFailure> {
    let byte_length = coordinate
        .byte_end_exclusive
        .checked_sub(coordinate.byte_start)
        .ok_or_else(|| {
            failure(
                HydrationFailureKind::InvalidLocator,
                "Mux locator byte range moved backwards",
            )
        })?;
    if byte_length == 0 || byte_length > MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2) as u64 {
        return Err(failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator byte range exceeds the record bound",
        ));
    }
    if coordinate.byte_end_exclusive > source.len() {
        return Err(failure(
            HydrationFailureKind::MissingRecord,
            "Mux locator byte range is no longer present",
        ));
    }
    if coordinate.stream_kind == MuxStreamKind::Chat
        && coordinate.byte_start > 0
        && source
            .read_exact_range(coordinate.byte_start - 1, 1, 1)
            .map_err(unavailable)?
            != b"\n"
    {
        return Err(failure(
            HydrationFailureKind::StaleRecordEvidence,
            "Mux chat record start boundary changed",
        ));
    }
    let length = usize::try_from(byte_length).map_err(|_| {
        failure(
            HydrationFailureKind::InvalidLocator,
            "Mux range is too large",
        )
    })?;
    let bytes = source
        .read_exact_range(
            coordinate.byte_start,
            length,
            MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2),
        )
        .map_err(unavailable)?;
    if coordinate.stream_kind.is_partial() {
        if coordinate.byte_start != 0
            || coordinate.byte_end_exclusive != source.len()
            || coordinate.source_record_ordinal != 0
        {
            return Err(failure(
                HydrationFailureKind::InvalidLocator,
                "Mux partial locator is not its whole snapshot",
            ));
        }
        return Ok(bytes);
    }
    let first_newline = bytes.iter().position(|byte| *byte == b'\n');
    if first_newline.is_some_and(|position| position + 1 != bytes.len())
        || (first_newline.is_none() && coordinate.byte_end_exclusive != source.len())
    {
        return Err(failure(
            HydrationFailureKind::StaleRecordEvidence,
            "Mux chat record end boundary changed",
        ));
    }
    Ok(strip_jsonl_ending(&bytes).to_vec())
}

fn validate_native_identity(
    source: &ctx_history_core::SourceKey,
    binding: &MuxBinding,
    request: &EventHydrationRequest,
    coordinate: &MuxLogicalRecordCoordinate,
    payload: &[u8],
    value: &Value,
) -> Result<(), HydrationFailure> {
    let line_number = usize::try_from(coordinate.source_record_ordinal)
        .ok()
        .and_then(|ordinal| ordinal.checked_add(1))
        .ok_or_else(|| {
            failure(
                HydrationFailureKind::InvalidLocator,
                "Mux ordinal is too large",
            )
        })?;
    let role = value
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let native_record_id = mux_event_id(
        value,
        line_number,
        role,
        coordinate.stream_kind.is_partial(),
    );
    if native_record_id != coordinate.native_record_id {
        return Err(failure(
            HydrationFailureKind::StaleRecordEvidence,
            "Mux native record identity changed",
        ));
    }
    let expected_sequence = if coordinate.stream_kind.is_partial() {
        PARTIAL_NATIVE_ORDINAL | (mux_partial_event_index(payload) & MAX_ORDINAL)
    } else {
        coordinate.source_record_ordinal
    };
    if expected_sequence != coordinate.event_sequence {
        return Err(failure(
            HydrationFailureKind::StaleRecordEvidence,
            "Mux native event sequence changed",
        ));
    }
    let native_item_key = NativeItemKey::native_id(
        NATIVE_ITEM_NAMESPACE,
        TypedKey::utf8(&native_record_id)
            .map_err(|error| failure(HydrationFailureKind::InvalidLocator, error))?,
    )
    .map_err(|error| failure(HydrationFailureKind::InvalidLocator, error))?;
    let expected_event_id = derive_event_id(EventIdentityInput {
        source,
        session_id: binding.session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map_err(|error| failure(HydrationFailureKind::InvalidLocator, error))?;
    if expected_event_id != request.event_id() {
        return Err(failure(
            HydrationFailureKind::InvalidLocator,
            "Mux event identity does not match its coordinate",
        ));
    }
    if let Some(output) = mux_output_projection(value) {
        if !output.body_available {
            return Err(failure(
                HydrationFailureKind::MissingRecord,
                "Mux native output body is unavailable",
            ));
        }
        if !matches!(
            output.outcome,
            MuxOutputOutcome::Failure | MuxOutputOutcome::Timeout
        ) {
            return Err(failure(
                HydrationFailureKind::InvalidLocator,
                "Mux successful output is not an indexed event",
            ));
        }
    }
    Ok(())
}

pub(super) fn mux_exact_logical_content(value: &Value) -> Result<String, HydrationFailure> {
    let event_type = mux_event_type(value);
    if matches!(
        event_type,
        ctx_history_core::EventType::ToolOutput | ctx_history_core::EventType::CommandOutput
    ) {
        return mux_result_content(value).ok_or_else(|| {
            failure(
                HydrationFailureKind::MissingRecord,
                "Mux exact output body is unavailable",
            )
        });
    }
    let mut rendered = Vec::new();
    if let Some(parts) = value.get("parts").and_then(Value::as_array) {
        for part in parts {
            match part.get("type").and_then(Value::as_str) {
                Some("text" | "reasoning") => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        rendered.push(text.to_owned());
                    }
                }
                Some("dynamic-tool") => rendered.push(exact_tool_part_text(part)),
                Some("file") => {
                    if let Some(label) = exact_file_part_text(part) {
                        rendered.push(label);
                    }
                }
                _ => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        rendered.push(text.to_owned());
                    }
                }
            }
        }
    }
    if !rendered.is_empty() {
        return Ok(rendered.join("\n"));
    }
    if let Some(text) = value
        .get("content")
        .or_else(|| value.get("message"))
        .and_then(provider_value_text)
    {
        return Ok(text);
    }
    Ok(mux_event_text(value, event_type))
}

fn exact_tool_part_text(part: &Value) -> String {
    let name = part
        .get("toolName")
        .or_else(|| part.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let state = part.get("state").and_then(Value::as_str);
    let prefix = if matches!(state, Some("output-available" | "output-redacted"))
        || part.get("output").is_some()
    {
        "tool output"
    } else {
        "tool call"
    };
    let mut text = format!("{prefix}: {name}");
    if let Some(input) = part.get("input") {
        text.push_str("\ninput: ");
        text.push_str(&exact_value_text(input));
    }
    if let Some(output) = part.get("output") {
        text.push_str("\noutput: ");
        text.push_str(&exact_value_text(output));
    }
    if let Some(nested) = part.get("nestedCalls").and_then(Value::as_array) {
        let names = nested
            .iter()
            .filter_map(|call| {
                call.get("toolName")
                    .or_else(|| call.get("name"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>();
        if !names.is_empty() {
            text.push_str("\nnested tools: ");
            text.push_str(&names.join(", "));
        }
    }
    text
}

fn exact_value_text(value: &Value) -> String {
    provider_value_text(value)
        .or_else(|| serde_json::to_string(value).ok())
        .unwrap_or_else(|| value.to_string())
}

fn exact_file_part_text(part: &Value) -> Option<String> {
    let label = part
        .get("filename")
        .or_else(|| part.get("name"))
        .or_else(|| part.get("mediaType"))
        .or_else(|| part.get("mimeType"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            part.get("url")
                .and_then(Value::as_str)
                .filter(|url| !url.starts_with("data:") && url.len() < 256)
                .map(str::to_owned)
        })?;
    Some(format!("file: {label}"))
}

fn strip_jsonl_ending(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn failure(kind: HydrationFailureKind, detail: impl std::fmt::Display) -> HydrationFailure {
    HydrationFailure {
        kind,
        detail: detail.to_string(),
    }
}

pub(super) fn unavailable(error: impl std::fmt::Display) -> HydrationFailure {
    failure(HydrationFailureKind::TemporarilyUnavailable, error)
}
