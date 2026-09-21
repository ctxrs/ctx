use crate::protocol::{
    CommitLineage, CommitLineageBounds, CommitLineageEdge, CommitLineageOmission,
    CommitLineageTruncationReason, CommitLineageYield, ExactCommitRef,
};

use super::{EvidencePage, public_resource};
use crate::graph::segment_graph::SegmentGraph;
use crate::graph::segment_graph::SegmentGraphError;
use crate::query::{Resource, ResourceId};

pub(super) fn compose_commit_lineage(
    graph: &SegmentGraph,
    target: &crate::protocol::ResolvedBlameTarget,
) -> Result<Option<crate::query::commit_lineage::CommitLineageDraft>, SegmentGraphError> {
    let crate::protocol::ResolvedBlameTarget::Commit { commit, repository } = target else {
        return Ok(None);
    };
    let repository = Resource {
        id: ResourceId(repository.id.clone()),
        kind: repository.kind,
        display: repository.display.clone(),
        logical_repository: None,
    };
    let commit = Resource {
        id: ResourceId(commit.id.clone()),
        kind: commit.kind,
        display: commit.display.clone(),
        logical_repository: Some(repository.id.clone()),
    };
    crate::query::commit_lineage::compose(graph, commit, repository)
        .map_err(SegmentGraphError::from)
}

/// The sole conversion seam for the public exact-repository carrier.
pub(super) fn public_commit_lineage(
    lineage: &crate::query::commit_lineage::CommitLineageDraft,
    evidence: &mut EvidencePage,
) -> Result<CommitLineage, SegmentGraphError> {
    let edges = lineage
        .edges
        .iter()
        .map(|edge| {
            Ok(CommitLineageEdge {
                operation_id: edge.metadata.operation_id.clone(),
                kind: edge.metadata.kind,
                relation_class: edge.metadata.relation_class,
                source: public_exact_commit(&edge.source),
                result: public_exact_commit(&edge.result),
                actor: public_resource(&edge.actor),
                proof_class: edge.metadata.proof_class,
                state: edge.metadata.state,
                observed_at_ms: edge.observed_at_ms,
                evidence_numbers: evidence.numbers(&edge.citations)?,
            })
        })
        .collect::<Result<Vec<_>, SegmentGraphError>>()?;
    let yielded_by = lineage
        .yielded_by
        .iter()
        .map(|yielded| {
            Ok(CommitLineageYield {
                yield_id: yielded.yield_id.clone(),
                operation_id: yielded.operation_id.clone(),
                logical_repository_id: lineage.requested.repository.display.clone(),
                actor: public_resource(&yielded.actor),
                proof_class: yielded.proof_class,
                state: yielded.state,
                observed_at_ms: yielded.observed_at_ms,
                evidence_numbers: evidence.numbers(&yielded.citations)?,
            })
        })
        .collect::<Result<Vec<_>, SegmentGraphError>>()?;
    let (omission, truncation_reason) = if lineage.complete {
        (CommitLineageOmission::Exact(0), None)
    } else {
        (
            CommitLineageOmission::AtLeast(
                u32::try_from(lineage.omitted_at_least.max(1))
                    .map_err(|_| SegmentGraphError::QueryRecordTooLarge)?,
            ),
            lineage.truncation.map(|reason| match reason {
                crate::query::commit_lineage::LineageTruncation::ReturnedOperationLimit => {
                    CommitLineageTruncationReason::ReturnedEventLimit
                }
                crate::query::commit_lineage::LineageTruncation::ExaminedOperationLimit => {
                    CommitLineageTruncationReason::ExaminedEventLimit
                }
                crate::query::commit_lineage::LineageTruncation::EvidenceGap => {
                    CommitLineageTruncationReason::EvidenceGap
                }
            }),
        )
    };
    Ok(CommitLineage {
        requested: public_exact_commit(&lineage.requested),
        edges,
        yielded_by,
        origin: lineage.origin.as_ref().map(public_exact_commit),
        endpoint: None,
        complete: lineage.complete,
        ambiguous: lineage.ambiguous,
        bounds: CommitLineageBounds {
            returned_events: u32::try_from(lineage.returned_operations)
                .map_err(|_| SegmentGraphError::QueryRecordTooLarge)?,
            returned_event_limit: crate::protocol::MAX_COMMIT_LINEAGE_RETURNED_EVENTS,
            examined_events: u32::try_from(lineage.examined_operations)
                .map_err(|_| SegmentGraphError::QueryRecordTooLarge)?,
            examined_event_limit: crate::protocol::MAX_COMMIT_LINEAGE_EXAMINED_EVENTS,
            omission,
            truncation_reason,
        },
    })
}

fn public_exact_commit(commit: &crate::query::commit_lineage::ExactCommit) -> ExactCommitRef {
    ExactCommitRef {
        resource: public_resource(&commit.resource),
        logical_repository_id: commit.repository.display.clone(),
        object_format: commit.object_format,
        oid: commit.oid.clone(),
    }
}
