//! Exact verified-content capture and recovery for Mux session streams.
//!
//! `chat.jsonl` is append-only and uses byte ranges. `partial.json` is a
//! mutable whole-file snapshot and therefore has its own locator kind and
//! content profile. Both routes verify the captured record
//! digest and normalized content reference before returning source content.

use ctx_history_core::{CaptureProvider, ContentRef, EventType, ProviderEventEnvelope};
use serde_json::{json, Value};

use super::{
    digest_bytes, CompleteContentBodyDigest, CompleteContentError, CompleteContentErrorKind,
    CompleteContentHashAuthority, CompleteContentSourceFamily, CompleteContentSourceLocator,
    CompleteMessage, CompleteMessageRequest, JsonlRange, SourceVerification,
    VerifiedContentLocatorV1, VerifiedContentRole, COMPLETE_CONTENT_MAX_BODY_BYTES,
};
use crate::captured_batch::jsonl::jsonl_locator_range;
use crate::captured_batch::{CapturedRecord, CapturedRecordPayload};
use crate::complete_content::{
    attach_verified_content_locator, verified_content_profile_for_locator,
    verified_content_route_matches, ResolvedResultContent, ResultContentRequest,
};
use crate::provider::providers::mux::{
    mux_event_id, mux_event_text, mux_event_type, mux_result_content,
};
use crate::{CaptureError, Result as CaptureResult, MUX_SOURCE_FORMAT};

pub(super) const MUX_LOCATOR_KIND: &str = "mux-record-v1";
const MUX_LOCATOR_BYTES: usize = 17;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MuxAddress {
    Chat(JsonlRange),
    Partial { byte_len: u64 },
}

impl MuxAddress {
    fn encode(self) -> [u8; MUX_LOCATOR_BYTES] {
        let (tag, range) = match self {
            Self::Chat(range) => (1, range),
            Self::Partial { byte_len } => (
                2,
                JsonlRange {
                    byte_start: 0,
                    byte_end_exclusive: byte_len,
                },
            ),
        };
        let mut encoded = [0_u8; MUX_LOCATOR_BYTES];
        encoded[0] = tag;
        encoded[1..].copy_from_slice(&range.encode());
        encoded
    }

    fn decode(locator: &CompleteContentSourceLocator) -> Option<Self> {
        if locator.kind() != MUX_LOCATOR_KIND || locator.value().len() != MUX_LOCATOR_BYTES {
            return None;
        }
        let range = JsonlRange::decode_bytes(&locator.value()[1..])?;
        match locator.value()[0] {
            1 => Some(Self::Chat(range)),
            2 if range.byte_start == 0 => Some(Self::Partial {
                byte_len: range.byte_end_exclusive,
            }),
            _ => None,
        }
    }
}

pub(crate) fn valid_mux_locator(value: &[u8]) -> bool {
    CompleteContentSourceLocator::new(MUX_LOCATOR_KIND, value.to_vec())
        .as_ref()
        .and_then(MuxAddress::decode)
        .is_some()
}

pub(crate) fn attach_mux_verified_content_locator(
    event: &mut ProviderEventEnvelope,
    result_content_ref: Option<&ContentRef>,
    raw_value: &Value,
    record: &CapturedRecord,
    line_number: usize,
    is_partial: bool,
) -> CaptureResult<()> {
    let locator_value = mux_locator(record, is_partial)?;
    let CapturedRecordPayload::NativeBytes(record_bytes) = record.payload() else {
        return Err(CaptureError::SystemInvariant(
            "Mux verified-content locator requires native bytes",
        ));
    };
    let native_record_id = mux_native_record_id(raw_value, line_number, is_partial);

    if event.event_type == EventType::Message {
        let text = mux_event_text(raw_value, EventType::Message);
        if text.chars().count() > crate::PROVIDER_MAX_TEXT_CHARS
            && text.len() <= COMPLETE_CONTENT_MAX_BODY_BYTES
        {
            if let Some(content_ref) = ContentRef::from_bytes(text.as_bytes()) {
                let _ = attach_mux_locator(
                    event,
                    VerifiedContentRole::MessageBody,
                    MUX_SOURCE_FORMAT,
                    MUX_LOCATOR_KIND,
                    &locator_value,
                    native_record_id.clone(),
                    digest_bytes(record_bytes),
                    content_ref,
                )?;
            }
        }
    }

    if matches!(
        event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    ) {
        if let Some(content_ref) = result_content_ref.filter(|content_ref| {
            usize::try_from(content_ref.byte_len())
                .ok()
                .is_some_and(|length| length <= COMPLETE_CONTENT_MAX_BODY_BYTES)
        }) {
            if event.payload.as_object().is_none() {
                return Ok(());
            }
            if attach_mux_locator(
                event,
                VerifiedContentRole::ResultBody,
                MUX_SOURCE_FORMAT,
                MUX_LOCATOR_KIND,
                &locator_value,
                native_record_id,
                digest_bytes(record_bytes),
                content_ref.clone(),
            )? {
                let payload =
                    event
                        .payload
                        .as_object_mut()
                        .ok_or(CaptureError::SystemInvariant(
                            "Mux event payload stopped being an object",
                        ))?;
                payload.insert("result_content_ref".to_owned(), json!(content_ref));
                payload.insert(
                    "output_bytes".to_owned(),
                    Value::from(content_ref.byte_len()),
                );
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn attach_mux_locator(
    event: &mut ProviderEventEnvelope,
    role: VerifiedContentRole,
    source_format: &str,
    locator_kind: &str,
    locator_value: &[u8],
    native_record_id: String,
    record_sha256: CompleteContentBodyDigest,
    content_ref: ContentRef,
) -> CaptureResult<bool> {
    let profile = verified_content_profile_for_locator(
        CaptureProvider::Mux,
        source_format,
        CompleteContentSourceFamily::Jsonl,
        role,
        locator_kind,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Mux verified-content route must have a profile",
    ))?;
    let Some(locator) = VerifiedContentLocatorV1::new(
        role,
        profile,
        content_ref,
        CompleteContentSourceFamily::Jsonl,
        locator_kind,
        locator_value,
        native_record_id,
        record_sha256,
    ) else {
        return Ok(false);
    };
    attach_verified_content_locator(&mut event.metadata, locator)
        .map(|()| true)
        .ok_or(CaptureError::SystemInvariant(
            "verified-content locator collection is malformed",
        ))
}

fn mux_locator(record: &CapturedRecord, is_partial: bool) -> CaptureResult<Vec<u8>> {
    if is_partial {
        if record.ordinal() != 0 {
            return Err(CaptureError::SystemInvariant(
                "Mux partial record ordinal must be zero",
            ));
        }
        let CapturedRecordPayload::NativeBytes(bytes) = record.payload() else {
            return Err(CaptureError::SystemInvariant(
                "Mux partial locator requires native bytes",
            ));
        };
        let byte_len = u64::try_from(bytes.len())
            .map_err(|_| CaptureError::SystemInvariant("Mux partial record length overflowed"))?;
        return Ok(MuxAddress::Partial { byte_len }.encode().to_vec());
    }
    let (byte_start, byte_end_exclusive) = jsonl_locator_range(record.locator())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    Ok(MuxAddress::Chat(JsonlRange {
        byte_start,
        byte_end_exclusive,
    })
    .encode()
    .to_vec())
}

pub(super) fn resolve_messages(
    requests: &[CompleteMessageRequest],
) -> Result<Vec<CompleteMessage>, CompleteContentError> {
    let Some(first) = requests.first() else {
        return Ok(Vec::new());
    };
    validate_message_requests(requests)?;
    let mut messages = Vec::with_capacity(requests.len());
    for request in requests {
        let record = read_mux_record(
            &request.source_access,
            request.event_id,
            &request.source_format,
            request.source_locator.as_ref().ok_or_else(|| {
                CompleteContentError::new(
                    CompleteContentErrorKind::HydrationUnsupported,
                    request.event_id,
                )
            })?,
            request.source_record_ordinal,
            request.expected_record_digest.as_ref().ok_or_else(|| {
                CompleteContentError::new(
                    CompleteContentErrorKind::HydrationUnsupported,
                    request.event_id,
                )
            })?,
        )?;
        messages.push(resolve_message_record(request, &record)?);
    }
    first.source_access.revalidate_jsonl(first.event_id)?;
    Ok(messages)
}

pub(super) fn resolve_results(
    requests: &[ResultContentRequest],
) -> Vec<Result<ResolvedResultContent, CompleteContentError>> {
    let group = resolve_result_group(requests);
    match group {
        Ok(results) => results,
        Err(error) => requests
            .iter()
            .map(|request| Err(CompleteContentError::new(error.kind, request.event_id)))
            .collect(),
    }
}

fn resolve_result_group(
    requests: &[ResultContentRequest],
) -> Result<Vec<Result<ResolvedResultContent, CompleteContentError>>, CompleteContentError> {
    if requests.is_empty() {
        return Ok(Vec::new());
    }
    validate_result_requests(requests)?;
    let mut results = Vec::with_capacity(requests.len());
    for request in requests {
        let resolved = read_mux_record(
            &request.source_access,
            request.event_id,
            &request.source_format,
            &request.source_locator,
            request.source_record_ordinal,
            &request.expected_record_digest,
        )
        .and_then(|record| resolve_result_record(request, &record));
        results.push(resolved);
    }
    requests[0]
        .source_access
        .revalidate_jsonl(requests[0].event_id)?;
    Ok(results)
}

fn validate_message_requests(
    requests: &[CompleteMessageRequest],
) -> Result<(), CompleteContentError> {
    let first = &requests[0];
    let mut prior = None;
    for request in requests {
        let position = (
            request.source_record_ordinal,
            request.source_record_subrecord_index,
        );
        if request.provider != CaptureProvider::Mux
            || request.source_format != first.source_format
            || !mux_source_is_supported(&request.source_format)
            || !request.source_locator.as_ref().is_some_and(|locator| {
                mux_locator_kind_supported(&request.source_format, locator.kind())
                    && verified_content_route_matches(
                        &request.content_profile,
                        request.provider,
                        &request.source_format,
                        CompleteContentSourceFamily::Jsonl,
                        VerifiedContentRole::MessageBody,
                        locator.kind(),
                    )
            })
            || request.source_access != first.source_access
            || request.source_access.family() != CompleteContentSourceFamily::Jsonl
            || request.source_record_subrecord_index != 0
            || request
                .expected_native_record_id
                .as_deref()
                .is_none_or(str::is_empty)
            || request.expected_record_digest.is_none()
            || request.expected_content_ref.is_none()
            || prior.is_some_and(|prior| prior >= position)
        {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::ContentVerificationFailed,
                request.event_id,
            ));
        }
        if request
            .source_locator
            .as_ref()
            .and_then(MuxAddress::decode)
            .is_some_and(|address| matches!(address, MuxAddress::Partial { .. }))
            && request.source_record_ordinal != 0
        {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::ContentVerificationFailed,
                request.event_id,
            ));
        }
        prior = Some(position);
    }
    Ok(())
}

fn validate_result_requests(requests: &[ResultContentRequest]) -> Result<(), CompleteContentError> {
    let first = &requests[0];
    let mut prior = None;
    for request in requests {
        let position = (
            request.source_record_ordinal,
            request.source_record_subrecord_index,
        );
        if request.provider != CaptureProvider::Mux
            || request.source_format != first.source_format
            || request.source_access != first.source_access
            || request.source_access.family() != CompleteContentSourceFamily::Jsonl
            || request.source_family != CompleteContentSourceFamily::Jsonl
            || !mux_locator_kind_supported(&request.source_format, request.source_locator.kind())
            || !verified_content_route_matches(
                &request.content_profile,
                request.provider,
                &request.source_format,
                request.source_family,
                VerifiedContentRole::ResultBody,
                request.source_locator.kind(),
            )
            || request.source_record_subrecord_index != 0
            || prior.is_some_and(|prior| prior >= position)
        {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::ContentVerificationFailed,
                request.event_id,
            ));
        }
        if MuxAddress::decode(&request.source_locator)
            .is_some_and(|address| matches!(address, MuxAddress::Partial { .. }))
            && request.source_record_ordinal != 0
        {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::ContentVerificationFailed,
                request.event_id,
            ));
        }
        prior = Some(position);
    }
    Ok(())
}

fn read_mux_record(
    access: &crate::complete_content::BrokeredSourceAccess,
    event_id: uuid::Uuid,
    source_format: &str,
    locator: &CompleteContentSourceLocator,
    source_record_ordinal: u64,
    expected_record_digest: &CompleteContentBodyDigest,
) -> Result<Vec<u8>, CompleteContentError> {
    if source_format != MUX_SOURCE_FORMAT {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::HydrationUnsupported,
            event_id,
        ));
    }
    match MuxAddress::decode(locator) {
        Some(MuxAddress::Chat(range)) => access.read_jsonl_record(
            range.byte_start,
            range.byte_end_exclusive,
            expected_record_digest,
            event_id,
        ),
        Some(MuxAddress::Partial { byte_len }) => {
            if source_record_ordinal != 0 {
                return Err(CompleteContentError::new(
                    CompleteContentErrorKind::ContentVerificationFailed,
                    event_id,
                ));
            }
            let record = access.read_jsonl_snapshot(expected_record_digest, event_id)?;
            if u64::try_from(record.len()).ok() != Some(byte_len) {
                return Err(CompleteContentError::new(
                    CompleteContentErrorKind::SourceChanged,
                    event_id,
                ));
            }
            Ok(record)
        }
        None => Err(CompleteContentError::new(
            CompleteContentErrorKind::HydrationUnsupported,
            event_id,
        )),
    }
}

fn resolve_message_record(
    request: &CompleteMessageRequest,
    record: &[u8],
) -> Result<CompleteMessage, CompleteContentError> {
    let value = parse_record(record, request.event_id)?;
    if mux_event_type(&value) != EventType::Message
        || request.expected_hash_authority != CompleteContentHashAuthority::ProviderSupplied
    {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::ContentVerificationFailed,
            request.event_id,
        ));
    }
    let line_number = mux_line_number(request.source_record_ordinal, request.event_id)?;
    let is_partial = request
        .source_locator
        .as_ref()
        .and_then(MuxAddress::decode)
        .is_some_and(|address| matches!(address, MuxAddress::Partial { .. }));
    let native_record_id = mux_native_record_id(&value, line_number, is_partial);
    if request.expected_native_record_id.as_deref() != Some(native_record_id.as_str())
        || request.expected_provider_event_hash != native_record_id
    {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::ContentVerificationFailed,
            request.event_id,
        ));
    }
    CompleteMessage::verified(
        request,
        mux_event_text(&value, EventType::Message),
        SourceVerification::VERIFIED,
    )
}

fn resolve_result_record(
    request: &ResultContentRequest,
    record: &[u8],
) -> Result<ResolvedResultContent, CompleteContentError> {
    let value = parse_record(record, request.event_id)?;
    if !matches!(
        mux_event_type(&value),
        EventType::ToolOutput | EventType::CommandOutput
    ) {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::ContentVerificationFailed,
            request.event_id,
        ));
    }
    let line_number = mux_line_number(request.source_record_ordinal, request.event_id)?;
    let native_record_id = mux_native_record_id(
        &value,
        line_number,
        matches!(
            MuxAddress::decode(&request.source_locator),
            Some(MuxAddress::Partial { .. })
        ),
    );
    let content = mux_result_content(&value).ok_or_else(|| {
        CompleteContentError::new(
            CompleteContentErrorKind::ContentVerificationFailed,
            request.event_id,
        )
    })?;
    if request.expected_native_record_id != native_record_id
        || !request.expected_content_ref.verifies(content.as_bytes())
    {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::ContentVerificationFailed,
            request.event_id,
        ));
    }
    Ok(ResolvedResultContent {
        event_id: request.event_id,
        content,
        content_ref: request.expected_content_ref.clone(),
        verification: SourceVerification::VERIFIED,
    })
}

fn parse_record(record: &[u8], event_id: uuid::Uuid) -> Result<Value, CompleteContentError> {
    serde_json::from_slice(record).map_err(|_| {
        CompleteContentError::new(
            CompleteContentErrorKind::ContentVerificationFailed,
            event_id,
        )
    })
}

fn mux_native_record_id(value: &Value, line_number: usize, is_partial: bool) -> String {
    let role = value
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    mux_event_id(value, line_number, role, is_partial)
}

fn mux_line_number(ordinal: u64, event_id: uuid::Uuid) -> Result<usize, CompleteContentError> {
    usize::try_from(ordinal)
        .ok()
        .and_then(|ordinal| ordinal.checked_add(1))
        .ok_or_else(|| {
            CompleteContentError::new(
                CompleteContentErrorKind::ContentVerificationFailed,
                event_id,
            )
        })
}

fn mux_source_is_supported(source_format: &str) -> bool {
    source_format == MUX_SOURCE_FORMAT
}

fn mux_locator_kind_supported(source_format: &str, locator_kind: &str) -> bool {
    source_format == MUX_SOURCE_FORMAT && locator_kind == MUX_LOCATOR_KIND
}

#[cfg(test)]
mod tests;
