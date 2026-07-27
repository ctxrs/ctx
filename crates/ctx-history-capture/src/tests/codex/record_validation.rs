use crate::tests::support::fixtures::jsonl::{jsonl_line, oversized_jsonl_line};
use crate::tests::support::paths::tempdir;
use crate::tests::support::provider_state::stored_provider_session_id;
use crate::{import_codex_session_jsonl, CodexSessionImportOptions, MAX_PROVIDER_JSONL_LINE_BYTES};
use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use serde_json::json;
use std::fs;
use std::path::Path;

fn assert_nativepath_record_rejections(
    summary: &crate::ProviderImportSummary,
    rejected: usize,
    source_path: &Path,
) {
    assert_eq!(summary.failed, rejected, "{:?}", summary.failures);
    assert_eq!(summary.failures.len(), rejected, "{:?}", summary.failures);
    let source_name = source_path.file_name().unwrap().to_string_lossy();
    let parent = source_path.parent().unwrap().display().to_string();
    for failure in &summary.failures {
        assert!(failure.line > 0, "{failure:?}");
        assert!(failure.error.contains(source_name.as_ref()), "{failure:?}");
        assert!(failure.error.contains("raw ordinal"), "{failure:?}");
        assert!(failure.error.contains("(bytes "), "{failure:?}");
        assert!(failure.error.len() <= 512, "{failure:?}");
        assert!(!failure.error.contains(&parent), "{failure:?}");
    }
}

fn assert_nativepath_missing_owner(summary: &crate::ProviderImportSummary) {
    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.failures.len(), 1, "{:?}", summary.failures);
    assert_eq!(summary.failures[0].line, 0);
    assert!(
        summary.failures[0]
            .error
            .contains("Codex NativePath source has no valid session owner"),
        "{:?}",
        summary.failures
    );
}

fn write_codex_session_with_oversized_event(path: &Path) {
    fs::write(
        path,
        [
            jsonl_line(json!({
                "timestamp": "2026-07-03T12:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "codex-oversized-skip",
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
                    "content": [{"type": "input_text", "text": "before oversized event"}]
                }
            })),
            jsonl_line(json!({
                "timestamp": "2026-07-03T12:00:02Z",
                "type": "event_msg",
                "payload": {
                    "type": "patch_apply_end",
                    "stdout": "x".repeat(MAX_PROVIDER_JSONL_LINE_BYTES + 1),
                    "stderr": "",
                    "success": true
                }
            })),
            jsonl_line(json!({
                "timestamp": "2026-07-03T12:00:03Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "after oversized event"}]
                }
            })),
        ]
        .concat(),
    )
    .unwrap();
}

#[test]
fn codex_session_jsonl_fast_import_reports_one_oversized_line() {
    let temp = tempdir();
    let path = temp.path().join("oversized-codex.jsonl");
    write_codex_session_with_oversized_event(&path);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_nativepath_record_rejections(&summary, 1, &path);
    assert_eq!(summary.skipped_events, 1);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Codex, "codex-oversized-skip");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    let payloads = events
        .iter()
        .map(|event| event.payload.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(payloads.contains("before oversized event"), "{payloads}");
    assert!(payloads.contains("after oversized event"), "{payloads}");
    assert!(!payloads.contains("patch_apply_end"), "{payloads}");
}

#[test]
fn codex_session_jsonl_reports_oversized_required_header_and_headerless_event() {
    let temp = tempdir();
    let path = temp.path().join("oversized-header-codex.jsonl");
    let mut bytes = oversized_jsonl_line();
    bytes.extend_from_slice(
        jsonl_line(json!({
            "timestamp": "2026-07-03T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "should not import without header"}]
            }
        }))
        .as_bytes(),
    );
    fs::write(&path, bytes).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_nativepath_missing_owner(&summary);
    assert_eq!(summary.skipped, 0);
    assert_eq!(summary.skipped_sessions, 0);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn codex_session_jsonl_reports_only_oversized_events_with_valid_session_authority() {
    let temp = tempdir();
    let path = temp.path().join("only-oversized-event-codex.jsonl");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        jsonl_line(json!({
            "timestamp": "2026-07-03T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "codex-only-oversized-event",
                "timestamp": "2026-07-03T12:00:00Z",
                "cwd": "/workspace",
                "originator": "codex-cli"
            }
        }))
        .as_bytes(),
    );
    bytes.extend_from_slice(&oversized_jsonl_line());
    fs::write(&path, bytes).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_nativepath_record_rejections(&summary, 1, &path);
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.skipped_sessions, 1);
    assert_eq!(summary.skipped_events, 1);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(summary.failures[0]
        .error
        .contains("Codex JSONL record exceeds the 16 MiB provider bound"));
    assert!(store.list_sessions().unwrap().is_empty());

    let mut slow_store = Store::open(temp.path().join("slow-work.sqlite")).unwrap();
    let slow_summary = import_codex_session_jsonl(
        &path,
        &mut slow_store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:31:00Z".parse().unwrap(),
            capture_work_limit: crate::CaptureWorkLimit::Drain,
            inventory_observation_token: None,
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_nativepath_record_rejections(&slow_summary, 1, &path);
    assert_eq!(slow_summary.skipped, 1);
    assert_eq!(slow_summary.skipped_sessions, 1);
    assert_eq!(slow_summary.skipped_events, 1);
    assert_eq!(slow_summary.imported_sessions, 0);
    assert_eq!(slow_summary.imported_events, 0);
    assert_eq!(slow_summary.failures, summary.failures);
    assert!(slow_store.list_sessions().unwrap().is_empty());
}

#[test]
fn codex_session_jsonl_malformed_header_is_not_hidden_by_oversized_line() {
    let temp = tempdir();
    let path = temp
        .path()
        .join("malformed-header-before-oversized-codex.jsonl");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        jsonl_line(json!({
            "timestamp": "2026-07-03T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "timestamp": "2026-07-03T12:00:00Z",
                "cwd": "/workspace",
                "originator": "codex-cli"
            }
        }))
        .as_bytes(),
    );
    bytes.extend_from_slice(&oversized_jsonl_line());
    fs::write(&path, bytes).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_nativepath_missing_owner(&summary);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn codex_session_jsonl_malformed_relevant_line_is_not_hidden_by_oversized_line() {
    let temp = tempdir();
    let path = temp
        .path()
        .join("malformed-relevant-before-oversized-codex.jsonl");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        jsonl_line(json!({
            "timestamp": "2026-07-03T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "codex-malformed-before-oversized",
                "timestamp": "2026-07-03T12:00:00Z",
                "cwd": "/workspace",
                "originator": "codex-cli"
            }
        }))
        .as_bytes(),
    );
    bytes.extend_from_slice(
        br#"{"timestamp":"2026-07-03T12:00:01Z","type":"response_item","payload":{"type":"message","role":"assistant","content":["unterminated"]"#,
    );
    bytes.push(b'\n');
    bytes.extend_from_slice(&oversized_jsonl_line());
    fs::write(&path, bytes).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_nativepath_record_rejections(&summary, 2, &path);
    assert_eq!(
        summary
            .failures
            .iter()
            .map(|failure| failure.line)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert!(summary
        .failures
        .iter()
        .all(|failure| !failure.error.contains("unterminated")));
    assert_eq!(summary.skipped_events, 2);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn codex_session_jsonl_reports_each_malformed_record_location_without_content() {
    let temp = tempdir();
    let path = temp.path().join("multiple-malformed-codex.jsonl");
    let header = jsonl_line(json!({
        "timestamp": "2026-07-03T12:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": "codex-multiple-malformed",
            "timestamp": "2026-07-03T12:00:00Z",
            "cwd": "/workspace",
            "originator": "codex-cli"
        }
    }));
    let malformed_one = br#"{"timestamp":"2026-07-03T12:00:01Z","secret":"first-rejected-record""#;
    let malformed_two =
        br#"{"timestamp":"2026-07-03T12:00:02Z","secret":"second-rejected-record",}"#;
    let retained = jsonl_line(json!({
        "timestamp": "2026-07-03T12:00:03Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "retained after malformed records"}]
        }
    }));
    let first_start = header.len() as u64;
    let first_end = first_start + malformed_one.len() as u64 + 1;
    let second_start = first_end;
    let second_end = second_start + malformed_two.len() as u64 + 1;
    let mut source = header.into_bytes();
    source.extend_from_slice(malformed_one);
    source.push(b'\n');
    source.extend_from_slice(malformed_two);
    source.push(b'\n');
    source.extend_from_slice(retained.as_bytes());
    fs::write(&path, source).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_nativepath_record_rejections(&summary, 2, &path);
    assert_eq!(
        summary
            .failures
            .iter()
            .map(|failure| failure.line)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert!(
        summary.failures[0]
            .error
            .contains(&format!("(bytes {first_start}..{first_end})")),
        "{:?}",
        summary.failures
    );
    assert!(
        summary.failures[1]
            .error
            .contains(&format!("(bytes {second_start}..{second_end})")),
        "{:?}",
        summary.failures
    );
    assert!(summary.failures.iter().all(|failure| {
        !failure.error.contains("first-rejected-record")
            && !failure.error.contains("second-rejected-record")
    }));
    assert_eq!(summary.skipped_events, 2);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
}

#[test]
fn codex_session_probe_reports_oversized_line_before_first_real_message() {
    let temp = tempdir();
    let path = temp.path().join("oversized-before-message-codex.jsonl");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        jsonl_line(json!({
            "timestamp": "2026-07-03T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "codex-oversized-before-message",
                "timestamp": "2026-07-03T12:00:00Z",
                "cwd": "/workspace",
                "originator": "codex-cli"
            }
        }))
        .as_bytes(),
    );
    bytes.extend_from_slice(&oversized_jsonl_line());
    bytes.extend_from_slice(
        jsonl_line(json!({
            "timestamp": "2026-07-03T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "message after oversized probe line"}]
            }
        }))
        .as_bytes(),
    );
    fs::write(&path, bytes).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_nativepath_record_rejections(&summary, 1, &path);
    assert_eq!(summary.skipped_events, 1);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    assert_eq!(
        store
            .search_event_hits("message after oversized probe line", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn codex_session_jsonl_keeps_valid_header_when_event_timestamp_is_malformed() {
    let temp = tempdir();
    let path = temp.path().join("bad-timestamp-codex.jsonl");
    fs::write(
        &path,
        [
            jsonl_line(json!({
                "timestamp": "2026-07-03T12:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "codex-bad-timestamp",
                    "timestamp": "2026-07-03T12:00:00Z",
                    "cwd": "/workspace",
                    "originator": "codex-cli"
                }
            })),
            jsonl_line(json!({
                "timestamp": "not-rfc3339",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [
                        {"type": "input_text", "text": "bad timestamp should not import"}
                    ]
                }
            })),
        ]
        .concat(),
    )
    .unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            capture_work_limit: crate::CaptureWorkLimit::Drain,
            inventory_observation_token: None,
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_nativepath_record_rejections(&summary, 1, &path);
    assert_eq!(summary.skipped_events, 1);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
    assert!(store
        .search_event_hits("bad timestamp should not import", 10)
        .unwrap()
        .is_empty());
}
#[test]
fn codex_session_jsonl_accepts_tool_only_session() {
    let temp = tempdir();
    let path = temp.path().join("metadata-only-codex.jsonl");
    fs::write(
        &path,
        [
            jsonl_line(json!({
                "timestamp": "2026-07-03T12:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "codex-metadata-only",
                    "timestamp": "2026-07-03T12:00:00Z",
                    "cwd": "/workspace",
                    "originator": "codex-cli"
                }
            })),
            jsonl_line(json!({
                "timestamp": "2026-07-03T12:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "shell",
                    "call_id": "call-tool-only",
                    "arguments": "{\"cmd\":\"echo tool-only\"}"
                }
            })),
        ]
        .concat(),
    )
    .unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            capture_work_limit: crate::CaptureWorkLimit::Drain,
            inventory_observation_token: None,
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    assert_eq!(store.list_sessions().unwrap().len(), 1);
    assert_eq!(
        store.search_event_hits("echo tool-only", 10).unwrap().len(),
        1
    );
}

#[test]
fn codex_session_jsonl_fast_accepts_tool_only_session() {
    let temp = tempdir();
    let path = temp.path().join("metadata-only-codex-fast.jsonl");
    fs::write(
        &path,
        [
            jsonl_line(json!({
                "timestamp": "2026-07-03T12:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "codex-fast-metadata-only",
                    "timestamp": "2026-07-03T12:00:00Z",
                    "cwd": "/workspace",
                    "originator": "codex-cli"
                }
            })),
            jsonl_line(json!({
                "timestamp": "2026-07-03T12:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "shell",
                    "call_id": "call-tool-only",
                    "arguments": "{\"cmd\":\"echo tool-only\"}"
                }
            })),
        ]
        .concat(),
    )
    .unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    assert_eq!(store.list_sessions().unwrap().len(), 1);
    assert_eq!(
        store.search_event_hits("echo tool-only", 10).unwrap().len(),
        1
    );
}

#[test]
fn codex_session_jsonl_fast_keeps_valid_header_when_event_timestamp_is_malformed() {
    let temp = tempdir();
    let path = temp.path().join("bad-timestamp-codex-fast.jsonl");
    fs::write(
        &path,
        [
            jsonl_line(json!({
                "timestamp": "2026-07-03T12:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "codex-fast-bad-timestamp",
                    "timestamp": "2026-07-03T12:00:00Z",
                    "cwd": "/workspace",
                    "originator": "codex-cli"
                }
            })),
            jsonl_line(json!({
                "timestamp": "not-rfc3339",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [
                        {"type": "input_text", "text": "fast bad timestamp should not import"}
                    ]
                }
            })),
        ]
        .concat(),
    )
    .unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_nativepath_record_rejections(&summary, 1, &path);
    assert_eq!(summary.skipped_events, 1);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
    assert!(store
        .search_event_hits("fast bad timestamp should not import", 10)
        .unwrap()
        .is_empty());
}
