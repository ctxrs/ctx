#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_astrbot_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("astrbot/v1/data/data_v4.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::AstrBot, fixture.clone());
    assert_eq!(source.source_format, ASTRBOT_SQLITE_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_astrbot_sqlite(
        &fixture,
        &mut store,
        AstrBotSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-06T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..AstrBotSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 3);

    let session_id = provider_session_uuid(CaptureProvider::AstrBot, "umo-astrbot-1");
    let session = store.get_session(session_id).unwrap();
    assert_eq!(session.provider, CaptureProvider::AstrBot);
    let source = store
        .capture_source_by_external_session(CaptureProvider::AstrBot, "umo-astrbot-1")
        .unwrap()
        .unwrap();
    assert_eq!(
        source.sync.metadata["source_format"].as_str(),
        Some(ASTRBOT_SQLITE_SOURCE_FORMAT)
    );

    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 3);
    assert!(events
        .iter()
        .any(|event| event.role == Some(EventRole::User)));
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("ASTRBOT_ORACLE_USER_TEXT violet jasper harbor"));
    assert!(rendered.contains("ASTRBOT_ORACLE_ASSISTANT_TEXT copper lantern atlas"));
    assert!(rendered.contains("ASTRBOT_PLATFORM_HISTORY_TEXT saffron comet"));

    assert!(store
        .search_event_hits("ASTRBOT_ORACLE_ASSISTANT_TEXT", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::AstrBot)));
    assert!(store
        .search_event_hits("ASTRBOT_PLATFORM_HISTORY_TEXT", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::AstrBot)));

    let second = import_astrbot_sqlite(
        &fixture,
        &mut store,
        AstrBotSqliteImportOptions {
            allow_partial_failures: true,
            ..AstrBotSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 3);
}
