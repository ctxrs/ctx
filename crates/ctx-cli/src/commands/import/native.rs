use std::path::Path;

use anyhow::{anyhow, Result};
use uuid::Uuid;

use ctx_history_capture::{
    CodexSessionImportProgressCallback, ProviderImportSummary, ProviderImportSupport,
};
use ctx_history_core::utc_now;
use ctx_history_store::{SourceImportFile, Store};

use crate::commands::import::catalog::{
    import_record_for_source, source_uses_incremental_event_search,
};
use crate::commands::import::report::{import_error_scope, ImportFailureScope};
use crate::commands::import::{
    cleanup_rejected_history_record, history_record_exists, provider_summary_has_imported_content,
    rejected_source_error, SourcePreinventory,
};
use crate::provider_sources::SourceInfo;

#[cfg(test)]
use crate::commands::import::manifest::inventory_source_import_files;
use crate::commands::import::manifest::source_uses_import_file_manifest;

mod bulk;
mod dispatch;
mod manifested;

#[cfg(test)]
use manifested::{import_manifested_source, manifest_pending_source_context};

pub(crate) fn validate_source_import_supported(source: &SourceInfo) -> Result<()> {
    match source.import_support {
        ProviderImportSupport::Native => Ok(()),
        ProviderImportSupport::Explicit => Ok(()),
        ProviderImportSupport::Unsupported => {
            let reason = source
                .unsupported_reason
                .unwrap_or("no native local-history parser is implemented");
            Err(anyhow!(
                "{} native import is unsupported: {reason}",
                source.provider.as_str()
            ))
        }
    }
}

pub(crate) fn import_one_source(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    full_rescan: bool,
    preinventory: &SourcePreinventory,
) -> Result<ProviderImportSummary> {
    let event_search_needs_backfill = store.event_search_projection_needs_backfill()?;
    let refresh_search_after_import =
        event_search_needs_backfill || !source_uses_incremental_event_search(source);
    import_one_source_inner(
        store,
        source,
        progress,
        refresh_search_after_import,
        full_rescan,
        preinventory,
    )
}

pub(crate) fn import_one_source_without_search_refresh(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    full_rescan: bool,
    preinventory: &SourcePreinventory,
) -> Result<ProviderImportSummary> {
    import_one_source_inner(store, source, progress, false, full_rescan, preinventory)
}

pub(crate) fn import_one_source_for_search_refresh(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    preinventory: &SourcePreinventory,
) -> Result<ProviderImportSummary> {
    import_one_source_for_search_refresh_with_limit(
        store,
        source,
        progress,
        preinventory,
        ctx_history_capture::CaptureWorkLimit::Drain,
    )
}

pub(crate) fn import_one_source_for_background_refresh(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    preinventory: &SourcePreinventory,
) -> Result<ProviderImportSummary> {
    import_one_source_for_search_refresh_with_limit(
        store,
        source,
        progress,
        preinventory,
        ctx_history_capture::CaptureWorkLimit::OneSafeGroup,
    )
}

fn import_one_source_for_search_refresh_with_limit(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    preinventory: &SourcePreinventory,
    capture_work_limit: ctx_history_capture::CaptureWorkLimit,
) -> Result<ProviderImportSummary> {
    if !source_uses_import_file_manifest(source)
        && preinventory.source_root_file().is_some()
        && store
            .list_pending_source_import_files(source.provider, &source.path.display().to_string())?
            .is_empty()
    {
        store.upsert_record(&import_record_for_source(source))?;
        if store.event_search_projection_needs_backfill()? {
            store.refresh_search_index()?;
        }
        return Ok(ProviderImportSummary::default());
    }
    bulk::import_one_source_inner_at_path(
        store,
        source,
        &source.path,
        progress,
        false,
        false,
        preinventory,
        capture_work_limit,
        None,
    )
}

pub(crate) fn import_one_source_inner(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    refresh_search_after_import: bool,
    full_rescan: bool,
    preinventory: &SourcePreinventory,
) -> Result<ProviderImportSummary> {
    bulk::import_one_source_inner_at_path(
        store,
        source,
        &source.path,
        progress,
        refresh_search_after_import,
        full_rescan,
        preinventory,
        ctx_history_capture::CaptureWorkLimit::Drain,
        None,
    )
}

struct NativeSourceRun<'a> {
    store: &'a mut Store,
    source: &'a SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    full_rescan: bool,
    preinventory: &'a SourcePreinventory,
    capture_work_limit: ctx_history_capture::CaptureWorkLimit,
    inventory_observation_token: Option<String>,
}

impl<'a> NativeSourceRun<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        store: &'a mut Store,
        source: &'a SourceInfo,
        progress: Option<CodexSessionImportProgressCallback>,
        full_rescan: bool,
        preinventory: &'a SourcePreinventory,
        capture_work_limit: ctx_history_capture::CaptureWorkLimit,
        inventory_observation_token: Option<String>,
    ) -> Self {
        Self {
            store,
            source,
            progress,
            full_rescan,
            preinventory,
            capture_work_limit,
            inventory_observation_token,
        }
    }

    fn run(mut self, input_path: &Path) -> Result<ProviderImportSummary> {
        let record = import_record_for_source(self.source);
        let record_id = record.id;
        let record_existed = history_record_exists(self.store, record_id)?;
        self.store.upsert_record(&record)?;
        let summary = if !self.full_rescan && source_uses_import_file_manifest(self.source) {
            manifested::import_manifested_source(
                self.store,
                self.source,
                self.progress.clone(),
                matches!(self.preinventory, SourcePreinventory::SourceImportManifest),
                self.capture_work_limit,
            )
        } else {
            dispatch::import_direct_source(
                self.store,
                self.source,
                input_path,
                record_id,
                self.progress.clone(),
                self.full_rescan,
                self.preinventory,
                self.capture_work_limit,
                self.inventory_observation_token.clone(),
            )
        };
        self.finish(record_id, record_existed, summary)
    }

    fn finish(
        &mut self,
        record_id: Uuid,
        record_existed: bool,
        summary: Result<ProviderImportSummary>,
    ) -> Result<ProviderImportSummary> {
        let summary = match summary {
            Ok(summary) => {
                // A manifested-source retry can contain only rejected files even though earlier
                // files under the same stable history record are already indexed. Preserve that
                // as a completed source with rejections; an orphan record is still cleaned up and
                // remains an all-rejected source failure.
                let retained_existing_content = if summary.failed > 0
                    && !provider_summary_has_imported_content(&summary)
                    && record_existed
                {
                    !self.store.delete_orphan_record(record_id)?
                        && history_record_exists(self.store, record_id)?
                } else {
                    false
                };
                if summary.failed > 0
                    && !provider_summary_has_imported_content(&summary)
                    && !retained_existing_content
                {
                    let inventory_result = mark_source_root_inventory_failed(
                        self.store,
                        self.preinventory,
                        &format!("provider import reported {} failure(s)", summary.failed),
                    );
                    let cleanup_result =
                        cleanup_rejected_history_record(self.store, record_id, record_existed);
                    finish_terminal_inventory_and_cleanup(inventory_result, cleanup_result)?;
                    return Err(provider_import_summary_failure(self.source, &summary));
                }
                if let Err(inventory_error) =
                    mark_source_root_inventory_indexed(self.store, self.preinventory)
                {
                    let cleanup_result = self
                        .store
                        .delete_orphan_record(record_id)
                        .map(|_| ())
                        .map_err(anyhow::Error::from);
                    return finish_terminal_inventory_and_cleanup(
                        Err(inventory_error),
                        cleanup_result,
                    )
                    .map(|()| summary);
                }
                summary
            }
            Err(err) => {
                let failure_scope = import_error_scope(&err);
                let inventory_result = mark_source_root_inventory_failed(
                    self.store,
                    self.preinventory,
                    &err.to_string(),
                );
                let cleanup_result = if failure_scope == ImportFailureScope::Source {
                    cleanup_rejected_history_record(self.store, record_id, record_existed)
                } else {
                    self.store
                        .delete_orphan_record(record_id)
                        .map(|_| ())
                        .map_err(anyhow::Error::from)
                };
                finish_terminal_inventory_and_cleanup(inventory_result, cleanup_result)?;
                return Err(err);
            }
        };
        Ok(summary)
    }
}

fn finish_terminal_inventory_and_cleanup(
    inventory_result: Result<()>,
    cleanup_result: Result<()>,
) -> Result<()> {
    match (inventory_result, cleanup_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(inventory_error), Err(cleanup_error)) => Err(inventory_error.context(format!(
            "history-record cleanup also failed: {cleanup_error:#}"
        ))),
    }
}

fn mark_source_root_inventory_indexed(
    store: &Store,
    preinventory: &SourcePreinventory,
) -> Result<()> {
    let Some(file) = preinventory.source_root_file() else {
        return Ok(());
    };
    mark_source_import_file_indexed(store, file)
}

fn mark_source_root_inventory_failed(
    store: &Store,
    preinventory: &SourcePreinventory,
    error: &str,
) -> Result<()> {
    let Some(file) = preinventory.source_root_file() else {
        return Ok(());
    };
    mark_source_import_file_failed(store, file, error)
}

fn mark_source_import_file_failed(
    store: &Store,
    file: &SourceImportFile,
    error: &str,
) -> Result<()> {
    store.mark_source_import_file_failed(file, error, utc_now().timestamp_millis())?;
    Ok(())
}

fn mark_source_import_file_indexed(store: &Store, file: &SourceImportFile) -> Result<()> {
    store.mark_source_import_file_indexed(file, utc_now().timestamp_millis())?;
    Ok(())
}

pub(crate) fn provider_import_summary_failure(
    source: &SourceInfo,
    summary: &ProviderImportSummary,
) -> anyhow::Error {
    let detail = summary
        .failures
        .first()
        .map(|failure| format!("line {}: {}", failure.line, failure.error))
        .unwrap_or_else(|| "unknown provider import failure".to_owned());
    rejected_source_error(
        format!(
            "import {} source {} failed with {} failure(s); first failure: {detail}",
            source.provider.as_str(),
            source.path.display(),
            summary.failed
        ),
        summary,
    )
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;
