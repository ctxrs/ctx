#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_forgecode_fixture_imports_searches_reimports_and_file_metrics() {
    let temp = tempdir();
    let fixture = provider_history_fixture("forgecode/v1/forge.db");
    let store_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    let source = provider_source_for_path(CaptureProvider::ForgeCode, fixture.clone());
    assert_eq!(source.source_format, FORGECODE_SQLITE_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_forgecode_sqlite(
        &fixture,
        &mut store,
        ForgeCodeSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..ForgeCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 3);
    let session_id = provider_session_uuid(CaptureProvider::ForgeCode, "forge-root");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 3);
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));
    assert!(store
        .search_event_hits("forgecode oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::ForgeCode)));
    let file_touch_count: i64 = Connection::open(&store_path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM ctx_files_touched WHERE provider = 'forgecode'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(file_touch_count, 4);

    let second = import_forgecode_sqlite(
        &fixture,
        &mut store,
        ForgeCodeSqliteImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            ..ForgeCodeSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 3);
    let file_touch_count_after: i64 = Connection::open(&store_path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM ctx_files_touched WHERE provider = 'forgecode'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(file_touch_count_after, file_touch_count);
}

#[test]
pub(crate) fn native_forgecode_reports_missing_table_and_corrupt_db() {
    let temp = tempdir();
    let missing_table = temp.path().join("missing-forge.db");
    let conn = Connection::open(&missing_table).unwrap();
    conn.execute_batch("CREATE TABLE unrelated (id INTEGER PRIMARY KEY);")
        .unwrap();
    drop(conn);
    let corrupt = temp.path().join("corrupt-forge.db");
    fs::write(&corrupt, b"not sqlite").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err = import_forgecode_sqlite(
        &missing_table,
        &mut store,
        ForgeCodeSqliteImportOptions::default(),
    )
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("ForgeCode .forge.db is missing required conversations table"));

    let err = import_forgecode_sqlite(
        &corrupt,
        &mut store,
        ForgeCodeSqliteImportOptions::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("not a database"));
}
