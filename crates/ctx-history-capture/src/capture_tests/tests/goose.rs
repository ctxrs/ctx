#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_goose_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("goose/v14/sessions.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_goose_sessions_sqlite(
        &fixture,
        &mut store,
        GooseSessionsSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..GooseSessionsSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 3);
    let session_id = provider_session_uuid(CaptureProvider::Goose, "goose-root");
    store.get_session(session_id).unwrap();
    let source = store
        .capture_source_by_external_session(CaptureProvider::Goose, "goose-root")
        .unwrap()
        .unwrap();
    assert_eq!(source.descriptor.cwd.as_deref(), Some("/workspace/goose"));
    assert!(source
        .sync
        .metadata
        .to_string()
        .contains("\"goose_schema_version\":14"));
    let events = store.events_for_session(session_id).unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));
    assert!(store
        .search_event_hits("goose oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Goose)));

    let second = import_goose_sessions_sqlite(
        &fixture,
        &mut store,
        GooseSessionsSqliteImportOptions {
            allow_partial_failures: true,
            ..GooseSessionsSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 3);
}
