#[allow(unused_imports)]
use super::*;

pub(crate) fn tool_status(data_root: &Path) -> Result<Value> {
    let db_path = database_path(data_root.to_path_buf());
    let initialized = db_path.exists();
    let (
        indexed_items,
        indexed_sources,
        cataloged_sessions,
        indexed_catalog_sessions,
        pending_catalog_sessions,
        failed_catalog_sessions,
        stale_catalog_sessions,
    ) = if initialized {
        let store = Store::open_read_only(&db_path)
            .with_context(|| format!("open read-only ctx store {}", db_path.display()))?;
        let catalog_counts = store.catalog_session_counts()?;
        (
            indexed_history_item_count(&store)?,
            store.capture_source_count()?,
            catalog_counts.total,
            catalog_counts.indexed,
            catalog_counts.pending,
            catalog_counts.failed,
            catalog_counts.stale,
        )
    } else {
        (0, 0, 0, 0, 0, 0, 0)
    };

    Ok(json!({
        "schema_version": 1,
        "initialized": initialized,
        "data_root": data_root,
        "database_path": db_path,
        "config_path": data_root.join(CONFIG_FILE),
        "indexed_items": indexed_items,
        "indexed_sources": indexed_sources,
        "cataloged_sessions": cataloged_sessions,
        "indexed_catalog_sessions": indexed_catalog_sessions,
        "pending_catalog_sessions": pending_catalog_sessions,
        "failed_catalog_sessions": failed_catalog_sessions,
        "stale_catalog_sessions": stale_catalog_sessions,
        "local_only": true,
        "read_only": true,
    }))
}
