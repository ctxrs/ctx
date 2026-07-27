use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde_json::json;

use super::*;
use crate::test_support_paths::tempdir;

fn observed_at() -> DateTime<Utc> {
    "2026-07-18T12:00:00Z".parse().unwrap()
}

fn context(path: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "zed-batch-test-machine".to_owned(),
        source_path: Some(path.to_path_buf()),
        source_root: None,
        imported_at: observed_at(),
    }
}

fn create_threads_schema(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE threads(\
            id TEXT PRIMARY KEY, parent_id TEXT, folder_paths TEXT, folder_paths_order TEXT,\
            summary TEXT NOT NULL, updated_at TEXT NOT NULL, data_type TEXT NOT NULL,\
            data BLOB NOT NULL, created_at TEXT\
         );",
    )
    .unwrap();
}

#[test]
fn zed_import_crosses_batch_boundary_and_replays_from_cursor() {
    let temp = tempdir().unwrap();
    let source_path = temp.path().join("threads.db");
    let conn = Connection::open(&source_path).unwrap();
    create_threads_schema(&conn);
    let tx = conn.unchecked_transaction().unwrap();
    for index in 0..65 {
        let id = format!("thread-{index:03}");
        let data = serde_json::to_vec(&json!({
            "title": id,
            "messages": [],
            "updated_at": "2026-07-18T12:00:00Z"
        }))
        .unwrap();
        tx.execute(
            "INSERT INTO threads(id, summary, updated_at, data_type, data, created_at) \
             VALUES (?1, ?2, '2026-07-18T12:00:00Z', 'json', ?3, '2026-07-18T11:00:00Z')",
            params![id, format!("summary {index}"), data],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    drop(conn);

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let options = NormalizedProviderImportOptions {
        history_record_id: None,
        persist_cursors: true,
        wrap_transaction: true,
        fast_event_inserts: true,
        capture_work_limit: crate::CaptureWorkLimit::Drain,
        inventory_observation_token: None,
    };
    let first = import_zed_threads_sqlite_batched(
        &source_path,
        &mut store,
        context(&source_path),
        options.clone(),
    )
    .unwrap();
    assert_eq!(first.imported_sessions, 65);
    assert_eq!(store.list_sessions().unwrap().len(), 65);

    let replay =
        import_zed_threads_sqlite_batched(&source_path, &mut store, context(&source_path), options)
            .unwrap();
    assert_eq!(replay.imported_sessions, 0);
    assert_eq!(store.list_sessions().unwrap().len(), 65);
}
