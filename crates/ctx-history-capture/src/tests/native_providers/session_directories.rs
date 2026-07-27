use crate::tests::support::assertions::{
    assert_event_type_count, assert_events_have_provider_citations, assert_search_hits_provider,
    assert_search_misses, assert_structural_oversize_failure,
};
use crate::tests::support::fixtures::jsonl::{jsonl_line, oversized_jsonl_line};
use crate::tests::support::paths::{provider_history_fixture, tempdir};
use crate::tests::support::provider_state::stored_provider_session_id;
use crate::{
    import_mistral_vibe_history, import_mux_history, import_rovodev_history,
    provider_source_for_path, MistralVibeImportOptions, MuxImportOptions, ProviderSourceStatus,
    RovoDevImportOptions,
};
use chrono::{DateTime, Utc};
use ctx_history_core::{AgentType, CaptureProvider, EventType};
use ctx_history_store::Store;
use serde_json::Value;
use std::fs;

#[test]
fn native_mistral_vibe_fixture_imports_searches_and_reimports() {
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
            ..MistralVibeImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 3);
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::MistralVibe, "mistral-vibe-native");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 3);
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(!events.iter().any(|event| matches!(
        event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    )));
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
            ..MistralVibeImportOptions::default()
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
fn native_mux_fixture_imports_searches_reimports_and_subagents() {
    let temp = tempdir();
    let fixture = provider_history_fixture("mux/v0.27.0/sessions");
    let child_chat =
        fixture.join("mux-parent-session/subagent-transcripts/mux-child-session/chat.jsonl");
    let mut child_lines = fs::read_to_string(&child_chat)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    child_lines[1]["parts"][1]["input"]["content"] =
        Value::String("mux-child-input-sentinel".into());
    fs::write(
        &child_chat,
        child_lines.into_iter().map(jsonl_line).collect::<String>(),
    )
    .unwrap();
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
            ..MuxImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    // NativePath accounting is source-scoped: parent chat, parent partial,
    // and child chat each publish one accepted session projection.
    assert_eq!(first.imported_sessions, 3);
    assert_eq!(first.imported_events, 4);
    assert_eq!(first.imported_edges, 1);

    let parent_id = stored_provider_session_id(&store, CaptureProvider::Mux, "mux-parent-session");
    let parent_events = store.events_for_session(parent_id).unwrap();
    assert_eq!(parent_events.len(), 3);
    assert_event_type_count(&parent_events, EventType::Message, 2);
    assert_event_type_count(&parent_events, EventType::ToolCall, 1);
    assert_event_type_count(&parent_events, EventType::ToolOutput, 0);
    assert_events_have_provider_citations(&store, &parent_events);
    let parent_rendered = serde_json::to_string(&parent_events).unwrap();
    assert!(parent_rendered.contains("mux jsonl oracle prompt"));
    assert!(parent_rendered.contains("mux partial response still searchable"));
    assert!(parent_rendered.contains("src/mux_oracle.txt"));

    let child_id = stored_provider_session_id(&store, CaptureProvider::Mux, "mux-child-session");
    let child = store.get_session(child_id).unwrap();
    assert_eq!(child.parent_session_id, Some(parent_id));
    assert_eq!(child.agent_type, AgentType::Subagent);
    let child_events = store.events_for_session(child_id).unwrap();
    assert_eq!(child_events.len(), 1);
    assert_event_type_count(&child_events, EventType::Message, 1);
    assert_event_type_count(&child_events, EventType::ToolOutput, 0);
    assert_events_have_provider_citations(&store, &child_events);
    assert!(!serde_json::to_string(&child_events)
        .unwrap()
        .contains("src/mux_child_oracle.txt"));

    assert_search_hits_provider(&store, "mux jsonl oracle", CaptureProvider::Mux);
    assert_search_hits_provider(
        &store,
        "mux partial response still searchable",
        CaptureProvider::Mux,
    );
    assert_search_misses(&store, "mux-child-input-sentinel");
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
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            ..MuxImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.imported_edges, 0);
    // Replay accounting is source-scoped: the parent chat, parent partial,
    // and child chat streams each retain one accepted session projection.
    assert_eq!(second.skipped_sessions, 3);
    assert_eq!(second.skipped_events, 4);
}

#[test]
fn native_mux_rejects_oversized_chat_record_and_keeps_valid_siblings() {
    let temp = tempdir();
    let fixture = provider_history_fixture("mux/v0.27.0/sessions");
    let chat_path = fixture.join("mux-parent-session/chat.jsonl");
    let original = fs::read(&chat_path).unwrap();
    let first_line_end = original
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&original[..first_line_end]);
    bytes.extend_from_slice(&oversized_jsonl_line());
    bytes.extend_from_slice(&original[first_line_end..]);
    fs::write(&chat_path, bytes).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_mux_history(
        &fixture,
        &mut store,
        MuxImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-07-04T19:30:00Z".parse().unwrap(),
            ..MuxImportOptions::default()
        },
    )
    .unwrap();

    assert_structural_oversize_failure(&summary, 2);
    assert_eq!(summary.skipped, 0);
    assert_eq!(summary.skipped_sessions, 0);
    assert_eq!(summary.skipped_events, 0);
    assert_eq!(summary.imported_sessions, 3);
    assert_eq!(summary.imported_events, 4);
    assert!(store
        .search_event_hits("mux jsonl oracle prompt", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Mux)));
}

#[test]
fn native_mux_reports_malformed_jsonl_and_keeps_valid_rows() {
    let temp = tempdir();
    let fixture = provider_history_fixture("mux/malformed/sessions");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_mux_history(
        &fixture,
        &mut store,
        MuxImportOptions {
            ..MuxImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    assert!(summary.failures[0]
        .error
        .starts_with("malformed Mux JSON record: "));
    assert!(store
        .search_event_hits("mux after malformed oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Mux)));
}
#[test]
fn native_rovodev_fixture_imports_searches_reimports_and_file_touches() {
    let temp = tempdir();
    let fixture = provider_history_fixture("rovodev/v1/sessions");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::RovoDev, fixture.clone());
    assert_eq!(source.source_format, "rovodev_session_json_tree");
    assert_eq!(source.status, ProviderSourceStatus::Available);
    let file_source = provider_source_for_path(
        CaptureProvider::RovoDev,
        fixture
            .join("rovodev-fixture-session")
            .join("session_context.json"),
    );
    assert_eq!(file_source.source_format, "rovodev_session_json_tree");
    assert_eq!(file_source.status, ProviderSourceStatus::Available);

    let first = import_rovodev_history(
        &fixture,
        &mut store,
        RovoDevImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T15:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            ..RovoDevImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 2);
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::RovoDev, "rovodev-fixture-session");
    let events = store.events_for_session(session_id).unwrap();
    assert_event_type_count(&events, EventType::ToolCall, 1);
    assert_event_type_count(&events, EventType::ToolOutput, 0);
    assert_events_have_provider_citations(&store, &events);
    assert_eq!(
        events[0].sync.metadata["source_format"].as_str(),
        Some("rovodev_session_json_tree")
    );
    assert!(store
        .search_event_hits("rovodev fixture oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::RovoDev)));
    assert_search_misses(&store, "wrote src/rovodev_oracle.rs");
    assert!(!serde_json::to_string(&events)
        .unwrap()
        .contains("wrote src/rovodev_oracle.rs"));
    assert!(store
        .export_archive()
        .unwrap()
        .files_touched
        .iter()
        .any(|file| file.path == "src/rovodev_oracle.rs"));

    let second = import_rovodev_history(
        &fixture,
        &mut store,
        RovoDevImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            ..RovoDevImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 2);
}
