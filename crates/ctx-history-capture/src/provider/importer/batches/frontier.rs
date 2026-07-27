use ctx_history_core::SyncCursor;
use ctx_history_store::Store;

use crate::captured_batch::{CapturedBatch, NativePosition, SourceObservation};
use crate::{CaptureError, ProviderImportSummary, Result};

use super::super::cursors::{
    captured_batch_cursor_stream, certified_provider_sync_cursor,
    compare_and_set_provider_sync_cursor, CertifiedProviderCursor, ProviderCursorCommit,
};
use super::admission::CapturedSourceAdmission;
use super::contracts::{CapturedBatchCursorMode, CapturedBatchProjector};
use super::projection::{ProjectedCapturedBatch, ProjectedCursorFinish};

pub(super) struct SafeCursorCandidate {
    store_cursor: SyncCursor,
    certified: CertifiedProviderCursor,
}

pub(super) struct CapturedBatchFrontier {
    group_start_store_cursor: Option<SyncCursor>,
    scan_frontier: NativePosition,
    safe_candidate: SafeCursorCandidate,
    rejected_records_after_candidate: u64,
}

impl CapturedBatchFrontier {
    pub(super) fn cumulative_rejected_records(&self) -> Result<u64> {
        self.safe_candidate
            .certified
            .rejected_records()
            .checked_add(self.rejected_records_after_candidate)
            .ok_or(CaptureError::SystemInvariant(
                "certified provider rejection count overflowed",
            ))
    }

    fn is_safe_for(&self, source: &SourceObservation) -> bool {
        self.rejected_records_after_candidate == 0
            && self.safe_candidate.certified.source_revision() == source.source_revision()
            && self.safe_candidate.certified.parser_revision() == source.capture_revision()
            && self.safe_candidate.certified.policy_revision() == source.policy_revision()
            && self.safe_candidate.certified.native_position() == &self.scan_frontier
    }

    pub(super) fn is_safe(&self, admission: &CapturedSourceAdmission) -> bool {
        self.is_safe_for(admission.source())
    }

    pub(super) fn validate_next_batch(
        &self,
        admission: &CapturedSourceAdmission,
        batch: &CapturedBatch,
    ) -> Result<()> {
        admission.validate_batch(batch)?;
        if batch.range_before() != &self.scan_frontier {
            return Err(CaptureError::SystemInvariant(
                "captured batch does not continue the transient scan frontier",
            ));
        }
        Ok(())
    }

    pub(super) fn apply_projected_batch(
        &mut self,
        batch: &CapturedBatch,
        projected: &ProjectedCapturedBatch,
    ) -> Result<()> {
        match &projected.cursor_finish {
            ProjectedCursorFinish::RetainPrior => {
                self.rejected_records_after_candidate = projected
                    .cumulative_rejected_records
                    .checked_sub(self.safe_candidate.certified.rejected_records())
                    .ok_or(CaptureError::SystemInvariant(
                        "retained provider cursor rejection count regressed",
                    ))?;
            }
            ProjectedCursorFinish::Advance(candidate) => {
                self.safe_candidate = SafeCursorCandidate {
                    store_cursor: candidate.store_cursor.clone(),
                    certified: candidate.certified.clone(),
                };
                self.rejected_records_after_candidate = 0;
            }
        }
        self.scan_frontier = batch.range_end().clone();
        Ok(())
    }

    pub(super) fn replay_rejection_summary(&self) -> Result<ProviderImportSummary> {
        replay_rejection_summary(self.cumulative_rejected_records()?)
    }

    pub(super) fn publish_initial_after_revalidation(
        &mut self,
        store: &Store,
        admission: &CapturedSourceAdmission,
        provider_source_is_current: bool,
    ) -> Result<()> {
        admission.require_revalidated_source(provider_source_is_current)?;
        publish_initial_candidate_if_needed(store, admission, self)
    }

    #[cfg(test)]
    pub(super) fn publish_candidate_after_revalidation(
        &mut self,
        store: &Store,
        admission: &CapturedSourceAdmission,
        provider_source_is_current: bool,
    ) -> Result<()> {
        admission.require_revalidated_source(provider_source_is_current)?;
        publish_frontier_candidate(store, admission, self)
    }

    pub(super) fn settle_group_after_revalidation(
        &mut self,
        store: &Store,
        admission: &CapturedSourceAdmission,
        provider_source_is_current: bool,
    ) -> Result<()> {
        admission.require_revalidated_source(provider_source_is_current)?;
        if self.is_safe(admission) {
            publish_frontier_candidate(store, admission, self)
        } else {
            validate_uncommitted_frontier(store, self)
        }
    }
}
#[allow(clippy::too_many_arguments)]
pub(super) fn initialize_captured_batch_frontier<Projector: CapturedBatchProjector>(
    admission: &CapturedSourceAdmission,
    machine_id: &str,
    observed_at: chrono::DateTime<chrono::Utc>,
    expected_store_cursor: Option<&SyncCursor>,
    initial_native_position: &NativePosition,
    cursor_mode: CapturedBatchCursorMode,
    projector: &Projector,
) -> Result<CapturedBatchFrontier> {
    if let Some(expected) = expected_store_cursor {
        validate_expected_cursor_identity(expected, admission.source(), machine_id)?;
    }
    let make_initial = || -> Result<SafeCursorCandidate> {
        let certified =
            projector.initial_cursor_candidate(admission.source(), initial_native_position)?;
        certified.validate_observation_position(admission.source(), initial_native_position)?;
        if certified.rejected_records() != 0 {
            return Err(CaptureError::SystemInvariant(
                "initial provider cursor candidate carried rejected records",
            ));
        }
        let store_cursor = certified_provider_sync_cursor(
            admission.source().provider(),
            machine_id,
            captured_batch_cursor_stream(admission.source()),
            &certified,
            observed_at,
        )?;
        Ok(SafeCursorCandidate {
            store_cursor,
            certified,
        })
    };

    let (safe_candidate, scan_frontier) = match (cursor_mode, expected_store_cursor) {
        (CapturedBatchCursorMode::Resume, Some(expected)) => {
            let certified = CertifiedProviderCursor::decode_if_certified(&expected.cursor)?.ok_or(
                CaptureError::SystemInvariant(
                    "captured batch resume requires a certified provider cursor",
                ),
            )?;
            certified
                .validate_observation_position(admission.source(), certified.native_position())?;
            let scan = certified.native_position().clone();
            (
                SafeCursorCandidate {
                    store_cursor: expected.clone(),
                    certified,
                },
                scan,
            )
        }
        (CapturedBatchCursorMode::Resume, None) => {
            let initial = make_initial()?;
            (initial, initial_native_position.clone())
        }
        (CapturedBatchCursorMode::ResumeAppend(verified_append), Some(expected)) => {
            let prior = CertifiedProviderCursor::decode_if_certified(&expected.cursor)?.ok_or(
                CaptureError::SystemInvariant(
                    "captured append resume requires a certified provider cursor",
                ),
            )?;
            if prior.source_revision() == admission.source().source_revision()
                || prior.parser_revision() != admission.source().capture_revision()
                || prior.policy_revision() != admission.source().policy_revision()
                || !verified_append.validates(prior.native_position(), admission.source())
            {
                return Err(CaptureError::SystemInvariant(
                    "captured append resume does not continue the certified provider boundary",
                ));
            }
            let certified = CertifiedProviderCursor::new(
                admission.source().source_revision(),
                admission.source().capture_revision(),
                admission.source().policy_revision(),
                prior.native_position().clone(),
                prior.parser_checkpoint().clone(),
            )?
            .with_rejected_records(prior.rejected_records());
            let store_cursor = certified_provider_sync_cursor(
                admission.source().provider(),
                machine_id,
                captured_batch_cursor_stream(admission.source()),
                &certified,
                observed_at,
            )?;
            let scan = certified.native_position().clone();
            (
                SafeCursorCandidate {
                    store_cursor,
                    certified,
                },
                scan,
            )
        }
        (CapturedBatchCursorMode::ResetChangedSource, Some(expected)) => {
            let prior = CertifiedProviderCursor::decode_if_certified(&expected.cursor)?.ok_or(
                CaptureError::SystemInvariant(
                    "captured batch source reset requires a certified provider cursor",
                ),
            )?;
            if prior.matches_revisions(
                admission.source().source_revision(),
                admission.source().capture_revision(),
                admission.source().policy_revision(),
            ) {
                return Err(CaptureError::SystemInvariant(
                    "captured batch cursor reset requires changed source or parser semantics",
                ));
            }
            let initial = make_initial()?;
            (initial, initial_native_position.clone())
        }
        (CapturedBatchCursorMode::ReplaceLegacyCursor, Some(expected)) => {
            if CertifiedProviderCursor::decode_if_certified(&expected.cursor)?.is_some() {
                return Err(CaptureError::SystemInvariant(
                    "legacy provider cursor replacement requires an uncertified cursor",
                ));
            }
            let initial = make_initial()?;
            (initial, initial_native_position.clone())
        }
        (CapturedBatchCursorMode::ResumeAppend(_), None)
        | (CapturedBatchCursorMode::ResetChangedSource, None)
        | (CapturedBatchCursorMode::ReplaceLegacyCursor, None) => {
            return Err(CaptureError::SystemInvariant(
                "captured batch cursor mode requires an existing Store cursor",
            ));
        }
    };
    Ok(CapturedBatchFrontier {
        group_start_store_cursor: expected_store_cursor.cloned(),
        scan_frontier,
        safe_candidate,
        rejected_records_after_candidate: 0,
    })
}

fn validate_expected_cursor_identity(
    expected: &SyncCursor,
    source: &SourceObservation,
    machine_id: &str,
) -> Result<()> {
    if expected.team_id.is_some()
        || expected.device_id != machine_id
        || expected.stream != captured_batch_cursor_stream(source)
    {
        return Err(CaptureError::SystemInvariant(
            "captured batch cursor expectation belongs to another source",
        ));
    }
    Ok(())
}

fn replay_rejection_summary(rejected_records: u64) -> Result<ProviderImportSummary> {
    let failed = usize::try_from(rejected_records).map_err(|_| {
        CaptureError::SystemInvariant(
            "certified provider replay rejection count exceeds platform limits",
        )
    })?;
    Ok(ProviderImportSummary {
        failed,
        ..ProviderImportSummary::default()
    })
}

fn publish_initial_candidate_if_needed(
    store: &Store,
    admission: &CapturedSourceAdmission,
    frontier: &mut CapturedBatchFrontier,
) -> Result<()> {
    let already_published = frontier
        .group_start_store_cursor
        .as_ref()
        .is_some_and(|current| current == &frontier.safe_candidate.store_cursor);
    if already_published {
        return Ok(());
    }
    publish_frontier_candidate(store, admission, frontier)
}

fn publish_frontier_candidate(
    store: &Store,
    admission: &CapturedSourceAdmission,
    frontier: &mut CapturedBatchFrontier,
) -> Result<()> {
    commit_captured_batch_cursor(
        store,
        frontier.group_start_store_cursor.as_ref(),
        &frontier.safe_candidate.store_cursor,
    )?;
    let committed = store
        .get_sync_cursor(
            None,
            &frontier.safe_candidate.store_cursor.device_id,
            &frontier.safe_candidate.store_cursor.stream,
        )?
        .ok_or(CaptureError::SystemInvariant(
            "committed captured-batch cursor could not be reloaded",
        ))?;
    validate_expected_cursor_identity(
        &committed,
        admission.source(),
        &frontier.safe_candidate.store_cursor.device_id,
    )?;
    if committed.cursor != frontier.safe_candidate.store_cursor.cursor {
        return Err(CaptureError::SystemInvariant(
            "committed captured-batch cursor differs from its safe candidate",
        ));
    }
    frontier.group_start_store_cursor = Some(committed.clone());
    frontier.safe_candidate.store_cursor = committed;
    Ok(())
}

fn commit_captured_batch_cursor(
    store: &Store,
    expected_store_cursor: Option<&SyncCursor>,
    next_store_cursor: &SyncCursor,
) -> Result<()> {
    store.begin_immediate_batch()?;
    match compare_and_set_provider_sync_cursor(store, expected_store_cursor, next_store_cursor) {
        Ok(ProviderCursorCommit::Committed) => {
            if let Err(error) = store.commit_batch() {
                let _ = store.rollback_batch();
                return Err(error.into());
            }
            Ok(())
        }
        Ok(ProviderCursorCommit::Conflict) => {
            let _ = store.rollback_batch();
            Err(CaptureError::ProviderCursorConflict)
        }
        Err(error) => {
            let _ = store.rollback_batch();
            Err(error)
        }
    }
}

fn validate_uncommitted_frontier(store: &Store, frontier: &CapturedBatchFrontier) -> Result<()> {
    let candidate = &frontier.safe_candidate.store_cursor;
    store.begin_immediate_batch()?;
    let observed = store.get_sync_cursor(None, &candidate.device_id, &candidate.stream);
    match observed {
        Ok(observed) if observed.as_ref() == frontier.group_start_store_cursor.as_ref() => {
            if let Err(error) = store.commit_batch() {
                let _ = store.rollback_batch();
                return Err(error.into());
            }
            Ok(())
        }
        Ok(_) => {
            let _ = store.rollback_batch();
            Err(CaptureError::ProviderCursorConflict)
        }
        Err(error) => {
            let _ = store.rollback_batch();
            Err(error.into())
        }
    }
}
