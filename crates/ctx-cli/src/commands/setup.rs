use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Result};
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
use crate::progress::{format_bytes, format_count, plural, ProgressArg, ProgressReporter};
use crate::provider_sources::{discovered_sources, sources_json};
use crate::semantic::semantic_query_service_supported;
use crate::{analytics, config, ImportArgs, SetupArgs};

pub(crate) fn run_setup(
    args: SetupArgs,
    data_root: PathBuf,
    analytics_properties: &mut AnalyticsProperties,
    quiet: bool,
    config: &config::AppConfig,
) -> Result<()> {
    fs::create_dir_all(&data_root)?;
    let db_path = database_path(data_root.clone());
    let store = Store::open(&db_path)?;
    let config_path = data_root.join(CONFIG_FILE);
    config::write_default_config(&data_root)?;
    let semantic_enabled = config.semantic_search_enabled();
    let semantic_supported = semantic_query_service_supported();
    if semantic_enabled && semantic_supported && (!config.daemon.enabled || args.no_daemon) {
        bail!(
            "local semantic search requires the ctx daemon. Set [daemon] enabled = true, remove --no-daemon, or set [search] semantic = false"
        );
    }
    let sources = discovered_sources();
    let progress_arg = setup_progress_arg(args.progress, quiet);
    let progress = ProgressReporter::new(progress_arg, args.json, "setup", 0);
    let daemon_backgrounding_enabled = config.daemon.enabled && !args.no_daemon;
    let foreground_import = !args.catalog_only && (args.wait || !daemon_backgrounding_enabled);
    let mut inventory_only = None;
    let import_report = if args.catalog_only || !foreground_import {
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
            partial: true,
            no_daemon: args.no_daemon,
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
    let inventory_units = catalog_counts
        .total
        .saturating_add(source_import_file_counts.total);
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
    let background_indexing_enabled = daemon_backgrounding_enabled
        && !args.catalog_only
        && !foreground_import
        && (pending_inventory_units > 0 || (semantic_enabled && semantic_supported));

    if args.json {
        print_json(json!({
            "schema_version": 1,
            "data_root": data_root,
            "database_path": db_path,
            "config_path": config_path,
            "mode": if args.catalog_only {
                "catalog_only"
            } else if foreground_import {
                "ready"
            } else {
                "background"
            },
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
            "import": setup_import_json(import_report.as_ref(), args.catalog_only),
            "background_indexing": setup_background_indexing_json(
                &inventory_totals,
                inventory_units,
                background_indexing_enabled,
                semantic_enabled,
                semantic_supported,
                args.json
            ),
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
                foreground_import,
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
            if background_indexing_enabled {
                print_background_indexing_guidance(
                    &inventory_totals,
                    inventory_units,
                    semantic_enabled,
                    semantic_supported,
                );
            }
            println!("Get started:");
            if args.catalog_only {
                println!("  ctx import --all");
                println!("  ctx sources");
            } else if background_indexing_enabled {
                println!("  ctx index watch");
                println!("  ctx search \"test failure\"");
                println!("  ctx status");
            } else if !foreground_import {
                println!("  ctx search \"test failure\"");
                println!("  ctx status");
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

pub(crate) fn setup_import_json(report: Option<&ImportReport>, catalog_only: bool) -> Value {
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
            "reason": if catalog_only { "catalog_only" } else { "background" },
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
    foreground_import: bool,
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
    if !foreground_import {
        if pending_inventory_units > 0 {
            println!(
                "ctx is initialized; local history indexing is queued for background processing"
            );
        } else {
            println!("ctx is initialized; background indexing has no pending local history");
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

fn setup_background_indexing_json(
    inventory: &InventoryTotals,
    units: usize,
    enabled: bool,
    semantic_enabled: bool,
    semantic_supported: bool,
    json_output: bool,
) -> Value {
    let semantic_estimate = semantic_index_estimate(inventory);
    json!({
        "enabled": enabled,
        "semantic_enabled": semantic_enabled,
        "semantic_supported": semantic_supported,
        "units": units,
        "source_bytes": inventory.source_bytes,
        "lexical_estimate_seconds": enabled.then(|| estimate_lexical_index_seconds(inventory)),
        "semantic_estimate_seconds": (enabled && semantic_enabled && semantic_supported).then_some(semantic_estimate.expected_seconds),
        "semantic_estimate_backend": (enabled && semantic_enabled && semantic_supported).then_some(semantic_estimate.backend),
        "semantic_cpu_fallback_estimate_seconds": (enabled && semantic_enabled && semantic_supported).then_some(semantic_estimate.cpu_fallback_seconds).flatten(),
        "daemon_autostart": setup_daemon_autostart_json(enabled, json_output),
        "status_command": "ctx index status",
        "watch_command": "ctx index watch",
        "wait_command": "ctx index wait --all",
    })
}

fn setup_daemon_autostart_json(enabled: bool, json_output: bool) -> Value {
    if !enabled {
        return json!({
            "status": "not_needed",
            "reason": "not_requested",
            "status_command": "ctx daemon status",
        });
    }
    if json_output {
        return json!({
            "status": "skipped",
            "reason": "json_output",
            "status_command": "ctx daemon status",
        });
    }
    json!({
        "status": "deferred",
        "reason": null,
        "status_command": "ctx daemon status",
    })
}

fn print_background_indexing_guidance(
    inventory: &InventoryTotals,
    units: usize,
    semantic_enabled: bool,
    semantic_supported: bool,
) {
    println!("ctx queued your local agent history for background indexing.");
    println!(
        "Identified {} {} ({}).",
        format_count(units),
        plural(units, "record", "records"),
        format_bytes(inventory.source_bytes)
    );
    println!(
        "Estimated lexical indexing: {}.",
        format_duration_estimate(estimate_lexical_index_seconds(inventory))
    );
    if semantic_enabled && semantic_supported {
        let estimate = semantic_index_estimate(inventory);
        println!(
            "Semantic search: enabled; the daemon will download the local embedding model if needed."
        );
        if let Some(cpu_fallback_seconds) = estimate.cpu_fallback_seconds {
            println!(
                "Estimated semantic indexing: {} with CoreML; CPU fallback can take about {}.",
                format_duration_estimate(estimate.expected_seconds),
                format_duration_estimate(cpu_fallback_seconds)
            );
        } else {
            println!(
                "Estimated semantic indexing: {} with {}.",
                format_duration_estimate(estimate.expected_seconds),
                estimate.backend
            );
        }
    } else if semantic_enabled {
        println!("Semantic search: unavailable on this platform; lexical indexing will continue.");
    } else {
        println!("Semantic search: disabled.");
    }
    println!();
    println!("To watch progress:");
    println!("  ctx index watch");
    println!("To inspect daemon status:");
    println!("  ctx daemon status");
    println!("To wait until ready:");
    println!("  ctx index wait --all");
    println!();
}

fn estimate_lexical_index_seconds(inventory: &InventoryTotals) -> u64 {
    estimate_seconds_for_bytes(inventory.source_bytes, 16 * 1024 * 1024)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SemanticIndexEstimate {
    expected_seconds: u64,
    backend: &'static str,
    cpu_fallback_seconds: Option<u64>,
}

fn semantic_index_estimate(inventory: &InventoryTotals) -> SemanticIndexEstimate {
    let preference = env::var("CTX_INTERNAL_SEMANTIC_BACKEND").ok();
    semantic_index_estimate_for(
        inventory,
        preference.as_deref(),
        cfg!(all(target_os = "macos", target_arch = "aarch64")),
    )
}

fn semantic_index_estimate_for(
    inventory: &InventoryTotals,
    preference: Option<&str>,
    apple_silicon: bool,
) -> SemanticIndexEstimate {
    const COREML_BYTES_PER_SECOND: u64 = 5 * 1024 * 1024 / 4;
    const CPU_BYTES_PER_SECOND: u64 = 256 * 1024;

    // These are measured end-to-end planning rates under the quiet daemon
    // policy, not startup benchmarks. Unknown internal overrides use the
    // conservative CPU estimate; backend acquisition will report their error.
    let preference = preference.map(str::trim).filter(|value| !value.is_empty());
    let coreml_expected =
        apple_silicon && matches!(preference, None | Some("auto") | Some("coreml"));
    if coreml_expected {
        SemanticIndexEstimate {
            expected_seconds: estimate_seconds_for_bytes(
                inventory.source_bytes,
                COREML_BYTES_PER_SECOND,
            ),
            backend: "CoreML",
            cpu_fallback_seconds: matches!(preference, None | Some("auto"))
                .then(|| estimate_seconds_for_bytes(inventory.source_bytes, CPU_BYTES_PER_SECOND)),
        }
    } else {
        SemanticIndexEstimate {
            expected_seconds: estimate_seconds_for_bytes(
                inventory.source_bytes,
                CPU_BYTES_PER_SECOND,
            ),
            backend: "CPU",
            cpu_fallback_seconds: None,
        }
    }
}

fn estimate_seconds_for_bytes(bytes: u64, bytes_per_second: u64) -> u64 {
    if bytes == 0 {
        return 0;
    }
    bytes.div_ceil(bytes_per_second).max(1)
}

fn format_duration_estimate(seconds: u64) -> String {
    if seconds == 0 {
        "under 1 minute".to_owned()
    } else if seconds < 60 {
        format!("{seconds} sec")
    } else if seconds < 3_600 {
        let minutes = seconds.div_ceil(60);
        format!(
            "{} {}",
            minutes,
            plural(minutes as usize, "minute", "minutes")
        )
    } else {
        let rounded_minutes = seconds.div_ceil(60);
        let hours = rounded_minutes / 60;
        let minutes = rounded_minutes % 60;
        if minutes == 0 {
            format!("{} {}", hours, plural(hours as usize, "hour", "hours"))
        } else {
            format!(
                "{} {}, {} {}",
                hours,
                plural(hours as usize, "hour", "hours"),
                minutes,
                plural(minutes as usize, "minute", "minutes")
            )
        }
    }
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

#[cfg(test)]
mod setup_estimate_tests {
    use super::*;

    #[test]
    fn semantic_estimate_uses_quiet_backend_throughput() {
        let inventory = InventoryTotals {
            source_bytes: 15 * 1024 * 1024 * 1024,
            ..InventoryTotals::default()
        };
        let coreml = semantic_index_estimate_for(&inventory, None, true);
        assert_eq!(coreml.expected_seconds, 12_288);
        assert_eq!(coreml.backend, "CoreML");
        assert_eq!(coreml.cpu_fallback_seconds, Some(61_440));

        let forced_cpu = semantic_index_estimate_for(&inventory, Some("cpu"), true);
        assert_eq!(forced_cpu.expected_seconds, 61_440);
        assert_eq!(forced_cpu.backend, "CPU");
        assert_eq!(forced_cpu.cpu_fallback_seconds, None);

        let conservative = semantic_index_estimate_for(&inventory, None, false);
        assert_eq!(conservative.expected_seconds, 61_440);
        assert_eq!(conservative.backend, "CPU");
    }

    #[test]
    fn duration_estimate_carries_rounded_minutes_into_hours() {
        assert_eq!(format_duration_estimate(7_199), "2 hours");
    }
}
