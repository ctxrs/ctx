use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use uuid::Uuid;

use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    Result,
};

mod dialect;
mod normalization;
mod projector;
mod source;
mod traversal;
mod windsurf;

pub(crate) use dialect::native_jsonl_missing_reason;
pub(crate) use normalization::{
    native_jsonl_entry_type, native_jsonl_event, native_jsonl_event_id, native_jsonl_event_text,
    native_jsonl_event_type, native_jsonl_timestamp,
};

pub(crate) struct NativeJsonlTreeImport<'a> {
    pub(crate) path: &'a Path,
    pub(crate) machine_id: String,
    pub(crate) source_path: Option<PathBuf>,
    pub(crate) source_root: Option<PathBuf>,
    pub(crate) imported_at: DateTime<Utc>,
    pub(crate) history_record_id: Option<Uuid>,
    pub(crate) capture_work_limit: crate::CaptureWorkLimit,
    pub(crate) inventory_observation_token: Option<String>,
}

pub(crate) fn import_bounded_native_jsonl_tree(
    store: &mut Store,
    request: NativeJsonlTreeImport<'_>,
    provider: CaptureProvider,
    source_format: &'static str,
) -> Result<ProviderImportSummary> {
    let configured_source_root = request
        .source_path
        .unwrap_or_else(|| request.path.to_path_buf());
    import_native_jsonl_tree_batched(
        request.path,
        store,
        ProviderAdapterContext {
            machine_id: request.machine_id,
            source_path: Some(request.path.to_path_buf()),
            source_root: request.source_root.or(Some(configured_source_root)),
            imported_at: request.imported_at,
        },
        provider,
        source_format,
        NormalizedProviderImportOptions {
            history_record_id: request.history_record_id,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
            capture_work_limit: request.capture_work_limit,
            inventory_observation_token: request.inventory_observation_token,
        },
    )
}

pub(crate) fn visit_native_jsonl_files(
    root: &Path,
    provider: CaptureProvider,
    visit: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<usize> {
    traversal::visit_jsonl_tree_files(
        root,
        &|path| dialect::native_jsonl_file_is_selected(provider, path),
        visit,
    )
}

pub(crate) fn import_native_jsonl_tree_batched(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    provider: CaptureProvider,
    source_format: &str,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    dialect::validate_direct_native_jsonl_provider(provider)?;
    let mut merged = ProviderImportSummary::default();
    let visited = visit_native_jsonl_files(path, provider, &mut |file_path| {
        let mut file_context = context.clone();
        file_context.source_path = Some(file_path.to_path_buf());
        merged.merge(source::import_native_jsonl_file_batched(
            file_path,
            store,
            file_context,
            provider,
            source_format,
            import_options.clone(),
        )?);
        Ok(())
    })?;
    if visited == 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: native_jsonl_missing_reason(provider),
        });
    }
    Ok(merged)
}

#[cfg(test)]
use std::io::{Seek, SeekFrom};

#[cfg(test)]
use ctx_history_core::{EventType, SessionStatus};
#[cfg(test)]
use serde_json::{json, Value};

#[cfg(test)]
use crate::captured_batch::{CapturedRecord, ProviderRecordKind, SourceObservation};
#[cfg(test)]
use crate::provider::importer::{
    CapturedBatchCursorFinish, CapturedBatchProjector, CertifiedProviderCursor,
    ProviderProjectionOutput, ProviderProjectionResult,
};
#[cfg(test)]
use crate::ProviderNormalizationResult;
#[cfg(test)]
use dialect::native_jsonl_record_kind;
#[cfg(test)]
use normalization::{
    native_jsonl_normalized_header_metadata, native_jsonl_session_metadata_from_normalized_header,
};
#[cfg(test)]
use projector::{
    NativeJsonlCapturedBatchProjector, NativeJsonlParserCheckpoint, NATIVE_JSONL_LOCATOR_KIND,
};
#[cfg(test)]
use source::{
    count_native_jsonl_source_file_opens, import_native_jsonl_file_batched,
    NATIVE_JSONL_CAPTURE_REVISION, NATIVE_JSONL_POLICY_REVISION,
};

#[cfg(test)]
mod tests;
