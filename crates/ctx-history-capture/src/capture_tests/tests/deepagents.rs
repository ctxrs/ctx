#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_deepagents_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("deepagents/v1/sessions.db");
    let store_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    let source = provider_source_for_path(CaptureProvider::DeepAgents, fixture.clone());
    assert_eq!(source.source_format, DEEPAGENTS_SQLITE_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_deepagents_sqlite(
        &fixture,
        &mut store,
        DeepAgentsSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T19:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..DeepAgentsSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 3);
    let session_id =
        provider_session_uuid(CaptureProvider::DeepAgents, "deepagents-fixture-thread");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 3);
    assert!(events
        .iter()
        .any(|event| event.role == Some(EventRole::User)));
    assert!(events
        .iter()
        .any(|event| event.role == Some(EventRole::Assistant)));
    assert!(events
        .iter()
        .any(|event| event.role == Some(EventRole::Tool)));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));
    assert!(events.iter().all(|event| {
        event
            .sync
            .metadata
            .to_string()
            .contains("decoded from writes.messages only")
    }));
    assert!(store
        .search_event_hits("deepagents fixture oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::DeepAgents)));

    let source_metadata: String = Connection::open(&store_path)
        .unwrap()
        .query_row(
            "SELECT metadata_json FROM capture_sources WHERE provider = 'deepagents'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(source_metadata.contains("checkpoint state blobs are not indexed"));

    let second = import_deepagents_sqlite(
        &fixture,
        &mut store,
        DeepAgentsSqliteImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            ..DeepAgentsSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 3);
}

#[test]
pub(crate) fn native_deepagents_reports_malformed_writes_and_corrupt_db() {
    let temp = tempdir();
    let fixture = provider_history_fixture("deepagents/v1/sessions.db");
    let malformed = temp.path().join("malformed-deepagents.db");
    fs::copy(&fixture, &malformed).unwrap();
    Connection::open(&malformed)
        .unwrap()
        .execute("UPDATE writes SET value = x'd9'", [])
        .unwrap();
    let corrupt = temp.path().join("corrupt-deepagents.db");
    fs::write(&corrupt, b"not sqlite").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_deepagents_sqlite(
        &malformed,
        &mut store,
        DeepAgentsSqliteImportOptions {
            source_path: Some(malformed.clone()),
            allow_partial_failures: true,
            ..DeepAgentsSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0]
        .error
        .contains("invalid Deep Agents msgpack payload"));
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 0);

    let err = import_deepagents_sqlite(
        &corrupt,
        &mut store,
        DeepAgentsSqliteImportOptions::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("not a database"));
}
