//! Bounded page-local output passed directly to one publication candidate.

use std::collections::{BTreeMap, BTreeSet};

use crate::core_materialization::ordered_parallel_map_owned;
use crate::core_materialization::{
    CoreProjectionPreparer, CoreStoreError, PreparedCoreEventDeltaPage,
};
use crate::graph::segment::projection::{
    CorePageProjectionMetrics, PreparedCorePageProjection, ProjectedCoreRecordEvidence,
    ProjectionError, prepare_core_page_projection, project_tombstone,
};
use crate::graph::segment::{
    CompactIndexedCoreEventState, EventIndexSource, EventLineageAccumulator, EventLineageTables,
    EventOwner, EventTombstone, IndexedCopiedEventOrigin, IndexedCoreEventLineage,
    IndexedCoreEventOriginKind, IndexedCoreEventTombstone, ServingRecord, event_output_root,
};
use crate::graph::segment_state::{
    SegmentCorePageOutput, SegmentPreparedEvent, SegmentPublicationMutation,
    SegmentPublicationTombstone,
};
use crate::protocol::{
    CoreEventDelta, CoreRecord, CoreSourceDelta, CoreSourceState, EventCopyProofKind,
    ProviderNativeCopyProof, ProviderNativeSessionRelationship, SessionRelationshipKind,
    StableEntityId,
};
use sha2::{Digest as _, Sha256};

use super::model::MAX_DIRECT_BATCH_PAGES;
use super::{SegmentMaterializer, SegmentMaterializerError};

const DIRECT_PUBLICATION_SEMANTICS_DOMAIN: &[u8] = b"ctx-pro-direct-publication-semantics-v1\0";

pub(super) struct StagedPage {
    pub(super) sequence: u32,
    pub(super) materialization_id: String,
    pub(super) graph_generation: u64,
    pub(super) serving_records: Vec<ServingRecord>,
    pub(super) flat_tombstones: Vec<EventTombstone>,
    pub(super) event_source: EventIndexSource,
    pub(super) index_records: Vec<CompactIndexedCoreEventState>,
    pub(super) index_lineage: EventLineageTables,
    pub(super) index_tombstones: Vec<IndexedCoreEventTombstone>,
    page_semantics_sha256: [u8; 32],
}

impl StagedPage {
    #[allow(clippy::too_many_arguments)]
    fn event(
        sequence: u32,
        output: SegmentCorePageOutput,
        event_identities: Vec<StableEntityId>,
        event_records: Vec<Option<&CoreRecord>>,
        source: CoreSourceState,
        projection: PreparedCorePageProjection,
        force_projection_rebuild: bool,
    ) -> Result<Self, SegmentMaterializerError> {
        let page_semantics_sha256 = direct_page_semantics_sha256(&output)?;
        let SegmentCorePageOutput {
            materialization_id,
            graph_generation,
            effect: response,
            mutations,
            ..
        } = output;
        if mutations.len() != event_identities.len() || mutations.len() != event_records.len() {
            return Err(SegmentMaterializerError::Corrupt(
                "event page identities do not match mutations",
            ));
        }
        if materialization_id != response.materialization_id
            || !source.source.exact_descriptor_eq(&response.source)
            || !is_lower_sha256(&materialization_id)
        {
            return Err(SegmentMaterializerError::Conflict);
        }
        let expected_records =
            usize::try_from(response.additions.saturating_add(response.replacements))
                .map_err(|_| SegmentMaterializerError::Bounds)?;
        let expected_tombstones = if force_projection_rebuild {
            0
        } else {
            usize::try_from(response.replacements.saturating_add(response.tombstones))
                .map_err(|_| SegmentMaterializerError::Bounds)?
        };
        let event_source = EventIndexSource::new(source.source.clone())?;
        let mut records = Vec::new();
        let mut lineage = EventLineageAccumulator::new();
        let mut owners = Vec::new();
        let mut tombstones = Vec::new();
        let mut flat_tombstones = Vec::new();
        for ((mutation, identity), record) in mutations
            .into_iter()
            .zip(event_identities)
            .zip(event_records)
        {
            match mutation {
                SegmentPublicationMutation::Added(event) => push_current(
                    &event_source,
                    event,
                    record.ok_or(SegmentMaterializerError::Corrupt(
                        "current Core record is absent",
                    ))?,
                    &mut records,
                    &mut lineage,
                    &mut owners,
                )?,
                SegmentPublicationMutation::Replaced {
                    tombstone,
                    replacement,
                } => {
                    push_current(
                        &event_source,
                        replacement,
                        record.ok_or(SegmentMaterializerError::Corrupt(
                            "replacement Core record is absent",
                        ))?,
                        &mut records,
                        &mut lineage,
                        &mut owners,
                    )?;
                    if !force_projection_rebuild {
                        push_tombstone(
                            &event_source,
                            &tombstone,
                            identity,
                            &mut flat_tombstones,
                            &mut tombstones,
                        )?;
                    }
                }
                SegmentPublicationMutation::Tombstoned(tombstone) => {
                    if record.is_some() {
                        return Err(SegmentMaterializerError::Corrupt(
                            "tombstoned event carries a current Core record",
                        ));
                    }
                    if !force_projection_rebuild {
                        push_tombstone(
                            &event_source,
                            &tombstone,
                            identity,
                            &mut flat_tombstones,
                            &mut tombstones,
                        )?;
                    }
                }
            }
        }

        let PreparedCorePageProjection {
            projected,
            record_evidence,
            metrics: _,
        } = projection;
        owners.sort();
        if projected.owners != owners || projected.records.len() != record_evidence.len() {
            return Err(SegmentMaterializerError::Corrupt(
                "prepared page projection ownership is inconsistent",
            ));
        }
        let oversized_events = projected
            .omissions
            .iter()
            .filter(|omission| {
                omission.reason
                    == crate::graph::segment::ProjectionOmissionReason::OversizedFlatRecord
            })
            .map(|omission| (omission.source_id.clone(), omission.event_id.clone()))
            .collect::<BTreeSet<_>>();
        populate_output_commitments(
            &event_source,
            graph_generation,
            &mut records,
            &projected.records,
            &record_evidence,
            &oversized_events,
        )?;
        let index_lineage = lineage.finish(&mut records)?;
        if records.len() != expected_records
            || tombstones.len() != expected_tombstones
            || flat_tombstones.len() > expected_tombstones
        {
            return Err(SegmentMaterializerError::Corrupt(
                "direct publication page shape is inconsistent",
            ));
        }
        Ok(Self {
            sequence,
            materialization_id,
            graph_generation,
            serving_records: projected.records,
            flat_tombstones,
            event_source,
            index_records: records,
            index_lineage,
            index_tombstones: tombstones,
            page_semantics_sha256,
        })
    }

    pub(super) fn advance_publication_semantics(
        &self,
        prior_sha256: &str,
    ) -> Result<String, SegmentMaterializerError> {
        let prior: [u8; 32] = hex::decode(prior_sha256)
            .map_err(|_| SegmentMaterializerError::Corrupt("publication digest is invalid"))?
            .try_into()
            .map_err(|_| SegmentMaterializerError::Corrupt("publication digest is invalid"))?;
        let mut digest = Sha256::new();
        digest.update(DIRECT_PUBLICATION_SEMANTICS_DOMAIN);
        digest.update(prior);
        digest.update(self.page_semantics_sha256);
        Ok(hex::encode(digest.finalize()))
    }
}

fn direct_page_semantics_sha256(
    output: &SegmentCorePageOutput,
) -> Result<[u8; 32], SegmentMaterializerError> {
    let encoded = output.encode().map_err(|error| match error {
        CoreStoreError::Bounds => SegmentMaterializerError::Bounds,
        _ => SegmentMaterializerError::Corrupt("Core page output is invalid"),
    })?;
    Ok(Sha256::digest(encoded).into())
}

pub(super) fn apply_event_pages(
    store: &mut SegmentMaterializer,
    candidate: &mut super::model::CandidateState,
    direct: &mut super::publication::DirectCandidate,
    cursor: &super::reconciliation_cursor::ReconciliationCursor,
    preparer: &CoreProjectionPreparer,
    prepared_pages: &[PreparedCoreEventDeltaPage],
    materializer_revision: &str,
) -> Result<(), SegmentMaterializerError> {
    if prepared_pages.is_empty() || prepared_pages.len() > MAX_DIRECT_BATCH_PAGES {
        return Err(SegmentMaterializerError::Bounds);
    }
    for prepared in prepared_pages {
        super::lifecycle::validate_prepared_request(prepared)?;
    }
    let first = prepared_pages
        .first()
        .ok_or(SegmentMaterializerError::Bounds)?
        .page();
    if prepared_pages.iter().any(|prepared| {
        let page = prepared.page();
        page.materialization_id != first.materialization_id
            || page.core_generation_id != first.core_generation_id
    }) {
        return Err(SegmentMaterializerError::Conflict);
    }

    // Projection preparation is pure and page-local. Collect every indexed
    // result before candidate-state mutation so worker completion order cannot
    // expose a partially prepared batch.
    let projections = prepare_page_projections(preparer, prepared_pages)?
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
    let oversized = projections
        .iter()
        .filter_map(|projection| {
            let mut omitted = projection.projected.omissions.iter().filter(|omission| {
                omission.reason
                    == crate::graph::segment::ProjectionOmissionReason::OversizedFlatRecord
            });
            omitted.next().map(|first| {
                (
                    first.source_id.clone(),
                    first.fact_id.clone(),
                    1 + omitted.count(),
                )
            })
        })
        .collect::<Vec<_>>();
    for projection in &projections {
        record_projection_metrics(&mut store.metrics, projection.metrics);
    }

    let original = candidate.clone();
    let mut next = original.clone();
    let mut staged = Vec::with_capacity(prepared_pages.len());
    for (ordinal, (prepared, projection)) in prepared_pages.iter().zip(projections).enumerate() {
        let validated = super::lifecycle::validation::validate_event_delta(
            store,
            &mut next,
            cursor,
            prepared,
            materializer_revision,
        );
        let validated = validated?;
        let original_coverage = projection
            .projected
            .omissions
            .iter()
            .any(|omission| {
                omission.reason
                    == crate::graph::segment::ProjectionOmissionReason::OversizedFlatRecord
            })
            .then(|| {
                validated
                    .output
                    .mutations
                    .iter()
                    .filter_map(|mutation| match mutation {
                        SegmentPublicationMutation::Added(event)
                        | SegmentPublicationMutation::Replaced {
                            replacement: event, ..
                        } => Some((
                            event.event_identity.to_string(),
                            event.prepared.coverage.clone(),
                        )),
                        SegmentPublicationMutation::Tombstoned(_) => None,
                    })
                    .collect::<BTreeMap<_, _>>()
            });
        let sequence = original
            .staged_page_count
            .checked_add(u32::try_from(ordinal).map_err(|_| SegmentMaterializerError::Bounds)?)
            .ok_or(SegmentMaterializerError::Bounds)?;
        let rebuild = next.control.force_projection_rebuild;
        let event_records = prepared
            .page()
            .deltas
            .iter()
            .map(CoreEventDelta::record)
            .collect::<Vec<_>>();
        let page = StagedPage::event(
            sequence,
            validated.output,
            validated.event_identities,
            event_records,
            validated.projection_source,
            projection,
            rebuild,
        );
        let page = page?;
        if let Some(original_coverage) = original_coverage {
            for state in &page.index_records {
                let original = original_coverage.get(&state.event_id.to_string()).ok_or(
                    SegmentMaterializerError::Corrupt(
                        "projected event coverage has no prepared source",
                    ),
                )?;
                if original != &state.coverage {
                    next.control.coverage = super::model::add_coverage(
                        &super::model::subtract_coverage(&next.control.coverage, original)?,
                        &state.coverage,
                    )?;
                }
            }
        }
        staged.push(page);
    }
    super::lifecycle::support::stage_direct_pages(direct, &mut next, staged)?;
    *candidate = next;
    for (source_id, fact_id, count) in oversized {
        eprintln!(
            "warning: Blame omitted {count} oversized fact(s) from source {source_id} (first fact {fact_id}); Flat record limit is {} bytes",
            crate::graph::segment::MAX_FLAT_RECORD_PAYLOAD_BYTES
        );
    }
    Ok(())
}

fn prepare_page_projections(
    preparer: &CoreProjectionPreparer,
    prepared_pages: &[PreparedCoreEventDeltaPage],
) -> Result<
    Vec<Result<PreparedCorePageProjection, SegmentMaterializerError>>,
    SegmentMaterializerError,
> {
    validate_projection_output_lengths(
        prepared_pages
            .iter()
            .map(|prepared| prepared.prepared_output_encoded_len()),
    )?;
    ordered_parallel_map_owned(
        preparer,
        prepared_pages.iter().collect(),
        MAX_DIRECT_BATCH_PAGES,
        |prepared| Ok(prepare_page_projection(prepared)),
    )
    .map_err(|_| SegmentMaterializerError::Corrupt("provider projection workers are unavailable"))
}

fn validate_projection_output_lengths(
    lengths: impl IntoIterator<Item = usize>,
) -> Result<(), SegmentMaterializerError> {
    let total = lengths.into_iter().try_fold(0_usize, |total, length| {
        total
            .checked_add(length)
            .ok_or(SegmentMaterializerError::Bounds)
    })?;
    if total > crate::protocol::MAX_CORE_EVENT_DELTA_PAGES_PREPARED_OUTPUT_BYTES {
        return Err(SegmentMaterializerError::Bounds);
    }
    Ok(())
}

#[cfg(test)]
#[path = "staging_tests.rs"]
mod tests;
#[cfg(test)]
pub(super) use tests::{
    prepare_page_projections_for_test, validate_projection_output_lengths_for_test,
};

fn prepare_page_projection(
    prepared: &PreparedCoreEventDeltaPage,
) -> Result<PreparedCorePageProjection, SegmentMaterializerError> {
    let page = prepared.page();
    let CoreSourceDelta::Present(source) = &page.reconciliation.delta else {
        if page.deltas.iter().any(|delta| delta.record().is_some()) {
            return Err(SegmentMaterializerError::Corrupt(
                "removed source contains a current event",
            ));
        }
        return Ok(PreparedCorePageProjection::empty());
    };
    let units = page
        .deltas
        .iter()
        .filter_map(CoreEventDelta::record)
        .map(|record| {
            prepared.units.get(&record.event_id.to_string()).ok_or(
                SegmentMaterializerError::Corrupt("prepared event unit is missing"),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    prepare_core_page_projection(&page.core_generation_id, source, &units).map_err(projection_error)
}

fn projection_error(error: ProjectionError) -> SegmentMaterializerError {
    match error {
        ProjectionError::FlatRecordBounds => SegmentMaterializerError::Bounds,
        ProjectionError::Encoding => SegmentMaterializerError::Encoding,
        ProjectionError::Invalid(_)
        | ProjectionError::InvalidTimestamp
        | ProjectionError::Model(_) => SegmentMaterializerError::Corrupt("Core projection failed"),
    }
}

fn record_projection_metrics(
    metrics: &mut super::model::MaterializerMetrics,
    projection: CorePageProjectionMetrics,
) {
    metrics.staging_projection_page_traversals = metrics
        .staging_projection_page_traversals
        .saturating_add(projection.page_traversals);
    metrics.staging_projection_unit_traversals = metrics
        .staging_projection_unit_traversals
        .saturating_add(projection.unit_traversals);
    metrics.staging_projection_fact_traversals = metrics
        .staging_projection_fact_traversals
        .saturating_add(projection.fact_traversals);
    metrics.staging_projection_record_serializations = metrics
        .staging_projection_record_serializations
        .saturating_add(projection.record_serializations);
    metrics.staging_projection_record_serialized_bytes = metrics
        .staging_projection_record_serialized_bytes
        .saturating_add(projection.record_serialized_bytes);
}

fn indexed_lineage(
    record: &CoreRecord,
    disposition: crate::core_materialization::ProducerAuthorityDisposition,
) -> IndexedCoreEventLineage {
    let (origin_kind, copied_from) = match &record.event_copy {
        Some(copy) => (
            IndexedCoreEventOriginKind::CopiedFromAncestor,
            Some(IndexedCopiedEventOrigin {
                ancestor_session_id: copy.ancestor_session_id,
                ancestor_event_id: copy.ancestor_event_id,
                proof: match copy.proof {
                    ProviderNativeCopyProof::NativeEventIdentity => {
                        EventCopyProofKind::NativeEventIdentity
                    }
                    ProviderNativeCopyProof::NativeCopiedFromField => {
                        EventCopyProofKind::NativeCopiedFromField
                    }
                    ProviderNativeCopyProof::NativeCallResultIdentity => {
                        EventCopyProofKind::NativeCallResultIdentity
                    }
                },
            }),
        ),
        None if disposition
            == crate::core_materialization::ProducerAuthorityDisposition::EligibleUnique =>
        {
            (IndexedCoreEventOriginKind::UniqueToSession, None)
        }
        None => (IndexedCoreEventOriginKind::Unknown, None),
    };
    IndexedCoreEventLineage {
        session_id: record.session_id,
        parent_session_id: record.parent_session_id,
        root_session_id: record.root_session_id,
        session_relationship: match record.session_relationship {
            Some(ProviderNativeSessionRelationship::Root) => SessionRelationshipKind::Root,
            Some(ProviderNativeSessionRelationship::Delegated) => {
                SessionRelationshipKind::Delegated
            }
            Some(ProviderNativeSessionRelationship::Forked) => SessionRelationshipKind::Forked,
            Some(ProviderNativeSessionRelationship::ResumedFrom) => {
                SessionRelationshipKind::ResumedFrom
            }
            Some(ProviderNativeSessionRelationship::WorkflowChild) => {
                SessionRelationshipKind::WorkflowChild
            }
            None => SessionRelationshipKind::RelatedUnknown,
        },
        origin_kind,
        copied_from,
    }
}

fn push_current(
    source: &EventIndexSource,
    event: SegmentPreparedEvent,
    record: &CoreRecord,
    records: &mut Vec<CompactIndexedCoreEventState>,
    lineage: &mut EventLineageAccumulator,
    owners: &mut Vec<EventOwner>,
) -> Result<(), SegmentMaterializerError> {
    if event.owner.source_id != source.storage_key
        || event.event_identity.source_descriptor_digest()
            != source.source.exact_descriptor_digest()
        || event.event_identity != record.event_id
        || event.owner.event_sequence != record.event_sequence
    {
        return Err(SegmentMaterializerError::Corrupt(
            "prepared event and Core record are inconsistent",
        ));
    }
    if !event.prepared.facts.is_empty() {
        owners.push(EventOwner {
            source_id: event.owner.source_id.clone(),
            event_id: event.owner.event_id.clone(),
            direct_session_id: event.owner.direct_session_id.clone(),
            root_session_id: event.owner.root_session_id.clone(),
            event_sequence: event.owner.event_sequence,
        });
    }
    let disposition = event.prepared.producer_authority_disposition;
    let mut state = CompactIndexedCoreEventState {
        source_storage_key: event.owner.source_id,
        event_id: event.event_identity,
        session_ref: 0,
        event_sequence: event.owner.event_sequence,
        core_record_sha256: event.core_record_sha256,
        core_record_leaf_sha256: event.core_record_leaf_sha256,
        flat_record_count: 0,
        event_output_root: String::new(),
        coverage: event.prepared.coverage,
    };
    state.session_ref = lineage
        .retain(state.key(), indexed_lineage(record, disposition))
        .map_err(|error| match error {
            crate::graph::segment::EventIndexError::Bound(bound) => {
                SegmentMaterializerError::BoundDetail(format!(
                    "event index {bound} for source {}",
                    state.source_storage_key
                ))
            }
            crate::graph::segment::EventIndexError::Conflict => SegmentMaterializerError::Conflict,
            _ => SegmentMaterializerError::Corrupt("Core event lineage is invalid"),
        })?
        .0;
    records.push(state);
    Ok(())
}

fn push_tombstone(
    source: &EventIndexSource,
    tombstone: &SegmentPublicationTombstone,
    event_id: StableEntityId,
    flat: &mut Vec<EventTombstone>,
    index: &mut Vec<IndexedCoreEventTombstone>,
) -> Result<(), SegmentMaterializerError> {
    if tombstone.owner.source_id != source.storage_key
        || event_id.source_descriptor_digest() != source.source.exact_descriptor_digest()
    {
        return Err(SegmentMaterializerError::Corrupt(
            "event tombstone source is inconsistent",
        ));
    }
    flat.push(
        project_tombstone(&EventOwner {
            source_id: tombstone.owner.source_id.clone(),
            event_id: tombstone.owner.event_id.clone(),
            direct_session_id: tombstone.owner.direct_session_id.clone(),
            root_session_id: tombstone.owner.root_session_id.clone(),
            event_sequence: tombstone.owner.event_sequence,
        })
        .map_err(|_| SegmentMaterializerError::Corrupt("Core tombstone projection failed"))?,
    );
    index.push(IndexedCoreEventTombstone {
        source_storage_key: tombstone.owner.source_id.clone(),
        event_id,
        prior_event_state_sha256: tombstone.prior_event_state_sha256.clone(),
    });
    Ok(())
}

fn populate_output_commitments(
    source: &EventIndexSource,
    publication_generation: u64,
    states: &mut [CompactIndexedCoreEventState],
    serving_records: &[ServingRecord],
    record_evidence: &[ProjectedCoreRecordEvidence],
    oversized_events: &BTreeSet<(String, String)>,
) -> Result<(), SegmentMaterializerError> {
    let mut grouped = grouped_record_digests(serving_records, record_evidence)?;
    for state in states {
        let key = (state.source_storage_key.clone(), state.event_id.to_string());
        let summary = grouped.remove(&key).unwrap_or_default();
        let (flat_record_count, root) = event_output_root(
            publication_generation,
            &source.source,
            state.event_id,
            state.event_sequence,
            &state.core_record_sha256,
            &state.core_record_leaf_sha256,
            summary
                .records
                .iter()
                .map(|(record_id, digest)| (record_id.as_str(), *digest)),
        )
        .map_err(|_| SegmentMaterializerError::Corrupt("event output commitment failed"))?;
        state.flat_record_count = flat_record_count;
        state.event_output_root = root;
        if oversized_events.contains(&key) {
            retain_projected_coverage(&mut state.coverage, &summary);
            state.coverage.bounded_omission_events = 1;
        }
    }
    if !grouped.is_empty() {
        return Err(SegmentMaterializerError::Corrupt(
            "projected serving row has no EventIndex state",
        ));
    }
    Ok(())
}

#[derive(Default)]
struct EventRecordSummary {
    records: Vec<(String, [u8; 32])>,
    file: bool,
    commit: bool,
    pull_request: bool,
    live_access: bool,
}

type EventRecordDigests = BTreeMap<(String, String), EventRecordSummary>;

fn retain_projected_coverage(
    coverage: &mut crate::graph::segment_state::SegmentCoreCoverage,
    summary: &EventRecordSummary,
) {
    if summary.records.is_empty() {
        let bounded_omission_events = coverage.bounded_omission_events;
        *coverage = Default::default();
        coverage.bounded_omission_events = bounded_omission_events;
        return;
    }
    if !summary.file {
        coverage.file_evidence_events = 0;
    }
    if !summary.commit {
        coverage.exact_commit_evidence_events = 0;
    }
    if !summary.pull_request {
        coverage.exact_pull_request_evidence_events = 0;
    }
    if !summary.live_access {
        coverage.certified_live_root_access_events = 0;
    }
}

fn grouped_record_digests(
    serving_records: &[ServingRecord],
    record_evidence: &[ProjectedCoreRecordEvidence],
) -> Result<EventRecordDigests, SegmentMaterializerError> {
    if serving_records.len() != record_evidence.len() {
        return Err(SegmentMaterializerError::Corrupt(
            "staged serving evidence count is invalid",
        ));
    }
    let mut grouped = EventRecordDigests::new();
    for (record, evidence) in serving_records.iter().zip(record_evidence) {
        let summary = grouped
            .entry((
                record.event_owner.source_id.clone(),
                record.event_owner.event_id.clone(),
            ))
            .or_default();
        summary
            .records
            .push((record.record_id.clone(), evidence.canonical_sha256));
        for resource in [Some(&record.subject), record.object.as_ref()]
            .into_iter()
            .flatten()
        {
            summary.file |= resource.kind == crate::protocol::ResourceKind::File.wire_name();
            summary.commit |= resource.kind == crate::protocol::ResourceKind::Commit.wire_name();
            summary.pull_request |=
                resource.kind == crate::protocol::ResourceKind::PullRequest.wire_name();
        }
        summary.live_access |= record.fact_family.as_str() == "_ctx.repository.live_access";
    }
    for summary in grouped.values_mut() {
        summary
            .records
            .sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    }
    Ok(grouped)
}

fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
