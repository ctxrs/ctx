use crate::tests::support::assertions::{
    assert_event_type_count, assert_event_with_role, assert_events_have_provider_citations,
    assert_search_hits_provider, assert_search_misses,
};
use crate::tests::support::paths::{provider_history_fixture, tempdir};
use crate::tests::support::provider_state::{
    assert_provider_policy_cursor_restored, delete_event_and_downgrade_provider_policy_cursor,
    only_provider_cursor_stream, stored_provider_session_id,
};
use crate::{
    import_antigravity_cli_history, import_claude_projects_jsonl_tree,
    import_cursor_native_history, import_qoder_history, import_windsurf_cascade_hook_transcripts,
    provider_source_for_path, AntigravityCliImportOptions, ClaudeProjectsImportOptions,
    CursorNativeImportOptions, ProviderImportSupport, ProviderSourceStatus, QoderImportOptions,
    WindsurfCascadeHookImportOptions, ANTIGRAVITY_CLI_SOURCE_FORMAT,
};
use ctx_history_core::{CaptureProvider, Confidence, EventRole, EventType};
use ctx_history_store::Store;
use serde_json::Value;
use std::fs;

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
    assert_eq!(future.len(), 2);
    assert!(future
        .iter()
        .all(|event| event.event_type == EventType::Notice));
    assert_eq!(
        future[1].payload["body"]["entry_type"].as_str(),
        Some("FUTURE_EVENT_KIND")
    );
    assert_eq!(future[1].payload["body"]["text"].as_str(), Some(""));
    assert_eq!(
        future[1].payload["body"]["tool_calls"][0]["name"].as_str(),
        Some("future_tool")
    );
    let future_body: Value =
        serde_json::from_str(future[1].payload["body"]["body"]["json"].as_str().unwrap()).unwrap();
    assert_eq!(
        future_body["content"]["field_retention"]["mode"].as_str(),
        Some("omitted")
    );

    let stored_sessions = store.export_archive().unwrap().sessions.len();
    let stored_events = store.export_archive().unwrap().events.len();
    let replay = import_antigravity_cli_history(
        &fixture,
        &mut store,
        AntigravityCliImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-24T14:00:00Z".parse().unwrap(),
            ..AntigravityCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(replay.imported_sessions, 0);
    assert_eq!(replay.imported_events, 0);
    let archive = store.export_archive().unwrap();
    assert_eq!(archive.sessions.len(), stored_sessions);
    assert_eq!(archive.events.len(), stored_events);
}

#[test]
fn native_windsurf_fixture_imports_searches_reimports_and_file_touches() {
    let temp = tempdir();
    let fixture = provider_history_fixture("windsurf/transcripts");
    let transcript = fixture.join("windsurf-hook-trajectory-1.jsonl");
    let transcript_text = fs::read_to_string(&transcript).unwrap().replace(
        "windsurf unknown typed payload oracle",
        "windsurf-unknown-payload-sentinel",
    );
    fs::write(&transcript, transcript_text).unwrap();
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
        .search_event_hits("windsurf-unknown-payload-sentinel", 10)
        .unwrap()
        .is_empty());

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
    let code_action_payload = code_action.payload.to_string();
    assert!(code_action_payload.contains("src/windsurf_hook_oracle.py"));
    assert!(!code_action_payload.contains("print('windsurf cascade hook oracle')"));

    let archive = store.export_archive().unwrap();
    assert!(archive.files_touched.iter().any(|file| {
        file.path == "src/windsurf_hook_oracle.py" && file.confidence == Confidence::High
    }));

    let second = import_windsurf_cascade_hook_transcripts(
        &fixture,
        &mut store,
        WindsurfCascadeHookImportOptions {
            source_path: Some(fixture.clone()),
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
    let tool_output = events
        .iter()
        .find(|event| {
            event.event_type == EventType::ToolOutput && event.role == Some(EventRole::User)
        })
        .expect("tool output metadata event imported");
    assert!(!tool_output.payload.to_string().contains("qoder import ok"));

    let second = import_qoder_history(
        &fixture,
        &mut store,
        QoderImportOptions {
            source_path: Some(fixture.clone()),
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
fn native_cursor_fixture_imports_searches_reports_malformed_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("cursor/2026.06.24");
    let transcript = fixture.join(
        "projects/sanitized-workspace/agent-transcripts/cursor-native-session-1/cursor-native-session-1.jsonl",
    );
    let transcript_text = fs::read_to_string(&transcript)
        .unwrap()
        .replace("cursor native fixture proof", "cursor-tool-input-sentinel")
        .replace(
            "wrote cursor-native-cli-oracle.txt",
            "created commit 0123456789abcdef0123456789abcdef01234567 https://github.com/ctxrs/ctx/pull/456?token=cursor-secret#fragment cursor-tool-output-sentinel",
        );
    fs::write(&transcript, transcript_text).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    store
        .activate_projection_journal(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .unwrap();

    let source = provider_source_for_path(CaptureProvider::Cursor, fixture.clone());
    assert_eq!(source.source_format, "cursor_agent_transcript_jsonl_tree");
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_cursor_native_history(
        &fixture,
        &mut store,
        CursorNativeImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-24T12:20:00Z".parse().unwrap(),
            ..CursorNativeImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 1, "{first:?}");
    assert_eq!(first.failures[0].line, 2);
    assert!(first.failures[0].error.contains("malformed JSONL"));
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 6);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Cursor, "cursor-native-session-1");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 5);
    assert_event_type_count(&events, EventType::Message, 2);
    assert_event_type_count(&events, EventType::ToolCall, 1);
    assert_event_type_count(&events, EventType::ToolOutput, 1);
    assert_event_type_count(&events, EventType::Summary, 1);
    assert_event_with_role(&events, EventType::ToolOutput, EventRole::User);
    assert_events_have_provider_citations(&events);
    let tool_output = events
        .iter()
        .find(|event| event.event_type == EventType::ToolOutput)
        .expect("Cursor tool result imported");
    let rendered = tool_output.payload.to_string();
    assert!(rendered.contains("0123456789abcdef0123456789abcdef01234567"));
    assert!(rendered.contains("https://github.com/ctxrs/ctx/pull/456"));
    assert!(rendered.contains("tool-1"));
    assert!(!rendered.contains("token=cursor-secret"));
    assert!(!rendered.contains("fragment"));
    assert!(!rendered.contains("cursor-tool-output-sentinel"));

    let journal = store.projection_journal_snapshot(None).unwrap();
    let canonical_tool_output = journal
        .records
        .iter()
        .find(|record| record.stable_entity_id == tool_output.id)
        .and_then(|record| record.canonical_payload.as_ref())
        .expect("Cursor tool result reaches the canonical projection journal");
    assert_eq!(
        canonical_tool_output["result"]["outcome"], "unknown",
        "Cursor's observed tool_result shape does not prove success"
    );

    let malformed_id =
        stored_provider_session_id(&store, CaptureProvider::Cursor, "cursor-malformed-session");
    let malformed_events = store.events_for_session(malformed_id).unwrap();
    assert_eq!(malformed_events.len(), 1);
    assert_event_type_count(&malformed_events, EventType::Message, 1);
    assert_events_have_provider_citations(&malformed_events);

    assert_search_hits_provider(
        &store,
        "Create cursor-native-cli-oracle",
        CaptureProvider::Cursor,
    );
    assert_search_hits_provider(
        &store,
        "This valid line should import",
        CaptureProvider::Cursor,
    );
    assert_search_misses(&store, "cursor-tool-output-sentinel");
    assert_search_misses(&store, "cursor-tool-input-sentinel");

    let archive = store.export_archive().unwrap();
    assert!(archive
        .files_touched
        .iter()
        .any(|file| file.path == "cursor-native-cli-oracle.txt"));

    let second = import_cursor_native_history(
        &fixture,
        &mut store,
        CursorNativeImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-24T12:25:00Z".parse().unwrap(),
            ..CursorNativeImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 1, "{second:?}");
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 2);
    assert_eq!(second.skipped_events, 6);
}

#[test]
fn native_cursor_policy_upgrade_repairs_once_then_is_terminal_noop() {
    let temp = tempdir();
    let database = temp.path().join("work.sqlite");
    let fixture = provider_history_fixture("cursor/2026.06.24");
    let transcript = fixture.join(
        "projects/sanitized-workspace/agent-transcripts/cursor-native-session-1/cursor-native-session-1.jsonl",
    );
    let machine_id = "cursor-policy-upgrade-machine";
    let options = CursorNativeImportOptions {
        machine_id: machine_id.to_owned(),
        source_path: Some(transcript.clone()),
        imported_at: "2026-06-24T12:20:00Z".parse().unwrap(),
        ..CursorNativeImportOptions::default()
    };
    let mut store = Store::open(&database).unwrap();

    let first = import_cursor_native_history(&transcript, &mut store, options.clone()).unwrap();
    assert_eq!(first.failed, 0, "{first:?}");
    assert_eq!(first.imported_events, 5);
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Cursor, "cursor-native-session-1");
    let output = store
        .events_for_session(session_id)
        .unwrap()
        .into_iter()
        .find(|event| event.event_type == EventType::ToolOutput)
        .expect("Cursor result exists before simulated legacy upgrade");
    let stream = only_provider_cursor_stream(&database, machine_id);
    let policy_revision = delete_event_and_downgrade_provider_policy_cursor(
        &database, &store, machine_id, &stream, output.id,
    );

    let repaired = import_cursor_native_history(&transcript, &mut store, options.clone()).unwrap();
    assert_eq!(repaired.failed, 0, "{repaired:?}");
    assert_eq!(repaired.imported_events, 1);
    assert_eq!(store.events_for_session(session_id).unwrap().len(), 5);
    assert_provider_policy_cursor_restored(&store, machine_id, &stream, policy_revision);
    let repaired_output = store
        .events_for_session(session_id)
        .unwrap()
        .into_iter()
        .find(|event| event.event_type == EventType::ToolOutput)
        .expect("Cursor result was restored by the policy rebuild");
    let repaired_ref = serde_json::from_value::<ctx_history_core::ContentRef>(
        repaired_output.payload["body"]["result_content_ref"].clone(),
    )
    .expect("policy rebuild restores compact result identity");
    let repaired_locators =
        crate::complete_content::VerifiedContentLocatorsV1::from_metadata_value(
            &repaired_output.sync.metadata
                [crate::complete_content::VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
        )
        .expect("policy rebuild restores verified content locators");
    let repaired_locator = repaired_locators
        .locator(crate::complete_content::VerifiedContentRole::ResultBody)
        .expect("policy rebuild restores the result-body locator");
    assert_eq!(repaired_locator.content_ref(), &repaired_ref);

    let terminal = import_cursor_native_history(&transcript, &mut store, options).unwrap();
    assert_eq!(terminal.failed, 0, "{terminal:?}");
    assert_eq!(terminal.imported_events, 0);
    assert_eq!(terminal.skipped_events, 5);
    assert_eq!(store.events_for_session(session_id).unwrap().len(), 5);
}

#[test]
fn native_windsurf_reports_malformed_jsonl_and_keeps_valid_rows() {
    let temp = tempdir();
    let fixture = provider_history_fixture("windsurf/malformed/transcripts");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_windsurf_cascade_hook_transcripts(
        &fixture,
        &mut store,
        WindsurfCascadeHookImportOptions {
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
    let path = fixture.join("claude-malformed.jsonl");
    fs::write(
            &path,
            concat!(
                "{\"sessionId\":\"claude-malformed\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"valid\"}}\n",
                "{\"sessionId\":\"claude-malformed\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"partial\"}]\n",
            ),
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_claude_projects_jsonl_tree(
        &path,
        &mut store,
        ClaudeProjectsImportOptions {
            ..ClaudeProjectsImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    assert!(summary.failures[0].error.contains("malformed JSONL"));
    assert!(summary.failures[0].error.contains("claude-malformed.jsonl"));
}
