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
    MuxOutputOutcome, MUX_LOCATOR_KIND,
};
use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    complete_content::CompleteContentBodyDigest,
    provider::normalization::provider_value_text,
    CaptureError, MAX_PROVIDER_JSONL_LINE_BYTES, MUX_SOURCE_FORMAT,
};

use crate::provider::providers::mux::{
    metadata::{mux_bounded_session_metadata_from_bytes, MuxBoundedSessionMetadata},
    source::{MuxFileObservation, MuxSessionSource},
};

use super::source::discover_sessions;

mod projection;
mod resolver;

use projection::{classify_scan, scan_counts, scan_leaf};
pub(crate) use resolver::MuxSourceBackedResolverV0;
#[cfg(test)]
use resolver::{decode_mux_coordinate, encode_mux_coordinate};
use resolver::{
    decode_mux_legacy_range, mux_exact_logical_content, read_mux_payload,
    MuxLogicalRecordCoordinate,
};

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
}

impl MuxSourceBackedCandidate {
    pub(crate) fn source_key(&self) -> &SourceKey {
        &self.source_key
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

// Append proof is owned source authority. Boxing its 992 bytes to match the
// 65-byte replacement evidence adds allocation without measured scan benefit.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub(crate) enum MuxSourceBackedDisposition {
    Cold,
    Unchanged,
    // The certified append proof is retained for commit-time/Pro consumers.
    #[allow(dead_code)]
    Append {
        proof: CertifiedSourceAppend,
    },
    Replacement {
        evidence: MuxReplacementEvidence,
    },
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
    // Stream kind remains explicit at both page and record boundaries.
    #[allow(dead_code)]
    pub(crate) stream_kind: MuxStreamKind,
    pub(crate) records: Vec<MuxSourceBackedRecord>,
    pub(crate) unaddressable: Vec<MuxUnaddressableRecord>,
}

#[derive(Debug, Clone)]
pub(crate) struct MuxSourceBackedRecord {
    pub(crate) document: LexicalDocument,
    pub(crate) stream_kind: MuxStreamKind,
    // Native identity and complete-content evidence remain available to staging
    // Pro/exact hydration consumers.
    #[allow(dead_code)]
    pub(crate) source_record_ordinal: u64,
    #[allow(dead_code)]
    pub(crate) native_record_id: String,
    #[allow(dead_code)]
    pub(crate) message_content_ref: Option<ctx_history_core::ContentRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MuxUnaddressableReason {
    RedactedOutput,
    MissingOutput,
}

#[derive(Debug, Clone)]
pub(crate) struct MuxBoundedProjection {
    // Keep provider identity with the bounded unaddressable projection.
    #[allow(dead_code)]
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
    // Exact native coordinates remain diagnostic evidence; Core currently
    // consumes only reason and bounded_projection.
    #[allow(dead_code)]
    pub(crate) event_id: StableEntityId,
    #[allow(dead_code)]
    pub(crate) stream_kind: MuxStreamKind,
    #[allow(dead_code)]
    pub(crate) source_record_ordinal: u64,
    #[allow(dead_code)]
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
        digest: Sha256::digest(bytes).into(),
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
