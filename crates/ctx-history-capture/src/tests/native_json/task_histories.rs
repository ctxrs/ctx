use crate::provider::importer::provider_session_uuid;
use crate::tests::support::assertions::{
    assert_event_type_count, assert_events_have_provider_citations, assert_search_hits_provider,
    assert_search_misses,
};
use crate::tests::support::paths::{provider_history_fixture, tempdir};
use crate::tests::support::provider_state::stored_provider_session_id;
#[cfg(unix)]
use crate::CaptureError;
use crate::{
    import_cline_task_json_history, import_codebuddy_history, import_roo_task_json_history,
    provider_source_for_path, ClineTaskJsonImportOptions, CodeBuddyImportOptions,
    ProviderSourceStatus, RooTaskJsonImportOptions, CODEBUDDY_SOURCE_FORMAT,
};
use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventRole, EventType};
use ctx_history_store::Store;
use serde_json::{json, Value};
use std::fs;

#[test]
fn native_task_json_imports_cline_and_roo_task_directories() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let cline = provider_history_fixture("cline/data");
    let cline_first = import_cline_task_json_history(
        &cline,
        &mut store,
        ClineTaskJsonImportOptions {
            source_path: Some(cline.clone()),
            imported_at: "2026-06-30T12:10:00Z".parse().unwrap(),
            ..ClineTaskJsonImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(cline_first.failed, 0, "{:?}", cline_first.failures);
    assert_eq!(cline_first.imported_sessions, 1);
    assert_eq!(cline_first.imported_events, 4);

    let cline_session = stored_provider_session_id(&store, CaptureProvider::Cline, "cline-task-1");
    let cline_events = store.events_for_session(cline_session).unwrap();
    assert_eq!(cline_events.len(), 4);
    assert_event_type_count(&cline_events, EventType::ToolCall, 1);
    assert_event_type_count(&cline_events, EventType::ToolOutput, 0);
    assert_events_have_provider_citations(&cline_events);
    assert_search_hits_provider(
        &store,
        "Write a short parser note for Cline task JSON support.",
        CaptureProvider::Cline,
    );
    assert_search_misses(&store, "CLINE_RAW_TOOL_RESULT_NEEDLE");
    assert!(!serde_json::to_string(&cline_events)
        .unwrap()
        .contains("CLINE_RAW_TOOL_RESULT_NEEDLE"));
    assert!(store
        .export_archive()
        .unwrap()
        .files_touched
        .iter()
        .any(|file| file.path == "docs/cline-task-json.md"));
    assert!(store.runs_for_session(cline_session).unwrap().is_empty());

    let cline_second = import_cline_task_json_history(
        &cline,
        &mut store,
        ClineTaskJsonImportOptions {
            source_path: Some(cline.clone()),
            imported_at: "2026-06-30T12:10:00Z".parse().unwrap(),
            ..ClineTaskJsonImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(cline_second.imported_sessions, 0);
    assert_eq!(cline_second.imported_events, 0);
    assert_eq!(cline_second.skipped_sessions, 1);
    assert_eq!(cline_second.skipped_events, 4);

    let roo = provider_history_fixture("roo/storage");
    let roo_first = import_roo_task_json_history(
        &roo,
        &mut store,
        RooTaskJsonImportOptions {
            source_path: Some(roo.clone()),
            imported_at: "2026-06-30T12:10:00Z".parse().unwrap(),
            ..RooTaskJsonImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(roo_first.failed, 0, "{:?}", roo_first.failures);
    assert_eq!(roo_first.imported_sessions, 2);
    assert_eq!(roo_first.imported_events, 6);

    let roo_session = stored_provider_session_id(&store, CaptureProvider::RooCode, "roo-task-1");
    let roo_events = store.events_for_session(roo_session).unwrap();
    assert_eq!(roo_events.len(), 4);
    assert_event_type_count(&roo_events, EventType::ToolCall, 1);
    assert_event_type_count(&roo_events, EventType::ToolOutput, 0);
    assert_events_have_provider_citations(&roo_events);
    assert_search_hits_provider(
        &store,
        "Add a Roo Code task JSON import smoke test.",
        CaptureProvider::RooCode,
    );
    assert_search_misses(&store, "ROO_RAW_TOOL_RESULT_NEEDLE");
    assert!(!serde_json::to_string(&roo_events)
        .unwrap()
        .contains("ROO_RAW_TOOL_RESULT_NEEDLE"));
    let fallback =
        stored_provider_session_id(&store, CaptureProvider::RooCode, "roo-fallback-task");
    assert_eq!(store.events_for_session(fallback).unwrap().len(), 2);
    assert!(store
        .export_archive()
        .unwrap()
        .files_touched
        .iter()
        .any(|file| file.path == "tests/roo-task-json.txt"));
    assert!(store.runs_for_session(roo_session).unwrap().is_empty());
    assert!(store.runs_for_session(fallback).unwrap().is_empty());

    let roo_second = import_roo_task_json_history(
        &roo,
        &mut store,
        RooTaskJsonImportOptions {
            source_path: Some(roo.clone()),
            imported_at: "2026-06-30T12:15:00Z".parse().unwrap(),
            ..RooTaskJsonImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(roo_second.imported_sessions, 0);
    assert_eq!(roo_second.imported_events, 0);
    assert_eq!(roo_second.skipped_sessions, 2);
    assert_eq!(roo_second.skipped_events, 6);
}
#[test]
fn native_task_json_all_invalid_file_reports_rejection() {
    let temp = tempdir();
    let task = temp.path().join("cline-data/tasks/cline-bad");
    fs::create_dir_all(&task).unwrap();
    fs::write(
        task.join("task_metadata.json"),
        r#"{"taskId":"cline-bad","createdAt":"2026-06-30T12:00:00Z"}"#,
    )
    .unwrap();
    fs::write(
        task.join("api_conversation_history.json"),
        "[{\"role\":\"user\"",
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_cline_task_json_history(
        temp.path().join("cline-data"),
        &mut store,
        ClineTaskJsonImportOptions::default(),
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(summary.failures[0]
        .error
        .contains("api_conversation_history.json"));
    let session_id = provider_session_uuid(CaptureProvider::Cline, "cline-bad");
    assert!(store.get_session(session_id).is_err());
}

#[test]
fn native_task_json_metadata_only_task_rejects_no_real_message() {
    let temp = tempdir();
    let task = temp.path().join("cline-data/tasks/cline-metadata-only");
    fs::create_dir_all(&task).unwrap();
    fs::write(
        task.join("task_metadata.json"),
        json!({
            "taskId": "cline-metadata-only",
            "createdAt": "2026-06-30T12:00:00Z",
            "task": "metadata only should not import"
        })
        .to_string(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_cline_task_json_history(
        temp.path().join("cline-data"),
        &mut store,
        ClineTaskJsonImportOptions {
            ..ClineTaskJsonImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(summary.failures[0]
        .error
        .contains("no real conversation message"));
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_roo_non_array_message_history_rejects_no_real_message() {
    let temp = tempdir();
    let task = temp.path().join("roo-storage/tasks/roo-non-array");
    fs::create_dir_all(&task).unwrap();
    fs::write(
        task.join("api_conversation_history.json"),
        json!({
            "messages": {
                "role": "user",
                "content": "roo non-array history should not import"
            }
        })
        .to_string(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_roo_task_json_history(
        temp.path().join("roo-storage"),
        &mut store,
        RooTaskJsonImportOptions {
            ..RooTaskJsonImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(summary.failures[0]
        .error
        .contains("no real conversation message"));
    assert!(store
        .search_event_hits("roo non-array history should not import", 10)
        .unwrap()
        .is_empty());
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_codebuddy_fixture_imports_searches_and_reimports() {
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
            ..CodeBuddyImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 3);

    let alpha = stored_provider_session_id(
        &store,
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

#[test]
fn native_codebuddy_empty_messages_rejects_no_real_message() {
    let temp = tempdir();
    let session_dir = temp.path().join("codebuddy/project/session-empty");
    fs::create_dir_all(session_dir.join("messages")).unwrap();
    fs::write(
        session_dir.join("index.json"),
        json!({"messages": []}).to_string(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codebuddy_history(
        temp.path().join("codebuddy/project"),
        &mut store,
        CodeBuddyImportOptions {
            ..CodeBuddyImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0]
        .error
        .contains("no real conversation messages"));
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_codebuddy_non_array_messages_rejects_orphan_message_file() {
    let temp = tempdir();
    let session_dir = temp.path().join("codebuddy/project/session-non-array");
    fs::create_dir_all(session_dir.join("messages")).unwrap();
    fs::write(
        session_dir.join("index.json"),
        json!({"messages": {"id": "message-1", "role": "user"}}).to_string(),
    )
    .unwrap();
    fs::write(
        session_dir.join("messages/message-1.json"),
        json!({"content": "codebuddy orphan message should not import"}).to_string(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codebuddy_history(
        temp.path().join("codebuddy/project"),
        &mut store,
        CodeBuddyImportOptions {
            ..CodeBuddyImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(summary.failures[0]
        .error
        .contains("no real conversation messages"));
    assert!(store
        .search_event_hits("codebuddy orphan message should not import", 10)
        .unwrap()
        .is_empty());
    assert!(store.list_sessions().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn native_codebuddy_symlinked_messages_dir_is_not_imported() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let project = temp.path().join("codebuddy/project");
    let session_dir = project.join("session-linked");
    let real_messages = temp.path().join("real-messages");
    fs::create_dir_all(&session_dir).unwrap();
    fs::create_dir_all(&real_messages).unwrap();
    fs::write(
        session_dir.join("index.json"),
        json!({"messages": [{"id": "message-1", "role": "user"}]}).to_string(),
    )
    .unwrap();
    fs::write(
        real_messages.join("message-1.json"),
        json!({"content": "symlinked CodeBuddy content must not import"}).to_string(),
    )
    .unwrap();
    symlink(&real_messages, session_dir.join("messages")).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err = import_codebuddy_history(
        &project,
        &mut store,
        CodeBuddyImportOptions {
            ..CodeBuddyImportOptions::default()
        },
    )
    .unwrap_err();

    assert!(matches!(
        err,
        CaptureError::InvalidProviderTranscriptPath { path, reason }
            if path.ends_with("project")
                && reason.contains("no CodeBuddy history sessions")
    ));
    assert!(store.list_sessions().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn native_codebuddy_symlinked_message_file_is_not_imported() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let project = temp.path().join("codebuddy/project");
    let session_dir = project.join("session-linked-message");
    let messages_dir = session_dir.join("messages");
    let outside_message = temp.path().join("outside-message.json");
    fs::create_dir_all(&messages_dir).unwrap();
    fs::write(
        session_dir.join("index.json"),
        json!({"messages": [{"id": "message-1", "role": "user"}]}).to_string(),
    )
    .unwrap();
    fs::write(
        &outside_message,
        json!({"content": "symlinked CodeBuddy message file must not import"}).to_string(),
    )
    .unwrap();
    symlink(&outside_message, messages_dir.join("message-1.json")).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codebuddy_history(
        &project,
        &mut store,
        CodeBuddyImportOptions {
            ..CodeBuddyImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(summary.failures[0]
        .error
        .contains("symlinked provider transcript files are rejected"));
    assert!(store
        .search_event_hits("symlinked CodeBuddy message file must not import", 10)
        .unwrap()
        .is_empty());
    assert!(store.list_sessions().unwrap().is_empty());
}
