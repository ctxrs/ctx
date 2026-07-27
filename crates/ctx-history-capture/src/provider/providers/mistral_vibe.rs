use std::{fs::File, io::BufReader, path::Path};

use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;

use crate::captured_batch::jsonl::{
    initial_jsonl_position, jsonl_position_offset, verify_jsonl_append_boundary, JsonlBatchError,
    JsonlBatchProducer,
};
use crate::captured_batch::{ProviderRecordKind, SourceObservation};
use crate::provider::importer::{
    captured_batch_cursor_stream, drain_captured_batches, provider_path_identity,
    provider_source_cursor_stream_for_path, CapturedBatchCursorMode, CapturedSourceAdmission,
    CertifiedProviderCursor,
};
use crate::provider::providers::native_jsonl::native_jsonl_missing_reason;
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    Result, MISTRAL_VIBE_SOURCE_FORMAT,
};

mod projector;
mod schema;
mod source;

#[cfg(test)]
mod tests;

use self::projector::MistralVibeCapturedBatchProjector;
use self::schema::mistral_vibe_bounded_metadata;
pub(crate) use self::schema::mistral_vibe_result_content;
use self::source::{
    visit_mistral_vibe_session_sources, MistralVibeFrozenFile, MistralVibeSessionObservation,
    MistralVibeSessionSource,
};

const MISTRAL_VIBE_CAPTURE_REVISION: u32 = 3;
const MISTRAL_VIBE_POLICY_REVISION: u32 = 6;
const MISTRAL_VIBE_RECORD_KIND: &str = "mistral-vibe-message-jsonl-v1";
const MISTRAL_VIBE_MAX_ID_BYTES: usize = 4 * 1024;
pub(crate) const MISTRAL_VIBE_RESULT_CONTENT_PROFILE: &str = "mistral-vibe.result-body.v1";

pub(crate) fn import_mistral_vibe_sessions_batched(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let mut merged = ProviderImportSummary::default();
    let source_count = visit_mistral_vibe_session_sources(path, &mut |source| {
        merged.merge(import_mistral_vibe_session_file_batched(
            source,
            store,
            &context,
            &import_options,
        )?);
        Ok(())
    })?;
    if source_count == 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: native_jsonl_missing_reason(CaptureProvider::MistralVibe),
        });
    }
    Ok(merged)
}

fn import_mistral_vibe_session_file_batched(
    session_source: MistralVibeSessionSource,
    store: &mut Store,
    context: &ProviderAdapterContext,
    import_options: &NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let observation = MistralVibeSessionObservation::read(&session_source)?;
    let path_identity = provider_path_identity(&observation.canonical_messages_path)?;
    let file_context = ProviderAdapterContext {
        machine_id: context.machine_id.clone(),
        source_path: Some(session_source.messages_path.clone()),
        source_root: context
            .source_root
            .clone()
            .or_else(|| context.source_path.clone()),
        imported_at: context.imported_at,
    };
    let source = SourceObservation::new(
        CaptureProvider::MistralVibe,
        MISTRAL_VIBE_SOURCE_FORMAT,
        format!("mistral-vibe-session-file:{path_identity}"),
        observation.source_revision(),
        provider_source_cursor_stream_for_path(
            CaptureProvider::MistralVibe,
            MISTRAL_VIBE_SOURCE_FORMAT,
            &path_identity,
        ),
        MISTRAL_VIBE_CAPTURE_REVISION,
        MISTRAL_VIBE_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(mistral_vibe_captured_batch_error)?;
    let complete_content_binding = crate::complete_content::jsonl::ExactJsonlSourceBinding::new(
        source.source_revision(),
        &path_identity,
    );
    let record_kind = ProviderRecordKind::new(MISTRAL_VIBE_RECORD_KIND)
        .map_err(mistral_vibe_captured_batch_error)?;
    let initial_position = initial_jsonl_position().map_err(mistral_vibe_jsonl_batch_error)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let had_expected_store_cursor = expected_store_cursor.is_some();
    let (metadata, metadata_failure) =
        mistral_vibe_bounded_metadata(&session_source, context.imported_at)?;
    let metadata_revision = observation.metadata_revision();
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
                let projector = MistralVibeCapturedBatchProjector::resume(
                    file_context.clone(),
                    session_source.clone(),
                    metadata.clone(),
                    metadata_failure.clone(),
                    &certified,
                    complete_content_binding.clone(),
                )?;
                let can_resume = if projector.metadata_revision == metadata_revision {
                    let same_source = certified.source_revision() == source.source_revision();
                    let append = if same_source {
                        None
                    } else {
                        let file = File::open(&session_source.messages_path)?;
                        if MistralVibeFrozenFile::from_metadata(&file.metadata()?)?
                            != observation.messages_file
                        {
                            return Err(CaptureError::SourceChangedDuringCapture);
                        }
                        let mut reader = BufReader::new(file);
                        match verify_jsonl_append_boundary(
                            &mut reader,
                            certified.native_position(),
                            &source,
                            observation.messages_file.length,
                        ) {
                            Ok(verified) => Some(verified),
                            Err(JsonlBatchError::Io(error)) => {
                                return Err(CaptureError::Io(error));
                            }
                            Err(_) => None,
                        }
                    };
                    if same_source || append.is_some() {
                        if let Some(verified) = append {
                            cursor_mode = CapturedBatchCursorMode::ResumeAppend(verified);
                        }
                        start_offset = jsonl_position_offset(certified.native_position())
                            .map_err(mistral_vibe_jsonl_batch_error)?;
                        start_ordinal = projector.next_ordinal;
                        resumed_projector = Some(projector);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                if !can_resume {
                    cursor_mode = CapturedBatchCursorMode::ResetChangedSource;
                }
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }

    let mut projector = resumed_projector.unwrap_or_else(|| {
        MistralVibeCapturedBatchProjector::fresh(
            file_context.clone(),
            session_source.clone(),
            metadata,
            metadata_revision,
            metadata_failure,
            complete_content_binding,
        )
    });
    if !observation.revalidate(&session_source)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let file = File::open(&session_source.messages_path)?;
    if MistralVibeFrozenFile::from_metadata(&file.metadata()?)? != observation.messages_file {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut producer = JsonlBatchProducer::new(
        BufReader::new(file),
        source.clone(),
        path_identity.into_bytes(),
        record_kind,
        observation.messages_file.length,
        start_offset,
        start_ordinal,
        false,
    )
    .map_err(mistral_vibe_jsonl_batch_error)?;
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &file_context)?;
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
            let batch = producer
                .next_batch()
                .map_err(mistral_vibe_jsonl_batch_error)?;
            imported_any |= batch.is_some();
            Ok(batch)
        },
        || observation.revalidate(&session_source),
    )?;
    if !imported_any && had_expected_store_cursor {
        projector.replay_summary()
    } else {
        Ok(summary)
    }
}

pub(crate) fn mistral_vibe_complete_content_record(
    value: &serde_json::Value,
    line_number: usize,
) -> Option<(String, String)> {
    let role = value
        .get("role")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let event_type = schema::mistral_vibe_event_type(role, value);
    (event_type == ctx_history_core::EventType::Message).then(|| {
        (
            schema::mistral_vibe_event_text(role, value, event_type),
            schema::mistral_vibe_event_id(value, line_number, role),
        )
    })
}

pub(crate) fn mistral_vibe_complete_content_source_from_admitted(
    metadata: &std::fs::Metadata,
    messages: &std::fs::Metadata,
    path_identity: String,
) -> Result<(String, String)> {
    Ok((
        source::mistral_vibe_complete_content_revision_from_admitted(metadata, messages)?,
        path_identity,
    ))
}

fn mistral_vibe_jsonl_batch_error(error: JsonlBatchError) -> CaptureError {
    match error {
        JsonlBatchError::Io(error) => CaptureError::Io(error),
        JsonlBatchError::SourceChangedDuringRead { .. } => CaptureError::SourceChangedDuringCapture,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

fn mistral_vibe_captured_batch_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}
