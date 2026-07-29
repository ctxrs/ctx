//! Provider-local source-backed projection for supported Mux session trees.
//!
//! A Mux session directory is one logical source. Its append-oriented
//! `chat.jsonl`, mutable `partial.json`, and optional metadata are certified as
//! one compound snapshot so chat and partial events retain one stable session
//! identity. The provider-neutral lifecycle/registry owner can consume these
//! bounded pages without learning Mux file-layout or locator details.

use std::{
    collections::HashMap,
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, BatchHydrationRequest, BatchHydrationResult,
    CaptureProvider, CertifiedSource, CertifiedSourceAppend, ContentSourceResolver,
    EventHydrationRequest, EventIdentityInput, EventType, HydratedProviderRecord, HydrationFailure,
    HydrationFailureKind, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, ProjectionContractError, ScannedSourceCounts, SessionHydrationRequest,
    SessionIdentityInput, SourceAnchor, SourceFrontier, SourceKey, SourceObservation,
    SourceRecordLocator, SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    model::{
        MuxFrontier, MuxPreparedRow, MuxSourcePlan, MuxStreamKind, MuxUnaddressableOutput,
        MUX_MAX_ORDINAL, MUX_PARTIAL_NATIVE_ORDINAL,
    },
    mux_event_id, mux_event_text, mux_event_type, mux_output_projection, mux_partial_event_index,
    mux_result_content,
    parse::read_core_page,
    MuxOutputOutcome,
};
use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    complete_content::CompleteContentBodyDigest,
    provider::normalization::provider_value_text,
    CaptureError, ProviderAdapterContext, MAX_PROVIDER_JSONL_LINE_BYTES, MUX_SOURCE_FORMAT,
};

use crate::provider::providers::mux::{
    metadata::{mux_bounded_session_metadata_from_bytes, MuxBoundedSessionMetadata},
    source::{MuxFileObservation, MuxSessionSource},
};

use super::source::discover_sessions;

const MUX_SOURCE_ANCHOR_NAMESPACE: &str = "mux.session";
const MUX_NATIVE_SESSION_NAMESPACE: &str = "mux.session";
const MUX_NATIVE_ITEM_NAMESPACE: &str = "mux.record";
const MUX_PROVIDER_NATIVE_LOCATOR_NAMESPACE: &str = "mux.logical-record.v2";
const MUX_LOGICAL_SESSION_KIND: &str = "mux-session";
const MUX_LOGICAL_EVENT_KIND: &str = "mux-event";
const MUX_SOURCE_SCHEMA_VARIANT: &str = "mux-session-tree-source-backed-v2";
const MUX_SOURCE_REVISION_KIND: &str = "mux-session-compound-observation-v2";
const MUX_FRONTIER_KIND: &str = "mux-session-compound-frontier-v2";
const MUX_PARSER_REVISION: &str = "mux-source-backed-v2";
const MUX_CHECKPOINT_VERSION: u32 = 2;

#[derive(Debug, Error)]
pub(crate) enum MuxSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Mux source-backed certificate has no compound checkpoint")]
    MissingCheckpoint,
    #[error("Mux source-backed checkpoint is malformed or incompatible")]
    InvalidCheckpoint,
    #[error("Mux source-backed candidate changed after discovery")]
    CandidateChanged,
    #[error("Mux source-backed row changed its native session owner")]
    OwnerChanged,
    #[error("Mux source-backed scan count overflow")]
    CountOverflow,
    #[error("Mux source-backed record digest is malformed")]
    InvalidRecordDigest,
    #[error("Mux source-backed locator is malformed or belongs to another source")]
    InvalidLocator,
    #[error("Mux source-backed locator evidence is stale")]
    StaleLocator,
    #[error("Mux native session {0:?} resolves to more than one source")]
    DuplicateNativeSession(String),
    #[error("Mux source-backed exact lexical projection failed: {0}")]
    ExactLexicalProjection(String),
}

pub(crate) type MuxSourceBackedResult<T> = Result<T, MuxSourceBackedError>;

/// One discovered, supported Mux session source.
///
/// `chat-archive.jsonl` is intentionally absent from this contract.
#[derive(Debug, Clone)]
pub(crate) struct MuxSourceBackedCandidate {
    configured_root: PathBuf,
    authority: ProviderSourceRoot,
    source: MuxSessionSource,
    session_relative_path: PathBuf,
    chat_relative_path: Option<PathBuf>,
    partial_relative_path: Option<PathBuf>,
    metadata_relative_path: Option<PathBuf>,
    metadata: MuxBoundedSessionMetadata,
    source_key: SourceKey,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    observed_at: DateTime<Utc>,
}

impl MuxSourceBackedCandidate {
    pub(crate) fn source_key(&self) -> &SourceKey {
        &self.source_key
    }

    pub(crate) fn session_id(&self) -> StableEntityId {
        self.session_id
    }

    pub(crate) fn parent_session_id(&self) -> Option<StableEntityId> {
        self.parent_session_id
    }

    pub(crate) fn root_session_id(&self) -> StableEntityId {
        self.root_session_id
    }

    pub(crate) fn provider_session_id(&self) -> &str {
        &self.metadata.provider_session_id
    }

    pub(crate) fn parent_provider_session_id(&self) -> Option<&str> {
        self.metadata.parent_provider_session_id.as_deref()
    }

    pub(crate) fn root_provider_session_id(&self) -> Option<&str> {
        self.metadata.root_provider_session_id.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MuxReplacementReason {
    ParserRevisionChanged,
    SourceSetChanged,
    MetadataChanged,
    ChatTruncated,
    ChatReplaced,
    ChatPrefixChanged,
    PartialSnapshotChanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MuxReplacementEvidence {
    pub(crate) reason: MuxReplacementReason,
    pub(crate) prior_content_digest: [u8; 32],
    pub(crate) replacement_content_digest: [u8; 32],
}

#[derive(Debug, Clone)]
pub(crate) enum MuxSourceBackedDisposition {
    Cold,
    Unchanged,
    Append { proof: CertifiedSourceAppend },
    Replacement { evidence: MuxReplacementEvidence },
}

#[derive(Debug, Clone)]
pub(crate) struct MuxSourceBackedScanReceipt {
    pub(crate) certificate: CertifiedSource,
    pub(crate) disposition: MuxSourceBackedDisposition,
    pub(crate) emitted_documents: u64,
    pub(crate) emitted_unaddressable: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct MuxSourceBackedPage {
    pub(crate) source: SourceKey,
    pub(crate) session_id: StableEntityId,
    pub(crate) stream_kind: MuxStreamKind,
    pub(crate) records: Vec<MuxSourceBackedRecord>,
    pub(crate) unaddressable: Vec<MuxUnaddressableRecord>,
}

#[derive(Debug, Clone)]
pub(crate) struct MuxSourceBackedRecord {
    pub(crate) document: LexicalDocument,
    pub(crate) stream_kind: MuxStreamKind,
    pub(crate) source_record_ordinal: u64,
    pub(crate) native_record_id: String,
    pub(crate) message_content_ref: Option<ctx_history_core::ContentRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MuxUnaddressableReason {
    RedactedOutput,
    MissingOutput,
}

#[derive(Debug, Clone)]
pub(crate) struct MuxBoundedProjection {
    pub(crate) provider_session_id: String,
    pub(crate) event_sequence: u64,
    pub(crate) occurred_at_unix_ms: Option<i64>,
    pub(crate) event_type: String,
    pub(crate) role: Option<String>,
    pub(crate) body: String,
    pub(crate) cwd: Option<String>,
    pub(crate) touched_files: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct MuxUnaddressableRecord {
    pub(crate) event_id: StableEntityId,
    pub(crate) stream_kind: MuxStreamKind,
    pub(crate) source_record_ordinal: u64,
    pub(crate) native_record_id: String,
    pub(crate) reason: MuxUnaddressableReason,
    pub(crate) bounded_projection: Option<MuxBoundedProjection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MuxLeafObservationWire {
    content_identity: String,
    source_revision: String,
    length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MuxCompoundObservationWire {
    version: u32,
    chat: Option<MuxLeafObservationWire>,
    partial: Option<MuxLeafObservationWire>,
    metadata_revision: String,
}

#[derive(Debug)]
struct MuxObservedSource {
    wire: MuxCompoundObservationWire,
    chat: Option<MuxFileObservation>,
    partial: Option<MuxFileObservation>,
    chat_file: Option<OpenedProviderSourceFile>,
    partial_file: Option<OpenedProviderSourceFile>,
    metadata_file: Option<OpenedProviderSourceFile>,
    metadata_bytes: Option<Vec<u8>>,
}

impl MuxObservedSource {
    fn revalidate(&self, authority: &ProviderSourceRoot) -> MuxSourceBackedResult<()> {
        if let Some(chat) = &self.chat_file {
            chat.revalidate()?;
        }
        if let Some(partial) = &self.partial_file {
            partial.revalidate()?;
        }
        if let Some(metadata) = &self.metadata_file {
            metadata.revalidate()?;
        }
        authority.revalidate()?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MuxComponentDigest {
    bytes: u64,
    digest: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MuxLeafCheckpoint {
    observation: MuxLeafObservationWire,
    frontier: MuxFrontier,
    content: MuxComponentDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MuxSourceBackedCheckpoint {
    version: u32,
    observation: MuxCompoundObservationWire,
    chat: Option<MuxLeafCheckpoint>,
    partial: Option<MuxLeafCheckpoint>,
    metadata: Option<MuxComponentDigest>,
    counts: ScannedSourceCounts,
}

#[derive(Debug, Clone)]
enum MuxScanPlan {
    Cold,
    Append {
        checkpoint: MuxSourceBackedCheckpoint,
    },
    Replacement {
        reason: MuxReplacementReason,
        checkpoint: MuxSourceBackedCheckpoint,
    },
}

#[derive(Debug)]
struct MuxLeafScan {
    checkpoint: MuxLeafCheckpoint,
    complete_records: u64,
    retained_records: u64,
    indexed_documents: u64,
    unaddressable_records: u64,
}

/// Discovers only supported chat/partial session directories.
pub(crate) fn discover_mux_source_backed_sources(
    root: &Path,
    observed_at: DateTime<Utc>,
) -> MuxSourceBackedResult<Vec<MuxSourceBackedCandidate>> {
    let mut sources = discover_sessions(root)?;
    sources.sort_by(|left, right| left.session_dir.cmp(&right.session_dir));
    let selected = std::fs::canonicalize(root)?;
    let authority_path = if std::fs::metadata(root)?.is_file() {
        selected
            .parent()
            .ok_or(CaptureError::InvalidProviderTranscriptPath {
                path: selected.clone(),
                reason: "Mux selected file has no authority directory",
            })?
            .to_path_buf()
    } else {
        selected
    };
    let authority = ProviderSourceRoot::open(&authority_path)?;
    let mut candidates = Vec::with_capacity(sources.len());
    for source in sources {
        let session_relative_path = relative_to_mux_authority(&authority, &source.session_dir)?;
        let chat_relative_path = source
            .chat_path
            .as_deref()
            .map(|path| relative_to_mux_authority(&authority, path))
            .transpose()?;
        let partial_relative_path = source
            .partial_path
            .as_deref()
            .map(|path| relative_to_mux_authority(&authority, path))
            .transpose()?;
        let metadata_relative_path = source
            .metadata_path
            .as_deref()
            .map(|path| relative_to_mux_authority(&authority, path))
            .transpose()?;
        let observation = admit_mux_source(
            &authority,
            &source,
            &session_relative_path,
            chat_relative_path.as_deref(),
            partial_relative_path.as_deref(),
            metadata_relative_path.as_deref(),
        )?;
        let metadata = mux_bounded_session_metadata_from_bytes(
            &source,
            &observation.wire.metadata_revision,
            observed_at,
            observation.metadata_bytes.as_deref(),
        )?;
        let source_key = mux_source_key(&metadata.provider_session_id)?;
        let session_id = mux_session_identity(&source_key, &metadata.provider_session_id)?;
        let parent_session_id = metadata
            .parent_provider_session_id
            .as_deref()
            .map(mux_related_session_identity)
            .transpose()?;
        let root_session_id = metadata
            .root_provider_session_id
            .as_deref()
            .map(mux_related_session_identity)
            .transpose()?
            .or(parent_session_id)
            .unwrap_or(session_id);
        candidates.push(MuxSourceBackedCandidate {
            configured_root: root.to_path_buf(),
            authority: authority.clone(),
            source,
            session_relative_path,
            chat_relative_path,
            partial_relative_path,
            metadata_relative_path,
            metadata,
            source_key,
            session_id,
            parent_session_id,
            root_session_id,
            observed_at,
        });
    }
    Ok(candidates)
}

/// Streams bounded parser pages and returns terminal source certification.
///
/// The callback may stage pages, but publication must wait for the returned
/// certificate and its integration-layer commit-time revalidation.
pub(crate) fn scan_mux_source_backed(
    candidate: &MuxSourceBackedCandidate,
    base: Option<&CertifiedSource>,
    mut emit: impl FnMut(MuxSourceBackedPage) -> MuxSourceBackedResult<()>,
) -> MuxSourceBackedResult<MuxSourceBackedScanReceipt> {
    let opening = admit_mux_candidate(candidate)?;
    if opening.wire.metadata_revision != candidate.metadata.metadata_revision {
        return Err(MuxSourceBackedError::CandidateChanged);
    }
    let opening_observation = source_observation(&candidate.source_key, &opening.wire)?;
    let source_revision_digest = Sha256::digest(opening_observation.revision()).into();

    if let Some(base) = base {
        candidate
            .source_key
            .validate_exact_descriptor(base.observation().source())?;
        let checkpoint = decode_checkpoint(base)?;
        if base.parser_revision() == MUX_PARSER_REVISION && checkpoint.observation == opening.wire {
            opening.revalidate(&candidate.authority)?;
            let closing = admit_mux_candidate(candidate)?;
            closing.revalidate(&candidate.authority)?;
            if closing.wire != opening.wire
                || base.observation().revision() != opening_observation.revision()
            {
                return Err(MuxSourceBackedError::CandidateChanged);
            }
            return Ok(MuxSourceBackedScanReceipt {
                certificate: base.clone(),
                disposition: MuxSourceBackedDisposition::Unchanged,
                emitted_documents: 0,
                emitted_unaddressable: 0,
            });
        }
    }

    let plan = classify_scan(base, &opening)?;
    let mut emitted_documents = 0_u64;
    let mut emitted_unaddressable = 0_u64;
    let mut session = candidate.metadata.clone();
    let context = ProviderAdapterContext {
        machine_id: "mux-source-backed".to_owned(),
        source_path: Some(candidate.configured_root.clone()),
        source_root: Some(candidate.configured_root.clone()),
        imported_at: candidate.observed_at,
    };

    let (chat_start, scan_partial, prior_checkpoint) = match &plan {
        MuxScanPlan::Cold => (MuxFrontier::initial(), true, None),
        MuxScanPlan::Append { checkpoint } => {
            session.metadata_failure = None;
            (
                checkpoint
                    .chat
                    .as_ref()
                    .map(|chat| chat.frontier.clone())
                    .ok_or(MuxSourceBackedError::InvalidCheckpoint)?,
                false,
                Some(checkpoint),
            )
        }
        MuxScanPlan::Replacement { checkpoint, .. } => {
            (MuxFrontier::initial(), true, Some(checkpoint))
        }
    };

    let chat_scan = match (
        candidate.source.chat_path.as_ref(),
        opening.chat.as_ref(),
        opening.chat_file.as_ref(),
        prior_checkpoint,
    ) {
        (Some(path), Some(observation), Some(file), _) => Some(scan_leaf(
            candidate,
            &context,
            &mut session,
            path,
            file,
            MuxStreamKind::Chat,
            observation,
            chat_start,
            source_revision_digest,
            &mut emitted_documents,
            &mut emitted_unaddressable,
            &mut emit,
        )?),
        (None, None, None, Some(checkpoint)) if !scan_partial => {
            checkpoint.chat.as_ref().map(|chat| MuxLeafScan {
                checkpoint: chat.clone(),
                complete_records: 0,
                retained_records: 0,
                indexed_documents: 0,
                unaddressable_records: 0,
            })
        }
        (None, None, None, _) => None,
        _ => return Err(MuxSourceBackedError::CandidateChanged),
    };

    let partial_scan = if scan_partial {
        match (
            candidate.source.partial_path.as_ref(),
            opening.partial.as_ref(),
            opening.partial_file.as_ref(),
        ) {
            (Some(path), Some(observation), Some(file)) => Some(scan_leaf(
                candidate,
                &context,
                &mut session,
                path,
                file,
                MuxStreamKind::Partial,
                observation,
                MuxFrontier::initial(),
                source_revision_digest,
                &mut emitted_documents,
                &mut emitted_unaddressable,
                &mut emit,
            )?),
            (None, None, None) => None,
            _ => return Err(MuxSourceBackedError::CandidateChanged),
        }
    } else {
        prior_checkpoint
            .and_then(|checkpoint| checkpoint.partial.as_ref())
            .map(|partial| MuxLeafScan {
                checkpoint: partial.clone(),
                complete_records: 0,
                retained_records: 0,
                indexed_documents: 0,
                unaddressable_records: 0,
            })
    };

    let metadata = if matches!(plan, MuxScanPlan::Append { .. }) {
        prior_checkpoint.and_then(|checkpoint| checkpoint.metadata.clone())
    } else {
        digest_optional_file(opening.metadata_bytes.as_deref())?
    };
    opening.revalidate(&candidate.authority)?;
    let closing = admit_mux_candidate(candidate)?;
    closing.revalidate(&candidate.authority)?;
    if closing.wire != opening.wire {
        return Err(MuxSourceBackedError::CandidateChanged);
    }

    let counts = scan_counts(
        base,
        &plan,
        chat_scan.as_ref(),
        partial_scan.as_ref(),
        metadata.as_ref(),
    )?;
    let checkpoint = MuxSourceBackedCheckpoint {
        version: MUX_CHECKPOINT_VERSION,
        observation: closing.wire.clone(),
        chat: chat_scan.map(|scan| scan.checkpoint),
        partial: partial_scan.map(|scan| scan.checkpoint),
        metadata,
        counts,
    };
    let content_digest = compound_content_digest(&checkpoint);
    let certified_bytes = counts.certified_bytes;
    let frontier = SourceFrontier::new(
        MUX_FRONTIER_KIND,
        TypedKey::bytes(serde_json::to_vec(&checkpoint)?)?,
        certified_bytes,
        content_digest,
    )?;
    let closing_observation = source_observation(&candidate.source_key, &closing.wire)?;
    let certificate = CertifiedSource::certify_with_frontier(
        opening_observation,
        closing_observation,
        MUX_PARSER_REVISION,
        content_digest,
        counts,
        Some(frontier),
    )?;

    let disposition = match plan {
        MuxScanPlan::Cold => MuxSourceBackedDisposition::Cold,
        MuxScanPlan::Append { .. } => {
            let base = base.ok_or(MuxSourceBackedError::InvalidCheckpoint)?;
            let base_frontier = base
                .frontier()
                .ok_or(MuxSourceBackedError::MissingCheckpoint)?;
            let proof = CertifiedSourceAppend::certify(
                base,
                certificate.clone(),
                base_frontier.certified_prefix_bytes(),
                *base_frontier.certified_prefix_digest(),
            )?;
            MuxSourceBackedDisposition::Append { proof }
        }
        MuxScanPlan::Replacement { reason, .. } => {
            let base = base.ok_or(MuxSourceBackedError::InvalidCheckpoint)?;
            MuxSourceBackedDisposition::Replacement {
                evidence: MuxReplacementEvidence {
                    reason,
                    prior_content_digest: *base.content_digest(),
                    replacement_content_digest: content_digest,
                },
            }
        }
    };
    Ok(MuxSourceBackedScanReceipt {
        certificate,
        disposition,
        emitted_documents,
        emitted_unaddressable,
    })
}

pub(crate) fn revalidate_mux_source_backed(
    candidate: &MuxSourceBackedCandidate,
    certificate: &CertifiedSource,
) -> MuxSourceBackedResult<bool> {
    if !candidate
        .source_key
        .exact_descriptor_eq(certificate.observation().source())
    {
        return Ok(false);
    }
    let checkpoint = decode_checkpoint(certificate)?;
    let observed = admit_mux_candidate(candidate)?;
    observed.revalidate(&candidate.authority)?;
    let observation = source_observation(&candidate.source_key, &observed.wire)?;
    Ok(checkpoint.observation == observed.wire
        && certificate.observation().revision() == observation.revision())
}

#[derive(Debug)]
pub(crate) struct MuxSourceBackedResolverV0 {
    sources: HashMap<StableEntityId, MuxSourceBackedCandidate>,
}

#[derive(Debug)]
struct MuxLogicalRecordCoordinate {
    stream_kind: MuxStreamKind,
    byte_start: u64,
    byte_end_exclusive: u64,
    source_record_ordinal: u64,
    event_sequence: u64,
    native_record_id: String,
}

impl MuxSourceBackedResolverV0 {
    pub(crate) fn discover(root: &Path, observed_at: DateTime<Utc>) -> MuxSourceBackedResult<Self> {
        let mut sources = HashMap::new();
        for candidate in discover_mux_source_backed_sources(root, observed_at)? {
            let provider_session_id = candidate.provider_session_id().to_owned();
            if sources
                .insert(candidate.source_key.identity(), candidate)
                .is_some()
            {
                return Err(MuxSourceBackedError::DuplicateNativeSession(
                    provider_session_id,
                ));
            }
        }
        Ok(Self { sources })
    }

    pub(crate) fn discover_for_hydration(
        root: &Path,
        observed_at: DateTime<Utc>,
    ) -> Result<Self, HydrationFailure> {
        Self::discover(root, observed_at)
            .map_err(|error| hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error))
    }

    pub(crate) fn hydrate_requests(
        &self,
        requests: &[EventHydrationRequest],
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        let Some(first) = requests.first() else {
            return Ok(Vec::new());
        };
        let candidate = self
            .sources
            .get(&first.locator().source().identity())
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::ConfirmedDeleted,
                    "the exact Mux session is absent from the complete source inventory",
                )
            })?;
        for request in requests {
            validate_mux_locator(candidate, request.locator())?;
        }
        let opening = admit_mux_candidate(candidate).map_err(|error| {
            hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
        })?;
        let current_observation = source_observation(&candidate.source_key, &opening.wire)
            .map_err(|error| {
                hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
            })?;
        let current_revision_digest: [u8; 32] =
            Sha256::digest(current_observation.revision()).into();
        let hydrated = requests
            .iter()
            .map(|request| {
                hydrate_mux_request(candidate, &opening, current_revision_digest, request)
            })
            .collect::<Result<Vec<_>, _>>()?;
        opening
            .revalidate(&candidate.authority)
            .map_err(|error| hydration_failure(HydrationFailureKind::StaleSourceEvidence, error))?;
        let closing = admit_mux_candidate(candidate).map_err(|error| {
            hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
        })?;
        closing
            .revalidate(&candidate.authority)
            .map_err(|error| hydration_failure(HydrationFailureKind::StaleSourceEvidence, error))?;
        if closing.wire != opening.wire {
            return Err(hydration_failure(
                HydrationFailureKind::StaleSourceEvidence,
                "Mux compound source changed during exact hydration",
            ));
        }
        Ok(hydrated)
    }
}

impl ContentSourceResolver for MuxSourceBackedResolverV0 {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        let mut hydrated = self.hydrate_requests(std::slice::from_ref(request))?;
        hydrated.pop().ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::MissingRecord,
                "Mux exact event hydration returned no record",
            )
        })
    }

    fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let result = BatchHydrationResult::new(self.hydrate_requests(request.events())?).map_err(
            |error| {
                hydration_failure(
                    HydrationFailureKind::InvalidLocator,
                    format!("invalid Mux batch hydration result: {error}"),
                )
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

fn validate_mux_locator(
    candidate: &MuxSourceBackedCandidate,
    locator: &SourceRecordLocator,
) -> Result<(), HydrationFailure> {
    locator
        .validate_contract()
        .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?;
    if locator.source().provider() != CaptureProvider::Mux.as_str()
        || locator.source().source_format() != MUX_SOURCE_FORMAT
        || locator.source().schema_variant() != MUX_SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
        || locator.certified_source_revision_digest().is_none()
        || !candidate.source_key.exact_descriptor_eq(locator.source())
    {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator source descriptor is invalid",
        ));
    }
    let coordinate = decode_mux_coordinate(locator)?;
    let expected_policy = if coordinate.stream_kind.is_partial() {
        LocatorRevisionPolicy::ExactSourceRevision
    } else {
        LocatorRevisionPolicy::StableRecordEvidence
    };
    if locator.revision_policy() != expected_policy {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator revision policy does not match its native stream",
        ));
    }
    Ok(())
}

fn hydrate_mux_request(
    candidate: &MuxSourceBackedCandidate,
    opening: &MuxObservedSource,
    current_revision_digest: [u8; 32],
    request: &EventHydrationRequest,
) -> Result<HydratedProviderRecord, HydrationFailure> {
    let coordinate = decode_mux_coordinate(request.locator())?;
    if coordinate.stream_kind.is_partial()
        && request.locator().certified_source_revision_digest() != Some(&current_revision_digest)
    {
        return Err(hydration_failure(
            HydrationFailureKind::StaleSourceEvidence,
            "Mux partial snapshot revision changed",
        ));
    }
    let source = match coordinate.stream_kind {
        MuxStreamKind::Chat => opening.chat_file.as_ref(),
        MuxStreamKind::Partial => opening.partial_file.as_ref(),
    }
    .ok_or_else(|| {
        hydration_failure(
            HydrationFailureKind::MissingRecord,
            "Mux locator stream is absent from the current session source",
        )
    })?;
    let payload = read_mux_payload(source, &coordinate)?;
    if Sha256::digest(&payload).as_slice() != request.locator().record_digest() {
        return Err(hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "Mux source record digest changed",
        ));
    }
    let value = serde_json::from_slice::<serde_json::Value>(&payload).map_err(|error| {
        hydration_failure(HydrationFailureKind::UnsupportedParserRevision, error)
    })?;
    if !value.is_object() {
        return Err(hydration_failure(
            HydrationFailureKind::UnsupportedParserRevision,
            "Mux native record is not an object",
        ));
    }
    validate_mux_native_identity(candidate, request, &coordinate, &payload, &value)?;
    let provider_bytes = mux_exact_logical_content(&value)?;
    Ok(HydratedProviderRecord {
        event_id: request.event_id(),
        provider_bytes: provider_bytes.into_bytes(),
    })
}

fn read_mux_payload(
    source: &OpenedProviderSourceFile,
    coordinate: &MuxLogicalRecordCoordinate,
) -> Result<Vec<u8>, HydrationFailure> {
    let byte_length = coordinate
        .byte_end_exclusive
        .checked_sub(coordinate.byte_start)
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Mux locator byte range moved backwards",
            )
        })?;
    if byte_length == 0 || byte_length > MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2) as u64 {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator byte range exceeds the native record bound",
        ));
    }
    if coordinate.byte_end_exclusive > source.len() {
        return Err(hydration_failure(
            HydrationFailureKind::MissingRecord,
            "Mux locator byte range is no longer present",
        ));
    }
    if coordinate.stream_kind == MuxStreamKind::Chat && coordinate.byte_start > 0 {
        let boundary = source
            .read_exact_range(coordinate.byte_start - 1, 1, 1)
            .map_err(|error| {
                hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
            })?;
        if boundary != b"\n" {
            return Err(hydration_failure(
                HydrationFailureKind::StaleRecordEvidence,
                "Mux chat record start boundary changed",
            ));
        }
    }
    let length = usize::try_from(byte_length).map_err(|_| {
        hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator byte range exceeds platform limits",
        )
    })?;
    let provider_bytes = source
        .read_exact_range(
            coordinate.byte_start,
            length,
            MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2),
        )
        .map_err(|error| hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error))?;
    if coordinate.stream_kind == MuxStreamKind::Partial {
        if coordinate.byte_start != 0
            || coordinate.byte_end_exclusive != source.len()
            || coordinate.source_record_ordinal != 0
        {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Mux partial locator does not address its whole snapshot",
            ));
        }
        return Ok(provider_bytes);
    }
    let first_newline = provider_bytes.iter().position(|byte| *byte == b'\n');
    if first_newline.is_some_and(|position| position + 1 != provider_bytes.len())
        || (first_newline.is_none() && coordinate.byte_end_exclusive != source.len())
    {
        return Err(hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "Mux chat record end boundary changed",
        ));
    }
    Ok(strip_jsonl_record_ending(&provider_bytes).to_vec())
}

fn strip_jsonl_record_ending(record: &[u8]) -> &[u8] {
    record
        .strip_suffix(b"\n")
        .unwrap_or(record)
        .strip_suffix(b"\r")
        .unwrap_or_else(|| record.strip_suffix(b"\n").unwrap_or(record))
}

fn encode_mux_coordinate(
    stream_kind: MuxStreamKind,
    legacy_locator: &[u8],
    source_record_ordinal: u64,
    event_sequence: u64,
    native_record_id: &str,
) -> MuxSourceBackedResult<TypedKey> {
    let (tag, byte_start, byte_end_exclusive) =
        decode_mux_legacy_range(legacy_locator).ok_or(MuxSourceBackedError::InvalidLocator)?;
    let expected_tag = if stream_kind.is_partial() { 2 } else { 1 };
    if tag != expected_tag {
        return Err(MuxSourceBackedError::InvalidLocator);
    }
    Ok(TypedKey::composite(vec![
        TypedKey::U64(2),
        TypedKey::U64(tag),
        TypedKey::U64(byte_start),
        TypedKey::U64(byte_end_exclusive),
        TypedKey::U64(source_record_ordinal),
        TypedKey::U64(event_sequence),
        TypedKey::utf8(native_record_id)?,
    ])?)
}

fn decode_mux_legacy_range(value: &[u8]) -> Option<(u64, u64, u64)> {
    if value.len() != 17 {
        return None;
    }
    let tag = u64::from(value[0]);
    let byte_start = u64::from_be_bytes(value[1..9].try_into().ok()?);
    let byte_end_exclusive = u64::from_be_bytes(value[9..17].try_into().ok()?);
    if !matches!(tag, 1 | 2) || byte_start >= byte_end_exclusive || (tag == 2 && byte_start != 0) {
        return None;
    }
    Some((tag, byte_start, byte_end_exclusive))
}

fn decode_mux_coordinate(
    locator: &SourceRecordLocator,
) -> Result<MuxLogicalRecordCoordinate, HydrationFailure> {
    let NativeRecordCoordinate::ProviderNative {
        namespace,
        coordinate,
    } = locator.coordinate()
    else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator does not use a provider-native coordinate",
        ));
    };
    if namespace != MUX_PROVIDER_NATIVE_LOCATOR_NAMESPACE {
        return Err(hydration_failure(
            if namespace.starts_with("mux.") {
                HydrationFailureKind::UnsupportedParserRevision
            } else {
                HydrationFailureKind::InvalidLocator
            },
            "Mux locator namespace is unsupported",
        ));
    }
    let TypedKey::Composite(parts) = coordinate else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator coordinate is malformed",
        ));
    };
    let [TypedKey::U64(version), TypedKey::U64(tag), TypedKey::U64(byte_start), TypedKey::U64(byte_end_exclusive), TypedKey::U64(source_record_ordinal), TypedKey::U64(event_sequence), TypedKey::Utf8(native_record_id)] =
        parts.as_slice()
    else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator coordinate is malformed",
        ));
    };
    if *version != 2 {
        return Err(hydration_failure(
            HydrationFailureKind::UnsupportedParserRevision,
            "Mux locator parser revision is unsupported",
        ));
    }
    let stream_kind = match *tag {
        1 => MuxStreamKind::Chat,
        2 => MuxStreamKind::Partial,
        _ => {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Mux locator stream tag is invalid",
            ))
        }
    };
    if byte_start >= byte_end_exclusive
        || native_record_id.is_empty()
        || (stream_kind.is_partial() && (*byte_start != 0 || *source_record_ordinal != 0))
        || (!stream_kind.is_partial() && event_sequence != source_record_ordinal)
    {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Mux locator coordinate is internally inconsistent",
        ));
    }
    Ok(MuxLogicalRecordCoordinate {
        stream_kind,
        byte_start: *byte_start,
        byte_end_exclusive: *byte_end_exclusive,
        source_record_ordinal: *source_record_ordinal,
        event_sequence: *event_sequence,
        native_record_id: native_record_id.clone(),
    })
}

fn validate_mux_native_identity(
    candidate: &MuxSourceBackedCandidate,
    request: &EventHydrationRequest,
    coordinate: &MuxLogicalRecordCoordinate,
    payload: &[u8],
    value: &serde_json::Value,
) -> Result<(), HydrationFailure> {
    let line_number = usize::try_from(coordinate.source_record_ordinal)
        .ok()
        .and_then(|ordinal| ordinal.checked_add(1))
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Mux native ordinal exceeds platform limits",
            )
        })?;
    let role = value
        .get("role")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let native_record_id = mux_event_id(
        value,
        line_number,
        role,
        coordinate.stream_kind.is_partial(),
    );
    if native_record_id != coordinate.native_record_id {
        return Err(hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "Mux native record identity changed",
        ));
    }
    let expected_sequence = if coordinate.stream_kind.is_partial() {
        MUX_PARTIAL_NATIVE_ORDINAL | (mux_partial_event_index(payload) & MUX_MAX_ORDINAL)
    } else {
        coordinate.source_record_ordinal
    };
    if expected_sequence != coordinate.event_sequence {
        return Err(hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "Mux native event sequence changed",
        ));
    }
    let native_item_key = NativeItemKey::native_id(
        MUX_NATIVE_ITEM_NAMESPACE,
        TypedKey::utf8(&native_record_id).map_err(|error| {
            hydration_failure(HydrationFailureKind::UnsupportedParserRevision, error)
        })?,
    )
    .map_err(|error| hydration_failure(HydrationFailureKind::UnsupportedParserRevision, error))?;
    let expected_event_id = derive_event_id(EventIdentityInput {
        source: &candidate.source_key,
        session_id: candidate.session_id,
        logical_item_kind: MUX_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?;
    if expected_event_id != request.event_id() {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Mux event identity does not match its native coordinate",
        ));
    }
    if let Some(output) = mux_output_projection(value) {
        if !output.body_available {
            return Err(hydration_failure(
                HydrationFailureKind::MissingRecord,
                "Mux native output body is unavailable",
            ));
        }
        if !matches!(
            output.outcome,
            MuxOutputOutcome::Failure | MuxOutputOutcome::Timeout
        ) {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Mux successful output is not an indexed Core event",
            ));
        }
    }
    Ok(())
}

fn mux_exact_logical_content(value: &serde_json::Value) -> Result<String, HydrationFailure> {
    let event_type = mux_event_type(value);
    if matches!(event_type, EventType::ToolOutput | EventType::CommandOutput) {
        return mux_result_content(value).ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::MissingRecord,
                "Mux exact output body is unavailable",
            )
        });
    }
    let mut rendered = Vec::new();
    if let Some(parts) = value.get("parts").and_then(serde_json::Value::as_array) {
        for part in parts {
            match part.get("type").and_then(serde_json::Value::as_str) {
                Some("text" | "reasoning") => {
                    if let Some(text) = part.get("text").and_then(serde_json::Value::as_str) {
                        rendered.push(text.to_owned());
                    }
                }
                Some("dynamic-tool") => rendered.push(mux_exact_tool_part_text(part)),
                Some("file") => {
                    if let Some(label) = mux_exact_file_part_text(part) {
                        rendered.push(label);
                    }
                }
                _ => {
                    if let Some(text) = part.get("text").and_then(serde_json::Value::as_str) {
                        rendered.push(text.to_owned());
                    }
                }
            }
        }
    }
    if !rendered.is_empty() {
        return Ok(rendered.join("\n"));
    }
    if let Some(text) = value
        .get("content")
        .or_else(|| value.get("message"))
        .and_then(provider_value_text)
    {
        return Ok(text);
    }
    Ok(mux_event_text(value, event_type))
}

fn mux_exact_tool_part_text(part: &serde_json::Value) -> String {
    let name = part
        .get("toolName")
        .or_else(|| part.get("name"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("tool");
    let state = part.get("state").and_then(serde_json::Value::as_str);
    let prefix = if matches!(state, Some("output-available" | "output-redacted"))
        || part.get("output").is_some()
    {
        "tool output"
    } else {
        "tool call"
    };
    let mut text = format!("{prefix}: {name}");
    if let Some(input) = part.get("input") {
        text.push_str("\ninput: ");
        text.push_str(&mux_exact_value_text(input));
    }
    if let Some(output) = part.get("output") {
        text.push_str("\noutput: ");
        text.push_str(&mux_exact_value_text(output));
    }
    if let Some(nested) = part
        .get("nestedCalls")
        .and_then(serde_json::Value::as_array)
    {
        let names = nested
            .iter()
            .filter_map(|call| {
                call.get("toolName")
                    .or_else(|| call.get("name"))
                    .and_then(serde_json::Value::as_str)
            })
            .collect::<Vec<_>>();
        if !names.is_empty() {
            text.push_str("\nnested tools: ");
            text.push_str(&names.join(", "));
        }
    }
    text
}

fn mux_exact_value_text(value: &serde_json::Value) -> String {
    provider_value_text(value)
        .or_else(|| serde_json::to_string(value).ok())
        .unwrap_or_else(|| value.to_string())
}

fn mux_exact_file_part_text(part: &serde_json::Value) -> Option<String> {
    let label = part
        .get("filename")
        .or_else(|| part.get("name"))
        .or_else(|| part.get("mediaType"))
        .or_else(|| part.get("mimeType"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            part.get("url")
                .and_then(serde_json::Value::as_str)
                .filter(|url| !url.starts_with("data:") && url.len() < 256)
                .map(str::to_owned)
        })?;
    Some(format!("file: {label}"))
}

fn hydration_failure(
    kind: HydrationFailureKind,
    detail: impl std::fmt::Display,
) -> HydrationFailure {
    HydrationFailure {
        kind,
        detail: detail.to_string(),
    }
}

fn classify_scan(
    base: Option<&CertifiedSource>,
    opening: &MuxObservedSource,
) -> MuxSourceBackedResult<MuxScanPlan> {
    let Some(base) = base else {
        return Ok(MuxScanPlan::Cold);
    };
    let checkpoint = decode_checkpoint(base)?;
    if base.parser_revision() != MUX_PARSER_REVISION {
        return Ok(MuxScanPlan::Replacement {
            reason: MuxReplacementReason::ParserRevisionChanged,
            checkpoint,
        });
    }
    if checkpoint.observation.metadata_revision != opening.wire.metadata_revision {
        return Ok(MuxScanPlan::Replacement {
            reason: MuxReplacementReason::MetadataChanged,
            checkpoint,
        });
    }
    if checkpoint.observation.partial != opening.wire.partial {
        let reason = if checkpoint.observation.partial.is_some() == opening.wire.partial.is_some() {
            MuxReplacementReason::PartialSnapshotChanged
        } else {
            MuxReplacementReason::SourceSetChanged
        };
        return Ok(MuxScanPlan::Replacement { reason, checkpoint });
    }
    let (Some(prior_chat), Some(current_chat), Some(chat_file)) = (
        checkpoint.chat.as_ref(),
        opening.wire.chat.as_ref(),
        opening.chat_file.as_ref(),
    ) else {
        return Ok(MuxScanPlan::Replacement {
            reason: MuxReplacementReason::SourceSetChanged,
            checkpoint,
        });
    };
    if prior_chat.observation.content_identity != current_chat.content_identity {
        return Ok(MuxScanPlan::Replacement {
            reason: MuxReplacementReason::ChatReplaced,
            checkpoint,
        });
    }
    if current_chat.length < prior_chat.observation.length
        || current_chat.length < prior_chat.frontier.next_offset
    {
        return Ok(MuxScanPlan::Replacement {
            reason: MuxReplacementReason::ChatTruncated,
            checkpoint,
        });
    }
    if current_chat.length <= prior_chat.observation.length {
        return Ok(MuxScanPlan::Replacement {
            reason: MuxReplacementReason::ChatPrefixChanged,
            checkpoint,
        });
    }
    if !prefix_matches_checkpoint(chat_file, &prior_chat.frontier)? {
        return Ok(MuxScanPlan::Replacement {
            reason: MuxReplacementReason::ChatPrefixChanged,
            checkpoint,
        });
    }
    Ok(MuxScanPlan::Append { checkpoint })
}

#[allow(clippy::too_many_arguments)]
fn scan_leaf(
    candidate: &MuxSourceBackedCandidate,
    context: &ProviderAdapterContext,
    session: &mut MuxBoundedSessionMetadata,
    path: &Path,
    file: &OpenedProviderSourceFile,
    kind: MuxStreamKind,
    observation: &MuxFileObservation,
    initial_frontier: MuxFrontier,
    source_revision_digest: [u8; 32],
    emitted_documents: &mut u64,
    emitted_unaddressable: &mut u64,
    emit: &mut impl FnMut(MuxSourceBackedPage) -> MuxSourceBackedResult<()>,
) -> MuxSourceBackedResult<MuxLeafScan> {
    let plan = source_plan(
        candidate,
        path,
        kind,
        observation.clone(),
        initial_frontier.clone(),
    );
    let (mut reader, mut hasher) = open_reader_at_frontier(file, &initial_frontier)?;
    let mut frontier = initial_frontier.clone();
    let mut retained_records = 0_u64;
    let mut indexed_documents = 0_u64;
    let mut unaddressable_records = 0_u64;
    let mut rejected_records = 0_u64;
    let mut first_failure = None;

    loop {
        let page = read_core_page(
            &mut reader,
            &mut hasher,
            session,
            &plan,
            frontier.clone(),
            rejected_records,
            first_failure.clone(),
            context,
        )?
        .ok_or(CaptureError::SystemInvariant(
            "Mux source-backed parser omitted a terminal page",
        ))?;
        if session.provider_session_id != candidate.metadata.provider_session_id {
            return Err(MuxSourceBackedError::OwnerChanged);
        }
        rejected_records = page.rejected_records;
        first_failure = page.first_failure.clone();
        let terminal = page.terminal;
        let deferred_incomplete = page.deferred_incomplete;
        frontier = page.next.clone();
        retained_records = checked_add(
            retained_records,
            u64::try_from(page.rows.len()).map_err(|_| MuxSourceBackedError::CountOverflow)?,
        )?;
        let projected = project_page(candidate, file, kind, page.rows, source_revision_digest)?;
        let page_documents = u64::try_from(projected.records.len())
            .map_err(|_| MuxSourceBackedError::CountOverflow)?;
        let page_unaddressable = u64::try_from(projected.unaddressable.len())
            .map_err(|_| MuxSourceBackedError::CountOverflow)?;
        indexed_documents = checked_add(indexed_documents, page_documents)?;
        unaddressable_records = checked_add(unaddressable_records, page_unaddressable)?;
        *emitted_documents = checked_add(*emitted_documents, page_documents)?;
        *emitted_unaddressable = checked_add(*emitted_unaddressable, page_unaddressable)?;
        if !projected.records.is_empty() || !projected.unaddressable.is_empty() {
            emit(projected)?;
        }
        if terminal || deferred_incomplete {
            break;
        }
    }
    let complete_records = frontier
        .next_ordinal
        .checked_sub(initial_frontier.next_ordinal)
        .ok_or(MuxSourceBackedError::InvalidCheckpoint)?;
    let content = MuxComponentDigest {
        bytes: frontier.next_offset,
        digest: hasher.finalize().into(),
    };
    Ok(MuxLeafScan {
        checkpoint: MuxLeafCheckpoint {
            observation: leaf_observation(observation, kind),
            frontier,
            content,
        },
        complete_records,
        retained_records,
        indexed_documents,
        unaddressable_records,
    })
}

fn source_plan(
    candidate: &MuxSourceBackedCandidate,
    path: &Path,
    kind: MuxStreamKind,
    observation: MuxFileObservation,
    initial_frontier: MuxFrontier,
) -> MuxSourcePlan {
    MuxSourcePlan {
        source: candidate.source.clone(),
        path: path.to_path_buf(),
        kind,
        source_revision: observation.source_revision(kind.label()),
        metadata_revision: observation.metadata_revision(),
        observation,
        path_identity: "mux-source-backed".to_owned(),
        cursor_stream: "mux-source-backed".to_owned(),
        canonical_source_identity: candidate.source_key.identity().to_string(),
        prior: None,
        generation: 0,
        initial_frontier,
        accepted_events: 0,
        rejected_records: 0,
        first_failure: None,
        legacy_bridge: None,
    }
}

fn project_page(
    candidate: &MuxSourceBackedCandidate,
    file: &OpenedProviderSourceFile,
    stream_kind: MuxStreamKind,
    rows: Vec<MuxPreparedRow>,
    source_revision_digest: [u8; 32],
) -> MuxSourceBackedResult<MuxSourceBackedPage> {
    let mut records = Vec::with_capacity(rows.len());
    let mut unaddressable = Vec::new();
    for row in rows {
        let native_item_key = NativeItemKey::native_id(
            MUX_NATIVE_ITEM_NAMESPACE,
            TypedKey::utf8(&row.native_record_id)?,
        )?;
        let event_id = derive_event_id(EventIdentityInput {
            source: &candidate.source_key,
            session_id: candidate.session_id,
            logical_item_kind: MUX_LOGICAL_EVENT_KIND,
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })?;
        if let Some(reason) = row.unaddressable_output {
            let bounded_projection = row
                .event
                .as_ref()
                .map(|event| bounded_projection(candidate, event, &row, None));
            unaddressable.push(MuxUnaddressableRecord {
                event_id,
                stream_kind,
                source_record_ordinal: row.source_record_ordinal,
                native_record_id: row.native_record_id,
                reason: match reason {
                    MuxUnaddressableOutput::Redacted => MuxUnaddressableReason::RedactedOutput,
                    MuxUnaddressableOutput::Missing => MuxUnaddressableReason::MissingOutput,
                },
                bounded_projection,
            });
            continue;
        }
        let Some(event) = row.event.as_ref() else {
            continue;
        };
        let exact_body =
            exact_mux_lexical_body(file, stream_kind, &row, event.provider_event_index)?;
        let projection = bounded_projection(candidate, event, &row, Some(exact_body));
        let locator = SourceRecordLocator::new(
            candidate.source_key.clone(),
            NativeRecordCoordinate::ProviderNative {
                namespace: MUX_PROVIDER_NATIVE_LOCATOR_NAMESPACE.to_owned(),
                coordinate: encode_mux_coordinate(
                    stream_kind,
                    row.source_locator.value(),
                    row.source_record_ordinal,
                    projection.event_sequence,
                    &row.native_record_id,
                )?,
            },
            if stream_kind.is_partial() {
                LocatorRevisionPolicy::ExactSourceRevision
            } else {
                LocatorRevisionPolicy::StableRecordEvidence
            },
            Some(source_revision_digest),
            decode_record_digest(&row.source_record_digest)?,
        )?;
        let document = LexicalDocument {
            event_id,
            session_id: candidate.session_id,
            parent_session_id: candidate.parent_session_id,
            root_session_id: candidate.root_session_id,
            source: candidate.source_key.clone(),
            locator,
            provider_session_id: Some(candidate.metadata.provider_session_id.clone()),
            branch: None,
            source_path: mux_stream_path(candidate, stream_kind)
                .map(|path| path.display().to_string()),
            agent_type: if candidate.parent_session_id.is_some() {
                "subagent".to_owned()
            } else {
                "primary".to_owned()
            },
            is_primary: candidate.parent_session_id.is_none(),
            event_sequence: projection.event_sequence,
            occurred_at_unix_ms: projection.occurred_at_unix_ms,
            event_type: projection.event_type,
            role: projection.role,
            body: projection.body,
            workspace: None,
            cwd: projection.cwd,
            touched_files: projection.touched_files,
        };
        records.push(MuxSourceBackedRecord {
            document,
            stream_kind,
            source_record_ordinal: row.source_record_ordinal,
            native_record_id: row.native_record_id,
            message_content_ref: row.message_content_ref,
        });
    }
    Ok(MuxSourceBackedPage {
        source: candidate.source_key.clone(),
        session_id: candidate.session_id,
        stream_kind,
        records,
        unaddressable,
    })
}

fn bounded_projection(
    candidate: &MuxSourceBackedCandidate,
    event: &super::MuxCoreEvent,
    row: &MuxPreparedRow,
    exact_body: Option<String>,
) -> MuxBoundedProjection {
    let text = event
        .payload
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| event.event_type.as_str());
    let touched_files = row
        .file_touches
        .iter()
        .map(|touch| touch.path.clone())
        .collect();
    MuxBoundedProjection {
        provider_session_id: candidate.metadata.provider_session_id.clone(),
        event_sequence: event.provider_event_index,
        occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: event.role.map(|role| role.as_str().to_owned()),
        body: exact_body.unwrap_or_else(|| text.to_owned()),
        cwd: candidate.metadata.cwd.clone(),
        touched_files,
    }
}

fn exact_mux_lexical_body(
    file: &OpenedProviderSourceFile,
    stream_kind: MuxStreamKind,
    row: &MuxPreparedRow,
    event_sequence: u64,
) -> MuxSourceBackedResult<String> {
    let (_, byte_start, byte_end_exclusive) = decode_mux_legacy_range(row.source_locator.value())
        .ok_or(MuxSourceBackedError::InvalidLocator)?;
    let coordinate = MuxLogicalRecordCoordinate {
        stream_kind,
        byte_start,
        byte_end_exclusive,
        source_record_ordinal: row.source_record_ordinal,
        event_sequence,
        native_record_id: row.native_record_id.clone(),
    };
    let payload = read_mux_payload(file, &coordinate).map_err(scan_projection_failure)?;
    if Sha256::digest(&payload).as_slice() != decode_record_digest(&row.source_record_digest)? {
        return Err(MuxSourceBackedError::ExactLexicalProjection(
            "native source record digest changed during scan".to_owned(),
        ));
    }
    let value = serde_json::from_slice::<serde_json::Value>(&payload)?;
    mux_exact_logical_content(&value).map_err(scan_projection_failure)
}

fn scan_projection_failure(failure: HydrationFailure) -> MuxSourceBackedError {
    MuxSourceBackedError::ExactLexicalProjection(format!("{:?}: {}", failure.kind, failure.detail))
}

fn scan_counts(
    base: Option<&CertifiedSource>,
    plan: &MuxScanPlan,
    chat: Option<&MuxLeafScan>,
    partial: Option<&MuxLeafScan>,
    metadata: Option<&MuxComponentDigest>,
) -> MuxSourceBackedResult<ScannedSourceCounts> {
    let delta_complete = sum_leaf(chat, partial, |scan| scan.complete_records)?;
    let delta_retained = sum_leaf(chat, partial, |scan| scan.retained_records)?;
    let delta_indexed = sum_leaf(chat, partial, |scan| scan.indexed_documents)?;
    let _delta_unaddressable = sum_leaf(chat, partial, |scan| scan.unaddressable_records)?;
    let (complete_records, retained_records, indexed_documents) = match plan {
        MuxScanPlan::Append { .. } => {
            let base = base
                .ok_or(MuxSourceBackedError::InvalidCheckpoint)?
                .counts();
            (
                checked_add(base.complete_records, delta_complete)?,
                checked_add(base.retained_records, delta_retained)?,
                checked_add(base.indexed_documents, delta_indexed)?,
            )
        }
        MuxScanPlan::Cold | MuxScanPlan::Replacement { .. } => {
            (delta_complete, delta_retained, delta_indexed)
        }
    };
    let rejected_records = complete_records
        .checked_sub(retained_records)
        .ok_or(MuxSourceBackedError::InvalidCheckpoint)?;
    let certified_bytes = checked_add(
        sum_leaf(chat, partial, |scan| scan.checkpoint.content.bytes)?,
        metadata.map_or(0, |metadata| metadata.bytes),
    )?;
    Ok(ScannedSourceCounts {
        complete_records,
        retained_records,
        rejected_records,
        ignored_records: 0,
        indexed_documents,
        certified_bytes,
    })
}

fn sum_leaf(
    chat: Option<&MuxLeafScan>,
    partial: Option<&MuxLeafScan>,
    value: impl Fn(&MuxLeafScan) -> u64,
) -> MuxSourceBackedResult<u64> {
    checked_add(
        chat.map(&value).unwrap_or(0),
        partial.map(value).unwrap_or(0),
    )
}

fn admit_mux_candidate(
    candidate: &MuxSourceBackedCandidate,
) -> MuxSourceBackedResult<MuxObservedSource> {
    admit_mux_source(
        &candidate.authority,
        &candidate.source,
        &candidate.session_relative_path,
        candidate.chat_relative_path.as_deref(),
        candidate.partial_relative_path.as_deref(),
        candidate.metadata_relative_path.as_deref(),
    )
}

fn admit_mux_source(
    authority: &ProviderSourceRoot,
    _source: &MuxSessionSource,
    session_relative_path: &Path,
    expected_chat: Option<&Path>,
    expected_partial: Option<&Path>,
    expected_metadata: Option<&Path>,
) -> MuxSourceBackedResult<MuxObservedSource> {
    let chat_relative = session_relative_path.join("chat.jsonl");
    let partial_relative = session_relative_path.join("partial.json");
    let metadata_relative = session_relative_path.join("metadata.json");
    let chat_file = open_optional_mux_file(authority, &chat_relative)?;
    let partial_file = open_optional_mux_file(authority, &partial_relative)?;
    let metadata_file = open_optional_mux_file(authority, &metadata_relative)?;
    if chat_file.is_some() != expected_chat.is_some()
        || partial_file.is_some() != expected_partial.is_some()
        || metadata_file.is_some() != expected_metadata.is_some()
    {
        return Err(MuxSourceBackedError::CandidateChanged);
    }
    let metadata_bytes = metadata_file
        .as_ref()
        .map(|metadata| metadata.read_all_bounded(MAX_PROVIDER_JSONL_LINE_BYTES))
        .transpose()?;
    let metadata_stamp = metadata_file
        .as_ref()
        .map(OpenedProviderSourceFile::metadata);
    let chat = chat_file
        .as_ref()
        .map(|file| {
            MuxFileObservation::from_admitted(
                authority.named_path().join(&chat_relative),
                file.metadata(),
                metadata_stamp,
            )
        })
        .transpose()?;
    let partial = partial_file
        .as_ref()
        .map(|file| {
            MuxFileObservation::from_admitted(
                authority.named_path().join(&partial_relative),
                file.metadata(),
                metadata_stamp,
            )
        })
        .transpose()?;
    let metadata_revision = chat
        .as_ref()
        .or(partial.as_ref())
        .map(MuxFileObservation::metadata_revision)
        .ok_or(CaptureError::SystemInvariant(
            "Mux source-backed session has no supported leaf",
        ))?;
    Ok(MuxObservedSource {
        wire: MuxCompoundObservationWire {
            version: MUX_CHECKPOINT_VERSION,
            chat: chat
                .as_ref()
                .map(|observation| leaf_observation(observation, MuxStreamKind::Chat)),
            partial: partial
                .as_ref()
                .map(|observation| leaf_observation(observation, MuxStreamKind::Partial)),
            metadata_revision,
        },
        chat,
        partial,
        chat_file,
        partial_file,
        metadata_file,
        metadata_bytes,
    })
}

fn open_optional_mux_file(
    authority: &ProviderSourceRoot,
    relative_path: &Path,
) -> MuxSourceBackedResult<Option<OpenedProviderSourceFile>> {
    match authority.open_file(relative_path) {
        Ok(file) => Ok(Some(file)),
        Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn relative_to_mux_authority(
    authority: &ProviderSourceRoot,
    path: &Path,
) -> MuxSourceBackedResult<PathBuf> {
    let canonical = std::fs::canonicalize(path)?;
    canonical
        .strip_prefix(authority.named_path())
        .map(Path::to_path_buf)
        .map_err(|_| {
            CaptureError::InvalidProviderTranscriptPath {
                path: canonical,
                reason: "Mux compound leaves must share one authority root",
            }
            .into()
        })
}

fn leaf_observation(
    observation: &MuxFileObservation,
    kind: MuxStreamKind,
) -> MuxLeafObservationWire {
    MuxLeafObservationWire {
        content_identity: observation.content_identity(),
        source_revision: observation.source_revision(kind.label()),
        length: observation.content.length,
    }
}

fn source_observation(
    source: &SourceKey,
    wire: &MuxCompoundObservationWire,
) -> MuxSourceBackedResult<SourceObservation> {
    Ok(SourceObservation::new(
        source.clone(),
        MUX_SOURCE_REVISION_KIND,
        serde_json::to_vec(wire)?,
    )?)
}

fn mux_source_key(native_session_id: &str) -> MuxSourceBackedResult<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        MUX_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?;
    Ok(SourceKey::derive(
        CaptureProvider::Mux.as_str(),
        MUX_SOURCE_FORMAT,
        MUX_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn mux_session_identity(
    source: &SourceKey,
    native_session_id: &str,
) -> MuxSourceBackedResult<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        MUX_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: MUX_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

fn mux_related_session_identity(native_session_id: &str) -> MuxSourceBackedResult<StableEntityId> {
    let source = mux_source_key(native_session_id)?;
    mux_session_identity(&source, native_session_id)
}

fn mux_stream_path(
    candidate: &MuxSourceBackedCandidate,
    stream_kind: MuxStreamKind,
) -> Option<&Path> {
    match stream_kind {
        MuxStreamKind::Chat => candidate.source.chat_path.as_deref(),
        MuxStreamKind::Partial => candidate.source.partial_path.as_deref(),
    }
}

fn decode_checkpoint(base: &CertifiedSource) -> MuxSourceBackedResult<MuxSourceBackedCheckpoint> {
    let frontier = base
        .frontier()
        .ok_or(MuxSourceBackedError::MissingCheckpoint)?;
    if frontier.checkpoint_kind() != MUX_FRONTIER_KIND {
        return Err(MuxSourceBackedError::InvalidCheckpoint);
    }
    let TypedKey::Bytes(bytes) = frontier.checkpoint() else {
        return Err(MuxSourceBackedError::InvalidCheckpoint);
    };
    let checkpoint: MuxSourceBackedCheckpoint = serde_json::from_slice(bytes)?;
    if checkpoint.version != MUX_CHECKPOINT_VERSION
        || checkpoint.observation.version != MUX_CHECKPOINT_VERSION
        || checkpoint.counts != base.counts()
    {
        return Err(MuxSourceBackedError::InvalidCheckpoint);
    }
    Ok(checkpoint)
}

fn open_reader_at_frontier(
    source: &OpenedProviderSourceFile,
    frontier: &MuxFrontier,
) -> MuxSourceBackedResult<(BufReader<std::fs::File>, Sha256)> {
    let mut file = source.file().try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut remaining = frontier.next_offset;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    while remaining != 0 {
        let take = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| MuxSourceBackedError::CountOverflow)?;
        let read = file.read(&mut buffer[..take])?;
        if read == 0 {
            return Err(CaptureError::InvalidPayload(
                "Mux cursor frontier exceeds its source".to_owned(),
            )
            .into());
        }
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    if <[u8; 32]>::from(hasher.clone().finalize()) != frontier.prefix_sha256 {
        return Err(CaptureError::InvalidPayload(
            "Mux cursor prefix no longer matches its source".to_owned(),
        )
        .into());
    }
    file.seek(SeekFrom::Start(frontier.next_offset))?;
    Ok((BufReader::new(file), hasher))
}

fn prefix_matches_checkpoint(
    source: &OpenedProviderSourceFile,
    frontier: &MuxFrontier,
) -> MuxSourceBackedResult<bool> {
    let mut file = source.file().try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut remaining = frontier.next_offset;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    while remaining != 0 {
        let take = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| MuxSourceBackedError::CountOverflow)?;
        let read = file.read(&mut buffer[..take])?;
        if read == 0 {
            return Ok(false);
        }
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok(<[u8; 32]>::from(hasher.finalize()) == frontier.prefix_sha256)
}

fn digest_optional_file(bytes: Option<&[u8]>) -> MuxSourceBackedResult<Option<MuxComponentDigest>> {
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    Ok(Some(MuxComponentDigest {
        bytes: u64::try_from(bytes.len()).map_err(|_| MuxSourceBackedError::CountOverflow)?,
        digest: Sha256::digest(&bytes).into(),
    }))
}

fn compound_content_digest(checkpoint: &MuxSourceBackedCheckpoint) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx/mux/source-backed/compound-content/v1\0");
    hash_component(
        &mut hasher,
        b"chat",
        checkpoint.chat.as_ref().map(|leaf| &leaf.content),
    );
    hash_component(
        &mut hasher,
        b"partial",
        checkpoint.partial.as_ref().map(|leaf| &leaf.content),
    );
    hash_component(&mut hasher, b"metadata", checkpoint.metadata.as_ref());
    hasher.finalize().into()
}

fn hash_component(hasher: &mut Sha256, label: &[u8], component: Option<&MuxComponentDigest>) {
    hasher.update((label.len() as u64).to_be_bytes());
    hasher.update(label);
    match component {
        Some(component) => {
            hasher.update([1]);
            hasher.update(component.bytes.to_be_bytes());
            hasher.update(component.digest);
        }
        None => hasher.update([0]),
    }
}

fn decode_record_digest(digest: &CompleteContentBodyDigest) -> MuxSourceBackedResult<[u8; 32]> {
    let bytes = digest.as_str().as_bytes();
    if bytes.len() != 64 {
        return Err(MuxSourceBackedError::InvalidRecordDigest);
    }
    let mut decoded = [0_u8; 32];
    for (index, pair) in bytes.chunks_exact(2).enumerate() {
        decoded[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Ok(decoded)
}

fn hex_nibble(byte: u8) -> MuxSourceBackedResult<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(MuxSourceBackedError::InvalidRecordDigest),
    }
}

fn checked_add(left: u64, right: u64) -> MuxSourceBackedResult<u64> {
    left.checked_add(right)
        .ok_or(MuxSourceBackedError::CountOverflow)
}

#[cfg(test)]
mod tests;
