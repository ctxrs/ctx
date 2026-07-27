//! Exact verified-content capture and recovery for Mux session streams.
//!
//! `chat.jsonl` is append-only and uses byte ranges. `partial.json` is a
//! mutable whole-file snapshot and therefore has its own locator kind and
//! content profile. Both routes verify the captured record
//! digest and normalized content reference before returning source content.

use ctx_history_core::{CaptureProvider, EventType};
use serde_json::Value;

#[cfg(test)]
use super::digest_bytes;
use super::{
    CompleteContentBodyDigest, CompleteContentError, CompleteContentErrorKind,
    CompleteContentHashAuthority, CompleteContentSourceFamily, CompleteContentSourceLocator,
    CompleteMessage, CompleteMessageRequest, JsonlRange, SourceVerification, VerifiedContentRole,
};
use crate::complete_content::{
    verified_content_route_matches, ResolvedResultContent, ResultContentRequest,
};
use crate::provider::providers::mux::{
    mux_event_id, mux_event_text, mux_event_type, mux_result_content,
};
use crate::MUX_SOURCE_FORMAT;

pub(super) const MUX_LOCATOR_KIND: &str = "mux-record-v1";
const MUX_LOCATOR_BYTES: usize = 17;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MuxAddress {
    Chat(JsonlRange),
    Partial { byte_len: u64 },
}

impl MuxAddress {
    #[cfg(test)]
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
