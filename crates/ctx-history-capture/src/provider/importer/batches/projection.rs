use std::collections::BTreeSet;

use ctx_history_core::{CaptureProvider, ProviderCaptureEnvelope, SyncCursor};
use ctx_history_store::Store;

use crate::captured_batch::{CapturedBatch, CapturedRecordPayload};
use crate::provider::file_touches::{
    visit_provider_file_touches_from_event, ProviderFileTouchSourceContext,
    PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
};
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderFileTouchedEnvelope,
    ProviderImportFailure, ProviderImportSummary, ProviderNormalizationResult, Result,
};

use super::super::cursors::{
    captured_batch_cursor_stream, certified_provider_sync_cursor, CertifiedProviderCursor,
};
use super::super::existing_session::resolve_provider_existing_session_identity;
use super::super::source_relocation::CanonicalProviderSourceOverride;
use super::super::{
    import_provider_capture_line_with_canonical_source, import_provider_event_for_session,
    import_provider_file_touched_line, ProviderImportCaches,
};
use super::admission::CapturedSourceAdmission;
use super::codex_fast_path::codex_existing_session_unit_bytes;
use super::contracts::{
    CapturedBatchCursorFinish, CapturedBatchProjector, ExistingSessionEventOutcome,
    ProviderProjectionFatal, ProviderProjectionOutput, ProviderProjectionResult,
    MAX_PROVIDER_RECORD_REJECTION_REASON_BYTES,
};
use super::write_tx::{serialized_len_or_rollback, ProviderImportTransaction};

pub(super) fn bounded_provider_rejection_reason(mut reason: String) -> String {
    if reason.is_empty() {
        return "provider record was deterministically rejected".to_owned();
    }
    if reason.len() <= MAX_PROVIDER_RECORD_REJECTION_REASON_BYTES {
        return reason;
    }
    let mut boundary = MAX_PROVIDER_RECORD_REJECTION_REASON_BYTES;
    while !reason.is_char_boundary(boundary) {
        boundary -= 1;
    }
    reason.truncate(boundary);
    reason
}
pub(super) struct ProjectedCapturedBatch {
    pub(super) summary: ProviderImportSummary,
    pub(super) cursor_finish: ProjectedCursorFinish,
    pub(super) cumulative_rejected_records: u64,
}

pub(super) struct ProjectedCursorCandidate {
    pub(super) store_cursor: SyncCursor,
    pub(super) certified: CertifiedProviderCursor,
}

pub(super) enum ProjectedCursorFinish {
    RetainPrior,
    Advance(Box<ProjectedCursorCandidate>),
}

struct ProjectedNormalizationPersistence<'a> {
    store: &'a mut Store,
    options: &'a NormalizedProviderImportOptions,
    transaction: &'a mut ProviderImportTransaction,
    caches: &'a mut ProviderImportCaches,
    summary: &'a mut ProviderImportSummary,
    canonical_source: Option<CanonicalProviderSourceOverride>,
}

struct PersistingProviderProjectionOutput<'a> {
    store: &'a mut Store,
    options: &'a NormalizedProviderImportOptions,
    transaction: &'a mut ProviderImportTransaction,
    caches: &'a mut ProviderImportCaches,
    summary: &'a mut ProviderImportSummary,
    admission: &'a CapturedSourceAdmission,
    current_line_number: usize,
    infer_file_touches: bool,
    projected_captures: &'a mut ProjectedRecordCaptures,
}

struct ProjectedRecordCaptures {
    event_count: u32,
    provider_session_id: Option<String>,
}

impl ProjectedRecordCaptures {
    fn new() -> Self {
        Self {
            event_count: 0,
            provider_session_id: None,
        }
    }

    fn next_subrecord_index(&self) -> u32 {
        self.event_count
    }

    fn accept_borrowed(&mut self, captures: &[(usize, ProviderCaptureEnvelope)]) -> Result<()> {
        for (_, capture) in captures {
            if capture.event.is_none() {
                continue;
            }
            self.accept_event_session(capture)?;
        }
        Ok(())
    }

    fn accept(&mut self, capture: &ProviderCaptureEnvelope) -> Result<()> {
        if capture.event.is_none() {
            return Ok(());
        }
        self.accept_event_session(capture)
    }

    fn accept_event_session(&mut self, capture: &ProviderCaptureEnvelope) -> Result<()> {
        match &self.provider_session_id {
            Some(provider_session_id)
                if provider_session_id != &capture.session.provider_session_id =>
            {
                return Err(CaptureError::SystemInvariant(
                    "one source record projected events for multiple native sessions",
                ));
            }
            Some(_) => {}
            None => {
                self.provider_session_id = Some(capture.session.provider_session_id.clone());
            }
        }
        self.event_count = self
            .event_count
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "captured event subrecord index exceeds u32",
            ))?;
        Ok(())
    }
}

fn annotate_source_record_coordinates(
    captures: &mut [(usize, ProviderCaptureEnvelope)],
    line_number: usize,
    first_subrecord_index: u32,
) -> Result<()> {
    let source_record_ordinal = u64::try_from(line_number.checked_sub(1).ok_or(
        CaptureError::SystemInvariant("captured projection line numbers are one-based"),
    )?)
    .map_err(|_| CaptureError::SystemInvariant("captured projection ordinal exceeds u64"))?;
    let mut subrecord_index = first_subrecord_index;
    for (_, capture) in captures {
        let Some(event) = capture.event.as_mut() else {
            continue;
        };
        let metadata = event
            .metadata
            .as_object_mut()
            .ok_or(CaptureError::SystemInvariant(
                "captured provider event metadata must be an object",
            ))?;
        metadata.insert(
            "source_record_ordinal".to_owned(),
            serde_json::Value::from(source_record_ordinal),
        );
        metadata.insert(
            "source_record_subrecord_index".to_owned(),
            serde_json::Value::from(subrecord_index),
        );
        subrecord_index = subrecord_index
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "captured event subrecord index exceeds u32",
            ))?;
    }
    Ok(())
}

impl ProviderProjectionOutput for PersistingProviderProjectionOutput<'_> {
    fn emit_normalization(
        &mut self,
        mut normalization: ProviderNormalizationResult,
    ) -> ProviderProjectionResult<()> {
        if normalization.captures.len() > 1 || normalization.files_touched.len() > 1 {
            return Err(ProviderProjectionFatal::system_invariant(
                "captured projection output must emit at most one normalized Store unit",
            ));
        }
        if let Err(error) = self.admission.validate_normalization(&normalization) {
            if projection_error_is_deterministic(&error) {
                self.reject_record(self.current_line_number, error.to_string());
                return Ok(());
            }
            return Err(ProviderProjectionFatal::new(error));
        }
        let first_subrecord_index = self.projected_captures.next_subrecord_index();
        annotate_source_record_coordinates(
            &mut normalization.captures,
            self.current_line_number,
            first_subrecord_index,
        )
        .map_err(ProviderProjectionFatal::new)?;
        self.projected_captures
            .accept_borrowed(&normalization.captures)
            .map_err(ProviderProjectionFatal::new)?;
        persist_projected_normalization(
            ProjectedNormalizationPersistence {
                store: self.store,
                options: self.options,
                transaction: self.transaction,
                caches: self.caches,
                summary: self.summary,
                canonical_source: self.admission.canonical_source_override(),
            },
            normalization,
            self.infer_file_touches,
        )
        .map_err(ProviderProjectionFatal::new)?;
        Ok(())
    }

    fn reject_record(&mut self, line_number: usize, reason: String) {
        self.summary.record_failure(ProviderImportFailure {
            line: line_number,
            error: bounded_provider_rejection_reason(reason),
        });
    }

    fn emit_existing_session_event(
        &mut self,
        line_number: usize,
        mut capture: ProviderCaptureEnvelope,
    ) -> ProviderProjectionResult<ExistingSessionEventOutcome> {
        if line_number != self.current_line_number {
            return Err(ProviderProjectionFatal::system_invariant(
                "existing-session provider event line does not match the current record",
            ));
        }
        if capture.event.is_none() {
            return Err(ProviderProjectionFatal::system_invariant(
                "existing-session provider event projection requires an event",
            ));
        }
        if let Err(error) = self.admission.validate_capture(&capture) {
            if projection_error_is_deterministic(&error) {
                self.reject_record(line_number, error.to_string());
                return Ok(ExistingSessionEventOutcome::Rejected);
            }
            return Err(ProviderProjectionFatal::new(error));
        }
        let first_subrecord_index = self.projected_captures.next_subrecord_index();
        let mut annotated = (line_number, capture);
        annotate_source_record_coordinates(
            std::slice::from_mut(&mut annotated),
            line_number,
            first_subrecord_index,
        )
        .map_err(ProviderProjectionFatal::new)?;
        capture = annotated.1;
        let (source_id, session_id, first_existing_session_use) =
            match resolve_provider_existing_session_identity(
                self.store,
                line_number,
                &capture,
                self.caches,
                self.admission.canonical_source_override().as_ref(),
            ) {
                Ok(session_id) => session_id,
                Err(error) if projection_error_is_deterministic(&error) => {
                    self.reject_record(line_number, error.to_string());
                    return Ok(ExistingSessionEventOutcome::Rejected);
                }
                Err(error) => return Err(ProviderProjectionFatal::new(error)),
            };
        if capture.provider == CaptureProvider::Codex && first_existing_session_use {
            self.summary.skipped = self.summary.skipped.saturating_add(1);
            self.summary.skipped_sessions = self.summary.skipped_sessions.saturating_add(1);
        }
        let unit_bytes = if capture.provider == CaptureProvider::Codex {
            codex_existing_session_unit_bytes(
                self.store,
                self.transaction,
                self.caches,
                source_id,
                &mut capture,
            )
        } else {
            serialized_len_or_rollback(self.transaction, self.store, &capture)
        }
        .map_err(ProviderProjectionFatal::new)?;
        persist_projected_existing_session_event(
            ProjectedNormalizationPersistence {
                store: self.store,
                options: self.options,
                transaction: self.transaction,
                caches: self.caches,
                summary: self.summary,
                canonical_source: self.admission.canonical_source_override(),
            },
            self.infer_file_touches,
            line_number,
            source_id,
            session_id,
            &capture,
            unit_bytes,
        )
        .map_err(ProviderProjectionFatal::new)?;
        self.projected_captures
            .accept(&capture)
            .map_err(ProviderProjectionFatal::new)?;
        Ok(ExistingSessionEventOutcome::Accepted)
    }

    fn use_explicit_file_touches(&mut self) {
        self.infer_file_touches = false;
    }
}
#[allow(clippy::too_many_arguments)]
pub(super) fn project_captured_batch_inner<Projector>(
    store: &mut Store,
    batch: &CapturedBatch,
    options: &NormalizedProviderImportOptions,
    machine_id: &str,
    observed_at: chrono::DateTime<chrono::Utc>,
    transaction: &mut ProviderImportTransaction,
    projector: &mut Projector,
    admission: &CapturedSourceAdmission,
    rejected_records_before_batch: u64,
) -> Result<ProjectedCapturedBatch>
where
    Projector: CapturedBatchProjector,
{
    let mut summary = ProviderImportSummary::default();
    let mut caches = ProviderImportCaches::default();
    for record in batch.records() {
        let line_number = usize::try_from(record.ordinal())
            .ok()
            .and_then(|ordinal| ordinal.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "captured provider record ordinal exceeds platform limits",
            ))?;
        let mut projected_captures = ProjectedRecordCaptures::new();
        let mut output = PersistingProviderProjectionOutput {
            store,
            options,
            transaction,
            caches: &mut caches,
            summary: &mut summary,
            admission,
            current_line_number: line_number,
            infer_file_touches: true,
            projected_captures: &mut projected_captures,
        };
        match record.payload() {
            CapturedRecordPayload::StructuralRejection { .. } => projector
                .project_structural_rejection(record, &mut output)
                .map_err(ProviderProjectionFatal::into_capture_error)?,
            CapturedRecordPayload::NativeBytes(_) | CapturedRecordPayload::SqliteValues(_) => {
                projector
                    .project_record(record, &mut output)
                    .map_err(ProviderProjectionFatal::into_capture_error)?;
            }
        }
    }
    if let Some((line_number, capture)) = projector
        .final_metadata_capture(batch)
        .map_err(ProviderProjectionFatal::into_capture_error)?
    {
        let matches_batch_record = line_number
            .checked_sub(1)
            .and_then(|ordinal| u64::try_from(ordinal).ok())
            .is_some_and(|ordinal| {
                batch
                    .records()
                    .iter()
                    .any(|record| record.ordinal() == ordinal)
            });
        if !matches_batch_record {
            return Err(CaptureError::SystemInvariant(
                "captured batch final metadata line is outside the batch",
            ));
        }
        if capture.event.is_some() {
            return Err(CaptureError::SystemInvariant(
                "captured batch final metadata refresh must be eventless",
            ));
        }
        let normalization = ProviderNormalizationResult {
            captures: vec![(line_number, capture)],
            ..ProviderNormalizationResult::default()
        };
        admission.validate_normalization(&normalization)?;
        let mut final_caches = ProviderImportCaches::default();
        let mut discarded_summary = ProviderImportSummary::default();
        persist_projected_normalization(
            ProjectedNormalizationPersistence {
                store,
                options,
                transaction,
                caches: &mut final_caches,
                summary: &mut discarded_summary,
                canonical_source: admission.canonical_source_override(),
            },
            normalization,
            false,
        )?;
    }
    let rejected_records_in_batch = u64::try_from(summary.failed)
        .map_err(|_| CaptureError::SystemInvariant("provider rejection count exceeds u64"))?;
    let cumulative_rejected_records = rejected_records_before_batch
        .checked_add(rejected_records_in_batch)
        .ok_or(CaptureError::SystemInvariant(
            "certified provider rejection count overflowed",
        ))?;
    let cursor_finish = match projector.finish_cursor(batch)? {
        CapturedBatchCursorFinish::RetainPrior => ProjectedCursorFinish::RetainPrior,
        CapturedBatchCursorFinish::Advance(cursor) => {
            let certified = cursor.with_rejected_records(cumulative_rejected_records);
            certified.validate_observation_position(batch.source(), batch.range_end())?;
            let store_cursor = certified_provider_sync_cursor(
                batch.source().provider(),
                machine_id,
                captured_batch_cursor_stream(batch.source()),
                &certified,
                observed_at,
            )?;
            ProjectedCursorFinish::Advance(Box::new(ProjectedCursorCandidate {
                store_cursor,
                certified,
            }))
        }
    };
    Ok(ProjectedCapturedBatch {
        summary,
        cursor_finish,
        cumulative_rejected_records,
    })
}

fn projection_error_is_deterministic(error: &CaptureError) -> bool {
    matches!(
        error,
        CaptureError::Json(_)
            | CaptureError::Time(_)
            | CaptureError::Uuid(_)
            | CaptureError::UnsupportedSchemaVersion(_)
            | CaptureError::InvalidPayload(_)
            | CaptureError::InvalidJsonLine { .. }
    )
}

fn persist_projected_normalization(
    persistence: ProjectedNormalizationPersistence<'_>,
    normalization: ProviderNormalizationResult,
    infer_file_touches: bool,
) -> Result<()> {
    let ProjectedNormalizationPersistence {
        store,
        options,
        transaction,
        caches,
        summary,
        canonical_source,
    } = persistence;
    let ProviderNormalizationResult {
        summary: normalized_summary,
        captures,
        files_touched,
    } = normalization;
    if normalized_summary.failed != 0 {
        return Err(CaptureError::SystemInvariant(
            "accepted provider record returned normalization failures",
        ));
    }
    summary.merge(normalized_summary);
    let supplied_file_touch_lines = files_touched
        .iter()
        .map(|(line_number, _)| *line_number)
        .collect::<BTreeSet<_>>();
    for (line_number, capture) in captures {
        let unit_bytes = serialized_len_or_rollback(transaction, store, &capture)?;
        transaction.prepare_unit(store, unit_bytes)?;
        let line_summary = import_provider_capture_line_with_canonical_source(
            store,
            &capture,
            options,
            line_number,
            caches,
            canonical_source.as_ref(),
        )?;
        summary.merge(line_summary);
        transaction.record_unit(store, unit_bytes)?;
        if !infer_file_touches
            || capture.provider == CaptureProvider::Codex
            || supplied_file_touch_lines.contains(&line_number)
        {
            continue;
        }
        let Some(event) = capture.event.as_ref() else {
            continue;
        };
        let outcome = visit_provider_file_touches_from_event(
            ProviderFileTouchSourceContext::new(
                capture.provider,
                &capture.session.provider_session_id,
                &capture.source.source_format,
                capture.source.raw_source_path.as_deref(),
                capture.source.source_root.as_deref(),
            ),
            event,
            line_number,
            |(_, file)| {
                persist_projected_file_touch(
                    store,
                    options,
                    transaction,
                    summary,
                    &file,
                    canonical_source.as_ref(),
                )
            },
        )?;
        if outcome.limit_exceeded() {
            summary.record_failure(ProviderImportFailure {
                line: line_number,
                error: PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
            });
        }
    }
    for (_line_number, file) in files_touched {
        persist_projected_file_touch(
            store,
            options,
            transaction,
            summary,
            &file,
            canonical_source.as_ref(),
        )?;
    }
    Ok(())
}

fn persist_projected_existing_session_event(
    persistence: ProjectedNormalizationPersistence<'_>,
    infer_file_touches: bool,
    line_number: usize,
    source_id: uuid::Uuid,
    session_id: uuid::Uuid,
    capture: &ProviderCaptureEnvelope,
    unit_bytes: usize,
) -> Result<()> {
    let ProjectedNormalizationPersistence {
        store,
        options,
        transaction,
        caches,
        summary,
        canonical_source,
    } = persistence;
    transaction.prepare_unit(store, unit_bytes)?;
    let event = capture.event.as_ref().ok_or(CaptureError::SystemInvariant(
        "existing-session provider event projection lost its mandatory event",
    ))?;
    let mut line_summary = ProviderImportSummary::default();
    import_provider_event_for_session(
        store,
        capture,
        event,
        options,
        line_number,
        caches,
        source_id,
        session_id,
        &mut line_summary,
    )?;
    summary.merge(line_summary);
    transaction.record_unit(store, unit_bytes)?;
    if !infer_file_touches || capture.provider == CaptureProvider::Codex {
        return Ok(());
    }
    let outcome = visit_provider_file_touches_from_event(
        ProviderFileTouchSourceContext::new(
            capture.provider,
            &capture.session.provider_session_id,
            &capture.source.source_format,
            capture.source.raw_source_path.as_deref(),
            capture.source.source_root.as_deref(),
        ),
        event,
        line_number,
        |(_, file)| {
            persist_projected_file_touch(
                store,
                options,
                transaction,
                summary,
                &file,
                canonical_source.as_ref(),
            )
        },
    )?;
    if outcome.limit_exceeded() {
        summary.record_failure(ProviderImportFailure {
            line: line_number,
            error: PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
        });
    }
    Ok(())
}

fn persist_projected_file_touch(
    store: &mut Store,
    options: &NormalizedProviderImportOptions,
    transaction: &mut ProviderImportTransaction,
    summary: &mut ProviderImportSummary,
    file: &ProviderFileTouchedEnvelope,
    canonical_source: Option<&CanonicalProviderSourceOverride>,
) -> Result<()> {
    let unit_bytes = serialized_len_or_rollback(transaction, store, file)?;
    transaction.prepare_unit(store, unit_bytes)?;
    import_provider_file_touched_line(store, file, options, canonical_source)?;
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    transaction.record_unit(store, unit_bytes)
}
