//! Stable structured locator encodings and import-time attachment contracts.

use ctx_history_core::{CaptureProvider, ContentRef, EventType, ProviderEventEnvelope};
use serde_json::Value;

use crate::{CaptureError, Result, PROVIDER_MAX_TEXT_CHARS};

use super::{
    source_access::error,
    verification::{digest_bytes, STRUCTURED_MAX_NATIVE_ID_BYTES},
    STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND, STRUCTURED_RESULT_CONTENT_LOCATOR_KIND,
};
use crate::complete_content::{
    attach_verified_content_locator, verified_content_profile, verified_content_route_matches,
    verified_content_route_supported, CompleteContentBodyDigest, CompleteContentError,
    CompleteContentErrorKind, CompleteContentSourceFamily, CompleteMessageRequest,
    ResultContentRequest, VerifiedContentLocatorV1, VerifiedContentRole, VERIFIED_CONTENT_ROUTES,
};

const STRUCTURED_LOCATOR_MAGIC: &[u8; 4] = b"SC\0\x01";
const STRUCTURED_RESULT_LOCATOR_MAGIC: &[u8; 4] = b"SR\0\x01";

pub(crate) fn attach_structured_complete_content_locator(
    provider: CaptureProvider,
    event: &mut ProviderEventEnvelope,
    source_record_ordinal: u64,
    source_record_subrecord_index: u32,
    native_record_id: &str,
    record_bytes: &[u8],
    complete_text: &str,
) -> Result<()> {
    if event.event_type != EventType::Message
        || complete_text.chars().count() <= PROVIDER_MAX_TEXT_CHARS
    {
        return Ok(());
    }
    if native_record_id.is_empty()
        || native_record_id.len() > STRUCTURED_MAX_NATIVE_ID_BYTES
        || native_record_id.chars().any(char::is_control)
    {
        return Err(CaptureError::InvalidPayload(
            "structured complete-content native record identity is invalid".to_owned(),
        ));
    }
    let value = encode_structured_locator(
        provider,
        source_record_ordinal,
        source_record_subrecord_index,
        native_record_id,
    )?;
    let record_sha256 = CompleteContentBodyDigest::parse(digest_bytes(record_bytes)).ok_or(
        CaptureError::SystemInvariant("SHA-256 formatting produced an invalid digest"),
    )?;
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("structured content length exceeds ContentRef bounds"),
    )?;
    let source_format = structured_source_format_for_provider(provider).ok_or(
        CaptureError::SystemInvariant("supported structured provider must have a source format"),
    )?;
    let profile = verified_content_profile(
        provider,
        source_format,
        CompleteContentSourceFamily::Structured,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "supported structured message route must have a verified-content profile",
    ))?;
    let locator = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Structured,
        STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND,
        &value,
        native_record_id,
        record_sha256,
    )
    .ok_or(CaptureError::SystemInvariant(
        "structured complete-content locator exceeds its bounded schema",
    ))?;
    attach_verified_content_locator(&mut event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("verified-content locator collection is malformed"),
    )?;
    Ok(())
}

/// Attaches a verified address for a structured command/tool result while
/// persisting only its compact `ContentRef` in the canonical event.
pub(crate) fn attach_structured_result_content_locator(
    provider: CaptureProvider,
    event: &mut ProviderEventEnvelope,
    source_record_ordinal: u64,
    source_record_subrecord_index: u32,
    native_record_id: &str,
    record_bytes: &[u8],
    content: &str,
) -> Result<()> {
    if !matches!(
        event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    ) {
        return Ok(());
    }
    if native_record_id.is_empty()
        || native_record_id.len() > STRUCTURED_MAX_NATIVE_ID_BYTES
        || native_record_id.chars().any(char::is_control)
    {
        return Err(CaptureError::InvalidPayload(
            "structured result-content native record identity is invalid".to_owned(),
        ));
    }
    let value = encode_structured_locator(
        provider,
        source_record_ordinal,
        source_record_subrecord_index,
        native_record_id,
    )?;
    let record_sha256 = CompleteContentBodyDigest::parse(digest_bytes(record_bytes)).ok_or(
        CaptureError::SystemInvariant("SHA-256 formatting produced an invalid digest"),
    )?;
    let content_ref = ContentRef::from_bytes(content.as_bytes()).ok_or(
        CaptureError::SystemInvariant("structured result length exceeds ContentRef bounds"),
    )?;
    let source_format = structured_source_format_for_provider(provider).ok_or(
        CaptureError::SystemInvariant("supported structured provider must have a source format"),
    )?;
    let profile = verified_content_profile(
        provider,
        source_format,
        CompleteContentSourceFamily::Structured,
        VerifiedContentRole::ResultBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "supported structured result route must have a verified-content profile",
    ))?;
    let locator = VerifiedContentLocatorV1::new(
        VerifiedContentRole::ResultBody,
        profile,
        content_ref.clone(),
        CompleteContentSourceFamily::Structured,
        STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND,
        &value,
        native_record_id,
        record_sha256,
    )
    .ok_or(CaptureError::SystemInvariant(
        "structured result-content locator exceeds its bounded schema",
    ))?;
    event
        .payload
        .as_object_mut()
        .ok_or(CaptureError::SystemInvariant(
            "provider result event payload must be an object",
        ))?
        .insert(
            "result_content_ref".to_owned(),
            serde_json::to_value(content_ref).map_err(CaptureError::Json)?,
        );
    attach_verified_content_locator(&mut event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("verified-content locator collection is malformed"),
    )?;
    Ok(())
}

/// Adds a local-only address for one authoritative Continue tool result.
///
/// The address binds both the canonical source coordinate and Continue's nested
/// history-item/tool-state coordinate without retaining the provider path or
/// result body.
#[allow(clippy::too_many_arguments)]
pub(crate) fn attach_continue_result_content_locator(
    event: &mut ProviderEventEnvelope,
    source_record_ordinal: u64,
    source_record_subrecord_index: u32,
    history_item_index: u32,
    tool_state_index: u32,
    native_record_id: &str,
    record_bytes: &[u8],
    result_body: &str,
) -> Result<()> {
    if !matches!(
        event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    ) {
        return Ok(());
    }
    if native_record_id.is_empty()
        || native_record_id.len() > STRUCTURED_MAX_NATIVE_ID_BYTES
        || native_record_id.chars().any(char::is_control)
    {
        return Err(CaptureError::InvalidPayload(
            "Continue result-content native record identity is invalid".to_owned(),
        ));
    }
    let value = encode_structured_result_locator(
        CaptureProvider::Continue,
        source_record_ordinal,
        source_record_subrecord_index,
        history_item_index,
        tool_state_index,
        native_record_id,
    )?;
    let record_sha256 = CompleteContentBodyDigest::parse(digest_bytes(record_bytes)).ok_or(
        CaptureError::SystemInvariant("SHA-256 formatting produced an invalid digest"),
    )?;
    if let Some(payload) = event.payload.as_object_mut() {
        // The Store's fallback event hash is computed from this transient
        // normalized payload before result compaction. Binding it to the whole
        // structured record lets append rewrites refresh the locator metadata
        // while reconciliation preserves the stable event UUID.
        payload.insert(
            "result_source_record_sha256".to_owned(),
            Value::String(record_sha256.as_str().to_owned()),
        );
    }
    let content_ref = ContentRef::from_bytes(result_body.as_bytes()).ok_or(
        CaptureError::SystemInvariant("Continue result length exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        CaptureProvider::Continue,
        "continue_cli_sessions_json",
        CompleteContentSourceFamily::Structured,
        VerifiedContentRole::ResultBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Continue result route must have a verified-content profile",
    ))?;
    let locator = VerifiedContentLocatorV1::new(
        VerifiedContentRole::ResultBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Structured,
        STRUCTURED_RESULT_CONTENT_LOCATOR_KIND,
        &value,
        native_record_id,
        record_sha256,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Continue result-content locator exceeds its bounded schema",
    ))?;
    attach_verified_content_locator(&mut event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("verified-content locator collection is malformed"),
    )?;
    Ok(())
}

pub(super) fn structured_source_format(provider: CaptureProvider, source_format: &str) -> bool {
    verified_content_route_supported(
        provider,
        source_format,
        CompleteContentSourceFamily::Structured,
        VerifiedContentRole::MessageBody,
    )
}

fn structured_source_format_for_provider(provider: CaptureProvider) -> Option<&'static str> {
    VERIFIED_CONTENT_ROUTES
        .iter()
        .find(|route| {
            route.provider == provider
                && route.role == VerifiedContentRole::MessageBody
                && verified_content_route_supported(
                    route.provider,
                    route.source_format,
                    CompleteContentSourceFamily::Structured,
                    route.role,
                )
        })
        .map(|route| route.source_format)
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

pub(super) fn encode_structured_locator(
    provider: CaptureProvider,
    ordinal: u64,
    subrecord: u32,
    native_id: &str,
) -> Result<Vec<u8>> {
    let provider = provider.as_str().as_bytes();
    let provider_len = u8::try_from(provider.len())
        .map_err(|_| CaptureError::SystemInvariant("provider identity exceeds locator bounds"))?;
    let native_id_bytes = native_id.as_bytes();
    let native_len = u16::try_from(native_id_bytes.len()).map_err(|_| {
        CaptureError::InvalidPayload(
            "structured complete-content native record identity is too long".to_owned(),
        )
    })?;
    let mut value = Vec::with_capacity(
        STRUCTURED_LOCATOR_MAGIC.len() + 1 + provider.len() + 8 + 4 + 2 + native_id_bytes.len(),
    );
    value.extend_from_slice(STRUCTURED_LOCATOR_MAGIC);
    value.push(provider_len);
    value.extend_from_slice(provider);
    value.extend_from_slice(&ordinal.to_be_bytes());
    value.extend_from_slice(&subrecord.to_be_bytes());
    value.extend_from_slice(&native_len.to_be_bytes());
    value.extend_from_slice(native_id_bytes);
    Ok(value)
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

#[allow(clippy::too_many_arguments)]
fn encode_structured_result_locator(
    provider: CaptureProvider,
    ordinal: u64,
    source_subrecord: u32,
    history_item: u32,
    tool_state: u32,
    native_id: &str,
) -> Result<Vec<u8>> {
    let provider = provider.as_str().as_bytes();
    let provider_len = u8::try_from(provider.len())
        .map_err(|_| CaptureError::SystemInvariant("provider identity exceeds locator bounds"))?;
    let native_id_bytes = native_id.as_bytes();
    let native_len = u16::try_from(native_id_bytes.len()).map_err(|_| {
        CaptureError::InvalidPayload(
            "structured result-content native record identity is too long".to_owned(),
        )
    })?;
    let mut value = Vec::with_capacity(
        STRUCTURED_RESULT_LOCATOR_MAGIC.len()
            + 1
            + provider.len()
            + 8
            + 4
            + 4
            + 4
            + 2
            + native_id_bytes.len(),
    );
    value.extend_from_slice(STRUCTURED_RESULT_LOCATOR_MAGIC);
    value.push(provider_len);
    value.extend_from_slice(provider);
    value.extend_from_slice(&ordinal.to_be_bytes());
    value.extend_from_slice(&source_subrecord.to_be_bytes());
    value.extend_from_slice(&history_item.to_be_bytes());
    value.extend_from_slice(&tool_state.to_be_bytes());
    value.extend_from_slice(&native_len.to_be_bytes());
    value.extend_from_slice(native_id_bytes);
    Ok(value)
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
