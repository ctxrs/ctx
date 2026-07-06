#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_kiro_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("kiro-cli/v2/data.sqlite3");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::KiroCli, fixture.clone());
    assert_eq!(source.source_format, KIRO_SQLITE_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_kiro_sqlite(
        &fixture,
        &mut store,
        KiroSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-06-25T20:12:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..KiroSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 3);
    let session_id = provider_session_uuid(
        CaptureProvider::KiroCli,
        "00000000-0000-4000-8000-000000000001",
    );
    let session = store.get_session(session_id).unwrap();
    assert_eq!(session.provider, CaptureProvider::KiroCli);
    let source = store
        .capture_source_by_external_session(
            CaptureProvider::KiroCli,
            "00000000-0000-4000-8000-000000000001",
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        source.descriptor.cwd.as_deref(),
        Some("/workspace/kiro-fixture")
    );
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 3);
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(store
        .export_archive()
        .unwrap()
        .files_touched
        .iter()
        .any(|file| file.path == "/workspace/kiro-fixture"));
    assert!(store
        .search_event_hits("kiro oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::KiroCli)));

    let second = import_kiro_sqlite(
        &fixture,
        &mut store,
        KiroSqliteImportOptions {
            allow_partial_failures: true,
            ..KiroSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 3);
}
