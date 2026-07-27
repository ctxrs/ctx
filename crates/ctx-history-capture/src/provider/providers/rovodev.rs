use std::{num::NonZeroUsize, path::Path};

use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;

use crate::captured_batch::whole_json::{
    WholeJsonBatchError, WholeJsonBatchProducer, WholeJsonItem,
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
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    Result, ROVODEV_SOURCE_FORMAT,
};

mod event;
mod source;
mod whole_json;

use source::{
    read_rovodev_metadata, visit_rovodev_session_sources, RovoDevSessionObservation,
    RovoDevSessionSource,
};
use whole_json::{whole_json_position, whole_json_position_ordinal, RovoDevCapturedBatchProjector};

const ROVODEV_CAPTURE_REVISION: u32 = 2;
const ROVODEV_POLICY_REVISION: u32 = 5;
const ROVODEV_RECORD_KIND: &str = "rovodev-session-context-json-v1";

pub(crate) fn import_rovodev_sessions_batched(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let mut merged = ProviderImportSummary::default();
    let source_count = visit_rovodev_session_sources(path, &mut |source| {
        let summary =
            import_rovodev_session_file_batched(source, store, &context, &import_options)?;
        merged.merge(summary);
        Ok(())
    })?;
    if source_count == 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no Rovo Dev session_context.json files found",
        });
    }
    Ok(merged)
}

fn import_rovodev_session_file_batched(
    source_path: RovoDevSessionSource,
    store: &mut Store,
    context: &ProviderAdapterContext,
    import_options: &NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let observation = RovoDevSessionObservation::read(&source_path)?;
    let path_identity = provider_path_identity(observation.canonical_path())?;
    let file_context = ProviderAdapterContext {
        machine_id: context.machine_id.clone(),
        source_path: Some(source_path.context_path.clone()),
        source_root: context
            .source_root
            .clone()
            .or_else(|| context.source_path.clone()),
        imported_at: context.imported_at,
    };
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::RovoDev,
        ROVODEV_SOURCE_FORMAT,
        &path_identity,
    );
    let source = SourceObservation::new(
        CaptureProvider::RovoDev,
        ROVODEV_SOURCE_FORMAT,
        format!("rovodev-session-file:{path_identity}"),
        observation.source_revision(),
        cursor_stream,
        ROVODEV_CAPTURE_REVISION,
        ROVODEV_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(rovodev_captured_batch_error)?;
    let record_kind =
        ProviderRecordKind::new(ROVODEV_RECORD_KIND).map_err(rovodev_captured_batch_error)?;
    let initial_position = whole_json_position(0)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let (metadata, metadata_failure) = read_rovodev_metadata(&source_path);
    let mut cursor_mode = CapturedBatchCursorMode::Resume;
    let mut resumable_cursor = None;
    if let Some(stored_cursor) = expected_store_cursor.as_ref() {
        match CertifiedProviderCursor::decode_if_certified(&stored_cursor.cursor)? {
            Some(certified)
                if certified.source_revision() == source.source_revision()
                    && certified.parser_revision() == source.capture_revision()
                    && certified.policy_revision() == source.policy_revision() =>
            {
                let ordinal = whole_json_position_ordinal(certified.native_position())?;
                if ordinal > 1 {
                    return Err(CaptureError::InvalidPayload(
                        "Rovo Dev per-file cursor exceeds its source".to_owned(),
                    ));
                }
                let projector = RovoDevCapturedBatchProjector::resume(
                    file_context.clone(),
                    source_path.clone(),
                    metadata.clone(),
                    metadata_failure.clone(),
                    &certified,
                )?;
                if ordinal == 1 {
                    if !observation.revalidate(&source_path)? {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    return projector.replay_summary();
                }
                resumable_cursor = Some(certified);
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }

    let mut emitted = false;
    let item_path = observation.context_path().to_path_buf();
    let item_length = observation.context_length();
    let source_item = path_identity.into_bytes();
    let mut producer = WholeJsonBatchProducer::new(source.clone(), record_kind, move || {
        if emitted {
            return Ok(None);
        }
        emitted = true;
        WholeJsonItem::new(0, source_item.clone(), item_length, item_path.clone()).map(Some)
    })
    .map_err(rovodev_whole_json_error)?;
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &file_context)?;
    let batch = producer
        .next_batch()
        .map_err(rovodev_whole_json_error)?
        .ok_or(CaptureError::SystemInvariant(
            "Rovo Dev per-file producer returned no captured batch",
        ))?;
    let mut projector = match resumable_cursor.as_ref() {
        Some(cursor) if cursor_mode == CapturedBatchCursorMode::Resume => {
            RovoDevCapturedBatchProjector::resume(
                file_context.clone(),
                source_path.clone(),
                metadata,
                metadata_failure,
                cursor,
            )?
        }
        _ => RovoDevCapturedBatchProjector::fresh(
            file_context.clone(),
            source_path.clone(),
            metadata,
            metadata_failure,
        ),
    };
    let mut pending_batch = Some(batch);
    let max_batches = NonZeroUsize::new(CAPTURE_BATCH_MAX_BATCHES_PER_GROUP).ok_or(
        CaptureError::SystemInvariant("captured batch group limit must be nonzero"),
    )?;
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
        || Ok(pending_batch.take()),
        || observation.revalidate(&source_path),
    )?;
    if outcome.batches_imported != 1 || !outcome.source_exhausted {
        return Err(CaptureError::SystemInvariant(
            "Rovo Dev per-file import did not consume exactly one batch",
        ));
    }
    Ok(outcome.summary)
}

fn rovodev_whole_json_error(error: WholeJsonBatchError) -> CaptureError {
    match error {
        WholeJsonBatchError::Io(error) => CaptureError::Io(error),
        WholeJsonBatchError::SourceSizeChanged { .. }
        | WholeJsonBatchError::SourceMetadataChangedDuringRead => {
            CaptureError::SourceChangedDuringCapture
        }
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

fn rovodev_captured_batch_error(error: crate::captured_batch::CapturedBatchError) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

#[cfg(test)]
mod tests;
