#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_mistral_vibe_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("mistral-vibe/v1/logs/session");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::MistralVibe, fixture.clone());
    assert_eq!(source.source_format, "mistral_vibe_session_jsonl_tree");
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_mistral_vibe_history(
        &fixture,
        &mut store,
        MistralVibeImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T19:05:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..MistralVibeImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 4);
    let session_id = provider_session_uuid(CaptureProvider::MistralVibe, "mistral-vibe-native");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 4);
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));
    assert!(store
        .search_event_hits("mistral vibe oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::MistralVibe)));

    let second = import_mistral_vibe_history(
        &fixture,
        &mut store,
        MistralVibeImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            ..MistralVibeImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 4);
}
