use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, EventHydrationRequest,
    EventIdentityInput, HydratedProviderRecord, HydrationFailure, HydrationFailureKind,
    LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
    ProjectionContractError, SessionIdentityInput, SourceAnchor, SourceKey, SourceRecordLocator,
    SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::dto::{GeminiEventBody, GeminiTranscriptLayout};
use super::parser::{read_gemini_session_header, GeminiBorrowedRecordParser};
use super::{
    discover_gemini_transcripts, GeminiEventIdentity, GeminiFileObservation, GeminiScanError,
    GeminiSession, GeminiTranscriptSource,
};
use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::source_backed::{
        executable_route,
        family::jsonl::{
            jsonl_family_driver, JsonlFamilyAdapter, JsonlFamilyHydrator, JsonlFamilyInventory,
            JsonlFamilyLeaf, JsonlFamilyProjector, JsonlRecordRef,
        },
        SourceBackedCoordinatorResult, SourceBackedProviderRegistry, SourceBackedRouteSelection,
        SourceBackedSelectorAuthority,
    },
    CaptureError, GEMINI_CLI_SOURCE_FORMAT, MAX_PROVIDER_JSONL_LINE_BYTES,
};

const GEMINI_SOURCE_ANCHOR_NAMESPACE: &str = "gemini.session";
const GEMINI_NATIVE_SESSION_NAMESPACE: &str = "gemini.session";
const GEMINI_NATIVE_EVENT_NAMESPACE: &str = "gemini.event";
const GEMINI_LOGICAL_SESSION_KIND: &str = "gemini-session";
const GEMINI_LOGICAL_EVENT_KIND: &str = "gemini-event";
const GEMINI_SOURCE_SCHEMA_VARIANT: &str = "gemini-nativepath-jsonl-v0";
const GEMINI_SOURCE_BACKED_PARSER_REVISION: &str = "gemini-nativepath-source-backed-v0-p6-p4";
const MAX_GEMINI_LEXICAL_METADATA_CHARS: usize = 8 * 1024;

pub(crate) mod registration {
    use super::*;
    use crate::ProviderSource;

    pub(crate) fn register(
        registry: &mut SourceBackedProviderRegistry,
        source: ProviderSource,
        selection: SourceBackedRouteSelection,
    ) -> SourceBackedCoordinatorResult<()> {
        let driver = jsonl_family_driver(gemini_jsonl_adapter(), source.path.clone());
        registry.register(executable_route(
            source,
            selection,
            SourceBackedSelectorAuthority::DiscoveredWinner,
            driver,
        )?);
        Ok(())
    }
}

#[derive(Debug, Error)]
pub(crate) enum GeminiSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Scan(#[from] GeminiScanError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("locator is not a Gemini NativePath JSONL record")]
    InvalidLocator,
    #[error("Gemini locator byte range exceeds the bounded JSONL record size")]
    LocatorRangeTooLarge,
    #[error("Gemini locator byte range ends after the provider source")]
    LocatorRangeMissing,
    #[error("Gemini locator record digest no longer matches provider bytes")]
    LocatorDigestMismatch,
    #[error("Gemini locator record has no exact canonical logical display content")]
    ExactDisplayUnavailable,
}

pub(crate) type GeminiSourceBackedResult<T> = Result<T, GeminiSourceBackedError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GeminiFamilyBinding {
    relative_path: PathBuf,
    layout: GeminiTranscriptLayout,
    observation: GeminiFileObservation,
    ordinary_file_token: [u8; 32],
    authority_relative_path: PathBuf,
    session: GeminiSession,
}

impl GeminiFamilyBinding {
    fn transcript(&self, leaf: &JsonlFamilyLeaf) -> GeminiTranscriptSource {
        GeminiTranscriptSource {
            path: leaf.source_path().to_path_buf(),
            relative_path: self.relative_path.clone(),
            layout: self.layout.clone(),
            observation: self.observation.clone(),
            ordinary_file_token: self.ordinary_file_token,
            authority_relative_path: self.authority_relative_path.clone(),
            authority: leaf.authority().as_ref().clone(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct GeminiJsonlAdapter;

fn gemini_jsonl_adapter() -> Arc<dyn JsonlFamilyAdapter> {
    Arc::new(GeminiJsonlAdapter)
}

impl JsonlFamilyAdapter for GeminiJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Gemini
    }

    fn source_format(&self) -> &'static str {
        GEMINI_CLI_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        GEMINI_SOURCE_SCHEMA_VARIANT
    }

    fn parser_revision(&self) -> &'static str {
        GEMINI_SOURCE_BACKED_PARSER_REVISION
    }

    fn discover(&self, root: &Path) -> crate::Result<JsonlFamilyInventory> {
        let metadata = match fs::symlink_metadata(root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return JsonlFamilyInventory::missing(self.provider(), root);
            }
            Err(error) => return Err(error.into()),
        };
        let discovery = discover_gemini_transcripts(root)?;
        if !discovery.completed_inventory {
            return Err(CaptureError::InvalidPayload(
                "Gemini discovery did not produce a complete inventory".to_owned(),
            ));
        }
        let authority = shared_authority(root, &metadata, &discovery.transcripts)?;
        let mut leaves = Vec::with_capacity(discovery.transcripts.len());
        for transcript in discovery.transcripts {
            if transcript.authority.named_path() != authority.named_path()
                || transcript.authority.authority_fingerprint() != authority.authority_fingerprint()
            {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let session = read_gemini_session_header(&transcript).map_err(capture_scan_error)?;
            let source = gemini_source_key(&session.native_session_id).map_err(capture_error)?;
            let binding = GeminiFamilyBinding {
                relative_path: transcript.relative_path.clone(),
                layout: transcript.layout.clone(),
                observation: transcript.observation.clone(),
                ordinary_file_token: transcript.ordinary_file_token,
                authority_relative_path: transcript.authority_relative_path.clone(),
                session,
            };
            leaves.push(JsonlFamilyLeaf::observe(
                source,
                transcript.path,
                Arc::clone(&authority),
                transcript.authority_relative_path,
                TypedKey::bytes(serde_json::to_vec(&binding)?).map_err(contract_error)?,
            )?);
        }
        JsonlFamilyInventory::present(self.provider(), root, authority, leaves)
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> crate::Result<Box<dyn JsonlFamilyProjector>> {
        let binding = decode_binding(leaf)?;
        if source_file.ordinary_file_token() != binding.ordinary_file_token {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let expected_source =
            gemini_source_key(&binding.session.native_session_id).map_err(capture_error)?;
        if !expected_source.exact_descriptor_eq(leaf.source()) {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let session_id = gemini_session_id(leaf.source(), &binding.session.native_session_id)
            .map_err(capture_error)?;
        let parent_session_id = binding
            .session
            .parent_native_session_id
            .as_deref()
            .map(|parent_native_session_id| {
                let parent_source =
                    gemini_source_key(parent_native_session_id).map_err(capture_error)?;
                gemini_session_id(&parent_source, parent_native_session_id).map_err(capture_error)
            })
            .transpose()?;
        let root_session_id = parent_session_id.unwrap_or(session_id);
        let transcript = binding.transcript(leaf);
        Ok(Box::new(GeminiProjector {
            parser: GeminiBorrowedRecordParser::new(transcript, binding.session.clone()),
            source: leaf.source().clone(),
            source_path: leaf.source_path().display().to_string(),
            session: binding.session,
            session_id,
            parent_session_id,
            root_session_id,
            source_file,
            authority: Arc::clone(leaf.authority()),
        }))
    }

    fn hydrator(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
    ) -> std::result::Result<Box<dyn JsonlFamilyHydrator>, HydrationFailure> {
        let binding = decode_binding(leaf).map_err(unavailable)?;
        if source_file.ordinary_file_token() != binding.ordinary_file_token {
            return Err(stale("Gemini source identity changed before hydration"));
        }
        Ok(Box::new(GeminiHydrator {
            source: leaf.source().clone(),
            source_file,
        }))
    }
}

struct GeminiProjector {
    parser: GeminiBorrowedRecordParser,
    source: SourceKey,
    source_path: String,
    session: GeminiSession,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    source_file: Arc<OpenedProviderSourceFile>,
    authority: Arc<ProviderSourceRoot>,
}

impl JsonlFamilyProjector for GeminiProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(LexicalDocument) -> crate::Result<()>,
    ) -> crate::Result<()> {
        let evidence = record.evidence();
        for event in self
            .parser
            .project(
                record.bytes(),
                evidence.physical_ordinal(),
                evidence.byte_start(),
                evidence.byte_end_exclusive(),
                evidence.record_digest(),
            )
            .map_err(capture_scan_error)?
        {
            emit(
                project_event(
                    &self.source,
                    self.session_id,
                    self.parent_session_id,
                    self.root_session_id,
                    &self.source_path,
                    &self.session,
                    event,
                )
                .map_err(capture_error)?,
            )?;
        }
        Ok(())
    }

    fn finish(&mut self) -> crate::Result<()> {
        self.parser.finish().map_err(capture_scan_error)?;
        self.source_file.revalidate_leaf()?;
        self.authority.revalidate()
    }
}

struct GeminiHydrator {
    source: SourceKey,
    source_file: Arc<OpenedProviderSourceFile>,
}

impl JsonlFamilyHydrator for GeminiHydrator {
    fn hydrate(
        &mut self,
        request: &EventHydrationRequest,
    ) -> std::result::Result<HydratedProviderRecord, HydrationFailure> {
        let hydrated = hydrate_opened_gemini_record(
            self.source_file.as_ref(),
            &self.source,
            request.locator(),
        )
        .map_err(map_hydration_error)?;
        Ok(HydratedProviderRecord {
            event_id: request.event_id(),
            provider_bytes: hydrated.provider_bytes,
        })
    }
}

fn shared_authority(
    root: &Path,
    metadata: &fs::Metadata,
    transcripts: &[GeminiTranscriptSource],
) -> crate::Result<Arc<ProviderSourceRoot>> {
    if let Some(transcript) = transcripts.first() {
        return Ok(Arc::new(transcript.authority.clone()));
    }
    let authority_path = if metadata.is_file() {
        root.parent()
            .ok_or(CaptureError::InvalidProviderTranscriptPath {
                path: root.to_path_buf(),
                reason: "Gemini transcript file has no parent authority",
            })?
    } else {
        root
    };
    Ok(Arc::new(ProviderSourceRoot::open(authority_path)?))
}

fn decode_binding(leaf: &JsonlFamilyLeaf) -> crate::Result<GeminiFamilyBinding> {
    let TypedKey::Bytes(bytes) = leaf.binding() else {
        return Err(CaptureError::InvalidPayload(
            "Gemini family leaf binding is malformed".to_owned(),
        ));
    };
    Ok(serde_json::from_slice(bytes)?)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GeminiHydratedSourceRecord {
    pub(crate) provider_bytes: Vec<u8>,
    pub(crate) decoded_display_text: Option<String>,
}

/// Reopens one exact record from a freshly discovered Gemini leaf. The
/// provider-owned record digest remains valid across a benign append.
#[cfg(test)]
pub(crate) fn hydrate_gemini_source_backed_record(
    source: &GeminiTranscriptSource,
    locator: &SourceRecordLocator,
) -> GeminiSourceBackedResult<GeminiHydratedSourceRecord> {
    let session = read_gemini_session_header(source)?;
    let expected_source = gemini_source_key(&session.native_session_id)?;
    let source_file = source.open()?;
    hydrate_opened_gemini_record(&source_file, &expected_source, locator)
}

fn hydrate_opened_gemini_record(
    source_file: &OpenedProviderSourceFile,
    expected_source: &SourceKey,
    locator: &SourceRecordLocator,
) -> GeminiSourceBackedResult<GeminiHydratedSourceRecord> {
    locator.validate_contract()?;
    let (byte_offset, byte_length, physical_ordinal) = validate_locator(locator, expected_source)?;
    let maximum = u64::try_from(MAX_PROVIDER_JSONL_LINE_BYTES)
        .map_err(|_| GeminiSourceBackedError::LocatorRangeTooLarge)?
        .checked_add(2)
        .ok_or(GeminiSourceBackedError::LocatorRangeTooLarge)?;
    if byte_length == 0 || byte_length > maximum {
        return Err(GeminiSourceBackedError::LocatorRangeTooLarge);
    }
    let range_end = byte_offset
        .checked_add(byte_length)
        .ok_or(GeminiSourceBackedError::LocatorRangeTooLarge)?;
    if source_file.len() < range_end {
        return Err(GeminiSourceBackedError::LocatorRangeMissing);
    }
    let byte_length =
        usize::try_from(byte_length).map_err(|_| GeminiSourceBackedError::LocatorRangeTooLarge)?;
    let provider_bytes = source_file.read_exact_range(
        byte_offset,
        byte_length,
        MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2),
    )?;
    let actual_digest: [u8; 32] = Sha256::digest(&provider_bytes).into();
    if &actual_digest != locator.record_digest() {
        return Err(GeminiSourceBackedError::LocatorDigestMismatch);
    }
    let decoded_display_text = decode_display_text(&provider_bytes, physical_ordinal)?
        .ok_or(GeminiSourceBackedError::ExactDisplayUnavailable)?;
    Ok(GeminiHydratedSourceRecord {
        provider_bytes: decoded_display_text.as_bytes().to_vec(),
        decoded_display_text: Some(decoded_display_text),
    })
}

fn project_event(
    source: &SourceKey,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    source_path: &str,
    session: &GeminiSession,
    event: super::GeminiRetainedEvent,
) -> GeminiSourceBackedResult<LexicalDocument> {
    let GeminiEventIdentity::NativeRecordId(native_event_id) = &event.identity;
    let native_item_key = NativeItemKey::native_id(
        GEMINI_NATIVE_EVENT_NAMESPACE,
        TypedKey::utf8(native_event_id)?,
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: GEMINI_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::Jsonl {
            byte_offset: event.source_record.byte_offset,
            byte_length: event.source_record.byte_length,
            physical_ordinal: event.native_order.raw_ordinal,
            native_session_key: Some(TypedKey::utf8(&session.native_session_id)?),
            native_event_key: Some(TypedKey::utf8(native_event_id)?),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        event.source_record.record_digest,
    )?;
    let event_sequence = event
        .native_order
        .raw_ordinal
        .checked_mul(u64::from(u32::MAX) + 1)
        .and_then(|sequence| sequence.checked_add(u64::from(event.native_order.sub_ordinal)))
        .ok_or_else(|| {
            GeminiSourceBackedError::Capture(CaptureError::SystemInvariant(
                "Gemini event sequence overflowed",
            ))
        })?;
    let body = lexical_body(&event);
    if body.is_empty() {
        return Err(CaptureError::InvalidPayload(
            "Gemini source-backed event has no lexical body".to_owned(),
        )
        .into());
    }
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id,
        root_session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(session.native_session_id.clone()),
        branch: None,
        source_path: Some(source_path.to_owned()),
        agent_type: session.agent_type.as_str().to_owned(),
        is_primary: session.parent_native_session_id.is_none()
            && session.agent_type != AgentType::Subagent,
        event_sequence,
        occurred_at_unix_ms: event
            .occurred_at
            .or(session.started_at)
            .map(|timestamp| timestamp.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: Some(event.role.as_str().to_owned()),
        body,
        workspace: None,
        cwd: session
            .cwd
            .as_deref()
            .map(|cwd| bounded_chars(cwd, MAX_GEMINI_LEXICAL_METADATA_CHARS)),
        touched_files: event.safe_file_touches,
    })
}

fn lexical_body(event: &super::GeminiRetainedEvent) -> String {
    if !event.searchable_text.is_empty() {
        return event.searchable_text.clone();
    }
    match &event.body {
        GeminiEventBody::Message { text, .. } => text.clone(),
        GeminiEventBody::ToolCall { .. } => "Gemini tool call".to_owned(),
        GeminiEventBody::OutputDiagnostic {
            call_id,
            tool_name,
            outcome,
            exit_code,
            duration_ms,
        } => format!(
            "Gemini {} output {}{}{}{}",
            tool_name.as_deref().unwrap_or("tool"),
            outcome,
            call_id
                .as_deref()
                .map(|call| format!(", call {call}"))
                .unwrap_or_default(),
            exit_code
                .map(|code| format!(", exit code {code}"))
                .unwrap_or_default(),
            duration_ms
                .map(|duration| format!(", duration {duration} ms"))
                .unwrap_or_default(),
        ),
        GeminiEventBody::StateNotice { summary } => summary
            .as_deref()
            .filter(|summary| !summary.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| "Gemini state update".to_owned()),
        GeminiEventBody::RewindNotice {
            target_native_record_id,
        } => format!("Gemini rewind to {target_native_record_id}"),
    }
}

fn bounded_chars(value: &str, maximum: usize) -> String {
    value.chars().take(maximum).collect()
}

fn gemini_source_key(native_session_id: &str) -> GeminiSourceBackedResult<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        GEMINI_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?;
    Ok(SourceKey::derive(
        CaptureProvider::Gemini.as_str(),
        GEMINI_CLI_SOURCE_FORMAT,
        GEMINI_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn gemini_session_id(
    source: &SourceKey,
    native_session_id: &str,
) -> GeminiSourceBackedResult<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        GEMINI_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: GEMINI_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

fn validate_locator(
    locator: &SourceRecordLocator,
    expected_source: &SourceKey,
) -> GeminiSourceBackedResult<(u64, u64, u64)> {
    if !expected_source.exact_descriptor_eq(locator.source())
        || locator.source().provider() != CaptureProvider::Gemini.as_str()
        || locator.source().source_format() != GEMINI_CLI_SOURCE_FORMAT
        || locator.source().schema_variant() != GEMINI_SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
        || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(GeminiSourceBackedError::InvalidLocator);
    }
    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        physical_ordinal,
        native_session_key,
        native_event_key,
    } = locator.coordinate()
    else {
        return Err(GeminiSourceBackedError::InvalidLocator);
    };
    let SourceAnchor::ProviderNative { namespace, key } = expected_source.anchor() else {
        return Err(GeminiSourceBackedError::InvalidLocator);
    };
    if namespace != GEMINI_SOURCE_ANCHOR_NAMESPACE
        || native_session_key.as_ref() != Some(key)
        || !matches!(native_event_key, Some(TypedKey::Utf8(value)) if !value.is_empty())
    {
        return Err(GeminiSourceBackedError::InvalidLocator);
    }
    Ok((*byte_offset, *byte_length, *physical_ordinal))
}

fn decode_display_text(
    provider_bytes: &[u8],
    _physical_ordinal: u64,
) -> GeminiSourceBackedResult<Option<String>> {
    let record = provider_bytes.strip_suffix(b"\n").unwrap_or(provider_bytes);
    let record = record.strip_suffix(b"\r").unwrap_or(record);
    let value: Value = serde_json::from_slice(record)?;
    if matches!(
        value.get("type").and_then(Value::as_str),
        Some("user" | "gemini")
    ) {
        return Ok(value
            .get("content")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned));
    }
    if let Some(calls) = value.get("toolCalls").and_then(Value::as_array) {
        if calls
            .iter()
            .any(|call| call.get("result").is_some_and(|result| !result.is_null()))
        {
            return Ok(None);
        }
        let mut text = String::new();
        for call in calls {
            if let Some(name) = call
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
            {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(name);
            }
            if let Some(args) = call.get("args") {
                if let Ok(args) = serde_json::to_string(args) {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&args);
                }
            }
        }
        return Ok((!text.is_empty()).then_some(text));
    }
    if let Some(summary) = value
        .pointer("/$set/summary")
        .and_then(Value::as_str)
        .filter(|summary| !summary.is_empty())
    {
        return Ok(Some(summary.to_owned()));
    }
    Ok(value
        .get("$rewindTo")
        .and_then(Value::as_str)
        .map(|target| format!("rewind to {}", target.trim()))
        .filter(|text| text != "rewind to "))
}

fn capture_scan_error(error: GeminiScanError) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

fn capture_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

fn contract_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

fn map_hydration_error(error: GeminiSourceBackedError) -> HydrationFailure {
    let kind = match error {
        GeminiSourceBackedError::InvalidLocator
        | GeminiSourceBackedError::Projection(_)
        | GeminiSourceBackedError::Resolver(_) => HydrationFailureKind::InvalidLocator,
        GeminiSourceBackedError::ExactDisplayUnavailable => {
            HydrationFailureKind::UnsupportedParserRevision
        }
        GeminiSourceBackedError::Capture(_)
        | GeminiSourceBackedError::Scan(_)
        | GeminiSourceBackedError::Io(_)
        | GeminiSourceBackedError::Json(_)
        | GeminiSourceBackedError::LocatorRangeTooLarge
        | GeminiSourceBackedError::LocatorRangeMissing
        | GeminiSourceBackedError::LocatorDigestMismatch => {
            HydrationFailureKind::StaleRecordEvidence
        }
    };
    HydrationFailure {
        kind,
        detail: error.to_string(),
    }
}

fn unavailable(error: impl std::fmt::Display) -> HydrationFailure {
    HydrationFailure {
        kind: HydrationFailureKind::TemporarilyUnavailable,
        detail: error.to_string(),
    }
}

fn stale(detail: impl std::fmt::Display) -> HydrationFailure {
    HydrationFailure {
        kind: HydrationFailureKind::StaleRecordEvidence,
        detail: detail.to_string(),
    }
}
