#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_codebuddy_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codebuddy/Data");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_codebuddy_history(
        &fixture,
        &mut store,
        CodeBuddyImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T16:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..CodeBuddyImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 3);

    let alpha = provider_session_uuid(
        CaptureProvider::CodeBuddy,
        "11112222333344445555666677778888/session-alpha",
    );
    let events = store.events_for_session(alpha).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].role, Some(EventRole::User));
    assert_eq!(events[1].role, Some(EventRole::Assistant));
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("codebuddy oracle prompt update"));
    assert!(rendered.contains("src/codebuddy_fixture.rs"));
    assert!(!events[0]
        .payload
        .pointer("/body/text")
        .and_then(Value::as_str)
        .unwrap()
        .contains("project_context"));
    assert!(store
        .search_event_hits("codebuddy oracle prompt", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::CodeBuddy)));
    assert!(store
        .search_event_hits("project_context", 10)
        .unwrap()
        .is_empty());
    assert!(store
        .search_event_hits("plain fallback codebuddy beta oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::CodeBuddy)));

    let source = provider_source_for_path(CaptureProvider::CodeBuddy, fixture.clone());
    assert_eq!(source.source_format, CODEBUDDY_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let second = import_codebuddy_history(
        &fixture,
        &mut store,
        CodeBuddyImportOptions {
            allow_partial_failures: true,
            ..CodeBuddyImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 2);
    assert_eq!(second.skipped_events, 3);
}
