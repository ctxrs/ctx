use crate::tests::support::fixtures::jsonl::{jsonl_line, oversized_jsonl_line};
use crate::tests::support::paths::tempdir;
use crate::{
    import_codex_session_jsonl, import_codex_session_jsonl_tail, CodexSessionImportOptions,
};
use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use serde_json::json;
use std::fs;

#[test]
fn codex_session_tail_keeps_valid_append_when_another_row_is_rejected() {
    let temp = tempdir();
    let path = temp.path().join("tail-bad-timestamp-codex.jsonl");
    let initial = [
        jsonl_line(json!({
            "timestamp": "2026-07-03T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "codex-tail-bad-timestamp",
                "timestamp": "2026-07-03T12:00:00Z",
                "cwd": "/workspace",
                "originator": "codex-cli"
            }
        })),
        jsonl_line(json!({
            "timestamp": "2026-07-03T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "initial tail event"}]
            }
        })),
    ]
    .concat();
    fs::write(&path, &initial).unwrap();
    let tail_start = initial.len() as u64;

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let first = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);

    fs::write(
        &path,
        [
            initial,
            jsonl_line(json!({
                "timestamp": "2026-07-03T12:00:02Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "codex-tail-rollback-sentinel"}]
                }
            })),
            jsonl_line(json!({
                "timestamp": "not-rfc3339",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "tail bad timestamp"}]
                }
            })),
        ]
        .concat(),
    )
    .unwrap();

    let summary = import_codex_session_jsonl_tail(
        &path,
        tail_start,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:31:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    let session_id = store
        .session_by_external_session(CaptureProvider::Codex, "codex-tail-bad-timestamp")
        .unwrap()
        .unwrap()
        .id;
    assert_eq!(summary.imported_events, 1, "{:?}", summary.failures);
    assert_eq!(store.events_for_session(session_id).unwrap().len(), 2);
    assert_eq!(store.search_event_hits("initial", 10).unwrap().len(), 1);
    assert_eq!(
        store
            .search_event_hits("codex-tail-rollback-sentinel", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn codex_session_full_then_tail_preserves_configured_source_root() {
    let temp = tempdir();
    let source_root = temp.path().join("sessions");
    let path = source_root.join("2026/07/20/session.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let session_id = "codex-tail-source-root";
    let initial = [
        jsonl_line(json!({
            "timestamp": "2026-07-20T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": session_id,
                "timestamp": "2026-07-20T12:00:00Z",
                "cwd": "/workspace",
                "originator": "codex-cli"
            }
        })),
        jsonl_line(json!({
            "timestamp": "2026-07-20T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "initial event"}]
            }
        })),
    ]
    .concat();
    fs::write(&path, &initial).unwrap();
    let tail_start = initial.len() as u64;

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let options = CodexSessionImportOptions {
        source_path: Some(source_root.clone()),
        imported_at: "2026-07-20T12:30:00Z".parse().unwrap(),
        ..CodexSessionImportOptions::default()
    };
    import_codex_session_jsonl(&path, &mut store, options.clone()).unwrap();

    fs::write(
        &path,
        [
            initial,
            jsonl_line(json!({
                "timestamp": "2026-07-20T12:00:02Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "appended event"}]
                }
            })),
        ]
        .concat(),
    )
    .unwrap();
    import_codex_session_jsonl_tail(&path, tail_start, &mut store, options).unwrap();

    let source = store
        .capture_source_by_external_session(CaptureProvider::Codex, &session_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        source.descriptor.raw_source_path.as_deref(),
        Some(path.to_string_lossy().as_ref())
    );
    assert_eq!(
        source.descriptor.source_root.as_deref(),
        Some(source_root.to_string_lossy().as_ref())
    );
}

#[test]
fn codex_fresh_store_tail_starts_after_session_meta_without_reimporting_it() {
    let temp = tempdir();
    let path = temp.path().join("fresh-tail-codex.jsonl");
    let header = jsonl_line(json!({
        "timestamp": "2026-07-03T12:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": "codex-fresh-tail",
            "timestamp": "2026-07-03T12:00:00Z",
            "cwd": "/workspace",
            "originator": "codex-cli"
        }
    }));
    let tail = jsonl_line(json!({
        "timestamp": "2026-07-03T12:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "fresh tail event"}]
        }
    }));
    fs::write(&path, [header.as_str(), tail.as_str()].concat()).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl_tail(
        &path,
        header.len() as u64,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:31:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.skipped_sessions, 0, "session_meta was reprojected");
    assert_eq!(summary.imported_events, 1);
    assert_eq!(
        store
            .search_event_hits("fresh tail event", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn codex_fresh_store_tail_rejects_a_mid_record_offset() {
    let temp = tempdir();
    let path = temp.path().join("mid-record-tail-codex.jsonl");
    let header = jsonl_line(json!({
        "timestamp": "2026-07-03T12:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": "codex-mid-record-tail",
            "timestamp": "2026-07-03T12:00:00Z"
        }
    }));
    let tail = jsonl_line(json!({
        "timestamp": "2026-07-03T12:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "must stay atomic"}]
        }
    }));
    fs::write(&path, [header.as_str(), tail.as_str()].concat()).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let error = import_codex_session_jsonl_tail(
        &path,
        header.len() as u64 + 1,
        &mut store,
        CodexSessionImportOptions::default(),
    )
    .unwrap_err();

    assert!(error.to_string().contains("complete JSONL record boundary"));
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn codex_fresh_store_tail_preserves_oversized_header_skip() {
    let temp = tempdir();
    let path = temp.path().join("tail-oversized-header-codex.jsonl");
    let mut bytes = oversized_jsonl_line();
    let tail_start = bytes.len() as u64;
    bytes.extend_from_slice(
        jsonl_line(json!({
            "timestamp": "2026-07-03T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "tail should not invent header"}]
            }
        }))
        .as_bytes(),
    );
    fs::write(&path, bytes).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl_tail(
        &path,
        tail_start,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:31:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.skipped_sessions, 1);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}
