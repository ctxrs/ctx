#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_windsurf_fixture_imports_searches_reimports_and_file_touches() {
    let temp = tempdir();
    let fixture = provider_history_fixture("windsurf/transcripts");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Windsurf, fixture.clone());
    assert_eq!(
        source.source_format,
        "windsurf_cascade_hook_transcript_jsonl_tree"
    );
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert!(source.import_support.is_auto_importable());
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_windsurf_cascade_hook_transcripts(
        &fixture,
        &mut store,
        WindsurfCascadeHookImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            imported_at: "2026-06-24T14:00:00Z".parse().unwrap(),
            ..WindsurfCascadeHookImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{first:?}");
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 5);
    assert!(store
        .search_event_hits("windsurf cascade hook oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Windsurf)));
    assert!(store
        .search_event_hits("windsurf unknown typed payload oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Windsurf)));

    let session_id = provider_session_uuid(CaptureProvider::Windsurf, "windsurf-hook-trajectory-1");
    let events = store.events_for_session(session_id).unwrap();
    let code_action = events
        .iter()
        .find(|event| event.event_type == EventType::ToolCall)
        .unwrap();
    assert_eq!(
        code_action.payload["body"]["body"]["code_action"]["path"].as_str(),
        Some("src/windsurf_hook_oracle.py")
    );
    assert_eq!(
        code_action.payload["body"]["body"]["code_action"]["new_content"]["redacted"].as_str(),
        Some("sensitive_transcript_field")
    );
    assert!(!code_action.payload.to_string().contains("print("));

    let archive = store.export_archive().unwrap();
    assert!(archive.files_touched.iter().any(|file| {
        file.path == "src/windsurf_hook_oracle.py" && file.confidence == Confidence::High
    }));

    let second = import_windsurf_cascade_hook_transcripts(
        &fixture,
        &mut store,
        WindsurfCascadeHookImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            imported_at: "2026-06-24T14:05:00Z".parse().unwrap(),
            ..WindsurfCascadeHookImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{second:?}");
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 5);
}

#[test]
pub(crate) fn native_windsurf_reports_malformed_jsonl_partially() {
    let temp = tempdir();
    let fixture = provider_history_fixture("windsurf/malformed/transcripts");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_windsurf_cascade_hook_transcripts(
        &fixture,
        &mut store,
        WindsurfCascadeHookImportOptions {
            allow_partial_failures: true,
            imported_at: "2026-06-24T14:00:00Z".parse().unwrap(),
            ..WindsurfCascadeHookImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{summary:?}");
    assert_eq!(summary.failures[0].line, 2);
    assert!(summary.failures[0].error.contains("malformed JSONL"));
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    assert!(store
        .search_event_hits("windsurf malformed after bad oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Windsurf)));
}
