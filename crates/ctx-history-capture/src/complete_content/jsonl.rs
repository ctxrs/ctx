//! Byte-range complete-message recovery for newline-delimited JSON sources.

use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
    time::SystemTime,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, ContentRef, EventType, ProviderEventEnvelope};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    CompleteContentBodyDigest, CompleteContentError, CompleteContentErrorKind,
    CompleteContentHashAuthority, CompleteContentResolver, CompleteContentSourceFamily,
    CompleteContentSourceLocator, CompleteMessage, CompleteMessageRequest,
    PersistedCompleteContentLocatorV1, SourceVerification, COMPLETE_CONTENT_LOCATOR_METADATA_KEY,
    COMPLETE_CONTENT_MAX_BODY_BYTES, RESULT_CONTENT_LOCATOR_METADATA_KEY,
};
use crate::captured_batch::jsonl::jsonl_locator_range;
use crate::captured_batch::{CapturedRecord, CapturedRecordPayload};
use crate::provider::codex::events::{
    codex_content_text, codex_message_event, codex_session_line_timestamp,
};
use crate::provider::providers::native_jsonl::{
    native_jsonl_event, native_jsonl_event_id, native_jsonl_event_text, native_jsonl_event_type,
};
use crate::{
    compute_payload_hash, CaptureError, Result as CaptureResult, CODEBUDDY_SOURCE_FORMAT,
    CODEX_SESSION_SOURCE_FORMAT, KIMI_CODE_CLI_SOURCE_FORMAT, MISTRAL_VIBE_SOURCE_FORMAT,
    OPENCLAW_SOURCE_FORMAT, PROVIDER_MAX_TEXT_CHARS,
};

mod results;

pub const JSONL_COMPLETE_CONTENT_LOCATOR_KIND: &str = "jsonl-range-v1";
/// Exact routes use a distinct kind because the legacy range locator deliberately
/// permits source relocation and append-only growth. Record/body hashes alone do
/// not bind byte-identical JSONL to its path or provider-owned auxiliary state.
pub const EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND: &str = "jsonl-exact-range-v1";
const JSONL_RANGE_LOCATOR_BYTES: usize = 16;
const EXACT_JSONL_LOCATOR_BYTES: usize = JSONL_RANGE_LOCATOR_BYTES + 64;
const SOURCE_REVISION_DIGEST_DOMAIN: &[u8] = b"ctx-complete-content-source-revision-v1\0";
const PATH_IDENTITY_DIGEST_DOMAIN: &[u8] = b"ctx-complete-content-path-identity-v1\0";

const SUPPORTED_JSONL_SOURCES: &[(CaptureProvider, &str)] = &[
    (CaptureProvider::Codex, CODEX_SESSION_SOURCE_FORMAT),
    (
        CaptureProvider::Antigravity,
        crate::ANTIGRAVITY_CLI_SOURCE_FORMAT,
    ),
    (CaptureProvider::Gemini, crate::GEMINI_CLI_SOURCE_FORMAT),
    (CaptureProvider::Tabnine, crate::TABNINE_CLI_SOURCE_FORMAT),
    (
        CaptureProvider::FactoryAiDroid,
        crate::FACTORY_DROID_SOURCE_FORMAT,
    ),
    (
        CaptureProvider::Cursor,
        crate::CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
    ),
    (
        CaptureProvider::Windsurf,
        crate::WINDSURF_CASCADE_HOOK_TRANSCRIPT_SOURCE_FORMAT,
    ),
    (CaptureProvider::Qoder, crate::QODER_SOURCE_FORMAT),
    (
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
    ),
    (CaptureProvider::QwenCode, crate::QWEN_CODE_SOURCE_FORMAT),
    (CaptureProvider::CodeBuddy, CODEBUDDY_SOURCE_FORMAT),
    (CaptureProvider::MistralVibe, MISTRAL_VIBE_SOURCE_FORMAT),
    (CaptureProvider::OpenClaw, OPENCLAW_SOURCE_FORMAT),
    (CaptureProvider::KimiCodeCli, KIMI_CODE_CLI_SOURCE_FORMAT),
];

const EXACT_JSONL_SOURCES: &[(CaptureProvider, &str)] = &[
    (CaptureProvider::CodeBuddy, CODEBUDDY_SOURCE_FORMAT),
    (CaptureProvider::MistralVibe, MISTRAL_VIBE_SOURCE_FORMAT),
    (CaptureProvider::OpenClaw, OPENCLAW_SOURCE_FORMAT),
    (CaptureProvider::KimiCodeCli, KIMI_CODE_CLI_SOURCE_FORMAT),
];

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
        SUPPORTED_JSONL_SOURCES.contains(&(provider, source_format))
    }

    fn resolve(
        &self,
        requests: &[CompleteMessageRequest],
    ) -> Result<Vec<CompleteMessage>, CompleteContentError> {
        let Some(first) = requests.first() else {
            return Ok(Vec::new());
        };
        if !self.supports(first.provider, &first.source_format) {
            return Err(error(first, CompleteContentErrorKind::HydrationUnsupported));
        }
        validate_batch(requests)?;
        let selected_path = selected_source_path(first)?;
        ensure_no_links(&selected_path, first)?;
        let exact_binding =
            if EXACT_JSONL_SOURCES.contains(&(first.provider, first.source_format.as_str())) {
                Some(observe_exact_source_binding(first, &selected_path)?)
            } else {
                None
            };
        let (mut file, frozen) = open_frozen_source(&selected_path, first)?;

        let mut messages = Vec::with_capacity(requests.len());
        for request in requests {
            let locator = request
                .source_locator
                .as_ref()
                .ok_or_else(|| error(request, CompleteContentErrorKind::HydrationUnsupported))?;
            let decoded = DecodedJsonlLocator::decode(locator)
                .ok_or_else(|| error(request, CompleteContentErrorKind::HydrationUnsupported))?;
            if decoded.binding.as_ref() != exact_binding.as_ref() {
                return Err(error(request, CompleteContentErrorKind::SourceChanged));
            }
            let record = read_record(&mut file, &frozen, decoded.range, request)?;
            let resolved = resolve_record(request, &record)?;
            messages.push(resolved);
        }

        if let Some(expected) = exact_binding.as_ref() {
            let current = observe_exact_source_binding(first, &selected_path)?;
            if &current != expected {
                return Err(error(first, CompleteContentErrorKind::SourceChanged));
            }
        }
        revalidate_open_source(&file, &selected_path, &frozen, first)?;
        Ok(messages)
    }
}

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
        || !SUPPORTED_JSONL_SOURCES.contains(&(provider, source_format))
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
    let body_sha256 = CompleteContentBodyDigest::from_text(&text);
    let Some(locator) = PersistedCompleteContentLocatorV1::new(
        CompleteContentSourceFamily::Jsonl,
        JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        &range.encode(),
        native_record_id,
        record_sha256,
        body_sha256,
    ) else {
        return Ok(());
    };
    let Some(metadata) = event.metadata.as_object_mut() else {
        return Err(CaptureError::SystemInvariant(
            "provider event metadata must be an object",
        ));
    };
    metadata.insert(
        COMPLETE_CONTENT_LOCATOR_METADATA_KEY.to_owned(),
        locator.to_metadata_value(),
    );
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
        || !EXACT_JSONL_SOURCES.contains(&(provider, source_format))
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
    let Some(locator) = PersistedCompleteContentLocatorV1::new(
        CompleteContentSourceFamily::Jsonl,
        EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        &encoded,
        native_record_id,
        digest_bytes(record_bytes),
        CompleteContentBodyDigest::from_text(&text),
    ) else {
        return Ok(());
    };
    let Some(metadata) = event.metadata.as_object_mut() else {
        return Err(CaptureError::SystemInvariant(
            "provider event metadata must be an object",
        ));
    };
    metadata.insert(
        COMPLETE_CONTENT_LOCATOR_METADATA_KEY.to_owned(),
        locator.to_metadata_value(),
    );
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
    if !matches!(
        event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    ) {
        return Ok(());
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
    let body_sha256 = CompleteContentBodyDigest::parse(content_ref.sha256().to_owned()).ok_or(
        CaptureError::SystemInvariant("Codex result ContentRef digest must be valid SHA-256"),
    )?;
    let Some(locator) = PersistedCompleteContentLocatorV1::new(
        CompleteContentSourceFamily::Jsonl,
        JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        &range.encode(),
        format!("line-{line_number}"),
        digest_bytes(record_bytes),
        body_sha256,
    ) else {
        return Ok(());
    };
    let Some(metadata) = event.metadata.as_object_mut() else {
        return Err(CaptureError::SystemInvariant(
            "provider event metadata must be an object",
        ));
    };
    metadata.insert(
        RESULT_CONTENT_LOCATOR_METADATA_KEY.to_owned(),
        locator.to_metadata_value(),
    );
    Ok(())
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

    fn length(self) -> Option<usize> {
        usize::try_from(self.byte_end_exclusive.checked_sub(self.byte_start)?).ok()
    }
}

fn validate_batch(requests: &[CompleteMessageRequest]) -> Result<(), CompleteContentError> {
    let first = &requests[0];
    let first_identity = first
        .source_identity
        .as_deref()
        .filter(|identity| !identity.is_empty())
        .ok_or_else(|| error(first, CompleteContentErrorKind::HydrationUnsupported))?;
    let mut prior = None;
    for request in requests {
        if request.provider != first.provider
            || request.source_format != first.source_format
            || request.raw_source_path != first.raw_source_path
            || request.source_root != first.source_root
            || request.source_identity.as_deref() != Some(first_identity)
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
            || request.expected_body_digest.is_none()
        {
            return Err(error(
                request,
                CompleteContentErrorKind::HydrationUnsupported,
            ));
        }
    }
    Ok(())
}

fn selected_source_path(request: &CompleteMessageRequest) -> Result<PathBuf, CompleteContentError> {
    let raw = normalize_lexical(&request.raw_source_path)
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceUnreadable))?;
    let selected = if raw.is_absolute() {
        raw
    } else {
        let root = request
            .source_root
            .as_deref()
            .and_then(normalize_lexical)
            .ok_or_else(|| error(request, CompleteContentErrorKind::SourceUnreadable))?;
        normalize_lexical(&root.join(raw))
            .ok_or_else(|| error(request, CompleteContentErrorKind::SourceUnreadable))?
    };
    if let Some(root) = request.source_root.as_deref().and_then(normalize_lexical) {
        if selected != root && !selected.starts_with(&root) {
            return Err(error(request, CompleteContentErrorKind::SourceUnreadable));
        }
    }
    Ok(selected)
}

fn normalize_lexical(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => return None,
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    (!normalized.as_os_str().is_empty()).then_some(normalized)
}

fn ensure_no_links(
    path: &Path,
    request: &CompleteMessageRequest,
) -> Result<(), CompleteContentError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if current.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(error(request, CompleteContentErrorKind::SourceUnreadable));
            }
            Ok(_) => {}
            Err(io_error) if io_error.kind() == io::ErrorKind::NotFound => {
                return Err(error(request, CompleteContentErrorKind::SourceMissing));
            }
            Err(_) => {
                return Err(error(request, CompleteContentErrorKind::SourceUnreadable));
            }
        }
    }
    Ok(())
}

fn open_frozen_source(
    path: &Path,
    request: &CompleteMessageRequest,
) -> Result<(File, FrozenFile), CompleteContentError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|io_error| match io_error.kind() {
            io::ErrorKind::NotFound => error(request, CompleteContentErrorKind::SourceMissing),
            _ => error(request, CompleteContentErrorKind::SourceUnreadable),
        })?;
    let metadata = file
        .metadata()
        .map_err(|_| error(request, CompleteContentErrorKind::SourceUnreadable))?;
    if !metadata.file_type().is_file() {
        return Err(error(request, CompleteContentErrorKind::SourceUnreadable));
    }
    let frozen = FrozenFile::from_metadata(&metadata)
        .map_err(|_| error(request, CompleteContentErrorKind::SourceUnreadable))?;
    if request
        .source_snapshot
        .size_bytes
        .is_some_and(|observed| frozen.length < observed)
    {
        return Err(error(request, CompleteContentErrorKind::SourceChanged));
    }
    Ok((file, frozen))
}

fn read_record(
    file: &mut File,
    frozen: &FrozenFile,
    range: JsonlRange,
    request: &CompleteMessageRequest,
) -> Result<Vec<u8>, CompleteContentError> {
    let length = range
        .length()
        .filter(|length| *length <= COMPLETE_CONTENT_MAX_BODY_BYTES)
        .ok_or_else(|| error(request, CompleteContentErrorKind::ContentTooLarge))?;
    if range.byte_end_exclusive > frozen.length {
        return Err(error(
            request,
            CompleteContentErrorKind::SourceRecordMissing,
        ));
    }
    if request
        .source_snapshot
        .size_bytes
        .is_some_and(|observed| range.byte_end_exclusive > observed)
    {
        return Err(error(request, CompleteContentErrorKind::SourceChanged));
    }
    if range.byte_start > 0 {
        file.seek(SeekFrom::Start(range.byte_start - 1))
            .map_err(|_| error(request, CompleteContentErrorKind::SourceUnreadable))?;
        let mut boundary = [0_u8; 1];
        file.read_exact(&mut boundary)
            .map_err(|_| error(request, CompleteContentErrorKind::SourceChanged))?;
        if boundary[0] != b'\n' {
            return Err(error(request, CompleteContentErrorKind::SourceChanged));
        }
    }
    file.seek(SeekFrom::Start(range.byte_start))
        .map_err(|_| error(request, CompleteContentErrorKind::SourceUnreadable))?;
    let mut record = vec![0_u8; length];
    file.read_exact(&mut record).map_err(|io_error| {
        if io_error.kind() == io::ErrorKind::UnexpectedEof {
            error(request, CompleteContentErrorKind::SourceRecordMissing)
        } else {
            error(request, CompleteContentErrorKind::SourceUnreadable)
        }
    })?;
    let first_newline = record.iter().position(|byte| *byte == b'\n');
    if first_newline.is_some_and(|position| position + 1 != record.len())
        || (first_newline.is_none() && range.byte_end_exclusive != frozen.length)
    {
        return Err(error(request, CompleteContentErrorKind::SourceChanged));
    }
    let expected_record_digest = request
        .expected_record_digest
        .as_ref()
        .ok_or_else(|| error(request, CompleteContentErrorKind::HydrationUnsupported))?;
    if &digest_bytes(jsonl_payload_bytes(&record)) != expected_record_digest {
        return Err(error(request, CompleteContentErrorKind::SourceChanged));
    }
    Ok(record)
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
    if !SUPPORTED_JSONL_SOURCES.contains(&(provider, source_format))
        || native_jsonl_event_type(provider, value) != EventType::Message
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
    let occurred_at = DateTime::<Utc>::from_timestamp(0, 0)?;
    native_jsonl_event(provider, source_format, value, line_number, occurred_at)
        .map(|event| event.payload)
}

fn observe_exact_source_binding(
    request: &CompleteMessageRequest,
    path: &Path,
) -> Result<ExactJsonlSourceBinding, CompleteContentError> {
    let observed = match request.provider {
        CaptureProvider::CodeBuddy if request.source_format == CODEBUDDY_SOURCE_FORMAT => {
            crate::provider::providers::codebuddy::codebuddy_cli_complete_content_source(path)
        }
        CaptureProvider::MistralVibe if request.source_format == MISTRAL_VIBE_SOURCE_FORMAT => {
            crate::provider::providers::mistral_vibe::mistral_vibe_complete_content_source(path)
        }
        CaptureProvider::OpenClaw if request.source_format == OPENCLAW_SOURCE_FORMAT => {
            crate::provider::providers::openclaw::openclaw_complete_content_source(path)
        }
        CaptureProvider::KimiCodeCli if request.source_format == KIMI_CODE_CLI_SOURCE_FORMAT => {
            crate::provider::providers::kimi::kimi_complete_content_source(
                path,
                request.source_root.as_deref(),
            )
        }
        _ => {
            return Err(error(
                request,
                CompleteContentErrorKind::HydrationUnsupported,
            ))
        }
    }
    .map_err(|capture_error| map_source_error(request, capture_error))?;
    Ok(ExactJsonlSourceBinding::new(&observed.0, &observed.1))
}

fn map_source_error(
    request: &CompleteMessageRequest,
    source: CaptureError,
) -> CompleteContentError {
    let kind = match source {
        CaptureError::Io(ref io_error) if io_error.kind() == io::ErrorKind::NotFound => {
            CompleteContentErrorKind::SourceMissing
        }
        CaptureError::InvalidPayload(ref message) if message.contains("exceeds") => {
            CompleteContentErrorKind::ContentTooLarge
        }
        CaptureError::InvalidProviderTranscriptPath { .. }
        | CaptureError::SourceChangedDuringCapture
        | CaptureError::InvalidPayload(_) => CompleteContentErrorKind::SourceChanged,
        _ => CompleteContentErrorKind::SourceUnreadable,
    };
    error(request, kind)
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

fn jsonl_payload_bytes(record: &[u8]) -> &[u8] {
    let without_newline = record.strip_suffix(b"\n").unwrap_or(record);
    without_newline
        .strip_suffix(b"\r")
        .unwrap_or(without_newline)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrozenFile {
    length: u64,
    modified: SystemTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl FrozenFile {
    fn from_metadata(metadata: &Metadata) -> io::Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);

        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
            readonly: metadata.permissions().readonly(),
            device,
            inode,
        })
    }
}

fn revalidate_open_source(
    file: &File,
    path: &Path,
    frozen: &FrozenFile,
    request: &CompleteMessageRequest,
) -> Result<(), CompleteContentError> {
    let current = file
        .metadata()
        .ok()
        .and_then(|metadata| FrozenFile::from_metadata(&metadata).ok());
    let selected = fs::symlink_metadata(path)
        .ok()
        .filter(|metadata| !metadata.file_type().is_symlink())
        .and_then(|metadata| FrozenFile::from_metadata(&metadata).ok());
    if current.as_ref() != Some(frozen) || selected.as_ref() != Some(frozen) {
        return Err(error(request, CompleteContentErrorKind::SourceChanged));
    }
    Ok(())
}

fn error(request: &CompleteMessageRequest, kind: CompleteContentErrorKind) -> CompleteContentError {
    CompleteContentError::new(kind, request.event_id)
}

#[cfg(test)]
mod tests;
