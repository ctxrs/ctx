use std::path::Path;

use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;

use crate::captured_batch::jsonl::JsonlBatchError;
use crate::captured_batch::whole_json::WholeJsonBatchError;
use crate::provider::providers::native_jsonl::native_jsonl_missing_reason;
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    Result,
};

mod chat;
mod metadata;
mod normalization;
mod partial;
mod projector;
mod source;

use chat::import_mux_chat_batched;
use partial::import_mux_partial_batched;
use source::{visit_mux_session_sources, MuxSessionSource};

const MUX_CAPTURE_REVISION: u32 = 2;
const MUX_POLICY_REVISION: u32 = 4;
const MUX_CHAT_RECORD_KIND: &str = "mux-chat-jsonl-v1";
const MUX_PARTIAL_RECORD_KIND: &str = "mux-partial-json-v1";
const MUX_WHOLE_JSON_POSITION_KIND: &str = "whole-json-item-v1";
const MUX_MAX_ID_BYTES: usize = 4 * 1024;
const MUX_MAX_FAILURE_BYTES: usize = 4 * 1024;

pub(crate) fn import_mux_sessions_batched(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    if context.source_path.is_none() {
        context.source_path = Some(path.to_path_buf());
    }
    let mut merged = ProviderImportSummary::default();
    let source_count = visit_mux_session_sources(path, &mut |source| {
        merged.merge(import_mux_session_batched(
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
            reason: native_jsonl_missing_reason(CaptureProvider::Mux),
        });
    }
    Ok(merged)
}

fn import_mux_session_batched(
    source: MuxSessionSource,
    store: &mut Store,
    context: &ProviderAdapterContext,
    import_options: &NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let mut merged = ProviderImportSummary::default();
    if let Some(chat_path) = source.chat_path.clone() {
        merged.merge(import_mux_chat_batched(
            source.clone(),
            chat_path,
            store,
            context,
            import_options,
        )?);
    }
    if let Some(partial_path) = source.partial_path.clone() {
        merged.merge(import_mux_partial_batched(
            source,
            partial_path,
            store,
            context,
            import_options,
        )?);
    }
    Ok(merged)
}

fn mux_file_context(
    context: &ProviderAdapterContext,
    source_path: &Path,
) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: context.machine_id.clone(),
        source_path: Some(source_path.to_path_buf()),
        source_root: context
            .source_root
            .clone()
            .or_else(|| context.source_path.clone()),
        imported_at: context.imported_at,
    }
}

fn mux_jsonl_batch_error(error: JsonlBatchError) -> CaptureError {
    match error {
        JsonlBatchError::Io(error) => CaptureError::Io(error),
        JsonlBatchError::SourceChangedDuringRead { .. } => CaptureError::SourceChangedDuringCapture,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

fn mux_whole_json_error(error: WholeJsonBatchError) -> CaptureError {
    match error {
        WholeJsonBatchError::Io(error) => CaptureError::Io(error),
        WholeJsonBatchError::SourceSizeChanged { .. }
        | WholeJsonBatchError::SourceMetadataChangedDuringRead => {
            CaptureError::SourceChangedDuringCapture
        }
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

fn mux_captured_batch_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

#[cfg(test)]
#[path = "mux/tests.rs"]
mod captured_batch_tests;
