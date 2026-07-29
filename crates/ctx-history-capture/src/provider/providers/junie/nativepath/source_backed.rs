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

#[derive(Debug)]
struct ResolvedSource {
    session_path: JunieSessionPath,
    source: SourceKey,
    session_id: StableEntityId,
    provider_session_id: String,
}

#[derive(Debug)]
pub(crate) struct JunieLocatorResolverV0 {
    sources: HashMap<StableEntityId, ResolvedSource>,
}

impl JunieLocatorResolverV0 {
    pub(crate) fn discover(root: impl AsRef<Path>) -> JunieSourceBackedResultV0<Self> {
        let mut sources = HashMap::new();
        let visit = visit_junie_session_event_paths(root.as_ref(), &mut |session_path, _| {
            let provider_session_id = junie_provider_session_id(&session_path)?;
            let source = source_key(&provider_session_id).map_err(|error| {
                CaptureError::InvalidPayload(format!(
                    "Junie source-backed identity is invalid: {error}"
                ))
            })?;
            let session_id = session_identity(&source, &provider_session_id).map_err(|error| {
                CaptureError::InvalidPayload(format!(
                    "Junie source-backed session identity is invalid: {error}"
                ))
            })?;
            let resolved = ResolvedSource {
                session_path,
                source: source.clone(),
                session_id,
                provider_session_id: provider_session_id.clone(),
            };
            if sources.insert(source.identity(), resolved).is_some() {
                return Err(CaptureError::InvalidPayload(format!(
                    "Junie native session {provider_session_id:?} resolves to more than one source"
                )));
            }
            Ok(())
        })?;
        if visit.rejection_count != 0 {
            return Err(JunieSourceBackedErrorV0::IncompleteDiscovery(
                visit.rejection_count,
            ));
        }
        Ok(Self { sources })
    }

    pub(crate) fn discover_for_hydration(root: impl AsRef<Path>) -> Result<Self, HydrationFailure> {
        Self::discover(root)
            .map_err(|_| hydration_failure(HydrationFailureKind::TemporarilyUnavailable))
    }

    pub(crate) fn hydrate_requests(
        &self,
        requests: &[EventHydrationRequest],
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        let Some(first) = requests.first() else {
            return Ok(Vec::new());
        };
        let resolved = self
            .sources
            .get(&first.locator().source().identity())
            .ok_or_else(|| hydration_failure(HydrationFailureKind::ConfirmedDeleted))?;
        for request in requests {
            request
                .locator()
                .validate_contract()
                .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?;
            validate_locator_source(request.locator(), resolved)?;
        }
        let opening = JunieSessionObservation::read(&resolved.session_path)
            .map_err(|_| hydration_failure(HydrationFailureKind::TemporarilyUnavailable))?;
        let opening_observation = source_observation(&resolved.source, &opening)
            .map_err(|_| hydration_failure(HydrationFailureKind::TemporarilyUnavailable))?;
        let opening_revision_digest: [u8; 32] =
            Sha256::digest(opening_observation.revision()).into();
        validate_revision_policy(first.locator(), opening_revision_digest)?;
        let file = resolved
            .session_path
            .open_events()
            .map_err(|_| hydration_failure(HydrationFailureKind::TemporarilyUnavailable))?;
        let hydrated = requests
            .iter()
            .map(|request| {
                validate_revision_policy(request.locator(), opening_revision_digest)?;
                self.hydrate_from_file(request, resolved, &file)
            })
            .collect::<Result<Vec<_>, _>>()?;
        file.revalidate()
            .and_then(|()| resolved.session_path.revalidate_root())
            .map_err(|_| hydration_failure(HydrationFailureKind::StaleSourceEvidence))?;
        let closing = JunieSessionObservation::read(&resolved.session_path)
            .map_err(|_| hydration_failure(HydrationFailureKind::TemporarilyUnavailable))?;
        if closing != opening {
            return Err(hydration_failure(HydrationFailureKind::StaleSourceEvidence));
        }
        Ok(hydrated)
    }

    fn hydrate_from_file(
        &self,
        request: &EventHydrationRequest,
        resolved: &ResolvedSource,
        file: &crate::common::io::OpenedProviderSourceFile,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        validate_locator_source(request.locator(), resolved)?;
        let exact_text = match request.locator().coordinate() {
            NativeRecordCoordinate::Jsonl {
                byte_offset,
                byte_length,
                physical_ordinal,
                native_session_key,
                native_event_key,
            } => {
                let expected_event_key = TypedKey::composite(vec![
                    TypedKey::utf8(USER_PROMPT_COORDINATE_KIND)
                        .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?,
                    TypedKey::U64(request_event_sequence(native_event_key)?),
                ])
                .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?;
                if native_session_key.as_ref()
                    != Some(&TypedKey::Utf8(resolved.provider_session_id.clone()))
                    || native_event_key.as_ref() != Some(&expected_event_key)
                {
                    return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
                }
                validate_event_identity(
                    request,
                    resolved,
                    request_event_sequence(native_event_key)?,
                )?;
                let payload = read_payload(file, *byte_offset, *byte_length)?;
                if Sha256::digest(&payload).as_slice() != request.locator().record_digest() {
                    return Err(hydration_failure(HydrationFailureKind::StaleRecordEvidence));
                }
                replay_user_prompt(*physical_ordinal, &payload)?
            }
            NativeRecordCoordinate::TreeRecord {
                relative_file_key,
                record_coordinate,
            } => {
                if relative_file_key != &TypedKey::Utf8(RELATIVE_EVENTS_FILE.to_owned()) {
                    return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
                }
                let (event_sequence, target, entries) = decode_record_set(record_coordinate)?;
                validate_event_identity(request, resolved, event_sequence)?;
                let values = read_record_set(file, &entries, request.locator().record_digest())?;
                replay_record_set(&target, &values)?
            }
            NativeRecordCoordinate::ProviderNative {
                namespace,
                coordinate,
            } if namespace == UNAVAILABLE_COORDINATE_NAMESPACE => {
                let (target, event_sequence) = decode_unavailable_coordinate(coordinate)?;
                validate_event_identity(request, resolved, event_sequence)?;
                if &unavailable_digest(event_sequence, &target) != request.locator().record_digest()
                {
                    return Err(hydration_failure(HydrationFailureKind::StaleRecordEvidence));
                }
                return Err(HydrationFailure {
                    kind: HydrationFailureKind::UnsupportedParserRevision,
                    detail: format!(
                        "Junie exact reopening requires at most {MAX_RECORD_SET_ENTRIES} source records"
                    ),
                });
            }
            _ => return Err(hydration_failure(HydrationFailureKind::InvalidLocator)),
        };
        Ok(HydratedProviderRecord {
            event_id: request.event_id(),
            provider_bytes: exact_text.into_bytes(),
        })
    }
}

impl ContentSourceResolver for JunieLocatorResolverV0 {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        let mut hydrated = self.hydrate_requests(std::slice::from_ref(request))?;
        hydrated
            .pop()
            .ok_or_else(|| hydration_failure(HydrationFailureKind::MissingRecord))
    }

    fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let result = BatchHydrationResult::new(self.hydrate_requests(request.events())?).map_err(
            |error| HydrationFailure {
                kind: HydrationFailureKind::InvalidLocator,
                detail: format!("invalid Junie batch hydration result: {error}"),
            },
        )?;
        result.validate_for_request(request)?;
        Ok(result)
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        self.hydrate_requests(request.events())
    }
}

fn validate_revision_policy(
    locator: &SourceRecordLocator,
    current_revision_digest: [u8; 32],
) -> Result<(), HydrationFailure> {
    let expected = locator
        .certified_source_revision_digest()
        .copied()
        .ok_or_else(|| hydration_failure(HydrationFailureKind::InvalidLocator))?;
    match locator.revision_policy() {
        LocatorRevisionPolicy::StableRecordEvidence => Ok(()),
        LocatorRevisionPolicy::ExactSourceRevision if expected == current_revision_digest => Ok(()),
        LocatorRevisionPolicy::ExactSourceRevision => {
            Err(hydration_failure(HydrationFailureKind::StaleSourceEvidence))
        }
    }
}

fn validate_event_identity(
    request: &EventHydrationRequest,
    resolved: &ResolvedSource,
    event_sequence: u64,
) -> Result<(), HydrationFailure> {
    let native_item_key = NativeItemKey::certified_position(
        NATIVE_EVENT_POSITION_KIND,
        TypedKey::U64(event_sequence),
        PositionStability::AppendStable,
    )
    .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?;
    let expected = derive_event_id(EventIdentityInput {
        source: &resolved.source,
        session_id: resolved.session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?;
    if expected != request.event_id() {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    }
    Ok(())
}

fn validate_locator_source(
    locator: &SourceRecordLocator,
    resolved: &ResolvedSource,
) -> Result<(), HydrationFailure> {
    if locator.source().provider() != CaptureProvider::Junie.as_str()
        || locator.source().source_format() != JUNIE_SESSION_EVENTS_SOURCE_FORMAT
        || locator.source().schema_variant() != SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
        || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_none()
        || !locator.source().exact_descriptor_eq(&resolved.source)
    {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    }
    let SourceAnchor::ProviderNative { namespace, key } = locator.source().anchor() else {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    };
    if namespace != SOURCE_ANCHOR_NAMESPACE
        || key != &TypedKey::Utf8(resolved.provider_session_id.clone())
    {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    }
    Ok(())
}

fn request_event_sequence(native_event_key: &Option<TypedKey>) -> Result<u64, HydrationFailure> {
    let Some(TypedKey::Composite(parts)) = native_event_key else {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    };
    let [TypedKey::Utf8(kind), TypedKey::U64(sequence)] = parts.as_slice() else {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    };
    if kind != USER_PROMPT_COORDINATE_KIND {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    }
    Ok(*sequence)
}

fn read_payload(
    file: &crate::common::io::OpenedProviderSourceFile,
    byte_offset: u64,
    byte_length: u64,
) -> Result<Vec<u8>, HydrationFailure> {
    if byte_length == 0 || byte_length > MAX_JUNIE_TRANSIENT_TURN_BYTES as u64 {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    }
    let length = usize::try_from(byte_length)
        .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?;
    let record = file
        .read_exact_range(byte_offset, length, MAX_JUNIE_TRANSIENT_TURN_BYTES)
        .map_err(|error| match error {
            CaptureError::InvalidPayload(_) => {
                hydration_failure(HydrationFailureKind::MissingRecord)
            }
            _ => hydration_failure(HydrationFailureKind::TemporarilyUnavailable),
        })?;
    Ok(strip_jsonl_ending(&record).to_vec())
}

#[derive(Debug)]
struct RecordSetEntry {
    ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
    payload_digest: [u8; 32],
}

fn decode_record_set(
    coordinate: &TypedKey,
) -> Result<(u64, SourceBackedTarget, Vec<RecordSetEntry>), HydrationFailure> {
    let TypedKey::Composite(parts) = coordinate else {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    };
    let [TypedKey::Utf8(kind), TypedKey::U64(event_sequence), target, TypedKey::Composite(encoded_entries)] =
        parts.as_slice()
    else {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    };
    if kind != RECORD_SET_COORDINATE_KIND {
        return Err(hydration_failure(
            HydrationFailureKind::UnsupportedParserRevision,
        ));
    }
    if encoded_entries.is_empty() || encoded_entries.len() > MAX_RECORD_SET_ENTRIES {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    }
    let target = decode_target(target)?;
    let mut entries = Vec::with_capacity(encoded_entries.len());
    for encoded in encoded_entries {
        let TypedKey::Composite(parts) = encoded else {
            return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
        };
        let [TypedKey::U64(ordinal), TypedKey::U64(byte_start), TypedKey::U64(byte_end_exclusive), TypedKey::Bytes(payload_digest)] =
            parts.as_slice()
        else {
            return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
        };
        let payload_digest: [u8; 32] = payload_digest
            .as_slice()
            .try_into()
            .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?;
        if byte_start >= byte_end_exclusive
            || entries.last().is_some_and(|prior: &RecordSetEntry| {
                prior.ordinal >= *ordinal || prior.byte_end_exclusive > *byte_start
            })
        {
            return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
        }
        entries.push(RecordSetEntry {
            ordinal: *ordinal,
            byte_start: *byte_start,
            byte_end_exclusive: *byte_end_exclusive,
            payload_digest,
        });
    }
    Ok((*event_sequence, target, entries))
}

fn decode_target(target: &TypedKey) -> Result<SourceBackedTarget, HydrationFailure> {
    let TypedKey::Composite(parts) = target else {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    };
    let [TypedKey::U64(tag), TypedKey::U64(first), TypedKey::U64(second)] = parts.as_slice() else {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    };
    match (*tag, *first, *second) {
        (1, 0, 0) => Ok(SourceBackedTarget::UserPrompt),
        (2, 0, 0) => Ok(SourceBackedTarget::AssistantMessage),
        (3, first, 0) => Ok(SourceBackedTarget::StepCall {
            step_order: u32::try_from(first)
                .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?,
        }),
        (4, first, 0) => Ok(SourceBackedTarget::StepOutput {
            step_order: u32::try_from(first)
                .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?,
        }),
        (5, first, second) => Ok(SourceBackedTarget::FileChange {
            step_order: u32::try_from(first)
                .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?,
            change_index: u32::try_from(second)
                .map_err(|_| hydration_failure(HydrationFailureKind::InvalidLocator))?,
        }),
        _ => Err(hydration_failure(HydrationFailureKind::InvalidLocator)),
    }
}

fn read_record_set(
    file: &crate::common::io::OpenedProviderSourceFile,
    entries: &[RecordSetEntry],
    expected_digest: &[u8; 32],
) -> Result<Vec<(u64, Value)>, HydrationFailure> {
    let total_bytes = entries.iter().try_fold(0_u64, |total, entry| {
        total.checked_add(entry.byte_end_exclusive.saturating_sub(entry.byte_start))
    });
    if total_bytes.is_none_or(|bytes| bytes > MAX_JUNIE_TRANSIENT_TURN_BYTES as u64) {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    }
    let mut aggregate = Sha256::new();
    aggregate.update(RECORD_SET_DIGEST_DOMAIN);
    aggregate.update((entries.len() as u64).to_be_bytes());
    let mut values = Vec::with_capacity(entries.len());
    for entry in entries {
        let payload = read_payload(
            file,
            entry.byte_start,
            entry.byte_end_exclusive.saturating_sub(entry.byte_start),
        )?;
        let observed: [u8; 32] = Sha256::digest(&payload).into();
        if observed != entry.payload_digest {
            return Err(hydration_failure(HydrationFailureKind::StaleRecordEvidence));
        }
        aggregate.update(entry.ordinal.to_be_bytes());
        aggregate.update(entry.byte_start.to_be_bytes());
        aggregate.update(entry.byte_end_exclusive.to_be_bytes());
        aggregate.update(observed);
        let value = serde_json::from_slice(&payload)
            .map_err(|_| hydration_failure(HydrationFailureKind::UnsupportedParserRevision))?;
        values.push((entry.ordinal, value));
    }
    let observed: [u8; 32] = aggregate.finalize().into();
    if &observed != expected_digest {
        return Err(hydration_failure(HydrationFailureKind::StaleRecordEvidence));
    }
    Ok(values)
}

fn replay_user_prompt(ordinal: u64, payload: &[u8]) -> Result<String, HydrationFailure> {
    let value: Value = serde_json::from_slice(payload)
        .map_err(|_| hydration_failure(HydrationFailureKind::UnsupportedParserRevision))?;
    if value.get("kind").and_then(Value::as_str) != Some("UserPromptEvent") {
        return Err(hydration_failure(
            HydrationFailureKind::UnsupportedParserRevision,
        ));
    }
    value
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            let _ = ordinal;
            hydration_failure(HydrationFailureKind::MissingRecord)
        })
}

fn replay_record_set(
    target: &SourceBackedTarget,
    values: &[(u64, Value)],
) -> Result<String, HydrationFailure> {
    let mut buffer = JunieAssistantBuffer::default();
    for (ordinal, value) in values {
        if value.get("kind").and_then(Value::as_str) != Some("SessionA2uxEvent") {
            return Err(hydration_failure(
                HydrationFailureKind::UnsupportedParserRevision,
            ));
        }
        let agent = value
            .get("event")
            .and_then(|event| event.get("agentEvent"))
            .ok_or_else(|| hydration_failure(HydrationFailureKind::UnsupportedParserRevision))?;
        let occurred_at = value
            .get("timestampMs")
            .and_then(Value::as_i64)
            .and_then(DateTime::<Utc>::from_timestamp_millis)
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
        if !junie_merge_buffered_agent_event(
            &mut buffer,
            agent,
            ordinal.saturating_add(1),
            occurred_at,
        ) {
            return Err(hydration_failure(
                HydrationFailureKind::UnsupportedParserRevision,
            ));
        }
    }
    match target {
        SourceBackedTarget::UserPrompt => {
            Err(hydration_failure(HydrationFailureKind::InvalidLocator))
        }
        SourceBackedTarget::AssistantMessage => {
            let text = junie_buffer_result_text(&buffer);
            if text.is_empty() {
                Err(hydration_failure(HydrationFailureKind::MissingRecord))
            } else {
                Ok(text)
            }
        }
        SourceBackedTarget::StepCall { step_order } => {
            let step = step_by_order(&buffer, *step_order)?;
            Ok(step_call_text(step))
        }
        SourceBackedTarget::StepOutput { step_order } => {
            let step = step_by_order(&buffer, *step_order)?;
            junie_step_output_projection(step)
                .map(|output| output.details.to_owned())
                .ok_or_else(|| hydration_failure(HydrationFailureKind::MissingRecord))
        }
        SourceBackedTarget::FileChange {
            step_order,
            change_index,
        } => {
            let step = step_by_order(&buffer, *step_order)?;
            let change = step
                .changes
                .get(*change_index as usize)
                .ok_or_else(|| hydration_failure(HydrationFailureKind::MissingRecord))?;
            let path = change
                .get("afterRelativePath")
                .and_then(Value::as_str)
                .or_else(|| change.get("beforeRelativePath").and_then(Value::as_str))
                .filter(|path| !path.trim().is_empty())
                .ok_or_else(|| hydration_failure(HydrationFailureKind::MissingRecord))?;
            Ok(format!("Edit: {path}"))
        }
    }
}

fn step_by_order(
    buffer: &JunieAssistantBuffer,
    step_order: u32,
) -> Result<&JunieStepAgg, HydrationFailure> {
    let step_id = buffer
        .step_ids_in_order
        .get(step_order as usize)
        .ok_or_else(|| hydration_failure(HydrationFailureKind::MissingRecord))?;
    buffer
        .steps
        .get(step_id)
        .ok_or_else(|| hydration_failure(HydrationFailureKind::MissingRecord))
}

fn step_call_text(step: &JunieStepAgg) -> String {
    if let Some(command) = &step.command {
        format!("Bash: {command}")
    } else if step.files.is_some() {
        step.label
            .clone()
            .unwrap_or_else(|| "View files".to_owned())
    } else {
        step.label
            .clone()
            .unwrap_or_else(|| "Junie tool step".to_owned())
    }
}

fn decode_unavailable_coordinate(
    coordinate: &TypedKey,
) -> Result<(TypedKey, u64), HydrationFailure> {
    let TypedKey::Composite(parts) = coordinate else {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    };
    let [target, TypedKey::U64(event_sequence)] = parts.as_slice() else {
        return Err(hydration_failure(HydrationFailureKind::InvalidLocator));
    };
    decode_target(target)?;
    Ok((target.clone(), *event_sequence))
}

fn hydration_failure(kind: HydrationFailureKind) -> HydrationFailure {
    HydrationFailure {
        kind,
        detail: "Junie source-backed locator could not be verified".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tree(root: &Path, session_id: &str, records: &[Value]) {
        let session = root.join(session_id);
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(
            root.join("index.jsonl"),
            format!(
                "{}\n",
                serde_json::json!({
                    "sessionId": session_id,
                    "createdAt": 1_783_339_200_000_i64,
                    "taskName": "Junie source-backed fixture",
                    "projectDir": "/workspace/junie",
                })
            ),
        )
        .unwrap();
        let mut events = String::new();
        for record in records {
            events.push_str(&serde_json::to_string(record).unwrap());
            events.push('\n');
        }
        std::fs::write(session.join(RELATIVE_EVENTS_FILE), events).unwrap();
    }

    fn scan_documents(root: &Path) -> Vec<LexicalDocument> {
        let mut scanner =
            JunieSourceBackedScannerV0::discover(root, DateTime::<Utc>::UNIX_EPOCH).unwrap();
        let mut began = 0;
        let mut certified = 0;
        let mut documents = Vec::new();
        while let Some(page) = scanner.next_page().unwrap() {
            match page {
                JunieSourceBackedEmissionV0::BeginSource(_) => began += 1,
                JunieSourceBackedEmissionV0::Documents(page) => documents.extend(page),
                JunieSourceBackedEmissionV0::CertifiedSource(source) => {
                    assert_eq!(source.counts().indexed_documents, documents.len() as u64);
                    certified += 1;
                }
            }
        }
        assert_eq!(began, 1);
        assert_eq!(certified, 1);
        documents
    }

    fn hydrate(
        root: &Path,
        document: &LexicalDocument,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        let resolver = JunieLocatorResolverV0::discover(root).unwrap();
        let request =
            EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap();
        resolver.hydrate_event(&request)
    }

    fn request(document: &LexicalDocument) -> EventHydrationRequest {
        EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap()
    }

    #[test]
    fn junie_exact_hydration_indexes_full_body_tail_terms_and_is_session_ordered() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let first = format!("first-head-{}-first-tail", "a".repeat(4_096));
        let second = format!("second-head-{}-second-tail", "b".repeat(4_096));
        write_tree(
            temp.path(),
            "ordered-session",
            &[
                serde_json::json!({
                    "kind": "UserPromptEvent",
                    "prompt": first.clone(),
                }),
                serde_json::json!({
                    "kind": "UserPromptEvent",
                    "prompt": second.clone(),
                }),
            ],
        );
        let documents = scan_documents(temp.path());
        assert_eq!(documents.len(), 2);
        assert!(documents.iter().all(|document| {
            document
                .locator
                .certified_source_revision_digest()
                .is_some()
        }));
        assert_eq!(documents[0].body, first);
        assert_eq!(documents[1].body, second);
        assert!(documents[0].body.contains("first-tail"));
        assert!(documents[1].body.contains("second-tail"));

        let session_request = SessionHydrationRequest::new(
            documents[0].session_id,
            vec![request(&documents[1]), request(&documents[0])],
        )
        .unwrap();
        let hydrated = JunieLocatorResolverV0::discover_for_hydration(temp.path())
            .unwrap()
            .hydrate_session(&session_request)
            .unwrap()
            .into_iter()
            .map(|record| String::from_utf8(record.provider_bytes).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(hydrated, vec![second, first]);
    }

    #[test]
    fn junie_rewrite_digest_and_truncation_have_distinct_typed_failures() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let original = [
            serde_json::json!({
                "kind": "UserPromptEvent",
                "prompt": "before",
            }),
            serde_json::json!({
                "kind": "UserPromptEvent",
                "prompt": "second",
            }),
        ];
        write_tree(temp.path(), "mutable-session", &original);
        let documents = scan_documents(temp.path());
        let first_request = request(&documents[0]);
        let second_request = request(&documents[1]);

        write_tree(
            temp.path(),
            "mutable-session",
            &[
                serde_json::json!({
                    "kind": "UserPromptEvent",
                    "prompt": "rewrit",
                }),
                original[1].clone(),
            ],
        );
        let stale = JunieLocatorResolverV0::discover_for_hydration(temp.path())
            .unwrap()
            .hydrate_event(&first_request)
            .unwrap_err();
        assert_eq!(stale.kind, HydrationFailureKind::StaleRecordEvidence);

        write_tree(temp.path(), "mutable-session", &original[..1]);
        let missing = JunieLocatorResolverV0::discover_for_hydration(temp.path())
            .unwrap()
            .hydrate_event(&second_request)
            .unwrap_err();
        assert_eq!(missing.kind, HydrationFailureKind::MissingRecord);
        let batch = BatchHydrationRequest::new(vec![first_request.clone(), second_request.clone()])
            .unwrap();
        let failed_batch = JunieLocatorResolverV0::discover_for_hydration(temp.path())
            .unwrap()
            .hydrate_batch(&batch)
            .unwrap_err();
        assert_eq!(failed_batch.kind, HydrationFailureKind::MissingRecord);
    }

    #[test]
    fn junie_source_deletion_and_unavailable_root_are_not_conflated() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        write_tree(
            temp.path(),
            "deleted-session",
            &[serde_json::json!({
                "kind": "UserPromptEvent",
                "prompt": "present",
            })],
        );
        let documents = scan_documents(temp.path());
        let event_request = request(&documents[0]);

        std::fs::remove_dir_all(temp.path().join("deleted-session")).unwrap();
        let deleted = JunieLocatorResolverV0::discover_for_hydration(temp.path())
            .unwrap()
            .hydrate_event(&event_request)
            .unwrap_err();
        assert_eq!(deleted.kind, HydrationFailureKind::ConfirmedDeleted);

        let unavailable_root = temp.path().join("unavailable-root");
        let unavailable =
            JunieLocatorResolverV0::discover_for_hydration(&unavailable_root).unwrap_err();
        assert_eq!(
            unavailable.kind,
            HydrationFailureKind::TemporarilyUnavailable
        );
    }

    #[test]
    fn junie_digest_matching_malformed_native_record_is_unsupported() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        write_tree(
            temp.path(),
            "malformed-session",
            &[serde_json::json!({
                "kind": "UserPromptEvent",
                "prompt": "present",
            })],
        );
        let document = scan_documents(temp.path()).pop().unwrap();
        let NativeRecordCoordinate::Jsonl {
            physical_ordinal,
            native_session_key,
            native_event_key,
            ..
        } = document.locator.coordinate()
        else {
            panic!("Junie prompt must use a JSONL coordinate");
        };
        let malformed_payload = b"{not-json}";
        let locator = SourceRecordLocator::new(
            document.source.clone(),
            NativeRecordCoordinate::Jsonl {
                byte_offset: 0,
                byte_length: u64::try_from(malformed_payload.len() + 1).unwrap(),
                physical_ordinal: *physical_ordinal,
                native_session_key: native_session_key.clone(),
                native_event_key: native_event_key.clone(),
            },
            LocatorRevisionPolicy::StableRecordEvidence,
            document.locator.certified_source_revision_digest().copied(),
            Sha256::digest(malformed_payload).into(),
        )
        .unwrap();
        std::fs::write(
            temp.path().join("malformed-session/events.jsonl"),
            [malformed_payload.as_slice(), b"\n"].concat(),
        )
        .unwrap();
        let event_request = EventHydrationRequest::new(document.event_id, locator).unwrap();
        let failure = JunieLocatorResolverV0::discover_for_hydration(temp.path())
            .unwrap()
            .hydrate_event(&event_request)
            .unwrap_err();
        assert_eq!(
            failure.kind,
            HydrationFailureKind::UnsupportedParserRevision
        );
    }

    #[test]
    fn junie_source_backed_has_no_preview_complete_or_legacy_store_fallback() {
        let provider_source = include_str!("source_backed.rs");
        let nativepath_source = include_str!("../nativepath.rs");
        let projection_source = include_str!("projection.rs");
        let registry_source = include_str!("../../../source_backed.rs");
        for source in [provider_source, projection_source] {
            for forbidden in [
                ["ctx_history_", "store"].concat(),
                ["Store", "::"].concat(),
                ["import_junie_", "nativepath("].concat(),
                ["provider_bytes: ", "document.body"].concat(),
                ["provider_local_preview(&", "row.text"].concat(),
            ] {
                assert!(
                    !source.contains(&forbidden),
                    "Junie source-backed route contains forbidden architecture token {forbidden:?}"
                );
            }
        }
        assert!(provider_source.contains("exact_junie_lexical_body"));
        assert!(provider_source.contains("fn hydrate_batch("));
        assert!(provider_source.contains("self.hydrate_requests(request.events())"));
        assert!(!nativepath_source.contains("mod output;"));
        assert!(!nativepath_source.contains("mod core;"));
        assert!(!nativepath_source.contains("mod cursor;"));
        assert!(!nativepath_source.contains("mod lifecycle;"));
        assert!(!nativepath_source.contains("mod publication;"));
        let legacy_store_type = ["ctx_history_", "store::Store"].concat();
        assert_eq!(
            nativepath_source.matches(&legacy_store_type).count(),
            1,
            "Junie compatibility Store reference must remain a single typed-unsupported shim"
        );
        assert!(nativepath_source.contains("CaptureError::UnsupportedSchema"));
        assert!(registry_source.contains("JunieLocatorResolverV0::discover_for_hydration"));
        assert!(registry_source.contains(".with_batch_hydration(move |request|"));
    }

    #[test]
    fn junie_source_backed_ordinary_record_exact_show_fixture() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let prompt = "ordinary exact Junie prompt ☃\nsecond line";
        write_tree(
            temp.path(),
            "ordinary-session",
            &[serde_json::json!({
                "kind": "UserPromptEvent",
                "prompt": prompt,
            })],
        );
        let documents = scan_documents(temp.path());
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].parent_session_id, None);
        assert_eq!(documents[0].root_session_id, documents[0].session_id);
        assert_eq!(
            documents[0].provider_session_id.as_deref(),
            Some("ordinary-session")
        );
        assert_eq!(documents[0].branch, None);
        assert!(documents[0]
            .source_path
            .as_deref()
            .is_some_and(|path| path.ends_with("/ordinary-session/events.jsonl")));
        assert_eq!(documents[0].agent_type, AgentType::Primary.as_str());
        assert!(documents[0].is_primary);
        assert_eq!(documents[0].workspace.as_deref(), Some("/workspace/junie"));
        assert_eq!(documents[0].cwd.as_deref(), Some("/workspace/junie"));
        assert!(matches!(
            documents[0].locator.coordinate(),
            NativeRecordCoordinate::Jsonl { .. }
        ));
        let hydrated = hydrate(temp.path(), &documents[0]).unwrap();
        assert_eq!(hydrated.provider_bytes, prompt.as_bytes());
    }

    #[test]
    fn junie_source_backed_record_set_exact_show_fixture() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let records = [
            serde_json::json!({
                "kind": "SessionA2uxEvent",
                "timestampMs": 1_783_339_200_001_i64,
                "event": {"agentEvent": {
                    "kind": "ResultBlockUpdatedEvent",
                    "stepId": "a",
                    "result": "first exact assistant block",
                }},
            }),
            serde_json::json!({
                "kind": "SessionA2uxEvent",
                "timestampMs": 1_783_339_200_002_i64,
                "event": {"agentEvent": {
                    "kind": "ResultBlockUpdatedEvent",
                    "stepId": "b",
                    "result": "second exact assistant block",
                }},
            }),
        ];
        write_tree(temp.path(), "record-set-session", &records);
        let documents = scan_documents(temp.path());
        assert_eq!(documents.len(), 1);
        assert!(matches!(
            documents[0].locator.coordinate(),
            NativeRecordCoordinate::TreeRecord { .. }
        ));
        let hydrated = hydrate(temp.path(), &documents[0]).unwrap();
        assert_eq!(
            hydrated.provider_bytes,
            b"first exact assistant block\n\nsecond exact assistant block"
        );
    }

    #[test]
    fn junie_source_backed_over_limit_turn_stays_indexed_and_typed_fails_show() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let records: Vec<_> = (0..=MAX_RECORD_SET_ENTRIES)
            .map(|index| {
                serde_json::json!({
                    "kind": "SessionA2uxEvent",
                    "timestampMs": 1_783_339_200_000_i64 + index as i64,
                    "event": {"agentEvent": {
                        "kind": "ResultBlockUpdatedEvent",
                        "stepId": format!("{index:03}"),
                        "result": format!("bounded searchable part {index}"),
                    }},
                })
            })
            .collect();
        write_tree(temp.path(), "over-limit-session", &records);
        let documents = scan_documents(temp.path());
        assert_eq!(documents.len(), 1);
        assert!(!documents[0].body.is_empty());
        assert!(matches!(
            documents[0].locator.coordinate(),
            NativeRecordCoordinate::ProviderNative { namespace, .. }
                if namespace == UNAVAILABLE_COORDINATE_NAMESPACE
        ));
        let failure = hydrate(temp.path(), &documents[0]).unwrap_err();
        assert_eq!(
            failure.kind,
            HydrationFailureKind::UnsupportedParserRevision
        );
        assert!(failure.detail.contains("at most 64 source records"));
    }
}
