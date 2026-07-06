#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_kilo_imports_opencode_derived_sqlite_fixture_idempotently() {
    let temp = tempdir();
    let fixture = provider_history_fixture("kilo/kilo.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_kilo_sqlite(
        &fixture,
        &mut store,
        KiloSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..KiloSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 2);

    let session_id = provider_session_uuid(CaptureProvider::Kilo, "kilo-root");
    let session = store.get_session(session_id).unwrap();
    assert_eq!(session.provider, CaptureProvider::Kilo);
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0].sync.metadata["source_format"].as_str(),
        Some(KILO_SQLITE_SOURCE_FORMAT)
    );
    assert_eq!(
        events[0].payload["body"]["session_message_seq"].as_i64(),
        Some(1)
    );
    assert_eq!(
        events[1].payload["body"]["session_message_seq"].as_i64(),
        Some(2)
    );

    let second = import_kilo_sqlite(
        &fixture,
        &mut store,
        KiloSqliteImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            ..KiloSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 2);
}
