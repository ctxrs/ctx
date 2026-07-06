#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_firebender_fixture_imports_project_root_db_and_reimports() {
    let temp = tempdir();
    let project_root = provider_history_fixture("firebender/v1");
    let fixture = project_root
        .join(".idea")
        .join("firebender")
        .join("chat_history.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let root_source = provider_source_for_path(CaptureProvider::Firebender, project_root.clone());
    assert_eq!(root_source.source_format, FIREBENDER_SQLITE_SOURCE_FORMAT);
    assert_eq!(root_source.status, ProviderSourceStatus::Available);
    let db_source = provider_source_for_path(CaptureProvider::Firebender, fixture.clone());
    assert_eq!(db_source.source_format, FIREBENDER_SQLITE_SOURCE_FORMAT);
    assert_eq!(db_source.status, ProviderSourceStatus::Available);

    let first = import_firebender_sqlite(
        &project_root,
        &mut store,
        FirebenderSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(project_root.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T20:10:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..FirebenderSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 3);
    let session_id =
        provider_session_uuid(CaptureProvider::Firebender, "firebender-fixture-session");
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
        .any(|event| event.event_type == EventType::ToolCall));
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("firebender fixture oracle prompt"));
    assert!(rendered.contains("Firebender fixture oracle response"));
    assert!(store
        .search_event_hits("firebender fixture oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Firebender)));

    let source = store
        .capture_source_by_external_session(
            CaptureProvider::Firebender,
            "firebender-fixture-session",
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        source.sync.metadata["source_metadata"]["storage"].as_str(),
        Some(".idea/firebender/chat_history.db")
    );

    let second = import_firebender_sqlite(
        &fixture,
        &mut store,
        FirebenderSqliteImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            ..FirebenderSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 3);
}
