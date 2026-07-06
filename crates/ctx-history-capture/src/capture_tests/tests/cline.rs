#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_task_json_imports_cline_and_roo_task_directories() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let cline = provider_history_fixture("cline/data");
    let cline_first = import_cline_task_json_history(
        &cline,
        &mut store,
        ClineTaskJsonImportOptions {
            source_path: Some(cline.clone()),
            allow_partial_failures: true,
            imported_at: "2026-06-30T12:10:00Z".parse().unwrap(),
            ..ClineTaskJsonImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(cline_first.failed, 0, "{:?}", cline_first.failures);
    assert_eq!(cline_first.imported_sessions, 1);
    assert_eq!(cline_first.imported_events, 3);

    let cline_session = provider_session_uuid(CaptureProvider::Cline, "cline-task-1");
    let cline_events = store.events_for_session(cline_session).unwrap();
    assert_eq!(cline_events.len(), 3);
    assert!(cline_events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(store
        .export_archive()
        .unwrap()
        .files_touched
        .iter()
        .any(|file| file.path == "docs/cline-task-json.md"));

    let cline_second = import_cline_task_json_history(
        &cline,
        &mut store,
        ClineTaskJsonImportOptions {
            source_path: Some(cline.clone()),
            allow_partial_failures: true,
            imported_at: "2026-06-30T12:10:00Z".parse().unwrap(),
            ..ClineTaskJsonImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(cline_second.imported_sessions, 0);
    assert_eq!(cline_second.imported_events, 0);
    assert_eq!(cline_second.skipped_sessions, 1);
    assert_eq!(cline_second.skipped_events, 3);

    let roo = provider_history_fixture("roo/storage");
    let roo_first = import_roo_task_json_history(
        &roo,
        &mut store,
        RooTaskJsonImportOptions {
            source_path: Some(roo.clone()),
            allow_partial_failures: true,
            imported_at: "2026-06-30T12:10:00Z".parse().unwrap(),
            ..RooTaskJsonImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(roo_first.failed, 0, "{:?}", roo_first.failures);
    assert_eq!(roo_first.imported_sessions, 2);
    assert_eq!(roo_first.imported_events, 5);

    let roo_session = provider_session_uuid(CaptureProvider::RooCode, "roo-task-1");
    let roo_events = store.events_for_session(roo_session).unwrap();
    assert_eq!(roo_events.len(), 3);
    assert!(roo_events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    let fallback = provider_session_uuid(CaptureProvider::RooCode, "roo-fallback-task");
    assert_eq!(store.events_for_session(fallback).unwrap().len(), 2);
    assert!(store
        .export_archive()
        .unwrap()
        .files_touched
        .iter()
        .any(|file| file.path == "tests/roo-task-json.txt"));
}

#[test]
pub(crate) fn native_task_json_malformed_file_is_atomic_without_partial_failures() {
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
