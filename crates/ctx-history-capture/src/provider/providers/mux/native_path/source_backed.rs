//! Provider-local source-backed projection for supported Mux session trees.
//!
//! A Mux session directory is one logical source. Its append-oriented
//! `chat.jsonl`, mutable `partial.json`, and optional metadata are certified as
//! one compound snapshot so chat and partial events retain one stable session
//! identity. The provider-neutral lifecycle/registry owner can consume these
//! bounded pages without learning Mux file-layout or locator details.

use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CertifiedSourceAppend,
    EventIdentityInput, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, ProjectionContractError, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceFrontier, SourceKey, SourceObservation, SourceRecordLocator,
    SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::{LexicalDocument, MAX_BODY_PREVIEW_CHARS};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    model::{MuxFrontier, MuxPreparedRow, MuxSourcePlan, MuxStreamKind, MuxUnaddressableOutput},
    parse::{open_reader_at_frontier, read_core_page},
};
use crate::{
    complete_content::{
        jsonl::{valid_mux_locator, MUX_LOCATOR_KIND},
        CompleteContentBodyDigest, CompleteContentSourceLocator,
    },
    provider::normalization::provider_local_preview,
    CaptureError, ProviderAdapterContext, MAX_PROVIDER_JSONL_LINE_BYTES, MUX_SOURCE_FORMAT,
};

use crate::provider::providers::mux::{
    metadata::{mux_bounded_session_metadata, MuxBoundedSessionMetadata},
    source::{MuxFileObservation, MuxSessionSource},
};

use super::source::discover_sessions;

const MUX_SOURCE_ANCHOR_NAMESPACE: &str = "mux.session";
const MUX_NATIVE_SESSION_NAMESPACE: &str = "mux.session";
const MUX_NATIVE_ITEM_NAMESPACE: &str = "mux.record";
const MUX_PROVIDER_NATIVE_LOCATOR_NAMESPACE: &str = "mux.record.v1";
const MUX_LOGICAL_SESSION_KIND: &str = "mux-session";
const MUX_LOGICAL_EVENT_KIND: &str = "mux-event";
const MUX_SOURCE_SCHEMA_VARIANT: &str = "mux-session-tree-source-backed-v1";
const MUX_SOURCE_REVISION_KIND: &str = "mux-session-compound-observation-v1";
const MUX_FRONTIER_KIND: &str = "mux-session-compound-frontier-v1";
const MUX_PARSER_REVISION: &str = "mux-source-backed-v1";
const MUX_CHECKPOINT_VERSION: u32 = 1;

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
}

pub(crate) type MuxSourceBackedResult<T> = Result<T, MuxSourceBackedError>;

/// One discovered, supported Mux session source.
///
/// `chat-archive.jsonl` is intentionally absent from this contract.
#[derive(Debug, Clone)]
pub(crate) struct MuxSourceBackedCandidate {
    configured_root: PathBuf,
    source: MuxSessionSource,
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
    pub(crate) complete_content_locator: CompleteContentSourceLocator,
    pub(crate) complete_content_record_digest: CompleteContentBodyDigest,
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

#[derive(Debug, Clone)]
struct MuxObservedSource {
    wire: MuxCompoundObservationWire,
    chat: Option<MuxFileObservation>,
    partial: Option<MuxFileObservation>,
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
    let mut candidates = Vec::with_capacity(sources.len());
    for source in sources {
        let observation = observe_mux_source(&source)?;
        let metadata = mux_bounded_session_metadata(
            &source,
            &observation.wire.metadata_revision,
            observed_at,
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
            source,
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
    let opening = observe_mux_source(&candidate.source)?;
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
            let closing = observe_mux_source(&candidate.source)?;
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

    let plan = classify_scan(candidate, base, &opening)?;
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
        prior_checkpoint,
    ) {
        (Some(path), Some(observation), _) => Some(scan_leaf(
            candidate,
            &context,
            &mut session,
            path,
            MuxStreamKind::Chat,
            observation,
            chat_start,
            source_revision_digest,
            &mut emitted_documents,
            &mut emitted_unaddressable,
            &mut emit,
        )?),
        (None, None, Some(checkpoint)) if !scan_partial => {
            checkpoint.chat.as_ref().map(|chat| MuxLeafScan {
                checkpoint: chat.clone(),
                complete_records: 0,
                retained_records: 0,
                indexed_documents: 0,
                unaddressable_records: 0,
            })
        }
        (None, None, _) => None,
        _ => return Err(MuxSourceBackedError::CandidateChanged),
    };

    let partial_scan = if scan_partial {
        match (
            candidate.source.partial_path.as_ref(),
            opening.partial.as_ref(),
        ) {
            (Some(path), Some(observation)) => Some(scan_leaf(
                candidate,
                &context,
                &mut session,
                path,
                MuxStreamKind::Partial,
                observation,
                MuxFrontier::initial(),
                source_revision_digest,
                &mut emitted_documents,
                &mut emitted_unaddressable,
                &mut emit,
            )?),
            (None, None) => None,
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
        digest_optional_file(candidate.source.metadata_path.as_deref())?
    };
    let closing = observe_mux_source(&candidate.source)?;
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
    let observed = observe_mux_source(&candidate.source)?;
    let observation = source_observation(&candidate.source_key, &observed.wire)?;
    Ok(checkpoint.observation == observed.wire
        && certificate.observation().revision() == observation.revision())
}

/// Translates the provider-native envelope back into the existing exact Mux
/// route locator. The integration registry only needs to supply brokered source
/// access for the path selected by [`mux_source_path_for_locator`].
pub(crate) fn mux_complete_content_locator(
    locator: &SourceRecordLocator,
) -> Option<CompleteContentSourceLocator> {
    if locator.source().provider() != CaptureProvider::Mux.as_str()
        || locator.source().source_format() != MUX_SOURCE_FORMAT
        || locator.source().schema_variant() != MUX_SOURCE_SCHEMA_VARIANT
    {
        return None;
    }
    let NativeRecordCoordinate::ProviderNative {
        namespace,
        coordinate: TypedKey::Bytes(value),
    } = locator.coordinate()
    else {
        return None;
    };
    if namespace != MUX_PROVIDER_NATIVE_LOCATOR_NAMESPACE || !valid_mux_locator(value) {
        return None;
    }
    CompleteContentSourceLocator::new(MUX_LOCATOR_KIND, value.clone())
}

pub(crate) fn mux_source_path_for_locator<'a>(
    candidate: &'a MuxSourceBackedCandidate,
    locator: &SourceRecordLocator,
) -> Option<&'a Path> {
    if !candidate.source_key.exact_descriptor_eq(locator.source()) {
        return None;
    }
    let locator = mux_complete_content_locator(locator)?;
    match locator.value().first().copied() {
        Some(1) => candidate.source.chat_path.as_deref(),
        Some(2) => candidate.source.partial_path.as_deref(),
        _ => None,
    }
}

fn classify_scan(
    candidate: &MuxSourceBackedCandidate,
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
    let (Some(prior_chat), Some(current_chat), Some(chat_path)) = (
        checkpoint.chat.as_ref(),
        opening.wire.chat.as_ref(),
        candidate.source.chat_path.as_deref(),
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
    if !prefix_matches_checkpoint(chat_path, &prior_chat.frontier)? {
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
    let (mut reader, mut hasher) = open_reader_at_frontier(path, &initial_frontier)?;
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
        let projected = project_page(candidate, kind, page.rows, source_revision_digest)?;
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
                .map(|event| bounded_projection(candidate, event, &row));
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
        let projection = bounded_projection(candidate, event, &row);
        let locator = SourceRecordLocator::new(
            candidate.source_key.clone(),
            NativeRecordCoordinate::ProviderNative {
                namespace: MUX_PROVIDER_NATIVE_LOCATOR_NAMESPACE.to_owned(),
                coordinate: TypedKey::bytes(row.source_locator.value().to_vec())?,
            },
            if stream_kind.is_partial() {
                LocatorRevisionPolicy::ExactSourceRevision
            } else {
                LocatorRevisionPolicy::StableRecordEvidence
            },
            stream_kind.is_partial().then_some(source_revision_digest),
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
            complete_content_locator: row.source_locator,
            complete_content_record_digest: row.source_record_digest,
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
) -> MuxBoundedProjection {
    let text = event
        .payload
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| event.event_type.as_str());
    let body = provider_local_preview(text, MAX_BODY_PREVIEW_CHARS).0;
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
        body,
        cwd: candidate.metadata.cwd.clone(),
        touched_files,
    }
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

fn observe_mux_source(source: &MuxSessionSource) -> MuxSourceBackedResult<MuxObservedSource> {
    let chat = source
        .chat_path
        .as_deref()
        .map(|path| MuxFileObservation::read(path, source.metadata_path.as_deref()))
        .transpose()?;
    let partial = source
        .partial_path
        .as_deref()
        .map(|path| MuxFileObservation::read(path, source.metadata_path.as_deref()))
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

fn prefix_matches_checkpoint(path: &Path, frontier: &MuxFrontier) -> MuxSourceBackedResult<bool> {
    let mut file = File::open(path)?;
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

fn digest_optional_file(path: Option<&Path>) -> MuxSourceBackedResult<Option<MuxComponentDigest>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let mut file = File::open(path)?;
    let maximum = u64::try_from(MAX_PROVIDER_JSONL_LINE_BYTES)
        .map_err(|_| MuxSourceBackedError::CountOverflow)?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_PROVIDER_JSONL_LINE_BYTES {
        return Err(CaptureError::InvalidPayload(
            "Mux metadata.json exceeds the supported size".to_owned(),
        )
        .into());
    }
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
