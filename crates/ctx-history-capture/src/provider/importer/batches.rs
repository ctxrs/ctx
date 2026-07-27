use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroUsize,
};

use ctx_history_core::{CaptureProvider, ProviderCaptureEnvelope};
use ctx_history_store::Store;

use crate::provider::file_touches::{
    provider_file_touches_from_event, PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
};

use super::cursors::{persist_provider_sync_cursor, provider_sync_cursor};
use super::normalized::{
    filter_provider_capture_lines_without_real_session_messages,
    provider_capture_lines_have_real_message,
};
use super::{
    import_provider_capture_line, import_provider_file_touched_line, ProviderImportCaches,
};
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderFileTouchedEnvelope,
    ProviderImportFailure, ProviderImportSummary, ProviderNormalizationResult, Result,
};

mod admission;
mod codex_fast_path;
mod contracts;
mod frontier;
mod projection;
mod run;
mod source_relocation;
mod write_tx;

use write_tx::{provider_transaction_batch_size, serialized_len_or_rollback};

pub(crate) use admission::CapturedSourceAdmission;
pub(crate) use contracts::{
    emit_projected_normalization_units, project_default_structural_rejection,
    CapturedBatchCursorFinish, CapturedBatchCursorMode, CapturedBatchProjector,
    ExistingSessionEventOutcome, ProviderProjectionFatal, ProviderProjectionOutput,
    ProviderProjectionResult,
};
pub(crate) use run::{drain_captured_batches, import_captured_batches};
pub(crate) use write_tx::ProviderImportTransaction;

#[cfg(test)]
use projection::bounded_provider_rejection_reason;
#[cfg(test)]
use run::import_captured_batch;
#[cfg(test)]
use write_tx::{
    provider_transaction_commits, reset_provider_transaction_commits,
    IMPORT_TRANSACTION_BATCH_BYTES, IMPORT_TRANSACTION_BATCH_UNITS,
};
pub(super) fn import_normalized_provider_captures(
    store: &mut Store,
    normalization: ProviderNormalizationResult,
    options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let transaction_batch_size = provider_transaction_batch_size();
    let ProviderNormalizationResult {
        summary,
        captures,
        files_touched,
    } = normalization;
    import_provider_capture_lines_with_batch_size(
        store,
        options,
        summary,
        captures,
        files_touched,
        transaction_batch_size,
        true,
    )
}

#[cfg(test)]
pub(crate) fn import_normalized_provider_captures_in_batches(
    store: &mut Store,
    normalization: ProviderNormalizationResult,
    options: NormalizedProviderImportOptions,
    transaction_batch_size: usize,
) -> Result<ProviderImportSummary> {
    if !options.wrap_transaction {
        return Err(CaptureError::InvalidPayload(
            "batched provider import requires transaction wrapping".to_owned(),
        ));
    }
    let transaction_batch_size = NonZeroUsize::new(transaction_batch_size).ok_or_else(|| {
        CaptureError::InvalidPayload(
            "provider import batch size must be greater than zero".to_owned(),
        )
    })?;
    let ProviderNormalizationResult {
        summary,
        captures,
        files_touched,
    } = normalization;
    import_provider_capture_lines_with_batch_size(
        store,
        options,
        summary,
        captures,
        files_touched,
        Some(transaction_batch_size),
        true,
    )
}

fn import_provider_capture_lines_with_batch_size(
    store: &mut Store,
    options: NormalizedProviderImportOptions,
    mut summary: ProviderImportSummary,
    mut captures: Vec<(usize, ProviderCaptureEnvelope)>,
    mut files_touched: Vec<(usize, ProviderFileTouchedEnvelope)>,
    transaction_batch_size: Option<NonZeroUsize>,
    suppress_search_merges: bool,
) -> Result<ProviderImportSummary> {
    let caches = ProviderImportCaches::default();
    filter_provider_capture_lines_without_real_session_messages(
        &mut summary,
        &mut captures,
        &mut files_touched,
    );
    let supplied_file_touch_lines = files_touched
        .iter()
        .map(|(line_number, _)| *line_number)
        .collect::<BTreeSet<_>>();
    if summary.failed == 0 && !provider_capture_lines_have_real_message(&captures) {
        let line = captures
            .first()
            .map(|(line_number, _)| *line_number)
            .or_else(|| files_touched.first().map(|(line_number, _)| *line_number))
            .unwrap_or(0);
        summary.record_failure(ProviderImportFailure {
            line,
            error: "provider source contained no real conversation message".to_owned(),
        });
        return Ok(summary);
    }
    for (line_number, capture) in &captures {
        if capture.provider == CaptureProvider::Codex
            || supplied_file_touch_lines.contains(line_number)
        {
            continue;
        }
        if let Some(event) = &capture.event {
            let (inferred_touches, outcome) = provider_file_touches_from_event(
                capture.provider,
                &capture.session.provider_session_id,
                &capture.source.source_format,
                capture.source.raw_source_path.as_deref(),
                capture.source.source_root.as_deref(),
                event,
                *line_number,
            )
            .into_parts();
            files_touched.extend(inferred_touches);
            if outcome.limit_exceeded() {
                summary.record_failure(ProviderImportFailure {
                    line: *line_number,
                    error: PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
                });
            }
        }
    }
    let has_captures = !captures.is_empty() || !files_touched.is_empty();
    let bulk_search_mode = suppress_search_merges && has_captures && options.wrap_transaction;
    let bulk_search_guard = bulk_search_mode
        .then(|| store.begin_event_search_bulk_mode())
        .transpose()?;
    let import_result = persist_provider_capture_lines(
        store,
        &options,
        summary,
        captures,
        files_touched,
        has_captures,
        transaction_batch_size,
        caches,
    );
    let finish_result = match &bulk_search_guard {
        Some(guard) => store
            .finish_event_search_bulk_mode(guard)
            .map_err(CaptureError::from),
        None => Ok(()),
    };
    match (import_result, finish_result) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(err)) => Err(err),
        (Err(err), Ok(())) => Err(err),
    }
}

#[allow(clippy::too_many_arguments)]
fn persist_provider_capture_lines(
    store: &mut Store,
    options: &NormalizedProviderImportOptions,
    mut summary: ProviderImportSummary,
    captures: Vec<(usize, ProviderCaptureEnvelope)>,
    files_touched: Vec<(usize, ProviderFileTouchedEnvelope)>,
    has_captures: bool,
    transaction_batch_size: Option<NonZeroUsize>,
    mut caches: ProviderImportCaches,
) -> Result<ProviderImportSummary> {
    let pending_cursors = if options.persist_cursors && summary.failed == 0 {
        captures
            .iter()
            .filter_map(|(_, capture)| provider_sync_cursor(capture))
            .map(|cursor| (cursor.id, cursor))
            .collect::<BTreeMap<_, _>>()
    } else {
        BTreeMap::new()
    };
    let mut transaction = ProviderImportTransaction::begin(
        store,
        has_captures && options.wrap_transaction,
        transaction_batch_size,
    )?;
    for (line_number, capture) in captures {
        let unit_bytes = serialized_len_or_rollback(&mut transaction, store, &capture)?;
        transaction.prepare_unit(store, unit_bytes)?;
        match import_provider_capture_line(store, &capture, options, line_number, &mut caches) {
            Ok(line_summary) => summary.merge(line_summary),
            Err(err @ CaptureError::Store(_)) => {
                transaction.rollback(store);
                return Err(err);
            }
            Err(err) => record_deterministic_rejection(&mut summary, line_number, &err),
        }
        transaction.record_unit(store, unit_bytes)?;
    }
    for (line_number, file) in files_touched {
        let unit_bytes = serialized_len_or_rollback(&mut transaction, store, &file)?;
        transaction.prepare_unit(store, unit_bytes)?;
        match import_provider_file_touched_line(store, &file, options, None) {
            Ok(()) => summary.accepted_content_records += 1,
            Err(err @ CaptureError::Store(_)) => {
                transaction.rollback(store);
                return Err(err);
            }
            Err(err) => record_deterministic_rejection(&mut summary, line_number, &err),
        }
        transaction.record_unit(store, unit_bytes)?;
    }
    if summary.failed == 0 {
        for cursor in pending_cursors.into_values() {
            let unit_bytes = serialized_len_or_rollback(&mut transaction, store, &cursor)?;
            transaction.prepare_unit(store, unit_bytes)?;
            if let Err(err) = persist_provider_sync_cursor(store, &cursor) {
                transaction.rollback(store);
                return Err(err);
            }
            transaction.record_unit(store, unit_bytes)?;
        }
    }
    transaction.commit(store)?;
    Ok(summary)
}

fn record_deterministic_rejection(
    summary: &mut ProviderImportSummary,
    line_number: usize,
    error: &CaptureError,
) {
    summary.record_failure(ProviderImportFailure {
        line: line_number,
        error: error.to_string(),
    });
}

#[cfg(test)]
mod tests {
    include!("batches_support_tests.rs");
    include!("batches_recovery_tests.rs");
    include!("batches_projection_tests.rs");
}
