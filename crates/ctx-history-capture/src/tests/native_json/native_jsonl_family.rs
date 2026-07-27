use crate::tests::support::assertions::{
    assert_event_type_count, assert_events_have_provider_citations, assert_search_hits_provider,
    assert_search_misses,
};
use crate::tests::support::fixtures::jsonl::{
    jsonl_line, oversized_jsonl_line, write_claude_smoke_fixture,
};
use crate::tests::support::paths::{provider_history_fixture, tempdir};
use crate::tests::support::provider_state::stored_provider_session_id;
use crate::{
    import_claude_projects_jsonl_tree, import_continue_cli_sessions, import_pi_session_jsonl,
    provider_source_for_path, ClaudeProjectsImportOptions, ContinueCliImportOptions,
    PiSessionImportOptions, ProviderImportSupport, ProviderSourceStatus,
};
use chrono::{DateTime, Utc};
use ctx_history_core::{AgentType, CaptureProvider, Confidence, EventType};
use ctx_history_store::Store;
use serde_json::json;
use std::fs;

#[test]
fn continue_cli_empty_history_imports_metadata_only_session() {
    let temp = tempdir();
    let root = temp.path().join("continue-sessions");
    fs::create_dir_all(&root).unwrap();
    let fixture = root.join("empty-session.json");
    fs::write(
        &fixture,
        json!({
            "sessionId": "continue-empty-session",
            "title": "Empty Continue session",
            "createdAt": "2026-07-04T16:00:00Z",
            "history": []
        })
        .to_string(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_continue_cli_sessions(
        &root,
        &mut store,
        ContinueCliImportOptions {
            source_path: Some(root.clone()),
            imported_at: "2026-07-04T16:00:00Z".parse().unwrap(),
            ..ContinueCliImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 0);
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Continue, "continue-empty-session");
    assert!(store.events_for_session(session_id).unwrap().is_empty());
}

#[test]
fn continue_cli_tool_call_redacts_raw_outputs_and_reimports_file_touches() {
    let temp = tempdir();
    let root = temp.path().join("continue-sessions");
    fs::create_dir_all(&root).unwrap();
    let raw_output = "CONTINUE_RAW_TOOL_OUTPUT_NEEDLE";
    let raw_old = "CONTINUE_RAW_DIFF_OLD_NEEDLE";
    let raw_new = "CONTINUE_RAW_DIFF_NEW_NEEDLE";
    let patch = format!(
        "*** Begin Patch\n*** Update File: src/continue_policy.rs\n- {raw_old}\n+ {raw_new}\n*** End Patch\n"
    );
    fs::write(
        root.join("continue-tool-boundary.json"),
        json!({
            "sessionId": "continue-tool-boundary",
            "title": "Continue tool policy",
            "createdAt": "2026-07-04T16:00:00Z",
            "history": [
                {
                    "id": "continue-user-1",
                    "timestamp": "2026-07-04T16:00:00Z",
                    "message": {
                        "role": "user",
                        "content": "continue tool policy oracle prompt"
                    }
                },
                {
                    "id": "continue-tool-1",
                    "timestamp": "2026-07-04T16:00:01Z",
                    "message": {
                        "role": "assistant",
                        "content": ""
                    },
                    "toolCallStates": [
                        {
                            "status": "done",
                            "toolCall": {
                                "function": {
                                    "name": "apply_patch",
                                    "arguments": patch
                                }
                            },
                            "output": raw_output
                        }
                    ]
                }
            ]
        })
        .to_string(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_continue_cli_sessions(
        &root,
        &mut store,
        ContinueCliImportOptions {
            source_path: Some(root.clone()),
            imported_at: "2026-07-04T16:05:00Z".parse().unwrap(),
            ..ContinueCliImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 2);
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Continue, "continue-tool-boundary");
    let events = store.events_for_session(session_id).unwrap();
    let tool = events
        .iter()
        .find(|event| event.event_type == EventType::ToolCall)
        .expect("tool call metadata event imported");
    let rendered_tool = serde_json::to_string(tool).unwrap();
    assert!(rendered_tool.contains("apply_patch"));
    assert!(!rendered_tool.contains(raw_output));
    assert!(!rendered_tool.contains(raw_old));
    assert!(!rendered_tool.contains(raw_new));
    assert_eq!(events.len(), 2);
    assert_event_type_count(&events, EventType::ToolCall, 1);
    assert_event_type_count(&events, EventType::ToolOutput, 0);
    assert_event_type_count(&events, EventType::CommandOutput, 0);
    assert!(store
        .search_event_hits("continue tool policy oracle prompt", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Continue)));
    assert!(store.search_event_hits(raw_output, 10).unwrap().is_empty());
    assert!(store.search_event_hits(raw_old, 10).unwrap().is_empty());
    assert!(store.search_event_hits(raw_new, 10).unwrap().is_empty());
    assert!(store
        .export_archive()
        .unwrap()
        .files_touched
        .iter()
        .any(|file| {
            file.sync.metadata["provider"].as_str() == Some(CaptureProvider::Continue.as_str())
                && file.path == "src/continue_policy.rs"
                && file.confidence == Confidence::Explicit
        }));

    let second = import_continue_cli_sessions(
        &root,
        &mut store,
        ContinueCliImportOptions {
            source_path: Some(root.clone()),
            imported_at: "2026-07-04T16:06:00Z".parse().unwrap(),
            ..ContinueCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 2);
}

#[test]
fn native_pi_fixture_imports_event_types_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("pi-session.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Pi, fixture.clone());
    assert_eq!(source.source_format, "pi_session_jsonl");
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_pi_session_jsonl(
        &fixture,
        &mut store,
        PiSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T16:10:00Z".parse().unwrap(),
            ..PiSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 4);

    let session_id = stored_provider_session_id(&store, CaptureProvider::Pi, "pi-session-docs-1");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 4);
    assert_event_type_count(&events, EventType::Message, 2);
    assert_event_type_count(&events, EventType::ToolCall, 1);
    assert_event_type_count(&events, EventType::ToolOutput, 0);
    assert_event_type_count(&events, EventType::CommandOutput, 0);
    assert_event_type_count(&events, EventType::Summary, 1);
    assert_events_have_provider_citations(&store, &events);

    assert_search_hits_provider(
        &store,
        "Inspect the provider metadata rows",
        CaptureProvider::Pi,
    );
    assert_search_hits_provider(
        &store,
        "Provider metadata import fixture",
        CaptureProvider::Pi,
    );
    assert_search_misses(&store, "tests passed");
    assert_search_misses(&store, "ok token=fixture-secret");

    let second = import_pi_session_jsonl(
        &fixture,
        &mut store,
        PiSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T16:15:00Z".parse().unwrap(),
            ..PiSessionImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 0);
    assert_eq!(second.skipped_events, 0);
}

#[test]
fn native_pi_malformed_file_imports_valid_records_and_reports_rejections() {
    let temp = tempdir();
    let fixture = provider_history_fixture("pi-malformed-mixed.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_pi_session_jsonl(
        &fixture,
        &mut store,
        PiSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            ..PiSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 2, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    let sessions = store.list_sessions().unwrap();
    assert_eq!(
        sessions.len(),
        1,
        "unexpected Pi sessions: {:?}",
        sessions
            .iter()
            .map(|session| (&session.id, &session.external_session_id))
            .collect::<Vec<_>>()
    );
    assert_eq!(store.search_event_hits("after", 10).unwrap().len(), 1);
}

#[test]
fn native_claude_manifested_files_import_parent_and_subagent() {
    let temp = tempdir();
    let root = write_claude_smoke_fixture(&temp);
    let parent_path = root.join("-workspace/claude-native-parent.jsonl");
    let child_path = root.join("-workspace/claude-native-parent/subagents/agent-scout.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let options = ClaudeProjectsImportOptions {
        machine_id: "test-machine".into(),
        source_path: Some(root.clone()),
        imported_at: DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        ..ClaudeProjectsImportOptions::default()
    };
    let mut summary =
        import_claude_projects_jsonl_tree(&parent_path, &mut store, options.clone()).unwrap();
    summary.merge(
        import_claude_projects_jsonl_tree(&child_path, &mut store, options.clone()).unwrap(),
    );

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 4);
    assert_eq!(summary.imported_edges, 1);
    let parent_id =
        stored_provider_session_id(&store, CaptureProvider::Claude, "claude-native-parent");
    let child_id = stored_provider_session_id(
        &store,
        CaptureProvider::Claude,
        "claude-native-parent/subagents/agent-scout",
    );
    let child = store.get_session(child_id).unwrap();
    assert_eq!(child.parent_session_id, Some(parent_id));
    assert_eq!(child.agent_type, AgentType::Subagent);
    let events = store.events_for_session(parent_id).unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert_event_type_count(&events, EventType::ToolOutput, 0);
    assert_event_type_count(&events, EventType::CommandOutput, 0);
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("tool-1"));
    assert!(!rendered.contains("abcdef0123456789abcdef0123456789abcdef01"));
    assert!(!rendered.contains("https://github.com/ctxrs/ctx/pull/123"));
    assert!(!rendered.contains("token=claude-secret"));
    assert!(!rendered.contains("fragment"));
    assert!(!rendered.contains("CLAUDE_RESULT_NARRATIVE_MUST_NOT_RETAIN"));
    let sources = store.list_capture_sources().unwrap();
    assert!(sources.iter().all(|source| {
        source.descriptor.source_root.as_deref() == Some(root.to_string_lossy().as_ref())
    }));
    assert!(sources.iter().any(|source| {
        source.descriptor.raw_source_path.as_deref() == Some(parent_path.to_string_lossy().as_ref())
    }));
    assert!(sources.iter().any(|source| {
        source.descriptor.raw_source_path.as_deref() == Some(child_path.to_string_lossy().as_ref())
    }));

    let replay =
        import_claude_projects_jsonl_tree(&parent_path, &mut store, options.clone()).unwrap();
    assert_eq!(replay.imported_sessions, 0);
    assert_eq!(replay.imported_events, 0);
    assert_eq!(replay.skipped_sessions, 1);
    assert_eq!(replay.skipped_events, 0);

    {
        use std::io::Write;

        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&parent_path)
            .unwrap();
        writeln!(
            file,
            "{}",
            json!({
                "sessionId": "claude-native-parent",
                "timestamp": "2026-06-24T12:00:05Z",
                "cwd": "/workspace",
                "version": "test",
                "type": "assistant",
                "message": {"role": "assistant", "content": "appended"},
                "uuid": "claude-parent-4"
            })
        )
        .unwrap();
    }
    let appended = import_claude_projects_jsonl_tree(&parent_path, &mut store, options).unwrap();
    assert_eq!(appended.failed, 0, "{:?}", appended.failures);
    assert_eq!(appended.imported_events, 1);
    assert_eq!(store.events_for_session(parent_id).unwrap().len(), 3);
}

#[test]
fn native_claude_tree_streams_parent_and_subagent_in_one_call() {
    let temp = tempdir();
    let root = write_claude_smoke_fixture(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_claude_projects_jsonl_tree(
        &root,
        &mut store,
        ClaudeProjectsImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(root.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            ..ClaudeProjectsImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 4);
    assert_eq!(summary.imported_edges, 1);
    let parent_id =
        stored_provider_session_id(&store, CaptureProvider::Claude, "claude-native-parent");
    let child_id = stored_provider_session_id(
        &store,
        CaptureProvider::Claude,
        "claude-native-parent/subagents/agent-scout",
    );
    assert_eq!(
        store.get_session(child_id).unwrap().parent_session_id,
        Some(parent_id)
    );
}

#[test]
fn native_claude_manifested_file_crosses_a_batch_boundary_and_replays() {
    let temp = tempdir();
    let root = temp.path().join("claude/projects");
    let path = root.join("-workspace/claude-batched.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut transcript = String::new();
    for index in 0..65 {
        transcript.push_str(&jsonl_line(json!({
            "sessionId": "claude-batched",
            "timestamp": format!("2026-06-24T12:01:{:02}Z", index.min(59)),
            "cwd": "/workspace",
            "version": "test",
            "type": if index % 2 == 0 { "user" } else { "assistant" },
            "message": {
                "role": if index % 2 == 0 { "user" } else { "assistant" },
                "content": format!("bounded Claude event {index}")
            },
            "uuid": format!("claude-batched-{index}")
        })));
    }
    fs::write(&path, transcript).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let options = ClaudeProjectsImportOptions {
        source_path: Some(root),
        imported_at: "2026-06-24T12:02:00Z".parse().unwrap(),
        ..ClaudeProjectsImportOptions::default()
    };

    let first = import_claude_projects_jsonl_tree(&path, &mut store, options.clone()).unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 65);

    let replay = import_claude_projects_jsonl_tree(&path, &mut store, options).unwrap();
    assert_eq!(replay.imported_sessions, 0);
    assert_eq!(replay.imported_events, 0);
    assert_eq!(replay.skipped_sessions, 1);
    assert_eq!(replay.skipped_events, 0);
}

#[test]
fn native_claude_caps_core_text_with_verified_retention() {
    let temp = tempdir();
    let root = temp.path().join("claude/projects/-workspace");
    fs::create_dir_all(&root).unwrap();
    let text = "x".repeat(20_000);
    fs::write(
        root.join("claude-bounded-text.jsonl"),
        jsonl_line(json!({
            "sessionId": "claude-bounded-text",
            "timestamp": "2026-07-04T14:00:00Z",
            "cwd": "/workspace",
            "version": "test",
            "type": "user",
            "message": {"role": "user", "content": text},
            "uuid": "claude-bounded-text-message"
        })),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let path = root.join("claude-bounded-text.jsonl");
    let summary = import_claude_projects_jsonl_tree(
        &path,
        &mut store,
        ClaudeProjectsImportOptions {
            source_path: Some(temp.path().join("claude/projects")),
            imported_at: "2026-07-04T14:30:00Z".parse().unwrap(),
            ..ClaudeProjectsImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Claude, "claude-bounded-text");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 1);
    let payload = &events[0].payload;
    assert_eq!(payload["body"].as_str().unwrap().chars().count(), 16_000);
    assert!(payload["body_sha256"].as_str().is_some());
    assert_eq!(payload["text_retention"]["mode"], "bounded");
    assert_eq!(payload["text_retention"]["limit_chars"], 16_000);
    assert_eq!(payload["text_retention"]["truncated"], true);
    assert!(events[0]
        .sync
        .metadata
        .get("verified_content_locators_v1")
        .is_some());
}

#[test]
fn native_claude_empty_manifested_jsonl_imports_metadata_only_session() {
    let temp = tempdir();
    let root = temp.path().join("claude/projects/-workspace");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("empty.jsonl");
    fs::write(&path, "").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_claude_projects_jsonl_tree(
        &path,
        &mut store,
        ClaudeProjectsImportOptions {
            ..ClaudeProjectsImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 0);
    let session_id = stored_provider_session_id(&store, CaptureProvider::Claude, "empty");
    assert!(store.events_for_session(session_id).unwrap().is_empty());
}

#[test]
fn native_claude_manifested_file_advances_past_oversized_jsonl_record() {
    let temp = tempdir();
    let root = temp.path().join("claude/projects/-workspace");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("claude-oversized.jsonl");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        jsonl_line(json!({
            "sessionId": "claude-oversized",
            "timestamp": "2026-07-04T14:00:00Z",
            "cwd": "/workspace",
            "version": "test",
            "type": "user",
            "message": {"role": "user", "content": "before oversized claude"},
            "uuid": "claude-oversized-before"
        }))
        .as_bytes(),
    );
    bytes.extend_from_slice(&oversized_jsonl_line());
    bytes.extend_from_slice(
        jsonl_line(json!({
            "sessionId": "claude-oversized",
            "timestamp": "2026-07-04T14:00:01Z",
            "cwd": "/workspace",
            "version": "test",
            "type": "assistant",
            "message": {"role": "assistant", "content": "after oversized claude"},
            "uuid": "claude-oversized-after"
        }))
        .as_bytes(),
    );
    fs::write(&path, bytes).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_claude_projects_jsonl_tree(
        &path,
        &mut store,
        ClaudeProjectsImportOptions {
            source_path: Some(temp.path().join("claude/projects")),
            imported_at: "2026-07-04T14:30:00Z".parse().unwrap(),
            ..ClaudeProjectsImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.failures[0].line, 2);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Claude, "claude-oversized");
    let rendered = serde_json::to_string(&store.events_for_session(session_id).unwrap()).unwrap();
    assert!(rendered.contains("before oversized claude"));
    assert!(rendered.contains("after oversized claude"));
}
