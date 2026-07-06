#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_zed_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("zed/v1/threads.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Zed, fixture.clone());
    assert_eq!(source.source_format, ZED_THREADS_SQLITE_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_zed_threads_sqlite(
        &fixture,
        &mut store,
        ZedThreadsSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T12:10:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..ZedThreadsSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 5);
    assert_eq!(first.imported_edges, 1);

    let parent_id = provider_session_uuid(CaptureProvider::Zed, "zed-root");
    let child_id = provider_session_uuid(CaptureProvider::Zed, "zed-child");
    assert_eq!(
        store.get_session(child_id).unwrap().parent_session_id,
        Some(parent_id)
    );
    let parent_events = store.events_for_session(parent_id).unwrap();
    assert_eq!(parent_events.len(), 3);
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::Summary));
    let rendered = serde_json::to_string(&parent_events).unwrap();
    assert!(rendered.contains("zed sqlite oracle prompt"));
    assert!(rendered.contains("zed sqlite oracle answer"));
    assert!(rendered.contains("zed compacted summary oracle"));
    assert!(store
        .export_archive()
        .unwrap()
        .files_touched
        .iter()
        .any(|file| file.path == "src/zed_oracle.txt"));
    assert!(store
        .search_event_hits("zed sqlite oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Zed)));

    let source = store
        .capture_source_by_external_session(CaptureProvider::Zed, "zed-root")
        .unwrap()
        .unwrap();
    assert_eq!(
        source.sync.metadata["source_metadata"]["upstream_schema_anchor"]["commit"].as_str(),
        Some("e3b73c6b30cdc09e820823fe44542b89850d4be1")
    );

    let second = import_zed_threads_sqlite(
        &fixture,
        &mut store,
        ZedThreadsSqliteImportOptions {
            allow_partial_failures: true,
            ..ZedThreadsSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.imported_edges, 0);
    assert_eq!(second.skipped_sessions, 2);
    assert_eq!(second.skipped_events, 5);
    assert_eq!(second.skipped_edges, 1);
}

#[test]
pub(crate) fn native_zed_reports_malformed_and_corrupt_db() {
    let temp = tempdir();
    let malformed = temp.path().join("zed-malformed.db");
    {
        let conn = rusqlite::Connection::open(&malformed).unwrap();
        conn.execute_batch(
            "CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                summary TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                data_type TEXT NOT NULL
            );",
        )
        .unwrap();
    }
    let corrupt = temp.path().join("zed-corrupt.db");
    fs::write(&corrupt, b"not sqlite").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err = import_zed_threads_sqlite(
        &malformed,
        &mut store,
        ZedThreadsSqliteImportOptions::default(),
    )
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("Zed threads table missing required column(s): data"));

    let err = import_zed_threads_sqlite(
        &corrupt,
        &mut store,
        ZedThreadsSqliteImportOptions::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("not a database"));
}

#[test]
pub(crate) fn provider_sources_discovers_zed_default_db() {
    let temp = tempdir();
    let db = temp.path().join(".local/share/zed/threads/threads.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    fs::write(&db, b"not inspected by source probe").unwrap();

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Zed);
    let source = sources
        .iter()
        .find(|source| source.source_format == ZED_THREADS_SQLITE_SOURCE_FORMAT)
        .unwrap_or_else(|| panic!("missing Zed source in {sources:#?}"));
    assert_eq!(source.provider, CaptureProvider::Zed);
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert_eq!(source.path, db);
}
