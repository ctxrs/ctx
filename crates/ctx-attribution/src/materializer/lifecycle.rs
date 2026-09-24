//! One concrete, materializer-owned Core generation session.
use super::StatusRequest;

#[path = "lifecycle/support.rs"]
pub(crate) mod support;

#[path = "lifecycle/completion.rs"]
mod completion;
#[path = "lifecycle/currentness.rs"]
mod currentness;
#[path = "lifecycle/validation.rs"]
pub(crate) mod validation;

/// Flat readers consult every immutable publication layer. Rebuild the next
/// snapshot once incremental publication reaches this conservative query-cost
/// ceiling; one rebuild emits one replacement layer without leveled state.
pub(crate) const MAX_INCREMENTAL_FLAT_LAYERS: usize = 16;

use crate::core_materialization::{
    CoreProjectionAvailability, CoreProjectionStatus, PreparedCoreEventDeltaPage,
};
use crate::graph::segment_state::{
    SegmentCompletedControl, SegmentCoreCoverage, SegmentCorePageOutput, SegmentPublicationMutation,
};
use crate::protocol::{
    CoreEventDelta, CoreEventDeltaPage, CoreEventDeltaPageApplied, CoreEventState,
    CoreGenerationHead, CoreMaterializationReceipt, CoreMaterializationReceiptIdentity,
    CoreProjectionCurrentness, CoreSourceDelta, CoreSourceDeltaPage, CoreSourceReconciliation,
    CoreSourceRemoval, MaterializedCoverage,
};

use super::model::{
    add_coverage, canonical_sha256, event_order_id, mutation_identities, source_order_id,
    source_storage_id, subtract_coverage,
};
use super::{SegmentMaterializer, SegmentMaterializerError};
use completion::*;
use support::*;

pub(super) struct ValidatedEventPage {
    pub(super) output: SegmentCorePageOutput,
    pub(super) event_identities: Vec<crate::protocol::StableEntityId>,
    pub(super) projection_source: crate::protocol::CoreSourceState,
}

#[expect(
    clippy::large_enum_variant,
    reason = "Beginning a generation transfers one materializer-owned session by value; retain its allocation and ownership contract."
)]
pub enum CoreGenerationStart<'a> {
    Current(CoreMaterializationReceipt),
    Started(CoreMaterializationSession<'a>),
}

pub struct CoreMaterializationSession<'a> {
    materializer: &'a mut SegmentMaterializer,
    candidate: Option<super::model::CandidateState>,
    direct_candidate: Option<super::publication::DirectCandidate>,
    reconciliation_cursor: super::reconciliation_cursor::ReconciliationCursor,
    materialization_id: String,
    head: CoreGenerationHead,
    prior_identity: Option<CoreMaterializationReceiptIdentity>,
    source_delta_pages: u32,
    changed_sources: u32,
    removed_sources: u32,
    event_delta_pages: u32,
    event_mutations: u64,
    preparer: crate::core_materialization::CoreProjectionPreparer,
    completed: bool,
}

#[derive(serde::Serialize)]
struct BeginGenerationIntegrity<'a> {
    head: &'a CoreGenerationHead,
    expected_prior_receipt: &'a Option<CoreMaterializationReceiptIdentity>,
}

#[derive(serde::Serialize)]
struct FinishGenerationIntegrity<'a> {
    materialization_id: &'a str,
    head: &'a CoreGenerationHead,
    expected_prior_receipt: &'a Option<CoreMaterializationReceiptIdentity>,
    source_delta_pages: u32,
    changed_sources: u32,
    removed_sources: u32,
    event_delta_pages: u32,
    event_mutations: u64,
}

impl CoreMaterializationSession<'_> {
    pub(crate) fn start_progress(&mut self, total_sources: u32) {
        if let Some(lock) = self.materializer.writer_lease.as_mut() {
            lock.update_progress(
                |progress| {
                    progress.phase = super::MaterializationPhase::Indexing;
                    progress.core_generation_id = Some(self.head.core_generation_id.clone());
                    progress.total_sources = Some(total_sources);
                    progress.completed_sources = Some(0);
                    progress.applied_changes = Some(0);
                },
                true,
            );
        }
    }

    #[must_use]
    pub fn materialization_id(&self) -> &str {
        &self.materialization_id
    }

    /// Reconciles one typed source page, including any internally paged
    /// deletions discovered after Core's terminal source page.
    pub fn reconcile_source_page(
        &mut self,
        mut page: CoreSourceDeltaPage,
    ) -> Result<Vec<CoreSourceReconciliation>, SegmentMaterializerError> {
        page.materialization_id.clone_from(&self.materialization_id);
        page.core_generation_id
            .clone_from(&self.head.core_generation_id);
        let mut acknowledgement_page_index = 0;
        let mut reconciliations = Vec::new();
        loop {
            let (page_reconciliations, acknowledgement_terminal, changed, removed) =
                Self::reconcile_source_page_inner(
                    self.materializer,
                    self.candidate
                        .as_mut()
                        .ok_or(SegmentMaterializerError::Conflict)?,
                    self.direct_candidate
                        .as_mut()
                        .ok_or(SegmentMaterializerError::Conflict)?,
                    &mut self.reconciliation_cursor,
                    &page,
                    acknowledgement_page_index,
                    crate::core_materialization::CORE_MATERIALIZER_REVISION,
                )?;
            self.changed_sources = self
                .changed_sources
                .checked_add(changed)
                .ok_or(SegmentMaterializerError::Bounds)?;
            self.removed_sources = self
                .removed_sources
                .checked_add(removed)
                .ok_or(SegmentMaterializerError::Bounds)?;
            reconciliations.extend(page_reconciliations);
            if acknowledgement_terminal {
                self.source_delta_pages = self
                    .source_delta_pages
                    .checked_add(1)
                    .ok_or(SegmentMaterializerError::Bounds)?;
                return Ok(reconciliations);
            }
            acknowledgement_page_index = acknowledgement_page_index
                .checked_add(1)
                .ok_or(SegmentMaterializerError::Bounds)?;
        }
    }

    pub fn event_states(
        &mut self,
        reconciliation: &CoreSourceReconciliation,
        after_event_id: Option<crate::protocol::StableEntityId>,
    ) -> Result<(Vec<CoreEventState>, bool), SegmentMaterializerError> {
        Self::read_event_states(
            self.materializer,
            self.candidate
                .as_ref()
                .ok_or(SegmentMaterializerError::Conflict)?,
            &self.reconciliation_cursor,
            &self.materialization_id,
            &self.head.core_generation_id,
            reconciliation,
            after_event_id,
            crate::core_materialization::CORE_MATERIALIZER_REVISION,
        )
    }

    pub fn ingest_event_pages(
        &mut self,
        mut pages: Vec<CoreEventDeltaPage>,
    ) -> Result<(), SegmentMaterializerError> {
        for page in &mut pages {
            page.materialization_id.clone_from(&self.materialization_id);
            page.core_generation_id
                .clone_from(&self.head.core_generation_id);
        }
        let completed_sources = pages.iter().filter(|page| page.terminal).count() as u32;
        let page_count =
            u32::try_from(pages.len()).map_err(|_| SegmentMaterializerError::Bounds)?;
        let mutations = pages.iter().try_fold(0_u64, |total, page| {
            total
                .checked_add(
                    u64::try_from(page.deltas.len())
                        .map_err(|_| SegmentMaterializerError::Bounds)?,
                )
                .ok_or(SegmentMaterializerError::Bounds)
        })?;
        // Each page has its own bounded prepared output. A count-bounded batch
        // can otherwise exceed the aggregate byte limit before the writer sees
        // any page, even though every page is individually valid.
        let result = pages.into_iter().try_for_each(|page| {
            let prepared = self
                .preparer
                .prepare_event_delta_page(page)
                .map_err(protocol_error)?;
            super::staging::apply_event_pages(
                self.materializer,
                self.candidate
                    .as_mut()
                    .ok_or(SegmentMaterializerError::Conflict)?,
                self.direct_candidate
                    .as_mut()
                    .ok_or(SegmentMaterializerError::Conflict)?,
                &self.reconciliation_cursor,
                &self.preparer,
                &[prepared],
                crate::core_materialization::CORE_MATERIALIZER_REVISION,
            )
        });
        if let Err(error) = result {
            if let Some(direct) = self.direct_candidate.take() {
                self.materializer.rollback_cleanup_failed = direct.abort().is_err();
            }
            return Err(error);
        }
        self.event_delta_pages = self
            .event_delta_pages
            .checked_add(page_count)
            .ok_or(SegmentMaterializerError::Bounds)?;
        self.event_mutations = self
            .event_mutations
            .checked_add(mutations)
            .ok_or(SegmentMaterializerError::Bounds)?;
        if let Some(lock) = self.materializer.writer_lease.as_mut() {
            lock.update_progress(
                |progress| {
                    progress.completed_sources = Some(
                        progress
                            .completed_sources
                            .unwrap_or_default()
                            .saturating_add(completed_sources),
                    );
                    progress.applied_changes = Some(self.event_mutations);
                },
                false,
            );
        }
        Ok(())
    }

    pub fn activate(self) -> Result<CoreMaterializationReceipt, SegmentMaterializerError> {
        self.activate_cancellable(None)
    }

    pub(crate) fn activate_cancellable(
        mut self,
        cancelled: Option<&(dyn Fn() -> bool + Sync)>,
    ) -> Result<CoreMaterializationReceipt, SegmentMaterializerError> {
        if cancelled.is_some_and(|check| check()) {
            return Err(SegmentMaterializerError::Cancelled);
        }
        if let Some(lock) = self.materializer.writer_lease.as_mut() {
            lock.update_progress(
                |progress| progress.phase = super::MaterializationPhase::Publishing,
                true,
            );
        }
        let _finish_phase = self
            .preparer
            .finish_measurement(&self.materialization_id)
            .map_err(protocol_error)?;
        let revision = crate::core_materialization::CORE_MATERIALIZER_REVISION;
        let candidate = require_candidate(
            self.candidate
                .as_ref()
                .ok_or(SegmentMaterializerError::Conflict)?,
            &self.materialization_id,
            &self.head.core_generation_id,
            revision,
        )?;
        if candidate.head != self.head
            || candidate.expected_prior_receipt != self.prior_identity
            || candidate.next_source_page != self.source_delta_pages
            || candidate.changed_sources != self.changed_sources
            || candidate.removed_sources != self.removed_sources
            || candidate.event_pages != self.event_delta_pages
            || candidate.event_mutations != self.event_mutations
            || candidate.event_count != self.head.event_count
            || (self.head.source_count > 0 && !candidate.source_terminal)
            || self
                .candidate
                .as_ref()
                .ok_or(SegmentMaterializerError::Conflict)?
                .next_materialize_index
                != candidate
                    .changed_sources
                    .saturating_add(candidate.removed_sources)
            || !candidate.pending_sources.is_empty()
        {
            return Err(SegmentMaterializerError::Conflict);
        }
        let finish_request_sha256 = canonical_sha256(&FinishGenerationIntegrity {
            materialization_id: &self.materialization_id,
            head: &self.head,
            expected_prior_receipt: &self.prior_identity,
            source_delta_pages: candidate.next_source_page,
            changed_sources: candidate.changed_sources,
            removed_sources: candidate.removed_sources,
            event_delta_pages: candidate.event_pages,
            event_mutations: candidate.event_mutations,
        })?;
        let receipt = CoreMaterializationReceipt {
            core_generation_id: self.head.core_generation_id.clone(),
            core_record_contract_fingerprint: self.head.core_record_contract_fingerprint.clone(),
            source_snapshot_sha256: self.head.source_snapshot_sha256.clone(),
            materializer_revision: revision.to_owned(),
            source_count: self.head.source_count,
            event_count: self.head.event_count,
        };
        let completed = completed_control(candidate, receipt.clone(), finish_request_sha256);
        let shape = super::publication_plan::direct_reference_shape(
            candidate,
            &candidate.publication_reference_plan,
            self.materializer.active.as_ref(),
        )?;
        if !shape.fits_manifest()? {
            let no_smaller_publication_mode =
                candidate.force_projection_rebuild || shape.retained_references()? == 0;
            self.materializer.force_next_rebuild = !no_smaller_publication_mode;
            return Err(if no_smaller_publication_mode {
                SegmentMaterializerError::Bounds
            } else {
                SegmentMaterializerError::RebuildRequired
            });
        }
        let previous_manifest = self
            .materializer
            .active
            .as_ref()
            .map(|active| active.manifest.clone());
        let direct = self
            .direct_candidate
            .take()
            .ok_or(SegmentMaterializerError::Corrupt(
                "direct publication candidate is missing",
            ))?;
        let manifest = direct.finish(
            &self.materializer.root,
            candidate,
            self.materializer.active.as_ref(),
            completed,
            receipt.clone(),
            &mut self.materializer.metrics,
            cancelled,
        )?;
        self.materializer.active = super::publication::load_active(&self.materializer.root)?;
        if self
            .materializer
            .active
            .as_ref()
            .is_none_or(|active| active.manifest.generation_id != manifest.generation_id)
        {
            return Err(SegmentMaterializerError::Corrupt(
                "published manifest did not reopen",
            ));
        }
        super::publication::remove_unreferenced_segments(
            &self.materializer.root,
            &manifest,
            previous_manifest.as_ref(),
        )?;
        self.candidate = None;
        self.materializer.metrics.reconciliation_cursor_entry_count = 0;
        self.materializer
            .metrics
            .reconciliation_cursor_reserved_bytes = 0;
        self.completed = true;
        if let Some(lock) = self.materializer.writer_lease.as_mut() {
            lock.update_progress(
                |progress| progress.phase = super::MaterializationPhase::Complete,
                true,
            );
        }
        Ok(receipt)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Validate the requested generation and revision against the candidate and cursor without combining their authorities."
    )]
    fn read_event_states(
        store: &mut SegmentMaterializer,
        state: &super::model::CandidateState,
        cursor: &super::reconciliation_cursor::ReconciliationCursor,
        materialization_id: &str,
        core_generation_id: &str,
        reconciliation: &CoreSourceReconciliation,
        after_event_id: Option<crate::protocol::StableEntityId>,
        materializer_revision: &str,
    ) -> Result<(Vec<CoreEventState>, bool), SegmentMaterializerError> {
        let source_id = source_storage_id(reconciliation.delta.source());
        let candidate = require_candidate(
            state,
            materialization_id,
            core_generation_id,
            materializer_revision,
        )?;
        if !candidate.source_terminal {
            return Err(SegmentMaterializerError::Conflict);
        }
        let force = candidate.force_projection_rebuild;
        // Event-state reads may run ahead across certified sources while the
        // caller builds one bounded cross-source batch. Writes remain ordered
        // by event ingestion; this read only proves source selection.
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
        cursor.require_committed(reconciliation)?;
        let maximum = ctx_attribution_index::MAX_EVENT_INDEX_PAGE_ITEMS;
        let (states, terminal) = if store
            .active
            .as_ref()
            .is_some_and(|active| active.sources.contains_key(&source_id))
        {
            let (states, _observed, terminal) = super::publication::active_event_page(
                store
                    .active
                    .as_mut()
                    .ok_or(SegmentMaterializerError::Corrupt(
                        "active cache disappeared",
                    ))?,
                &store.root,
                reconciliation.delta.source(),
                after_event_id,
                maximum,
                force,
            )?;
            (states, terminal)
        } else {
            (Vec::new(), true)
        };
        Ok((states, terminal))
    }

    fn reconcile_source_page_inner(
        store: &mut SegmentMaterializer,
        state: &mut super::model::CandidateState,
        direct: &mut super::publication::DirectCandidate,
        cursor: &mut super::reconciliation_cursor::ReconciliationCursor,
        page: &CoreSourceDeltaPage,
        acknowledgement_page_index: u32,
        materializer_revision: &str,
    ) -> Result<(Vec<CoreSourceReconciliation>, bool, u32, u32), SegmentMaterializerError> {
        page.validate().map_err(protocol_error)?;
        let active_sources = store.active.as_ref().map(|active| &active.sources);
        let candidate = require_candidate_mut(
            state,
            &page.materialization_id,
            &page.core_generation_id,
            materializer_revision,
        )?;
        let total_reconciliations = candidate
            .changed_sources
            .checked_add(candidate.removed_sources)
            .ok_or(SegmentMaterializerError::Bounds)?;
        if candidate.next_source_page != page.page_index
            || candidate.next_source_acknowledgement_page != acknowledgement_page_index
            || candidate.source_terminal
            || (acknowledgement_page_index != 0 && !page.terminal)
        {
            return Err(SegmentMaterializerError::Conflict);
        }
        let mut materialize_index = total_reconciliations;
        let mut changed = 0_u32;
        let mut removed = 0_u32;
        let mut reconciliations = Vec::new();
        if acknowledgement_page_index == 0 {
            if let Some(first) = page.deltas.first() {
                let first_order = source_order_id(first.source());
                if candidate
                    .last_source_order_id
                    .as_deref()
                    .is_some_and(|prior| prior >= first_order.as_str())
                {
                    return Err(SegmentMaterializerError::Conflict);
                }
            }
            for delta in &page.deltas {
                let CoreSourceDelta::Present(state) = delta else {
                    return Err(SegmentMaterializerError::Conflict);
                };
                let source_id = source_storage_id(&state.source);
                candidate.seen_source_ids.push(source_id.clone());
                if candidate.seen_source_ids.len() > crate::protocol::MAX_CORE_SOURCE_STATES {
                    return Err(SegmentMaterializerError::Bounds);
                }
                let existing = active_sources.and_then(|sources| sources.get(&source_id));
                let needs_change = existing.is_none_or(|active| {
                    !source_state_exact_eq(&active.state, state)
                        || active.materializer_revision != materializer_revision
                }) || candidate.force_projection_rebuild;
                if !needs_change {
                    continue;
                }
                changed = changed
                    .checked_add(1)
                    .ok_or(SegmentMaterializerError::Bounds)?;
                reconciliations.push(CoreSourceReconciliation {
                    materialize_index,
                    delta: delta.clone(),
                });
                materialize_index = materialize_index
                    .checked_add(1)
                    .ok_or(SegmentMaterializerError::Bounds)?;
            }
            candidate.last_source_order_id = page
                .deltas
                .last()
                .map(|delta| source_order_id(delta.source()))
                .or_else(|| candidate.last_source_order_id.clone());
        }
        let unseen_sources = active_sources
            .into_iter()
            .flat_map(|sources| sources.values())
            .filter(|active| {
                candidate
                    .seen_source_ids
                    .binary_search(&source_storage_id(&active.state.source))
                    .is_err()
            })
            .collect::<Vec<_>>();
        let already_removed = usize::try_from(candidate.removed_sources)
            .map_err(|_| SegmentMaterializerError::Bounds)?;
        let remaining_items = crate::protocol::MAX_CORE_SOURCE_DELTA_PAGE_ITEMS
            .checked_sub(reconciliations.len())
            .ok_or(SegmentMaterializerError::Bounds)?;
        if page.terminal {
            for active in unseen_sources
                .iter()
                .skip(already_removed)
                .take(remaining_items)
            {
                reconciliations.push(CoreSourceReconciliation {
                    materialize_index,
                    delta: CoreSourceDelta::Removed(CoreSourceRemoval {
                        source: active.state.source.clone(),
                    }),
                });
                materialize_index = materialize_index
                    .checked_add(1)
                    .ok_or(SegmentMaterializerError::Bounds)?;
                removed = removed
                    .checked_add(1)
                    .ok_or(SegmentMaterializerError::Bounds)?;
            }
        }
        let acknowledgement_terminal = !page.terminal
            || already_removed
                .checked_add(
                    usize::try_from(removed).map_err(|_| SegmentMaterializerError::Bounds)?,
                )
                .is_some_and(|count| count == unseen_sources.len());
        candidate.changed_sources = candidate
            .changed_sources
            .checked_add(changed)
            .ok_or(SegmentMaterializerError::Bounds)?;
        candidate.removed_sources = candidate
            .removed_sources
            .checked_add(removed)
            .ok_or(SegmentMaterializerError::Bounds)?;
        if acknowledgement_terminal {
            candidate.next_source_page = candidate
                .next_source_page
                .checked_add(1)
                .ok_or(SegmentMaterializerError::Bounds)?;
            candidate.next_source_acknowledgement_page = 0;
            candidate.source_terminal = page.terminal;
        } else {
            candidate.next_source_acknowledgement_page = candidate
                .next_source_acknowledgement_page
                .checked_add(1)
                .ok_or(SegmentMaterializerError::Bounds)?;
        }
        direct.apply_source_reconciliations(&reconciliations);
        let cursor_append = cursor.prepare_append(&reconciliations)?;
        cursor.commit_append(cursor_append)?;
        store.metrics.reconciliation_cursor_entry_count =
            u64::try_from(cursor.entry_count()).map_err(|_| SegmentMaterializerError::Bounds)?;
        Ok((reconciliations, acknowledgement_terminal, changed, removed))
    }
}

impl Drop for CoreMaterializationSession<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.preparer.abort_measurement(&self.materialization_id);
            if self.candidate.is_some() {
                let direct = self.direct_candidate.take();
                self.materializer.rollback_cleanup_failed =
                    abort_session_candidate(self.materializer, direct).is_err();
            }
        }
    }
}

fn validate_status_request(request: &StatusRequest) -> Result<(), SegmentMaterializerError> {
    if request
        .requested_core_generation_id
        .as_deref()
        .is_some_and(|generation| !is_lower_sha256(generation))
    {
        Err(SegmentMaterializerError::Conflict)
    } else {
        Ok(())
    }
}

pub(super) fn validate_prepared_request(
    prepared: &PreparedCoreEventDeltaPage,
) -> Result<(), SegmentMaterializerError> {
    prepared.validate_cached_integrity().map_err(|error| {
        if error.class == crate::protocol::ErrorClass::Internal {
            SegmentMaterializerError::Corrupt("prepared Core event page metadata is invalid")
        } else {
            protocol_error(error)
        }
    })?;
    validate_prepared(prepared)
}

fn projection_status(
    active: &SegmentCompletedControl,
    candidate_active: bool,
    publication_rebuild_required: bool,
    request: &StatusRequest,
) -> Result<CoreProjectionStatus, SegmentMaterializerError> {
    validate_status_request(request)?;
    let currentness = if candidate_active {
        CoreProjectionCurrentness::Partial
    } else if publication_rebuild_required
        || completed_requires_rebuild(
            active,
            crate::core_materialization::CORE_MATERIALIZER_REVISION,
        )
    {
        CoreProjectionCurrentness::NeedsRebuild
    } else if active.receipt.is_none() {
        CoreProjectionCurrentness::NotMaterialized
    } else if request
        .requested_core_generation_id
        .as_deref()
        .zip(active.receipt.as_ref())
        .is_some_and(|(requested, receipt)| requested != receipt.core_generation_id)
    {
        CoreProjectionCurrentness::Stale
    } else {
        CoreProjectionCurrentness::Current
    };
    let coverage = if currentness == CoreProjectionCurrentness::Partial {
        SegmentCoreCoverage::default()
    } else {
        active.coverage.clone()
    };
    let materialized_coverage = match currentness {
        CoreProjectionCurrentness::NotMaterialized => MaterializedCoverage::NotMaterialized,
        CoreProjectionCurrentness::Partial
        | CoreProjectionCurrentness::Stale
        | CoreProjectionCurrentness::NeedsRebuild => MaterializedCoverage::Partial,
        CoreProjectionCurrentness::Current if active.event_count == 0 => {
            MaterializedCoverage::Empty
        }
        CoreProjectionCurrentness::Current if coverage.logical_binding_events == 0 => {
            MaterializedCoverage::Abstained
        }
        CoreProjectionCurrentness::Current => MaterializedCoverage::Complete,
    };
    let (local_repository_access, availability) =
        projection_availability(currentness, materialized_coverage, &coverage);
    Ok(CoreProjectionStatus {
        diagnostic: crate::diagnostic::projection_diagnostic(currentness),
        currentness,
        requested_core_generation_id: request.requested_core_generation_id.clone(),
        receipt: active.receipt.clone(),
        materialized_coverage,
        coverage: protocol_coverage(&coverage),
        local_repository_access,
        availability,
    })
}

fn projection_availability(
    currentness: CoreProjectionCurrentness,
    materialized_coverage: MaterializedCoverage,
    coverage: &SegmentCoreCoverage,
) -> (bool, CoreProjectionAvailability) {
    let blame_ready = currentness == CoreProjectionCurrentness::Current
        && materialized_coverage == MaterializedCoverage::Complete;
    let local_repository_access = blame_ready && coverage.certified_live_root_access_events > 0;
    (
        local_repository_access,
        CoreProjectionAvailability {
            file_blame: local_repository_access
                && coverage.file_evidence_events > 0
                && coverage.exact_commit_evidence_events > 0,
            commit_blame: blame_ready && coverage.exact_commit_evidence_events > 0,
            pull_request_blame: blame_ready && coverage.exact_pull_request_evidence_events > 0,
        },
    )
}

impl SegmentMaterializer {
    pub fn start_core_generation(
        &mut self,
        head: CoreGenerationHead,
    ) -> Result<CoreGenerationStart<'_>, SegmentMaterializerError> {
        self.acquire_writer_lease()?;
        if self.rollback_cleanup_failed {
            let expected_revision = self.expected_materializer_revision.clone();
            self.refresh_locked(expected_revision.as_deref())?;
        }
        head.validate().map_err(protocol_error)?;
        crate::core_materialization::validate_core_generation_head(&head)
            .map_err(protocol_error)?;
        let revision = crate::core_materialization::CORE_MATERIALIZER_REVISION;
        let initial_active = super::model::initial_completed_control();
        let active = self
            .active
            .as_ref()
            .map(|generation| &generation.completed)
            .unwrap_or(&initial_active);
        let prior_identity = super::model::active_receipt_identity(&active.receipt);
        if active.receipt.as_ref().is_some_and(|receipt| {
            !completed_requires_rebuild(active, revision)
                && receipt.materializer_revision == revision
                && receipt.validate_for_head(&head).is_ok()
        }) {
            return active
                .receipt
                .clone()
                .map(CoreGenerationStart::Current)
                .ok_or(SegmentMaterializerError::Corrupt(
                    "current Core generation receipt is missing",
                ));
        }
        let materialization_id = canonical_sha256(&(
            BeginGenerationIntegrity {
                head: &head,
                expected_prior_receipt: &prior_identity,
            },
            revision,
        ))?;
        let requires_clean_rebuild = self
            .active
            .as_ref()
            .is_some_and(|active| active.requires_clean_rebuild);
        let graph_generation = active
            .graph_generation
            .checked_add(1)
            .ok_or(SegmentMaterializerError::Bounds)?;
        let compact = self.active.as_ref().is_some_and(|generation| {
            generation.manifest.segments.len() >= COMPACTION_TRIGGER
                || active_flat_layers_require_rebuild(generation)
        });
        let force_projection_rebuild = completed_requires_rebuild(active, revision)
            || requires_clean_rebuild
            || compact
            || self.force_next_rebuild;
        let candidate = super::model::CandidateState {
            control: crate::graph::segment_state::SegmentCandidateControl {
                materialization_id: materialization_id.clone(),
                head: head.clone(),
                expected_prior_receipt: prior_identity.clone(),
                materializer_revision: revision.to_owned(),
                schema_contract: crate::graph::segment_graph::SEGMENT_SCHEMA_IDENTITY.to_owned(),
                graph_generation,
                force_projection_rebuild,
                next_source_page: 0,
                next_source_acknowledgement_page: 0,
                source_terminal: false,
                last_source_order_id: None,
                seen_source_ids: Vec::new(),
                changed_sources: 0,
                removed_sources: 0,
                event_pages: 0,
                event_mutations: 0,
                event_count: if requires_clean_rebuild {
                    0
                } else {
                    active.event_count
                },
                pending_sources: Vec::new(),
                coverage: if requires_clean_rebuild {
                    Default::default()
                } else {
                    active.coverage.clone()
                },
                publication_semantics_sha256: if force_projection_rebuild {
                    crate::graph::segment_state::EMPTY_PUBLICATION_SEMANTICS_SHA256.to_owned()
                } else {
                    active.publication_semantics_sha256.clone()
                },
                publication_reference_plan: Default::default(),
            },
            next_materialize_index: 0,
            staged_page_count: 0,
        };
        self.force_next_rebuild = false;
        let reconciliation_cursor = super::reconciliation_cursor::ReconciliationCursor::empty_for(
            &candidate,
            self.active
                .as_ref()
                .map_or(0, |active| active.sources.len()),
        )?;
        self.metrics.reconciliation_cursor_entry_count =
            u64::try_from(reconciliation_cursor.entry_count())
                .map_err(|_| SegmentMaterializerError::Bounds)?;
        self.metrics.reconciliation_cursor_reserved_bytes =
            u64::try_from(reconciliation_cursor.reserved_bytes()?)
                .map_err(|_| SegmentMaterializerError::Bounds)?;
        let preparer =
            crate::core_materialization::CoreProjectionPreparer::new().map_err(protocol_error)?;
        if let Err(error) = preparer.start_measurement(materialization_id.clone()) {
            self.metrics.reconciliation_cursor_entry_count = 0;
            self.metrics.reconciliation_cursor_reserved_bytes = 0;
            return Err(protocol_error(error));
        }
        let direct_candidate = match super::publication::DirectCandidate::new(
            &self.root,
            &candidate.control,
            self.active.as_ref(),
        ) {
            Ok(candidate) => candidate,
            Err(error) => {
                preparer.abort_measurement(&materialization_id);
                self.rollback_cleanup_failed = abort_session_candidate(self, None).is_err();
                return Err(error);
            }
        };
        Ok(CoreGenerationStart::Started(CoreMaterializationSession {
            materializer: self,
            candidate: Some(candidate),
            direct_candidate: Some(direct_candidate),
            reconciliation_cursor,
            materialization_id,
            head,
            prior_identity,
            source_delta_pages: 0,
            changed_sources: 0,
            removed_sources: 0,
            event_delta_pages: 0,
            event_mutations: 0,
            preparer,
            completed: false,
        }))
    }
}

#[cfg(test)]
#[path = "lifecycle_availability_tests.rs"]
mod availability_tests;
