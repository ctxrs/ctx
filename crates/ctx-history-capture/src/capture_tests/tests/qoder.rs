#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_qoder_fixture_imports_documented_transcript_jsonl() {
    let temp = tempdir();
    let fixture = provider_history_fixture("qoder/projects");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Qoder, fixture.clone());
    assert_eq!(source.source_format, "qoder_transcript_jsonl_tree");
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_qoder_history(
        &fixture,
        &mut store,
        QoderImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            imported_at: "2026-07-01T12:00:00Z".parse().unwrap(),
            ..QoderImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{first:?}");
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 7);
    assert!(store
        .search_event_hits("qoder jsonl oracle prompt", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Qoder)));
    assert!(store
        .search_event_hits("qoder native import ok", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Qoder)));

    let session_id = provider_session_uuid(CaptureProvider::Qoder, "qoder-session-1");
    let events = store.events_for_session(session_id).unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.event_type == EventType::Message
                && event.role == Some(EventRole::User))
    );
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall
            && event.role == Some(EventRole::Assistant)));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput
            && event.role == Some(EventRole::User)
            && event.payload["body"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("qoder import ok"))));

    let second = import_qoder_history(
        &fixture,
        &mut store,
        QoderImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            imported_at: "2026-07-01T12:05:00Z".parse().unwrap(),
            ..QoderImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{second:?}");
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 7);
}
