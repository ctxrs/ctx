use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::Path,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, BatchHydrationRequest, BatchHydrationResult,
    CaptureProvider, CertifiedSource, ContentSourceResolver, EventHydrationRequest,
    EventIdentityInput, HydratedProviderRecord, HydrationFailure, HydrationFailureKind,
    LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
    PositionStability, ProjectionContractError, ScannedSourceCounts, SessionHydrationRequest,
    SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation, SourceRecordLocator,
    SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    parse_session_turn, strip_jsonl_ending, EventDraft, Frontier, RecordSetBinding, RuntimeState,
    SourceBackedBinding, SourceBackedTarget, CORE_PAGE_MAX_ROWS, MAX_RECORD_SET_ENTRIES,
    RECORD_SET_DIGEST_DOMAIN,
};
use crate::{
    provider::{
        normalization::provider_local_preview,
        providers::junie::{
            assistant::{
                junie_buffer_result_text, junie_merge_buffered_agent_event,
                junie_step_output_projection, JunieAssistantBuffer, JunieStepAgg,
            },
            session_tree::{
                bounded_junie_index_meta, junie_provider_session_id,
                visit_junie_session_event_paths, JunieSessionPath,
            },
            source::JunieSessionObservation,
            MAX_JUNIE_TRANSIENT_TURN_BYTES,
        },
    },
    CaptureError, JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
};

mod resolver;

pub(crate) use resolver::JunieLocatorResolverV0;
use resolver::{
    read_payload, read_record_set, replay_record_set, replay_user_prompt, RecordSetEntry,
};

const SOURCE_ANCHOR_NAMESPACE: &str = "junie.session-events";
const NATIVE_SESSION_NAMESPACE: &str = "junie.session";
const NATIVE_EVENT_POSITION_KIND: &str = "junie.normalized-event-index";
const LOGICAL_SESSION_KIND: &str = "junie-session";
const LOGICAL_EVENT_KIND: &str = "junie-event";
const SOURCE_SCHEMA_VARIANT: &str = "junie-session-events-v2";
const SOURCE_REVISION_KIND: &str = "junie-session-observation-v1";
const PARSER_REVISION: &str = "junie-source-backed-v2";
const RELATIVE_EVENTS_FILE: &str = "events.jsonl";
const RECORD_SET_COORDINATE_KIND: &str = "junie-record-set-coordinate-v2";
const USER_PROMPT_COORDINATE_KIND: &str = "junie-user-prompt-coordinate-v2";
const UNAVAILABLE_COORDINATE_NAMESPACE: &str = "junie.record-set-unavailable.v2";
const UNAVAILABLE_DIGEST_DOMAIN: &[u8] = b"ctx-junie-unavailable-record-set-v2\0";
const METADATA_TEXT_MAX_CHARS: usize = 2_048;

#[derive(Debug, Error)]
pub(crate) enum JunieSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Junie session-tree discovery retained {0} rejected index entries")]
    IncompleteDiscovery(u64),
    #[error("Junie source-backed scanner made no progress before a safe boundary")]
    StalledScanner,
    #[error("Junie source-backed scanner reached an incomplete trailing record")]
    IncompleteTrailingRecord,
    #[error("Junie native session {0:?} resolves to more than one events source")]
    DuplicateNativeSession(String),
    #[error("Junie source-backed event has no exact lexical text")]
    MissingLexicalBody,
    #[error("Junie source-backed exact lexical projection failed: {0}")]
    ExactLexicalProjection(String),
    #[error("Junie source-backed count overflow")]
    CountOverflow,
}

pub(crate) type JunieSourceBackedResultV0<T> = Result<T, JunieSourceBackedErrorV0>;

// Emissions move source authority as bounded units. Boxing the 496-byte
// certificate solely to match the 248-byte source key has no measured benefit.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(crate) enum JunieSourceBackedEmissionV0 {
    BeginSource(SourceKey),
    Documents(Vec<LexicalDocument>),
    CertifiedSource(CertifiedSource),
}

/// Provider-local, bounded source scanner.
///
/// The caller owns index lifecycle and publication. Each source starts with a
/// `BeginSource`, emits pages no larger than Junie's existing Core page bound,
/// and ends with the matching source certificate.
#[derive(Debug)]
pub(crate) struct JunieSourceBackedScannerV0 {
    imported_at: DateTime<Utc>,
    sessions: VecDeque<JunieSessionPath>,
    current: Option<CurrentSource>,
}

#[derive(Debug)]
struct CurrentSource {
    session_path: JunieSessionPath,
    provider_session_id: String,
    source: SourceKey,
    session_id: StableEntityId,
    opening: SourceObservation,
    opening_native: JunieSessionObservation,
    frontier: Frontier,
    pending_documents: VecDeque<LexicalDocument>,
    retained_records: u64,
    rejected_records: u64,
    began: bool,
    terminal: bool,
}

impl JunieSourceBackedScannerV0 {
    pub(crate) fn discover(
        root: impl AsRef<Path>,
        imported_at: DateTime<Utc>,
    ) -> JunieSourceBackedResultV0<Self> {
        let mut sessions = VecDeque::new();
        let visit = visit_junie_session_event_paths(root.as_ref(), &mut |session, _| {
            sessions.push_back(session);
            Ok(())
        })?;
        if visit.rejection_count != 0 {
            return Err(JunieSourceBackedErrorV0::IncompleteDiscovery(
                visit.rejection_count,
            ));
        }
        let mut native_sessions = HashSet::new();
        for session in &sessions {
            let provider_session_id = junie_provider_session_id(session)?;
            if !native_sessions.insert(provider_session_id.clone()) {
                return Err(JunieSourceBackedErrorV0::DuplicateNativeSession(
                    provider_session_id,
                ));
            }
        }
        Ok(Self {
            imported_at,
            sessions,
            current: None,
        })
    }

    pub(crate) fn next_page(
        &mut self,
    ) -> JunieSourceBackedResultV0<Option<JunieSourceBackedEmissionV0>> {
        loop {
            if self.current.is_none() {
                let Some(session_path) = self.sessions.pop_front() else {
                    return Ok(None);
                };
                self.current = Some(CurrentSource::open(session_path, self.imported_at)?);
            }
            let current = self
                .current
                .as_mut()
                .ok_or(JunieSourceBackedErrorV0::StalledScanner)?;
            if !current.began {
                current.began = true;
                return Ok(Some(JunieSourceBackedEmissionV0::BeginSource(
                    current.source.clone(),
                )));
            }
            if !current.pending_documents.is_empty() {
                let count = current.pending_documents.len().min(CORE_PAGE_MAX_ROWS);
                let documents = current.pending_documents.drain(..count).collect();
                return Ok(Some(JunieSourceBackedEmissionV0::Documents(documents)));
            }
            if current.terminal {
                let certificate = current.certify()?;
                self.current = None;
                return Ok(Some(JunieSourceBackedEmissionV0::CertifiedSource(
                    certificate,
                )));
            }
            current.scan_turn()?;
        }
    }
}

impl CurrentSource {
    fn open(
        session_path: JunieSessionPath,
        imported_at: DateTime<Utc>,
    ) -> JunieSourceBackedResultV0<Self> {
        let provider_session_id = junie_provider_session_id(&session_path)?;
        let source = source_key(&provider_session_id)?;
        let session_id = session_identity(&source, &provider_session_id)?;
        let opening_native = JunieSessionObservation::read(&session_path)?;
        let opening = source_observation(&source, &opening_native)?;
        let meta = bounded_junie_index_meta(&session_path.index_meta);
        Ok(Self {
            session_path,
            provider_session_id,
            source,
            session_id,
            opening,
            opening_native,
            frontier: Frontier {
                offset: 0,
                next_ordinal: 0,
                next_event_index: 0,
                prefix_sha256: Sha256::digest([]).into(),
                state: RuntimeState::fresh(&meta, imported_at),
            },
            pending_documents: VecDeque::new(),
            retained_records: 0,
            rejected_records: 0,
            began: false,
            terminal: false,
        })
    }

    fn scan_turn(&mut self) -> JunieSourceBackedResultV0<()> {
        let parsed = parse_session_turn(&self.session_path, &self.frontier)?;
        if parsed.incomplete {
            return Err(JunieSourceBackedErrorV0::IncompleteTrailingRecord);
        }
        if parsed.end_offset == self.frontier.offset
            && parsed.end_ordinal == self.frontier.next_ordinal
            && !parsed.terminal
        {
            return Err(JunieSourceBackedErrorV0::StalledScanner);
        }
        self.rejected_records = checked_add(self.rejected_records, parsed.rejection_count)?;
        let workspace = self
            .session_path
            .index_meta
            .project_dir
            .as_deref()
            .map(|value| provider_local_preview(value, METADATA_TEXT_MAX_CHARS).0);
        let cwd = parsed
            .after_state
            .cwd
            .as_deref()
            .map(|value| provider_local_preview(value, METADATA_TEXT_MAX_CHARS).0)
            .or_else(|| workspace.clone());
        let source_path = self.opening_native.canonical_path.to_string_lossy();
        let source_revision_digest = Sha256::digest(self.opening.revision()).into();
        let events_file = self.session_path.open_events()?;
        for row in parsed.rows {
            let document = lexical_document(
                &self.source,
                self.session_id,
                &self.provider_session_id,
                source_revision_digest,
                source_path.as_ref(),
                workspace.as_deref(),
                cwd.as_deref(),
                &events_file,
                row,
            )?;
            self.pending_documents.push_back(document);
            self.retained_records = checked_add(self.retained_records, 1)?;
        }
        self.frontier = Frontier {
            offset: parsed.end_offset,
            next_ordinal: parsed.end_ordinal,
            next_event_index: parsed.next_event_index,
            prefix_sha256: parsed.after_prefix_sha256,
            state: parsed.after_state,
        };
        self.terminal = parsed.terminal;
        Ok(())
    }

    fn certify(&self) -> JunieSourceBackedResultV0<CertifiedSource> {
        if self.frontier.offset != self.opening_native.events_file.length {
            return Err(JunieSourceBackedErrorV0::IncompleteTrailingRecord);
        }
        if self.session_path.require_supported_events
            && !self.frontier.state.saw_supported_event
            && self.rejected_records == 0
        {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: self.session_path.events_path.clone(),
                reason: "Junie events.jsonl contained no supported session events",
            }
            .into());
        }
        let closing_native = JunieSessionObservation::read(&self.session_path)?;
        let closing = source_observation(&self.source, &closing_native)?;
        let complete_records = checked_add(self.retained_records, self.rejected_records)?;
        Ok(CertifiedSource::certify(
            self.opening.clone(),
            closing,
            PARSER_REVISION,
            self.frontier.prefix_sha256,
            ScannedSourceCounts {
                complete_records,
                retained_records: self.retained_records,
                rejected_records: self.rejected_records,
                ignored_records: 0,
                indexed_documents: self.retained_records,
                certified_bytes: self.frontier.offset,
            },
        )?)
    }
}

fn checked_add(left: u64, right: u64) -> JunieSourceBackedResultV0<u64> {
    left.checked_add(right)
        .ok_or(JunieSourceBackedErrorV0::CountOverflow)
}

fn source_key(provider_session_id: &str) -> JunieSourceBackedResultV0<SourceKey> {
    Ok(SourceKey::derive(
        CaptureProvider::Junie.as_str(),
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        SourceAnchor::provider_native(
            SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8(provider_session_id)?,
        )?,
    )?)
}

fn source_observation(
    source: &SourceKey,
    observation: &JunieSessionObservation,
) -> JunieSourceBackedResultV0<SourceObservation> {
    Ok(SourceObservation::new(
        source.clone(),
        SOURCE_REVISION_KIND,
        observation.source_revision().into_bytes(),
    )?)
}

fn session_identity(
    source: &SourceKey,
    provider_session_id: &str,
) -> JunieSourceBackedResultV0<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(provider_session_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

// These nine arguments are the explicit certified identity and exact-hydration
// inputs at Junie's provider-local projection boundary.
#[allow(clippy::too_many_arguments)]
fn lexical_document(
    source: &SourceKey,
    session_id: StableEntityId,
    provider_session_id: &str,
    source_revision_digest: [u8; 32],
    source_path: &str,
    workspace: Option<&str>,
    cwd: Option<&str>,
    events_file: &crate::common::io::OpenedProviderSourceFile,
    row: EventDraft,
) -> JunieSourceBackedResultV0<LexicalDocument> {
    let native_item_key = NativeItemKey::certified_position(
        NATIVE_EVENT_POSITION_KIND,
        TypedKey::U64(row.event_index),
        PositionStability::AppendStable,
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let locator = source_locator(
        source,
        provider_session_id,
        row.event_index,
        source_revision_digest,
        &row.source_backed_binding,
    )?;
    let body = exact_junie_lexical_body(events_file, &row.source_backed_binding)?
        .unwrap_or_else(|| unavailable_junie_lexical_body(&row));
    if body.is_empty() {
        return Err(JunieSourceBackedErrorV0::MissingLexicalBody);
    }
    let touched_files = row
        .file_change
        .as_ref()
        .map(|change| vec![change.path.clone()])
        .unwrap_or_default();
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(provider_session_id.to_owned()),
        branch: None,
        source_path: Some(source_path.to_owned()),
        agent_type: AgentType::Primary.as_str().to_owned(),
        is_primary: true,
        event_sequence: row.event_index,
        occurred_at_unix_ms: Some(row.occurred_at.timestamp_millis()),
        event_type: row.event_type.as_str().to_owned(),
        role: row.role.map(|role| role.as_str().to_owned()),
        body,
        workspace: workspace.map(str::to_owned),
        cwd: cwd.map(str::to_owned),
        touched_files,
    })
}

fn unavailable_junie_lexical_body(row: &EventDraft) -> String {
    match &row.source_backed_binding.target {
        SourceBackedTarget::StepOutput { .. } => row
            .body
            .get("details")
            .and_then(Value::as_str)
            .map_or_else(|| row.text.clone(), str::to_owned),
        _ => row.text.clone(),
    }
}

fn exact_junie_lexical_body(
    file: &crate::common::io::OpenedProviderSourceFile,
    binding: &SourceBackedBinding,
) -> JunieSourceBackedResultV0<Option<String>> {
    if binding.records.unavailable || binding.records.entries.is_empty() {
        return Ok(None);
    }
    let exact = match &binding.target {
        SourceBackedTarget::UserPrompt => {
            let entry = binding.records.entries.first().ok_or_else(|| {
                JunieSourceBackedErrorV0::ExactLexicalProjection(
                    "user prompt has no native source record".to_owned(),
                )
            })?;
            let payload = read_payload(
                file,
                entry.byte_start,
                entry.byte_end_exclusive.saturating_sub(entry.byte_start),
            )
            .map_err(scan_projection_failure)?;
            if Sha256::digest(&payload).as_slice() != entry.payload_sha256 {
                return Err(JunieSourceBackedErrorV0::ExactLexicalProjection(
                    "user prompt source record digest changed during scan".to_owned(),
                ));
            }
            replay_user_prompt(entry.ordinal, &payload).map_err(scan_projection_failure)?
        }
        target => {
            let values = read_record_set(
                file,
                &binding
                    .records
                    .entries
                    .iter()
                    .map(|entry| RecordSetEntry {
                        ordinal: entry.ordinal,
                        byte_start: entry.byte_start,
                        byte_end_exclusive: entry.byte_end_exclusive,
                        payload_digest: entry.payload_sha256,
                    })
                    .collect::<Vec<_>>(),
                &aggregate_digest(&binding.records),
            )
            .map_err(scan_projection_failure)?;
            replay_record_set(target, &values).map_err(scan_projection_failure)?
        }
    };
    Ok(Some(exact))
}

fn scan_projection_failure(failure: HydrationFailure) -> JunieSourceBackedErrorV0 {
    JunieSourceBackedErrorV0::ExactLexicalProjection(format!(
        "{:?}: {}",
        failure.kind, failure.detail
    ))
}

fn source_locator(
    source: &SourceKey,
    provider_session_id: &str,
    event_sequence: u64,
    source_revision_digest: [u8; 32],
    binding: &SourceBackedBinding,
) -> JunieSourceBackedResultV0<SourceRecordLocator> {
    if binding.target == SourceBackedTarget::UserPrompt
        && !binding.records.unavailable
        && binding.records.entries.len() == 1
    {
        let entry = &binding.records.entries[0];
        return Ok(SourceRecordLocator::new(
            source.clone(),
            NativeRecordCoordinate::Jsonl {
                byte_offset: entry.byte_start,
                byte_length: entry.byte_end_exclusive.saturating_sub(entry.byte_start),
                physical_ordinal: entry.ordinal,
                native_session_key: Some(TypedKey::utf8(provider_session_id)?),
                native_event_key: Some(TypedKey::composite(vec![
                    TypedKey::utf8(USER_PROMPT_COORDINATE_KIND)?,
                    TypedKey::U64(event_sequence),
                ])?),
            },
            LocatorRevisionPolicy::StableRecordEvidence,
            Some(source_revision_digest),
            entry.payload_sha256,
        )?);
    }

    if binding.records.unavailable || binding.records.entries.is_empty() {
        let target = target_key(&binding.target)?;
        let coordinate = TypedKey::composite(vec![target.clone(), TypedKey::U64(event_sequence)])?;
        return Ok(SourceRecordLocator::new(
            source.clone(),
            NativeRecordCoordinate::ProviderNative {
                namespace: UNAVAILABLE_COORDINATE_NAMESPACE.to_owned(),
                coordinate,
            },
            LocatorRevisionPolicy::StableRecordEvidence,
            Some(source_revision_digest),
            unavailable_digest(event_sequence, &target),
        )?);
    }

    let mut entries = Vec::with_capacity(binding.records.entries.len());
    for entry in &binding.records.entries {
        entries.push(TypedKey::composite(vec![
            TypedKey::U64(entry.ordinal),
            TypedKey::U64(entry.byte_start),
            TypedKey::U64(entry.byte_end_exclusive),
            TypedKey::bytes(entry.payload_sha256.to_vec())?,
        ])?);
    }
    let coordinate = TypedKey::composite(vec![
        TypedKey::utf8(RECORD_SET_COORDINATE_KIND)?,
        TypedKey::U64(event_sequence),
        target_key(&binding.target)?,
        TypedKey::composite(entries)?,
    ])?;
    Ok(SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::TreeRecord {
            relative_file_key: TypedKey::utf8(RELATIVE_EVENTS_FILE)?,
            record_coordinate: coordinate,
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        Some(source_revision_digest),
        aggregate_digest(&binding.records),
    )?)
}

fn unavailable_digest(event_sequence: u64, target: &TypedKey) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(UNAVAILABLE_DIGEST_DOMAIN);
    digest.update(event_sequence.to_be_bytes());
    digest.update(format!("{target:?}").as_bytes());
    digest.finalize().into()
}

fn target_key(target: &SourceBackedTarget) -> JunieSourceBackedResultV0<TypedKey> {
    let (tag, first, second) = match target {
        SourceBackedTarget::UserPrompt => (1, 0, 0),
        SourceBackedTarget::AssistantMessage => (2, 0, 0),
        SourceBackedTarget::StepCall { step_order } => (3, u64::from(*step_order), 0),
        SourceBackedTarget::StepOutput { step_order } => (4, u64::from(*step_order), 0),
        SourceBackedTarget::FileChange {
            step_order,
            change_index,
        } => (5, u64::from(*step_order), u64::from(*change_index)),
    };
    Ok(TypedKey::composite(vec![
        TypedKey::U64(tag),
        TypedKey::U64(first),
        TypedKey::U64(second),
    ])?)
}

fn aggregate_digest(binding: &RecordSetBinding) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(RECORD_SET_DIGEST_DOMAIN);
    digest.update((binding.entries.len() as u64).to_be_bytes());
    for entry in &binding.entries {
        digest.update(entry.ordinal.to_be_bytes());
        digest.update(entry.byte_start.to_be_bytes());
        digest.update(entry.byte_end_exclusive.to_be_bytes());
        digest.update(entry.payload_sha256);
    }
    digest.finalize().into()
}

#[cfg(test)]
mod tests;
