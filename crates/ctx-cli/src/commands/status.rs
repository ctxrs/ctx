use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::Result;
use serde_json::json;

use ctx_history_core::database_path;
use ctx_history_store::{IndexingAdmission, IndexingResourceSnapshot};

use crate::config::{self, CONFIG_FILE};
use crate::output::print_json;
use crate::semantic::{
    daemon_report, semantic_worker_report_cached, semantic_worker_report_configured_json,
};
use crate::store_util::open_existing_store_read_only;
use crate::JsonArgs;

pub(crate) fn run_status(args: JsonArgs, data_root: PathBuf, quiet: bool) -> Result<()> {
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
        fts_maintenance_pending,
        semantic,
        daemon,
    ) = if initialized {
        let store = open_existing_store_read_only(&db_path, "ctx status")?;
        let counts = store.indexed_history_counts()?;
        let semantic_report = semantic_worker_report_cached(&data_root, Some(&store))?;
        let daemon = daemon_report(&data_root, &semantic_report);
        (
            counts.items(),
            counts.sessions,
            counts.events,
            store.capture_source_count()?,
            store.catalog_session_counts()?,
            store.source_import_file_counts()?,
            store.event_search_maintenance_pending()?,
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
            false,
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
    let admission = IndexingAdmission::status(&db_path).ok();
    let resource_snapshot = IndexingResourceSnapshot::current(
        &db_path,
        initialized.then(|| wal_size_bytes(&db_path)).flatten(),
    );
    let indexing = json!({
        "writer_active": admission.map(|status| status.writer_active),
        "foreground_pending": admission.map(|status| status.foreground_pending),
        "pressure": resource_snapshot.pressure().as_str(),
        "wal_band": resource_snapshot.wal_band(),
        "fts_maintenance_pending": fts_maintenance_pending,
    });

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
            "indexing": indexing,
            "semantic": semantic,
            "daemon": daemon,
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
            "indexing_writer_active: {}",
            admission
                .map(|status| status.writer_active.to_string())
                .unwrap_or_else(|| "unknown".to_owned())
        );
        println!(
            "indexing_foreground_pending: {}",
            admission
                .map(|status| status.foreground_pending.to_string())
                .unwrap_or_else(|| "unknown".to_owned())
        );
        println!(
            "indexing_pressure: {}",
            resource_snapshot.pressure().as_str()
        );
        println!("indexing_wal_band: {}", resource_snapshot.wal_band());
        println!("fts_maintenance_pending: {fts_maintenance_pending}");
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
        println!("local_only: true");
        println!("read_only: true");
    }
    Ok(())
}

fn wal_size_bytes(db_path: &Path) -> Option<u64> {
    let mut path = OsString::from(db_path.as_os_str());
    path.push("-wal");
    match fs::metadata(PathBuf::from(path)) {
        Ok(metadata) => Some(metadata.len()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Some(0),
        Err(_) => None,
    }
}
