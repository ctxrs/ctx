//! Work-owned semantic ingestion contracts shared by Core feed and materialization.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::envelope::Fact;
use crate::protocol::{
    CoreMaterializationReceipt, CoreProjectionCurrentness, CoreSourceState, EvidenceCitation,
    MaterializedCoverage, StableEntityId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreStoreError {
    Conflict,
    Bounds,
    RebuildRequired,
    Backend,
}

impl fmt::Display for CoreStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Conflict => "Core materialization CAS conflict",
            Self::Bounds => "Core materialization bound exceeded",
            Self::RebuildRequired => "Core materialization requires a complete rebuild",
            Self::Backend => "Core materialization graph operation failed",
        })
    }
}

impl std::error::Error for CoreStoreError {}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CoreProjectionCoverage {
    pub repository_candidate_events: u64,
    pub logical_binding_events: u64,
    pub certified_live_root_access_events: u64,
    pub file_evidence_events: u64,
    pub exact_commit_evidence_events: u64,
    pub exact_pull_request_evidence_events: u64,
    pub bounded_omission_events: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CoreProjectionStatus {
    pub currentness: CoreProjectionCurrentness,
    pub requested_core_generation_id: Option<String>,
    pub receipt: Option<CoreMaterializationReceipt>,
    pub materialized_coverage: MaterializedCoverage,
    pub coverage: CoreProjectionCoverage,
    pub local_repository_access: bool,
    pub availability: CoreProjectionAvailability,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<ctx_attribution_model::BlameDiagnostic>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CoreProjectionAvailability {
    pub file_blame: bool,
    pub commit_blame: bool,
    pub pull_request_blame: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PreparedCoreEvidence {
    pub citation: EvidenceCitation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProducerAuthorityDisposition {
    EligibleUnique,
    AbstainUnknown,
    IneligibleCopied,
}

impl ProducerAuthorityDisposition {
    #[must_use]
    pub const fn permits_positive_authority(self) -> bool {
        matches!(self, Self::EligibleUnique)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PreparedCoreUnit {
    pub origin_event_id: String,
    pub producer_authority_disposition: ProducerAuthorityDisposition,
    pub stable_entities: Vec<StableEntityId>,
    pub facts: Vec<Fact>,
    pub evidence: Option<PreparedCoreEvidence>,
    pub coverage: CoreProjectionCoverage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PreparedCoreProjectionBatch {
    pub core_generation_id: String,
    pub source: CoreSourceState,
    pub units: Vec<PreparedCoreUnit>,
}
