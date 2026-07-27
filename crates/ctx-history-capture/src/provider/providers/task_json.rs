use std::path::{Path, PathBuf};

use ctx_history_store::Store;

use crate::captured_batch::{ProviderRecordKind, SourceObservation};
use crate::provider::importer::{
    captured_batch_cursor_stream, drain_captured_batches, provider_path_identity,
    provider_source_cursor_stream_for_path, CapturedBatchCursorMode, CapturedSourceAdmission,
    CertifiedProviderCursor,
};
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    Result,
};

mod dialect;
mod normalization;
mod projector;
mod scanner;
mod source;

pub(crate) use dialect::{task_json_provider, TaskJsonProviderSpec};
pub(crate) use normalization::{
    task_json_event, task_json_event_text, task_json_event_type, task_json_string_field,
    task_json_time_field, TaskJsonEventInput,
};

use dialect::{
    task_json_captured_batch_error, task_json_decode_position, task_json_native_position,
    TaskJsonMessagePhase, TaskJsonStreamPosition, TASK_JSON_CAPTURE_REVISION, TASK_JSON_DONE_PHASE,
    TASK_JSON_POLICY_REVISION, TASK_JSON_RECORD_KIND,
};
use projector::{task_json_session_state, TaskJsonCapturedBatchProjector};
use scanner::{TaskJsonBatchProducer, TaskJsonMessageSource};
use source::{
    task_json_missing_reason, task_json_root_history_candidate_paths, visit_task_json_dirs,
    TaskJsonTaskObservation,
};

pub(crate) fn import_task_json_history_batched(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
    spec: TaskJsonProviderSpec,
) -> Result<ProviderImportSummary> {
    let root_history_paths = task_json_root_history_candidate_paths(path, spec);
    let mut merged = ProviderImportSummary::default();
    let source_count = visit_task_json_dirs(path, spec, &mut |task_dir| {
        merged.merge(import_task_json_task_dir_batched(
            task_dir,
            &root_history_paths,
            store,
            &context,
            &import_options,
            spec,
        )?);
        Ok(())
    })?;
    if source_count == 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: task_json_missing_reason(spec.provider),
        });
    }
    Ok(merged)
}

fn import_task_json_task_dir_batched(
    task_dir: &Path,
    root_history_paths: &[PathBuf],
    store: &mut Store,
    context: &ProviderAdapterContext,
    import_options: &NormalizedProviderImportOptions,
    spec: TaskJsonProviderSpec,
) -> Result<ProviderImportSummary> {
    let observation = TaskJsonTaskObservation::read(task_dir, root_history_paths, spec)?;
    let path_identity = provider_path_identity(&observation.canonical_task_dir)?;
    let raw_source_path = task_dir.display().to_string();
    let file_context = ProviderAdapterContext {
        machine_id: context.machine_id.clone(),
        source_path: Some(task_dir.to_path_buf()),
        source_root: context
            .source_root
            .clone()
            .or_else(|| context.source_path.clone()),
        imported_at: context.imported_at,
    };
    let source = SourceObservation::new(
        spec.provider,
        spec.source_format,
        format!(
            "{}-task-json-directory:{path_identity}",
            spec.provider.as_str()
        ),
        observation.source_revision(spec),
        provider_source_cursor_stream_for_path(spec.provider, spec.source_format, &path_identity),
        TASK_JSON_CAPTURE_REVISION,
        TASK_JSON_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(task_json_captured_batch_error)?;
    let record_kind =
        ProviderRecordKind::new(TASK_JSON_RECORD_KIND).map_err(task_json_captured_batch_error)?;
    let initial_position = task_json_native_position(TaskJsonStreamPosition::initial())?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let had_expected_store_cursor = expected_store_cursor.is_some();
    let mut cursor_mode = CapturedBatchCursorMode::Resume;
    let mut start_position = TaskJsonStreamPosition::initial();
    let mut resume_cursor = None;

    if let Some(stored_cursor) = expected_store_cursor.as_ref() {
        match CertifiedProviderCursor::decode_if_certified(&stored_cursor.cursor)? {
            Some(certified)
                if certified.source_revision() == source.source_revision()
                    && certified.parser_revision() == source.capture_revision()
                    && certified.policy_revision() == source.policy_revision() =>
            {
                start_position = task_json_decode_position(certified.native_position())?;
                if start_position.phase == TASK_JSON_DONE_PHASE {
                    if !observation.revalidate(task_dir)? {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    return TaskJsonCapturedBatchProjector::replay_summary_from_cursor(&certified);
                }
                resume_cursor = Some(certified);
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }

    let (session, state_failures) =
        task_json_session_state(task_dir, &observation, &file_context, spec)?;
    let mut projector = match resume_cursor {
        Some(cursor) => TaskJsonCapturedBatchProjector::resume(
            spec,
            file_context.clone(),
            raw_source_path,
            session,
            state_failures,
            &cursor,
        )?,
        None => {
            start_position = TaskJsonStreamPosition::initial();
            TaskJsonCapturedBatchProjector::fresh(
                spec,
                file_context.clone(),
                raw_source_path,
                session,
                state_failures,
            )?
        }
    };
    if !observation.revalidate(task_dir)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut message_sources = Vec::new();
    for phase in [
        TaskJsonMessagePhase::Api,
        TaskJsonMessagePhase::Ui,
        TaskJsonMessagePhase::Fallback,
    ] {
        let Some(observed) = observation.message_file(spec, phase) else {
            continue;
        };
        let frozen = observed
            .frozen
            .clone()
            .ok_or(CaptureError::SystemInvariant(
                "task JSON observed message file lost its metadata",
            ))?;
        message_sources.push(TaskJsonMessageSource {
            phase,
            path: observed.path.clone(),
            frozen,
        });
    }
    let mut producer =
        TaskJsonBatchProducer::new(source.clone(), record_kind, message_sources, start_position)?;
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
            let batch = producer.next_batch()?;
            imported_any |= batch.is_some();
            Ok(batch)
        },
        || observation.revalidate(task_dir),
    )?;
    if !imported_any && had_expected_store_cursor {
        projector.replay_summary()
    } else {
        Ok(summary)
    }
}

#[cfg(test)]
#[path = "task_json/tests/mod.rs"]
mod captured_batch_tests;
