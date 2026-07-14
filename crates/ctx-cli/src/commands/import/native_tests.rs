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

#[test]
fn unchanged_root_source_skips_provider_normalization() {
    let temp = tempdir();
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
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let source_path = temp.path().join("state.db");
    let source = explicit_path_source(CaptureProvider::Hermes, source_path);
    let store = Store::open(&db_path).unwrap();
    let file = persist_indexed_root(&store, &source, 0, 0);
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
    let temp = tempdir();
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
