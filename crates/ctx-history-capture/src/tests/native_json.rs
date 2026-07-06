use super::support::*;

#[test]

fn provider_fixture_replay_supports_claude_cursor_metadata() {
    let temp = tempdir();
    let fixture = provider_fixture("claude.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_provider_fixture_jsonl(&fixture, &mut store, fixed_import_options(fixture.clone()))
            .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Claude, "claude-session-1");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events[1].event_type, EventType::Summary);
    assert_eq!(
        events[1].sync.metadata["cursor"].as_str(),
        Some("claude-cursor-1")
    );
    assert_eq!(events[1].payload["provider_event_index"].as_u64(), Some(1));
}

#[test]
fn provider_fixture_replay_supports_opencode_fixture() {
    let temp = tempdir();
    let fixture = provider_fixture("opencode.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_provider_fixture_jsonl(&fixture, &mut store, fixed_import_options(fixture.clone()))
            .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 3);
    assert_eq!(summary.imported_edges, 1);
    let parent_id =
        stored_provider_session_id(&store, CaptureProvider::OpenCode, "opencode-session-1");
    let child_id = stored_provider_session_id(
        &store,
        CaptureProvider::OpenCode,
        "opencode-session-1-scout",
    );
    let parent = store.get_session(parent_id).unwrap();
    let child = store.get_session(child_id).unwrap();
    assert_eq!(parent.provider, CaptureProvider::OpenCode);
    assert_eq!(child.parent_session_id, Some(parent_id));
    assert_eq!(child.agent_type, AgentType::Subagent);
    assert_eq!(store.events_for_session(parent_id).unwrap().len(), 2);
    assert_eq!(store.events_for_session(child_id).unwrap().len(), 1);
}

#[test]
fn native_pi_malformed_file_is_atomic_without_partial_failures() {
    let temp = tempdir();
    let fixture = provider_history_fixture("pi-malformed-partial.jsonl");
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
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
    assert!(store
        .search_event_hits("after malformed line", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn native_claude_projects_imports_jsonl_tree() {
    let temp = tempdir();
    let fixture = write_claude_smoke_fixture(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_claude_projects_jsonl_tree(
        &fixture,
        &mut store,
        ClaudeProjectsImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..ClaudeProjectsImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 5);
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
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));
}

#[test]
fn antigravity_native_history_imports_transcripts_and_preserves_previews() {
    let temp = tempdir();
    let fixture = provider_history_fixture("antigravity/v1/brain");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_antigravity_cli_history(
        &fixture,
        &mut store,
        AntigravityCliImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            imported_at: "2026-06-24T14:00:00Z".parse().unwrap(),
            ..AntigravityCliImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.failures[0].line, 3);
    assert!(summary.failures[0].error.contains("malformed JSONL"));
    assert_eq!(summary.imported_sessions, 4);
    assert_eq!(summary.imported_events, 11);

    let success_session =
        stored_provider_session_id(&store, CaptureProvider::Antigravity, "agy-success");
    let success = store.events_for_session(success_session).unwrap();
    assert_eq!(success.len(), 3);
    let tool = success
        .iter()
        .find(|event| event.event_type == EventType::ToolCall)
        .unwrap();
    assert!(tool.payload["body"]["tool_calls"].is_array());
    assert!(tool.payload["body"]["tool_calls"][0]["args"].is_object());
    assert_eq!(
        tool.payload["body"]["tool_calls"][0]["args"]["CodeContent"].as_str(),
        Some("# Demo\n\nThis is a sanitized Antigravity fixture.\n")
    );
    let archive = store.export_archive().unwrap();
    assert!(archive.files_touched.iter().any(|file| {
        file.path == "/workspace/demo/README.md" && file.confidence == Confidence::High
    }));
    assert_eq!(
        tool.sync.metadata["metadata"]["source_format"].as_str(),
        Some(ANTIGRAVITY_CLI_SOURCE_FORMAT)
    );
    let source_paths: Vec<String> = store
        .list_capture_sources()
        .unwrap()
        .into_iter()
        .filter_map(|source| source.descriptor.raw_source_path)
        .collect();
    assert!(source_paths
        .iter()
        .any(|path| path.contains("transcript_full.jsonl")));

    let future_session =
        stored_provider_session_id(&store, CaptureProvider::Antigravity, "agy-future");
    let future = store.events_for_session(future_session).unwrap();
    assert!(future
        .iter()
        .any(|event| event.event_type == EventType::Notice
            && event.payload["body"]["entry_type"] == "FUTURE_EVENT_KIND"));
    let rendered = serde_json::to_string(&future).unwrap();
    assert!(rendered.contains("ghp_1234567890abcdef"));
    assert!(rendered.contains("/home/example/private.txt"));
    assert!(!rendered.contains("[REDACTED"));
}

#[test]
fn native_windsurf_fixture_imports_searches_reimports_and_file_touches() {
    let temp = tempdir();
    let fixture = provider_history_fixture("windsurf/transcripts");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Windsurf, fixture.clone());
    assert_eq!(
        source.source_format,
        "windsurf_cascade_hook_transcript_jsonl_tree"
    );
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert!(source.import_support.is_auto_importable());
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_windsurf_cascade_hook_transcripts(
        &fixture,
        &mut store,
        WindsurfCascadeHookImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            imported_at: "2026-06-24T14:00:00Z".parse().unwrap(),
            ..WindsurfCascadeHookImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{first:?}");
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 5);
    assert!(store
        .search_event_hits("windsurf cascade hook oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Windsurf)));
    assert!(store
        .search_event_hits("windsurf unknown typed payload oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Windsurf)));

    let session_id = stored_provider_session_id(
        &store,
        CaptureProvider::Windsurf,
        "windsurf-hook-trajectory-1",
    );
    let events = store.events_for_session(session_id).unwrap();
    let code_action = events
        .iter()
        .find(|event| event.event_type == EventType::ToolCall)
        .unwrap();
    assert_eq!(
        code_action.payload["body"]["body"]["code_action"]["path"].as_str(),
        Some("src/windsurf_hook_oracle.py")
    );
    assert_eq!(
        code_action.payload["body"]["body"]["code_action"]["new_content"]["redacted"].as_str(),
        Some("sensitive_transcript_field")
    );
    assert!(!code_action.payload.to_string().contains("print("));

    let archive = store.export_archive().unwrap();
    assert!(archive.files_touched.iter().any(|file| {
        file.path == "src/windsurf_hook_oracle.py" && file.confidence == Confidence::High
    }));

    let second = import_windsurf_cascade_hook_transcripts(
        &fixture,
        &mut store,
        WindsurfCascadeHookImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            imported_at: "2026-06-24T14:05:00Z".parse().unwrap(),
            ..WindsurfCascadeHookImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{second:?}");
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 5);
}

#[test]
fn native_qoder_fixture_imports_documented_transcript_jsonl() {
    let temp = tempdir();
    let fixture = provider_history_fixture("qoder/projects");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Qoder, fixture.clone());
    assert_eq!(source.source_format, "qoder_transcript_jsonl_tree");
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_qoder_history(
        &fixture,
        &mut store,
        QoderImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            imported_at: "2026-07-01T12:00:00Z".parse().unwrap(),
            ..QoderImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{first:?}");
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 7);
    assert!(store
        .search_event_hits("qoder jsonl oracle prompt", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Qoder)));
    assert!(store
        .search_event_hits("qoder native import ok", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Qoder)));

    let session_id = stored_provider_session_id(&store, CaptureProvider::Qoder, "qoder-session-1");
    let events = store.events_for_session(session_id).unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.event_type == EventType::Message
                && event.role == Some(EventRole::User))
    );
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall
            && event.role == Some(EventRole::Assistant)));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput
            && event.role == Some(EventRole::User)
            && event.payload["body"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("qoder import ok"))));

    let second = import_qoder_history(
        &fixture,
        &mut store,
        QoderImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            imported_at: "2026-07-01T12:05:00Z".parse().unwrap(),
            ..QoderImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{second:?}");
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 7);
}

#[test]
fn native_windsurf_reports_malformed_jsonl_partially() {
    let temp = tempdir();
    let fixture = provider_history_fixture("windsurf/malformed/transcripts");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_windsurf_cascade_hook_transcripts(
        &fixture,
        &mut store,
        WindsurfCascadeHookImportOptions {
            allow_partial_failures: true,
            imported_at: "2026-06-24T14:00:00Z".parse().unwrap(),
            ..WindsurfCascadeHookImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{summary:?}");
    assert_eq!(summary.failures[0].line, 2);
    assert!(summary.failures[0].error.contains("malformed JSONL"));
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    assert!(store
        .search_event_hits("windsurf malformed after bad oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Windsurf)));
}

#[test]
fn native_claude_projects_reports_malformed_jsonl() {
    let temp = tempdir();
    let fixture = temp.path().join("claude-malformed/projects/-workspace");
    fs::create_dir_all(&fixture).unwrap();
    fs::write(
            fixture.join("claude-malformed.jsonl"),
            concat!(
                "{\"sessionId\":\"claude-malformed\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"valid\"}}\n",
                "{\"sessionId\":\"claude-malformed\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"partial\"}]\n",
            ),
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_claude_projects_jsonl_tree(
        &fixture,
        &mut store,
        ClaudeProjectsImportOptions {
            allow_partial_failures: true,
            ..ClaudeProjectsImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    assert!(summary.failures[0].error.contains("malformed JSONL"));
}

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
            allow_partial_failures: true,
            imported_at: "2026-06-30T12:10:00Z".parse().unwrap(),
            ..ClineTaskJsonImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(cline_first.failed, 0, "{:?}", cline_first.failures);
    assert_eq!(cline_first.imported_sessions, 1);
    assert_eq!(cline_first.imported_events, 3);

    let cline_session = stored_provider_session_id(&store, CaptureProvider::Cline, "cline-task-1");
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

    let roo_session = stored_provider_session_id(&store, CaptureProvider::RooCode, "roo-task-1");
    let roo_events = store.events_for_session(roo_session).unwrap();
    assert_eq!(roo_events.len(), 3);
    assert!(roo_events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    let fallback =
        stored_provider_session_id(&store, CaptureProvider::RooCode, "roo-fallback-task");
    assert_eq!(store.events_for_session(fallback).unwrap().len(), 2);
    assert!(store
        .export_archive()
        .unwrap()
        .files_touched
        .iter()
        .any(|file| file.path == "tests/roo-task-json.txt"));
}
#[test]
fn native_task_json_malformed_file_is_atomic_without_partial_failures() {
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
            allow_partial_failures: true,
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
            allow_partial_failures: true,
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
fn native_trae_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("trae/User/workspaceStorage");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_trae_history(
        &fixture,
        &mut store,
        TraeImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T21:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..TraeImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 2);

    let source = provider_source_for_path(CaptureProvider::Trae, fixture.clone());
    assert_eq!(source.source_format, TRAE_STATE_VSCDB_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let session_id = stored_provider_session_id(
        &store,
        CaptureProvider::Trae,
        "trae-workspace-1/trae-fixture-session",
    );
    let session = store.get_session(session_id).unwrap();
    assert_eq!(session.provider, CaptureProvider::Trae);
    assert_eq!(
        session.sync.metadata["metadata"]["workspace_folder"].as_str(),
        Some("/workspace/trae-fixture")
    );

    let events = store.events_for_session(session_id).unwrap();
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("trae oracle prompt from state vscdb"));
    assert!(rendered.contains("trae oracle answer from state vscdb"));
    assert!(store
        .search_event_hits("trae oracle answer", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Trae)));

    let second = import_trae_history(
        &fixture,
        &mut store,
        TraeImportOptions {
            allow_partial_failures: true,
            ..TraeImportOptions::default()
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
fn native_trae_chatstore_entries_schema_drift_imports() {
    let temp = tempdir();
    let workspace = temp.path().join("User/workspaceStorage/schema-drift");
    fs::create_dir_all(&workspace).unwrap();
    let db_path = workspace.join("state.vscdb");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "CREATE TABLE ItemTable ([key] TEXT PRIMARY KEY, value TEXT)",
        [],
    )
    .unwrap();
    let value = json!({
        "entries": {
            "drift-session": {
                "id": "drift-session",
                "name": "Drift session",
                "messages": [
                    {
                        "id": "drift-user",
                        "role": "user",
                        "content": [{"type": "text", "text": "trae drift prompt"}],
                        "createdAt": "2026-07-05T12:00:00Z"
                    },
                    {
                        "id": "drift-assistant",
                        "role": "assistant",
                        "content": {"summary": "trae drift answer"},
                        "createdAt": "2026-07-05T12:01:00Z"
                    }
                ]
            }
        }
    })
    .to_string();
    conn.execute(
        "INSERT INTO ItemTable ([key], value) VALUES ('ChatStore', ?1)",
        [value],
    )
    .unwrap();
    drop(conn);

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_trae_history(
        temp.path().join("User/workspaceStorage"),
        &mut store,
        TraeImportOptions {
            allow_partial_failures: true,
            ..TraeImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    assert!(store
        .search_event_hits("trae drift answer", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Trae)));
}

#[test]
fn native_trae_cn_input_history_key_imports_user_messages() {
    let temp = tempdir();
    let workspace = temp
        .path()
        .join("Trae CN/User/workspaceStorage/cn-workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(
        workspace.join("workspace.json"),
        r#"{"folder":"file:///workspace/trae-cn-fixture"}"#,
    )
    .unwrap();
    let db_path = workspace.join("state.vscdb");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "CREATE TABLE ItemTable ([key] TEXT PRIMARY KEY, value TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ItemTable ([key], value) VALUES (?1, ?2)",
        rusqlite::params![
            TRAE_CN_INPUT_HISTORY_KEY,
            json!([
                {
                    "id": "cn-input-1",
                    "inputText": "TRAE_CN_INPUT_HISTORY_ORACLE alpha",
                    "createdAt": "2026-07-05T13:00:00Z"
                },
                {
                    "id": "cn-input-2",
                    "text": "TRAE_CN_INPUT_HISTORY_ORACLE beta",
                    "createdAt": "2026-07-05T13:01:00Z"
                }
            ])
            .to_string()
        ],
    )
    .unwrap();
    drop(conn);

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_trae_history(
        temp.path().join("Trae CN/User/workspaceStorage"),
        &mut store,
        TraeImportOptions {
            allow_partial_failures: true,
            ..TraeImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);

    let session_id = stored_provider_session_id(
        &store,
        CaptureProvider::Trae,
        "cn-workspace/trae-cn-input-history",
    );
    let session = store.get_session(session_id).unwrap();
    assert_eq!(
        session.sync.metadata["metadata"]["workspace_folder"].as_str(),
        Some("/workspace/trae-cn-fixture")
    );
    let events = store.events_for_session(session_id).unwrap();
    assert!(events
        .iter()
        .all(|event| event.role == Some(EventRole::User)));
    assert!(store
        .search_event_hits("TRAE_CN_INPUT_HISTORY_ORACLE", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Trae)));
}
#[test]
fn native_auggie_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("auggie/v0.32.0/sessions");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Auggie, fixture.clone());
    assert_eq!(source.source_format, AUGGIE_SESSION_JSON_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_auggie_history(
        &fixture,
        &mut store,
        AuggieImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T20:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..AuggieImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 4);

    let session_id = stored_provider_session_id(
        &store,
        CaptureProvider::Auggie,
        "01K0AUGGIESESSION0000000000",
    );
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].role, Some(EventRole::User));
    assert_eq!(events[1].role, Some(EventRole::Assistant));
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("auggie session json oracle prompt"));
    assert!(rendered.contains("Auggie session import finished"));
    assert!(rendered.contains("auggie node text oracle prompt"));
    assert!(store
        .search_event_hits("Auggie node response imported", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Auggie)));

    let source = store
        .capture_source_by_external_session(CaptureProvider::Auggie, "01K0AUGGIESESSION0000000000")
        .unwrap()
        .unwrap();
    assert_eq!(
        source.sync.metadata["source_metadata"]["upstream_schema_anchor"]["package"].as_str(),
        Some("@augmentcode/auggie@0.32.0")
    );

    let second = import_auggie_history(
        &fixture,
        &mut store,
        AuggieImportOptions {
            allow_partial_failures: true,
            ..AuggieImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 4);
}

#[test]
fn native_firebender_fixture_imports_project_root_db_and_reimports() {
    let temp = tempdir();
    let project_root = provider_history_fixture("firebender/v1");
    let fixture = project_root
        .join(".idea")
        .join("firebender")
        .join("chat_history.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let root_source = provider_source_for_path(CaptureProvider::Firebender, project_root.clone());
    assert_eq!(root_source.source_format, FIREBENDER_SQLITE_SOURCE_FORMAT);
    assert_eq!(root_source.status, ProviderSourceStatus::Available);
    let db_source = provider_source_for_path(CaptureProvider::Firebender, fixture.clone());
    assert_eq!(db_source.source_format, FIREBENDER_SQLITE_SOURCE_FORMAT);
    assert_eq!(db_source.status, ProviderSourceStatus::Available);

    let first = import_firebender_sqlite(
        &project_root,
        &mut store,
        FirebenderSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(project_root.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T20:10:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..FirebenderSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 3);
    let session_id = stored_provider_session_id(
        &store,
        CaptureProvider::Firebender,
        "firebender-fixture-session",
    );
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 3);
    assert!(events
        .iter()
        .any(|event| event.role == Some(EventRole::User)));
    assert!(events
        .iter()
        .any(|event| event.role == Some(EventRole::Assistant)));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("firebender fixture oracle prompt"));
    assert!(rendered.contains("Firebender fixture oracle response"));
    assert!(store
        .search_event_hits("firebender fixture oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Firebender)));

    let source = store
        .capture_source_by_external_session(
            CaptureProvider::Firebender,
            "firebender-fixture-session",
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        source.sync.metadata["source_metadata"]["storage"].as_str(),
        Some(".idea/firebender/chat_history.db")
    );

    let second = import_firebender_sqlite(
        &fixture,
        &mut store,
        FirebenderSqliteImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            ..FirebenderSqliteImportOptions::default()
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
fn provider_sources_discovers_auggie_default_sessions() {
    let temp = tempdir();
    let fixture = provider_history_fixture("auggie/v0.32.0/sessions");
    let sessions = temp.path().join(".augment").join("sessions");
    copy_dir_all(&fixture, &sessions);

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Auggie);
    let source = sources
        .iter()
        .find(|source| source.source_format == AUGGIE_SESSION_JSON_SOURCE_FORMAT)
        .unwrap_or_else(|| panic!("missing Auggie source in {sources:#?}"));
    assert_eq!(source.provider, CaptureProvider::Auggie);
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert_eq!(source.path, sessions);
}

#[test]
fn native_lingma_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("lingma/v1/local.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Lingma, fixture.clone());
    assert_eq!(source.source_format, LINGMA_SQLITE_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_lingma_sqlite(
        &fixture,
        &mut store,
        LingmaSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T16:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..LingmaSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 6);

    let alpha = stored_provider_session_id(&store, CaptureProvider::Lingma, "lingma-session-1");
    let events = store.events_for_session(alpha).unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].role, Some(EventRole::User));
    assert_eq!(events[1].role, Some(EventRole::Assistant));
    assert_eq!(events[1].sync.fidelity, Fidelity::SummaryOnly);
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("lingma oracle prompt update"));
    assert!(rendered.contains("src/lingma_fixture.rs"));
    assert!(rendered.contains("Lingma summary oracle answer"));
    assert!(rendered.contains("summary_only"));
    assert!(rendered.contains("assistant_content_caveat"));
    assert!(store
        .search_event_hits("Lingma summary oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Lingma)));
    assert!(store
        .search_event_hits("lingma oracle prompt update", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Lingma)));

    let error_session =
        stored_provider_session_id(&store, CaptureProvider::Lingma, "lingma-session-2");
    let error_events = store.events_for_session(error_session).unwrap();
    assert_eq!(error_events.len(), 2);
    assert_eq!(error_events[1].event_type, EventType::Notice);
    assert!(serde_json::to_string(&error_events)
        .unwrap()
        .contains("sanitized Lingma error"));

    let second = import_lingma_sqlite(
        &fixture,
        &mut store,
        LingmaSqliteImportOptions {
            allow_partial_failures: true,
            ..LingmaSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 2);
    assert_eq!(second.skipped_events, 6);
}

#[test]
fn native_lingma_import_reports_corrupt_sqlite() {
    let temp = tempdir();
    let db = temp.path().join("corrupt-lingma.db");
    fs::write(&db, b"not sqlite").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err = import_lingma_sqlite(&db, &mut store, LingmaSqliteImportOptions::default())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("not a database") || err.contains("sqlite"),
        "{err}"
    );
}

#[cfg(unix)]
#[test]
fn native_lingma_normalizer_rejects_symlinked_sqlite() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let fixture = provider_history_fixture("lingma/v1/local.db");
    let link = temp.path().join("linked-lingma.db");
    symlink(&fixture, &link).unwrap();

    let err = normalize_lingma_sqlite(&link, &ProviderAdapterContext::default()).unwrap_err();
    assert!(matches!(
        err,
        CaptureError::InvalidProviderTranscriptPath { path, reason }
            if path.ends_with("linked-lingma.db")
                && reason == "symlinked provider transcript files are rejected"
    ));
}
