use std::path::Path;

use anyhow::{anyhow, Result};
use uuid::Uuid;

use ctx_history_capture::{
    CaptureError, CodexSessionImportProgressCallback, ImportProfile, ProviderImportSummary,
    ProviderImportSupport,
};
use ctx_history_core::{utc_now, HistoryRecord};
use ctx_history_store::{SourceImportFile, Store, StoreError};

use crate::commands::import::catalog::import_record_for_source;
use crate::commands::import::{
    cleanup_rejected_history_record, history_record_exists, provider_summary_has_imported_content,
    rejected_source_error, SourcePreinventory,
};
use crate::provider_sources::SourceInfo;

#[cfg(test)]
use crate::commands::import::manifest::inventory_source_import_files;
use crate::commands::import::manifest::{
    provider_owns_root_manifest, source_uses_import_file_manifest,
};

mod bulk;
mod dispatch;
mod manifested;

const SEARCH_PROJECTION_REPAIR_REQUIRED: &str = "search projection repair required before provider import; ctx will not rebuild the full projection during import; rebuild the local ctx index as documented by `ctx docs show storage`, then retry";

#[cfg(test)]
use manifested::{import_manifested_source, manifest_pending_source_context};

pub(crate) fn ensure_search_projection_ready_for_provider_import(store: &Store) -> Result<()> {
    // A provider projection this binary cannot address must never be written
    // into: every source would be indexed a second time. This is the single
    // funnel every provider write into an existing Store passes through.
    crate::provider_projection::ensure_native_provider_projection(store)?;
    if store.event_search_projection_needs_backfill()? {
        return Err(CaptureError::SystemInvariant(SEARCH_PROJECTION_REPAIR_REQUIRED).into());
    }
    Ok(())
}

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

pub(crate) fn import_one_source_with_profile(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    full_rescan: bool,
    preinventory: &SourcePreinventory,
    import_profile: &ImportProfile,
) -> Result<ProviderImportSummary> {
    import_one_source_inner_with_profile(
        store,
        source,
        progress,
        full_rescan,
        preinventory,
        import_profile,
    )
}

#[cfg(test)]
pub(crate) fn import_one_source_for_search_refresh(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    preinventory: &SourcePreinventory,
) -> Result<ProviderImportSummary> {
    import_one_source_for_search_refresh_with_profile(
        store,
        source,
        progress,
        preinventory,
        &ImportProfile::CoreOnly,
    )
}

pub(crate) fn import_one_source_for_search_refresh_with_profile(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    preinventory: &SourcePreinventory,
    import_profile: &ImportProfile,
) -> Result<ProviderImportSummary> {
    import_one_source_for_search_refresh_with_limit(
        store,
        source,
        progress,
        preinventory,
        ctx_history_capture::CaptureWorkLimit::Drain,
        import_profile,
    )
}

#[cfg(test)]
pub(crate) fn import_one_source_for_background_refresh(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    preinventory: &SourcePreinventory,
) -> Result<ProviderImportSummary> {
    import_one_source_for_background_refresh_with_profile(
        store,
        source,
        progress,
        preinventory,
        &ImportProfile::CoreOnly,
    )
}

pub(crate) fn import_one_source_for_background_refresh_with_profile(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    preinventory: &SourcePreinventory,
    import_profile: &ImportProfile,
) -> Result<ProviderImportSummary> {
    import_one_source_for_search_refresh_with_limit(
        store,
        source,
        progress,
        preinventory,
        ctx_history_capture::CaptureWorkLimit::OneSafeGroup,
        import_profile,
    )
}

fn import_one_source_for_search_refresh_with_limit(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    preinventory: &SourcePreinventory,
    capture_work_limit: ctx_history_capture::CaptureWorkLimit,
    import_profile: &ImportProfile,
) -> Result<ProviderImportSummary> {
    ensure_search_projection_ready_for_provider_import(store)?;
    // An unchanged outer scheduling token cannot prove a provider-owned root manifest is
    // unchanged. Enter those providers so they can reconcile siblings, retirement, and Pro state.
    if matches!(import_profile, ImportProfile::CoreOnly)
        && !source_uses_import_file_manifest(source)
        && !provider_owns_root_manifest(source)
        && preinventory.source_root_file().is_some()
        && store
            .list_pending_source_import_files(source.provider, &source.path.display().to_string())?
            .is_empty()
    {
        let record = import_record_for_source(source);
        ensure_import_record(store, record)?;
        return Ok(ProviderImportSummary::default());
    }
    bulk::import_one_source_inner_at_path(
        store,
        source,
        &source.path,
        progress,
        false,
        preinventory,
        capture_work_limit,
        None,
        import_profile,
    )
}

#[cfg(test)]
pub(crate) fn import_one_source_inner(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    full_rescan: bool,
    preinventory: &SourcePreinventory,
) -> Result<ProviderImportSummary> {
    import_one_source_inner_with_profile(
        store,
        source,
        progress,
        full_rescan,
        preinventory,
        &ImportProfile::CoreOnly,
    )
}

pub(crate) fn import_one_source_inner_with_profile(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    full_rescan: bool,
    preinventory: &SourcePreinventory,
    import_profile: &ImportProfile,
) -> Result<ProviderImportSummary> {
    ensure_search_projection_ready_for_provider_import(store)?;
    bulk::import_one_source_inner_at_path(
        store,
        source,
        &source.path,
        progress,
        full_rescan,
        preinventory,
        ctx_history_capture::CaptureWorkLimit::Drain,
        None,
        import_profile,
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
    import_profile: ImportProfile,
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
        import_profile: &ImportProfile,
    ) -> Self {
        Self {
            store,
            source,
            progress,
            full_rescan,
            preinventory,
            capture_work_limit,
            inventory_observation_token,
            import_profile: import_profile.clone(),
        }
    }

    fn run(mut self, input_path: &Path) -> Result<ProviderImportSummary> {
        let record = import_record_for_source(self.source);
        let record_id = record.id;
        let record_existed = ensure_import_record(self.store, record)?;
        let missing_without_preinventory =
            matches!(self.preinventory, SourcePreinventory::None) && !input_path.exists();
        let summary = if !self.full_rescan
            && source_uses_import_file_manifest(self.source)
            && !missing_without_preinventory
        {
            manifested::import_manifested_source(
                self.store,
                self.source,
                self.progress.clone(),
                matches!(self.preinventory, SourcePreinventory::SourceImportManifest),
                self.capture_work_limit,
                &self.import_profile,
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
                &self.import_profile,
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
                let inventory_result = mark_source_root_inventory_failed(
                    self.store,
                    self.preinventory,
                    &err.to_string(),
                );
                // A raw error does not prove that earlier bounded work from
                // this source failed to commit. Remove only an orphan and
                // preserve any attached accepted content.
                let cleanup_result = self
                    .store
                    .delete_orphan_record(record_id)
                    .map(|_| ())
                    .map_err(anyhow::Error::from);
                finish_terminal_inventory_and_cleanup(inventory_result, cleanup_result)?;
                return Err(err);
            }
        };
        Ok(summary)
    }
}

fn ensure_import_record(store: &Store, mut desired: HistoryRecord) -> Result<bool> {
    let existing = match store.get_record(desired.id) {
        Ok(existing) => existing,
        Err(StoreError::NotFound(_)) => {
            store.upsert_record(&desired)?;
            return Ok(false);
        }
        Err(error) => return Err(error.into()),
    };
    let unchanged = existing.title == desired.title
        && existing.body == desired.body
        && existing.tags == desired.tags
        && existing.kind == desired.kind
        && existing.workspace == desired.workspace;
    if !unchanged {
        desired.created_at = existing.created_at;
        store.upsert_record(&desired)?;
    }
    Ok(true)
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
