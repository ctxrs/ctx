use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
};

use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;

use crate::captured_batch::jsonl::{
    initial_jsonl_position, jsonl_position_offset, verify_jsonl_append_boundary, JsonlBatchError,
    JsonlBatchProducer,
};
use crate::captured_batch::{
    CapturedBatch, CapturedBatchBuilder, CapturedRecord, NativeLocator, NativePosition,
    ProviderRecordKind, SourceObservation,
};
use crate::provider::importer::{
    captured_batch_cursor_stream, drain_captured_batches, provider_path_identity,
    provider_source_cursor_stream_for_path, CapturedBatchCursorMode, CapturedSourceAdmission,
    CertifiedProviderCursor,
};
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    Result, JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
};

use super::{
    coalesce_junie_session_summary, junie_captured_batch_error, junie_jsonl_batch_error,
    projector::JunieCapturedBatchProjector,
    session_tree::JunieSessionPath,
    source::{JunieFrozenFileMetadata, JunieSessionObservation},
    JUNIE_CAPTURE_REVISION, JUNIE_END_LOCATOR_KIND, JUNIE_END_RECORD_KIND, JUNIE_POLICY_REVISION,
    JUNIE_RECORD_KIND,
};

pub(super) struct JunieCapturedBatchProducer {
    pub(super) inner: JsonlBatchProducer<BufReader<File>>,
    pub(super) source: SourceObservation,
    pub(super) source_item: Vec<u8>,
    pub(super) end_record_kind: ProviderRecordKind,
    pub(super) current_position: NativePosition,
    pub(super) next_ordinal: u64,
    pub(super) emit_end_record: bool,
}

impl JunieCapturedBatchProducer {
    pub(super) fn next_batch(&mut self) -> Result<Option<CapturedBatch>> {
        if let Some(batch) = self.inner.next_batch().map_err(junie_jsonl_batch_error)? {
            let first_ordinal = batch.records().first().map(CapturedRecord::ordinal).ok_or(
                CaptureError::SystemInvariant("Junie JSONL producer returned an empty batch"),
            )?;
            if first_ordinal != self.next_ordinal {
                return Err(CaptureError::SystemInvariant(
                    "Junie JSONL producer skipped a captured record ordinal",
                ));
            }
            self.next_ordinal = batch
                .records()
                .last()
                .and_then(|record| record.ordinal().checked_add(1))
                .ok_or(CaptureError::SystemInvariant(
                    "Junie JSONL producer overflowed its captured record ordinal",
                ))?;
            self.current_position = batch.range_end().clone();
            return Ok(Some(if self.emit_end_record {
                batch.into_source_continues()
            } else {
                batch
            }));
        }
        if !self.emit_end_record {
            return Ok(None);
        }
        self.emit_end_record = false;
        let locator = NativeLocator::new(JUNIE_END_LOCATOR_KIND, self.source_item.clone())
            .map_err(junie_captured_batch_error)?;
        let record = CapturedRecord::content(
            self.next_ordinal,
            locator,
            self.end_record_kind.clone(),
            Vec::new(),
        )
        .map_err(junie_captured_batch_error)?;
        self.next_ordinal =
            self.next_ordinal
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Junie end record ordinal overflowed",
                ))?;
        let mut builder =
            CapturedBatchBuilder::new(self.source.clone(), self.current_position.clone());
        builder.push(record).map_err(junie_captured_batch_error)?;
        builder.mark_source_exhausted();
        builder
            .finish(self.current_position.clone())
            .map(Some)
            .map_err(junie_captured_batch_error)
    }
}

pub(super) fn import_junie_session_events_file_batched(
    session_path: &JunieSessionPath,
    session_ordinal: usize,
    store: &mut Store,
    context: &ProviderAdapterContext,
    import_options: &NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let observation = JunieSessionObservation::read(session_path)?;
    let cursor_source_path = provider_path_identity(&session_path.events_path)?;
    let canonical_path_identity = provider_path_identity(&observation.canonical_path)?;
    let file_context = ProviderAdapterContext {
        machine_id: context.machine_id.clone(),
        source_path: Some(session_path.events_path.clone()),
        source_root: context
            .source_root
            .clone()
            .or_else(|| context.source_path.clone()),
        imported_at: context.imported_at,
    };
    let source = SourceObservation::new(
        CaptureProvider::Junie,
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        format!("junie-session-events:{canonical_path_identity}"),
        observation.source_revision(),
        provider_source_cursor_stream_for_path(
            CaptureProvider::Junie,
            JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            &cursor_source_path,
        ),
        JUNIE_CAPTURE_REVISION,
        JUNIE_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(junie_captured_batch_error)?;
    let record_kind =
        ProviderRecordKind::new(JUNIE_RECORD_KIND).map_err(junie_captured_batch_error)?;
    let end_record_kind =
        ProviderRecordKind::new(JUNIE_END_RECORD_KIND).map_err(junie_captured_batch_error)?;
    let source_item = canonical_path_identity.into_bytes();
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &file_context.machine_id, &stream)?;
    let had_expected_store_cursor = expected_store_cursor.is_some();
    let initial_position = initial_jsonl_position().map_err(junie_jsonl_batch_error)?;
    let mut start_position = initial_position.clone();
    let mut cursor_mode = CapturedBatchCursorMode::Resume;
    let mut start_offset = 0_u64;
    let mut start_ordinal = 0_u64;
    let mut projector = JunieCapturedBatchProjector::fresh(
        session_path,
        file_context.clone(),
        session_ordinal,
        observation.auxiliary_revision,
    )?;

    if let Some(stored_cursor) = expected_store_cursor.as_ref() {
        match CertifiedProviderCursor::decode_if_certified(&stored_cursor.cursor)? {
            Some(certified)
                if certified.parser_revision() == source.capture_revision()
                    && certified.policy_revision() == source.policy_revision() =>
            {
                let resumed = JunieCapturedBatchProjector::resume(
                    session_path,
                    file_context.clone(),
                    session_ordinal,
                    &certified,
                    certified.source_revision() != source.source_revision(),
                )?;
                if let Some(mut resumed) = resumed {
                    let auxiliary_unchanged =
                        resumed.state.auxiliary_revision == observation.auxiliary_revision;
                    let can_resume = if certified.source_revision() == source.source_revision()
                        && auxiliary_unchanged
                    {
                        true
                    } else if auxiliary_unchanged {
                        let file = File::open(&session_path.events_path)?;
                        if JunieFrozenFileMetadata::from_metadata(&file.metadata()?)?
                            != observation.events_file
                        {
                            return Err(CaptureError::SourceChangedDuringCapture);
                        }
                        let mut reader = BufReader::new(file);
                        match verify_jsonl_append_boundary(
                            &mut reader,
                            certified.native_position(),
                            &source,
                            observation.events_file.length,
                        ) {
                            Ok(verified_append) => {
                                cursor_mode =
                                    CapturedBatchCursorMode::ResumeAppend(verified_append);
                                true
                            }
                            Err(JsonlBatchError::Io(error)) => {
                                return Err(CaptureError::Io(error));
                            }
                            Err(_) => false,
                        }
                    } else {
                        false
                    };
                    if can_resume {
                        start_offset = jsonl_position_offset(certified.native_position())
                            .map_err(junie_jsonl_batch_error)?;
                        start_position = certified.native_position().clone();
                        start_ordinal = resumed.state.next_ordinal;
                        // The terminal marker closes an otherwise open Junie turn, but it is
                        // not a native source record. When the file subsequently grows, reuse
                        // that marker's ordinal for the first appended JSONL record so a
                        // resumed import has the same canonical record coordinates as a
                        // one-shot import of the final file.
                        if matches!(cursor_mode, CapturedBatchCursorMode::ResumeAppend(_))
                            && resumed.state.source_ended
                            && start_offset < observation.events_file.length
                        {
                            start_ordinal = start_ordinal.checked_sub(1).ok_or(
                                CaptureError::SystemInvariant(
                                    "Junie terminal record did not have a prior ordinal",
                                ),
                            )?;
                            resumed.state.next_ordinal = start_ordinal;
                            resumed.state.source_ended = false;
                        }
                        projector = resumed;
                    } else {
                        cursor_mode = CapturedBatchCursorMode::ResetChangedSource;
                    }
                } else {
                    cursor_mode = CapturedBatchCursorMode::ResetChangedSource;
                }
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }

    if !observation.revalidate(session_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut file = File::open(&session_path.events_path)?;
    if JunieFrozenFileMetadata::from_metadata(&file.metadata()?)? != observation.events_file {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let ends_at_record_boundary =
        junie_file_ends_at_record_boundary(&mut file, observation.events_file.length)?;
    let inner = JsonlBatchProducer::new(
        BufReader::new(file),
        source.clone(),
        source_item.clone(),
        record_kind,
        observation.events_file.length,
        start_offset,
        start_ordinal,
        false,
    )
    .map_err(junie_jsonl_batch_error)?;
    let emit_end_record = ends_at_record_boundary
        && (!projector.state.source_ended || start_offset < observation.events_file.length);
    let mut producer = JunieCapturedBatchProducer {
        inner,
        source: source.clone(),
        source_item,
        end_record_kind,
        current_position: start_position,
        next_ordinal: start_ordinal,
        emit_end_record,
    };
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &file_context)?;
    let mut imported_any = false;
    let summary = drain_captured_batches(
        store,
        &admission,
        import_options.clone(),
        &file_context.machine_id,
        file_context.imported_at,
        expected_store_cursor,
        &initial_position,
        cursor_mode,
        &stream,
        &mut projector,
        || {
            let batch = producer.next_batch()?;
            imported_any |= batch.is_some();
            Ok(batch)
        },
        || observation.revalidate(session_path),
    )?;
    let summary = if !imported_any && had_expected_store_cursor {
        projector.replay_summary()?
    } else {
        summary
    };
    Ok(coalesce_junie_session_summary(summary))
}

fn junie_file_ends_at_record_boundary(file: &mut File, length: u64) -> Result<bool> {
    if length == 0 {
        return Ok(true);
    }
    file.seek(SeekFrom::Start(length - 1))?;
    let mut byte = [0_u8; 1];
    file.read_exact(&mut byte)?;
    Ok(byte[0] == b'\n')
}
