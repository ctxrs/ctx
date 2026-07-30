//! Byte-range complete-message recovery for newline-delimited JSON sources.

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventType};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    verified_content_address_supported, verified_content_route_supported,
    CompleteContentBodyDigest, CompleteContentError, CompleteContentErrorKind,
    CompleteContentHashAuthority, CompleteContentResolver, CompleteContentSourceFamily,
    CompleteContentSourceLocator, CompleteMessage, CompleteMessageRequest, SourceVerification,
    VerifiedContentRole, COMPLETE_CONTENT_MAX_BODY_BYTES,
};
use crate::provider::codex::events::{
    codex_content_text, codex_message_event, codex_session_line_timestamp,
};
use crate::provider::providers::native_jsonl::{
    direct_jsonl_complete_message_provider_event_hash, native_jsonl_event_id,
    native_jsonl_event_text, native_jsonl_event_type, native_jsonl_normalized_payload,
    qoder_complete_content_message_record,
};
use crate::{
    compute_payload_hash, CODEBUDDY_SOURCE_FORMAT, CODEX_SESSION_SOURCE_FORMAT,
    CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT, JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
    KIMI_CODE_CLI_SOURCE_FORMAT, MISTRAL_VIBE_SOURCE_FORMAT, OPENCLAW_SOURCE_FORMAT,
};

mod junie;
mod mux;

pub(crate) use junie::valid_junie_record_set_locator;

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

pub(crate) use mux::valid_mux_locator;

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

    #[cfg(test)]
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
            || (request.provider != CaptureProvider::Cursor
                && request.source_record_subrecord_index != 0)
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
    if request.provider == CaptureProvider::Cursor
        && request.source_format == CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT
    {
        let (text, native_record_id, provider_event_hash) =
            crate::provider::providers::cursor::cursor_complete_content_message_record(
                &value,
                request.source_record_ordinal,
                request.source_record_subrecord_index,
                &request.indexed_text,
            )
            .ok_or_else(|| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
        if request.expected_native_record_id.as_deref() != Some(native_record_id.as_str())
            || request.expected_hash_authority
                != CompleteContentHashAuthority::NormalizedPayloadFallback
            || request.expected_provider_event_hash != provider_event_hash
        {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
        return CompleteMessage::verified(request, text, SourceVerification::VERIFIED);
    }
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
        if value.get("type").and_then(Value::as_str) != Some("response_item") {
            return None;
        }
        let payload = value.get("payload")?;
        let fallback = DateTime::<Utc>::from_timestamp(0, 0)?;
        let timestamp = codex_session_line_timestamp(value, fallback).ok()?;
        codex_message_event(payload, line_number, timestamp)?;
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
    if provider == CaptureProvider::Qoder && source_format == crate::QODER_SOURCE_FORMAT {
        return qoder_complete_content_message_record(value, line_number);
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
            if matches!(
                request.provider,
                CaptureProvider::Antigravity
                    | CaptureProvider::CopilotCli
                    | CaptureProvider::FactoryAiDroid
                    | CaptureProvider::Gemini
                    | CaptureProvider::Qoder
                    | CaptureProvider::QwenCode
                    | CaptureProvider::Tabnine
                    | CaptureProvider::Windsurf
            ) {
                let observed = direct_jsonl_complete_message_provider_event_hash(
                    request.provider,
                    &request.source_format,
                    value,
                    request.source_record_ordinal,
                    line_number,
                );
                return if observed.as_deref() == Some(request.expected_provider_event_hash.as_str())
                {
                    Ok(())
                } else {
                    Err(error(
                        request,
                        CompleteContentErrorKind::ContentVerificationFailed,
                    ))
                };
            }
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
    if provider == CaptureProvider::CodeBuddy && source_format == CODEBUDDY_SOURCE_FORMAT {
        return Some(value.clone());
    }
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
    if provider == CaptureProvider::KimiCodeCli && source_format == KIMI_CODE_CLI_SOURCE_FORMAT {
        return crate::provider::providers::kimi::kimi_complete_content_normalized_payload(value);
    }
    if provider == CaptureProvider::Junie && source_format == JUNIE_SESSION_EVENTS_SOURCE_FORMAT {
        let prompt = value.get("prompt").and_then(Value::as_str)?;
        return Some(serde_json::json!({
            "text": prompt,
            "body": {"kind": "UserPromptEvent", "prompt": prompt},
        }));
    }
    Some(native_jsonl_normalized_payload(
        provider,
        value,
        line_number,
    ))
}

fn domain_digest(domain: &[u8], value: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
    digest.finalize().into()
}

#[cfg(test)]
fn digest_bytes(bytes: &[u8]) -> CompleteContentBodyDigest {
    CompleteContentBodyDigest::parse(format!("{:x}", Sha256::digest(bytes)))
        .expect("SHA-256 formatting is valid")
}

fn error(request: &CompleteMessageRequest, kind: CompleteContentErrorKind) -> CompleteContentError {
    CompleteContentError::new(kind, request.event_id)
}

#[cfg(test)]
mod tests;
