//! Byte-range complete-message recovery for newline-delimited JSON sources.

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, ContentRef, EventType, ProviderEventEnvelope};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    attach_verified_content_locator, verified_content_address_supported, verified_content_profile,
    verified_content_route_supported, CompleteContentBodyDigest, CompleteContentError,
    CompleteContentErrorKind, CompleteContentHashAuthority, CompleteContentResolver,
    CompleteContentSourceFamily, CompleteContentSourceLocator, CompleteMessage,
    CompleteMessageRequest, SourceVerification, VerifiedContentLocatorV1, VerifiedContentRole,
    COMPLETE_CONTENT_MAX_BODY_BYTES,
};
use crate::captured_batch::jsonl::jsonl_locator_range;
use crate::captured_batch::{CapturedRecord, CapturedRecordPayload};
use crate::provider::codex::events::{
    codex_content_text, codex_message_event, codex_session_line_timestamp,
};
use crate::provider::providers::native_jsonl::result_content::native_jsonl_result_content_profile;
use crate::provider::providers::native_jsonl::{
    native_jsonl_event, native_jsonl_event_id, native_jsonl_event_text, native_jsonl_event_type,
};
use crate::{
    compute_payload_hash, CaptureError, Result as CaptureResult, CODEBUDDY_SOURCE_FORMAT,
    CODEX_SESSION_SOURCE_FORMAT, JUNIE_SESSION_EVENTS_SOURCE_FORMAT, KIMI_CODE_CLI_SOURCE_FORMAT,
    MISTRAL_VIBE_SOURCE_FORMAT, OPENCLAW_SOURCE_FORMAT, PROVIDER_MAX_TEXT_CHARS,
};

mod junie;
mod mux;
mod results;
pub(crate) use results::result_content_and_id;

pub(crate) use junie::valid_junie_record_set_locator;
pub(crate) use junie::{
    attach_junie_record_set_locator, JunieRecordSetBinding, JunieRecordSetTarget,
};

pub const JSONL_COMPLETE_CONTENT_LOCATOR_KIND: &str = "jsonl-range-v1";
/// Exact routes use a distinct kind because the legacy range locator deliberately
/// permits source relocation and append-only growth. Record/body hashes alone do
/// not bind byte-identical JSONL to its path or provider-owned auxiliary state.
pub const EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND: &str = "jsonl-exact-range-v1";
const JSONL_RANGE_LOCATOR_BYTES: usize = 16;
const EXACT_JSONL_LOCATOR_BYTES: usize = JSONL_RANGE_LOCATOR_BYTES + 64;
const SOURCE_REVISION_DIGEST_DOMAIN: &[u8] = b"ctx-complete-content-source-revision-v1\0";
const PATH_IDENTITY_DIGEST_DOMAIN: &[u8] = b"ctx-complete-content-path-identity-v1\0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactJsonlSourceBinding {
    source_revision_digest: [u8; 32],
    path_identity_digest: [u8; 32],
}

impl ExactJsonlSourceBinding {
    pub(crate) fn new(source_revision: &str, path_identity: &str) -> Self {
        Self {
            source_revision_digest: domain_digest(SOURCE_REVISION_DIGEST_DOMAIN, source_revision),
            path_identity_digest: domain_digest(PATH_IDENTITY_DIGEST_DOMAIN, path_identity),
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct JsonlCompleteContentResolver;

impl JsonlCompleteContentResolver {
    pub const fn new() -> Self {
        Self
    }
}

impl CompleteContentResolver for JsonlCompleteContentResolver {
    fn family(&self) -> CompleteContentSourceFamily {
        CompleteContentSourceFamily::Jsonl
    }

    fn supports(&self, provider: CaptureProvider, source_format: &str) -> bool {
        verified_content_route_supported(
            provider,
            source_format,
            CompleteContentSourceFamily::Jsonl,
            VerifiedContentRole::MessageBody,
        )
    }

    fn resolve(
        &self,
        requests: &[CompleteMessageRequest],
    ) -> Result<Vec<CompleteMessage>, CompleteContentError> {
        let Some(first) = requests.first() else {
            return Ok(Vec::new());
        };
        if first.provider == CaptureProvider::Mux {
            return mux::resolve_messages(requests);
        }
        if first.provider == CaptureProvider::Junie {
            return junie::resolve_messages(requests);
        }
        if !self.supports(first.provider, &first.source_format) {
            return Err(error(first, CompleteContentErrorKind::HydrationUnsupported));
        }
        validate_batch(requests)?;
        let decoded_locators = requests
            .iter()
            .map(|request| {
                request
                    .source_locator
                    .as_ref()
                    .and_then(DecodedJsonlLocator::decode)
                    .ok_or_else(|| error(request, CompleteContentErrorKind::HydrationUnsupported))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let exact_binding = if verified_content_address_supported(
            first.provider,
            &first.source_format,
            CompleteContentSourceFamily::Jsonl,
            VerifiedContentRole::MessageBody,
            EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        ) {
            first.source_access.exact_jsonl_binding().cloned()
        } else {
            None
        };

        let mut messages = Vec::with_capacity(requests.len());
        for (request, decoded) in requests.iter().zip(&decoded_locators) {
            if decoded.binding.as_ref() != exact_binding.as_ref() {
                return Err(error(request, CompleteContentErrorKind::SourceChanged));
            }
            let expected_record_digest = request
                .expected_record_digest
                .as_ref()
                .ok_or_else(|| error(request, CompleteContentErrorKind::HydrationUnsupported))?;
            let record = request.source_access.read_jsonl_record(
                decoded.range.byte_start,
                decoded.range.byte_end_exclusive,
                expected_record_digest,
                request.event_id,
            )?;
            let resolved = resolve_record(request, &record)?;
            messages.push(resolved);
        }
        first.source_access.revalidate_jsonl(first.event_id)?;
        Ok(messages)
    }
}

pub(crate) use mux::{attach_mux_verified_content_locator, valid_mux_locator};

/// Adds the local-only locator consumed by complete-message show.
///
/// Only truncated ordinary messages receive a locator. Untruncated messages
/// remain on the canonical fast path and all other content categories remain
/// explicitly ineligible.
pub(crate) fn attach_jsonl_complete_content_locator(
    event: &mut ProviderEventEnvelope,
    provider: CaptureProvider,
    source_format: &str,
    raw_value: &Value,
    record: &CapturedRecord,
    line_number: usize,
) -> CaptureResult<()> {
    if event.event_type != EventType::Message
        || !verified_content_address_supported(
            provider,
            source_format,
            CompleteContentSourceFamily::Jsonl,
            VerifiedContentRole::MessageBody,
            JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        )
    {
        return Ok(());
    }
    let Some((text, native_record_id)) =
        complete_message_text_and_id(provider, source_format, raw_value, line_number)
    else {
        return Ok(());
    };
    if text.chars().count() <= PROVIDER_MAX_TEXT_CHARS
        || text.len() > COMPLETE_CONTENT_MAX_BODY_BYTES
    {
        return Ok(());
    }
    let CapturedRecordPayload::NativeBytes(record_bytes) = record.payload() else {
        return Err(CaptureError::SystemInvariant(
            "JSONL complete-content locator requires native bytes",
        ));
    };
    let (byte_start, byte_end_exclusive) = jsonl_locator_range(record.locator())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let range = JsonlRange {
        byte_start,
        byte_end_exclusive,
    };
    let record_sha256 = digest_bytes(record_bytes);
    let Some(content_ref) = ContentRef::from_bytes(text.as_bytes()) else {
        return Ok(());
    };
    let Some(profile) = verified_content_profile(
        provider,
        source_format,
        CompleteContentSourceFamily::Jsonl,
        VerifiedContentRole::MessageBody,
    ) else {
        return Err(CaptureError::SystemInvariant(
            "supported JSONL message route must have a verified-content profile",
        ));
    };
    let Some(locator) = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Jsonl,
        JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        &range.encode(),
        native_record_id,
        record_sha256,
    ) else {
        return Ok(());
    };
    attach_verified_content_locator(&mut event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("verified-content locator collection is malformed"),
    )?;
    Ok(())
}

pub(crate) fn attach_exact_jsonl_complete_content_locator(
    event: &mut ProviderEventEnvelope,
    provider: CaptureProvider,
    source_format: &str,
    raw_value: &Value,
    record: &CapturedRecord,
    line_number: usize,
    binding: &ExactJsonlSourceBinding,
) -> CaptureResult<()> {
    if event.event_type != EventType::Message
        || !verified_content_address_supported(
            provider,
            source_format,
            CompleteContentSourceFamily::Jsonl,
            VerifiedContentRole::MessageBody,
            EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        )
    {
        return Ok(());
    }
    let Some((text, native_record_id)) =
        complete_message_text_and_id(provider, source_format, raw_value, line_number)
    else {
        return Ok(());
    };
    if text.chars().count() <= PROVIDER_MAX_TEXT_CHARS
        || text.len() > COMPLETE_CONTENT_MAX_BODY_BYTES
    {
        return Ok(());
    }
    let CapturedRecordPayload::NativeBytes(record_bytes) = record.payload() else {
        return Err(CaptureError::SystemInvariant(
            "exact JSONL complete-content locator requires native bytes",
        ));
    };
    let (byte_start, byte_end_exclusive) = jsonl_locator_range(record.locator())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let encoded = DecodedJsonlLocator {
        range: JsonlRange {
            byte_start,
            byte_end_exclusive,
        },
        binding: Some(binding.clone()),
    }
    .encode_exact();
    let Some(content_ref) = ContentRef::from_bytes(text.as_bytes()) else {
        return Ok(());
    };
    let Some(profile) = verified_content_profile(
        provider,
        source_format,
        CompleteContentSourceFamily::Jsonl,
        VerifiedContentRole::MessageBody,
    ) else {
        return Err(CaptureError::SystemInvariant(
            "supported exact JSONL message route must have a verified-content profile",
        ));
    };
    let Some(locator) = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Jsonl,
        EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        &encoded,
        native_record_id,
        digest_bytes(record_bytes),
    ) else {
        return Ok(());
    };
    attach_verified_content_locator(&mut event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("verified-content locator collection is malformed"),
    )?;
    Ok(())
}

/// Adds a local-only immutable-record locator for a Codex result body.
///
/// This locator is intentionally separate from complete-message eligibility;
/// public complete show remains message-only.
pub(crate) fn attach_codex_result_content_locator(
    event: &mut ProviderEventEnvelope,
    content_ref: &ContentRef,
    record: &CapturedRecord,
    line_number: usize,
) -> CaptureResult<()> {
    let attached = attach_jsonl_result_content_locator_with_ref(
        event,
        content_ref,
        CaptureProvider::Codex,
        CODEX_SESSION_SOURCE_FORMAT,
        format!("line-{line_number}"),
        record,
        None,
    )?;
    if !attached {
        clear_result_content_ref(event);
    }
    Ok(())
}

/// Adds a local-only result locator for a direct native JSONL provider. The
/// normalizer has already extracted the exact result bytes and computed the
/// sole `ContentRef`; this path reuses that reference and never re-extracts or
/// re-hashes the result body.
pub(crate) fn attach_native_jsonl_result_content_locator(
    event: &mut ProviderEventEnvelope,
    provider: CaptureProvider,
    source_format: &str,
    raw_value: &Value,
    record: &CapturedRecord,
    line_number: usize,
    content_ref: Option<&ContentRef>,
) -> CaptureResult<()> {
    if event.event_type != EventType::ToolOutput {
        return Ok(());
    }
    let Some(content_ref) = content_ref else {
        return Ok(());
    };
    let Some(payload) = event.payload.as_object() else {
        return Ok(());
    };
    if payload.contains_key("result_content_ref") {
        return Err(CaptureError::SystemInvariant(
            "native JSONL result ContentRef was published before locator validation",
        ));
    }
    let content_ref_value = serde_json::to_value(content_ref).map_err(|_| {
        CaptureError::SystemInvariant("native JSONL result ContentRef is malformed")
    })?;
    let Some(profile) = native_jsonl_result_content_profile(provider) else {
        return Ok(());
    };
    if verified_content_profile(
        provider,
        source_format,
        CompleteContentSourceFamily::Jsonl,
        VerifiedContentRole::ResultBody,
    ) != Some(profile)
    {
        return Ok(());
    }
    let attached = attach_jsonl_result_content_locator_with_ref(
        event,
        content_ref,
        provider,
        source_format,
        native_jsonl_event_id(provider, raw_value, line_number),
        record,
        None,
    )?;
    if attached {
        event
            .payload
            .as_object_mut()
            .expect("native JSONL result payload was validated as an object")
            .insert("result_content_ref".to_owned(), content_ref_value);
    }
    Ok(())
}

fn attach_jsonl_result_content_locator_with_ref(
    event: &mut ProviderEventEnvelope,
    content_ref: &ContentRef,
    provider: CaptureProvider,
    source_format: &str,
    native_record_id: String,
    record: &CapturedRecord,
    binding: Option<&ExactJsonlSourceBinding>,
) -> CaptureResult<bool> {
    if !matches!(
        event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    ) {
        return Ok(false);
    }
    let CapturedRecordPayload::NativeBytes(record_bytes) = record.payload() else {
        return Err(CaptureError::SystemInvariant(
            "JSONL result-content locator requires native bytes",
        ));
    };
    let (byte_start, byte_end_exclusive) = jsonl_locator_range(record.locator())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let range = JsonlRange {
        byte_start,
        byte_end_exclusive,
    };
    let Some(profile) = verified_content_profile(
        provider,
        source_format,
        CompleteContentSourceFamily::Jsonl,
        VerifiedContentRole::ResultBody,
    ) else {
        return Ok(false);
    };
    let (kind, encoded) = match binding {
        Some(binding) => (
            EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
            DecodedJsonlLocator {
                range,
                binding: Some(binding.clone()),
            }
            .encode_exact()
            .to_vec(),
        ),
        None => (JSONL_COMPLETE_CONTENT_LOCATOR_KIND, range.encode().to_vec()),
    };
    let Some(locator) = VerifiedContentLocatorV1::new(
        VerifiedContentRole::ResultBody,
        profile,
        content_ref.clone(),
        CompleteContentSourceFamily::Jsonl,
        kind,
        &encoded,
        native_record_id,
        digest_bytes(record_bytes),
    ) else {
        return Ok(false);
    };
    attach_verified_content_locator(&mut event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("verified-content locator collection is malformed"),
    )?;
    Ok(true)
}

/// Adds a verified result-body locator and publishes its content reference only
/// after every locator field has validated.
pub(crate) fn attach_jsonl_result_content_locator(
    event: &mut ProviderEventEnvelope,
    provider: CaptureProvider,
    source_format: &str,
    content: &str,
    native_record_id: &str,
    record: &CapturedRecord,
) -> CaptureResult<()> {
    attach_standalone_jsonl_result_content_locator(
        event,
        provider,
        source_format,
        content,
        native_record_id,
        record,
        None,
    )
}

/// Exact-source variant used by providers whose result meaning depends on
/// provider-owned auxiliary state in addition to the addressed JSONL record.
pub(crate) fn attach_exact_jsonl_result_content_locator(
    event: &mut ProviderEventEnvelope,
    provider: CaptureProvider,
    source_format: &str,
    content: &str,
    native_record_id: &str,
    record: &CapturedRecord,
    binding: &ExactJsonlSourceBinding,
) -> CaptureResult<()> {
    attach_standalone_jsonl_result_content_locator(
        event,
        provider,
        source_format,
        content,
        native_record_id,
        record,
        Some(binding),
    )
}

fn attach_standalone_jsonl_result_content_locator(
    event: &mut ProviderEventEnvelope,
    provider: CaptureProvider,
    source_format: &str,
    content: &str,
    native_record_id: &str,
    record: &CapturedRecord,
    binding: Option<&ExactJsonlSourceBinding>,
) -> CaptureResult<()> {
    let Some(content_ref) = ContentRef::from_bytes(content.as_bytes()) else {
        return Ok(());
    };
    let content_ref_value = serde_json::to_value(&content_ref)
        .map_err(|_| CaptureError::SystemInvariant("result ContentRef is malformed"))?;
    let payload = event
        .payload
        .as_object()
        .ok_or(CaptureError::SystemInvariant(
            "provider result event payload must be an object",
        ))?;
    if payload.contains_key("result_content_ref") {
        return Err(CaptureError::SystemInvariant(
            "result ContentRef was published before locator validation",
        ));
    }
    let attached = attach_jsonl_result_content_locator_with_ref(
        event,
        &content_ref,
        provider,
        source_format,
        native_record_id.to_owned(),
        record,
        binding,
    )?;
    if attached {
        event
            .payload
            .as_object_mut()
            .expect("result payload was validated as an object")
            .insert("result_content_ref".to_owned(), content_ref_value);
    }
    Ok(())
}

fn clear_result_content_ref(event: &mut ProviderEventEnvelope) {
    if let Some(payload) = event.payload.as_object_mut() {
        payload.insert("result_content_ref".to_owned(), Value::Null);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DecodedJsonlLocator {
    range: JsonlRange,
    binding: Option<ExactJsonlSourceBinding>,
}

impl DecodedJsonlLocator {
    fn decode(locator: &CompleteContentSourceLocator) -> Option<Self> {
        if locator.kind() == JSONL_COMPLETE_CONTENT_LOCATOR_KIND {
            return JsonlRange::decode(locator).map(|range| Self {
                range,
                binding: None,
            });
        }
        if locator.kind() != EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND
            || locator.value().len() != EXACT_JSONL_LOCATOR_BYTES
        {
            return None;
        }
        let value = locator.value();
        let range = JsonlRange::decode_bytes(&value[..JSONL_RANGE_LOCATOR_BYTES])?;
        Some(Self {
            range,
            binding: Some(ExactJsonlSourceBinding {
                source_revision_digest: value[16..48].try_into().ok()?,
                path_identity_digest: value[48..80].try_into().ok()?,
            }),
        })
    }

    fn encode_exact(&self) -> [u8; EXACT_JSONL_LOCATOR_BYTES] {
        let mut value = [0_u8; EXACT_JSONL_LOCATOR_BYTES];
        value[..16].copy_from_slice(&self.range.encode());
        let binding = self.binding.as_ref().expect("exact locator has a binding");
        value[16..48].copy_from_slice(&binding.source_revision_digest);
        value[48..80].copy_from_slice(&binding.path_identity_digest);
        value
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct JsonlRange {
    byte_start: u64,
    byte_end_exclusive: u64,
}

impl JsonlRange {
    fn length(self) -> Option<usize> {
        self.byte_end_exclusive
            .checked_sub(self.byte_start)
            .and_then(|length| usize::try_from(length).ok())
    }

    fn encode(self) -> [u8; JSONL_RANGE_LOCATOR_BYTES] {
        let mut value = [0_u8; JSONL_RANGE_LOCATOR_BYTES];
        value[..8].copy_from_slice(&self.byte_start.to_be_bytes());
        value[8..].copy_from_slice(&self.byte_end_exclusive.to_be_bytes());
        value
    }

    fn decode(locator: &CompleteContentSourceLocator) -> Option<Self> {
        if locator.kind() != JSONL_COMPLETE_CONTENT_LOCATOR_KIND
            || locator.value().len() != JSONL_RANGE_LOCATOR_BYTES
        {
            return None;
        }
        Self::decode_bytes(locator.value())
    }

    fn decode_bytes(value: &[u8]) -> Option<Self> {
        let byte_start = u64::from_be_bytes(value[..8].try_into().ok()?);
        let byte_end_exclusive = u64::from_be_bytes(value[8..].try_into().ok()?);
        (byte_start < byte_end_exclusive).then_some(Self {
            byte_start,
            byte_end_exclusive,
        })
    }
}

fn validate_batch(requests: &[CompleteMessageRequest]) -> Result<(), CompleteContentError> {
    let first = &requests[0];
    let mut prior = None;
    for request in requests {
        if request.provider != first.provider
            || request.source_format != first.source_format
            || request.source_access != first.source_access
            || request.source_access.family() != CompleteContentSourceFamily::Jsonl
            || request.source_record_subrecord_index != 0
        {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
        let position = (
            request.source_record_ordinal,
            request.source_record_subrecord_index,
        );
        if prior.is_some_and(|prior| prior >= position) {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
        prior = Some(position);
        if request.expected_native_record_id.is_none()
            || request.expected_record_digest.is_none()
            || request.expected_content_ref.is_none()
        {
            return Err(error(
                request,
                CompleteContentErrorKind::HydrationUnsupported,
            ));
        }
    }
    Ok(())
}

fn resolve_record(
    request: &CompleteMessageRequest,
    record: &[u8],
) -> Result<CompleteMessage, CompleteContentError> {
    let value = serde_json::from_slice::<Value>(record)
        .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
    let line_number = usize::try_from(request.source_record_ordinal)
        .ok()
        .and_then(|ordinal| ordinal.checked_add(1))
        .ok_or_else(|| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
    let (text, native_record_id) = complete_message_text_and_id(
        request.provider,
        &request.source_format,
        &value,
        line_number,
    )
    .ok_or_else(|| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
    if request.expected_native_record_id.as_deref() != Some(native_record_id.as_str()) {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    verify_provider_event_hash(request, &value, line_number, &native_record_id)?;
    CompleteMessage::verified(request, text, SourceVerification::VERIFIED)
}

fn complete_message_text_and_id(
    provider: CaptureProvider,
    source_format: &str,
    value: &Value,
    line_number: usize,
) -> Option<(String, String)> {
    if provider == CaptureProvider::Codex && source_format == CODEX_SESSION_SOURCE_FORMAT {
        let payload = value.get("payload")?;
        if value.get("type").and_then(Value::as_str) != Some("response_item")
            || payload.get("type").and_then(Value::as_str) != Some("message")
            || !matches!(
                payload.get("role").and_then(Value::as_str),
                Some("user" | "assistant" | "developer" | "system")
            )
        {
            return None;
        }
        let text = payload.get("content").and_then(codex_content_text)?;
        return Some((text, format!("line-{line_number}")));
    }
    if provider == CaptureProvider::Claude && source_format == crate::CLAUDE_PROJECTS_SOURCE_FORMAT
    {
        return crate::provider::providers::claude::claude_complete_content_message_record(
            value,
            line_number,
        );
    }
    if provider == CaptureProvider::Pi
        && source_format == crate::provider::providers::pi::PI_SOURCE_FORMAT
    {
        return crate::provider::providers::pi::pi_complete_content_message_record(
            value,
            line_number,
        );
    }
    if provider == CaptureProvider::CodeBuddy && source_format == CODEBUDDY_SOURCE_FORMAT {
        return crate::provider::providers::codebuddy::codebuddy_cli_complete_content_record(
            value,
            line_number,
        );
    }
    if provider == CaptureProvider::MistralVibe && source_format == MISTRAL_VIBE_SOURCE_FORMAT {
        return crate::provider::providers::mistral_vibe::mistral_vibe_complete_content_record(
            value,
            line_number,
        );
    }
    if provider == CaptureProvider::OpenClaw && source_format == OPENCLAW_SOURCE_FORMAT {
        return crate::provider::providers::openclaw::openclaw_complete_content_record(
            value,
            line_number,
        );
    }
    if provider == CaptureProvider::KimiCodeCli && source_format == KIMI_CODE_CLI_SOURCE_FORMAT {
        return crate::provider::providers::kimi::kimi_complete_content_record(value, line_number);
    }
    if provider == CaptureProvider::Junie && source_format == JUNIE_SESSION_EVENTS_SOURCE_FORMAT {
        if value.get("kind").and_then(Value::as_str) != Some("UserPromptEvent") {
            return None;
        }
        return value
            .get("prompt")
            .and_then(Value::as_str)
            .map(|text| (text.to_owned(), format!("line-{line_number}")));
    }
    if !verified_content_route_supported(
        provider,
        source_format,
        CompleteContentSourceFamily::Jsonl,
        VerifiedContentRole::MessageBody,
    ) || native_jsonl_event_type(provider, value) != EventType::Message
    {
        return None;
    }
    let event_type = EventType::Message;
    let entry_type =
        crate::provider::providers::native_jsonl::native_jsonl_entry_type(provider, value);
    Some((
        native_jsonl_event_text(provider, value, event_type, &entry_type),
        native_jsonl_event_id(provider, value, line_number),
    ))
}

fn verify_provider_event_hash(
    request: &CompleteMessageRequest,
    value: &Value,
    line_number: usize,
    native_record_id: &str,
) -> Result<(), CompleteContentError> {
    let verified = match request.expected_hash_authority {
        CompleteContentHashAuthority::ProviderSupplied => {
            let expected = if request.provider == CaptureProvider::CodeBuddy
                && request.source_format == CODEBUDDY_SOURCE_FORMAT
            {
                request
                    .provider_session_id
                    .as_deref()
                    .map(|session| format!("{session}:{native_record_id}"))
            } else if request.provider == CaptureProvider::Junie
                && request.source_format == JUNIE_SESSION_EVENTS_SOURCE_FORMAT
            {
                Some(format!("line:{line_number}:user"))
            } else {
                Some(native_record_id.to_owned())
            };
            expected.as_deref() == Some(request.expected_provider_event_hash.as_str())
        }
        CompleteContentHashAuthority::NormalizedPayloadFallback => {
            let normalized = normalized_message_payload(
                request.provider,
                &request.source_format,
                value,
                line_number,
            )
            .ok_or_else(|| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
            compute_payload_hash(&normalized)
                .ok()
                .is_some_and(|hash| hash == request.expected_provider_event_hash)
        }
    };
    if verified {
        Ok(())
    } else {
        Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ))
    }
}

fn normalized_message_payload(
    provider: CaptureProvider,
    source_format: &str,
    value: &Value,
    line_number: usize,
) -> Option<Value> {
    if provider == CaptureProvider::Codex && source_format == CODEX_SESSION_SOURCE_FORMAT {
        let fallback = DateTime::<Utc>::from_timestamp(0, 0)?;
        let timestamp = codex_session_line_timestamp(value, fallback).ok()?;
        return codex_message_event(value.get("payload")?, line_number, timestamp)
            .map(|event| event.payload);
    }
    if provider == CaptureProvider::Claude && source_format == crate::CLAUDE_PROJECTS_SOURCE_FORMAT
    {
        return crate::provider::providers::claude::claude_complete_content_normalized_payload(
            value,
            line_number,
        );
    }
    if provider == CaptureProvider::Pi
        && source_format == crate::provider::providers::pi::PI_SOURCE_FORMAT
    {
        return crate::provider::providers::pi::pi_complete_content_normalized_payload(value);
    }
    if provider == CaptureProvider::OpenClaw && source_format == OPENCLAW_SOURCE_FORMAT {
        let session = "complete-content-verification";
        return Some(
            crate::provider::providers::openclaw::openclaw_event(
                session,
                line_number.saturating_sub(1) as u64,
                line_number,
                value,
                DateTime::<Utc>::from_timestamp(0, 0)?,
            )
            .payload,
        );
    }
    if provider == CaptureProvider::Junie && source_format == JUNIE_SESSION_EVENTS_SOURCE_FORMAT {
        let prompt = value.get("prompt").and_then(Value::as_str)?;
        return Some(serde_json::json!({
            "text": prompt,
            "body": {"kind": "UserPromptEvent", "prompt": prompt},
        }));
    }
    let occurred_at = DateTime::<Utc>::from_timestamp(0, 0)?;
    native_jsonl_event(provider, source_format, value, line_number, occurred_at)
        .map(|event| event.payload)
}

fn domain_digest(domain: &[u8], value: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
    digest.finalize().into()
}

fn digest_bytes(bytes: &[u8]) -> CompleteContentBodyDigest {
    CompleteContentBodyDigest::parse(format!("{:x}", Sha256::digest(bytes)))
        .expect("SHA-256 formatting is valid")
}

fn error(request: &CompleteMessageRequest, kind: CompleteContentErrorKind) -> CompleteContentError {
    CompleteContentError::new(kind, request.event_id)
}

#[cfg(test)]
mod tests;
