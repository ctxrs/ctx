use crate::provider::codex::events::{
    codex_output_text, codex_result_content, codex_tool_output_event,
};
use crate::provider::file_touches::{
    visit_provider_file_touch_drafts_with_limit, FileTouchDraft,
    MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
};
use crate::provider::importer::provider_command_run;
use crate::tests::support::paths::{provider_history_fixture, tempdir};
use crate::tests::support::provider_state::stored_provider_session_id;
use crate::{import_codex_session_jsonl, import_codex_session_tree, CodexSessionImportOptions};
use chrono::{DateTime, Utc};
use ctx_history_core::{new_id, CaptureProvider, EventType, Fidelity, FileChangeKind};
use ctx_history_store::Store;
use serde_json::{json, Value};
use std::{borrow::Cow, fs};

#[test]
fn provider_command_run_rejects_negative_duration() {
    let err = provider_command_run(
        CaptureProvider::Codex,
        "duration-session",
        new_id(),
        new_id(),
        None,
        None,
        EventType::CommandOutput,
        "2026-07-03T12:00:00Z".parse().unwrap(),
        Fidelity::Imported,
        0,
        &json!({
            "command": "cargo test",
            "duration_ms": -1
        }),
        "event-hash",
    )
    .unwrap_err();

    assert!(err.to_string().contains("duration_ms must be nonnegative"));
}

#[test]
fn codex_session_tree_keeps_calls_and_elides_successful_results() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-rich-sessions");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codex_session_tree(
        &fixture,
        &mut store,
        CodexSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-24T01:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 5);
    assert_eq!(summary.skipped_events, 3);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Codex, "codex-rich-session");
    let events = store.events_for_session(session_id).unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall
            && event.payload.to_string().contains("apply_patch")));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::Summary
            && event
                .payload
                .to_string()
                .contains("sample command completed")));

    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("cargo test -p sample -- --token fixture-secret-token"));
    assert!(!rendered.contains("unit tests passed in /workspace/ctx-rich-fixture"));
    assert!(!rendered.contains("*** Begin Patch"));
    assert!(!rendered.contains("old_fixture"));
    assert!(!rendered.contains("new_fixture"));
    assert!(!rendered.contains("patch_apply_end"));
    assert!(!rendered.contains("opaque-private-reasoning-payload"));
}

#[test]
fn codex_current_nested_legacy_and_wait_outputs_keep_stable_call_linkage() {
    let temp = tempdir();
    let fixture = temp.path().join("codex-output-shapes.jsonl");
    let lines = [
        json!({
            "timestamp": "2026-07-18T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "codex-output-shapes",
                "timestamp": "2026-07-18T12:00:00Z",
                "cwd": "/workspace/ctx"
            }
        }),
        json!({
            "timestamp": "2026-07-18T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call",
                "name": "exec",
                "call_id": "call-current",
                "input": "git commit -m fixture"
            }
        }),
        json!({
            "timestamp": "2026-07-18T12:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call_output",
                "call_id": "call-current",
                "output": [
                    {"type": "input_text", "text": "Script completed\nWall time 0.1 seconds\nOutput:\n"},
                    {"type": "input_text", "text": "[codex/zig-conformance db817fa] test(zig): harden semantic acceptance\n"}
                ]
            }
        }),
        json!({
            "timestamp": "2026-07-18T12:00:03Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "shell",
                "call_id": "call-legacy",
                "arguments": "git status --short"
            }
        }),
        json!({
            "timestamp": "2026-07-18T12:00:04Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "call-legacy",
                "output": "Process exited with code 0\nOutput:\nclean\n"
            }
        }),
        json!({
            "timestamp": "2026-07-18T12:00:05Z",
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call",
                "name": "wait",
                "call_id": "call-wait",
                "input": "cell-123"
            }
        }),
        json!({
            "timestamp": "2026-07-18T12:00:06Z",
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call_output",
                "call_id": "call-wait",
                "output": [{"type": "input_text", "text": "Script running with cell ID cell-123"}]
            }
        }),
    ];
    fs::write(
        &fixture,
        lines
            .iter()
            .map(|line| serde_json::to_string(line).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_codex_session_jsonl(&fixture, &mut store, CodexSessionImportOptions::default())
            .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 3);
    assert_eq!(summary.skipped_events, 3);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Codex, "codex-output-shapes");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 3);
    assert!(events
        .iter()
        .all(|event| event.event_type == EventType::ToolCall));
    for call_id in ["call-current", "call-legacy", "call-wait"] {
        assert!(events.iter().any(|event| {
            event
                .payload
                .pointer("/body/call_id")
                .and_then(Value::as_str)
                == Some(call_id)
        }));
    }
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(!rendered.contains("Script completed"));
    assert!(!rendered.contains("harden semantic acceptance"));
    assert!(!rendered.contains("Process exited with code 0"));
}

#[test]
fn codex_current_nested_success_has_no_core_projection() {
    let payload = json!({
        "type": "custom_tool_call_output",
        "call_id": "call-bounded",
        "output": [
            {"type": "input_text", "text": "Script completed\nWall time 0.1 seconds\nOutput:\n"},
            {"type": "input_text", "text": format!("[branch db817fa] fixture\n{}", "x".repeat(8_000))}
        ]
    });
    let event = codex_tool_output_event(
        &payload,
        12,
        DateTime::parse_from_rfc3339("2026-07-18T12:00:02Z")
            .unwrap()
            .with_timezone(&Utc),
        &std::collections::BTreeMap::new(),
    );
    let normalized = codex_output_text(&payload["output"]);
    assert!(normalized.contains("[branch db817fa]"));
    assert!(event.is_none());
}

#[test]
fn result_content_borrows_plain_strings_and_normalizes_structured_output() {
    let plain_payload = json!({
        "type": "function_call_output",
        "call_id": "call-plain",
        "output": "plain output body"
    });
    assert!(matches!(
        codex_result_content(&plain_payload),
        Some(Cow::Borrowed("plain output body"))
    ));

    let structured_payload = json!({
        "type": "custom_tool_call_output",
        "call_id": "call-structured",
        "output": [{"type": "input_text", "text": "structured output"}]
    });
    assert!(matches!(
        codex_result_content(&structured_payload),
        Some(Cow::Owned(_))
    ));
}

#[test]
fn codex_structured_failed_tool_output_keeps_outcome_without_body() {
    let payload = json!({
        "type": "function_call_output",
        "call_id": "call-structured-failure",
        "output": {
            "message": {
                "exitCode": 1,
                "output": "structured failed output oracle"
            }
        }
    });
    let event = codex_tool_output_event(
        &payload,
        12,
        DateTime::parse_from_rfc3339("2026-06-24T01:00:04.500Z")
            .unwrap()
            .with_timezone(&Utc),
        &std::collections::BTreeMap::new(),
    )
    .expect("structured failed output should be retained");

    assert_eq!(event.event_type, EventType::ToolOutput);
    let rendered = event.payload.to_string();
    assert!(!rendered.contains("structured failed output oracle"));
    assert_eq!(event.payload["exit_code"], 1);
    assert!(event.payload.get("result_content_ref").is_none());
    assert!(event.payload.get("result_evidence").is_none());
}

#[test]
fn codex_failed_diff_output_keeps_only_bounded_outcome() {
    let payload = json!({
        "type": "function_call_output",
        "call_id": "call-failed-diff",
        "output": "Process exited with code 1\nOutput:\ndiff --git a/src/lib.rs b/src/lib.rs\n@@\n-old raw diff\n+new raw diff\n"
    });
    let event = codex_tool_output_event(
        &payload,
        13,
        DateTime::parse_from_rfc3339("2026-06-24T01:00:05.000Z")
            .unwrap()
            .with_timezone(&Utc),
        &std::collections::BTreeMap::new(),
    )
    .expect("failed diff output should keep a diagnostic event");

    let rendered = event.payload.to_string();
    assert_eq!(event.payload["exit_code"], 1);
    assert!(event.payload.get("result_content_ref").is_none());
    assert!(!rendered.contains("diff --git"));
    assert!(!rendered.contains("old raw diff"));
    assert!(!rendered.contains("new raw diff"));
}

#[test]
fn codex_nested_failed_diff_output_keeps_only_bounded_outcome() {
    let payload = json!({
        "type": "function_call_output",
        "call_id": "call-nested-failed-diff",
        "output": {
            "message": {
                "exitCode": 1,
                "output": "@@ -1 +1\n-old nested diff\n+new nested diff\n"
            }
        }
    });
    let event = codex_tool_output_event(
        &payload,
        14,
        DateTime::parse_from_rfc3339("2026-06-24T01:00:05.500Z")
            .unwrap()
            .with_timezone(&Utc),
        &std::collections::BTreeMap::new(),
    )
    .expect("nested failed diff output should keep a diagnostic event");

    let rendered = event.payload.to_string();
    assert_eq!(event.payload["exit_code"], 1);
    assert!(event.payload.get("result_content_ref").is_none());
    assert!(!rendered.contains("old nested diff"));
    assert!(!rendered.contains("new nested diff"));
}

#[test]
fn codex_default_policy_persists_file_touches_without_raw_patch_text() {
    let temp = tempdir();
    let root = temp.path().join("codex-sessions/2026/06/24");
    fs::create_dir_all(&root).unwrap();
    let fixture = root.join("search-file-touch.jsonl");
    fs::write(
            &fixture,
            concat!(
                "{\"timestamp\":\"2026-06-24T01:00:00.000Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"codex-search-file-touch\",\"cwd\":\"/workspace/ctx\"}}\n",
                "{\"timestamp\":\"2026-06-24T01:00:01.000Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"Please update the CLI.\"}]}}\n",
                "{\"timestamp\":\"2026-06-24T01:00:02.000Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"name\":\"apply_patch\",\"input\":\"*** Begin Patch\\n*** Update File: crates/ctx-cli/src/main.rs\\n@@\\n-old\\n+new\\n*** End Patch\",\"call_id\":\"call-patch\",\"status\":\"completed\"}}\n",
            ),
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codex_session_tree(
        temp.path().join("codex-sessions"),
        &mut store,
        CodexSessionImportOptions {
            source_path: Some(temp.path().join("codex-sessions")),
            imported_at: "2026-06-24T02:00:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 2);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Codex, "codex-search-file-touch");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event_type, EventType::Message);
    assert_eq!(events[1].event_type, EventType::ToolCall);
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("file touches: modified:crates/ctx-cli/src/main.rs"));
    assert!(!rendered.contains("*** Begin Patch"));
    assert!(!rendered.contains("-old"));
    assert!(!rendered.contains("+new"));

    let archive = store.export_archive().unwrap();
    let touched = archive
        .files_touched
        .iter()
        .find(|file| file.path == "crates/ctx-cli/src/main.rs")
        .expect("apply_patch should create file touch metadata");
    assert_eq!(touched.change_kind, Some(FileChangeKind::Modified));
    assert!(touched.event_id.is_some());
    assert_eq!(touched.history_record_id, None);
}

#[test]
fn codex_default_policy_omits_non_patch_edit_tool_arguments() {
    let temp = tempdir();
    let root = temp.path().join("codex-sessions/2026/06/24");
    fs::create_dir_all(&root).unwrap();
    let fixture = root.join("edit-tool.jsonl");
    fs::write(
        &fixture,
        concat!(
            "{\"timestamp\":\"2026-06-24T01:00:00.000Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"codex-edit-tool\",\"cwd\":\"/workspace/ctx\"}}\n",
            "{\"timestamp\":\"2026-06-24T01:00:01.000Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"Please edit the file.\"}]}}\n",
            "{\"timestamp\":\"2026-06-24T01:00:02.000Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"name\":\"edit_file\",\"arguments\":{\"path\":\"src/edit_tool.rs\",\"old_string\":\"old-edit-tool-secret\",\"new_string\":\"new-edit-tool-secret\"},\"call_id\":\"call-edit\"}}\n",
        ),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codex_session_tree(
        temp.path().join("codex-sessions"),
        &mut store,
        CodexSessionImportOptions {
            source_path: Some(temp.path().join("codex-sessions")),
            imported_at: "2026-06-24T02:00:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 2);

    let session_id = stored_provider_session_id(&store, CaptureProvider::Codex, "codex-edit-tool");
    let events = store.events_for_session(session_id).unwrap();
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("file touches:"));
    assert!(rendered.contains("src/edit_tool.rs"));
    assert!(!rendered.contains("old-edit-tool-secret"));
    assert!(!rendered.contains("new-edit-tool-secret"));
    assert!(store
        .search_event_hits("old-edit-tool-secret", 10)
        .unwrap()
        .is_empty());

    let archive = store.export_archive().unwrap();
    assert!(archive
        .files_touched
        .iter()
        .any(|file| file.path == "src/edit_tool.rs"));
}

#[test]
fn structured_file_touch_extractor_reads_nested_provider_paths() {
    let antigravity = serde_json::json!({
        "type": "CODE_ACTION",
        "tool_calls": [{
            "name": "write_to_file",
            "args": {
                "TargetFile": "/workspace/demo/README.md",
                "CodeContent": "# Demo\n"
            }
        }]
    });
    let antigravity_touches = file_touch_drafts(&antigravity);
    assert_eq!(antigravity_touches[0].path, "/workspace/demo/README.md");
    assert_eq!(
        antigravity_touches[0].change_kind,
        Some(FileChangeKind::Created)
    );
}

#[test]
fn structured_file_touch_extractor_covers_provider_tool_shapes() {
    for (provider, raw, expected_path) in [
        (
            CaptureProvider::Claude,
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "content": [{
                        "type": "tool_use",
                        "name": "Edit",
                        "input": {"file_path": "src/claude_file.rs"}
                    }]
                }
            }),
            "src/claude_file.rs",
        ),
        (
            CaptureProvider::OpenCode,
            serde_json::json!({
                "content": [{
                    "type": "tool",
                    "name": "write",
                    "input": {"file": "src/opencode_file.rs"}
                }]
            }),
            "src/opencode_file.rs",
        ),
        (
            CaptureProvider::Gemini,
            serde_json::json!({
                "type": "gemini",
                "toolCalls": [{
                    "name": "write_file",
                    "args": {"path": "src/gemini_file.rs", "content": "proof"}
                }]
            }),
            "src/gemini_file.rs",
        ),
        (
            CaptureProvider::CopilotCli,
            serde_json::json!({
                "type": "tool.execution_start",
                "data": {
                    "toolName": "write_file",
                    "args": {"path": "src/copilot_file.rs"}
                }
            }),
            "src/copilot_file.rs",
        ),
        (
            CaptureProvider::FactoryAiDroid,
            serde_json::json!({
                "type": "message",
                "content": [{
                    "type": "tool_use",
                    "name": "write_file",
                    "input": {"path": "src/droid_file.rs"}
                }]
            }),
            "src/droid_file.rs",
        ),
        (
            CaptureProvider::ForgeCode,
            serde_json::json!({
                "message": {
                    "text": {
                        "tool_calls": [{
                            "name": "write",
                            "arguments": {
                                "path": "src/forge_file.rs",
                                "content": "proof"
                            }
                        }]
                    }
                }
            }),
            "src/forge_file.rs",
        ),
    ] {
        let touches = file_touch_drafts(&raw);
        assert_eq!(
            touches.first().map(|file| file.path.as_str()),
            Some(expected_path),
            "{provider:?} should extract an explicit tool file path"
        );
    }
}

fn file_touch_drafts(raw: &Value) -> Vec<FileTouchDraft> {
    let mut drafts = Vec::new();
    let outcome = visit_provider_file_touch_drafts_with_limit(
        raw,
        true,
        MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
        |(_, draft)| {
            drafts.push(draft);
            Ok::<(), std::convert::Infallible>(())
        },
    )
    .expect("an infallible file-touch sink cannot fail");
    assert!(!outcome.limit_exceeded());
    drafts
}
