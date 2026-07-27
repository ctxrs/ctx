//! Complete-content recovery for bounded structured JSON and compound trees.
//!
//! These providers cannot use a byte-range JSONL resolver or a SQLite key
//! lookup. Import therefore persists a small provider-native identity plus
//! SHA-256 digests for the captured record and complete ordinary-message body.
//! Resolution replays only the addressed bounded record under the currently
//! selected source root and fails atomically on any mismatch.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventType, ProviderEventEnvelope};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    CompleteContentBodyDigest, CompleteContentError, CompleteContentErrorKind,
    CompleteContentHashAuthority, CompleteContentResolver, CompleteContentSourceFamily,
    CompleteMessage, CompleteMessageRequest, PersistedCompleteContentLocatorV1, SourceVerification,
    COMPLETE_CONTENT_LOCATOR_METADATA_KEY, COMPLETE_CONTENT_MAX_BODY_BYTES,
};
use crate::provider::normalization::{provider_block_text, provider_message_id};
#[cfg(test)]
use crate::provider::providers::openhands::decode_openhands_event;
use crate::provider::providers::{
    auggie::{auggie_request_text, auggie_response_text},
    codebuddy::{codebuddy_decoded_message, codebuddy_message_text},
    continue_cli::{continue_history_item_event, continue_history_item_text},
    openhands::decode_openhands_event_value,
    task_json::{
        task_json_event, task_json_event_text, task_json_event_type, task_json_provider,
        TaskJsonEventInput,
    },
};
use crate::{compute_payload_hash, CaptureError, Result, PROVIDER_MAX_TEXT_CHARS};

pub const STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND: &str = "structured-message-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredCompleteContentCapabilityStatus {
    Supported,
    NotNeeded,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuredCompleteContentCapability {
    pub provider: CaptureProvider,
    pub status: StructuredCompleteContentCapabilityStatus,
    pub reason: &'static str,
}

/// Exhaustive public-provider ownership table for this resolver family.
///
/// `NotNeeded` means a JSONL or SQLite family resolver owns that provider. It
/// never means that a truncated body may silently fall back to indexed text.
pub const STRUCTURED_COMPLETE_CONTENT_CAPABILITIES: &[StructuredCompleteContentCapability] = &[
    not_needed(CaptureProvider::Codex, "JSONL family"),
    not_needed(CaptureProvider::Pi, "JSONL family"),
    not_needed(CaptureProvider::Claude, "JSONL family"),
    not_needed(CaptureProvider::OpenCode, "SQLite family"),
    not_needed(CaptureProvider::Kilo, "SQLite family"),
    not_needed(CaptureProvider::MiMoCode, "SQLite family"),
    not_needed(CaptureProvider::KiroCli, "SQLite family"),
    not_needed(CaptureProvider::Crush, "SQLite family"),
    not_needed(CaptureProvider::Goose, "SQLite family"),
    not_needed(CaptureProvider::Lingma, "SQLite family"),
    not_needed(CaptureProvider::Qoder, "JSONL family"),
    not_needed(CaptureProvider::Warp, "SQLite family"),
    supported(
        CaptureProvider::CodeBuddy,
        "extension JSON compound tree; CLI JSONL is family-routed",
    ),
    not_needed(CaptureProvider::Trae, "SQLite family"),
    not_needed(CaptureProvider::OpenClaw, "JSONL family"),
    not_needed(CaptureProvider::Hermes, "SQLite family"),
    not_needed(CaptureProvider::NanoClaw, "SQLite family"),
    not_needed(CaptureProvider::AstrBot, "SQLite family"),
    not_needed(CaptureProvider::Shelley, "SQLite family"),
    supported(CaptureProvider::Continue, "single structured session JSON"),
    supported(CaptureProvider::OpenHands, "one-record JSON event tree"),
    not_needed(CaptureProvider::Antigravity, "JSONL family"),
    not_needed(CaptureProvider::Gemini, "JSONL family"),
    not_needed(CaptureProvider::Tabnine, "JSONL family"),
    not_needed(CaptureProvider::Cursor, "JSONL family"),
    not_needed(CaptureProvider::Windsurf, "JSONL family"),
    not_needed(CaptureProvider::Zed, "SQLite family"),
    not_needed(CaptureProvider::CopilotCli, "JSONL family"),
    not_needed(CaptureProvider::FactoryAiDroid, "JSONL family"),
    not_needed(CaptureProvider::QwenCode, "JSONL family"),
    not_needed(CaptureProvider::KimiCodeCli, "JSONL family"),
    supported(CaptureProvider::Auggie, "single structured session JSON"),
    not_needed(CaptureProvider::Junie, "JSONL family"),
    not_needed(CaptureProvider::Firebender, "SQLite family"),
    not_needed(CaptureProvider::ForgeCode, "SQLite family"),
    not_needed(CaptureProvider::DeepAgents, "SQLite family"),
    not_needed(CaptureProvider::MistralVibe, "JSONL family"),
    not_needed(CaptureProvider::Mux, "JSONL compound family"),
    supported(
        CaptureProvider::RovoDev,
        "single structured session JSON tree",
    ),
    supported(CaptureProvider::Cline, "bounded task JSON compound tree"),
    supported(CaptureProvider::RooCode, "bounded task JSON compound tree"),
];

const fn supported(
    provider: CaptureProvider,
    reason: &'static str,
) -> StructuredCompleteContentCapability {
    StructuredCompleteContentCapability {
        provider,
        status: StructuredCompleteContentCapabilityStatus::Supported,
        reason,
    }
}

const fn not_needed(
    provider: CaptureProvider,
    reason: &'static str,
) -> StructuredCompleteContentCapability {
    StructuredCompleteContentCapability {
        provider,
        status: StructuredCompleteContentCapabilityStatus::NotNeeded,
        reason,
    }
}

const STRUCTURED_LOCATOR_MAGIC: &[u8; 4] = b"SC\0\x01";
const STRUCTURED_MAX_FILES: usize = 4_096;
const STRUCTURED_MAX_DIRECTORY_DEPTH: usize = 12;
const STRUCTURED_MAX_JSON_ENTRIES: usize = 65_536;
const STRUCTURED_MAX_JSON_DEPTH: usize = 64;
const STRUCTURED_MAX_TOTAL_READ_BYTES: usize = 64 * 1024 * 1024;
const STRUCTURED_MAX_COMPOUND_FILE_BYTES: usize = 64 * 1024 * 1024;
const STRUCTURED_MAX_NATIVE_ID_BYTES: usize = 1_024;
const STRUCTURED_DEADLINE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy)]
struct StructuredBounds {
    max_files: usize,
    max_depth: usize,
    max_entries: usize,
    max_json_depth: usize,
    max_total_read_bytes: usize,
    deadline: Duration,
}

impl Default for StructuredBounds {
    fn default() -> Self {
        Self {
            max_files: STRUCTURED_MAX_FILES,
            max_depth: STRUCTURED_MAX_DIRECTORY_DEPTH,
            max_entries: STRUCTURED_MAX_JSON_ENTRIES,
            max_json_depth: STRUCTURED_MAX_JSON_DEPTH,
            max_total_read_bytes: STRUCTURED_MAX_TOTAL_READ_BYTES,
            deadline: STRUCTURED_DEADLINE,
        }
    }
}

/// Bounded resolver for single-JSON, one-record-file, and compound JSON trees.
#[derive(Debug, Default)]
pub struct StructuredCompleteContentResolver {
    bounds: StructuredBounds,
}

impl StructuredCompleteContentResolver {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    fn with_bounds(bounds: StructuredBounds) -> Self {
        Self { bounds }
    }
}

impl CompleteContentResolver for StructuredCompleteContentResolver {
    fn family(&self) -> CompleteContentSourceFamily {
        CompleteContentSourceFamily::Structured
    }

    fn supports(&self, provider: CaptureProvider, source_format: &str) -> bool {
        structured_source_format(provider, source_format)
    }

    fn resolve(
        &self,
        requests: &[CompleteMessageRequest],
    ) -> std::result::Result<Vec<CompleteMessage>, CompleteContentError> {
        let Some(first) = requests.first() else {
            return Ok(Vec::new());
        };
        if !self.supports(first.provider, &first.source_format) {
            return Err(error(first, CompleteContentErrorKind::HydrationUnsupported));
        }
        validate_request_batch(requests)?;
        let deadline = Instant::now() + self.bounds.deadline;
        let mut budget = ResolutionBudget::new(self.bounds, deadline);
        let roots = selected_roots(first, &mut budget)?;
        let mut output = Vec::with_capacity(requests.len());
        for request in requests {
            budget.check(request)?;
            let locator = StructuredLocator::for_request(request)?;
            let resolved = resolve_one(request, &locator, &roots, &mut budget)?;
            output.push(CompleteMessage::verified(
                request,
                resolved.text,
                SourceVerification::VERIFIED,
            )?);
        }
        Ok(output)
    }
}

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
    let body_sha256 = CompleteContentBodyDigest::from_text(complete_text);
    let locator = PersistedCompleteContentLocatorV1::new(
        CompleteContentSourceFamily::Structured,
        STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND,
        &value,
        native_record_id,
        record_sha256,
        body_sha256,
    )
    .ok_or(CaptureError::SystemInvariant(
        "structured complete-content locator exceeds its bounded schema",
    ))?;
    event
        .metadata
        .as_object_mut()
        .ok_or(CaptureError::SystemInvariant(
            "provider event metadata must be an object",
        ))?
        .insert(
            COMPLETE_CONTENT_LOCATOR_METADATA_KEY.to_owned(),
            locator.to_metadata_value(),
        );
    Ok(())
}

fn digest_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn structured_source_format(provider: CaptureProvider, source_format: &str) -> bool {
    matches!(
        (provider, source_format),
        (CaptureProvider::Auggie, "auggie_session_json")
            | (CaptureProvider::Continue, "continue_cli_sessions_json")
            | (CaptureProvider::OpenHands, "openhands_file_events")
            | (CaptureProvider::RovoDev, "rovodev_session_json_tree")
            | (CaptureProvider::Cline, "cline_task_directory_json")
            | (CaptureProvider::RooCode, "roo_task_directory_json")
            | (CaptureProvider::CodeBuddy, "codebuddy_history_json")
    )
}

fn validate_request_batch(
    requests: &[CompleteMessageRequest],
) -> std::result::Result<(), CompleteContentError> {
    let first = &requests[0];
    let mut previous = None;
    for request in requests {
        let coordinate = (
            request.source_record_ordinal,
            request.source_record_subrecord_index,
        );
        if request.provider != first.provider
            || request.source_format != first.source_format
            || request.raw_source_path != first.raw_source_path
            || previous.is_some_and(|prior| prior >= coordinate)
        {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
        previous = Some(coordinate);
    }
    Ok(())
}

#[derive(Debug)]
struct StructuredLocator {
    provider: CaptureProvider,
    ordinal: u64,
    subrecord: u32,
    native_id: String,
    record_digest: CompleteContentBodyDigest,
}

impl StructuredLocator {
    fn for_request(
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
}

fn encode_structured_locator(
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

fn decode_structured_locator(value: &[u8]) -> Option<(CaptureProvider, u64, u32, String)> {
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

struct ResolutionBudget {
    bounds: StructuredBounds,
    deadline: Instant,
    files: usize,
    entries: usize,
    bytes: usize,
}

impl ResolutionBudget {
    fn new(bounds: StructuredBounds, deadline: Instant) -> Self {
        Self {
            bounds,
            deadline,
            files: 0,
            entries: 0,
            bytes: 0,
        }
    }

    fn check(
        &self,
        request: &CompleteMessageRequest,
    ) -> std::result::Result<(), CompleteContentError> {
        if Instant::now() > self.deadline {
            return Err(error(request, CompleteContentErrorKind::SourceChanged));
        }
        Ok(())
    }

    fn observe_file(
        &mut self,
        request: &CompleteMessageRequest,
    ) -> std::result::Result<(), CompleteContentError> {
        self.check(request)?;
        self.files = self.files.saturating_add(1);
        if self.files > self.bounds.max_files {
            return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
        }
        Ok(())
    }

    fn observe_entries(
        &mut self,
        request: &CompleteMessageRequest,
        count: usize,
    ) -> std::result::Result<(), CompleteContentError> {
        self.entries = self.entries.saturating_add(count);
        if self.entries > self.bounds.max_entries {
            return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
        }
        self.check(request)
    }

    fn observe_bytes(
        &mut self,
        request: &CompleteMessageRequest,
        count: usize,
    ) -> std::result::Result<(), CompleteContentError> {
        self.bytes = self.bytes.saturating_add(count);
        if self.bytes > self.bounds.max_total_read_bytes {
            return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
        }
        self.check(request)
    }
}

#[derive(Debug)]
struct ResolvedMessage {
    text: String,
    provider_hash: Option<String>,
    fallback_hash: Option<String>,
    native_id: String,
}

fn resolve_one(
    request: &CompleteMessageRequest,
    locator: &StructuredLocator,
    roots: &[PathBuf],
    budget: &mut ResolutionBudget,
) -> std::result::Result<ResolvedMessage, CompleteContentError> {
    debug_assert_eq!(request.provider, locator.provider);
    let resolved = match request.provider {
        CaptureProvider::Auggie => {
            resolve_whole_json(request, locator, roots, budget, auggie_message)
        }
        CaptureProvider::Continue => {
            resolve_whole_json(request, locator, roots, budget, continue_message)
        }
        CaptureProvider::RovoDev => {
            resolve_whole_json(request, locator, roots, budget, rovodev_message)
        }
        CaptureProvider::OpenHands => resolve_openhands(request, locator, roots, budget),
        CaptureProvider::Cline | CaptureProvider::RooCode => {
            resolve_task_json(request, locator, roots, budget)
        }
        CaptureProvider::CodeBuddy => resolve_codebuddy(request, locator, roots, budget),
        _ => Err(error(
            request,
            CompleteContentErrorKind::HydrationUnsupported,
        )),
    }?;
    if resolved.native_id != locator.native_id {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let observed_hash = match request.expected_hash_authority {
        CompleteContentHashAuthority::ProviderSupplied => resolved.provider_hash.as_ref(),
        CompleteContentHashAuthority::NormalizedPayloadFallback => resolved.fallback_hash.as_ref(),
    };
    if observed_hash.map(String::as_str) != Some(request.expected_provider_event_hash.as_str()) {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    Ok(resolved)
}

type WholeJsonExtractor = fn(
    &CompleteMessageRequest,
    &StructuredLocator,
    &Path,
    &Value,
) -> std::result::Result<ResolvedMessage, CompleteContentError>;

fn resolve_whole_json(
    request: &CompleteMessageRequest,
    locator: &StructuredLocator,
    roots: &[PathBuf],
    budget: &mut ResolutionBudget,
    extract: WholeJsonExtractor,
) -> std::result::Result<ResolvedMessage, CompleteContentError> {
    let paths = candidate_files(request, roots, budget)?;
    let mut saw_candidate = false;
    for path in paths {
        if !whole_json_path_candidate(request.provider, &path) {
            continue;
        }
        saw_candidate = true;
        let bytes = read_frozen_file(request, &path, budget)?;
        if digest_bytes(&bytes) != locator.record_digest.as_str() {
            continue;
        }
        let value = parse_bounded_json(request, &bytes, budget)?;
        return extract(request, locator, &path, &value);
    }
    Err(error(
        request,
        if saw_candidate {
            CompleteContentErrorKind::SourceChanged
        } else {
            CompleteContentErrorKind::SourceMissing
        },
    ))
}

fn auggie_message(
    request: &CompleteMessageRequest,
    locator: &StructuredLocator,
    _path: &Path,
    session: &Value,
) -> std::result::Result<ResolvedMessage, CompleteContentError> {
    if locator.ordinal != 0 {
        return Err(error(
            request,
            CompleteContentErrorKind::SourceRecordMissing,
        ));
    }
    let history = session
        .get("chatHistory")
        .or_else(|| session.get("chat_history"))
        .and_then(Value::as_array)
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let mut current = 0_u32;
    for (chat_index, entry) in history.iter().enumerate() {
        let exchange = entry.get("exchange").unwrap_or(entry);
        for (label, text) in [
            ("request", auggie_request_text(exchange)),
            ("response", auggie_response_text(exchange)),
        ] {
            let Some(text) = text else { continue };
            if current == locator.subrecord {
                let native_id = exchange
                    .get("request_id")
                    .or_else(|| exchange.get("requestId"))
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| format!("{value}:{label}"))
                    .unwrap_or_else(|| format!("chat-{chat_index}:{label}"));
                return Ok(ResolvedMessage {
                    text,
                    provider_hash: Some(native_id.clone()),
                    fallback_hash: None,
                    native_id,
                });
            }
            current = current.saturating_add(1);
        }
    }
    Err(error(
        request,
        CompleteContentErrorKind::SourceRecordMissing,
    ))
}

fn continue_message(
    request: &CompleteMessageRequest,
    locator: &StructuredLocator,
    _path: &Path,
    session: &Value,
) -> std::result::Result<ResolvedMessage, CompleteContentError> {
    if locator.ordinal != 0 {
        return Err(error(
            request,
            CompleteContentErrorKind::SourceRecordMissing,
        ));
    }
    let item = session
        .get("history")
        .and_then(Value::as_array)
        .and_then(|items| items.get(locator.subrecord as usize))
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let text = continue_history_item_text(item)
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let provider_hash = item
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    let provider_session_id = request.provider_session_id.as_deref().unwrap_or("unknown");
    let event = continue_history_item_event(
        provider_session_id,
        item,
        u64::from(locator.subrecord),
        DateTime::<Utc>::UNIX_EPOCH,
    );
    if event.event_type != EventType::Message {
        return Err(error(
            request,
            CompleteContentErrorKind::HydrationUnsupported,
        ));
    }
    let fallback_hash = compute_payload_hash(&event.payload).ok();
    let native_id = provider_hash
        .clone()
        .unwrap_or_else(|| format!("history:{provider_session_id}:{}", locator.subrecord));
    Ok(ResolvedMessage {
        text,
        provider_hash,
        fallback_hash,
        native_id,
    })
}

fn rovodev_message(
    request: &CompleteMessageRequest,
    locator: &StructuredLocator,
    _path: &Path,
    session: &Value,
) -> std::result::Result<ResolvedMessage, CompleteContentError> {
    if locator.ordinal != 0 {
        return Err(error(
            request,
            CompleteContentErrorKind::SourceRecordMissing,
        ));
    }
    let messages = session
        .get("message_history")
        .or_else(|| session.pointer("/session_context/message_history"))
        .or_else(|| session.get("messages"))
        .or_else(|| session.pointer("/conversation/messages"))
        .and_then(Value::as_array)
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let message = messages
        .get(locator.subrecord as usize)
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let text = provider_block_text(message)
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let native_id = provider_message_id(message, u64::from(locator.subrecord));
    Ok(ResolvedMessage {
        text,
        provider_hash: Some(native_id.clone()),
        fallback_hash: None,
        native_id,
    })
}

fn resolve_openhands(
    request: &CompleteMessageRequest,
    locator: &StructuredLocator,
    roots: &[PathBuf],
    budget: &mut ResolutionBudget,
) -> std::result::Result<ResolvedMessage, CompleteContentError> {
    if locator.subrecord != 0 {
        return Err(error(
            request,
            CompleteContentErrorKind::SourceRecordMissing,
        ));
    }
    let paths = candidate_files(request, roots, budget)?;
    let mut saw_candidate = false;
    for path in paths {
        if path.extension().and_then(|value| value.to_str()) != Some("json")
            || !path
                .components()
                .any(|part| part.as_os_str() == "v1_conversations")
        {
            continue;
        }
        saw_candidate = true;
        let bytes = read_frozen_file(request, &path, budget)?;
        if digest_bytes(&bytes) != locator.record_digest.as_str() {
            continue;
        }
        let value = serde_json::from_slice::<Value>(&bytes)
            .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
        validate_json_shape(request, &value, budget, 0)?;
        let decoded = decode_openhands_event_value(&path, value).map_err(|decode_error| {
            error(
                request,
                if decode_error.is_too_large() {
                    CompleteContentErrorKind::ContentTooLarge
                } else {
                    CompleteContentErrorKind::ContentVerificationFailed
                },
            )
        })?;
        if decoded.event_type() != EventType::Message {
            return Err(error(
                request,
                CompleteContentErrorKind::HydrationUnsupported,
            ));
        }
        let native_id = decoded.event_id().to_owned();
        return Ok(ResolvedMessage {
            text: decoded.text().to_owned(),
            provider_hash: Some(native_id.clone()),
            fallback_hash: None,
            native_id,
        });
    }
    Err(error(
        request,
        if saw_candidate {
            CompleteContentErrorKind::SourceChanged
        } else {
            CompleteContentErrorKind::SourceMissing
        },
    ))
}

fn resolve_codebuddy(
    request: &CompleteMessageRequest,
    locator: &StructuredLocator,
    roots: &[PathBuf],
    budget: &mut ResolutionBudget,
) -> std::result::Result<ResolvedMessage, CompleteContentError> {
    if locator.subrecord != 0 {
        return Err(error(
            request,
            CompleteContentErrorKind::SourceRecordMissing,
        ));
    }
    let paths = candidate_files(request, roots, budget)?;
    let mut saw_candidate = false;
    for path in paths {
        if path.extension().and_then(|value| value.to_str()) != Some("json")
            || path
                .parent()
                .and_then(Path::file_name)
                .and_then(|v| v.to_str())
                != Some("messages")
        {
            continue;
        }
        saw_candidate = true;
        let bytes = read_frozen_file(request, &path, budget)?;
        if digest_bytes(&bytes) != locator.record_digest.as_str() {
            continue;
        }
        let raw = parse_bounded_json(request, &bytes, budget)?;
        let decoded = codebuddy_decoded_message(&raw);
        let text = codebuddy_message_text(&decoded, &raw);
        let message_id = path
            .file_stem()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
        let session_id = request
            .provider_session_id
            .as_deref()
            .ok_or_else(|| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
        let native_id = format!("{session_id}:{message_id}");
        return Ok(ResolvedMessage {
            text,
            provider_hash: Some(native_id.clone()),
            fallback_hash: None,
            native_id,
        });
    }
    Err(error(
        request,
        if saw_candidate {
            CompleteContentErrorKind::SourceChanged
        } else {
            CompleteContentErrorKind::SourceMissing
        },
    ))
}

fn resolve_task_json(
    request: &CompleteMessageRequest,
    locator: &StructuredLocator,
    roots: &[PathBuf],
    budget: &mut ResolutionBudget,
) -> std::result::Result<ResolvedMessage, CompleteContentError> {
    if locator.subrecord != 0 {
        return Err(error(
            request,
            CompleteContentErrorKind::SourceRecordMissing,
        ));
    }
    let spec = task_json_provider(request.provider);
    let candidates = candidate_files(request, roots, budget)?;
    let mut saw_candidate = false;
    for path in candidates {
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let source = if file_name == spec.api_file {
            "api_conversation_history"
        } else if file_name == spec.ui_file {
            "ui_messages"
        } else if spec.fallback_api_file == Some(file_name) {
            "claude_messages"
        } else {
            continue;
        };
        saw_candidate = true;
        let bytes = read_frozen_file_with_limit(
            request,
            &path,
            budget,
            STRUCTURED_MAX_COMPOUND_FILE_BYTES,
        )?;
        for record in task_json_records(request, &bytes, budget)? {
            let TaskJsonRecord {
                native_index,
                bytes: record_bytes,
                value: raw,
            } = record;
            if digest_bytes(record_bytes) != locator.record_digest.as_str() {
                continue;
            }
            let event_type = task_json_event_type(&raw, source);
            if event_type != EventType::Message {
                return Err(error(
                    request,
                    CompleteContentErrorKind::HydrationUnsupported,
                ));
            }
            let text = task_json_event_text(&raw, source, event_type);
            let task_id = request.provider_session_id.as_deref().ok_or_else(|| {
                error(request, CompleteContentErrorKind::ContentVerificationFailed)
            })?;
            let event = task_json_event(
                spec,
                task_id,
                TaskJsonEventInput {
                    source,
                    native_index,
                    raw,
                },
                locator.ordinal as usize,
                DateTime::<Utc>::UNIX_EPOCH,
            );
            let native_id = event.provider_event_hash.clone().ok_or_else(|| {
                error(request, CompleteContentErrorKind::ContentVerificationFailed)
            })?;
            return Ok(ResolvedMessage {
                text,
                provider_hash: Some(native_id.clone()),
                fallback_hash: None,
                native_id,
            });
        }
    }
    Err(error(
        request,
        if saw_candidate {
            CompleteContentErrorKind::SourceChanged
        } else {
            CompleteContentErrorKind::SourceMissing
        },
    ))
}

mod source_access;
use source_access::{
    candidate_files, error, parse_bounded_json, read_frozen_file, read_frozen_file_with_limit,
    selected_roots, task_json_records, validate_json_shape, whole_json_path_candidate,
    TaskJsonRecord,
};

#[cfg(test)]
#[path = "structured/tests.rs"]
mod tests;
