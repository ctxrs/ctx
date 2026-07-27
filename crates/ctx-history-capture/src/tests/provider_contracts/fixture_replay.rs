use crate::import_provider_fixture_jsonl;
use crate::provider::importer::provider_source_cursor_stream;
use crate::tests::support::paths::{provider_fixture, tempdir};
use crate::tests::support::provider_state::{
    fixed_import_options, provider_fixture_session_id, stored_provider_session_id,
};
use ctx_history_core::{AgentType, CaptureProvider, EventType};
use ctx_history_store::Store;
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

fn write_minimal_provider_fixture(
    temp: &TempDir,
    provider: CaptureProvider,
    external_session_id: &str,
) -> PathBuf {
    let provider_name = provider.as_str();
    let path = temp.path().join(format!("{provider_name}.jsonl"));
    let line = json!({
        "provider": provider_name,
        "session": {
            "provider_session_id": external_session_id,
            "agent_type": "primary",
            "role_hint": "primary",
            "is_primary": true,
            "status": "imported",
            "started_at": "2026-06-23T17:00:00Z",
            "cwd": "/workspace/example",
            "metadata": {"source": "temp-fixture", "provider": provider_name}
        },
        "event": {
            "provider_event_index": 0,
            "cursor": format!("{provider_name}-cursor-0"),
            "event_type": "message",
            "role": "user",
            "occurred_at": "2026-06-23T17:00:01Z",
            "payload": {"text": format!("{provider_name} provider fixture smoke")},
            "metadata": {"source": "temp-fixture"}
        }
    });
    fs::write(&path, format!("{line}\n")).unwrap();
    path
}

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
fn provider_fixture_replay_supports_antigravity_gemini_and_cursor() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let antigravity = provider_fixture("antigravity.jsonl");
    let antigravity_summary = import_provider_fixture_jsonl(
        &antigravity,
        &mut store,
        fixed_import_options(antigravity.clone()),
    )
    .unwrap();
    assert_eq!(antigravity_summary.failed, 0);
    assert_eq!(antigravity_summary.imported_sessions, 2);
    assert_eq!(antigravity_summary.imported_events, 3);
    assert_eq!(antigravity_summary.imported_edges, 1);
    let antigravity_parent =
        provider_fixture_session_id(CaptureProvider::Antigravity, "agy-session-1", &antigravity);
    let antigravity_child = provider_fixture_session_id(
        CaptureProvider::Antigravity,
        "agy-session-1-worker",
        &antigravity,
    );
    assert_eq!(
        store
            .get_session(antigravity_child)
            .unwrap()
            .parent_session_id,
        Some(antigravity_parent)
    );

    let gemini = provider_fixture("gemini.jsonl");
    let gemini_summary =
        import_provider_fixture_jsonl(&gemini, &mut store, fixed_import_options(gemini.clone()))
            .unwrap();
    assert_eq!(gemini_summary.failed, 0);
    assert_eq!(gemini_summary.imported_sessions, 1);
    assert_eq!(gemini_summary.imported_events, 2);
    let gemini_session =
        provider_fixture_session_id(CaptureProvider::Gemini, "gemini-session-1", &gemini);
    let gemini_events = store.events_for_session(gemini_session).unwrap();
    assert_eq!(gemini_events[1].event_type, EventType::ToolOutput);
    assert_eq!(
        gemini_events[1].sync.metadata["metadata"]["telemetry_outfile"].as_str(),
        Some(".gemini/telemetry.log")
    );

    let cursor = provider_fixture("cursor.jsonl");
    let cursor_summary =
        import_provider_fixture_jsonl(&cursor, &mut store, fixed_import_options(cursor.clone()))
            .unwrap();
    assert_eq!(cursor_summary.failed, 0);
    assert_eq!(cursor_summary.imported_sessions, 1);
    assert_eq!(cursor_summary.imported_events, 2);
    let cursor_session =
        provider_fixture_session_id(CaptureProvider::Cursor, "cursor-session-1", &cursor);
    let cursor_events = store.events_for_session(cursor_session).unwrap();
    assert_eq!(cursor_events[1].event_type, EventType::ToolCall);
    assert_eq!(
        cursor_events[0].sync.metadata["metadata"]["docs_surface"].as_str(),
        Some("Cursor CLI sessions and stream-json output")
    );
}

#[test]
fn provider_fixture_replay_is_idempotent_for_native_supported_providers() {
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

        let session_id = provider_fixture_session_id(provider, external_session_id, &fixture);
        assert!(!store.events_for_session(session_id).unwrap().is_empty());
    }
}

#[test]
fn provider_fixture_replay_supports_search_only_temp_fixtures() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    for (
        fixture_name,
        provider,
        external_session_id,
        fixture_sessions,
        fixture_events,
        fixture_edges,
    ) in [
        (
            "copilot_cli.jsonl",
            CaptureProvider::CopilotCli,
            "copilot-cli-session-1",
            1,
            2,
            0,
        ),
        (
            "factory_ai_droid.jsonl",
            CaptureProvider::FactoryAiDroid,
            "factory-ai-droid-session-1",
            2,
            3,
            1,
        ),
    ] {
        let fixture = provider_fixture(fixture_name);
        let (fixture, sessions, events, edges) = if fixture.exists() {
            (fixture, fixture_sessions, fixture_events, fixture_edges)
        } else {
            (
                write_minimal_provider_fixture(&temp, provider, external_session_id),
                1,
                1,
                0,
            )
        };
        let mut options = fixed_import_options(fixture.clone());
        options.expected_provider = Some(provider);

        let first = import_provider_fixture_jsonl(&fixture, &mut store, options.clone()).unwrap();
        assert_eq!(first.failed, 0, "{provider}: {:?}", first.failures);
        assert_eq!(first.imported_sessions, sessions, "{provider}");
        assert_eq!(first.imported_events, events, "{provider}");
        assert_eq!(first.imported_edges, edges, "{provider}");

        let second = import_provider_fixture_jsonl(&fixture, &mut store, options).unwrap();
        assert_eq!(second.failed, 0, "{provider}: {:?}", second.failures);
        assert_eq!(second.imported_sessions, 0, "{provider}");
        assert_eq!(second.imported_events, 0, "{provider}");
        assert_eq!(second.imported_edges, 0, "{provider}");
        assert_eq!(second.skipped_sessions, sessions, "{provider}");
        assert_eq!(second.skipped_events, events, "{provider}");
        assert_eq!(second.skipped_edges, edges, "{provider}");

        let session_id = provider_fixture_session_id(provider, external_session_id, &fixture);
        let session = store.get_session(session_id).unwrap();
        assert_eq!(session.provider, provider);
        assert!(!store.events_for_session(session_id).unwrap().is_empty());
    }
}

#[test]
fn provider_fixture_replay_persists_cursor_checkpoint_and_source_contract_metadata() {
    let temp = tempdir();
    let fixture = provider_fixture("codex.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_provider_fixture_jsonl(&fixture, &mut store, fixed_import_options(fixture.clone()))
            .unwrap();

    assert_eq!(summary.failed, 0);
    let source_path = fixture.display().to_string();
    let cursor_stream = provider_source_cursor_stream(
        CaptureProvider::Codex,
        "normalized_provider_fixture_jsonl",
        Some(&source_path),
    );
    let cursor = store
        .get_sync_cursor(None, "test-machine", &cursor_stream)
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
    assert!(source.sync.metadata["source_idempotency_key"]
        .as_str()
        .is_some());
    assert_eq!(
        source.sync.metadata["cursor"]["after"]["stream"].as_str(),
        Some(cursor_stream.as_str())
    );
    assert!(!cursor_stream.contains(source_path.as_str()));
}
