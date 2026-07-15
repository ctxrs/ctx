use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use ctx_history_capture::{
    catalog_codex_session_tree, CatalogSummary, CodexSessionCatalogOptions, ProviderImportSupport,
    ProviderSourceStatus,
};
use ctx_history_core::{utc_now, CaptureProvider};
use ctx_history_store::{
    CatalogIndexedStatus, CatalogSourceIndexUpdate, SourceImportFile, SourceImportFileIndexUpdate,
    Store,
};

use crate::commands::import::catalog::codex_catalog_root_identity;
use crate::commands::import::catalog::{source_stats, system_time_ms};
use crate::commands::import::manifest::{
    collect_source_import_files, persist_new_source_import_observation, persisted_import_identity,
    source_uses_import_file_manifest,
};
use crate::commands::import::{
    error_summary, import_error_scope, import_failure_type, CatalogTotals, ImportFailureScope,
    ImportSourceFailure, InventoryTotals, PlannedImportSource, SourcePreinventory, SourceStats,
};
use crate::provider_sources::SourceInfo;

#[derive(Debug, Default)]
pub(crate) struct ImportInventory {
    pub(crate) sources: Vec<PlannedImportSource>,
    pub(crate) failures: Vec<ImportSourceFailure>,
    pub(crate) totals: InventoryTotals,
    pub(crate) catalog: CatalogTotals,
    pub(crate) catalog_sources: Vec<Value>,
}

pub(crate) fn inventory_import_sources(
    store: &Store,
    sources: Vec<SourceInfo>,
    full_rescan: bool,
) -> Result<ImportInventory> {
    let mut inventory = ImportInventory::default();
    for source in sources {
        inventory.totals.sources += 1;
        let failure_source = source.clone();
        let (plan, cataloged) = match inventory_import_source(store, source, full_rescan) {
            Ok(inventoried) => inventoried,
            Err(error) if import_error_scope(&error) == ImportFailureScope::Source => {
                inventory.failures.push(ImportSourceFailure {
                    source: failure_source,
                    stats: SourceStats::default(),
                    error: error_summary(&error),
                    failure_type: import_failure_type(&error),
                    rejected_summary: None,
                });
                continue;
            }
            Err(error) => return Err(error),
        };
        inventory.totals.source_files += plan.stats.files;
        inventory.totals.source_bytes = inventory
            .totals
            .source_bytes
            .saturating_add(plan.stats.bytes);
        match &plan.preinventory {
            SourcePreinventory::SourceImportFiles { files, .. } => {
                inventory.totals.source_import_files += files.len();
            }
            SourcePreinventory::SourceRoot { .. } => {
                inventory.totals.source_import_files += 1;
            }
            SourcePreinventory::None | SourcePreinventory::CodexSessionCatalog { .. } => {}
        }
        if let Some((summary, source_json)) = cataloged {
            inventory.catalog.add(&summary);
            inventory.totals.codex_catalog_sources += 1;
            inventory.totals.codex_catalog_sessions += summary.cataloged_sessions;
            inventory.catalog_sources.push(source_json);
        }
        inventory.sources.push(plan);
    }
    Ok(inventory)
}

pub(crate) fn inventory_available_sources(
    store: &Store,
    sources: &[SourceInfo],
) -> Result<ImportInventory> {
    let available = sources
        .iter()
        .filter(|source| {
            source.exists
                && source.status == ProviderSourceStatus::Available
                && source.import_support == ProviderImportSupport::Native
        })
        .cloned()
        .collect::<Vec<_>>();
    inventory_import_sources(store, available, false)
}

fn inventory_import_source(
    store: &Store,
    source: SourceInfo,
    resume: bool,
) -> Result<(PlannedImportSource, Option<(CatalogSummary, Value)>)> {
    if source.provider == CaptureProvider::Codex {
        codex_catalog_root_identity(&source.path)?;
    }
    if is_incremental_codex_session_tree(&source) {
        let source_root = persisted_import_identity(&source.path, "source root")?.to_owned();
        let mut cataloged = None;
        for _ in 0..3 {
            let inventory_generation = store
                .allocate_catalog_inventory_generation(CaptureProvider::Codex, &source_root)?;
            match catalog_codex_session_tree(
                &source.path,
                store,
                CodexSessionCatalogOptions {
                    source_root: Some(source.path.clone()),
                    observation_generation: Some(inventory_generation),
                    ..CodexSessionCatalogOptions::default()
                },
            ) {
                Ok(summary) => {
                    cataloged = Some((inventory_generation, summary));
                    break;
                }
                Err(ctx_history_capture::CaptureError::InventorySuperseded) => continue,
                Err(error) => {
                    return Err(anyhow::Error::new(error).context(format!(
                        "inventory Codex sessions from {}",
                        source.path.display()
                    )))
                }
            }
        }
        let Some((inventory_generation, summary)) = cataloged else {
            return Err(
                anyhow::Error::new(ctx_history_capture::CaptureError::InventorySuperseded).context(
                    format!(
                        "Codex inventory generation kept changing for {}",
                        source.path.display()
                    ),
                ),
            );
        };
        let stats = SourceStats {
            files: summary.source_files,
            bytes: summary.source_bytes,
            change_token: None,
        };
        if resume {
            schedule_pending_catalog_resume(
                store,
                CaptureProvider::Codex,
                &source_root,
                inventory_generation,
            )?;
        }
        let plan = PlannedImportSource {
            source,
            stats,
            preinventory: SourcePreinventory::CodexSessionCatalog {
                summary: summary.clone(),
                inventory_generation,
            },
        };
        let source_json = json!({
            "provider": plan.source.provider.as_str(),
            "path": plan.source.path.clone(),
            "source_format": plan.source.source_format,
            "source_files": summary.source_files,
            "source_bytes": summary.source_bytes,
            "cataloged_sessions": summary.cataloged_sessions,
            "cached_sessions": summary.cached_sessions,
            "parsed_sessions": summary.parsed_sessions,
            "skipped_sessions": summary.skipped_sessions,
            "failed_sessions": summary.failed_sessions,
        });
        return Ok((plan, Some((summary, source_json))));
    }

    if source_uses_import_file_manifest(&source) {
        let files = collect_source_import_files(&source)
            .with_context(|| format!("inventory import files from {}", source.path.display()))?;
        if files.is_empty() {
            return Err(anyhow::anyhow!(
                "no importable {} history files found under {}",
                source.provider.as_str(),
                source.path.display()
            ));
        }
        let persisted = persist_new_source_import_observation(store, &source, &files)?;
        if resume {
            schedule_pending_source_resume(
                store,
                source.provider,
                persisted_import_identity(&source.path, "source root")?,
                persisted.inventory_generation,
            )?;
        }
        let stats = source_stats_from_import_files(&files);
        return Ok((
            PlannedImportSource {
                source,
                stats,
                preinventory: SourcePreinventory::SourceImportFiles {
                    files,
                    inventory_generation: persisted.inventory_generation,
                },
            },
            None,
        ));
    }

    let (stats, root_file) = observe_source_root(&source)?;
    let persisted =
        persist_new_source_import_observation(store, &source, std::slice::from_ref(&root_file))?;
    if resume {
        schedule_pending_source_resume(
            store,
            source.provider,
            &root_file.source_root,
            persisted.inventory_generation,
        )?;
    }
    Ok((
        PlannedImportSource {
            source,
            stats,
            preinventory: SourcePreinventory::SourceRoot {
                file: root_file,
                inventory_generation: persisted.inventory_generation,
            },
        },
        None,
    ))
}

fn schedule_pending_catalog_resume(
    store: &Store,
    provider: CaptureProvider,
    source_root: &str,
    inventory_generation: u64,
) -> Result<()> {
    for session in store.list_pending_catalog_sessions(provider, source_root)? {
        let state = store.catalog_source_index_state(
            session.provider,
            &session.source_root,
            &session.source_path,
        )?;
        store.record_catalog_source_import_result(
            session.provider,
            CatalogSourceIndexUpdate {
                source_root: &session.source_root,
                source_path: &session.source_path,
                file_size_bytes: session.file_size_bytes,
                file_modified_at_ms: session.file_modified_at_ms,
                import_revision: session.import_revision,
                inventory_generation,
                file_sha256: state
                    .as_ref()
                    .and_then(|state| state.last_imported_file_sha256.as_deref()),
                event_count: state
                    .as_ref()
                    .and_then(|state| state.last_imported_event_count),
                indexed_at_ms: utc_now().timestamp_millis(),
            },
            CatalogIndexedStatus::Pending,
            None,
        )?;
    }
    Ok(())
}

fn schedule_pending_source_resume(
    store: &Store,
    provider: CaptureProvider,
    source_root: &str,
    inventory_generation: u64,
) -> Result<()> {
    for file in store.list_pending_source_import_files(provider, source_root)? {
        store.record_source_import_file_result(
            file.provider,
            SourceImportFileIndexUpdate {
                source_root: &file.source_root,
                source_path: &file.source_path,
                file_size_bytes: file.file_size_bytes,
                file_modified_at_ms: file.file_modified_at_ms,
                import_revision: file.import_revision,
                inventory_generation,
                metadata: &file.metadata,
                indexed_at_ms: utc_now().timestamp_millis(),
            },
            CatalogIndexedStatus::Pending,
            None,
        )?;
    }
    Ok(())
}

fn is_incremental_codex_session_tree(source: &SourceInfo) -> bool {
    source.provider == CaptureProvider::Codex && source.source_format == "codex_session_jsonl_tree"
}

fn source_stats_from_import_files(files: &[SourceImportFile]) -> SourceStats {
    SourceStats {
        files: files.len(),
        bytes: files.iter().fold(0_u64, |bytes, file| {
            bytes.saturating_add(file.file_size_bytes)
        }),
        change_token: None,
    }
}

pub(crate) fn observe_source_root(source: &SourceInfo) -> Result<(SourceStats, SourceImportFile)> {
    let stats = source_stats(&source.path)
        .with_context(|| format!("inventory import source {}", source.path.display()))?;
    let metadata = fs::metadata(&source.path)
        .with_context(|| format!("stat import source {}", source.path.display()))?;
    let source_identity = persisted_import_identity(&source.path, "source root")?;
    let file = SourceImportFile {
        provider: source.provider,
        source_format: source.source_format.to_owned(),
        source_root: source_identity.to_owned(),
        source_path: source_identity.to_owned(),
        file_size_bytes: stats.bytes,
        file_modified_at_ms: system_time_ms(metadata.modified().unwrap_or(UNIX_EPOCH)),
        import_revision: source.import_revision,
        observed_at_ms: system_time_ms(SystemTime::now()),
        metadata: json!({
            "inventory_unit": "source_root",
            "source_files": stats.files,
            "change_token_v1": stats
                .change_token
                .unwrap_or_default()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join(""),
        }),
    };
    Ok((stats, file))
}
