use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

use ctx_history_capture::{
    CaptureError, CaptureWorkLimit, CodexSessionImportProgressCallback, ImportProfile,
    ProviderImportFailure, ProviderImportSummary, ProviderImportTerminalOutcome,
};
use ctx_history_core::CaptureProvider;
use ctx_history_store::{SourceImportFile, Store, StoreError};

use crate::commands::import::manifest::inventory_source_import_files;
use crate::commands::import::report::{error_summary, import_error_scope, ImportFailureScope};
use crate::commands::import::SourcePreinventory;
use crate::provider_sources::SourceInfo;

use super::{mark_source_import_file_failed, mark_source_import_file_indexed};

fn inventory_observation_token(file: &SourceImportFile) -> Option<String> {
    ["inventory_file_change_token_v1", "change_token_v1"]
        .into_iter()
        .find_map(|key| file.metadata.get(key).and_then(Value::as_str))
        .map(str::to_owned)
}

pub(crate) fn import_manifested_source(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    preinventoried: bool,
    capture_work_limit: CaptureWorkLimit,
    import_profile: &ImportProfile,
) -> Result<ProviderImportSummary> {
    let source_root = source.path.display().to_string();
    if !preinventoried {
        let _inventory_guard = store.acquire_source_inventory_lock()?;
        inventory_source_import_files(store, source, false)
            .with_context(|| format!("inventory import files from {}", source.path.display()))?;
    }
    let (active_files, _) =
        store.source_import_file_stats_for_source(source.provider, &source_root)?;
    if active_files == 0 {
        if store.source_import_file_history_exists(source.provider, &source_root)? {
            return Ok(ProviderImportSummary::default());
        }
        return Err(anyhow!(
            "no importable {} history files found under {}",
            source.provider.as_str(),
            source.path.display()
        ));
    }

    let mut summary = ProviderImportSummary::default();
    let mut after_source_path = None;
    loop {
        let pending = store.list_pending_source_import_files_page(
            source.provider,
            &source_root,
            after_source_path.as_deref(),
        )?;
        if pending.is_empty() {
            break;
        }
        after_source_path = pending.last().map(|file| file.source_path.clone());
        // NativePath provider groups own their event, search, and cursor transactions.
        // Keeping a manifest page transaction open here would turn those commits into
        // savepoints, defeat bounded WAL checkpoints, and make a large file one unbounded
        // transaction. File completion updates are independently conditional and crash-safe.
        let control = import_manifested_source_page(
            store,
            source,
            &pending,
            progress.clone(),
            capture_work_limit,
            import_profile,
            &mut summary,
        )?;
        match control {
            ManifestImportPageControl::Continue => {}
            ManifestImportPageControl::WorkRemaining => return Ok(summary),
            ManifestImportPageControl::SystemFailure(error) => return Err(error),
        }
    }
    Ok(summary)
}

enum ManifestImportPageControl {
    Continue,
    WorkRemaining,
    SystemFailure(anyhow::Error),
}

fn import_manifested_source_page(
    store: &mut Store,
    source: &SourceInfo,
    pending: &[SourceImportFile],
    progress: Option<CodexSessionImportProgressCallback>,
    capture_work_limit: CaptureWorkLimit,
    import_profile: &ImportProfile,
    summary: &mut ProviderImportSummary,
) -> Result<ManifestImportPageControl> {
    for pending_file in pending {
        let pending_context = manifest_pending_source_context(source, pending_file)?;
        // One Mux inventory unit intentionally expands its metadata/chat/
        // partial siblings into separate, independently revalidated
        // NativePath source routes. A token for the pending inventory file
        // cannot be asserted against every sibling path.
        let observation_token = (source.provider != CaptureProvider::Mux)
            .then(|| inventory_observation_token(pending_file))
            .flatten();
        let imported = super::bulk::import_one_source_inner_at_path(
            store,
            pending_context.source,
            &pending_context.input_path,
            progress.clone(),
            true,
            &SourcePreinventory::None,
            capture_work_limit,
            observation_token,
            import_profile,
        );
        match imported {
            Ok(file_summary) => {
                let work_remaining = file_summary.work_remaining;
                if !work_remaining {
                    let completion = if file_summary.failed > 0
                        && !manifested_rejection_has_terminal_cursor(source.provider, &file_summary)
                    {
                        mark_source_import_file_failed(
                            store,
                            pending_file,
                            &source_import_file_failure(&file_summary),
                        )
                    } else {
                        mark_source_import_file_indexed(store, pending_file)
                    };
                    if let Err(error) = completion {
                        if is_source_import_observation_conflict(&error) {
                            return Ok(ManifestImportPageControl::SystemFailure(error));
                        }
                        return Err(error);
                    }
                }
                summary.merge_from(file_summary);
                if work_remaining {
                    return Ok(ManifestImportPageControl::WorkRemaining);
                }
            }
            Err(err) => {
                let failure_scope = import_error_scope(&err);
                let error = error_summary(&err);
                if let Err(mark_error) = mark_source_import_file_failed(store, pending_file, &error)
                {
                    if is_source_import_observation_conflict(&mark_error) {
                        return Ok(ManifestImportPageControl::SystemFailure(mark_error));
                    }
                    return Err(mark_error);
                }
                if failure_scope == ImportFailureScope::System {
                    return Ok(ManifestImportPageControl::SystemFailure(err));
                }
                if err.chain().any(|cause| {
                    matches!(
                        cause.downcast_ref::<CaptureError>(),
                        Some(CaptureError::ProviderSource { .. })
                    )
                }) {
                    return Err(err);
                }
                summary.failed += 1;
                summary
                    .failures
                    .push(ProviderImportFailure { line: 0, error });
            }
        }
    }
    Ok(ManifestImportPageControl::Continue)
}

fn is_source_import_observation_conflict(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<StoreError>(),
        Some(StoreError::SourceImportObservationConflict { .. })
    )
}

fn manifested_rejection_has_terminal_cursor(
    provider: CaptureProvider,
    summary: &ProviderImportSummary,
) -> bool {
    if summary.terminal_outcome() == ProviderImportTerminalOutcome::CoreCursorCommitted {
        return true;
    }
    // OpenHands manifests one complete event file per certified cursor stream.
    // An Ok result means its singleton cursor CAS and source revalidation
    // committed, even when projection retained deterministic record rejections.
    provider == CaptureProvider::OpenHands && summary.failed > 0
}

pub(super) struct ManifestPendingSourceContext<'a> {
    pub(super) source: &'a SourceInfo,
    pub(super) input_path: PathBuf,
}

pub(super) fn manifest_pending_source_context<'a>(
    source: &'a SourceInfo,
    pending_file: &'a SourceImportFile,
) -> Result<ManifestPendingSourceContext<'a>> {
    if pending_file.provider != source.provider
        || pending_file.source_root != source.path.display().to_string()
    {
        return Err(anyhow::Error::new(CaptureError::SystemInvariant(
            "manifest pending file escaped its inventory source root",
        )));
    }
    let source_root = fs::canonicalize(&source.path)
        .with_context(|| format!("resolve inventory source root {}", source.path.display()))?;
    let pending_path = Path::new(&pending_file.source_path);
    let pending_metadata = fs::symlink_metadata(pending_path)
        .with_context(|| format!("stat pending import file {}", pending_path.display()))?;
    if !pending_metadata.file_type().is_file() {
        return Err(anyhow!(
            "pending import path is no longer a regular file: {}",
            pending_path.display()
        ));
    }
    let input_path = fs::canonicalize(pending_path)
        .with_context(|| format!("resolve pending import file {}", pending_path.display()))?;
    if !input_path.starts_with(&source_root) {
        return Err(anyhow!(
            "pending import file escaped its inventory source root: {}",
            pending_path.display()
        ));
    }
    Ok(ManifestPendingSourceContext { source, input_path })
}

fn source_import_file_failure(summary: &ProviderImportSummary) -> String {
    let Some(failure) = summary.failures.first() else {
        return "provider import failed".to_owned();
    };
    match failure.line {
        0 => failure.error.clone(),
        line => format!("line {line}: {}", failure.error),
    }
}
