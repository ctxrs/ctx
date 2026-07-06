#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_junie_fixture_imports_searches_reimports_and_file_touches() {
    let temp = tempdir();
    let fixture = provider_history_fixture("junie/sessions");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Junie, fixture.clone());
    assert_eq!(source.source_format, JUNIE_SESSION_EVENTS_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_junie_history(
        &fixture,
        &mut store,
        JunieImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-06T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..JunieImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 5);

    let session_id = provider_session_uuid(CaptureProvider::Junie, "session-260607-100000-acme");
    let session = store.get_session(session_id).unwrap();
    assert_eq!(session.provider, CaptureProvider::Junie);
    let source = store
        .capture_source_by_external_session(CaptureProvider::Junie, "session-260607-100000-acme")
        .unwrap()
        .unwrap();
    assert_eq!(
        source.descriptor.cwd.as_deref(),
        Some("/workspace/junie-fixture")
    );
    assert_eq!(
        source.sync.metadata["source_format"].as_str(),
        Some(JUNIE_SESSION_EVENTS_SOURCE_FORMAT)
    );

    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 5);
    assert!(events
        .iter()
        .any(|event| event.role == Some(EventRole::User)));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::CommandOutput));
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("JUNIE_ORACLE_USER_TEXT violet cedar compass"));
    assert!(rendered.contains("JUNIE_TERMINAL_OUTPUT saffron harbor"));
    assert!(rendered.contains("JUNIE_FILE_CHANGE_TEXT cobalt lantern"));
    assert!(rendered.contains("JUNIE_RESULT_TEXT copper lantern atlas"));

    assert!(store
        .search_event_hits("JUNIE_RESULT_TEXT", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Junie)));
    assert!(store
        .search_event_hits("JUNIE_TERMINAL_OUTPUT", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Junie)));

    let archive = store.export_archive().unwrap();
    let touched = archive
        .files_touched
        .iter()
        .find(|file| file.path == "src/junie_theme.rs")
        .expect("missing Junie file touch");
    assert_eq!(touched.change_kind, Some(FileChangeKind::Modified));
    assert_eq!(touched.confidence, Confidence::Explicit);
    assert!(touched.event_id.is_some());

    let second = import_junie_history(
        &fixture,
        &mut store,
        JunieImportOptions {
            allow_partial_failures: true,
            ..JunieImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 5);
}

#[test]
pub(crate) fn native_junie_index_rejects_traversal_session_ids() {
    let temp = tempdir();
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(sessions.join("session-safe")).unwrap();
    fs::write(
        sessions.join("index.jsonl"),
        "{\"sessionId\":\"../escape\",\"createdAt\":1783339200000}\n\
         {\"sessionId\":\"session-safe\",\"createdAt\":1783339200000,\"taskName\":\"safe\"}\n",
    )
    .unwrap();
    fs::write(
        sessions.join("session-safe").join("events.jsonl"),
        "{\"kind\":\"UserPromptEvent\",\"prompt\":\"JUNIE_SAFE_SESSION_TEXT\"}\n",
    )
    .unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_junie_history(
        &sessions,
        &mut store,
        JunieImportOptions {
            allow_partial_failures: true,
            ..JunieImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert!(store
        .capture_source_by_external_session(CaptureProvider::Junie, "../escape")
        .unwrap()
        .is_none());
    assert!(store
        .search_event_hits("JUNIE_SAFE_SESSION_TEXT", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Junie)));
}
