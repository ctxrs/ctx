use crate::tests::support::fixtures::jsonl::jsonl_line;
use crate::tests::support::paths::tempdir;
use crate::{
    import_codex_session_jsonl, import_codex_session_jsonl_tail, import_codex_session_paths,
    CodexSessionImportOptions,
};
use ctx_history_store::Store;
use rusqlite::Connection;
use serde_json::json;
use std::fs;

#[test]
fn codex_fast_session_stream_publishes_through_pinned_reader() {
    let temp = tempdir();
    let path = temp.path().join("codex-fast-bounded.jsonl");
    let mut lines = vec![jsonl_line(json!({
        "timestamp": "2026-07-13T12:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": "codex-fast-bounded",
            "timestamp": "2026-07-13T12:00:00Z",
            "cwd": "/repo",
            "originator": "codex-cli"
        }
    }))];
    lines.extend((0..64).map(|index| {
        jsonl_line(json!({
            "timestamp": format!("2026-07-13T12:00:{:02}Z", index % 60),
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": if index % 2 == 0 { "user" } else { "assistant" },
                "content": [{
                    "type": if index % 2 == 0 { "input_text" } else { "output_text" },
                    "text": format!("codex fast bounded event {index}")
                }]
            }
        }))
    }));
    fs::write(&path, lines.concat()).unwrap();

    let db_path = temp.path().join("work.sqlite");
    let mut store =
        Store::open_with_busy_timeout(&db_path, std::time::Duration::from_millis(10)).unwrap();
    let reader = Connection::open(&db_path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    assert_eq!(
        reader
            .query_row("SELECT COUNT(*) FROM events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );

    let summary =
        import_codex_session_jsonl(&path, &mut store, CodexSessionImportOptions::default())
            .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 64);
    assert_eq!(
        reader
            .query_row("SELECT COUNT(*) FROM events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0,
        "the pinned reader must retain its original snapshot"
    );
    reader.execute_batch("ROLLBACK").unwrap();

    let conn = Connection::open(&db_path).unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM events", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        64
    );
    assert_eq!(
        store
            .search_event_hits("codex fast bounded event", 100)
            .unwrap()
            .len(),
        64
    );
}

#[test]
fn codex_paths_publish_all_sources_through_pinned_reader() {
    let temp = tempdir();
    let sessions_root = temp.path().join("codex-parallel-bounded");
    fs::create_dir_all(&sessions_root).unwrap();
    let mut paths = Vec::new();
    for session_index in 0..13 {
        let path = sessions_root.join(format!("session-{session_index}.jsonl"));
        let mut lines = vec![jsonl_line(json!({
            "timestamp": "2026-07-13T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": format!("codex-parallel-bounded-{session_index}"),
                "timestamp": "2026-07-13T12:00:00Z",
                "cwd": "/repo",
                "originator": "codex-cli"
            }
        }))];
        lines.extend((0..4).map(|event_index| {
            jsonl_line(json!({
                "timestamp": format!("2026-07-13T12:00:0{}Z", event_index + 1),
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": if event_index % 2 == 0 { "user" } else { "assistant" },
                    "content": [{
                        "type": if event_index % 2 == 0 { "input_text" } else { "output_text" },
                        "text": format!(
                            "codex parallel bounded {session_index} event {event_index}"
                        )
                    }]
                }
            }))
        }));
        fs::write(&path, lines.concat()).unwrap();
        paths.push(path);
    }

    let db_path = temp.path().join("work.sqlite");
    let mut store =
        Store::open_with_busy_timeout(&db_path, std::time::Duration::from_millis(10)).unwrap();
    let reader = Connection::open(&db_path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    assert_eq!(
        reader
            .query_row("SELECT COUNT(*) FROM events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );

    let summary =
        import_codex_session_paths(paths, &mut store, CodexSessionImportOptions::default())
            .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 13);
    assert_eq!(summary.imported_events, 52);
    assert_eq!(
        reader
            .query_row("SELECT COUNT(*) FROM events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0,
        "the pinned reader must retain its original snapshot"
    );
    reader.execute_batch("ROLLBACK").unwrap();

    let conn = Connection::open(&db_path).unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM events", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        52
    );
    assert_eq!(
        store
            .search_event_hits("codex parallel bounded", 100)
            .unwrap()
            .len(),
        52
    );
}

#[test]
fn codex_tail_stream_publishes_through_pinned_reader() {
    let temp = tempdir();
    let path = temp.path().join("codex-tail-bounded.jsonl");
    let initial = [
        jsonl_line(json!({
            "timestamp": "2026-07-13T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "codex-tail-bounded",
                "timestamp": "2026-07-13T12:00:00Z",
                "cwd": "/repo",
                "originator": "codex-cli"
            }
        })),
        jsonl_line(json!({
            "timestamp": "2026-07-13T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "codex tail initial"}]
            }
        })),
    ]
    .concat();
    fs::write(&path, &initial).unwrap();
    let tail_start = initial.len() as u64;

    let db_path = temp.path().join("work.sqlite");
    let mut store =
        Store::open_with_busy_timeout(&db_path, std::time::Duration::from_millis(10)).unwrap();
    import_codex_session_jsonl(&path, &mut store, CodexSessionImportOptions::default()).unwrap();

    let mut complete = initial;
    complete.push_str(
        &(0..64)
            .map(|index| {
                jsonl_line(json!({
                    "timestamp": format!("2026-07-13T12:01:{:02}Z", index % 60),
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": format!("codex tail bounded event {index}")
                        }]
                    }
                }))
            })
            .collect::<String>(),
    );
    fs::write(&path, complete).unwrap();

    let reader = Connection::open(&db_path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    assert_eq!(
        reader
            .query_row("SELECT COUNT(*) FROM events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let summary = import_codex_session_jsonl_tail(
        &path,
        tail_start,
        &mut store,
        CodexSessionImportOptions::default(),
    )
    .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 64);
    assert_eq!(
        reader
            .query_row("SELECT COUNT(*) FROM events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1,
        "the pinned reader must retain its pre-tail snapshot"
    );
    reader.execute_batch("ROLLBACK").unwrap();

    let conn = Connection::open(&db_path).unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM events", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        65
    );
    assert_eq!(store.search_event_hits("bounded", 100).unwrap().len(), 64);
}
