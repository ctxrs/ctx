use std::{fs::File, io::BufReader, path::PathBuf};

use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;

use crate::captured_batch::jsonl::{
    initial_jsonl_position, jsonl_position_offset, verify_jsonl_append_boundary, JsonlBatchProducer,
};
use crate::captured_batch::{ProviderRecordKind, SourceObservation};
use crate::provider::importer::{
    captured_batch_cursor_stream, drain_captured_batches, provider_path_identity,
    provider_source_cursor_stream_for_path, CapturedBatchCursorMode, CapturedSourceAdmission,
    CertifiedProviderCursor,
};
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    Result, MUX_SOURCE_FORMAT,
};

use super::metadata::mux_bounded_session_metadata;
use super::projector::{MuxCapturedBatchProjector, MuxCapturedStreamKind};
use super::source::{MuxFileObservation, MuxFrozenFile, MuxSessionSource};
use super::{
    mux_captured_batch_error, mux_file_context, mux_jsonl_batch_error, MUX_CAPTURE_REVISION,
    MUX_CHAT_RECORD_KIND, MUX_POLICY_REVISION,
};

pub(super) fn import_mux_chat_batched(
    session_source: MuxSessionSource,
    chat_path: PathBuf,
    store: &mut Store,
    context: &ProviderAdapterContext,
    import_options: &NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let observation =
        MuxFileObservation::read(&chat_path, session_source.metadata_path.as_deref())?;
    let path_identity = provider_path_identity(&observation.canonical_path)?;
    let source = SourceObservation::new(
        CaptureProvider::Mux,
        MUX_SOURCE_FORMAT,
        format!("mux-chat-jsonl:{path_identity}"),
        observation.source_revision("chat-jsonl"),
        provider_source_cursor_stream_for_path(
            CaptureProvider::Mux,
            MUX_SOURCE_FORMAT,
            &path_identity,
        ),
        MUX_CAPTURE_REVISION,
        MUX_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(mux_captured_batch_error)?;
    let record_kind =
        ProviderRecordKind::new(MUX_CHAT_RECORD_KIND).map_err(mux_captured_batch_error)?;
    let initial_position = initial_jsonl_position().map_err(mux_jsonl_batch_error)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let had_expected_store_cursor = expected_store_cursor.is_some();
    let file_context = mux_file_context(context, &chat_path);
    let bounded_session = mux_bounded_session_metadata(
        &session_source,
        &observation.metadata_revision(),
        context.imported_at,
    )?;
    let mut cursor_mode = CapturedBatchCursorMode::Resume;
    let mut start_offset = 0_u64;
    let mut start_ordinal = 0_u64;
    let mut resumed_projector = None;

    if let Some(stored_cursor) = expected_store_cursor.as_ref() {
        match CertifiedProviderCursor::decode_if_certified(&stored_cursor.cursor)? {
            Some(certified)
                if certified.parser_revision() == source.capture_revision()
                    && certified.policy_revision() == source.policy_revision() =>
            {
                let projector = MuxCapturedBatchProjector::resume(
                    file_context.clone(),
                    session_source.clone(),
                    chat_path.clone(),
                    MuxCapturedStreamKind::Chat,
                    bounded_session.clone(),
                    &certified,
                )?;
                let metadata_unchanged =
                    projector.certified_metadata_revision == bounded_session.metadata_revision;
                let can_resume = if metadata_unchanged
                    && certified.source_revision() == source.source_revision()
                {
                    true
                } else if metadata_unchanged {
                    let file = File::open(&chat_path)?;
                    if MuxFrozenFile::from_metadata(&file.metadata()?)? != observation.content {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    let mut reader = BufReader::new(file);
                    match verify_jsonl_append_boundary(
                        &mut reader,
                        certified.native_position(),
                        &source,
                        observation.content.length,
                    ) {
                        Ok(verified) => {
                            cursor_mode = CapturedBatchCursorMode::ResumeAppend(verified);
                            true
                        }
                        Err(crate::captured_batch::jsonl::JsonlBatchError::Io(error)) => {
                            return Err(CaptureError::Io(error));
                        }
                        Err(_) => false,
                    }
                } else {
                    false
                };
                if can_resume {
                    start_offset = jsonl_position_offset(certified.native_position())
                        .map_err(mux_jsonl_batch_error)?;
                    start_ordinal = projector.next_ordinal;
                    resumed_projector = Some(projector);
                } else {
                    cursor_mode = CapturedBatchCursorMode::ResetChangedSource;
                }
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }

    let mut projector = resumed_projector.unwrap_or_else(|| {
        MuxCapturedBatchProjector::fresh(
            file_context.clone(),
            session_source.clone(),
            chat_path.clone(),
            MuxCapturedStreamKind::Chat,
            bounded_session,
        )
    });
    if !observation.revalidate(&chat_path, session_source.metadata_path.as_deref())? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let file = File::open(&chat_path)?;
    if MuxFrozenFile::from_metadata(&file.metadata()?)? != observation.content {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut producer = JsonlBatchProducer::new(
        BufReader::new(file),
        source.clone(),
        path_identity.into_bytes(),
        record_kind,
        observation.content.length,
        start_offset,
        start_ordinal,
        false,
    )
    .map_err(mux_jsonl_batch_error)?;
    let admission = CapturedSourceAdmission::file_for_context(&source, &file_context)?;
    let mut imported_any = false;
    let summary = drain_captured_batches(
        store,
        &admission,
        import_options.clone(),
        &context.machine_id,
        context.imported_at,
        expected_store_cursor,
        &initial_position,
        cursor_mode,
        &stream,
        &mut projector,
        || {
            let batch = producer.next_batch().map_err(mux_jsonl_batch_error)?;
            imported_any |= batch.is_some();
            Ok(batch)
        },
        || observation.revalidate(&chat_path, session_source.metadata_path.as_deref()),
    )?;
    if !imported_any && had_expected_store_cursor {
        projector.replay_summary()
    } else {
        Ok(summary)
    }
}
