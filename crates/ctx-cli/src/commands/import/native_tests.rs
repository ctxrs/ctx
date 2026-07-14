use super::*;
use crate::provider_sources::explicit_path_source;
use ctx_history_core::{
    new_id, Event, EventRole, EventType, Fidelity, SyncMetadata, SyncState, Visibility,
};
use ctx_history_store::{SourceImportFile, SourceImportFileIndexUpdate};
use serde_json::json;

#[test]
fn codex_preinventory_failures_survive_when_catalog_has_no_pending_sessions() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("sessions");
    fs::create_dir_all(&source_path).unwrap();
    let source = explicit_path_source(CaptureProvider::Codex, source_path);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let catalog = CatalogSummary {
        failed_sessions: 1,
        failures: vec![ProviderImportFailure {
            line: 0,
            error: "catalog-only rejection".to_owned(),
        }],
        ..CatalogSummary::default()
    };

    let summary =
        import_incremental_codex_session_tree(&mut store, &source, new_id(), None, Some(&catalog))
            .unwrap();

    assert_eq!(summary.failed, 1);
    assert_eq!(summary.failures, catalog.failures);
}

fn persist_indexed_root(
    store: &Store,
    source: &SourceInfo,
    file_size_bytes: u64,
    file_modified_at_ms: i64,
) -> SourceImportFile {
    let source_root = source.path.display().to_string();
    let file = SourceImportFile {
        provider: source.provider,
        source_format: source.source_format.to_owned(),
        source_root: source_root.clone(),
        source_path: source_root.clone(),
        file_size_bytes,
        file_modified_at_ms,
        observed_at_ms: 0,
        metadata: json!({}),
    };
    store
        .upsert_source_import_files(std::slice::from_ref(&file))
        .unwrap();
    store
        .mark_source_import_file_indexed(
            source.provider,
            SourceImportFileIndexUpdate {
                source_root: &source_root,
                source_path: &source_root,
                file_size_bytes,
                file_modified_at_ms,
                indexed_at_ms: 1,
            },
        )
        .unwrap();
    file
}

fn persist_indexed_manifest_file(store: &Store, source: &SourceInfo) -> SourceImportFile {
    let source_root = source.path.display().to_string();
    let source_path = source.path.join("session.jsonl").display().to_string();
    let file = SourceImportFile {
        provider: source.provider,
        source_format: source.source_format.to_owned(),
        source_root: source_root.clone(),
        source_path: source_path.clone(),
        file_size_bytes: 0,
        file_modified_at_ms: 0,
        observed_at_ms: 0,
        metadata: json!({}),
    };
    store
        .upsert_source_import_files(std::slice::from_ref(&file))
        .unwrap();
    store
        .mark_source_import_file_indexed(
            source.provider,
            SourceImportFileIndexUpdate {
                source_root: &source_root,
                source_path: &source_path,
                file_size_bytes: 0,
                file_modified_at_ms: 0,
                indexed_at_ms: 1,
            },
        )
        .unwrap();
    file
}

fn persist_fixed_root_record(store: &Store, source: &SourceInfo) -> HistoryRecord {
    let fixed = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let mut record = import_record_for_source(source);
    record.created_at = fixed;
    record.updated_at = fixed;
    store.upsert_record(&record).unwrap();
    record
}

fn data_version(conn: &rusqlite::Connection) -> i64 {
    conn.query_row("PRAGMA data_version", [], |row| row.get(0))
        .unwrap()
}

fn assert_batched_noop_preserves_root_record(
    store: &mut Store,
    db_path: &Path,
    source: &SourceInfo,
    preinventory: &SourcePreinventory,
) {
    let before_record = persist_fixed_root_record(store, source);
    let observer = rusqlite::Connection::open(db_path).unwrap();
    let before_data_version = data_version(&observer);
    let provider_work_required =
        source_preinventory_requires_provider_work(store, source, preinventory).unwrap();
    assert!(!provider_work_required);

    let summary =
        import_one_source_inner_batched(store, source, None, false, preinventory).unwrap();

    assert_eq!(summary, ProviderImportSummary::default());
    assert_eq!(store.get_record(before_record.id).unwrap(), before_record);
    assert_eq!(data_version(&observer), before_data_version);
}

#[test]
fn unchanged_codex_catalog_skips_root_record_and_projection_writes() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("work.sqlite");
    let source_path = temp.path().join("sessions");
    fs::create_dir_all(&source_path).unwrap();
    let source = explicit_path_source(CaptureProvider::Codex, source_path);
    let mut store = Store::open(&db_path).unwrap();
    let preinventory = SourcePreinventory::CodexSessionCatalog(CatalogSummary::default());

    assert_batched_noop_preserves_root_record(&mut store, &db_path, &source, &preinventory);
}

#[test]
fn unchanged_manifest_skips_root_record_and_projection_writes() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("work.sqlite");
    let source_path = temp.path().join("projects");
    fs::create_dir_all(&source_path).unwrap();
    let source = explicit_path_source(CaptureProvider::Claude, source_path);
    assert!(source_uses_import_file_manifest(&source));
    let mut store = Store::open(&db_path).unwrap();
    let file = persist_indexed_manifest_file(&store, &source);
    let preinventory = SourcePreinventory::SourceImportFiles(vec![file]);

    assert_batched_noop_preserves_root_record(&mut store, &db_path, &source, &preinventory);
}

#[test]
fn missing_root_record_is_repaired_without_reimporting_unchanged_source() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("state.db");
    let source = explicit_path_source(CaptureProvider::Hermes, source_path);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = persist_indexed_root(&store, &source, 0, 0);
    let record_id = import_record_for_source(&source).id;

    let summary = import_one_source_inner(
        &mut store,
        &source,
        None,
        false,
        false,
        &SourcePreinventory::SourceRoot(file),
    )
    .unwrap();

    assert_eq!(summary, ProviderImportSummary::default());
    assert_eq!(store.get_record(record_id).unwrap().id, record_id);
}

#[test]
fn unchanged_root_source_skips_provider_normalization() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("state.db");
    let source = explicit_path_source(CaptureProvider::Hermes, source_path.clone());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = persist_indexed_root(&store, &source, 0, 0);

    let summary = import_one_source_for_search_refresh(
        &mut store,
        &source,
        None,
        &SourcePreinventory::SourceRoot(file),
    )
    .unwrap();

    assert_eq!(summary.imported_events, 0);
    assert_eq!(summary.failed, 0);
}

#[test]
fn unchanged_root_source_still_repairs_event_search_backfill() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("work.sqlite");
    let source_path = temp.path().join("state.db");
    let source = explicit_path_source(CaptureProvider::Hermes, source_path);
    let store = Store::open(&db_path).unwrap();
    let file = persist_indexed_root(&store, &source, 0, 0);
    let root_record = persist_fixed_root_record(&store, &source);
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
        &SourcePreinventory::SourceRoot(file),
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
    assert_eq!(store.get_record(root_record.id).unwrap(), root_record);
}

#[test]
fn changed_root_source_does_not_skip_provider_normalization() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("state.db");
    let source = explicit_path_source(CaptureProvider::Hermes, source_path.clone());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    persist_indexed_root(&store, &source, 0, 0);
    std::fs::write(&source_path, b"not a sqlite database").unwrap();
    let changed = SourceImportFile {
        provider: source.provider,
        source_format: source.source_format.to_owned(),
        source_root: source_path.display().to_string(),
        source_path: source_path.display().to_string(),
        file_size_bytes: 21,
        file_modified_at_ms: 1,
        observed_at_ms: 1,
        metadata: json!({}),
    };
    store
        .upsert_source_import_files(std::slice::from_ref(&changed))
        .unwrap();

    let result = import_one_source_for_search_refresh(
        &mut store,
        &source,
        None,
        &SourcePreinventory::SourceRoot(changed),
    );

    assert!(
        result.is_err(),
        "changed source must reach the Hermes adapter"
    );
}

#[test]
fn full_rescan_does_not_skip_unchanged_root_source() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("state.db");
    std::fs::write(&source_path, b"not a sqlite database").unwrap();
    let source = explicit_path_source(CaptureProvider::Hermes, source_path);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = persist_indexed_root(&store, &source, 21, 1);

    let result = import_one_source_inner(
        &mut store,
        &source,
        None,
        false,
        true,
        &SourcePreinventory::SourceRoot(file),
    );

    assert!(result.is_err(), "full rescan must reach the Hermes adapter");
}
