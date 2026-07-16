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

#[cfg(test)]
fn import_one_source_for_search_refresh(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    preinventory: &SourcePreinventory,
) -> Result<ProviderImportSummary> {
    let refresh_search_after_import = store.event_search_projection_needs_backfill()?;
    import_one_source_inner(
        store,
        source,
        progress,
        refresh_search_after_import,
        false,
        preinventory,
    )
}

pub(crate) fn import_selected_source(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    preinventory: &SourcePreinventory,
    selection: &SelectedImportWork,
) -> Result<SelectedSourceImportResult> {
    let (outcome, remaining_error) = match import_one_source_inner_batched(
        store,
        source,
        progress,
        false,
        preinventory,
        Some(selection),
    ) {
        Ok(outcome) => (outcome, None),
        Err(error) => match error.downcast::<ProviderImportBatchError>() {
            Ok(error) => {
                let (outcome, source) = error.into_parts();
                (outcome, Some(source))
            }
            Err(error) => return Err(error),
        },
    };
    let (selected_units, selected_bytes) = match selection {
        SelectedImportWork::Catalog(work) => (
            work.len(),
            work.iter().fold(0_u64, |total, work| {
                total.saturating_add(work.estimated_bytes)
            }),
        ),
        SelectedImportWork::SourceFiles(work) => (
            work.len(),
            work.iter().fold(0_u64, |total, work| {
                total.saturating_add(work.estimated_bytes)
            }),
        ),
    };
    let mut summary = outcome.summary;
    if selected_work_is_explicit_rescan(selection) {
        summary.skipped = summary.skipped.saturating_add(summary.imported);
        summary.imported = 0;
        summary.skipped_sessions = summary
            .skipped_sessions
            .saturating_add(summary.imported_sessions);
        summary.imported_sessions = 0;
        summary.skipped_events = summary
            .skipped_events
            .saturating_add(summary.imported_events);
        summary.imported_events = 0;
        summary.skipped_edges = summary.skipped_edges.saturating_add(summary.imported_edges);
        summary.imported_edges = 0;
    }
    Ok(SelectedSourceImportResult {
        outcome: SelectedSourceImportOutcome {
            summary,
            completed_units: outcome.completed_units,
            completed_bytes: if outcome.completed_units == selected_units
                && outcome.deferred_units == 0
            {
                selected_bytes
            } else {
                outcome.completed_bytes
            },
            deferred_units: outcome.deferred_units,
            durable_progress: outcome.durable_progress,
            post_import_inventory_generation: outcome.post_import_inventory_generation,
            post_import_preinventory: outcome.post_import_preinventory,
        },
        remaining_error,
    })
}

fn selected_work_is_explicit_rescan(selection: &SelectedImportWork) -> bool {
    match selection {
        SelectedImportWork::Catalog(work) => work
            .iter()
            .all(|unit| unit.reason == ImportPendingReason::ExplicitRescan),
        SelectedImportWork::SourceFiles(work) => work
            .iter()
            .all(|unit| unit.reason == ImportPendingReason::ExplicitRescan),
    }
}

pub(crate) fn import_one_source_inner(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    refresh_search_after_import: bool,
    full_rescan: bool,
    preinventory: &SourcePreinventory,
) -> Result<ProviderImportSummary> {
    import_one_source_inner_with_pre_lock_hook(
        store,
        source,
        progress,
        refresh_search_after_import,
        full_rescan,
        preinventory,
        || {},
    )
}

fn import_one_source_inner_with_pre_lock_hook(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    refresh_search_after_import: bool,
    full_rescan: bool,
    preinventory: &SourcePreinventory,
    pre_lock_hook: impl FnOnce(),
) -> Result<ProviderImportSummary> {
    if !full_rescan && preinventory_is_complete(store, source, preinventory)? {
        if refresh_search_after_import {
            store.refresh_search_index()?;
        }
        return Ok(ProviderImportSummary::default());
    }
    pre_lock_hook();
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let import_result = (|| {
        let mut revalidated = if full_rescan {
            RevalidatedSourcePreinventory::Import(preinventory.clone())
        } else if preinventory_is_complete(store, source, preinventory)? {
            RevalidatedSourcePreinventory::Complete
        } else {
            revalidate_source_preinventory(store, source, preinventory)?
        };
        for _ in 0..3 {
            match revalidated {
                RevalidatedSourcePreinventory::Complete => {
                    return Ok(ProviderImportSummary::default())
                }
                RevalidatedSourcePreinventory::Import(ref current) => {
                    match import_one_source_inner_batched(
                        store,
                        source,
                        progress.clone(),
                        full_rescan,
                        current,
                        None,
                    ) {
                        Err(error) if is_inventory_superseded(&error) => {
                            revalidated = revalidate_source_preinventory(store, source, current)?;
                        }
                        result => return result.map(|outcome| outcome.summary),
                    }
                }
            }
        }
        Err(
            anyhow::Error::new(CaptureError::InventorySuperseded).context(format!(
                "{} inventory generation kept changing during import",
                source.provider.as_str()
            )),
        )
    })();
    let finish_result = store.finish_event_search_bulk_mode(&bulk_guard);
    let summary = match (import_result, finish_result) {
        (Ok(summary), Ok(ctx_history_store::EventSearchBulkMaintenanceOutcome::Complete)) => {
            Ok(summary)
        }
        (Ok(summary), Ok(ctx_history_store::EventSearchBulkMaintenanceOutcome::Pending)) => {
            Ok(summary)
        }
        (_, Err(error)) => Err(error.into()),
        (Err(error), Ok(_)) => Err(error),
    }?;
    if refresh_search_after_import {
        store.refresh_search_index()?;
    }
    Ok(summary)
}

enum RevalidatedSourcePreinventory {
    Complete,
    Import(SourcePreinventory),
}

fn preinventory_is_complete(
    store: &Store,
    source: &SourceInfo,
    preinventory: &SourcePreinventory,
) -> Result<bool> {
    match preinventory {
        SourcePreinventory::SourceRoot { file, .. } => Ok(store
            .list_pending_source_import_files(source.provider, &file.source_root)?
            .is_empty()),
        SourcePreinventory::SourceImportFiles { files, .. } => {
            if files.is_empty() {
                return Ok(false);
            }
            let source_root = &files[0].source_root;
            Ok(store
                .list_pending_source_import_files(source.provider, source_root)?
                .is_empty())
        }
        SourcePreinventory::CodexSessionCatalog {
            summary,
            inventory_generation,
        } => {
            if summary.failed_sessions > 0 {
                return Ok(false);
            }
            let source_root = super::catalog::codex_catalog_root_identity(&source.path)?;
            store
                .catalog_inventory_generation_is_complete_without_pending(
                    CaptureProvider::Codex,
                    source_root,
                    *inventory_generation,
                )
                .map_err(Into::into)
        }
        SourcePreinventory::None => Ok(false),
    }
}

fn is_inventory_superseded(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<CaptureError>(),
            Some(CaptureError::InventorySuperseded)
        )
    })
}

fn revalidate_source_preinventory(
    store: &Store,
    source: &SourceInfo,
    preinventory: &SourcePreinventory,
) -> Result<RevalidatedSourcePreinventory> {
    match preinventory {
        SourcePreinventory::SourceRoot { .. } => {
            let (_, current) = observe_source_root(source)?;
            let persisted = persist_new_source_import_observation(
                store,
                source,
                std::slice::from_ref(&current),
            )?;
            if persisted.pending_files.is_empty() {
                Ok(RevalidatedSourcePreinventory::Complete)
            } else {
                Ok(RevalidatedSourcePreinventory::Import(
                    SourcePreinventory::SourceRoot {
                        file: current,
                        inventory_generation: persisted.inventory_generation,
                    },
                ))
            }
        }
        SourcePreinventory::SourceImportFiles { .. } => {
            let current = collect_source_import_files(source).with_context(|| {
                format!("re-inventory import files from {}", source.path.display())
            })?;
            let persisted = persist_new_source_import_observation(store, source, &current)?;
            if current.is_empty() {
                Ok(RevalidatedSourcePreinventory::Import(
                    SourcePreinventory::SourceImportFiles {
                        files: current,
                        inventory_generation: persisted.inventory_generation,
                    },
                ))
            } else if persisted.pending_files.is_empty() {
                Ok(RevalidatedSourcePreinventory::Complete)
            } else {
                Ok(RevalidatedSourcePreinventory::Import(
                    SourcePreinventory::SourceImportFiles {
                        files: current,
                        inventory_generation: persisted.inventory_generation,
                    },
                ))
            }
        }
        SourcePreinventory::CodexSessionCatalog { .. } => {
            const MAX_GENERATION_RETRIES: usize = 3;
            let source_root = super::catalog::codex_catalog_root_identity(&source.path)?.to_owned();
            for _ in 0..MAX_GENERATION_RETRIES {
                let inventory_generation = store
                    .allocate_catalog_inventory_generation(CaptureProvider::Codex, &source_root)?;
                let summary = match catalog_codex_session_tree(
                    &source.path,
                    store,
                    CodexSessionCatalogOptions {
                        source_root: Some(source.path.clone()),
                        observation_generation: Some(inventory_generation),
                        ..CodexSessionCatalogOptions::default()
                    },
                ) {
                    Ok(summary) => summary,
                    Err(CaptureError::InventorySuperseded) => continue,
                    Err(error) => {
                        return Err(anyhow::Error::new(error).context(format!(
                            "re-inventory Codex sessions from {}",
                            source.path.display()
                        )))
                    }
                };
                if summary.failed_sessions == 0
                    && store.catalog_inventory_generation_is_complete_without_pending(
                        CaptureProvider::Codex,
                        &source_root,
                        inventory_generation,
                    )?
                {
                    return Ok(RevalidatedSourcePreinventory::Complete);
                }
                if !store.catalog_inventory_generation_is_complete(
                    CaptureProvider::Codex,
                    &source_root,
                    inventory_generation,
                )? {
                    continue;
                }
                return Ok(RevalidatedSourcePreinventory::Import(
                    SourcePreinventory::CodexSessionCatalog {
                        summary,
                        inventory_generation,
                    },
                ));
            }
            Err(
                anyhow::Error::new(CaptureError::InventorySuperseded).context(format!(
                    "Codex inventory generation kept changing for {}",
                    source.path.display()
                )),
            )
        }
        SourcePreinventory::None => Ok(RevalidatedSourcePreinventory::Import(preinventory.clone())),
    }
}
