use std::num::NonZeroUsize;

use ctx_history_core::SyncCursor;
use ctx_history_store::Store;

use crate::captured_batch::{
    CapturedBatch, NativePosition, CAPTURE_BATCH_MAX_BATCHES_PER_GROUP,
    CAPTURE_BATCH_MAX_PAYLOAD_BYTES,
};
use crate::{CaptureError, NormalizedProviderImportOptions, ProviderImportSummary, Result};

use super::super::cursors::captured_batch_cursor_stream;
use super::admission::CapturedSourceAdmission;
use super::contracts::{
    CapturedBatchCursorMode, CapturedBatchImportOutcome, CapturedBatchProjector,
};
use super::frontier::{initialize_captured_batch_frontier, CapturedBatchFrontier};
use super::projection::{project_captured_batch_inner, ProjectedCapturedBatch};
use super::write_tx::ProviderImportTransaction;

#[derive(Clone, Copy, PartialEq, Eq)]
enum CapturedBatchSequenceMode {
    OneSafeGroup,
    Drain,
}

struct ProjectedBatchGroup {
    summary: ProviderImportSummary,
    batches: usize,
    source_exhausted: bool,
    retained_final: CapturedBatch,
}

/// Sole owner of captured-batch import sequencing.
///
/// Providers supply bounded batches and source revalidation. This coordinator orders canonical
/// projection commits, FTS maintenance completion, source revalidation, and frontier publication.
struct CapturedImportRun<'a, Projector> {
    store: &'a mut Store,
    admission: &'a CapturedSourceAdmission,
    options: NormalizedProviderImportOptions,
    machine_id: &'a str,
    observed_at: chrono::DateTime<chrono::Utc>,
    projector: &'a mut Projector,
}

impl<'a, Projector> CapturedImportRun<'a, Projector>
where
    Projector: CapturedBatchProjector,
{
    fn new(
        store: &'a mut Store,
        admission: &'a CapturedSourceAdmission,
        options: NormalizedProviderImportOptions,
        machine_id: &'a str,
        observed_at: chrono::DateTime<chrono::Utc>,
        projector: &'a mut Projector,
    ) -> Self {
        Self {
            store,
            admission,
            options,
            machine_id,
            observed_at,
            projector,
        }
    }

    fn initialize_frontier(
        &self,
        expected_store_cursor: Option<&SyncCursor>,
        initial_native_position: &NativePosition,
        cursor_mode: CapturedBatchCursorMode,
    ) -> Result<CapturedBatchFrontier> {
        initialize_captured_batch_frontier(
            self.admission,
            self.machine_id,
            self.observed_at,
            expected_store_cursor,
            initial_native_position,
            cursor_mode,
            self.projector,
        )
    }

    fn with_event_search_bulk_mode<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        let guard = self.store.begin_event_search_bulk_mode()?;
        let operation_result = operation(self);
        let finish_result = self
            .store
            .finish_event_search_bulk_mode(&guard)
            .map_err(CaptureError::from);
        match (operation_result, finish_result) {
            (Ok(value), Ok(())) => Ok(value),
            (_, Err(error)) => Err(error),
            (Err(error), Ok(())) => Err(error),
        }
    }

    fn project_batch(
        &mut self,
        batch: &CapturedBatch,
        rejected_records_before_batch: u64,
    ) -> Result<ProjectedCapturedBatch> {
        let mut transaction = ProviderImportTransaction::begin_projection(self.store)?;
        let projected = project_captured_batch_inner(
            self.store,
            batch,
            &self.options,
            self.machine_id,
            self.observed_at,
            &mut transaction,
            self.projector,
            self.admission,
            rejected_records_before_batch,
        );
        match projected {
            Ok(projected) => {
                transaction.commit(self.store)?;
                Ok(projected)
            }
            Err(error) => {
                transaction.rollback(self.store);
                Err(error)
            }
        }
    }

    #[cfg(test)]
    fn import_one<Revalidate>(
        &mut self,
        batch: &CapturedBatch,
        expected_store_cursor: Option<&SyncCursor>,
        initial_native_position: &NativePosition,
        cursor_mode: CapturedBatchCursorMode,
        revalidate_source: Revalidate,
    ) -> Result<CapturedBatchImportOutcome>
    where
        Revalidate: FnOnce() -> Result<bool>,
    {
        self.admission.require_current_inventory_observation()?;
        let mut frontier =
            self.initialize_frontier(expected_store_cursor, initial_native_position, cursor_mode)?;
        frontier.validate_next_batch(self.admission, batch)?;
        let rejected_records_before_batch = frontier.cumulative_rejected_records()?;
        let projected = self.with_event_search_bulk_mode(|run| {
            run.project_batch(batch, rejected_records_before_batch)
        })?;
        frontier.apply_projected_batch(batch, &projected)?;
        let provider_source_is_current = revalidate_source()?;
        frontier.publish_candidate_after_revalidation(
            self.store,
            self.admission,
            provider_source_is_current,
        )?;
        Ok(CapturedBatchImportOutcome {
            summary: projected.summary,
            batches_imported: 1,
            source_exhausted: batch.source_exhausted(),
            cursor_safe: frontier.is_safe(self.admission),
        })
    }

    fn project_group<NextBatch>(
        &mut self,
        frontier: &mut CapturedBatchFrontier,
        batch: &mut Option<CapturedBatch>,
        max_batches: NonZeroUsize,
        recovery_group: bool,
        next_batch: &mut NextBatch,
    ) -> Result<ProjectedBatchGroup>
    where
        NextBatch: FnMut() -> Result<Option<CapturedBatch>>,
    {
        let mut summary = ProviderImportSummary::default();
        let mut batches = 0_usize;
        let mut source_exhausted = false;
        let retained_final = loop {
            let current = batch.take().ok_or(CaptureError::SystemInvariant(
                "captured batch group lost its pending batch",
            ))?;
            frontier.validate_next_batch(self.admission, &current)?;
            let rejected_records_before_batch = frontier.cumulative_rejected_records()?;
            let projected = self.project_batch(&current, rejected_records_before_batch)?;
            frontier.apply_projected_batch(&current, &projected)?;
            summary.merge(projected.summary);
            batches = batches.saturating_add(1);

            if current.source_exhausted() {
                source_exhausted = true;
                break current;
            }
            if (recovery_group && frontier.is_safe(self.admission))
                || batches >= max_batches.get()
                || current.retained_payload_bytes() > CAPTURE_BATCH_MAX_PAYLOAD_BYTES
            {
                // Keep the final raw batch alive through FTS maintenance, source revalidation,
                // and cursor CAS. Earlier batches are released before the producer may hydrate
                // its one permitted lookahead row.
                break current;
            }
            drop(current);
            *batch = next_batch()?;
            if batch.is_none() {
                return Err(CaptureError::SystemInvariant(
                    "captured batch producer exhausted without tagging its final batch",
                ));
            }
        };
        Ok(ProjectedBatchGroup {
            summary,
            batches,
            source_exhausted,
            retained_final,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn import_sequence<NextBatch, Revalidate>(
        &mut self,
        expected_store_cursor: Option<&SyncCursor>,
        initial_native_position: &NativePosition,
        cursor_mode: CapturedBatchCursorMode,
        max_batches: NonZeroUsize,
        mut next_batch: NextBatch,
        mut revalidate_source: Revalidate,
        sequence_mode: CapturedBatchSequenceMode,
    ) -> Result<CapturedBatchImportOutcome>
    where
        NextBatch: FnMut() -> Result<Option<CapturedBatch>>,
        Revalidate: FnMut() -> Result<bool>,
    {
        self.admission
            .reconcile_provider_locator(self.store, self.observed_at)?;
        self.admission.require_current_inventory_observation()?;
        let mut frontier =
            self.initialize_frontier(expected_store_cursor, initial_native_position, cursor_mode)?;
        let mut batch = next_batch()?;
        if batch.is_none() {
            let provider_source_is_current = revalidate_source()?;
            frontier.publish_initial_after_revalidation(
                self.store,
                self.admission,
                provider_source_is_current,
            )?;
            if !frontier.is_safe(self.admission) {
                return Err(CaptureError::SystemInvariant(
                    "captured source exhausted without a parser-safe cursor",
                ));
            }
            return Ok(CapturedBatchImportOutcome {
                summary: frontier.replay_rejection_summary()?,
                batches_imported: 0,
                source_exhausted: true,
                cursor_safe: frontier.is_safe(self.admission),
            });
        }

        let mut merged = ProviderImportSummary::default();
        let mut total_batches = 0_usize;
        loop {
            let recovery_group = !frontier.is_safe(self.admission);
            let group = self.with_event_search_bulk_mode(|run| {
                run.project_group(
                    &mut frontier,
                    &mut batch,
                    max_batches,
                    recovery_group,
                    &mut next_batch,
                )
            })?;
            let provider_source_is_current = revalidate_source()?;
            frontier.settle_group_after_revalidation(
                self.store,
                self.admission,
                provider_source_is_current,
            )?;
            drop(group.retained_final);
            merged.merge(group.summary);
            total_batches = total_batches.saturating_add(group.batches);

            if group.source_exhausted {
                if !frontier.is_safe(self.admission) {
                    return Err(CaptureError::SystemInvariant(
                        "captured source exhausted without a parser-safe cursor",
                    ));
                }
                return Ok(CapturedBatchImportOutcome {
                    summary: merged,
                    batches_imported: total_batches,
                    source_exhausted: true,
                    cursor_safe: frontier.is_safe(self.admission),
                });
            }
            if sequence_mode == CapturedBatchSequenceMode::OneSafeGroup
                && frontier.is_safe(self.admission)
            {
                return Ok(CapturedBatchImportOutcome {
                    summary: merged,
                    batches_imported: total_batches,
                    source_exhausted: false,
                    cursor_safe: true,
                });
            }
            if batch.is_none() {
                batch = next_batch()?;
                if batch.is_none() {
                    if !frontier.is_safe(self.admission) {
                        return Err(CaptureError::SystemInvariant(
                            "captured source exhausted without a parser-safe cursor",
                        ));
                    }
                    return Ok(CapturedBatchImportOutcome {
                        summary: merged,
                        batches_imported: total_batches,
                        source_exhausted: true,
                        cursor_safe: frontier.is_safe(self.admission),
                    });
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(crate) fn import_captured_batch<Projector, Revalidate>(
    store: &mut Store,
    admission: &CapturedSourceAdmission,
    batch: &CapturedBatch,
    options: NormalizedProviderImportOptions,
    machine_id: &str,
    observed_at: chrono::DateTime<chrono::Utc>,
    expected_store_cursor: Option<&SyncCursor>,
    initial_native_position: &NativePosition,
    cursor_mode: CapturedBatchCursorMode,
    projector: &mut Projector,
    revalidate_source: Revalidate,
) -> Result<CapturedBatchImportOutcome>
where
    Projector: CapturedBatchProjector,
    Revalidate: FnOnce() -> Result<bool>,
{
    CapturedImportRun::new(
        store,
        admission,
        options,
        machine_id,
        observed_at,
        projector,
    )
    .import_one(
        batch,
        expected_store_cursor,
        initial_native_position,
        cursor_mode,
        revalidate_source,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn import_captured_batches<Projector, NextBatch, Revalidate>(
    store: &mut Store,
    admission: &CapturedSourceAdmission,
    options: NormalizedProviderImportOptions,
    machine_id: &str,
    observed_at: chrono::DateTime<chrono::Utc>,
    expected_store_cursor: Option<&SyncCursor>,
    initial_native_position: &NativePosition,
    initial_cursor_mode: CapturedBatchCursorMode,
    max_batches: NonZeroUsize,
    projector: &mut Projector,
    next_batch: NextBatch,
    revalidate_source: Revalidate,
) -> Result<CapturedBatchImportOutcome>
where
    Projector: CapturedBatchProjector,
    NextBatch: FnMut() -> Result<Option<CapturedBatch>>,
    Revalidate: FnMut() -> Result<bool>,
{
    CapturedImportRun::new(
        store,
        admission,
        options,
        machine_id,
        observed_at,
        projector,
    )
    .import_sequence(
        expected_store_cursor,
        initial_native_position,
        initial_cursor_mode,
        max_batches,
        next_batch,
        revalidate_source,
        CapturedBatchSequenceMode::OneSafeGroup,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn drain_captured_batches<Projector, NextBatch, Revalidate>(
    store: &mut Store,
    admission: &CapturedSourceAdmission,
    options: NormalizedProviderImportOptions,
    machine_id: &str,
    observed_at: chrono::DateTime<chrono::Utc>,
    expected_store_cursor: Option<SyncCursor>,
    initial_native_position: &NativePosition,
    cursor_mode: CapturedBatchCursorMode,
    cursor_stream: &str,
    projector: &mut Projector,
    next_batch: NextBatch,
    revalidate_source: Revalidate,
) -> Result<ProviderImportSummary>
where
    Projector: CapturedBatchProjector,
    NextBatch: FnMut() -> Result<Option<CapturedBatch>>,
    Revalidate: FnMut() -> Result<bool>,
{
    if cursor_stream != captured_batch_cursor_stream(admission.source()) {
        return Err(CaptureError::SystemInvariant(
            "captured batch drain cursor stream does not match its admitted source",
        ));
    }
    let max_batches = NonZeroUsize::new(CAPTURE_BATCH_MAX_BATCHES_PER_GROUP).ok_or(
        CaptureError::SystemInvariant("captured batch group limit must be nonzero"),
    )?;
    let sequence_mode = match options.capture_work_limit {
        crate::CaptureWorkLimit::Drain => CapturedBatchSequenceMode::Drain,
        crate::CaptureWorkLimit::OneSafeGroup => CapturedBatchSequenceMode::OneSafeGroup,
    };
    let mut outcome = CapturedImportRun::new(
        store,
        admission,
        options,
        machine_id,
        observed_at,
        projector,
    )
    .import_sequence(
        expected_store_cursor.as_ref(),
        initial_native_position,
        cursor_mode,
        max_batches,
        next_batch,
        revalidate_source,
        sequence_mode,
    )?;
    if !outcome.cursor_safe {
        return Err(CaptureError::SystemInvariant(
            "captured batch drain returned without a parser-safe cursor",
        ));
    }
    outcome.summary.work_remaining =
        sequence_mode == CapturedBatchSequenceMode::OneSafeGroup && !outcome.source_exhausted;
    Ok(outcome.summary)
}
