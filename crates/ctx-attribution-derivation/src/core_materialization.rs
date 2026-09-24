use std::collections::BTreeMap;
use std::io::{self, Write};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};

use crate::envelope::{Citation, DetectionBatch, MessageRole, Observation, ObservationPayload};
use crate::protocol::{
    CORE_CONTENT_POLICY_REVISION, CORE_NORMALIZATION_REVISION, CORE_RECORD_VERSION,
    CORE_REPOSITORY_CONTRACT_REVISION, CoreEventDeltaPage, CoreGenerationHead, CoreRecord,
    CoreSourceState, ErrorClass, EvidenceCitation, IDENTITY_VERSION,
    MAX_CORE_EVENT_DELTA_PAGES_PREPARED_OUTPUT_BYTES, ProtocolError,
    core_record_contract_fingerprint,
};
#[cfg(test)]
use crate::worker_budget::{
    MAX_CONFIGURED_CORE_PREPARATION_WORKERS, configured_provider_worker_limits,
};
use crate::worker_budget::{
    ProviderFinishPhase, ProviderWorkerBudget, default_provider_worker_budget,
};
use ctx_attribution_index::SCHEMA_CRITICAL_FACT_FAMILIES;

mod producer_authority;
#[cfg(any(test, feature = "test-support"))]
pub mod provider_contract_test_support;
mod repositories;

pub use producer_authority::producer_authority_disposition;
use repositories::{ProjectedRepositories, has_repository_candidate, project_repositories};

pub const CORE_MATERIALIZER_REVISION: &str = "2026.09.24.1+bounded-omissions";

#[allow(dead_code)]
pub const CORE_PREPARATION_WORKERS_ENV: &str = crate::worker_budget::CORE_PREPARATION_WORKERS_ENV;
pub const MAX_CORE_PREPARATION_PARALLELISM: usize = 32;
pub const MAX_CORE_PREPARATION_CREDITS: usize = 16;
pub const MAX_CORE_PREPARED_UNIT_BYTES: usize = 8 * 1024 * 1024;

pub struct CoreProjectionFinishGuard {
    _phase: ProviderFinishPhase,
}

static DEFAULT_CORE_PROJECTION_PREPARER: OnceLock<
    Result<Arc<preparation::CoreProjectionPreparerInner>, ProtocolError>,
> = OnceLock::new();

pub use crate::ingest::ProducerAuthorityDisposition;
pub use crate::ingest::{
    CoreProjectionAvailability, CoreProjectionCoverage, CoreProjectionStatus, CoreStoreError,
    PreparedCoreEvidence, PreparedCoreProjectionBatch, PreparedCoreUnit,
};

fn add_record_coverage(
    coverage: &mut CoreProjectionCoverage,
    repository: &ctx_repository_evidence::RepositoryEvaluation,
    projected: &ProjectedRepositories,
) {
    coverage.repository_candidate_events += u64::from(has_repository_candidate(repository));
    coverage.logical_binding_events += u64::from(projected.logical_binding);
    coverage.certified_live_root_access_events += u64::from(projected.live_access);
    coverage.file_evidence_events += u64::from(projected.file_evidence);
    coverage.exact_commit_evidence_events += u64::from(projected.commit_evidence);
    coverage.exact_pull_request_evidence_events += u64::from(projected.pull_request_evidence);
}

mod canonical;
mod preparation;
mod prepared_page;

pub use canonical::CoreEventJsonSerializationMetrics;
pub use preparation::ordered_parallel_map_owned;
pub use preparation::{CoreProjectionPreparer, core_preparation_peak_workers};
pub use prepared_page::PreparedCoreEventDeltaPage;

pub fn validate_core_generation_head(head: &CoreGenerationHead) -> Result<(), ProtocolError> {
    if head.identity_version != IDENTITY_VERSION
        || head.core_record_version != CORE_RECORD_VERSION
        || head.core_record_contract_fingerprint != core_record_contract_fingerprint()
        || head.normalization_revision != CORE_NORMALIZATION_REVISION
        || head.content_policy_revision != CORE_CONTENT_POLICY_REVISION
        || head.repository_contract_revision != CORE_REPOSITORY_CONTRACT_REVISION
    {
        return Err(ProtocolError::new(
            ErrorClass::ProtocolMismatch,
            "Core generation contract does not match this materializer",
        ));
    }
    Ok(())
}

/// Exhaustive shape assertion for the strict Core DTO mirror.
///
/// Adding an outcome, pull-request, or other authoritative field to
/// `CoreRecord` must make this destructure fail to compile until the
/// materializer makes an explicit projection or abstention decision.
fn assert_current_core_record_shape(record: &CoreRecord) {
    let CoreRecord {
        record_version: _,
        event_id: _,
        session_id: _,
        parent_session_id: _,
        root_session_id: _,
        session_relationship: _,
        event_copy: _,
        source: _,
        provider_session_id: _,
        native_event_id: _,
        event_sequence: _,
        occurred_at_unix_ms: _,
        event_type: _,
        role: _,
        agent_scope: _,
        parser_revision: _,
        normalization_revision: _,
        content: _,
    } = record;
}

fn validate_core_generation_id(core_generation_id: &str) -> Result<(), ProtocolError> {
    if core_generation_id.len() != 64
        || !core_generation_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ProtocolError::new(
            ErrorClass::InvalidRequest,
            "Core generation ID must be lowercase SHA-256",
        ));
    }
    Ok(())
}

pub fn core_source_storage_id(source: &crate::protocol::SourceKey) -> String {
    format!("core_source_{}", hex::encode(source.identity().digest()))
}

#[cfg(test)]
#[path = "core_materialization/preparation_tests.rs"]
mod preparation_tests;
