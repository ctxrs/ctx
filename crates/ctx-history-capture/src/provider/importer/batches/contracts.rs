use ctx_history_core::ProviderCaptureEnvelope;

use crate::captured_batch::jsonl::VerifiedJsonlAppend;
use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, NativePosition, StructuralRejectionKind,
};
use crate::{CaptureError, ProviderImportSummary, ProviderNormalizationResult, Result};

use super::super::cursors::CertifiedProviderCursor;

pub(super) const MAX_PROVIDER_RECORD_REJECTION_REASON_BYTES: usize = 4 * 1024;

pub(crate) struct ProviderProjectionFatal(CaptureError);

impl std::fmt::Debug for ProviderProjectionFatal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ProviderProjectionFatal(<redacted>)")
    }
}

impl ProviderProjectionFatal {
    pub(crate) fn new(error: CaptureError) -> Self {
        Self(error)
    }

    pub(crate) fn system_invariant(message: &'static str) -> Self {
        Self(CaptureError::SystemInvariant(message))
    }

    pub(crate) fn into_capture_error(self) -> CaptureError {
        self.0
    }
}

pub(crate) type ProviderProjectionResult<T> = std::result::Result<T, ProviderProjectionFatal>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExistingSessionEventOutcome {
    /// The event persisted or replayed idempotently; dependent units may be emitted.
    Accepted,
    /// The event was deterministically rejected; dependent units for the record must be skipped.
    Rejected,
}

#[derive(Debug)]
pub(crate) enum CapturedBatchCursorFinish {
    /// The projector has transient parser state that cannot be certified yet.
    ///
    /// The importer keeps the prior certified candidate byte-for-byte and replays normalized
    /// units after a crash until the projector reaches a later safe boundary.
    RetainPrior,
    /// The supplied cursor certifies exactly the current captured batch end.
    Advance(CertifiedProviderCursor),
}

pub(crate) trait ProviderProjectionOutput {
    fn emit_normalization(
        &mut self,
        normalization: ProviderNormalizationResult,
    ) -> ProviderProjectionResult<()>;

    fn reject_record(&mut self, line_number: usize, reason: String);

    /// Emits an event for a session that an earlier projected record already persisted.
    ///
    /// The persisting sink resolves the exact admitted source-scoped session and deliberately
    /// leaves its source and session metadata untouched. Test collectors retain their existing
    /// capture-shaped oracle through this default forwarding implementation.
    fn emit_existing_session_event(
        &mut self,
        line_number: usize,
        capture: ProviderCaptureEnvelope,
    ) -> ProviderProjectionResult<ExistingSessionEventOutcome> {
        self.emit_normalization(ProviderNormalizationResult {
            captures: vec![(line_number, capture)],
            ..ProviderNormalizationResult::default()
        })?;
        Ok(ExistingSessionEventOutcome::Accepted)
    }

    /// Declares that the current raw record streams its own explicit file-touch units.
    ///
    /// Test collectors do not infer touches, so their default implementation is intentionally a
    /// no-op. The persisting sink uses this declaration to suppress legacy event-payload
    /// inference before the record's first capture is stored.
    fn use_explicit_file_touches(&mut self) {}
}

pub(crate) fn emit_projected_normalization_units(
    output: &mut dyn ProviderProjectionOutput,
    normalization: ProviderNormalizationResult,
) -> ProviderProjectionResult<()> {
    let ProviderNormalizationResult {
        summary,
        captures,
        files_touched,
    } = normalization;
    if summary != ProviderImportSummary::default() {
        output.emit_normalization(ProviderNormalizationResult {
            summary,
            ..ProviderNormalizationResult::default()
        })?;
    }
    for capture in captures {
        output.emit_normalization(ProviderNormalizationResult {
            captures: vec![capture],
            ..ProviderNormalizationResult::default()
        })?;
    }
    for file_touch in files_touched {
        output.emit_normalization(ProviderNormalizationResult {
            files_touched: vec![file_touch],
            ..ProviderNormalizationResult::default()
        })?;
    }
    Ok(())
}

pub(crate) trait CapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()>;

    /// Projects a producer-level rejection that has no retained native payload.
    ///
    /// The importer durably records the cumulative rejection count in the certified cursor. A
    /// projector only needs to override this hook when a structural rejection also changes its
    /// provider-specific parser state; the default reports the rejection while the captured batch
    /// range advances the native cursor.
    fn project_structural_rejection(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        project_default_structural_rejection(record, output)
    }

    /// Builds the content-free parser state for an empty fresh or replacement observation.
    fn initial_cursor_candidate(
        &self,
        source: &crate::captured_batch::SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor>;

    /// Rebuilds one eventless session/source metadata envelope from the bounded final record.
    ///
    /// The importer persists this refresh after all record projections and before cursor finish.
    /// It is deliberately narrower than a general projection hook: no events, file touches,
    /// rejections, or multiple normalized units can be returned here.
    fn final_metadata_capture(
        &mut self,
        _batch: &CapturedBatch,
    ) -> ProviderProjectionResult<Option<(usize, ProviderCaptureEnvelope)>> {
        Ok(None)
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish>;
}

pub(crate) fn project_default_structural_rejection(
    record: &CapturedRecord,
    output: &mut dyn ProviderProjectionOutput,
) -> ProviderProjectionResult<()> {
    let (line_number, reason) = structural_rejection(record)?;
    output.reject_record(line_number, reason);
    Ok(())
}

fn structural_rejection(record: &CapturedRecord) -> ProviderProjectionResult<(usize, String)> {
    let line_number = usize::try_from(record.ordinal())
        .ok()
        .and_then(|ordinal| ordinal.checked_add(1))
        .ok_or_else(|| {
            ProviderProjectionFatal::system_invariant(
                "captured provider record ordinal exceeds platform limits",
            )
        })?;
    match record.payload() {
        CapturedRecordPayload::StructuralRejection {
            kind: StructuralRejectionKind::OversizeRecord,
            observed_bytes,
        } => Ok((
            line_number,
            format!(
                "provider record exceeds the {} byte limit (observed {observed_bytes} bytes)",
                crate::MAX_PROVIDER_JSONL_LINE_BYTES
            ),
        )),
        CapturedRecordPayload::NativeBytes(_) | CapturedRecordPayload::SqliteValues(_) => {
            Err(ProviderProjectionFatal::system_invariant(
                "structural rejection projection requires a structural rejection record",
            ))
        }
    }
}

#[derive(Debug)]
pub(crate) struct CapturedBatchImportOutcome {
    pub(crate) summary: ProviderImportSummary,
    pub(crate) batches_imported: usize,
    pub(crate) source_exhausted: bool,
    pub(crate) cursor_safe: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapturedBatchCursorMode {
    Resume,
    ResumeAppend(VerifiedJsonlAppend),
    ResetChangedSource,
    ReplaceLegacyCursor,
}
