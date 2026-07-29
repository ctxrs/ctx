use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource,
    EventIdentityInput, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, ProjectionContractError, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceFrontier, SourceKey, SourceObservation, SourceRecordLocator,
    SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::dto::GeminiEventBody;
use super::{
    read_gemini_transcript_pages_with_profile, GeminiEventIdentity, GeminiFileObservation,
    GeminiNativePage, GeminiNativePageReader, GeminiNativePathProfile, GeminiRetainedEvent,
    GeminiScanError, GeminiSession, GeminiTranscriptSource,
};
use crate::{CaptureError, GEMINI_CLI_SOURCE_FORMAT, MAX_PROVIDER_JSONL_LINE_BYTES};

const GEMINI_SOURCE_ANCHOR_NAMESPACE: &str = "gemini.session";
const GEMINI_NATIVE_SESSION_NAMESPACE: &str = "gemini.session";
const GEMINI_NATIVE_EVENT_NAMESPACE: &str = "gemini.event";
const GEMINI_LOGICAL_SESSION_KIND: &str = "gemini-session";
const GEMINI_LOGICAL_EVENT_KIND: &str = "gemini-event";
const GEMINI_SOURCE_SCHEMA_VARIANT: &str = "gemini-nativepath-jsonl-v0";
const GEMINI_SOURCE_REVISION_KIND: &str = "gemini-ordinary-file-observation-v0";
const GEMINI_FRONTIER_KIND: &str = "gemini-nativepath-checkpoint-v0";
const GEMINI_SOURCE_BACKED_PARSER_REVISION: &str = "gemini-nativepath-source-backed-v0-p6-p4";
const MAX_GEMINI_LEXICAL_METADATA_CHARS: usize = 8 * 1024;

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
    #[error("Gemini source has no importable native session header")]
    MissingSession,
    #[error("Gemini source-backed reader must be drained before certification")]
    ReaderNotDrained,
    #[error("Gemini source-backed page changed its native session")]
    SessionChanged,
    #[error("Gemini Core-only source-backed reader emitted transient output")]
    UnexpectedOutput,
    #[error("Gemini source-backed scan counts do not reconcile")]
    CountMismatch,
    #[error("Gemini source-backed count or byte range overflowed")]
    CountOverflow,
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

/// One provider-owned, independently bounded page for the shared coordinator.
#[derive(Debug)]
pub(crate) struct GeminiSourceBackedPage {
    pub(crate) page_identity: [u8; 32],
    pub(crate) expected_prefix_bytes: u64,
    pub(crate) next_prefix_bytes: u64,
    pub(crate) documents: Vec<LexicalDocument>,
}

/// Terminal leaf evidence. Publication and generation lifecycle remain shared
/// coordinator responsibilities.
#[derive(Debug)]
pub(crate) struct GeminiSourceBackedLeaf {
    pub(crate) source: SourceKey,
    pub(crate) session: GeminiSession,
    pub(crate) session_id: StableEntityId,
    pub(crate) parent_session_id: Option<StableEntityId>,
    pub(crate) root_session_id: StableEntityId,
    pub(crate) certificate: CertifiedSource,
}

/// Cold-source Gemini adapter. It buffers at most one already-bounded native
/// page while deriving path-independent source and session identities.
pub(crate) struct GeminiSourceBackedLeafReader<'a> {
    native: GeminiNativePageReader<'a>,
    pending: Option<GeminiNativePage>,
    source: SourceKey,
    source_observation: GeminiFileObservation,
    source_path: String,
    session: GeminiSession,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    emitted_documents: u64,
}

impl<'a> GeminiSourceBackedLeafReader<'a> {
    pub(crate) fn open(source: &'a GeminiTranscriptSource) -> GeminiSourceBackedResult<Self> {
        let mut native = read_gemini_transcript_pages_with_profile(
            source,
            None,
            GeminiNativePathProfile::CoreOnly,
        )?;
        let pending = loop {
            let Some(page) = native.next_page()? else {
                return Err(GeminiSourceBackedError::MissingSession);
            };
            if page.next_safe_frontier.session.is_some() {
                break page;
            }
            if !page.events.is_empty() {
                return Err(GeminiSourceBackedError::MissingSession);
            }
        };
        let session = pending
            .next_safe_frontier
            .session
            .clone()
            .ok_or(GeminiSourceBackedError::MissingSession)?;
        let source_observation = source.observation.clone();
        let source_key = gemini_source_key(&session.native_session_id)?;
        let session_id = gemini_session_id(&source_key, &session.native_session_id)?;
        let parent_session_id = session
            .parent_native_session_id
            .as_deref()
            .map(|parent_native_session_id| {
                let parent_source = gemini_source_key(parent_native_session_id)?;
                gemini_session_id(&parent_source, parent_native_session_id)
            })
            .transpose()?;
        let root_session_id = parent_session_id.unwrap_or(session_id);
        Ok(Self {
            native,
            pending: Some(pending),
            source: source_key,
            source_observation,
            source_path: source.path.display().to_string(),
            session,
            session_id,
            parent_session_id,
            root_session_id,
            emitted_documents: 0,
        })
    }

    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
    }

    pub(crate) fn session(&self) -> &GeminiSession {
        &self.session
    }

    pub(crate) fn session_id(&self) -> StableEntityId {
        self.session_id
    }

    pub(crate) fn next_page(&mut self) -> GeminiSourceBackedResult<Option<GeminiSourceBackedPage>> {
        let page = match self.pending.take() {
            Some(page) => Some(page),
            None => self.native.next_page()?,
        };
        let Some(page) = page else {
            return Ok(None);
        };
        if !page.output_pages.is_empty() {
            return Err(GeminiSourceBackedError::UnexpectedOutput);
        }
        let page_session = page
            .next_safe_frontier
            .session
            .as_ref()
            .ok_or(GeminiSourceBackedError::MissingSession)?;
        if page_session.native_session_id != self.session.native_session_id {
            return Err(GeminiSourceBackedError::SessionChanged);
        }
        let projected = project_page(
            &self.source,
            self.session_id,
            self.parent_session_id,
            self.root_session_id,
            &self.source_path,
            &self.session,
            page,
        )?;
        self.emitted_documents = self
            .emitted_documents
            .checked_add(
                u64::try_from(projected.documents.len())
                    .map_err(|_| GeminiSourceBackedError::CountOverflow)?,
            )
            .ok_or(GeminiSourceBackedError::CountOverflow)?;
        Ok(Some(projected))
    }

    pub(crate) fn finish(self) -> GeminiSourceBackedResult<GeminiSourceBackedLeaf> {
        let outcome = self
            .native
            .outcome()
            .cloned()
            .ok_or(GeminiSourceBackedError::ReaderNotDrained)?;
        if outcome
            .checkpoint
            .session
            .as_ref()
            .map(|session| session.native_session_id.as_str())
            != Some(self.session.native_session_id.as_str())
            || outcome.checkpoint.retained_event_count != self.emitted_documents
        {
            return Err(GeminiSourceBackedError::CountMismatch);
        }

        let retained_records = self.emitted_documents;
        let rejected_records = outcome.rejected_records;
        let ignored_records = outcome
            .metrics
            .ignored_records
            .checked_add(outcome.metrics.header_records)
            .ok_or(GeminiSourceBackedError::CountOverflow)?;
        let complete_records = retained_records
            .checked_add(rejected_records)
            .and_then(|count| count.checked_add(ignored_records))
            .ok_or(GeminiSourceBackedError::CountOverflow)?;
        let counts = ScannedSourceCounts {
            complete_records,
            retained_records,
            rejected_records,
            ignored_records,
            indexed_documents: self.emitted_documents,
            certified_bytes: outcome.checkpoint.complete_prefix_end,
        };
        let opening = source_observation(&self.source, &self.source_observation)?;
        let closing = source_observation(&self.source, &outcome.terminal_source_observation)?;
        let frontier = SourceFrontier::new(
            GEMINI_FRONTIER_KIND,
            TypedKey::bytes(serde_json::to_vec(&GeminiSourceBackedFrontier::from(
                &outcome.checkpoint,
            ))?)?,
            outcome.checkpoint.complete_prefix_end,
            outcome.checkpoint.complete_prefix_sha256,
        )?;
        let certificate = CertifiedSource::certify_with_frontier(
            opening,
            closing,
            GEMINI_SOURCE_BACKED_PARSER_REVISION,
            outcome.checkpoint.complete_prefix_sha256,
            counts,
            Some(frontier),
        )?;
        Ok(GeminiSourceBackedLeaf {
            source: self.source,
            session: self.session,
            session_id: self.session_id,
            parent_session_id: self.parent_session_id,
            root_session_id: self.root_session_id,
            certificate,
        })
    }
}

#[derive(Debug, Serialize)]
struct GeminiSourceBackedFrontier {
    parser_revision: u32,
    policy_revision: u32,
    complete_prefix_end: u64,
    complete_prefix_sha256: [u8; 32],
    source_sha256: [u8; 32],
    next_raw_ordinal: u64,
    retained_event_count: u64,
    rejected_records: u64,
    append_boundary_safe: bool,
    terminal: bool,
}

impl From<&super::GeminiCheckpoint> for GeminiSourceBackedFrontier {
    fn from(checkpoint: &super::GeminiCheckpoint) -> Self {
        Self {
            parser_revision: checkpoint.parser_revision,
            policy_revision: checkpoint.policy_revision,
            complete_prefix_end: checkpoint.complete_prefix_end,
            complete_prefix_sha256: checkpoint.complete_prefix_sha256,
            source_sha256: checkpoint.source_sha256,
            next_raw_ordinal: checkpoint.next_raw_ordinal,
            retained_event_count: checkpoint.retained_event_count,
            rejected_records: checkpoint.rejected_records,
            append_boundary_safe: checkpoint.append_boundary_safe,
            terminal: checkpoint.terminal,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GeminiHydratedSourceRecord {
    pub(crate) provider_bytes: Vec<u8>,
    pub(crate) decoded_display_text: Option<String>,
}

/// Reopens one exact record from a freshly discovered Gemini leaf. The
/// provider-owned record digest remains valid across a benign append.
pub(crate) fn hydrate_gemini_source_backed_record(
    source: &GeminiTranscriptSource,
    locator: &SourceRecordLocator,
) -> GeminiSourceBackedResult<GeminiHydratedSourceRecord> {
    locator.validate_contract()?;
    let identity_reader = GeminiSourceBackedLeafReader::open(source)?;
    let expected_source = identity_reader.source().clone();
    drop(identity_reader);
    let (byte_offset, byte_length, physical_ordinal) = validate_locator(locator, &expected_source)?;
    let maximum = u64::try_from(MAX_PROVIDER_JSONL_LINE_BYTES)
        .map_err(|_| GeminiSourceBackedError::CountOverflow)?
        .checked_add(2)
        .ok_or(GeminiSourceBackedError::CountOverflow)?;
    if byte_length > maximum {
        return Err(GeminiSourceBackedError::LocatorRangeTooLarge);
    }
    let range_end = byte_offset
        .checked_add(byte_length)
        .ok_or(GeminiSourceBackedError::LocatorRangeTooLarge)?;
    if source.source_file.len() < range_end {
        return Err(GeminiSourceBackedError::LocatorRangeMissing);
    }
    let byte_length =
        usize::try_from(byte_length).map_err(|_| GeminiSourceBackedError::LocatorRangeTooLarge)?;
    let provider_bytes = source
        .source_file
        .read_exact_range(
            byte_offset,
            byte_length,
            MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2),
        )
        .map_err(GeminiSourceBackedError::Capture)?;
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

fn project_page(
    source: &SourceKey,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    source_path: &str,
    session: &GeminiSession,
    page: GeminiNativePage,
) -> GeminiSourceBackedResult<GeminiSourceBackedPage> {
    let mut documents = Vec::with_capacity(page.events.len());
    for event in page.events {
        documents.push(project_event(
            source,
            session_id,
            parent_session_id,
            root_session_id,
            source_path,
            session,
            event,
        )?);
    }
    Ok(GeminiSourceBackedPage {
        page_identity: *page.identity.as_bytes(),
        expected_prefix_bytes: page.expected_frontier.complete_prefix_end,
        next_prefix_bytes: page.next_safe_frontier.complete_prefix_end,
        documents,
    })
}

fn project_event(
    source: &SourceKey,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    source_path: &str,
    session: &GeminiSession,
    event: GeminiRetainedEvent,
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
        .ok_or(GeminiSourceBackedError::CountOverflow)?;
    let body = lexical_body(&event);
    if body.is_empty() {
        return Err(GeminiSourceBackedError::CountMismatch);
    }
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id,
        root_session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(session.native_session_id.clone()),
        // Gemini CLI JSONL does not expose VCS branch authority.
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

fn lexical_body(event: &GeminiRetainedEvent) -> String {
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

fn source_observation(
    source: &SourceKey,
    observation: &GeminiFileObservation,
) -> GeminiSourceBackedResult<SourceObservation> {
    Ok(SourceObservation::new(
        source.clone(),
        GEMINI_SOURCE_REVISION_KIND,
        serde_json::to_vec(observation)?,
    )?)
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
