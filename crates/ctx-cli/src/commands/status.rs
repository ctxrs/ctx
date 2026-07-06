use std::path::PathBuf;

use anyhow::Result;
use serde_json::json;

use ctx_history_core::database_path;

use crate::config::CONFIG_FILE;
use crate::output::print_json;
use crate::store_util::open_existing_store_snapshot_read_only;
use crate::JsonArgs;

pub(crate) fn run_status(args: JsonArgs, data_root: PathBuf) -> Result<()> {
    let db_path = database_path(data_root.clone());
    let initialized = db_path.exists();
    let config_path = data_root.join(CONFIG_FILE);
    let (records, sessions, events, sources, catalog_counts) = if initialized {
        let store = open_existing_store_snapshot_read_only(&db_path, "status")?;
        let counts = store.indexed_history_counts()?;
        (
            counts.items(),
            counts.sessions,
            counts.events,
            store.capture_source_count()?,
            store.catalog_session_counts()?,
        )
    } else {
        (0, 0, 0, 0, Default::default())
    };

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
            "cataloged_sessions": catalog_counts.total,
            "indexed_catalog_sessions": catalog_counts.indexed,
            "pending_catalog_sessions": catalog_counts.pending,
            "failed_catalog_sessions": catalog_counts.failed,
            "stale_catalog_sessions": catalog_counts.stale,
            "local_only": true,
            "read_only": true,
        }))?;
    } else {
        println!("data_root: {}", data_root.display());
        println!("database_path: {}", db_path.display());
        println!("config_path: {}", config_path.display());
        println!("initialized: {initialized}");
        println!("indexed_items: {records}");
        println!("indexed_sources: {sources}");
        println!("cataloged_sessions: {}", catalog_counts.total);
        println!("indexed_catalog_sessions: {}", catalog_counts.indexed);
        println!("pending_catalog_sessions: {}", catalog_counts.pending);
        println!("failed_catalog_sessions: {}", catalog_counts.failed);
        println!("stale_catalog_sessions: {}", catalog_counts.stale);
        println!("local_only: true");
        println!("read_only: true");
    }
    Ok(())
}
