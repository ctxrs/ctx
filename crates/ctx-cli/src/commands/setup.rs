use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;
use serde_json::{json, Value};

use ctx_history_core::database_path;
use ctx_history_store::Store;

use crate::analytics::AnalyticsProperties;
use crate::commands::import::{
    import_totals_json, inventory_available_sources, run_import_internal, CatalogTotals,
    ImportInventory, ImportReport, ImportRunOptions, InventoryTotals,
};
use crate::config::CONFIG_FILE;
use crate::output::print_json;
use crate::progress::{format_count, plural, ProgressArg, ProgressReporter};
use crate::provider_sources::{discovered_sources, sources_json};
use crate::{analytics, config, ImportArgs, SetupArgs};

pub(crate) fn run_setup(
    args: SetupArgs,
    data_root: PathBuf,
    analytics_properties: &mut AnalyticsProperties,
    quiet: bool,
) -> Result<()> {
    fs::create_dir_all(&data_root)?;
    let db_path = database_path(data_root.clone());
    let store = Store::open(&db_path)?;
    let config_path = data_root.join(CONFIG_FILE);
    config::write_default_config(&data_root)?;
    let sources = discovered_sources();
    let progress_arg = setup_progress_arg(args.progress, quiet);
    let progress = ProgressReporter::new(progress_arg, args.json, "setup", 0);
    let mut inventory_only = None;
    let import_report = if args.catalog_only {
        progress.message("inventorying", "Preparing local history...");
        let inventory = inventory_available_sources(&store, &sources)?;
        progress.done(
            "inventorying",
            format!(
                "Found {} history {} ({}).",
                format_count(inventory.totals.sources),
                plural(inventory.totals.sources, "source", "sources"),
                crate::progress::format_bytes(inventory.totals.source_bytes)
            ),
            inventory.totals.source_bytes,
        );
        inventory_only = Some(inventory);
        None
    } else {
        drop(store);
        let import_args = ImportArgs {
            provider: None,
            path: None,
            history_source: None,
            history_source_manifest: Vec::new(),
            reset_cursor: false,
            format: None,
            all: true,
            resume: false,
            partial: false,
            json: args.json,
            progress: progress_arg,
        };
        Some(run_import_internal(
            &import_args,
            data_root.clone(),
            analytics_properties,
            ImportRunOptions {
                progress: progress_arg,
                json: args.json,
                print_human: false,
                allow_empty_sources: true,
                include_history_source_plugins: false,
                operation: "setup",
            },
        )?)
    };
    let inventory_totals = setup_inventory_totals(import_report.as_ref(), inventory_only.as_ref());
    let catalog = setup_catalog_totals(import_report.as_ref(), inventory_only.as_ref());
    let catalog_sources = setup_catalog_sources(import_report.as_ref(), inventory_only.as_ref());
    let setup_store = Store::open(&db_path)?;
    let catalog_counts = setup_store.catalog_session_counts()?;
    let source_import_file_counts = setup_store.source_import_file_counts()?;
    let pending_inventory_units = catalog_counts
        .pending
        .saturating_add(source_import_file_counts.pending);
    analytics::insert_count_bucket(
        analytics_properties,
        "providers_detected_bucket",
        sources.len() as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "cataloged_sessions_bucket",
        catalog.cataloged_sessions as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "inventory_sources_bucket",
        inventory_totals.sources as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "inventory_source_files_bucket",
        inventory_totals.source_files as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "pending_sessions_bucket",
        catalog_counts.pending as u64,
    );
    analytics::insert_bytes_bucket(
        analytics_properties,
        "catalog_source_bytes_bucket",
        catalog.source_bytes,
    );
    analytics::insert_bytes_bucket(
        analytics_properties,
        "inventory_source_bytes_bucket",
        inventory_totals.source_bytes,
    );
    let indexed_items = indexed_history_item_count(&setup_store)?;
    insert_store_analytics_counts(analytics_properties, &setup_store)?;
    analytics::insert_bool(
        analytics_properties,
        "has_indexed_content_after_setup",
        setup_has_indexed_content(indexed_items),
    );

    if args.json {
        print_json(json!({
            "schema_version": 1,
            "data_root": data_root,
            "database_path": db_path,
            "config_path": config_path,
            "mode": if args.catalog_only { "catalog_only" } else { "ready" },
            "indexed_items": indexed_items,
            "sources": sources_json(&sources),
            "inventory": inventory_totals_json(
                &inventory_totals,
                &catalog_counts,
                &source_import_file_counts
            ),
            "catalog": {
                "sources": catalog.sources,
                "source_files": catalog.source_files,
                "source_bytes": catalog.source_bytes,
                "cataloged_sessions": catalog.cataloged_sessions,
                "cached_sessions": catalog.cached_sessions,
                "parsed_sessions": catalog.parsed_sessions,
                "indexed_sessions": catalog_counts.indexed,
                "pending_sessions": catalog_counts.pending,
                "skipped_sessions": catalog.skipped_sessions,
                "failed_sessions": catalog.failed_sessions,
                "failed_index_sessions": catalog_counts.failed,
                "stale_sessions": catalog_counts.stale,
            },
            "catalog_sources": catalog_sources,
            "import": setup_import_json(import_report.as_ref()),
            "network_required": false,
            "repo_writes": false,
        }))?;
    } else {
        progress.finish_line();
        if !quiet {
            if progress.is_enabled() {
                println!();
            }
            print_setup_status_line(
                import_report.as_ref(),
                args.catalog_only,
                pending_inventory_units,
                indexed_items,
            );
            if !setup_has_indexed_content(indexed_items) && catalog.cataloged_sessions > 0 {
                println!(
                    "Prepared {} Codex sessions.",
                    format_count(catalog.cataloged_sessions)
                );
            }
            if let Some(report) = &import_report {
                if report.totals.imported_sources > 0
                    || report.totals.imported_sessions > 0
                    || report.totals.imported_events > 0
                {
                    println!(
                        "Indexed {} {}, {} {} from {} {}.",
                        format_count(report.totals.imported_sessions),
                        plural(report.totals.imported_sessions, "session", "sessions"),
                        format_count(report.totals.imported_events),
                        plural(report.totals.imported_events, "event", "events"),
                        format_count(report.totals.imported_sources),
                        plural(report.totals.imported_sources, "source", "sources")
                    );
                }
                if report.totals.failed_sources > 0 {
                    println!(
                        "Skipped {} {}.",
                        format_count(report.totals.failed_sources),
                        plural(report.totals.failed_sources, "source", "sources")
                    );
                }
            }
            println!("Data: {}", data_root.display());
            println!();
            println!("Get started:");
            if args.catalog_only {
                println!("  ctx import --all");
                println!("  ctx sources");
            } else if setup_has_indexed_content(indexed_items) {
                println!("  ctx search \"test failure\"");
                println!("  ctx show event <event-id> --window 3");
                println!("  ctx show session <session-id>");
                println!("  ctx sources");
                if setup_has_failed_sources(import_report.as_ref()) {
                    println!("  ctx import --provider <provider>");
                }
            } else {
                println!("  ctx sources");
                println!("  ctx import --all");
            }
        }
    }
    Ok(())
}

fn setup_progress_arg(progress: ProgressArg, quiet: bool) -> ProgressArg {
    if quiet && progress == ProgressArg::Auto {
        ProgressArg::None
    } else {
        progress
    }
}

pub(crate) fn setup_import_json(report: Option<&ImportReport>) -> Value {
    match report {
        Some(report) => json!({
            "ran": true,
            "resume": report.resume,
            "resume_mode": report.resume_mode(),
            "totals": import_totals_json(&report.totals),
            "sources": report.sources.clone(),
        }),
        None => json!({
            "ran": false,
            "reason": "catalog_only",
        }),
    }
}

pub(crate) fn inventory_totals_json(
    inventory: &InventoryTotals,
    catalog_counts: &ctx_history_store::CatalogCounts,
    source_import_file_counts: &ctx_history_store::SourceImportFileCounts,
) -> Value {
    let units = catalog_counts
        .total
        .saturating_add(source_import_file_counts.total);
    json!({
        "sources": inventory.sources,
        "units": units,
        "source_files": inventory.source_files,
        "source_bytes": inventory.source_bytes,
        "source_import_files": inventory.source_import_files,
        "indexed_source_import_files": source_import_file_counts.indexed,
        "pending_source_import_files": source_import_file_counts.pending,
        "failed_source_import_files": source_import_file_counts.failed,
        "stale_source_import_files": source_import_file_counts.stale,
        "codex_catalog_sources": inventory.codex_catalog_sources,
        "codex_catalog_sessions": inventory.codex_catalog_sessions,
        "indexed_catalog_sessions": catalog_counts.indexed,
        "pending_catalog_sessions": catalog_counts.pending,
        "failed_catalog_sessions": catalog_counts.failed,
        "stale_catalog_sessions": catalog_counts.stale,
    })
}

fn setup_inventory_totals(
    report: Option<&ImportReport>,
    inventory_only: Option<&ImportInventory>,
) -> InventoryTotals {
    report
        .map(|report| report.inventory.clone())
        .or_else(|| inventory_only.map(|inventory| inventory.totals.clone()))
        .unwrap_or_default()
}

fn setup_catalog_totals(
    report: Option<&ImportReport>,
    inventory_only: Option<&ImportInventory>,
) -> CatalogTotals {
    report
        .map(|report| report.catalog.clone())
        .or_else(|| inventory_only.map(|inventory| inventory.catalog.clone()))
        .unwrap_or_default()
}

fn setup_catalog_sources(
    report: Option<&ImportReport>,
    inventory_only: Option<&ImportInventory>,
) -> Vec<Value> {
    report
        .map(|report| report.catalog_sources.clone())
        .or_else(|| inventory_only.map(|inventory| inventory.catalog_sources.clone()))
        .unwrap_or_default()
}

pub(crate) fn print_setup_status_line(
    report: Option<&ImportReport>,
    catalog_only: bool,
    pending_inventory_units: usize,
    indexed_items: usize,
) {
    if catalog_only {
        if pending_inventory_units > 0 {
            println!("ctx local history inventory is ready; import is still pending");
        } else {
            println!("ctx local history inventory is ready");
        }
        return;
    }
    let Some(report) = report else {
        println!("ctx is initialized; no local history was indexed");
        return;
    };
    if setup_has_indexed_content(indexed_items) && report.totals.failed_sources > 0 {
        println!("ctx indexed available local agent history; some sources were skipped");
    } else if setup_has_indexed_content(indexed_items) {
        println!("ctx local agent history search is ready");
    } else {
        println!("ctx is initialized; no local history was indexed");
    }
}

pub(crate) fn setup_has_indexed_content(indexed_items: usize) -> bool {
    indexed_items > 0
}

pub(crate) fn indexed_history_item_count(store: &Store) -> Result<usize> {
    Ok(store.indexed_history_item_count()?)
}

pub(crate) fn insert_store_analytics_counts(
    analytics_properties: &mut AnalyticsProperties,
    store: &Store,
) -> Result<()> {
    let counts = store.indexed_history_counts()?;
    analytics::insert_count_bucket(
        analytics_properties,
        "indexed_sessions_bucket",
        counts.sessions as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "indexed_events_bucket",
        counts.events as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "indexed_items_bucket",
        counts.items() as u64,
    );
    Ok(())
}

pub(crate) fn insert_db_size_bucket(
    analytics_properties: &mut AnalyticsProperties,
    db_path: &Path,
) {
    let bytes = fs::metadata(db_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    analytics::insert_bytes_bucket(analytics_properties, "db_size_bucket", bytes);
}

pub(crate) fn setup_has_failed_sources(report: Option<&ImportReport>) -> bool {
    report.is_some_and(|report| report.totals.failed_sources > 0)
}
