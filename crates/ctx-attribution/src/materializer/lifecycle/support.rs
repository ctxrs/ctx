use crate::core_materialization::producer_authority_disposition;
use crate::core_materialization::{CoreProjectionCoverage, PreparedCoreEventDeltaPage};
use crate::graph::segment_graph::SEGMENT_SCHEMA_IDENTITY;
use crate::graph::segment_state::{
    SegmentCandidateControl, SegmentCoreCoverage, SegmentEventOwner, SegmentPreparedEvent,
    SegmentPreparedEvidence, SegmentPreparedUnit, SegmentPublicationTombstone,
    SegmentSourceFrontier,
};
use crate::protocol::{
    CoreEventDelta, CoreSourceDelta, CoreSourceReconciliation, CoreSourceState, ErrorClass,
};

use super::super::model::{
    CandidateState, MAX_MANIFEST_SEGMENTS, coverage_from, source_order_id, source_storage_id,
};
use super::super::publication::ObservedEvent;
use super::super::{SegmentMaterializer, SegmentMaterializerError};
use super::MAX_INCREMENTAL_FLAT_LAYERS;

pub(super) const COMPACTION_TRIGGER: usize = MAX_MANIFEST_SEGMENTS / 2;

pub(super) fn active_flat_layers_require_rebuild(
    active: &super::super::publication::ActiveGeneration,
) -> bool {
    let mut prior_generation = None;
    let mut layers = 0_usize;
    for reference in active
        .manifest
        .segments
        .iter()
        .filter(|reference| reference.role == crate::graph::segment::FLAT_SERVING_ROLE)
    {
        if prior_generation == Some(reference.publication_generation) {
            continue;
        }
        prior_generation = Some(reference.publication_generation);
        layers = layers.saturating_add(1);
        if layers >= MAX_INCREMENTAL_FLAT_LAYERS {
            return true;
        }
    }
    false
}

pub(super) fn protocol_error(error: crate::protocol::ProtocolError) -> SegmentMaterializerError {
    if error.class == ErrorClass::Bounds {
        SegmentMaterializerError::BoundDetail(error.message)
    } else {
        SegmentMaterializerError::Conflict
    }
}

#[cfg(test)]
#[test]
fn prepared_core_bound_keeps_its_reason() {
    let error = protocol_error(crate::protocol::ProtocolError::new(
        ErrorClass::Bounds,
        "Core prepared unit exceeds its worst-case byte credit (source core_source_abc)",
    ));
    assert_eq!(
        error.to_string(),
        "segment materializer bound exceeded: Core prepared unit exceeds its worst-case byte credit (source core_source_abc)"
    );
}

pub(crate) fn stage_direct_pages(
    direct: &mut super::super::publication::DirectCandidate,
    state: &mut CandidateState,
    pages: Vec<super::super::staging::StagedPage>,
) -> Result<(), SegmentMaterializerError> {
    let page_count = u32::try_from(pages.len()).map_err(|_| SegmentMaterializerError::Bounds)?;
    let next_page_count = state
        .staged_page_count
        .checked_add(page_count)
        .ok_or(SegmentMaterializerError::Bounds)?;
    for (ordinal, page) in pages.iter().enumerate() {
        let expected = state
            .staged_page_count
            .checked_add(u32::try_from(ordinal).map_err(|_| SegmentMaterializerError::Bounds)?)
            .ok_or(SegmentMaterializerError::Bounds)?;
        if page.sequence != expected
            || page.materialization_id != state.control.materialization_id
            || page.graph_generation != state.control.graph_generation
        {
            return Err(SegmentMaterializerError::Conflict);
        }
    }
    direct.stage_pages(&mut state.control, pages)?;
    state.staged_page_count = next_page_count;
    Ok(())
}

impl SegmentMaterializer {
    pub(super) fn refresh_locked(
        &mut self,
        _expected_materializer_revision: Option<&str>,
    ) -> Result<(), SegmentMaterializerError> {
        crate::graph::segment::SegmentStore::new(&self.root).cleanup_candidates()?;
        self.active = super::super::publication::load_active_for_materializer(&self.root)?;

        super::super::publication::cleanup_candidate_orphans(
            &self.root,
            self.active.as_ref().map(|active| &active.manifest),
        )?;
        self.metrics.reconciliation_cursor_entry_count = 0;
        self.metrics.reconciliation_cursor_reserved_bytes = 0;
        self.rollback_cleanup_failed = false;
        Ok(())
    }
}

pub(super) fn abort_session_candidate(
    store: &mut SegmentMaterializer,
    direct: Option<super::super::publication::DirectCandidate>,
) -> Result<(), SegmentMaterializerError> {
    store.acquire_writer_lease()?;
    // A direct publication can have renamed its checked manifest before
    // reporting a directory-sync failure. The manifest on disk is authoritative
    // for cleanup, not the cache that predates this session. If it cannot be
    // checked, return before deleting any candidate-owned segment.
    store.active = super::super::publication::load_active_for_materializer(&store.root)?;
    let direct_result = direct.map_or(Ok(()), super::super::publication::DirectCandidate::abort);
    let orphan_result = super::super::publication::cleanup_candidate_orphans(
        &store.root,
        store.active.as_ref().map(|active| &active.manifest),
    );
    let lease_result = store.verify_writer_lease();
    store.metrics.reconciliation_cursor_entry_count = 0;
    store.metrics.reconciliation_cursor_reserved_bytes = 0;
    direct_result?;
    orphan_result?;
    lease_result?;
    Ok(())
}

pub(super) fn require_candidate<'a>(
    state: &'a CandidateState,
    materialization_id: &str,
    core_generation_id: &str,
    materializer_revision: &str,
) -> Result<&'a SegmentCandidateControl, SegmentMaterializerError> {
    let candidate = &state.control;
    if candidate.materialization_id != materialization_id
        || candidate.head.core_generation_id != core_generation_id
        || candidate.materializer_revision != materializer_revision
        || candidate.schema_contract != SEGMENT_SCHEMA_IDENTITY
    {
        return Err(SegmentMaterializerError::Conflict);
    }
    Ok(candidate)
}

pub(super) fn require_candidate_mut<'a>(
    state: &'a mut CandidateState,
    materialization_id: &str,
    core_generation_id: &str,
    materializer_revision: &str,
) -> Result<&'a mut SegmentCandidateControl, SegmentMaterializerError> {
    let candidate = &mut state.control;
    if candidate.materialization_id != materialization_id
        || candidate.head.core_generation_id != core_generation_id
        || candidate.materializer_revision != materializer_revision
        || candidate.schema_contract != SEGMENT_SCHEMA_IDENTITY
    {
        return Err(SegmentMaterializerError::Conflict);
    }
    Ok(candidate)
}

pub(super) fn require_frontier<'a>(
    candidate: &'a SegmentCandidateControl,
    reconciliation: &CoreSourceReconciliation,
) -> Result<&'a SegmentSourceFrontier, SegmentMaterializerError> {
    let source_id = source_storage_id(reconciliation.delta.source());
    let frontier = candidate
        .pending_sources
        .iter()
        .find(|frontier| frontier.source_id == source_id)
        .ok_or(SegmentMaterializerError::Conflict)?;
    if frontier.source_identity_sha256 != source_order_id(reconciliation.delta.source())
        || frontier.source_descriptor_sha256
            != hex::encode(reconciliation.delta.source().exact_descriptor_digest())
        || frontier.removal != matches!(reconciliation.delta, CoreSourceDelta::Removed(_))
    {
        return Err(SegmentMaterializerError::Conflict);
    }
    if let CoreSourceDelta::Present(state) = &reconciliation.delta
        && frontier.core_record_accumulator != state.core_record_accumulator
    {
        return Err(SegmentMaterializerError::Conflict);
    }
    Ok(frontier)
}

pub(super) fn ensure_current_frontier(
    store: &mut SegmentMaterializer,
    state: &mut CandidateState,
    cursor: &super::super::reconciliation_cursor::ReconciliationCursor,
    requested: &CoreSourceReconciliation,
) -> Result<(), SegmentMaterializerError> {
    if !cursor.owner_matches(
        state,
        store
            .active
            .as_ref()
            .map_or(0, |active| active.sources.len()),
    ) {
        return Err(SegmentMaterializerError::Corrupt(
            "runtime reconciliation cursor owner changed",
        ));
    }
    cursor.require_current(state.next_materialize_index, requested)?;
    let candidate = &state.control;
    if !candidate.pending_sources.is_empty() {
        let _ = require_frontier(candidate, requested)?;
        return Ok(());
    }
    let expected = state.next_materialize_index;
    let total = candidate
        .changed_sources
        .checked_add(candidate.removed_sources)
        .ok_or(SegmentMaterializerError::Bounds)?;
    if !candidate.source_terminal || expected >= total {
        return Err(SegmentMaterializerError::Conflict);
    }
    let selected = requested.delta.clone();
    let source_id = source_storage_id(selected.source());
    let frontier = match selected {
        CoreSourceDelta::Present(state) => {
            let prior_count = store
                .active
                .as_ref()
                .filter(|active| !active.requires_clean_rebuild)
                .and_then(|active| active.sources.get(&source_id))
                .map_or(0, |active| active.state.event_count);
            frontier(&state, false, expected, prior_count)
        }
        CoreSourceDelta::Removed(removal) => {
            let active = store
                .active
                .as_ref()
                .and_then(|active| active.sources.get(&source_id))
                .ok_or(SegmentMaterializerError::Conflict)?;
            if !active.state.source.exact_descriptor_eq(&removal.source) {
                return Err(SegmentMaterializerError::Conflict);
            }
            frontier(&active.state, true, expected, active.state.event_count)
        }
    };
    state.control.pending_sources.push(frontier);
    Ok(())
}

pub(super) fn frontier(
    state: &CoreSourceState,
    removal: bool,
    materialize_index: u32,
    current_event_count: u64,
) -> SegmentSourceFrontier {
    SegmentSourceFrontier {
        source_id: source_storage_id(&state.source),
        source_identity_sha256: source_order_id(&state.source),
        source_descriptor_sha256: hex::encode(state.source.exact_descriptor_digest()),
        core_record_accumulator: state.core_record_accumulator.clone(),
        event_count: current_event_count,
        removal,
        materialize_index,
        next_event_page: 0,
        last_event_order_id: None,
        event_mutations: 0,
    }
}

pub(super) fn prepare_event(
    source_id: &str,
    record: &crate::protocol::CoreRecord,
    prepared: &PreparedCoreEventDeltaPage,
) -> Result<SegmentPreparedEvent, SegmentMaterializerError> {
    let unit = prepared.units.get(&record.event_id.to_string()).ok_or(
        SegmentMaterializerError::Corrupt("prepared event unit is missing"),
    )?;
    if unit.producer_authority_disposition != producer_authority_disposition(record) {
        return Err(SegmentMaterializerError::Corrupt(
            "prepared producer authority is inconsistent",
        ));
    }
    Ok(SegmentPreparedEvent {
        owner: SegmentEventOwner {
            source_id: source_id.to_owned(),
            event_id: record.event_id.to_string(),
            direct_session_id: record.session_id.to_string(),
            root_session_id: record.root_session_id.map(|root| root.to_string()),
            event_sequence: record.event_sequence,
        },
        event_identity: record.event_id,
        core_record_sha256: prepared
            .core_record_sha256(record.event_id)
            .ok_or(SegmentMaterializerError::Corrupt(
                "prepared Core record digest is missing",
            ))?
            .to_owned(),
        core_record_leaf_sha256: prepared
            .core_record_leaf_sha256(record.event_id)
            .ok_or(SegmentMaterializerError::Corrupt(
                "prepared Core record leaf digest is missing",
            ))?
            .to_owned(),
        prepared: SegmentPreparedUnit {
            origin_event_id: unit.origin_event_id.clone(),
            producer_authority_disposition: unit.producer_authority_disposition,
            stable_entities: unit.stable_entities.clone(),
            facts: unit.facts.clone(),
            evidence: unit
                .evidence
                .as_ref()
                .map(|evidence| SegmentPreparedEvidence {
                    citation: evidence.citation.clone(),
                }),
            coverage: coverage_from(&unit.coverage),
        },
    })
}

pub(super) fn validate_prepared(
    prepared: &PreparedCoreEventDeltaPage,
) -> Result<(), SegmentMaterializerError> {
    let records = prepared
        .page()
        .deltas
        .iter()
        .filter_map(CoreEventDelta::record)
        .collect::<Vec<_>>();
    if records.len() != prepared.units.len()
        || records.iter().any(|record| {
            prepared
                .units
                .get(&record.event_id.to_string())
                .is_none_or(|unit| {
                    unit.origin_event_id != record.event_id.to_string()
                        || unit.producer_authority_disposition
                            != producer_authority_disposition(record)
                })
        })
    {
        return Err(SegmentMaterializerError::Corrupt(
            "prepared event page is inconsistent",
        ));
    }
    Ok(())
}

pub(super) fn require_prior(
    prior: Option<ObservedEvent>,
    expected_sha256: &str,
) -> Result<ObservedEvent, SegmentMaterializerError> {
    let prior = prior.ok_or(SegmentMaterializerError::Conflict)?;
    if prior.core_record_sha256 != expected_sha256 {
        return Err(SegmentMaterializerError::Conflict);
    }
    Ok(prior)
}

pub(super) fn publication_tombstone(prior: &ObservedEvent) -> SegmentPublicationTombstone {
    SegmentPublicationTombstone {
        owner: SegmentEventOwner {
            source_id: prior.source_id.clone(),
            event_id: prior.event_id.to_string(),
            direct_session_id: prior.direct_session_id.to_string(),
            root_session_id: prior.root_session_id.as_ref().map(ToString::to_string),
            event_sequence: prior.event_sequence,
        },
        prior_core_record_sha256: prior.core_record_sha256.clone(),
        prior_event_state_sha256: prior.event_output_root.clone(),
    }
}

pub(super) fn source_state_exact_eq(left: &CoreSourceState, right: &CoreSourceState) -> bool {
    left.source.exact_descriptor_eq(&right.source)
        && left.core_record_accumulator == right.core_record_accumulator
        && left.event_count == right.event_count
}

pub(super) fn protocol_coverage(value: &SegmentCoreCoverage) -> CoreProjectionCoverage {
    CoreProjectionCoverage {
        repository_candidate_events: value.repository_candidate_events,
        logical_binding_events: value.logical_binding_events,
        certified_live_root_access_events: value.certified_live_root_access_events,
        file_evidence_events: value.file_evidence_events,
        exact_commit_evidence_events: value.exact_commit_evidence_events,
        exact_pull_request_evidence_events: value.exact_pull_request_evidence_events,
    }
}

pub(super) fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
