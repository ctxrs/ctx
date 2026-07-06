#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn antigravity_native_history_imports_transcripts_and_preserves_previews() {
    let temp = tempdir();
    let fixture = provider_history_fixture("antigravity/v1/brain");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_antigravity_cli_history(
        &fixture,
        &mut store,
        AntigravityCliImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            imported_at: "2026-06-24T14:00:00Z".parse().unwrap(),
            ..AntigravityCliImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.failures[0].line, 3);
    assert!(summary.failures[0].error.contains("malformed JSONL"));
    assert_eq!(summary.imported_sessions, 4);
    assert_eq!(summary.imported_events, 11);

    let success_session = provider_session_uuid(CaptureProvider::Antigravity, "agy-success");
    let success = store.events_for_session(success_session).unwrap();
    assert_eq!(success.len(), 3);
    let tool = success
        .iter()
        .find(|event| event.event_type == EventType::ToolCall)
        .unwrap();
    assert!(tool.payload["body"]["tool_calls"].is_array());
    assert!(tool.payload["body"]["tool_calls"][0]["args"].is_object());
    assert_eq!(
        tool.payload["body"]["tool_calls"][0]["args"]["CodeContent"].as_str(),
        Some("# Demo\n\nThis is a sanitized Antigravity fixture.\n")
    );
    let archive = store.export_archive().unwrap();
    assert!(archive.files_touched.iter().any(|file| {
        file.path == "/workspace/demo/README.md" && file.confidence == Confidence::High
    }));
    assert_eq!(
        tool.sync.metadata["metadata"]["source_format"].as_str(),
        Some(ANTIGRAVITY_CLI_SOURCE_FORMAT)
    );
    let source_paths: Vec<String> = store
        .list_capture_sources()
        .unwrap()
        .into_iter()
        .filter_map(|source| source.descriptor.raw_source_path)
        .collect();
    assert!(source_paths
        .iter()
        .any(|path| path.contains("transcript_full.jsonl")));

    let future_session = provider_session_uuid(CaptureProvider::Antigravity, "agy-future");
    let future = store.events_for_session(future_session).unwrap();
    assert!(future
        .iter()
        .any(|event| event.event_type == EventType::Notice
            && event.payload["body"]["entry_type"] == "FUTURE_EVENT_KIND"));
    let rendered = serde_json::to_string(&future).unwrap();
    assert!(rendered.contains("ghp_1234567890abcdef"));
    assert!(rendered.contains("/home/example/private.txt"));
    assert!(!rendered.contains("[REDACTED"));
}

#[test]
pub(crate) fn provider_fixture_replay_supports_antigravity_gemini_and_cursor() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let antigravity = provider_fixture("antigravity.jsonl");
    let antigravity_summary = import_provider_fixture_jsonl(
        &antigravity,
        &mut store,
        fixed_import_options(antigravity.clone()),
    )
    .unwrap();
    assert_eq!(antigravity_summary.failed, 0);
    assert_eq!(antigravity_summary.imported_sessions, 2);
    assert_eq!(antigravity_summary.imported_events, 3);
    assert_eq!(antigravity_summary.imported_edges, 1);
    let antigravity_parent = provider_session_uuid(CaptureProvider::Antigravity, "agy-session-1");
    let antigravity_child =
        provider_session_uuid(CaptureProvider::Antigravity, "agy-session-1-worker");
    assert_eq!(
        store
            .get_session(antigravity_child)
            .unwrap()
            .parent_session_id,
        Some(antigravity_parent)
    );

    let gemini = provider_fixture("gemini.jsonl");
    let gemini_summary =
        import_provider_fixture_jsonl(&gemini, &mut store, fixed_import_options(gemini.clone()))
            .unwrap();
    assert_eq!(gemini_summary.failed, 0);
    assert_eq!(gemini_summary.imported_sessions, 1);
    assert_eq!(gemini_summary.imported_events, 2);
    let gemini_session = provider_session_uuid(CaptureProvider::Gemini, "gemini-session-1");
    let gemini_events = store.events_for_session(gemini_session).unwrap();
    assert_eq!(gemini_events[1].event_type, EventType::ToolOutput);
    assert_eq!(
        gemini_events[1].sync.metadata["metadata"]["telemetry_outfile"].as_str(),
        Some(".gemini/telemetry.log")
    );

    let cursor = provider_fixture("cursor.jsonl");
    let cursor_summary =
        import_provider_fixture_jsonl(&cursor, &mut store, fixed_import_options(cursor.clone()))
            .unwrap();
    assert_eq!(cursor_summary.failed, 0);
    assert_eq!(cursor_summary.imported_sessions, 1);
    assert_eq!(cursor_summary.imported_events, 2);
    let cursor_session = provider_session_uuid(CaptureProvider::Cursor, "cursor-session-1");
    let cursor_events = store.events_for_session(cursor_session).unwrap();
    assert_eq!(cursor_events[1].event_type, EventType::ToolCall);
    assert_eq!(
        cursor_events[0].sync.metadata["metadata"]["docs_surface"].as_str(),
        Some("Cursor CLI sessions and stream-json output")
    );
}
