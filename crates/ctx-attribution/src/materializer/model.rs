//! Publication metadata and bounded in-process materializer state.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::core_materialization::CORE_MATERIALIZER_REVISION;
use crate::core_materialization::{CoreProjectionCoverage, CoreStoreError};
use crate::graph::segment::SegmentRef;
use crate::graph::segment_graph::{SEGMENT_ORDERING_IDENTITY, SEGMENT_SCHEMA_IDENTITY};
use crate::graph::segment_state::{
    EMPTY_PUBLICATION_SEMANTICS_SHA256, SegmentCandidateControl, SegmentCompletedControl,
    SegmentCoreCoverage, SegmentPublicationMutation,
};
use crate::graph::{GRAPH_EVIDENCE_FINGERPRINT, GRAPH_SEMANTICS_FINGERPRINT};
use crate::protocol::{CoreEventDelta, CoreSourceState, StableEntityId};

pub const MATERIALIZER_SOURCE_ROLE: u32 = 0x4d_53_52_43;
const OBSOLETE_DERIVED_ROLE: u32 = 0x4d_52_50_4c;
pub use ctx_attribution_index::SEGMENT_CHUNK_BYTES;
pub const MAX_DIRECT_BATCH_PAGES: usize = 16;
pub const MAX_MANIFEST_SEGMENTS: usize = 4_096;

pub use crate::graph::segment_state::{
    MAX_SEGMENT_METADATA_BYTES as MAX_METADATA_SEGMENT_BYTES,
    MAX_SEGMENT_METADATA_ENTRIES as MAX_METADATA_SEGMENT_ENTRIES,
    MAX_SEGMENT_PUBLICATION_EVENT_INDEX_OPEN_BYTES as MAX_PUBLICATION_EVENT_INDEX_OPEN_BYTES,
    MAX_SEGMENT_PUBLICATION_FLAT_INDEX_ASSOCIATIONS as MAX_PUBLICATION_FLAT_INDEX_ASSOCIATIONS,
    MAX_SEGMENT_PUBLICATION_FLAT_RETAINED_BYTES as MAX_PUBLICATION_FLAT_RETAINED_BYTES,
    MAX_SEGMENT_PUBLICATION_RECORDS as MAX_PUBLICATION_RECORDS,
    MAX_SEGMENT_PUBLICATION_TOMBSTONES as MAX_PUBLICATION_TOMBSTONES,
};

/// Aggregate accounted payload admitted to Flat/event-index publication
/// workers. The ordered producer retains at most one additional open batch of
/// each role while these credits are outstanding.
pub const MAX_PUBLICATION_IN_FLIGHT_BYTES: usize = 256 * 1024 * 1024;

pub const EVENT_INDEX_PUBLICATION_MEMORY_RESERVE: usize = 1024 * 1024;
const _: () = assert!(
    MAX_PUBLICATION_EVENT_INDEX_OPEN_BYTES
        == MAX_PUBLICATION_IN_FLIGHT_BYTES - EVENT_INDEX_PUBLICATION_MEMORY_RESERVE
);

/// Additional encoded/transient Flat writer bytes beyond retained input.
pub const MAX_PUBLICATION_FLAT_WRITER_ADDITIONAL_BYTES: usize =
    MAX_PUBLICATION_FLAT_INDEX_ASSOCIATIONS * (4 + 8)
        + 2 * 256 * 1024
        + crate::graph::segment::MAX_FLAT_RECORD_FRAME_BYTES
        + crate::graph::segment::FLAT_CHUNK_BYTES as usize;

#[derive(Clone, Debug)]
pub struct CandidateState {
    pub control: SegmentCandidateControl,
    pub next_materialize_index: u32,
    pub staged_page_count: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MaterializerMetrics {
    pub reconciliation_cursor_entry_count: u64,
    pub reconciliation_cursor_reserved_bytes: u64,
    pub staging_projection_page_traversals: u64,
    pub staging_projection_unit_traversals: u64,
    pub staging_projection_fact_traversals: u64,
    pub staging_projection_record_serializations: u64,
    pub staging_projection_record_serialized_bytes: u64,
    pub publication_worker_limit: u64,
    pub publication_jobs_started: u64,
    pub publication_jobs_completed: u64,
    pub publication_peak_workers: u64,
    pub publication_in_flight_byte_limit: u64,
    pub publication_peak_in_flight_bytes: u64,
    pub publication_checked_readbacks: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ManifestIdentitySet {
    pub schema: &'static str,
    pub evidence: &'static str,
    pub ordering: &'static str,
}

pub const fn manifest_identities() -> ManifestIdentitySet {
    ManifestIdentitySet {
        schema: SEGMENT_SCHEMA_IDENTITY,
        evidence: GRAPH_EVIDENCE_FINGERPRINT,
        ordering: SEGMENT_ORDERING_IDENTITY,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[expect(
    clippy::large_enum_variant,
    reason = "Source deltas carry the existing Core state by value; boxing every upsert would add an allocation to materialization."
)]
pub enum SourceMutation {
    Upsert {
        state: CoreSourceState,
        materializer_revision: String,
    },
    Removed {
        source_id: String,
    },
}

impl SourceMutation {
    pub fn source_id(&self) -> String {
        match self {
            Self::Upsert { state, .. } => source_storage_id(&state.source),
            Self::Removed { source_id } => source_id.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceStateSegment {
    pub completed: SegmentCompletedControl,
    pub mutations: Vec<SourceMutation>,
}

impl SourceStateSegment {
    pub fn encode(&self) -> Result<Vec<u8>, CoreStoreError> {
        self.validate()?;
        bounded_json(self, MAX_METADATA_SEGMENT_BYTES)
    }

    fn validate(&self) -> Result<(), CoreStoreError> {
        if self.mutations.len() > MAX_METADATA_SEGMENT_ENTRIES {
            return Err(CoreStoreError::Bounds);
        }
        self.completed.validate()?;
        let mut prior = None;
        for mutation in &self.mutations {
            validate_source_mutation(mutation)?;
            let source_id = mutation.source_id();
            if prior
                .as_deref()
                .is_some_and(|prior| prior >= source_id.as_str())
            {
                return Err(CoreStoreError::Backend);
            }
            prior = Some(source_id);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveSource {
    pub state: CoreSourceState,
    pub materializer_revision: String,
}

#[derive(Default)]
pub struct ManifestRoles {
    pub flat: Vec<SegmentRef>,
    pub event_indexes: Vec<SegmentRef>,
    pub sources: Vec<SegmentRef>,
}

impl ManifestRoles {
    pub fn classify(segments: &[SegmentRef]) -> Result<Self, CoreStoreError> {
        if segments.len() > MAX_MANIFEST_SEGMENTS {
            return Err(CoreStoreError::Bounds);
        }
        let mut roles = Self::default();
        for segment in segments {
            match segment.role {
                ctx_attribution_index::FLAT_SERVING_ROLE => roles.flat.push(segment.clone()),
                ctx_attribution_index::EVENT_STATE_INDEX_ROLE => {
                    roles.event_indexes.push(segment.clone());
                }
                MATERIALIZER_SOURCE_ROLE => roles.sources.push(segment.clone()),
                OBSOLETE_DERIVED_ROLE => {}
                _ => return Err(CoreStoreError::Backend),
            }
        }
        Ok(roles)
    }
}

pub const fn is_obsolete_derived_role(role: u32) -> bool {
    role == OBSOLETE_DERIVED_ROLE
}

pub fn initial_completed_control() -> SegmentCompletedControl {
    SegmentCompletedControl::current(
        0,
        0,
        None,
        None,
        None,
        None,
        None,
        CORE_MATERIALIZER_REVISION.to_owned(),
        SEGMENT_SCHEMA_IDENTITY.to_owned(),
        GRAPH_SEMANTICS_FINGERPRINT.to_owned(),
        GRAPH_EVIDENCE_FINGERPRINT.to_owned(),
        crate::protocol::core_record_contract_fingerprint(),
        SegmentCoreCoverage::default(),
        EMPTY_PUBLICATION_SEMANTICS_SHA256.to_owned(),
    )
}

pub fn coverage_from(value: &CoreProjectionCoverage) -> SegmentCoreCoverage {
    SegmentCoreCoverage {
        repository_candidate_events: value.repository_candidate_events,
        logical_binding_events: value.logical_binding_events,
        certified_live_root_access_events: value.certified_live_root_access_events,
        file_evidence_events: value.file_evidence_events,
        exact_commit_evidence_events: value.exact_commit_evidence_events,
        exact_pull_request_evidence_events: value.exact_pull_request_evidence_events,
    }
}

pub fn add_coverage(
    left: &SegmentCoreCoverage,
    right: &SegmentCoreCoverage,
) -> Result<SegmentCoreCoverage, CoreStoreError> {
    Ok(SegmentCoreCoverage {
        repository_candidate_events: add(
            left.repository_candidate_events,
            right.repository_candidate_events,
        )?,
        logical_binding_events: add(left.logical_binding_events, right.logical_binding_events)?,
        certified_live_root_access_events: add(
            left.certified_live_root_access_events,
            right.certified_live_root_access_events,
        )?,
        file_evidence_events: add(left.file_evidence_events, right.file_evidence_events)?,
        exact_commit_evidence_events: add(
            left.exact_commit_evidence_events,
            right.exact_commit_evidence_events,
        )?,
        exact_pull_request_evidence_events: add(
            left.exact_pull_request_evidence_events,
            right.exact_pull_request_evidence_events,
        )?,
    })
}

pub fn subtract_coverage(
    left: &SegmentCoreCoverage,
    right: &SegmentCoreCoverage,
) -> Result<SegmentCoreCoverage, CoreStoreError> {
    Ok(SegmentCoreCoverage {
        repository_candidate_events: subtract(
            left.repository_candidate_events,
            right.repository_candidate_events,
        )?,
        logical_binding_events: subtract(
            left.logical_binding_events,
            right.logical_binding_events,
        )?,
        certified_live_root_access_events: subtract(
            left.certified_live_root_access_events,
            right.certified_live_root_access_events,
        )?,
        file_evidence_events: subtract(left.file_evidence_events, right.file_evidence_events)?,
        exact_commit_evidence_events: subtract(
            left.exact_commit_evidence_events,
            right.exact_commit_evidence_events,
        )?,
        exact_pull_request_evidence_events: subtract(
            left.exact_pull_request_evidence_events,
            right.exact_pull_request_evidence_events,
        )?,
    })
}

pub fn source_storage_id(source: &crate::protocol::SourceKey) -> String {
    crate::core_materialization::core_source_storage_id(source)
}

pub fn source_order_id(source: &crate::protocol::SourceKey) -> String {
    hex::encode(source.identity().digest())
}

pub fn event_order_id(event: StableEntityId) -> String {
    hex::encode(event.digest())
}

pub fn canonical_sha256(value: &impl Serialize) -> Result<String, CoreStoreError> {
    let bytes = serde_json::to_vec(value).map_err(|_| CoreStoreError::Backend)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

pub fn active_receipt_identity(
    receipt: &Option<crate::protocol::CoreMaterializationReceipt>,
) -> Option<crate::protocol::CoreMaterializationReceiptIdentity> {
    receipt.as_ref().map(
        |receipt| crate::protocol::CoreMaterializationReceiptIdentity {
            core_generation_id: receipt.core_generation_id.clone(),
            materializer_revision: receipt.materializer_revision.clone(),
        },
    )
}

pub fn source_map_from_mutations(
    segments: impl IntoIterator<Item = SourceMutation>,
) -> Result<BTreeMap<String, ActiveSource>, CoreStoreError> {
    let mut sources = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for mutation in segments {
        let source_id = mutation.source_id();
        if !seen.insert(source_id.clone()) {
            continue;
        }
        if seen.len() > MAX_METADATA_SEGMENT_ENTRIES {
            return Err(CoreStoreError::Bounds);
        }
        match mutation {
            SourceMutation::Upsert {
                state,
                materializer_revision,
            } => {
                sources.insert(
                    source_id,
                    ActiveSource {
                        state,
                        materializer_revision,
                    },
                );
                if sources.len() > crate::protocol::MAX_CORE_SOURCE_STATES {
                    return Err(CoreStoreError::Bounds);
                }
            }
            SourceMutation::Removed { .. } => {}
        }
    }
    Ok(sources)
}

pub fn mutation_identities(
    page: &[CoreEventDelta],
    mutations: &[SegmentPublicationMutation],
) -> Result<Vec<StableEntityId>, CoreStoreError> {
    if page.len() != mutations.len() {
        return Err(CoreStoreError::Backend);
    }
    Ok(page.iter().map(CoreEventDelta::event_id).collect())
}

fn validate_source_mutation(mutation: &SourceMutation) -> Result<(), CoreStoreError> {
    match mutation {
        SourceMutation::Upsert {
            state,
            materializer_revision,
        } => {
            state.validate().map_err(|_| CoreStoreError::Backend)?;
            validate_source_id(&source_storage_id(&state.source))?;
            if state.event_count > crate::graph::segment_state::MAX_SEGMENT_CORE_EVENTS as u64
                || materializer_revision.is_empty()
                || materializer_revision.len()
                    > crate::protocol::MAX_CORE_MATERIALIZER_REVISION_BYTES
                || materializer_revision.chars().any(char::is_control)
            {
                return Err(CoreStoreError::Backend);
            }
        }
        SourceMutation::Removed { source_id } => validate_source_id(source_id)?,
    }
    Ok(())
}

fn validate_source_id(value: &str) -> Result<(), CoreStoreError> {
    let digest = value
        .strip_prefix("core_source_")
        .ok_or(CoreStoreError::Backend)?;
    if !is_lower_sha256(digest) {
        return Err(CoreStoreError::Backend);
    }
    Ok(())
}

fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn bounded_json(value: &impl Serialize, maximum: usize) -> Result<Vec<u8>, CoreStoreError> {
    let mut writer = BoundedWriter {
        bytes: Vec::new(),
        maximum,
        exceeded: false,
    };
    if serde_json::to_writer(&mut writer, value).is_err() {
        return Err(if writer.exceeded {
            CoreStoreError::Bounds
        } else {
            CoreStoreError::Backend
        });
    }
    if writer.bytes.is_empty() {
        return Err(CoreStoreError::Backend);
    }
    Ok(writer.bytes)
}

struct BoundedWriter {
    bytes: Vec<u8>,
    maximum: usize,
    exceeded: bool,
}

impl io::Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(length) = self.bytes.len().checked_add(bytes.len()) else {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "metadata bound",
            ));
        };
        if length > self.maximum {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "metadata bound",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn add(left: u64, right: u64) -> Result<u64, CoreStoreError> {
    left.checked_add(right).ok_or(CoreStoreError::Bounds)
}

fn subtract(left: u64, right: u64) -> Result<u64, CoreStoreError> {
    left.checked_sub(right).ok_or(CoreStoreError::Conflict)
}
