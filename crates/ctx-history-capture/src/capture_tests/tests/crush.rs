#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_crush_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("crush/v1/crush.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_crush_sqlite(
        &fixture,
        &mut store,
        CrushSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..CrushSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 4);
    assert_eq!(first.imported_edges, 1);
    let parent_id = provider_session_uuid(CaptureProvider::Crush, "crush-root");
    let child_id = provider_session_uuid(CaptureProvider::Crush, "crush-child");
    assert_eq!(
        store.get_session(child_id).unwrap().parent_session_id,
        Some(parent_id)
    );
    let events = store.events_for_session(parent_id).unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::Summary));
    assert!(store
        .search_event_hits("crush oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Crush)));
    let source = provider_source_for_path(CaptureProvider::Crush, fixture.clone());
    assert_eq!(source.source_format, CRUSH_SQLITE_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let second = import_crush_sqlite(
        &fixture,
        &mut store,
        CrushSqliteImportOptions {
            allow_partial_failures: true,
            ..CrushSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.imported_edges, 0);
    assert_eq!(second.skipped_sessions, 2);
    assert_eq!(second.skipped_events, 4);
    assert_eq!(second.skipped_edges, 1);
}
