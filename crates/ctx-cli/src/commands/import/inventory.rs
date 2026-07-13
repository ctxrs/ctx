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
use ctx_history_core::CaptureProvider;
use ctx_history_store::{SourceImportFile, Store};

use crate::commands::import::catalog::{source_import_stats, source_stats, system_time_ms};
use crate::commands::import::manifest::{
    collect_source_import_files, persist_source_import_files, source_uses_import_file_manifest,
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
    for (index, source) in sources.into_iter().enumerate() {
        inventory.totals.sources += 1;
        let failure_source = source.clone();
        let (plan, cataloged) = match inventory_import_source(store, source, full_rescan) {
            Ok(inventoried) => inventoried,
            Err(error) if import_error_scope(&error) == ImportFailureScope::Source => {
                inventory.failures.push(ImportSourceFailure {
                    index,
                    source: failure_source,
                    stats: SourceStats::default(),
                    error: error_summary(&error),
                    failure_scope: ImportFailureScope::Source,
                    failure_type: import_failure_type(&error),
                    rejected_summary: None,
                    system_error: None,
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
            SourcePreinventory::SourceImportFiles(files) => {
                inventory.totals.source_import_files += files.len();
            }
            SourcePreinventory::SourceRoot(_) => {
                inventory.totals.source_import_files += 1;
            }
            SourcePreinventory::None | SourcePreinventory::CodexSessionCatalog(_) => {}
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
    full_rescan: bool,
) -> Result<(PlannedImportSource, Option<(CatalogSummary, Value)>)> {
    if !full_rescan && is_incremental_codex_session_tree(&source) {
        let summary = catalog_codex_session_tree(
            &source.path,
            store,
            CodexSessionCatalogOptions {
                source_root: Some(source.path.clone()),
                ..CodexSessionCatalogOptions::default()
            },
        )
        .with_context(|| format!("inventory Codex sessions from {}", source.path.display()))?;
        let stats = SourceStats {
            files: summary.source_files,
            bytes: summary.source_bytes,
            change_token: None,
        };
        let plan = PlannedImportSource {
            source,
            stats,
            preinventory: SourcePreinventory::CodexSessionCatalog(summary.clone()),
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

    if !full_rescan && source_uses_import_file_manifest(&source) {
        let files = collect_source_import_files(&source)
            .with_context(|| format!("inventory import files from {}", source.path.display()))?;
        persist_source_import_files(store, &source, &files)?;
        let stats = source_stats_from_import_files(&files);
        return Ok((
            PlannedImportSource {
                source,
                stats,
                preinventory: SourcePreinventory::SourceImportFiles(files),
            },
            None,
        ));
    }

    if !full_rescan {
        let stats = source_stats(&source.path)
            .with_context(|| format!("inventory import source {}", source.path.display()))?;
        let root_file = source_root_import_file(&source, stats)?;
        persist_source_import_files(store, &source, std::slice::from_ref(&root_file))?;
        return Ok((
            PlannedImportSource {
                source,
                stats,
                preinventory: SourcePreinventory::SourceRoot(root_file),
            },
            None,
        ));
    }

    let stats = source_import_stats(&source)
        .with_context(|| format!("inventory import source {}", source.path.display()))?;
    Ok((
        PlannedImportSource {
            source,
            stats,
            preinventory: SourcePreinventory::None,
        },
        None,
    ))
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

fn source_root_import_file(source: &SourceInfo, stats: SourceStats) -> Result<SourceImportFile> {
    let metadata = fs::metadata(&source.path)
        .with_context(|| format!("stat import source {}", source.path.display()))?;
    Ok(SourceImportFile {
        provider: source.provider,
        source_format: source.source_format.to_owned(),
        source_root: source.path.display().to_string(),
        source_path: source.path.display().to_string(),
        file_size_bytes: stats.bytes,
        file_modified_at_ms: system_time_ms(metadata.modified().unwrap_or(UNIX_EPOCH)),
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
    })
}
