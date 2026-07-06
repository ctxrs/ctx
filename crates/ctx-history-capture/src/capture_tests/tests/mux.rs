#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_mux_fixture_imports_searches_reimports_and_subagents() {
    let temp = tempdir();
    let fixture = provider_history_fixture("mux/v0.27.0/sessions");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Mux, fixture.clone());
    assert_eq!(source.source_format, "mux_session_jsonl_tree");
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_mux_history(
        &fixture,
        &mut store,
        MuxImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T19:20:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..MuxImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 6);
    assert_eq!(first.imported_edges, 1);

    let parent_id = provider_session_uuid(CaptureProvider::Mux, "mux-parent-session");
    let parent_events = store.events_for_session(parent_id).unwrap();
    assert_eq!(parent_events.len(), 4);
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));
    let parent_rendered = serde_json::to_string(&parent_events).unwrap();
    assert!(parent_rendered.contains("mux jsonl oracle prompt"));
    assert!(parent_rendered.contains("mux partial response still searchable"));
    assert!(parent_rendered.contains("src/mux_oracle.txt"));

    let child_id = provider_session_uuid(CaptureProvider::Mux, "mux-child-session");
    let child = store.get_session(child_id).unwrap();
    assert_eq!(child.parent_session_id, Some(parent_id));
    assert_eq!(child.agent_type, AgentType::Subagent);
    let child_events = store.events_for_session(child_id).unwrap();
    assert_eq!(child_events.len(), 2);
    assert!(serde_json::to_string(&child_events)
        .unwrap()
        .contains("src/mux_child_oracle.txt"));

    assert!(store
        .search_event_hits("mux jsonl oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Mux)));
    assert!(store
        .export_archive()
        .unwrap()
        .files_touched
        .iter()
        .any(|file| file.path == "src/mux_oracle.txt"));

    let second = import_mux_history(
        &fixture,
        &mut store,
        MuxImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            ..MuxImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.imported_edges, 0);
    assert_eq!(second.skipped_sessions, 2);
    assert_eq!(second.skipped_events, 6);
}

#[test]
pub(crate) fn native_mux_reports_malformed_jsonl_partially() {
    let temp = tempdir();
    let fixture = provider_history_fixture("mux/malformed/sessions");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_mux_history(
        &fixture,
        &mut store,
        MuxImportOptions {
            allow_partial_failures: true,
            ..MuxImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    assert!(summary.failures[0].error.contains("malformed JSONL"));
    assert!(store
        .search_event_hits("mux after malformed oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Mux)));
}
