use std::{
    fs,
    path::{Path, PathBuf},
};

use ctx_history_capture::{import_cursor_native_history, CursorNativeImportOptions};
use ctx_history_store::Store;
use rusqlite::Connection;
use tempfile::TempDir;

fn tempdir() -> TempDir {
    tempfile::Builder::new()
        .prefix("cursor-native-import-")
        .tempdir()
        .unwrap()
}

fn provider_history_fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/provider-history")
        .join(path)
}

fn materialized_provider_history_fixture(temp: &TempDir, path: &str) -> PathBuf {
    let source = provider_history_fixture(path);
    let target = temp.path().join("materialized-provider-history").join(path);
    copy_dir_all(&source, &target);
    target
}

fn copy_dir_all(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let metadata = fs::metadata(&source_path).unwrap();
        if metadata.is_dir() {
            copy_dir_all(&source_path, &target_path);
        } else if metadata.is_file() {
            fs::copy(&source_path, &target_path).unwrap();
        }
    }
}

#[test]
fn imports_cursor_agent_transcript_jsonl_tree() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let fixture = materialized_provider_history_fixture(&temp, "cursor/2026.06.24/projects");
    let mut store = Store::open(&db_path).unwrap();

    let summary = import_cursor_native_history(
        &fixture,
        &mut store,
        CursorNativeImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-24T13:00:00Z".parse().unwrap(),
            allow_partial_failures: true,
            ..CursorNativeImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 6);
    drop(store);

    let conn = Connection::open(&db_path).unwrap();
    let session_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE provider = 'cursor'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let tool_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events e JOIN sessions s ON e.session_id = s.id WHERE s.provider = 'cursor' AND e.event_type IN ('tool_call', 'tool_output')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(session_count, 2);
    assert_eq!(tool_count, 2);
}

#[test]
fn reports_malformed_cursor_agent_transcript_when_partial_disallowed() {
    let temp = tempdir();
    let fixture = temp
        .path()
        .join("cursor/projects/sanitized-workspace/agent-transcripts/cursor-malformed-session");
    fs::create_dir_all(&fixture).unwrap();
    fs::write(
        fixture.join("cursor-malformed-session.jsonl"),
        concat!(
            "{\"timestamp\":\"2026-06-24T12:10:00Z\",\"role\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"valid\"}]}}\n",
            "{\"timestamp\":\"2026-06-24T12:10:01Z\",\"role\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"partial\"}]}\n",
        ),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_cursor_native_history(&fixture, &mut store, CursorNativeImportOptions::default())
            .unwrap();

    assert_eq!(summary.failed, 1);
    assert_eq!(summary.imported_events, 0);
    assert!(summary.failures[0].error.contains("malformed JSONL"));
}
