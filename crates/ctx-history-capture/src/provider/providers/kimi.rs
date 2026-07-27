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
use crate::provider::providers::native_jsonl::{
    native_jsonl_missing_reason, visit_native_jsonl_files,
};
use crate::{
    stable_capture_uuid, CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext,
    ProviderImportSummary, Result, KIMI_CODE_CLI_SOURCE_FORMAT,
};

mod event;
mod layout;
mod projection;
mod source;

pub(crate) use event::kimi_result_content;

#[cfg(test)]
mod tests;

use layout::KimiFrozenFileMetadata;
use projection::{KimiCapturedBatchProjector, KimiParserCheckpoint};
use source::KimiWireObservation;

const KIMI_CAPTURE_REVISION: u32 = 4;
const KIMI_POLICY_REVISION: u32 = 6;
const KIMI_WIRE_RECORD_KIND: &str = "kimi-wire-jsonl-v1";
pub(crate) const KIMI_RESULT_CONTENT_PROFILE: &str = "kimi.result-body.v1";

pub(crate) fn import_kimi_wire_jsonl_file_batched(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    context.source_path = Some(path.to_path_buf());
    let admission_scope_revision = kimi_admission_scope_revision(&context);
    let observation = KimiWireObservation::read(path)?;
    let cursor_source_path = provider_path_identity(path)?;
    let canonical_path_identity = provider_path_identity(observation.canonical_path())?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::KimiCodeCli,
        KIMI_CODE_CLI_SOURCE_FORMAT,
        &cursor_source_path,
    );
    let source = SourceObservation::new(
        CaptureProvider::KimiCodeCli,
        KIMI_CODE_CLI_SOURCE_FORMAT,
        format!("kimi-wire-jsonl:{canonical_path_identity}"),
        observation.source_revision(&admission_scope_revision),
        cursor_stream,
        KIMI_CAPTURE_REVISION,
        KIMI_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(kimi_captured_batch_error)?;
    let complete_content_binding = crate::complete_content::jsonl::ExactJsonlSourceBinding::new(
        source.source_revision(),
        &canonical_path_identity,
    );
    let source_item = canonical_path_identity.into_bytes();
    let record_kind =
        ProviderRecordKind::new(KIMI_WIRE_RECORD_KIND).map_err(kimi_captured_batch_error)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let had_expected_store_cursor = expected_store_cursor.is_some();
    let initial_position = initial_jsonl_position().map_err(kimi_jsonl_batch_error)?;
    let mut cursor_mode = CapturedBatchCursorMode::Resume;
    let mut start_offset = 0_u64;
    let mut start_ordinal = 0_u64;
    let mut projector = KimiCapturedBatchProjector::fresh(
        context.clone(),
        observation.session.clone(),
        complete_content_binding.clone(),
    );

    if let Some(stored_cursor) = expected_store_cursor.as_ref() {
        match CertifiedProviderCursor::decode_if_certified(&stored_cursor.cursor)? {
            Some(certified)
                if certified.parser_revision() == source.capture_revision()
                    && certified.policy_revision() == source.policy_revision() =>
            {
                let checkpoint: KimiParserCheckpoint =
                    certified.parser_checkpoint().deserialize()?;
                let auxiliary_unchanged =
                    checkpoint.auxiliary_revision == observation.session.auxiliary_revision;
                let admission_scope_unchanged =
                    checkpoint.admission_scope_revision == admission_scope_revision;
                let can_resume = if auxiliary_unchanged
                    && admission_scope_unchanged
                    && certified.source_revision() == source.source_revision()
                {
                    true
                } else if auxiliary_unchanged && admission_scope_unchanged {
                    let file = File::open(path)?;
                    if KimiFrozenFileMetadata::from_metadata(&file.metadata()?)?
                        != *observation.wire()
                    {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    let mut reader = BufReader::new(file);
                    match verify_jsonl_append_boundary(
                        &mut reader,
                        certified.native_position(),
                        &source,
                        observation.wire().length,
                    ) {
                        Ok(verified) => {
                            cursor_mode = CapturedBatchCursorMode::ResumeAppend(verified);
                            true
                        }
                        Err(JsonlBatchError::Io(error)) => return Err(CaptureError::Io(error)),
                        Err(_) => false,
                    }
                } else {
                    false
                };
                if can_resume {
                    start_offset = jsonl_position_offset(certified.native_position())
                        .map_err(kimi_jsonl_batch_error)?;
                    projector = KimiCapturedBatchProjector::resume(
                        context.clone(),
                        observation.session.clone(),
                        &certified,
                        complete_content_binding.clone(),
                    )?;
                    start_ordinal = projector.next_ordinal;
                } else {
                    cursor_mode = CapturedBatchCursorMode::ResetChangedSource;
                }
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }

    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context)?;
    if !observation.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let file = File::open(path)?;
    if KimiFrozenFileMetadata::from_metadata(&file.metadata()?)? != *observation.wire() {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut producer = JsonlBatchProducer::new(
        BufReader::new(file),
        source,
        source_item,
        record_kind,
        observation.wire().length,
        start_offset,
        start_ordinal,
        false,
    )
    .map_err(kimi_jsonl_batch_error)?;
    let mut imported_any = false;
    let summary = drain_captured_batches(
        store,
        &admission,
        import_options,
        &context.machine_id,
        context.imported_at,
        expected_store_cursor,
        &initial_position,
        cursor_mode,
        &stream,
        &mut projector,
        || {
            let batch = producer.next_batch().map_err(kimi_jsonl_batch_error)?;
            imported_any |= batch.is_some();
            Ok(batch)
        },
        || observation.revalidate(path),
    )?;
    if !imported_any && had_expected_store_cursor {
        let mut replay = projector.replay_summary()?;
        replay.failed = summary.failed;
        Ok(replay)
    } else {
        Ok(summary)
    }
}

pub(crate) fn import_kimi_wire_jsonl_tree_batched(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let source_root = context
        .source_root
        .clone()
        .or_else(|| context.source_path.clone())
        .unwrap_or_else(|| path.to_path_buf());
    let mut merged = ProviderImportSummary::default();
    let source_count =
        visit_native_jsonl_files(path, CaptureProvider::KimiCodeCli, &mut |file_path| {
            let mut file_context = context.clone();
            file_context.source_path = Some(file_path.to_path_buf());
            file_context.source_root = Some(source_root.clone());
            merged.merge(import_kimi_wire_jsonl_file_batched(
                file_path,
                store,
                file_context,
                import_options.clone(),
            )?);
            Ok(())
        })?;
    if source_count == 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: native_jsonl_missing_reason(CaptureProvider::KimiCodeCli),
        });
    }
    Ok(merged)
}

fn kimi_admission_scope_revision(context: &ProviderAdapterContext) -> String {
    kimi_admission_scope_revision_for_display(context.source_root_display())
}

fn kimi_admission_scope_revision_for_display(source_root: Option<String>) -> String {
    stable_capture_uuid(
        &format!(
            "provider={};source_format={};source_root={:?}",
            CaptureProvider::KimiCodeCli.as_str(),
            KIMI_CODE_CLI_SOURCE_FORMAT,
            source_root,
        ),
        "kimi-admission-scope",
    )
    .to_string()
}

pub(crate) fn kimi_complete_content_record(
    value: &serde_json::Value,
    line_number: usize,
) -> Option<(String, String)> {
    let record_type = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let event_type = event::kimi_event_type(record_type, value);
    (event_type == ctx_history_core::EventType::Message).then(|| {
        let native_record_id = format!(
            "{}:{}",
            record_type,
            value
                .get("time")
                .and_then(serde_json::Value::as_i64)
                .map(|time| time.to_string())
                .unwrap_or_else(|| line_number.to_string())
        );
        (
            event::kimi_event_text(record_type, value, event_type),
            native_record_id,
        )
    })
}

pub(crate) fn kimi_complete_content_auxiliary_paths(
    path: &Path,
) -> Result<(std::path::PathBuf, std::path::PathBuf)> {
    layout::complete_content_auxiliary_paths(path)
}

pub(crate) fn kimi_complete_content_source_from_admitted(
    path: &Path,
    source_root: Option<&Path>,
    canonical_path: std::path::PathBuf,
    wire_metadata: &std::fs::Metadata,
    state: Option<(&std::fs::Metadata, &[u8])>,
    index: Option<(&std::fs::Metadata, &[u8])>,
    path_identity: String,
) -> Result<(String, String)> {
    let observation =
        KimiWireObservation::read_from_admitted(path, canonical_path, wire_metadata, state, index)?;
    let admission_scope_revision = kimi_admission_scope_revision_for_display(Some(
        source_root.unwrap_or(path).display().to_string(),
    ));
    Ok((
        observation.source_revision(&admission_scope_revision),
        path_identity,
    ))
}

fn kimi_jsonl_batch_error(error: JsonlBatchError) -> CaptureError {
    match error {
        JsonlBatchError::Io(error) => CaptureError::Io(error),
        JsonlBatchError::SourceChangedDuringRead { .. } => CaptureError::SourceChangedDuringCapture,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

fn kimi_captured_batch_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}
