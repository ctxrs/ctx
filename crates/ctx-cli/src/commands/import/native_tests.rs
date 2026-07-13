use super::*;
use crate::provider_sources::explicit_path_source;
use ctx_history_core::{
    new_id, Event, EventRole, EventType, Fidelity, SyncMetadata, SyncState, Visibility,
};
use ctx_history_store::{SourceImportFile, SourceImportFileIndexUpdate};
use serde_json::json;

fn tempdir() -> tempfile::TempDir {
    let temp_root = fs::canonicalize(std::env::temp_dir())
        .expect("system temporary directory should be canonicalizable");
    tempfile::Builder::new()
        .prefix("ctx-native-import-")
        .tempdir_in(temp_root)
        .unwrap()
}

#[test]
fn codex_preinventory_failures_survive_when_catalog_has_no_pending_sessions() {
    let temp = tempdir();
    let source_path = temp.path().join("sessions");
    fs::create_dir_all(&source_path).unwrap();
    let source = explicit_path_source(CaptureProvider::Codex, source_path);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let source_root = source.path.display().to_string();
    let inventory_generation = store
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, &source_root)
        .unwrap();
    let catalog = CatalogSummary {
        failed_sessions: 1,
        failures: vec![ProviderImportFailure {
            line: 0,
            error: "catalog-only rejection".to_owned(),
        }],
        ..CatalogSummary::default()
    };

    let summary = import_incremental_codex_session_tree(
        &mut store,
        &source,
        new_id(),
        None,
        Some(&catalog),
        Some(inventory_generation),
        false,
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert_eq!(summary.failures, catalog.failures);
}

fn persist_indexed_root(
    store: &Store,
    source: &SourceInfo,
    file_size_bytes: u64,
    file_modified_at_ms: i64,
) -> (SourceImportFile, u64) {
    let source_root = source.path.display().to_string();
    let file = SourceImportFile {
        provider: source.provider,
        source_format: source.source_format.to_owned(),
        import_revision: source.import_revision,
        source_root: source_root.clone(),
        source_path: source_root.clone(),
        file_size_bytes,
        file_modified_at_ms,
        observed_at_ms: 0,
        metadata: json!({}),
    };
    let inventory_generation = inventory_source_file(store, &file);
    store
        .mark_source_import_file_indexed(
            source.provider,
            SourceImportFileIndexUpdate {
                source_root: &source_root,
                source_path: &source_root,
                file_size_bytes,
                file_modified_at_ms,
                import_revision: source.import_revision,
                inventory_generation,
                metadata: &file.metadata,
                indexed_at_ms: 1,
            },
        )
        .unwrap();
    (file, inventory_generation)
}

fn inventory_source_file(store: &Store, file: &SourceImportFile) -> u64 {
    let inventory_generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(inventory_generation, std::slice::from_ref(file))
        .unwrap();
    inventory_generation
}

#[test]
fn unchanged_root_source_skips_provider_normalization() {
    let temp = tempdir();
    let source_path = temp.path().join("state.db");
    let source = explicit_path_source(CaptureProvider::Hermes, source_path.clone());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let (file, inventory_generation) = persist_indexed_root(&store, &source, 0, 0);

    let summary = import_one_source_for_search_refresh(
        &mut store,
        &source,
        None,
        &SourcePreinventory::SourceRoot {
            file,
            inventory_generation,
        },
    )
    .unwrap();

    assert_eq!(summary.imported_events, 0);
    assert_eq!(summary.failed, 0);
}

#[test]
fn unchanged_root_source_still_repairs_event_search_backfill() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let source_path = temp.path().join("state.db");
    let source = explicit_path_source(CaptureProvider::Hermes, source_path);
    let store = Store::open(&db_path).unwrap();
    let (file, inventory_generation) = persist_indexed_root(&store, &source, 0, 0);
    let event = Event {
        id: new_id(),
        seq: 1,
        history_record_id: None,
        session_id: None,
        run_id: None,
        event_type: EventType::Message,
        role: Some(EventRole::User),
        occurred_at: utc_now(),
        capture_source_id: None,
        payload: json!({"text": "unchanged root backfill oracle"}),
        payload_blob_id: None,
        dedupe_key: None,
        sync: SyncMetadata {
            visibility: Visibility::LocalOnly,
            fidelity: Fidelity::Imported,
            sync_state: SyncState::LocalOnly,
            sync_version: 0,
            deleted_at: None,
            metadata: json!({}),
        },
    };
    store.upsert_event(&event).unwrap();
    drop(store);
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute("DELETE FROM event_search", []).unwrap();
    drop(conn);
    let mut store = Store::open(&db_path).unwrap();
    assert!(store.event_search_projection_needs_backfill().unwrap());

    import_one_source_for_search_refresh(
        &mut store,
        &source,
        None,
        &SourcePreinventory::SourceRoot {
            file,
            inventory_generation,
        },
    )
    .unwrap();

    assert!(!store.event_search_projection_needs_backfill().unwrap());
    assert_eq!(
        store
            .search_event_hits("unchanged root backfill oracle", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn changed_root_source_does_not_skip_provider_normalization() {
    let temp = tempdir();
    let source_path = temp.path().join("state.db");
    let source = explicit_path_source(CaptureProvider::Hermes, source_path.clone());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    persist_indexed_root(&store, &source, 0, 0);
    std::fs::write(&source_path, b"not a sqlite database").unwrap();
    let changed = SourceImportFile {
        provider: source.provider,
        source_format: source.source_format.to_owned(),
        import_revision: source.import_revision,
        source_root: source_path.display().to_string(),
        source_path: source_path.display().to_string(),
        file_size_bytes: 21,
        file_modified_at_ms: 1,
        observed_at_ms: 1,
        metadata: json!({}),
    };
    let inventory_generation = inventory_source_file(&store, &changed);

    let result = import_one_source_for_search_refresh(
        &mut store,
        &source,
        None,
        &SourcePreinventory::SourceRoot {
            file: changed,
            inventory_generation,
        },
    );

    assert!(
        result.is_err(),
        "changed source must reach the Hermes adapter"
    );
}

#[test]
fn full_rescan_does_not_skip_unchanged_root_source() {
    let temp = tempdir();
    let source_path = temp.path().join("state.db");
    std::fs::write(&source_path, b"not a sqlite database").unwrap();
    let source = explicit_path_source(CaptureProvider::Hermes, source_path);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let (file, inventory_generation) = persist_indexed_root(&store, &source, 21, 1);

    let result = import_one_source_inner(
        &mut store,
        &source,
        None,
        false,
        true,
        &SourcePreinventory::SourceRoot {
            file,
            inventory_generation,
        },
    );

    assert!(result.is_err(), "full rescan must reach the Hermes adapter");
}

#[test]
fn pre_summary_source_error_is_terminal_for_the_observed_revision() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("transcript.jsonl");
    let source = explicit_path_source(CaptureProvider::Hermes, source_path.clone());
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = SourceImportFile {
        provider: source.provider,
        source_format: source.source_format.to_owned(),
        import_revision: source.import_revision,
        source_root: source_path.display().to_string(),
        source_path: source_path.display().to_string(),
        file_size_bytes: 17,
        file_modified_at_ms: 23,
        observed_at_ms: 29,
        metadata: json!({}),
    };
    let inventory_generation = inventory_source_file(&store, &file);
    let error = anyhow::Error::new(CaptureError::InvalidProviderTranscriptPath {
        path: source_path,
        reason: "expected a provider transcript file",
    });

    assert!(rejected_source_summary(&error).is_none());
    let status = import_error_status(&error);
    assert_eq!(status, CatalogIndexedStatus::Rejected);
    mark_source_import_file_result(
        &store,
        &file,
        inventory_generation,
        status,
        Some(&error.to_string()),
    )
    .unwrap();

    assert!(store
        .list_pending_source_import_files(source.provider, &file.source_root)
        .unwrap()
        .is_empty());
    assert_eq!(store.source_import_file_counts().unwrap().rejected, 1);
}

#[test]
fn transient_source_io_remains_retryable_for_the_observed_revision() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("transcript.jsonl");
    let source = explicit_path_source(CaptureProvider::Hermes, source_path.clone());
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = SourceImportFile {
        provider: source.provider,
        source_format: source.source_format.to_owned(),
        import_revision: source.import_revision,
        source_root: source_path.display().to_string(),
        source_path: source_path.display().to_string(),
        file_size_bytes: 17,
        file_modified_at_ms: 23,
        observed_at_ms: 29,
        metadata: json!({}),
    };
    let inventory_generation = inventory_source_file(&store, &file);
    let error = anyhow::Error::new(CaptureError::Io(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "transient test failure",
    )));

    assert_eq!(import_error_scope(&error), ImportFailureScope::Source);
    let status = import_error_status(&error);
    assert_eq!(status, CatalogIndexedStatus::Failed);
    mark_source_import_file_result(
        &store,
        &file,
        inventory_generation,
        status,
        Some(&error.to_string()),
    )
    .unwrap();

    assert_eq!(
        store
            .list_pending_source_import_files(source.provider, &file.source_root)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(store.source_import_file_counts().unwrap().failed, 1);
}

#[test]
fn provider_sqlite_lock_is_pending_until_the_lock_is_released() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("provider.sqlite");
    let source = explicit_path_source(CaptureProvider::Hermes, source_path.clone());
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = SourceImportFile {
        provider: source.provider,
        source_format: source.source_format.to_owned(),
        import_revision: source.import_revision,
        source_root: source_path.display().to_string(),
        source_path: source_path.display().to_string(),
        file_size_bytes: 17,
        file_modified_at_ms: 23,
        observed_at_ms: 29,
        metadata: json!({}),
    };
    let inventory_generation = inventory_source_file(&store, &file);

    let lock = rusqlite::Connection::open(&source_path).unwrap();
    lock.execute_batch(
        "PRAGMA journal_mode = DELETE;
         CREATE TABLE state(value INTEGER NOT NULL);
         INSERT INTO state VALUES (1);
         BEGIN EXCLUSIVE;
         UPDATE state SET value = 2;",
    )
    .unwrap();
    let reader = rusqlite::Connection::open(&source_path).unwrap();
    reader.busy_timeout(std::time::Duration::ZERO).unwrap();
    let sqlite_error = reader
        .query_row("SELECT value FROM state", [], |row| row.get::<_, i64>(0))
        .unwrap_err();
    let error = anyhow::Error::new(CaptureError::Sqlite(sqlite_error));
    assert_eq!(import_error_scope(&error), ImportFailureScope::Source);
    let status = import_error_status(&error);
    assert_eq!(status, CatalogIndexedStatus::Failed);
    mark_source_import_file_result(
        &store,
        &file,
        inventory_generation,
        status,
        Some(&error.to_string()),
    )
    .unwrap();
    assert_eq!(
        store
            .list_pending_source_import_files(source.provider, &file.source_root)
            .unwrap()
            .len(),
        1
    );

    lock.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        reader
            .query_row("SELECT value FROM state", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    mark_source_import_file_result(
        &store,
        &file,
        inventory_generation,
        CatalogIndexedStatus::Indexed,
        None,
    )
    .unwrap();
    assert!(store
        .list_pending_source_import_files(source.provider, &file.source_root)
        .unwrap()
        .is_empty());
}
