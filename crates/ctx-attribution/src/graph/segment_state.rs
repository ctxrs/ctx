//! Bounded Core materialization control and page-local projection output.

mod validation;

use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

use crate::envelope::Fact;
use crate::ingest::{CoreStoreError, ProducerAuthorityDisposition};
use crate::protocol::{
    CoreEventDeltaPageApplied, CoreGenerationHead, CoreMaterializationReceipt,
    CoreMaterializationReceiptIdentity, EvidenceCitation, StableEntityId, StableEntityKind,
};

use self::validation::bounded_json;

pub use ctx_attribution_index::{MAX_SEGMENT_CORE_EVENTS, SegmentCoreCoverage};
pub const MAX_SEGMENT_PUBLICATION_MUTATIONS: usize = MAX_SEGMENT_CORE_EVENTS * 2;
pub const MAX_SEGMENT_PREPARED_FACTS_PER_EVENT: usize = 4_096;
pub const MAX_SEGMENT_PREPARED_ENTITIES_PER_EVENT: usize = 64;
pub const MAX_SEGMENT_FACT_EVIDENCE_ITEMS: usize = 256;
pub const MAX_SEGMENT_FACT_ATTRIBUTES: usize = 256;
pub const MAX_SEGMENT_PREPARED_UNIT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_SEGMENT_CORE_PAGE_OUTPUT_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_SEGMENT_METADATA_ENTRIES: usize = 100_000;
pub const MAX_SEGMENT_METADATA_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_SEGMENT_PUBLICATION_RECORDS: usize = 100_000;
pub const MAX_SEGMENT_PUBLICATION_TOMBSTONES: usize = 100_000;
pub const MAX_SEGMENT_PUBLICATION_FLAT_RETAINED_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_SEGMENT_PUBLICATION_FLAT_INDEX_ASSOCIATIONS: usize = 32 * 1024;
pub const MAX_SEGMENT_PUBLICATION_EVENT_INDEX_OPEN_BYTES: usize =
    (256 * 1024 * 1024) - (1024 * 1024);

const QUALIFIED_CORE_CORPUS_EVENTS: usize = 2_647_903;
const _: () = assert!(QUALIFIED_CORE_CORPUS_EVENTS <= MAX_SEGMENT_CORE_EVENTS);
pub const EMPTY_PUBLICATION_SEMANTICS_SHA256: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentPreparedEvidence {
    pub citation: EvidenceCitation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentPreparedUnit {
    pub origin_event_id: String,
    pub producer_authority_disposition: ProducerAuthorityDisposition,
    pub stable_entities: Vec<StableEntityId>,
    pub facts: Vec<Fact>,
    pub evidence: Option<SegmentPreparedEvidence>,
    pub coverage: SegmentCoreCoverage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentEventOwner {
    pub source_id: String,
    pub event_id: String,
    pub direct_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_session_id: Option<String>,
    pub event_sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentPreparedEvent {
    pub owner: SegmentEventOwner,
    pub event_identity: StableEntityId,
    pub core_record_sha256: String,
    pub core_record_leaf_sha256: String,
    pub prepared: SegmentPreparedUnit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentPublicationTombstone {
    pub owner: SegmentEventOwner,
    pub prior_core_record_sha256: String,
    pub prior_event_state_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum SegmentPublicationMutation {
    Added(SegmentPreparedEvent),
    Replaced {
        tombstone: SegmentPublicationTombstone,
        replacement: SegmentPreparedEvent,
    },
    Tombstoned(SegmentPublicationTombstone),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentSourceFrontier {
    pub source_id: String,
    pub source_identity_sha256: String,
    pub source_descriptor_sha256: String,
    pub core_record_accumulator: String,
    pub event_count: u64,
    pub removal: bool,
    pub materialize_index: u32,
    pub next_event_page: u32,
    pub last_event_order_id: Option<String>,
    pub event_mutations: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentCompletedControl {
    pub graph_generation: u64,
    pub event_count: u64,
    pub receipt: Option<CoreMaterializationReceipt>,
    pub materialization_id: Option<String>,
    pub head: Option<CoreGenerationHead>,
    pub expected_prior_receipt: Option<CoreMaterializationReceiptIdentity>,
    pub finish_request_sha256: Option<String>,
    pub materializer_revision: String,
    pub schema_contract: String,
    pub semantics_contract: String,
    pub evidence_contract: String,
    pub core_record_contract: String,
    pub coverage: SegmentCoreCoverage,
    /// Legacy source-state fields from the released direct-materialization
    /// predecessor are accepted while reading authenticated control, but never
    /// participate in current materialization. Remove these sinks with that
    /// predecessor decoder after its release is outside the support floor.
    #[serde(
        default,
        rename = "replay_count",
        skip_serializing,
        deserialize_with = "deserialize_legacy_ignored"
    )]
    _legacy_replay_count: (),
    #[serde(
        default,
        rename = "replay_accumulator_sha256",
        skip_serializing,
        deserialize_with = "deserialize_legacy_ignored"
    )]
    _legacy_replay_accumulator_sha256: (),
    /// Streaming commitment to the validated Core page outputs used to build
    /// the published segments.
    pub publication_semantics_sha256: String,
}

impl SegmentCompletedControl {
    #[allow(clippy::too_many_arguments)]
    pub fn current(
        graph_generation: u64,
        event_count: u64,
        receipt: Option<CoreMaterializationReceipt>,
        materialization_id: Option<String>,
        head: Option<CoreGenerationHead>,
        expected_prior_receipt: Option<CoreMaterializationReceiptIdentity>,
        finish_request_sha256: Option<String>,
        materializer_revision: String,
        schema_contract: String,
        semantics_contract: String,
        evidence_contract: String,
        core_record_contract: String,
        coverage: SegmentCoreCoverage,
        publication_semantics_sha256: String,
    ) -> Self {
        Self {
            graph_generation,
            event_count,
            receipt,
            materialization_id,
            head,
            expected_prior_receipt,
            finish_request_sha256,
            materializer_revision,
            schema_contract,
            semantics_contract,
            evidence_contract,
            core_record_contract,
            coverage,
            _legacy_replay_count: (),
            _legacy_replay_accumulator_sha256: (),
            publication_semantics_sha256,
        }
    }
}

fn deserialize_legacy_ignored<'de, D>(deserializer: D) -> Result<(), D::Error>
where
    D: serde::Deserializer<'de>,
{
    IgnoredAny::deserialize(deserializer).map(drop)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentCandidateControl {
    pub materialization_id: String,
    pub head: CoreGenerationHead,
    pub expected_prior_receipt: Option<CoreMaterializationReceiptIdentity>,
    pub materializer_revision: String,
    /// Exact serving schema under which candidate page outputs were projected.
    pub schema_contract: String,
    pub graph_generation: u64,
    pub force_projection_rebuild: bool,
    pub next_source_page: u32,
    pub next_source_acknowledgement_page: u32,
    pub source_terminal: bool,
    pub last_source_order_id: Option<String>,
    /// Ordered source IDs observed in Core's current snapshot. This bounded
    /// candidate-only set lets the helper derive stored-minus-snapshot removals
    /// without accepting host-authored tombstones.
    pub seen_source_ids: Vec<String>,
    pub changed_sources: u32,
    pub removed_sources: u32,
    pub event_pages: u32,
    pub event_mutations: u64,
    pub event_count: u64,
    pub pending_sources: Vec<SegmentSourceFrontier>,
    pub coverage: SegmentCoreCoverage,
    pub publication_semantics_sha256: String,
    /// Exact persisted shape of candidate segment output accumulated from the
    /// authenticated staged pages. Finish seals this state and combines it with
    /// retained active references before any candidate segment write begins.
    #[serde(default)]
    pub publication_reference_plan: SegmentPublicationReferencePlan,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentPublicationReferencePlan {
    pub flat_segments: u32,
    pub flat_open_records: u32,
    pub flat_open_tombstones: u32,
    pub flat_open_retained_bytes: u64,
    pub flat_open_index_associations: u64,
    pub event_index_segments: u32,
    pub event_index_open_records: u32,
    pub event_index_open_tombstones: u32,
    pub event_index_open_accounted_bytes: u64,
    pub event_index_open_source_id: Option<String>,
}

impl SegmentPublicationReferencePlan {
    pub fn validate(&self) -> Result<(), CoreStoreError> {
        let flat_records =
            usize::try_from(self.flat_open_records).map_err(|_| CoreStoreError::Bounds)?;
        let flat_tombstones =
            usize::try_from(self.flat_open_tombstones).map_err(|_| CoreStoreError::Bounds)?;
        let flat_bytes =
            usize::try_from(self.flat_open_retained_bytes).map_err(|_| CoreStoreError::Bounds)?;
        let flat_associations = usize::try_from(self.flat_open_index_associations)
            .map_err(|_| CoreStoreError::Bounds)?;
        let index_records =
            usize::try_from(self.event_index_open_records).map_err(|_| CoreStoreError::Bounds)?;
        let index_tombstones = usize::try_from(self.event_index_open_tombstones)
            .map_err(|_| CoreStoreError::Bounds)?;
        let index_bytes = usize::try_from(self.event_index_open_accounted_bytes)
            .map_err(|_| CoreStoreError::Bounds)?;
        if flat_records > MAX_SEGMENT_PUBLICATION_RECORDS
            || flat_tombstones > MAX_SEGMENT_PUBLICATION_TOMBSTONES
            || flat_bytes > MAX_SEGMENT_PUBLICATION_FLAT_RETAINED_BYTES
            || flat_associations > MAX_SEGMENT_PUBLICATION_FLAT_INDEX_ASSOCIATIONS
            || index_records.saturating_add(index_tombstones) > MAX_SEGMENT_PUBLICATION_TOMBSTONES
            || index_bytes > MAX_SEGMENT_PUBLICATION_EVENT_INDEX_OPEN_BYTES
            || (index_records == 0 && index_tombstones == 0)
                != self.event_index_open_source_id.is_none()
            || self
                .event_index_open_source_id
                .as_deref()
                .is_some_and(|source| source.is_empty() || source.len() > 256)
        {
            return Err(CoreStoreError::Bounds);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SegmentCorePageOutput {
    pub materialization_id: String,
    pub graph_generation: u64,
    pub request_sha256: String,
    pub effect: CoreEventDeltaPageApplied,
    pub mutations: Vec<SegmentPublicationMutation>,
}

impl SegmentCorePageOutput {
    pub fn encode(&self) -> Result<Vec<u8>, CoreStoreError> {
        self.validate()?;
        bounded_json(self, MAX_SEGMENT_CORE_PAGE_OUTPUT_BYTES)
    }
}

#[cfg(test)]
#[path = "segment_state_tests.rs"]
mod tests;
