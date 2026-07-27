//! Released structured locator decoding and hydration contracts.

use ctx_history_core::CaptureProvider;

use super::{
    source_access::error, verification::STRUCTURED_MAX_NATIVE_ID_BYTES,
    STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND,
};
use crate::complete_content::{
    verified_content_route_matches, verified_content_route_supported, CompleteContentBodyDigest,
    CompleteContentError, CompleteContentErrorKind, CompleteContentSourceFamily,
    CompleteMessageRequest, ResultContentRequest, VerifiedContentRole,
};

const STRUCTURED_LOCATOR_MAGIC: &[u8; 4] = b"SC\0\x01";
const STRUCTURED_RESULT_LOCATOR_MAGIC: &[u8; 4] = b"SR\0\x01";

pub(super) fn structured_source_format(provider: CaptureProvider, source_format: &str) -> bool {
    verified_content_route_supported(
        provider,
        source_format,
        CompleteContentSourceFamily::Structured,
        VerifiedContentRole::MessageBody,
    )
}

#[derive(Debug)]
pub(super) struct StructuredLocator {
    pub(super) provider: CaptureProvider,
    pub(super) ordinal: u64,
    pub(super) subrecord: u32,
    pub(super) native_id: String,
    pub(super) record_digest: CompleteContentBodyDigest,
}

impl StructuredLocator {
    pub(super) fn for_request(
        request: &CompleteMessageRequest,
    ) -> std::result::Result<Self, CompleteContentError> {
        if request.source_family != Some(CompleteContentSourceFamily::Structured) {
            return Err(error(
                request,
                CompleteContentErrorKind::HydrationUnsupported,
            ));
        }
        let Some(source_locator) = request.source_locator.as_ref() else {
            return Err(error(
                request,
                CompleteContentErrorKind::HydrationUnsupported,
            ));
        };
        if source_locator.kind() != STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND {
            return Err(error(
                request,
                CompleteContentErrorKind::HydrationUnsupported,
            ));
        }
        let (provider, ordinal, subrecord, native_id) =
            decode_structured_locator(source_locator.value()).ok_or_else(|| {
                error(request, CompleteContentErrorKind::ContentVerificationFailed)
            })?;
        let expected_native = request
            .expected_native_record_id
            .as_deref()
            .ok_or_else(|| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
        let record_digest = request
            .expected_record_digest
            .clone()
            .ok_or_else(|| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
        if provider != request.provider
            || ordinal != request.source_record_ordinal
            || subrecord != request.source_record_subrecord_index
            || native_id != expected_native
        {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
        Ok(Self {
            provider,
            ordinal,
            subrecord,
            native_id,
            record_digest,
        })
    }

    pub(super) fn for_result_request(
        request: &ResultContentRequest,
    ) -> std::result::Result<Self, CompleteContentError> {
        if request.source_family != CompleteContentSourceFamily::Structured
            || request.source_locator.kind() != STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND
            || !verified_content_route_matches(
                &request.content_profile,
                request.provider,
                &request.source_format,
                request.source_family,
                VerifiedContentRole::ResultBody,
                request.source_locator.kind(),
            )
        {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::HydrationUnsupported,
                request.event_id,
            ));
        }
        let (provider, ordinal, subrecord, native_id) =
            decode_structured_locator(request.source_locator.value()).ok_or_else(|| {
                CompleteContentError::new(
                    CompleteContentErrorKind::ContentVerificationFailed,
                    request.event_id,
                )
            })?;
        if provider != request.provider
            || ordinal != request.source_record_ordinal
            || subrecord != request.source_record_subrecord_index
            || native_id != request.expected_native_record_id
        {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::ContentVerificationFailed,
                request.event_id,
            ));
        }
        Ok(Self {
            provider,
            ordinal,
            subrecord,
            native_id,
            record_digest: request.expected_record_digest.clone(),
        })
    }
}

pub(crate) fn decode_structured_locator(
    value: &[u8],
) -> Option<(CaptureProvider, u64, u32, String)> {
    if !value.starts_with(STRUCTURED_LOCATOR_MAGIC) {
        return None;
    }
    let mut cursor = STRUCTURED_LOCATOR_MAGIC.len();
    let provider_len = usize::from(*value.get(cursor)?);
    cursor = cursor.checked_add(1)?;
    let provider_end = cursor.checked_add(provider_len)?;
    let provider = std::str::from_utf8(value.get(cursor..provider_end)?).ok()?;
    cursor = provider_end;
    let ordinal_end = cursor.checked_add(8)?;
    let ordinal = u64::from_be_bytes(value.get(cursor..ordinal_end)?.try_into().ok()?);
    cursor = ordinal_end;
    let subrecord_end = cursor.checked_add(4)?;
    let subrecord = u32::from_be_bytes(value.get(cursor..subrecord_end)?.try_into().ok()?);
    cursor = subrecord_end;
    let length_end = cursor.checked_add(2)?;
    let native_len = usize::from(u16::from_be_bytes(
        value.get(cursor..length_end)?.try_into().ok()?,
    ));
    cursor = length_end;
    let native_end = cursor.checked_add(native_len)?;
    if native_end != value.len() || native_len == 0 || native_len > STRUCTURED_MAX_NATIVE_ID_BYTES {
        return None;
    }
    let native_id = std::str::from_utf8(value.get(cursor..native_end)?)
        .ok()?
        .to_owned();
    let provider = provider.parse().ok()?;
    Some((provider, ordinal, subrecord, native_id))
}

pub(crate) fn decode_structured_result_locator(
    value: &[u8],
) -> Option<(CaptureProvider, u64, u32, u32, u32, String)> {
    if !value.starts_with(STRUCTURED_RESULT_LOCATOR_MAGIC) {
        return None;
    }
    let mut cursor = STRUCTURED_RESULT_LOCATOR_MAGIC.len();
    let provider_len = usize::from(*value.get(cursor)?);
    cursor = cursor.checked_add(1)?;
    let provider_end = cursor.checked_add(provider_len)?;
    let provider = std::str::from_utf8(value.get(cursor..provider_end)?)
        .ok()?
        .parse()
        .ok()?;
    cursor = provider_end;
    let ordinal_end = cursor.checked_add(8)?;
    let ordinal = u64::from_be_bytes(value.get(cursor..ordinal_end)?.try_into().ok()?);
    cursor = ordinal_end;
    let source_subrecord_end = cursor.checked_add(4)?;
    let source_subrecord =
        u32::from_be_bytes(value.get(cursor..source_subrecord_end)?.try_into().ok()?);
    cursor = source_subrecord_end;
    let history_item_end = cursor.checked_add(4)?;
    let history_item = u32::from_be_bytes(value.get(cursor..history_item_end)?.try_into().ok()?);
    cursor = history_item_end;
    let tool_state_end = cursor.checked_add(4)?;
    let tool_state = u32::from_be_bytes(value.get(cursor..tool_state_end)?.try_into().ok()?);
    cursor = tool_state_end;
    let length_end = cursor.checked_add(2)?;
    let native_len = usize::from(u16::from_be_bytes(
        value.get(cursor..length_end)?.try_into().ok()?,
    ));
    cursor = length_end;
    let native_end = cursor.checked_add(native_len)?;
    if native_end != value.len() || native_len == 0 || native_len > STRUCTURED_MAX_NATIVE_ID_BYTES {
        return None;
    }
    let native_id = std::str::from_utf8(value.get(cursor..native_end)?)
        .ok()?
        .to_owned();
    Some((
        provider,
        ordinal,
        source_subrecord,
        history_item,
        tool_state,
        native_id,
    ))
}
