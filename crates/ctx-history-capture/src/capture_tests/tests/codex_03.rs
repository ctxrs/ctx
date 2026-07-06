#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn codex_search_event_mode_only_parses_search_relevant_lines() {
    let session_meta =
        br#"{"timestamp":"2026-06-24T01:00:00.000Z","type":"session_meta","payload":{"id":"s"}}"#;
    let user_message = br#"{"timestamp":"2026-06-24T01:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"question"}]}}"#;
    let assistant_message = br#"{"timestamp":"2026-06-24T01:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer"}]}}"#;
    let tool_call = br#"{"timestamp":"2026-06-24T01:00:03.000Z","type":"response_item","payload":{"type":"function_call","call_id":"call-1","name":"shell","arguments":"cargo test"}}"#;
    let tool_output = br#"{"timestamp":"2026-06-24T01:00:04.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-1","output":"passed"}}"#;
    let reasoning = br#"{"timestamp":"2026-06-24T01:00:05.000Z","type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"thinking"}]}}"#;
    let notice = br#"{"timestamp":"2026-06-24T01:00:06.000Z","type":"event_msg","payload":{"type":"task_complete"}}"#;
    let apply_patch = br#"{"timestamp":"2026-06-24T01:00:07.000Z","type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"*** Begin Patch\n*** Update File: crates/ctx-cli/src/main.rs\n@@\n-old\n+new\n*** End Patch","call_id":"call-patch","status":"completed"}}"#;

    for line in [
        session_meta.as_slice(),
        user_message.as_slice(),
        assistant_message.as_slice(),
        apply_patch.as_slice(),
    ] {
        assert!(should_parse_codex_session_line(
            line,
            CodexEventImportMode::Search
        ));
    }
    for line in [
        tool_call.as_slice(),
        tool_output.as_slice(),
        reasoning.as_slice(),
        notice.as_slice(),
    ] {
        assert!(!should_parse_codex_session_line(
            line,
            CodexEventImportMode::Search
        ));
        assert!(should_parse_codex_session_line(
            line,
            CodexEventImportMode::Rich
        ));
    }
}

#[test]
pub(crate) fn codex_search_event_mode_persists_file_touches_without_tool_events() {
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
            event_mode: CodexEventImportMode::Search,
            tool_output_mode: CodexToolOutputMode::Skip,
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 1);

    let session_id = provider_session_uuid(CaptureProvider::Codex, "codex-search-file-touch");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::Message);

    let archive = store.export_archive().unwrap();
    let touched = archive
        .files_touched
        .iter()
        .find(|file| file.path == "crates/ctx-cli/src/main.rs")
        .expect("apply_patch should create file touch metadata in search mode");
    assert_eq!(touched.change_kind, Some(FileChangeKind::Modified));
    assert_eq!(touched.event_id, None);
    assert_eq!(touched.history_record_id, None);
}

#[test]
pub(crate) fn provider_fixture_replay_persists_cursor_checkpoint_and_source_contract_metadata() {
    let temp = tempdir();
    let fixture = provider_fixture("codex.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_provider_fixture_jsonl(&fixture, &mut store, fixed_import_options(fixture.clone()))
            .unwrap();

    assert_eq!(summary.failed, 0);
    let cursor = store
        .get_sync_cursor(
            None,
            "test-machine",
            &provider_cursor_stream(CaptureProvider::Codex, "normalized_provider_fixture_jsonl"),
        )
        .unwrap()
        .unwrap();
    assert_eq!(cursor.cursor, "codex-sub-cursor-0");

    let source = store
        .capture_source_by_external_session(CaptureProvider::Codex, "codex-session-1")
        .unwrap()
        .unwrap();
    assert_eq!(
        source.sync.metadata["source_format"].as_str(),
        Some("normalized_provider_fixture_jsonl")
    );
    assert_eq!(
        source.sync.metadata["source_trust"].as_str(),
        Some("fixture")
    );
    assert_eq!(
        source.sync.metadata["raw_retention"].as_str(),
        Some("path_reference")
    );
    assert_eq!(
        source.sync.metadata["redaction_boundary"].as_str(),
        Some("before_export")
    );
    assert!(source.sync.metadata["source_idempotency_key"]
        .as_str()
        .is_some());
}

#[test]
pub(crate) fn codex_history_import_is_prompt_only_summary_fidelity_and_idempotent() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-history.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_codex_history_jsonl(
        &fixture,
        &mut store,
        CodexHistoryImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T15:30:00Z".parse().unwrap(),
            ..CodexHistoryImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 3);
    assert_eq!(first.imported_edges, 0);
    assert!(!store.event_search_projection_needs_backfill().unwrap());

    let second = import_codex_history_jsonl(
        &fixture,
        &mut store,
        CodexHistoryImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T15:30:00Z".parse().unwrap(),
            ..CodexHistoryImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_events, 3);

    let session_id = provider_session_uuid(CaptureProvider::Codex, "codex-history-session-1");
    let session = store.get_session(session_id).unwrap();
    assert_eq!(session.sync.fidelity, Fidelity::SummaryOnly);
    assert_eq!(
        session.sync.metadata["source_format"].as_str(),
        Some("codex_history_jsonl")
    );
    assert_eq!(
        session.sync.metadata["metadata"]["source_fidelity"].as_str(),
        Some("prompt_log_only")
    );
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].sync.fidelity, Fidelity::SummaryOnly);
    assert_eq!(events[0].role, Some(EventRole::User));
    assert_eq!(events[0].event_type, EventType::Message);
    assert_eq!(
        events[0].sync.metadata["source_format"].as_str(),
        Some("codex_history_jsonl")
    );
    let cursor = store
        .get_sync_cursor(
            None,
            &CodexHistoryImportOptions::default().machine_id,
            &provider_cursor_stream(CaptureProvider::Codex, "codex_history_jsonl"),
        )
        .unwrap()
        .unwrap();
    assert_eq!(cursor.cursor, "line:3");
}
