#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn provider_fixture_replay_supports_claude_cursor_metadata() {
    let temp = tempdir();
    let fixture = provider_fixture("claude.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_provider_fixture_jsonl(&fixture, &mut store, fixed_import_options(fixture.clone()))
            .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    let session_id = provider_session_uuid(CaptureProvider::Claude, "claude-session-1");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events[1].event_type, EventType::Summary);
    assert_eq!(
        events[1].sync.metadata["cursor"].as_str(),
        Some("claude-cursor-1")
    );
    assert_eq!(events[1].payload["provider_event_index"].as_u64(), Some(1));
}

#[test]
pub(crate) fn native_claude_projects_imports_jsonl_tree() {
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
    let parent_id = provider_session_uuid(CaptureProvider::Claude, "claude-native-parent");
    let child_id = provider_session_uuid(
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
pub(crate) fn native_claude_projects_reports_malformed_jsonl() {
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

pub(crate) fn write_claude_smoke_fixture(temp: &TempDir) -> PathBuf {
    let root = temp.path().join("claude/projects/-workspace");
    let subagents = root.join("claude-native-parent/subagents");
    fs::create_dir_all(&subagents).unwrap();
    fs::write(
        root.join("claude-native-parent.jsonl"),
        concat!(
            "{\"sessionId\":\"claude-native-parent\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\",\"version\":\"test\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"Run a smoke tool.\"}]},\"uuid\":\"claude-parent-1\"}\n",
            "{\"sessionId\":\"claude-native-parent\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"cwd\":\"/workspace\",\"version\":\"test\",\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"Bash\",\"input\":{\"command\":\"true\"}}]},\"uuid\":\"claude-parent-2\"}\n",
            "{\"sessionId\":\"claude-native-parent\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"cwd\":\"/workspace\",\"version\":\"test\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"tool-1\",\"content\":\"ok\"}]},\"uuid\":\"claude-parent-3\"}\n",
        ),
    )
    .unwrap();
    fs::write(
        subagents.join("agent-scout.jsonl"),
        concat!(
            "{\"sessionId\":\"claude-native-parent\",\"timestamp\":\"2026-06-24T12:00:03Z\",\"cwd\":\"/workspace\",\"version\":\"test\",\"isSidechain\":true,\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"inspect\"},\"uuid\":\"claude-child-1\"}\n",
            "{\"sessionId\":\"claude-native-parent\",\"timestamp\":\"2026-06-24T12:00:04Z\",\"cwd\":\"/workspace\",\"version\":\"test\",\"isSidechain\":true,\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":\"done\"},\"uuid\":\"claude-child-2\"}\n",
        ),
    )
    .unwrap();
    temp.path().join("claude/projects")
}

#[test]
pub(crate) fn provider_fixture_replay_is_idempotent_for_native_supported_providers() {
    for (name, provider, external_session_id, sessions, events, edges) in [
        (
            "claude.jsonl",
            CaptureProvider::Claude,
            "claude-session-1",
            1,
            2,
            0,
        ),
        (
            "opencode.jsonl",
            CaptureProvider::OpenCode,
            "opencode-session-1",
            2,
            3,
            1,
        ),
        (
            "antigravity.jsonl",
            CaptureProvider::Antigravity,
            "agy-session-1",
            2,
            3,
            1,
        ),
        (
            "gemini.jsonl",
            CaptureProvider::Gemini,
            "gemini-session-1",
            1,
            2,
            0,
        ),
        (
            "cursor.jsonl",
            CaptureProvider::Cursor,
            "cursor-session-1",
            1,
            2,
            0,
        ),
    ] {
        let temp = tempdir();
        let fixture = provider_fixture(name);
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let first = import_provider_fixture_jsonl(
            &fixture,
            &mut store,
            fixed_import_options(fixture.clone()),
        )
        .unwrap();
        assert_eq!(first.failed, 0, "{name}: {:?}", first.failures);
        assert_eq!(first.imported_sessions, sessions, "{name}");
        assert_eq!(first.imported_events, events, "{name}");
        assert_eq!(first.imported_edges, edges, "{name}");

        let second = import_provider_fixture_jsonl(
            &fixture,
            &mut store,
            fixed_import_options(fixture.clone()),
        )
        .unwrap();
        assert_eq!(second.failed, 0, "{name}: {:?}", second.failures);
        assert_eq!(second.imported_sessions, 0, "{name}");
        assert_eq!(second.imported_events, 0, "{name}");
        assert_eq!(second.imported_edges, 0, "{name}");
        assert_eq!(second.skipped_sessions, sessions, "{name}");
        assert_eq!(second.skipped_events, events, "{name}");
        assert_eq!(second.skipped_edges, edges, "{name}");

        let session_id = provider_session_uuid(provider, external_session_id);
        assert!(!store.events_for_session(session_id).unwrap().is_empty());
    }
}

#[test]
pub(crate) fn provider_import_reuses_existing_legacy_provider_event_identity() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let provider = CaptureProvider::Claude;
    let provider_session_id = "legacy-provider-session";
    let source_format = "provider_format";
    let raw_source_path = temp
        .path()
        .join("legacy-source.jsonl")
        .display()
        .to_string();
    let occurred_at = DateTime::parse_from_rfc3339("2026-06-23T17:00:01Z")
        .unwrap()
        .with_timezone(&Utc);
    let legacy_source_id = provider_source_uuid(provider, provider_session_id);
    let new_source_id = provider_scoped_source_uuid(
        provider,
        provider_session_id,
        source_format,
        Some(&raw_source_path),
    );
    let session_id = provider_session_uuid(provider, provider_session_id);
    let legacy_event_id = provider_event_uuid(provider, provider_session_id, 0);
    let legacy_touch_id = provider_file_touch_uuid(provider, provider_session_id, 0);
    let event_hash = compute_payload_hash(&json!({"text": "same provider event payload"})).unwrap();
    assert_ne!(legacy_source_id, new_source_id);

    store
        .upsert_capture_source(&CaptureSource {
            id: legacy_source_id,
            descriptor: CaptureSourceDescriptor {
                kind: CaptureSourceKind::ProviderImport,
                provider,
                machine_id: "test-machine".to_owned(),
                process_id: None,
                cwd: Some("/workspace/example".to_owned()),
                raw_source_path: None,
                external_session_id: Some(provider_session_id.to_owned()),
            },
            started_at: occurred_at,
            ended_at: None,
            sync: provider_sync_metadata(Fidelity::Imported, json!({"legacy": true})),
        })
        .unwrap();
    store
        .upsert_session(&Session {
            id: session_id,
            history_record_id: None,
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: Some(legacy_source_id),
            provider,
            external_session_id: Some(provider_session_id.to_owned()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: occurred_at,
            ended_at: None,
            timestamps: timestamps(occurred_at),
            sync: provider_sync_metadata(Fidelity::Imported, json!({"legacy": true})),
        })
        .unwrap();
    store
        .upsert_event(&Event {
            id: legacy_event_id,
            seq: provider_event_seq(provider, provider_session_id, 0),
            history_record_id: None,
            session_id: Some(session_id),
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::User),
            occurred_at,
            capture_source_id: Some(legacy_source_id),
            payload: json!({"body": {"text": "same provider event payload"}}),
            payload_blob_id: None,
            dedupe_key: Some(Store::provider_event_dedupe_key(
                provider,
                provider_session_id,
                0,
                &event_hash,
            )),
            redaction_state: RedactionState::LocalPreview,
            sync: provider_sync_metadata(Fidelity::Imported, json!({"legacy": true})),
        })
        .unwrap();
    store
        .upsert_file_touched(&FileTouched {
            id: legacy_touch_id,
            history_record_id: None,
            run_id: None,
            event_id: Some(legacy_event_id),
            vcs_workspace_id: None,
            path: "src/lib.rs".to_owned(),
            change_kind: Some(FileChangeKind::Modified),
            old_path: None,
            line_count_delta: Some(1),
            confidence: Confidence::Explicit,
            timestamps: timestamps(occurred_at),
            source_id: Some(legacy_source_id),
            sync: provider_sync_metadata(Fidelity::Imported, json!({"legacy": true})),
        })
        .unwrap();

    let normalization = ProviderNormalizationResult {
        summary: ProviderImportSummary::default(),
        captures: vec![(
            1,
            provider_collision_capture(
                provider,
                provider_session_id,
                source_format,
                &raw_source_path,
                occurred_at,
            ),
        )],
        files_touched: vec![(
            1,
            provider_collision_file_touch(
                provider,
                provider_session_id,
                source_format,
                &raw_source_path,
                occurred_at,
            ),
        )],
    };

    let summary = import_normalized_provider_captures(
        &mut store,
        normalization,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.skipped_events, 1);
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, legacy_event_id);
    assert_eq!(events[0].capture_source_id, Some(legacy_source_id));

    let archive = store.export_archive().unwrap();
    assert_eq!(archive.files_touched.len(), 1);
    assert_eq!(archive.files_touched[0].id, legacy_touch_id);
    assert_eq!(archive.files_touched[0].event_id, Some(legacy_event_id));
    assert_eq!(archive.files_touched[0].source_id, Some(new_source_id));
}

#[test]
pub(crate) fn provider_fixture_replay_rejects_expected_provider_mismatch() {
    let temp = tempdir();
    let fixture = provider_fixture("claude.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut options = fixed_import_options(fixture.clone());
    options.expected_provider = Some(CaptureProvider::Codex);

    let summary = import_provider_fixture_jsonl(fixture, &mut store, options).unwrap();

    assert_eq!(summary.imported, 0);
    assert_eq!(summary.failed, 2);
    assert!(summary.failures.iter().all(|failure| failure
        .error
        .contains("has provider `claude` but expected `codex`")));
}
