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

use crate::commands::import::catalog::source_stats;
use crate::commands::import::manifest::{
    bounded_source_root_stats, inventory_source_import_files, persist_source_import_files,
    persist_source_import_page, provider_owns_root_manifest, source_uses_import_file_manifest,
};
use crate::commands::import::requests::{
    invalid_missing_explicit_path, missing_explicit_path_has_prior_route_authority,
};
use crate::commands::import::{
    error_summary, import_error_scope, import_failure_type, provider_path_text, system_time_ms,
    CatalogTotals, ImportFailureScope, ImportSourceFailure, InventoryTotals, PlannedImportSource,
    SourcePreinventory, SourceStats,
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
    force_inventory_reindex: bool,
    allow_incremental_codex_catalog: bool,
    allow_missing_prior_routes: bool,
) -> Result<ImportInventory> {
    let _inventory_guard = store.acquire_source_inventory_lock()?;
    let mut inventory = ImportInventory::default();
    for source in sources {
        inventory.totals.sources += 1;
        let failure_source = source.clone();
        let (plan, cataloged) = match inventory_import_source(
            store,
            source,
            force_inventory_reindex,
            allow_incremental_codex_catalog,
            allow_missing_prior_routes,
        ) {
            Ok(inventoried) => inventoried,
            Err(error) if import_error_scope(&error) == ImportFailureScope::Source => {
                inventory.failures.push(ImportSourceFailure {
                    source: failure_source,
                    stats: SourceStats::default(),
                    error: error_summary(&error),
                    failure_type: import_failure_type(&error),
                    rejected_summary: None,
                    runtime_facts: None,
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
            SourcePreinventory::SourceImportManifest => {
                inventory.totals.source_import_files += plan.stats.files;
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
    let mut available = Vec::new();
    for source in sources {
        if source.import_support != ProviderImportSupport::Native {
            continue;
        }
        let is_available = source.exists && source.status == ProviderSourceStatus::Available;
        let is_known_root = if provider_owns_root_manifest(source) {
            store.source_import_file_history_exists(
                source.provider,
                provider_path_text(&source.path)?,
            )?
        } else {
            false
        };
        if is_available || is_known_root {
            available.push(source.clone());
        }
    }
    inventory_import_sources(store, available, false, false, false)
}

fn inventory_import_source(
    store: &Store,
    source: SourceInfo,
    force_inventory_reindex: bool,
    allow_incremental_codex_catalog: bool,
    allow_missing_prior_routes: bool,
) -> Result<(PlannedImportSource, Option<(CatalogSummary, Value)>)> {
    if !source.exists {
        return missing_explicit_path_plan(store, source, allow_missing_prior_routes);
    }
    match fs::symlink_metadata(&source.path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return missing_explicit_path_plan(store, source, allow_missing_prior_routes);
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("stat import source {}", source.path.display()));
        }
    }

    if (!force_inventory_reindex || allow_incremental_codex_catalog)
        && is_incremental_codex_session_tree(&source)
    {
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

    if source_uses_import_file_manifest(&source) {
        let inventoried = inventory_source_import_files(store, &source, force_inventory_reindex)
            .with_context(|| format!("inventory import files from {}", source.path.display()))?;
        let stats = SourceStats {
            files: inventoried.files,
            bytes: inventoried.bytes,
            change_token: None,
        };
        return Ok((
            PlannedImportSource {
                source,
                stats,
                preinventory: SourcePreinventory::SourceImportManifest,
            },
            None,
        ));
    }

    let source_root = provider_path_text(&source.path)?.to_owned();
    let root_metadata = match fs::symlink_metadata(&source.path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return missing_explicit_path_plan(store, source, allow_missing_prior_routes);
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("stat import source {}", source.path.display()))
        }
    };
    let observed_at_ms = store.next_source_import_observed_at_ms(
        source.provider,
        &source_root,
        system_time_ms(SystemTime::now()),
    )?;
    if root_metadata.file_type().is_dir() {
        let discovering = source_root_import_file(&source, SourceStats::default(), observed_at_ms)?;
        persist_source_import_page(store, std::slice::from_ref(&discovering))?;
    }
    let stats = if root_metadata.file_type().is_dir() {
        bounded_source_root_stats(&source.path)
    } else {
        source_stats(&source.path)
    }
    .with_context(|| format!("inventory import source {}", source.path.display()))?;
    let root_file = source_root_import_file(&source, stats, observed_at_ms)?;
    persist_source_import_files(store, &source, std::slice::from_ref(&root_file))?;
    Ok((
        PlannedImportSource {
            source,
            stats,
            preinventory: SourcePreinventory::SourceRoot(root_file),
        },
        None,
    ))
}

fn missing_explicit_path_plan(
    store: &Store,
    source: SourceInfo,
    allow_missing_prior_routes: bool,
) -> Result<(PlannedImportSource, Option<(CatalogSummary, Value)>)> {
    if provider_owns_root_manifest(&source)
        && store
            .source_import_file_history_exists(source.provider, provider_path_text(&source.path)?)?
    {
        // Retire the root scheduling token, but still return a plan: the provider owns exact
        // route retirement and must observe a known root's disappearance.
        persist_source_import_files(store, &source, &[])?;
        return Ok((
            PlannedImportSource {
                source,
                stats: SourceStats::default(),
                preinventory: SourcePreinventory::None,
            },
            None,
        ));
    }
    if allow_missing_prior_routes
        && missing_explicit_path_has_prior_route_authority(store, &source)?
    {
        return Ok((
            PlannedImportSource {
                source,
                stats: SourceStats::default(),
                preinventory: SourcePreinventory::None,
            },
            None,
        ));
    }
    Err(invalid_missing_explicit_path(&source))
}

fn is_incremental_codex_session_tree(source: &SourceInfo) -> bool {
    source.provider == CaptureProvider::Codex && source.source_format == "codex_session_jsonl_tree"
}

fn source_root_import_file(
    source: &SourceInfo,
    stats: SourceStats,
    observed_at_ms: i64,
) -> Result<SourceImportFile> {
    let metadata = fs::metadata(&source.path)
        .with_context(|| format!("stat import source {}", source.path.display()))?;
    let source_root = provider_path_text(&source.path)?.to_owned();
    Ok(SourceImportFile {
        provider: source.provider,
        source_format: source.source_format.to_owned(),
        source_root: source_root.clone(),
        source_path: source_root,
        file_size_bytes: stats.bytes,
        file_modified_at_ms: system_time_ms(metadata.modified().unwrap_or(UNIX_EPOCH)),
        observed_at_ms,
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
