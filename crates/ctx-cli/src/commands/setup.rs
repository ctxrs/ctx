use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Result};
use serde_json::{json, Value};

use ctx_history_core::database_path;
use ctx_history_store::Store;

use crate::analytics::{self, ProviderRefreshTrigger, SetupMode, SetupTelemetry, StoreTelemetry};
use crate::commands::import::{
    import_report_analytics_outcome, import_report_failure_type, import_totals_json,
    insert_import_error_analytics, insert_import_report_analytics, inventory_available_sources,
    run_import_internal, CatalogTotals, ImportInventory, ImportReport, ImportRunOptions,
    InventoryTotals, ProviderRefreshCollector,
};
use crate::config::CONFIG_FILE;
use crate::output::print_json;
use crate::progress::{format_bytes, format_count, plural, ProgressArg, ProgressReporter};
use crate::provider_sources::{discovered_sources, sources_json};
use crate::semantic::{
    autostart_daemon_and_wait, daemon_autostart_can_reuse_existing,
    daemon_autostart_suppression_reason, semantic_query_service_supported, DaemonHandoff,
};
use crate::{config, ImportArgs, SetupArgs};


pub(crate) fn run_setup(
    args: SetupArgs,
    data_root: PathBuf,
    telemetry: &mut SetupTelemetry,
    provider_refreshes: &mut ProviderRefreshCollector,
    quiet: bool,
    config: &mut config::AppConfig,
) -> Result<()> {
    let semantic_supported = semantic_query_service_supported();
    if args.semantic && (!config.daemon.enabled || args.no_daemon) {
        bail!(
            "`ctx setup --semantic` requires daemon maintenance. Enable [daemon] enabled = true and rerun without --no-daemon"
        );
    }
    if args.semantic {
        config::set_semantic_search_enabled(&data_root, true)?;
        config.search.semantic = Some(true);
    }
    let semantic_enabled = config.semantic_search_enabled();
    if semantic_enabled && semantic_supported && (!config.daemon.enabled || args.no_daemon) {
        bail!(
            "local semantic search requires the ctx daemon. Set [daemon] enabled = true, remove --no-daemon, or set [search] semantic = false"
        );
    }

    fs::create_dir_all(&data_root)?;
    let db_path = database_path(data_root.clone());
    let json_output = args.format.is_json();
    let machine_readable_output = json_output || args.progress == ProgressArg::Json;
    let daemon_suppression_reason = daemon_autostart_suppression_reason();
    let daemon_backgrounding_enabled = config.daemon.enabled
        && !args.no_daemon
        && (machine_readable_output || daemon_suppression_reason.is_none());
    let foreground_import = !args.catalog_only && (args.wait || !daemon_backgrounding_enabled);
    let inventory_store = setup_inventory_store(&db_path, args.catalog_only || !foreground_import)?;
    let config_path = data_root.join(CONFIG_FILE);
    config::write_default_config(&data_root)?;
    let sources = discovered_sources();
    let progress_arg = setup_progress_arg(args.progress, quiet);
    let progress = ProgressReporter::new(progress_arg, json_output, "setup", 0);
    let mut inventory_only = None;
    let import_report = if let Some(store) = inventory_store.as_ref() {
        progress.message("inventorying", "Preparing local history...");
        let inventory = inventory_available_sources(store, &sources)?;
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
        let import_args = ImportArgs {
            provider: None,
            path: None,
            history_source: None,
            history_source_manifest: Vec::new(),
            reset_cursor: false,
            input_format: None,
            all: true,
            resume: false,
            partial: false,
            no_daemon: args.no_daemon,
            format: crate::output::JsonOutputFormat::Text,
            progress: progress_arg,
        };
        provider_refreshes.start_timing();
        let report = run_import_internal(
            &import_args,
            data_root.clone(),
            &mut telemetry.import,
            provider_refreshes,
            ProviderRefreshTrigger::Setup,
            config,
            ImportRunOptions {
                progress: progress_arg,
                json: json_output,
                print_human: false,
                allow_empty_sources: true,
                include_history_source_plugins: false,
                operation: "setup",
            },
        );
        provider_refreshes.stop_timing();
        match report {
            Ok(report) => Some(report),
            Err(error) => {
                insert_import_error_analytics(&mut telemetry.import, &error);
                return Err(error);
            }
        }
    };
    if let Some(report) = import_report.as_ref() {
        insert_import_report_analytics(&mut telemetry.import, report);
    }
    let all_import_sources_failed = import_report
        .as_ref()
        .is_some_and(|report| import_report_analytics_outcome(&report.totals).0 == "failure");
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
    telemetry.providers_detected = Some(analytics::count_bucket(sources.len() as u64));
    telemetry.cataloged_sessions = Some(analytics::count_bucket(catalog.cataloged_sessions as u64));
    telemetry.inventory_sources = Some(analytics::count_bucket(inventory_totals.sources as u64));
    telemetry.inventory_source_files = Some(analytics::count_bucket(
        inventory_totals.source_files as u64,
    ));
    telemetry.pending_sessions = Some(analytics::count_bucket(catalog_counts.pending as u64));
    telemetry.catalog_source_bytes = Some(analytics::bytes_bucket(catalog.source_bytes));
    telemetry.inventory_source_bytes = Some(analytics::bytes_bucket(inventory_totals.source_bytes));
    let indexed_items = indexed_history_item_count(&setup_store)?;
    let _ =
        insert_store_analytics_counts(&mut telemetry.store, &setup_store, config.analytics.enabled);
    telemetry.has_indexed_content = Some(setup_has_indexed_content(indexed_items));
    let background_indexing_enabled = daemon_backgrounding_enabled
        && !args.catalog_only
        && !foreground_import
        && (pending_inventory_units > 0 || (semantic_enabled && semantic_supported));
    // Machine-readable setup must not create a background process, but it must
    // still notify and wait for a daemon that already owns this data root.
    // Otherwise a semantic config mutation can be reported while the live
    // runtime remains stale until an arbitrary scheduler tick.
    let machine_output_can_reuse_daemon = machine_readable_output
        && daemon_suppression_reason.is_none()
        && daemon_autostart_can_reuse_existing(&data_root);
    let daemon_autostart_requested = daemon_backgrounding_enabled
        && !args.catalog_only
        && (!machine_readable_output || machine_output_can_reuse_daemon);
    let daemon_autostart_reason = if args.catalog_only {
        Some("catalog_only")
    } else if args.no_daemon {
        Some("explicit_opt_out")
    } else if !config.daemon.enabled {
        Some("daemon_disabled")
    } else if machine_readable_output {
        Some("machine_readable_output")
    } else if daemon_suppression_reason.is_some() {
        daemon_suppression_reason
    } else {
        None
    };
    telemetry.mode = Some(if args.catalog_only {
        SetupMode::CatalogOnly
    } else if foreground_import || !background_indexing_enabled {
        SetupMode::Ready
    } else {
        SetupMode::Background
    });
    let daemon_handoff = if daemon_autostart_requested && !all_import_sources_failed {
        Some(autostart_daemon_and_wait(
            &data_root,
            config,
            crate::DaemonTriggerCommandArg::Setup,
        )?)
    } else {
        None
    };

    if json_output {
        print_json(json!({
            "schema_version": 1,
            "data_root": data_root,
            "database_path": db_path,
            "config_path": config_path,
            "mode": if args.catalog_only {
                "catalog_only"
            } else if foreground_import || !background_indexing_enabled {
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
            "import": setup_import_json(
                import_report.as_ref(),
                args.catalog_only,
                background_indexing_enabled
            ),
            "background_indexing": setup_background_indexing_json(
                &inventory_totals,
                inventory_units,
                background_indexing_enabled,
                semantic_enabled,
                semantic_supported,
                daemon_autostart_requested,
                daemon_autostart_reason,
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
            print_daemon_autostart_guidance(
                daemon_autostart_requested,
                daemon_autostart_reason,
                daemon_handoff.as_ref(),
            );
            println!("Get started:");
            if args.catalog_only {
                println!("  ctx import --all");
                println!("  ctx sources");
            } else if background_indexing_enabled {
                println!("  ctx index watch");
                println!("  ctx search \"test failure\"");
                println!("  ctx status");
            } else if !foreground_import && setup_has_indexed_content(indexed_items) {
                println!("  ctx search \"test failure\"");
                println!("  ctx status");
            } else if !foreground_import {
                println!("  ctx sources");
                println!("  ctx import --all");
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
    if all_import_sources_failed {
        bail!("all setup import sources failed");
    }
    Ok(())
}

fn setup_inventory_store(db_path: &Path, inventory_required: bool) -> Result<Option<Store>> {
    if inventory_required {
        Ok(Some(Store::open(db_path)?))
    } else {
        Ok(None)
    }
}

fn setup_progress_arg(progress: ProgressArg, quiet: bool) -> ProgressArg {
    if quiet && progress == ProgressArg::Auto {
        ProgressArg::None
    } else {
        progress
    }
}

pub(crate) fn setup_import_json(
    report: Option<&ImportReport>,
    catalog_only: bool,
    background_indexing_enabled: bool,
) -> Value {
    match report {
        Some(report) => json!({
            "ran": true,
            "outcome": import_report_analytics_outcome(&report.totals).0,
            "failure_scope": import_report_analytics_outcome(&report.totals).1,
            "failure_type": import_report_failure_type(&report.totals),
            "resume": report.resume,
            "resume_mode": report.resume_mode(),
            "totals": import_totals_json(&report.totals),
            "sources": report.sources.clone(),
        }),
        None => json!({
            "ran": false,
            "reason": if catalog_only {
                "catalog_only"
            } else if background_indexing_enabled {
                "background"
            } else {
                "no_sources"
            },
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
            println!("ctx is initialized; no local history was indexed");
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
    daemon_autostart_requested: bool,
    daemon_autostart_reason: Option<&str>,
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
        "daemon_autostart": setup_daemon_autostart_json(
            daemon_autostart_requested,
            daemon_autostart_reason,
        ),
        "status_command": "ctx index status",
        "watch_command": "ctx index watch",
        "wait_command": "ctx index wait --all",
    })
}

fn setup_daemon_autostart_json(requested: bool, reason: Option<&str>) -> Value {
    if !requested {
        return json!({
            "status": "not_needed",
            "reason": reason.unwrap_or("not_requested"),
            "status_command": "ctx daemon status",
        });
    }
    json!({
        "status": "deferred",
        "reason": null,
        "status_command": "ctx daemon status",
    })
}

fn print_daemon_autostart_guidance(
    requested: bool,
    reason: Option<&str>,
    handoff: Option<&DaemonHandoff>,
) {
    if let Some(handoff) = handoff {
        println!(
            "Daemon is running (PID {}); background maintenance handoff is verified.",
            handoff.pid
        );
        return;
    }
    if requested {
        println!("Daemon handoff was not verified; run `ctx daemon status`.");
        return;
    }
    match reason {
        Some("explicit_opt_out") => {
            println!("Daemon autostart was skipped for this setup because --no-daemon was used.");
        }
        Some("daemon_disabled") => {
            println!("Daemon autostart was skipped because daemon maintenance is disabled.");
        }
        Some("machine_readable_output") => {
            println!("Daemon autostart was skipped because machine-readable output was requested.");
        }
        Some("catalog_only") => {
            println!("Catalog-only setup does not autostart daemon maintenance.");
        }
        Some("ci" | "autostart_disabled" | "daemon_child") => {
            println!("Daemon autostart is disabled for this process; setup ran in the foreground.");
        }
        _ => {}
    }
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
        println!("Semantic search: enabled; model acquisition is queued for the daemon.");
        println!("The setup process does not download the embedding model.");
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

pub(crate) fn analytics_preflight<T>(
    enabled: bool,
    query: impl FnOnce() -> Result<T>,
) -> Option<T> {
    enabled.then(query)?.ok()
}

pub(crate) fn insert_store_analytics_counts(
    telemetry: &mut StoreTelemetry,
    store: &Store,
    enabled: bool,
) -> Option<usize> {
    let counts = analytics_preflight(enabled, || Ok(store.indexed_history_counts()?))?;
    telemetry.indexed_sessions = Some(analytics::count_bucket(counts.sessions as u64));
    telemetry.indexed_events = Some(analytics::count_bucket(counts.events as u64));
    telemetry.indexed_items = Some(analytics::count_bucket(counts.items() as u64));
    Some(counts.items())
}

pub(crate) fn insert_db_size_bucket(telemetry: &mut StoreTelemetry, db_path: &Path) {
    let bytes = fs::metadata(db_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    telemetry.db_size = Some(analytics::bytes_bucket(bytes));
}

pub(crate) fn setup_has_failed_sources(report: Option<&ImportReport>) -> bool {
    report.is_some_and(|report| report.totals.failed_sources > 0)
}

#[cfg(test)]
mod setup_estimate_tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn fresh_foreground_setup_enters_import_before_store_creation() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");

        let store = setup_inventory_store(&db_path, false).unwrap();
        assert!(store.is_none());
        assert!(
            !db_path.exists(),
            "fresh setup target must remain absent at foreground import entry"
        );

        Store::open(&db_path).unwrap();
        let store = setup_inventory_store(&db_path, false).unwrap();
        assert!(store.is_none());
        assert!(
            db_path.exists(),
            "foreground setup must preserve an existing Store"
        );
    }

    #[test]
    fn setup_inventory_modes_open_and_persist_store() {
        for mode in ["catalog_only", "background"] {
            let temp = tempfile::tempdir().unwrap();
            let db_path = temp.path().join("work.sqlite");

            let store = setup_inventory_store(&db_path, true).unwrap();
            assert!(store.is_some(), "{mode} must open the Store");
            assert!(db_path.exists(), "{mode} must persist the Store");
        }
    }

    #[test]
    fn analytics_preflight_is_disabled_without_running_the_query() {
        let called = Cell::new(false);
        let value = analytics_preflight(false, || {
            called.set(true);
            Ok::<_, anyhow::Error>(42)
        });
        assert_eq!(value, None);
        assert!(!called.get());
    }

    #[test]
    fn analytics_preflight_errors_become_unknown() {
        let value = analytics_preflight(true, || Err::<usize, _>(anyhow::anyhow!("preflight")));
        assert_eq!(value, None);
    }

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
