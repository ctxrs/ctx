use crate::graph::GRAPH_SEMANTICS_FINGERPRINT;
use crate::graph::segment_graph::{SEGMENT_EVIDENCE_IDENTITY, SEGMENT_SCHEMA_IDENTITY};
use crate::graph::segment_state::{SegmentCandidateControl, SegmentCompletedControl};

use super::*;

pub(super) fn completed_requires_rebuild(active: &SegmentCompletedControl, revision: &str) -> bool {
    let Some(receipt) = active.receipt.as_ref() else {
        return false;
    };
    active.schema_contract != SEGMENT_SCHEMA_IDENTITY
        || active.semantics_contract != GRAPH_SEMANTICS_FINGERPRINT
        || active.evidence_contract != SEGMENT_EVIDENCE_IDENTITY
        || active.core_record_contract != crate::protocol::core_record_contract_fingerprint()
        || active.materializer_revision != revision
        || receipt.materializer_revision != revision
        || active.head.as_ref().is_none_or(|head| {
            head.identity_version != crate::protocol::IDENTITY_VERSION
                || head.core_record_version != crate::protocol::CORE_RECORD_VERSION
                || head.normalization_revision != crate::protocol::CORE_NORMALIZATION_REVISION
                || head.content_policy_revision != crate::protocol::CORE_CONTENT_POLICY_REVISION
                || head.repository_contract_revision
                    != crate::protocol::CORE_REPOSITORY_CONTRACT_REVISION
        })
}

pub(super) fn completed_control(
    candidate: &SegmentCandidateControl,
    receipt: CoreMaterializationReceipt,
    finish_request_sha256: String,
) -> SegmentCompletedControl {
    SegmentCompletedControl::current(
        candidate.graph_generation,
        candidate.event_count,
        Some(receipt),
        Some(candidate.materialization_id.clone()),
        Some(candidate.head.clone()),
        candidate.expected_prior_receipt.clone(),
        Some(finish_request_sha256),
        candidate.materializer_revision.clone(),
        candidate.schema_contract.clone(),
        GRAPH_SEMANTICS_FINGERPRINT.to_owned(),
        SEGMENT_EVIDENCE_IDENTITY.to_owned(),
        crate::protocol::core_record_contract_fingerprint(),
        candidate.coverage.clone(),
        candidate.publication_semantics_sha256.clone(),
    )
}
