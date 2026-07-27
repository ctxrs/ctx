use std::path::PathBuf;

use anyhow::Result;
use serde_json::json;

use ctx_history_core::database_path;

use crate::analytics::{count_bucket, StatusTelemetry};
use crate::config::{self, CONFIG_FILE};
use crate::output::print_json;
use crate::semantic::{
    daemon_report, semantic_worker_report_cached, semantic_worker_report_configured_json,
};
use crate::store_util::open_existing_store_read_only;
use crate::JsonArgs;

pub(crate) fn run_status(
    args: JsonArgs,
    data_root: PathBuf,
    quiet: bool,
    telemetry: &mut StatusTelemetry,
) -> Result<()> {
    let db_path = database_path(data_root.clone());
    let initialized = db_path.exists();
    let config_path = data_root.join(CONFIG_FILE);
    let config = config::AppConfig::load(&data_root)?;
    let (
        records,
        sessions,
        events,
        sources,
        catalog_counts,
        source_import_file_counts,
        semantic,
        daemon,
    ) = if initialized {
        let store = open_existing_store_read_only(&db_path, "ctx status")?;
        let counts = store.indexed_history_counts()?;
        let semantic_report = semantic_worker_report_cached(&data_root, Some(&store))?;
        let daemon = daemon_report(&data_root, &semantic_report);
        let catalog_counts = store.catalog_session_counts()?;
        (
            counts.items(),
            counts.sessions,
            counts.events,
            store.capture_source_count()?,
            catalog_counts,
            store.source_import_file_counts()?,
            semantic_worker_report_configured_json(&config, &semantic_report),
            daemon,
        )
    } else {
        let semantic_report = semantic_worker_report_cached(&data_root, None)?;
        let daemon = daemon_report(&data_root, &semantic_report);
        (
            0,
            0,
            0,
            0,
            Default::default(),
            Default::default(),
            semantic_worker_report_configured_json(&config, &semantic_report),
            daemon,
        )
    };
    let inventory_units = catalog_counts
        .total
        .saturating_add(source_import_file_counts.total);
    let pending_inventory_units = catalog_counts
        .pending
        .saturating_add(source_import_file_counts.pending);
    let failed_inventory_units = catalog_counts
        .failed
        .saturating_add(source_import_file_counts.failed);
    let stale_inventory_units = catalog_counts
        .stale
        .saturating_add(source_import_file_counts.stale);
    telemetry.initialized = Some(initialized);
    telemetry.indexed_items = Some(count_bucket(records as u64));
    telemetry.indexed_sessions = Some(count_bucket(sessions as u64));
    telemetry.indexed_events = Some(count_bucket(events as u64));
    telemetry.indexed_sources = Some(count_bucket(sources as u64));
    telemetry.inventory_units = Some(count_bucket(inventory_units as u64));
    telemetry.pending_inventory_units = Some(count_bucket(pending_inventory_units as u64));
    telemetry.failed_inventory_units = Some(count_bucket(failed_inventory_units as u64));
    telemetry.stale_inventory_units = Some(count_bucket(stale_inventory_units as u64));
    let pro = crate::pro::lifecycle_status_json(&data_root);

    if args.json {
        print_json(json!({
            "schema_version": 1,
            "initialized": initialized,
            "data_root": data_root,
            "database_path": db_path,
            "config_path": config_path,
            "indexed_items": records,
            "indexed_sessions": sessions,
            "indexed_events": events,
            "indexed_sources": sources,
            "inventory_units": inventory_units,
            "pending_inventory_units": pending_inventory_units,
            "failed_inventory_units": failed_inventory_units,
            "stale_inventory_units": stale_inventory_units,
            "cataloged_sessions": catalog_counts.total,
            "indexed_catalog_sessions": catalog_counts.indexed,
            "pending_catalog_sessions": catalog_counts.pending,
            "failed_catalog_sessions": catalog_counts.failed,
            "stale_catalog_sessions": catalog_counts.stale,
            "source_import_files": source_import_file_counts.total,
            "indexed_source_import_files": source_import_file_counts.indexed,
            "pending_source_import_files": source_import_file_counts.pending,
            "failed_source_import_files": source_import_file_counts.failed,
            "stale_source_import_files": source_import_file_counts.stale,
            "semantic": semantic,
            "daemon": daemon,
            "pro": pro,
            "local_only": true,
            "read_only": true,
        }))?;
    } else if !quiet {
        println!("data_root: {}", data_root.display());
        println!("database_path: {}", db_path.display());
        println!("config_path: {}", config_path.display());
        println!("initialized: {initialized}");
        println!("indexed_items: {records}");
        println!("indexed_sources: {sources}");
        println!("inventory_units: {inventory_units}");
        println!("pending_inventory_units: {pending_inventory_units}");
        println!("failed_inventory_units: {failed_inventory_units}");
        println!("stale_inventory_units: {stale_inventory_units}");
        println!("cataloged_sessions: {}", catalog_counts.total);
        println!("indexed_catalog_sessions: {}", catalog_counts.indexed);
        println!("pending_catalog_sessions: {}", catalog_counts.pending);
        println!("failed_catalog_sessions: {}", catalog_counts.failed);
        println!("stale_catalog_sessions: {}", catalog_counts.stale);
        println!("source_import_files: {}", source_import_file_counts.total);
        println!(
            "indexed_source_import_files: {}",
            source_import_file_counts.indexed
        );
        println!(
            "pending_source_import_files: {}",
            source_import_file_counts.pending
        );
        println!(
            "failed_source_import_files: {}",
            source_import_file_counts.failed
        );
        println!(
            "stale_source_import_files: {}",
            source_import_file_counts.stale
        );
        println!(
            "semantic_status: {}",
            semantic
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown")
        );
        println!(
            "semantic_embedded_items: {}",
            semantic
                .get("coverage")
                .and_then(|coverage| coverage.get("embedded_items"))
                .and_then(|value| value.as_u64())
                .unwrap_or(0)
        );
        println!(
            "daemon_enabled: {}",
            daemon
                .get("enabled")
                .and_then(|value| value.as_bool())
                .unwrap_or(true)
        );
        println!(
            "daemon_status: {}",
            daemon
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown")
        );
        if let Some(reason) = daemon.get("reason").and_then(|value| value.as_str()) {
            println!("daemon_reason: {reason}");
        }
        if daemon
            .get("recoverable")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        {
            println!("daemon_recoverable: true");
        }
        if pro["installed"].as_bool() == Some(true) {
            println!(
                "pro_status: {}",
                pro["state"].as_str().unwrap_or("unavailable")
            );
            if let Some(access_state) = pro["access_state"].as_str() {
                println!("pro_access_state: {access_state}");
            }
            for field in [
                "refresh_after_unix",
                "access_deadline_unix",
                "grace_deadline_unix",
            ] {
                if let Some(deadline) = pro[field].as_i64() {
                    println!("pro_{field}: {deadline}");
                }
            }
        }
        println!("local_only: true");
        println!("read_only: true");
    }
    Ok(())
}
