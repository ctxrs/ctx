use std::{
    fs,
    io::BufReader,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

#[cfg(test)]
use std::thread;

use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;

use crate::captured_batch::jsonl::{
    jsonl_position_offset, verify_jsonl_append_boundary, JsonlBatchError, JsonlBatchProducer,
};
use crate::captured_batch::{
    ProviderRecordKind, SourceObservation, CAPTURE_BATCH_MAX_BATCHES_PER_GROUP,
};
use crate::provider::importer::{
    captured_batch_cursor_stream, import_captured_batches, provider_path_identity,
    provider_source_cursor_stream_for_path, CapturedBatchCursorMode, CapturedSourceAdmission,
    CertifiedProviderCursor,
};
use crate::{
    CaptureError, CodexSessionImportOptions, NormalizedProviderImportOptions,
    ProviderAdapterContext, ProviderImportFailure, ProviderImportSummary, Result,
    CODEX_SESSION_SOURCE_FORMAT,
};

use super::projection::{CodexCapturedBatchProjector, CodexParserCheckpoint};
use super::resume::{
    codex_legacy_cursor_next_ordinal, codex_verified_append_cursor_mode,
    read_codex_anchored_header, read_codex_tail_header, validate_codex_tail_start_boundary,
    CodexTailHeaderBootstrap,
};
use super::selection::{codex_session_paths_total_bytes, report_codex_import_progress};
use super::source_file::{
    canonical_codex_source_path, open_codex_source_file, CodexFrozenFileMetadata,
};
use super::{CODEX_CAPTURE_REVISION, CODEX_POLICY_REVISION, CODEX_RECORD_KIND};

pub(super) fn import_codex_session_file_batched(
    path: &Path,
    store: &mut Store,
    options: &CodexSessionImportOptions,
    required_start_offset: Option<u64>,
    report_progress: bool,
) -> Result<ProviderImportSummary> {
    let context = ProviderAdapterContext {
        machine_id: options.machine_id.clone(),
        source_path: Some(path.to_path_buf()),
        source_root: options.source_path.clone(),
        imported_at: options.imported_at,
    };
    let import_options = NormalizedProviderImportOptions {
        history_record_id: options.history_record_id,
        persist_cursors: false,
        wrap_transaction: true,
        fast_event_inserts: options.fast_event_inserts,
        capture_work_limit: options.capture_work_limit,
        inventory_observation_token: options.inventory_observation_token.clone(),
    };
    let frozen = CodexFrozenFileMetadata::read(path)?;
    if frozen.length == 0 {
        if !frozen.revalidate(path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let mut summary = ProviderImportSummary::default();
        summary.record_failure(ProviderImportFailure {
            line: 1,
            error: format!(
                "codex session JSONL contained no real message content: {}",
                path.display()
            ),
        });
        if report_progress {
            report_codex_import_progress(options, 1, 0, 0, 0, &summary, false);
            report_codex_import_progress(options, 1, 0, 1, 0, &summary, true);
        }
        return Ok(summary);
    }
    let canonical_path = canonical_codex_source_path(path)?;
    let cursor_source_path = provider_path_identity(path)?;
    let canonical_path_identity = provider_path_identity(&canonical_path)?;
    let source = SourceObservation::new(
        CaptureProvider::Codex,
        CODEX_SESSION_SOURCE_FORMAT,
        format!("codex-session-jsonl-file:{canonical_path_identity}"),
        frozen.source_revision(),
        provider_source_cursor_stream_for_path(
            CaptureProvider::Codex,
            CODEX_SESSION_SOURCE_FORMAT,
            &cursor_source_path,
        ),
        CODEX_CAPTURE_REVISION,
        CODEX_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(codex_captured_batch_error)?;
    let source_item = canonical_path_identity.into_bytes();
    let record_kind =
        ProviderRecordKind::new(CODEX_RECORD_KIND).map_err(codex_captured_batch_error)?;
    let stream = captured_batch_cursor_stream(&source);
    let mut expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let mut cursor_mode = CapturedBatchCursorMode::Resume;
    let mut start_offset = 0_u64;
    let mut start_ordinal = 0_u64;
    let mut resumed_cursor = None;
    let mut bootstrap_tail_header = false;

    if let Some(stored_cursor) = expected_store_cursor.as_ref() {
        match CertifiedProviderCursor::decode_if_certified(&stored_cursor.cursor)? {
            Some(certified)
                if certified.parser_revision() == source.capture_revision()
                    && certified.policy_revision() == source.policy_revision() =>
            {
                let certified_offset = jsonl_position_offset(certified.native_position())
                    .map_err(codex_jsonl_batch_error)?;
                let can_resume = if certified.source_revision() == source.source_revision() {
                    true
                } else {
                    let file = open_codex_source_file(path)?;
                    if CodexFrozenFileMetadata::from_metadata(&file.metadata()?)? != frozen {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    let mut reader = BufReader::new(file);
                    match verify_jsonl_append_boundary(
                        &mut reader,
                        certified.native_position(),
                        &source,
                        frozen.length,
                    ) {
                        Ok(verified_append) => {
                            cursor_mode = codex_verified_append_cursor_mode(verified_append);
                            true
                        }
                        Err(JsonlBatchError::Io(error)) => return Err(CaptureError::Io(error)),
                        Err(_) => false,
                    }
                };
                if can_resume {
                    start_offset = certified_offset;
                    resumed_cursor = Some(certified);
                } else {
                    cursor_mode = CapturedBatchCursorMode::ResetChangedSource;
                }
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => {
                cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor;
                if let Some(required) = required_start_offset {
                    start_offset = required;
                    start_ordinal = codex_legacy_cursor_next_ordinal(&stored_cursor.cursor)?;
                    bootstrap_tail_header = true;
                }
            }
        }
    } else if let Some(required) = required_start_offset {
        start_offset = required;
        bootstrap_tail_header = true;
    }

    if !frozen.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let file = open_codex_source_file(path)?;
    if CodexFrozenFileMetadata::from_metadata(&file.metadata()?)? != frozen {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut reader = BufReader::new(file);
    let mut projector = if let Some(certified) = resumed_cursor.as_ref() {
        let checkpoint: CodexParserCheckpoint = certified.parser_checkpoint().deserialize()?;
        let header = checkpoint
            .header_anchor
            .as_ref()
            .map(|anchor| read_codex_anchored_header(&mut reader, anchor))
            .transpose()?
            .flatten();
        if checkpoint.header_anchor.is_some() && header.is_none() {
            if certified.source_revision() == source.source_revision() {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            cursor_mode = CapturedBatchCursorMode::ResetChangedSource;
            start_offset = 0;
            start_ordinal = 0;
            CodexCapturedBatchProjector::fresh(context.clone())
        } else {
            let mut projector =
                CodexCapturedBatchProjector::resume(context.clone(), certified, header.clone())?;
            if let Some(header) = header.as_ref() {
                projector
                    .correlation
                    .hydrate_from_store(store, &projector.context, header)?;
            }
            start_ordinal = projector.next_ordinal;
            projector
        }
    } else {
        CodexCapturedBatchProjector::fresh(context.clone())
    };
    if bootstrap_tail_header {
        validate_codex_tail_start_boundary(&mut reader, start_offset)?;
        let (header, header_end) = match read_codex_tail_header(&mut reader)? {
            CodexTailHeaderBootstrap::Ready {
                header,
                header_end,
                header_anchor,
            } => {
                projector.header_anchor = Some(header_anchor);
                (*header, header_end)
            }
            CodexTailHeaderBootstrap::Skipped(summary) => {
                if report_progress {
                    let progress_total = frozen.length.saturating_sub(start_offset);
                    report_codex_import_progress(options, 1, progress_total, 0, 0, &summary, false);
                    report_codex_import_progress(
                        options,
                        1,
                        progress_total,
                        1,
                        progress_total,
                        &summary,
                        true,
                    );
                }
                return Ok(summary);
            }
        };
        if expected_store_cursor.is_none() {
            if start_offset != header_end {
                return Err(CaptureError::InvalidPayload(
                    "Codex tail import without a stored cursor must start immediately after session_meta"
                        .to_owned(),
                ));
            }
            start_ordinal = 1;
        }
        projector.next_ordinal = start_ordinal;
        projector.header = Some(header);
    }
    let mut producer = JsonlBatchProducer::new(
        reader,
        source.clone(),
        source_item,
        record_kind,
        frozen.length,
        start_offset,
        start_ordinal,
        false,
    )
    .map_err(codex_jsonl_batch_error)?;
    let producer_initial_position = producer.current_position().clone();
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context)?;
    let mut first_tail_batch = producer.next_batch().map_err(codex_jsonl_batch_error)?;
    let initial_position = first_tail_batch
        .as_ref()
        .map(|batch| batch.range_before().clone())
        .unwrap_or(producer_initial_position);
    let max_batches = NonZeroUsize::new(CAPTURE_BATCH_MAX_BATCHES_PER_GROUP).ok_or(
        CaptureError::SystemInvariant("captured batch group limit must be nonzero"),
    )?;
    let progress_start = required_start_offset
        .filter(|required| *required == start_offset)
        .unwrap_or(0);
    let progress_total = frozen.length.saturating_sub(progress_start);
    let mut completed_offset = start_offset;
    let mut merged = ProviderImportSummary::default();
    let mut imported_any = false;
    if report_progress {
        report_codex_import_progress(options, 1, progress_total, 0, 0, &merged, false);
    }
    if first_tail_batch.is_none() && matches!(cursor_mode, CapturedBatchCursorMode::ResumeAppend(_))
    {
        if !frozen.revalidate(path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let mut summary = if required_start_offset.is_some() {
            ProviderImportSummary::default()
        } else {
            projector.replay_summary()?
        };
        record_incomplete_codex_tail(&mut summary, &projector, completed_offset, frozen.length);
        if report_progress {
            report_codex_import_progress(
                options,
                1,
                progress_total,
                usize::from(completed_offset == frozen.length),
                completed_offset.saturating_sub(progress_start),
                &summary,
                completed_offset == frozen.length,
            );
        }
        return Ok(summary);
    }
    loop {
        let outcome = import_captured_batches(
            store,
            &admission,
            import_options.clone(),
            &context.machine_id,
            context.imported_at,
            expected_store_cursor.as_ref(),
            &initial_position,
            cursor_mode,
            max_batches,
            &mut projector,
            || {
                let batch = match first_tail_batch.take() {
                    Some(batch) => Some(batch),
                    None => producer.next_batch().map_err(codex_jsonl_batch_error)?,
                };
                if let Some(batch) = &batch {
                    completed_offset = jsonl_position_offset(batch.range_end())
                        .map_err(codex_jsonl_batch_error)?;
                }
                Ok(batch)
            },
            || frozen.revalidate(path),
        )?;
        if outcome.batches_imported == 0 {
            let work_result = if imported_any {
                merged.work_result().merge(outcome.summary.work_result())
            } else {
                outcome.summary.work_result()
            };
            let mut summary = if imported_any {
                merged
            } else if required_start_offset.is_some() {
                ProviderImportSummary::default()
            } else if expected_store_cursor.is_some() {
                projector.replay_summary()?
            } else {
                merged
            };
            summary.set_work_result(work_result);
            record_incomplete_codex_tail(&mut summary, &projector, completed_offset, frozen.length);
            if report_progress {
                report_codex_import_progress(
                    options,
                    1,
                    progress_total,
                    usize::from(completed_offset == frozen.length),
                    completed_offset.saturating_sub(progress_start),
                    &summary,
                    completed_offset == frozen.length,
                );
            }
            return Ok(summary);
        }
        imported_any = true;
        merged.merge(outcome.summary);
        if report_progress {
            report_codex_import_progress(
                options,
                1,
                progress_total,
                usize::from(outcome.source_exhausted && completed_offset == frozen.length),
                completed_offset.saturating_sub(progress_start),
                &merged,
                outcome.source_exhausted && completed_offset == frozen.length,
            );
        }
        if outcome.source_exhausted {
            record_incomplete_codex_tail(&mut merged, &projector, completed_offset, frozen.length);
            return Ok(merged);
        }
        if import_options.capture_work_limit == crate::CaptureWorkLimit::OneSafeGroup {
            merged.work_remaining = true;
            return Ok(merged);
        }
        expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
        if expected_store_cursor.is_none() {
            return Err(CaptureError::SystemInvariant(
                "published Codex captured-batch cursor could not be reloaded",
            ));
        }
        cursor_mode = CapturedBatchCursorMode::Resume;
    }
}

fn record_incomplete_codex_tail(
    summary: &mut ProviderImportSummary,
    projector: &CodexCapturedBatchProjector,
    completed_offset: u64,
    observed_length: u64,
) {
    if completed_offset >= observed_length {
        return;
    }
    let line = usize::try_from(projector.next_ordinal)
        .ok()
        .and_then(|ordinal| ordinal.checked_add(1))
        .unwrap_or(usize::MAX);
    summary.record_failure(ProviderImportFailure {
        line,
        error: "Codex session JSONL ended with an incomplete record".to_owned(),
    });
}

pub(super) fn codex_jsonl_batch_error(error: JsonlBatchError) -> CaptureError {
    match error {
        JsonlBatchError::Io(error) => CaptureError::Io(error),
        JsonlBatchError::SourceChangedDuringRead { .. } => CaptureError::SourceChangedDuringCapture,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

fn codex_captured_batch_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}
pub(super) fn import_codex_session_paths_batched(
    paths: Vec<PathBuf>,
    store: &mut Store,
    options: &CodexSessionImportOptions,
    skipped_by_bounds: usize,
) -> Result<ProviderImportSummary> {
    let mut merged = ProviderImportSummary::default();
    merged.skipped_sessions += skipped_by_bounds;
    merged.skipped += skipped_by_bounds;
    let total_files = paths.len();
    let total_bytes = codex_session_paths_total_bytes(&paths);
    let mut completed_files = 0usize;
    let mut completed_bytes = 0u64;
    report_codex_import_progress(
        options,
        total_files,
        total_bytes,
        completed_files,
        completed_bytes,
        &merged,
        false,
    );
    for path in paths {
        let file_bytes = fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        merged.merge(import_codex_session_file_batched(
            &path, store, options, None, false,
        )?);
        completed_files = completed_files.saturating_add(1);
        completed_bytes = completed_bytes.saturating_add(file_bytes);
        report_codex_import_progress(
            options,
            total_files,
            total_bytes,
            completed_files,
            completed_bytes,
            &merged,
            false,
        );
    }
    report_codex_import_progress(
        options,
        total_files,
        total_bytes,
        completed_files,
        completed_bytes,
        &merged,
        true,
    );
    Ok(merged)
}

#[cfg(test)]
pub(crate) fn join_codex_import_worker<T>(
    handle: thread::ScopedJoinHandle<'_, Result<T>>,
) -> Result<T> {
    handle
        .join()
        .map_err(|_| CaptureError::WorkerPanicked("Codex import"))?
}
