use crate::tests::support::assertions::{
    assert_event_type_count, assert_event_with_role, assert_events_have_provider_citations,
    assert_search_hits_provider, assert_search_misses, assert_structural_oversize_failure,
};
use crate::tests::support::fixtures::jsonl::{
    jsonl_line, oversized_jsonl_line, write_copilot_smoke_fixture, write_droid_smoke_fixture,
    write_gemini_smoke_fixture,
};
use crate::tests::support::paths::{provider_history_fixture, tempdir};
use crate::tests::support::provider_state::stored_provider_session_id;
use crate::{
    import_copilot_cli_session_events, import_factory_ai_droid_sessions, import_gemini_cli_history,
    import_kimi_code_cli_history, import_qwen_code_history, import_tabnine_cli_history,
    CopilotCliImportOptions, FactoryAiDroidImportOptions, GeminiCliImportOptions,
    KimiCodeCliImportOptions, ProviderImportSummary, QwenCodeImportOptions,
    TabnineCliImportOptions,
};
use ctx_history_core::{CaptureProvider, EventRole, EventType};
use ctx_history_store::Store;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn write_unimportable_jsonl_siblings(root: &Path, prefix: &str) {
    fs::write(root.join(format!("{prefix}-empty.jsonl")), "").unwrap();
    fs::write(
        root.join(format!("{prefix}-malformed.jsonl")),
        "{\"not valid\"\n",
    )
    .unwrap();
    fs::write(
        root.join(format!("{prefix}-headerless.jsonl")),
        "{\"type\":\"message\",\"content\":\"missing session header\"}\n",
    )
    .unwrap();
}

fn write_unimportable_copilot_siblings(root: &Path) {
    for (session, content) in [
        ("copilot-empty", ""),
        ("copilot-malformed", "{\"not valid\"\n"),
        (
            "copilot-headerless",
            "{\"type\":\"user.message\",\"data\":{\"content\":\"missing session header\"}}\n",
        ),
    ] {
        let path = root.join(session);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("events.jsonl"), content).unwrap();
    }
}

fn write_qwen_smoke_fixture(temp: &TempDir) -> PathBuf {
    let chats = temp.path().join("qwen/.qwen/projects/workspace/chats");
    fs::create_dir_all(&chats).unwrap();
    fs::write(
            chats.join("qwen-smoke.jsonl"),
            concat!(
                "{\"uuid\":\"qwen-1\",\"parentUuid\":null,\"sessionId\":\"qwen-smoke\",\"timestamp\":\"2026-07-04T12:00:00Z\",\"type\":\"user\",\"cwd\":\"/workspace/qwen\",\"version\":\"test\",\"gitBranch\":\"main\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"qwen jsonl oracle prompt\"}]},\"model\":\"qwen3-coder\"}\n",
                "{\"uuid\":\"qwen-2\",\"parentUuid\":\"qwen-1\",\"sessionId\":\"qwen-smoke\",\"timestamp\":\"2026-07-04T12:00:01Z\",\"type\":\"assistant\",\"cwd\":\"/workspace/qwen\",\"version\":\"test\",\"gitBranch\":\"main\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"qwen jsonl oracle answer\"},{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"Write\",\"input\":{\"path\":\"src/qwen_oracle.txt\",\"content\":\"proof\"}}]},\"usageMetadata\":{\"inputTokens\":5,\"outputTokens\":7},\"model\":\"qwen3-coder\"}\n",
                "{\"uuid\":\"qwen-3\",\"parentUuid\":\"qwen-2\",\"sessionId\":\"qwen-smoke\",\"timestamp\":\"2026-07-04T12:00:02Z\",\"type\":\"tool_result\",\"cwd\":\"/workspace/qwen\",\"version\":\"test\",\"gitBranch\":\"main\",\"message\":{\"role\":\"tool\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"tool-1\",\"content\":\"QWEN_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH\"}]},\"toolCallResult\":{\"tool\":\"Write\",\"path\":\"src/qwen_oracle.txt\",\"output\":\"QWEN_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH\"},\"model\":\"qwen3-coder\"}\n",
            ),
        )
        .unwrap();
    temp.path().join("qwen/.qwen/projects")
}

fn write_kimi_smoke_fixture(temp: &TempDir) -> PathBuf {
    let home = temp.path().join("kimi/.kimi-code");
    let session = home.join("sessions/wd_demo_abc123/kimi-smoke");
    let main = session.join("agents/main");
    let child = session.join("agents/agent-1");
    fs::create_dir_all(&main).unwrap();
    fs::create_dir_all(&child).unwrap();
    fs::write(
        home.join("session_index.jsonl"),
        format!(
            "{}\n",
            json!({
                "sessionId": "kimi-smoke",
                "sessionDir": session.display().to_string(),
                "workDir": "/workspace/kimi"
            })
        ),
    )
    .unwrap();
    fs::write(
            session.join("state.json"),
            json!({
                "createdAt": "2026-07-04T13:00:00Z",
                "updatedAt": "2026-07-04T13:00:05Z",
                "title": "Kimi JSONL oracle",
                "lastPrompt": "kimi jsonl oracle prompt",
                "agents": {
                    "main": {"homedir": "/fixture/agents/main", "type": "main", "parentAgentId": null},
                    "agent-1": {"homedir": "/fixture/agents/agent-1", "type": "coder", "parentAgentId": "main"}
                }
            })
            .to_string(),
        )
        .unwrap();
    fs::write(
            main.join("wire.jsonl"),
            concat!(
                "{\"type\":\"metadata\",\"protocol_version\":\"1.4\",\"created_at\":1783170000000}\n",
                "{\"type\":\"turn.prompt\",\"time\":1783170001000,\"input\":[{\"type\":\"text\",\"text\":\"kimi jsonl oracle prompt\"}],\"origin\":{\"kind\":\"user\"}}\n",
                "{\"type\":\"context.append_message\",\"time\":1783170002000,\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"kimi jsonl oracle answer\"}]}}\n",
                "{\"type\":\"context.append_loop_event\",\"time\":1783170003000,\"event\":{\"type\":\"tool.call\",\"toolName\":\"Write\",\"input\":{\"path\":\"src/kimi_oracle.txt\",\"content\":\"proof\"}}}\n",
                "{\"type\":\"context.append_loop_event\",\"time\":1783170004000,\"event\":{\"type\":\"tool.result\",\"toolName\":\"Write\",\"output\":\"KIMI_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH\"}}\n",
                "{\"type\":\"usage.record\",\"time\":1783170005000,\"model\":\"kimi-k2\",\"usage\":{\"input_tokens\":11,\"output_tokens\":13}}\n",
            ),
        )
        .unwrap();
    fs::write(
            child.join("wire.jsonl"),
            concat!(
                "{\"type\":\"metadata\",\"protocol_version\":\"1.4\",\"created_at\":1783170006000}\n",
                "{\"type\":\"turn.prompt\",\"time\":1783170007000,\"input\":[{\"type\":\"text\",\"text\":\"child inspect\"}]}\n",
                "{\"type\":\"context.append_message\",\"time\":1783170008000,\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"child done\"}]}}\n",
            ),
        )
        .unwrap();
    home
}

#[test]

fn native_jsonl_tree_imports_gemini_droid_and_copilot_smokes() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let gemini = write_gemini_smoke_fixture(&temp);
    let gemini_summary = import_gemini_cli_history(
        &gemini,
        &mut store,
        GeminiCliImportOptions {
            ..GeminiCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(gemini_summary.failed, 0);
    assert_eq!(gemini_summary.imported_sessions, 2);
    assert_eq!(gemini_summary.imported_events, 5);
    assert_eq!(gemini_summary.imported_edges, 1);
    let gemini_parent = stored_provider_session_id(&store, CaptureProvider::Gemini, "gemini-root");
    let gemini_events = store.events_for_session(gemini_parent).unwrap();
    assert_eq!(gemini_events.len(), 3);
    assert_event_type_count(&gemini_events, EventType::Message, 1);
    assert_event_type_count(&gemini_events, EventType::ToolCall, 1);
    assert_event_type_count(&gemini_events, EventType::ToolOutput, 0);
    assert_event_type_count(&gemini_events, EventType::Notice, 1);
    assert_events_have_provider_citations(&gemini_events);
    assert_search_hits_provider(
        &store,
        "gemini jsonl oracle prompt",
        CaptureProvider::Gemini,
    );
    assert_search_misses(&store, "GEMINI_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH");
    let gemini_child = stored_provider_session_id(&store, CaptureProvider::Gemini, "gemini-child");
    assert_eq!(
        store.get_session(gemini_child).unwrap().parent_session_id,
        Some(gemini_parent)
    );
    let gemini_second = import_gemini_cli_history(
        &gemini,
        &mut store,
        GeminiCliImportOptions {
            ..GeminiCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(gemini_second.failed, 0, "{:?}", gemini_second.failures);
    assert_eq!(gemini_second.imported_sessions, 0);
    assert_eq!(gemini_second.imported_events, 0);
    assert_eq!(gemini_second.imported_edges, 0);
    assert_eq!(gemini_second.skipped_sessions, 2);
    assert_eq!(gemini_second.skipped_events, 5);
    assert_eq!(gemini_second.skipped_edges, 0);

    let tabnine = provider_history_fixture("tabnine-cli/.tabnine/agent");
    let tabnine_summary = import_tabnine_cli_history(
        &tabnine,
        &mut store,
        TabnineCliImportOptions {
            ..TabnineCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(tabnine_summary.failed, 0, "{:?}", tabnine_summary.failures);
    assert_eq!(tabnine_summary.imported_sessions, 2);
    assert_eq!(tabnine_summary.imported_events, 6);
    assert_eq!(tabnine_summary.imported_edges, 1);

    let tabnine_events = store
        .events_for_session(stored_provider_session_id(
            &store,
            CaptureProvider::Tabnine,
            "tabnine-root",
        ))
        .unwrap();
    assert_eq!(tabnine_events.len(), 4);
    assert_event_type_count(&tabnine_events, EventType::Message, 2);
    assert_event_type_count(&tabnine_events, EventType::ToolCall, 1);
    assert_event_type_count(&tabnine_events, EventType::ToolOutput, 0);
    assert_event_type_count(&tabnine_events, EventType::Notice, 1);
    assert_event_with_role(&tabnine_events, EventType::ToolCall, EventRole::Assistant);
    assert_events_have_provider_citations(&tabnine_events);
    let tabnine_rendered = serde_json::to_string(&tabnine_events).unwrap();
    assert!(tabnine_rendered.contains("tabnine jsonl oracle prompt"));
    assert!(tabnine_rendered.contains("tabnine jsonl oracle answer"));
    assert!(tabnine_rendered.contains("src/tabnine_oracle.txt"));
    assert_search_hits_provider(
        &store,
        "tabnine jsonl oracle prompt",
        CaptureProvider::Tabnine,
    );
    assert_search_misses(&store, "TABNINE_RAW_TOOL_RESULT_SHOULD_NOT_SEARCH");

    let tabnine_child =
        stored_provider_session_id(&store, CaptureProvider::Tabnine, "tabnine-child");
    let tabnine_parent =
        stored_provider_session_id(&store, CaptureProvider::Tabnine, "tabnine-root");
    assert_eq!(
        store.get_session(tabnine_child).unwrap().parent_session_id,
        Some(tabnine_parent)
    );

    let droid = write_droid_smoke_fixture(&temp);
    let droid_summary = import_factory_ai_droid_sessions(
        &droid,
        &mut store,
        FactoryAiDroidImportOptions {
            ..FactoryAiDroidImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(droid_summary.failed, 0);
    assert_eq!(droid_summary.imported_sessions, 2);
    assert_eq!(droid_summary.imported_events, 5);
    assert_eq!(droid_summary.imported_edges, 1);
    let droid_parent =
        stored_provider_session_id(&store, CaptureProvider::FactoryAiDroid, "droid-root");
    let droid_events = store.events_for_session(droid_parent).unwrap();
    assert_eq!(droid_events.len(), 3);
    assert_event_type_count(&droid_events, EventType::Message, 1);
    assert_event_type_count(&droid_events, EventType::ToolCall, 1);
    assert_event_type_count(&droid_events, EventType::ToolOutput, 0);
    assert_event_type_count(&droid_events, EventType::Notice, 1);
    assert_events_have_provider_citations(&droid_events);
    assert_search_hits_provider(
        &store,
        "droid jsonl oracle prompt",
        CaptureProvider::FactoryAiDroid,
    );
    assert_search_misses(&store, "DROID_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH");
    let droid_child =
        stored_provider_session_id(&store, CaptureProvider::FactoryAiDroid, "droid-child");
    assert_eq!(
        store.get_session(droid_child).unwrap().parent_session_id,
        Some(droid_parent)
    );
    let droid_second = import_factory_ai_droid_sessions(
        &droid,
        &mut store,
        FactoryAiDroidImportOptions {
            ..FactoryAiDroidImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(droid_second.failed, 0, "{:?}", droid_second.failures);
    assert_eq!(droid_second.imported_sessions, 0);
    assert_eq!(droid_second.imported_events, 0);
    assert_eq!(droid_second.imported_edges, 0);
    assert_eq!(droid_second.skipped_sessions, 2);
    assert_eq!(droid_second.skipped_events, 5);
    assert_eq!(droid_second.skipped_edges, 0);

    let copilot = write_copilot_smoke_fixture(&temp);
    let copilot_summary = import_copilot_cli_session_events(
        &copilot,
        &mut store,
        CopilotCliImportOptions {
            ..CopilotCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(copilot_summary.failed, 0);
    assert_eq!(copilot_summary.imported_sessions, 2);
    assert_eq!(copilot_summary.imported_events, 6);
    let copilot_events = store
        .events_for_session(stored_provider_session_id(
            &store,
            CaptureProvider::CopilotCli,
            "copilot-root",
        ))
        .unwrap();
    assert_eq!(copilot_events.len(), 4);
    assert_event_type_count(&copilot_events, EventType::Message, 2);
    assert_event_type_count(&copilot_events, EventType::ToolCall, 1);
    assert_event_type_count(&copilot_events, EventType::ToolOutput, 0);
    assert_event_type_count(&copilot_events, EventType::Notice, 1);
    assert_events_have_provider_citations(&copilot_events);
    assert_search_hits_provider(&store, "running", CaptureProvider::CopilotCli);
    assert_search_misses(&store, "COPILOT_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH");
    stored_provider_session_id(&store, CaptureProvider::CopilotCli, "copilot-child");
    assert_search_hits_provider(
        &store,
        "copilot child oracle prompt",
        CaptureProvider::CopilotCli,
    );
    assert!(store.export_archive().unwrap().runs.is_empty());

    let copilot_second = import_copilot_cli_session_events(
        &copilot,
        &mut store,
        CopilotCliImportOptions {
            ..CopilotCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(copilot_second.failed, 0, "{:?}", copilot_second.failures);
    assert_eq!(copilot_second.imported_sessions, 0);
    assert_eq!(copilot_second.imported_events, 0);
    assert_eq!(copilot_second.skipped_sessions, 2);
    assert_eq!(copilot_second.skipped_events, 6);
}

#[test]
fn native_jsonl_tree_rejects_oversized_record_and_continues_session() {
    let temp = tempdir();
    let chats = temp.path().join("gemini/.gemini/tmp/project/chats");
    fs::create_dir_all(&chats).unwrap();
    let path = chats.join("oversized-gemini.jsonl");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        jsonl_line(json!({
            "sessionId": "gemini-oversized",
            "startTime": "2026-07-04T15:00:00Z",
            "directories": ["/workspace"]
        }))
        .as_bytes(),
    );
    bytes.extend_from_slice(&oversized_jsonl_line());
    bytes.extend_from_slice(
        jsonl_line(json!({
            "id": "gemini-after-oversized",
            "timestamp": "2026-07-04T15:00:01Z",
            "type": "user",
            "content": "after oversized gemini"
        }))
        .as_bytes(),
    );
    fs::write(&path, bytes).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_gemini_cli_history(
        temp.path().join("gemini/.gemini"),
        &mut store,
        GeminiCliImportOptions {
            source_path: Some(temp.path().join("gemini/.gemini")),
            imported_at: "2026-07-04T15:30:00Z".parse().unwrap(),
            ..GeminiCliImportOptions::default()
        },
    )
    .unwrap();

    assert_structural_oversize_failure(&summary, 2);
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.skipped_sessions, 1);
    assert_eq!(summary.skipped_events, 0);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Gemini, "gemini-oversized");
    let rendered = serde_json::to_string(&store.events_for_session(session_id).unwrap()).unwrap();
    assert!(rendered.contains("after oversized gemini"));
}

#[test]
fn native_jsonl_tree_imports_qwen_and_kimi_smokes_are_idempotent() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let qwen = write_qwen_smoke_fixture(&temp);
    let qwen_summary = import_qwen_code_history(
        &qwen,
        &mut store,
        QwenCodeImportOptions {
            ..QwenCodeImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(qwen_summary.failed, 0, "{:?}", qwen_summary.failures);
    assert_eq!(qwen_summary.imported_sessions, 1);
    assert_eq!(qwen_summary.imported_events, 2);

    let qwen_events = store
        .events_for_session(stored_provider_session_id(
            &store,
            CaptureProvider::QwenCode,
            "qwen-smoke",
        ))
        .unwrap();
    assert_eq!(qwen_events.len(), 2);
    assert_event_type_count(&qwen_events, EventType::Message, 1);
    assert_event_type_count(&qwen_events, EventType::ToolCall, 1);
    assert_event_type_count(&qwen_events, EventType::ToolOutput, 0);
    assert_events_have_provider_citations(&qwen_events);
    let qwen_rendered = serde_json::to_string(&qwen_events).unwrap();
    assert!(qwen_rendered.contains("qwen jsonl oracle prompt"));
    assert!(qwen_rendered.contains("src/qwen_oracle.txt"));
    assert_search_hits_provider(
        &store,
        "qwen jsonl oracle prompt",
        CaptureProvider::QwenCode,
    );
    assert_search_misses(&store, "QWEN_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH");
    assert!(store.export_archive().unwrap().runs.is_empty());

    let qwen_second = import_qwen_code_history(
        &qwen,
        &mut store,
        QwenCodeImportOptions {
            ..QwenCodeImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(qwen_second.failed, 0, "{:?}", qwen_second.failures);
    assert_eq!(qwen_second.imported_sessions, 0);
    assert_eq!(qwen_second.imported_events, 0);
    assert_eq!(qwen_second.skipped_sessions, 1);
    assert_eq!(qwen_second.skipped_events, 2);

    let kimi = write_kimi_smoke_fixture(&temp);
    let kimi_summary = import_kimi_code_cli_history(
        &kimi,
        &mut store,
        KimiCodeCliImportOptions {
            ..KimiCodeCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(kimi_summary.failed, 0, "{:?}", kimi_summary.failures);
    assert_eq!(kimi_summary.imported_sessions, 2);
    assert_eq!(kimi_summary.imported_events, 6);
    assert_eq!(kimi_summary.imported_edges, 1);

    let kimi_events = store
        .events_for_session(stored_provider_session_id(
            &store,
            CaptureProvider::KimiCodeCli,
            "kimi-smoke",
        ))
        .unwrap();
    assert_eq!(kimi_events.len(), 4);
    assert_event_type_count(&kimi_events, EventType::Message, 2);
    assert_event_type_count(&kimi_events, EventType::ToolCall, 1);
    assert_event_type_count(&kimi_events, EventType::ToolOutput, 0);
    assert_event_type_count(&kimi_events, EventType::Notice, 1);
    assert_events_have_provider_citations(&kimi_events);
    let kimi_rendered = serde_json::to_string(&kimi_events).unwrap();
    assert!(kimi_rendered.contains("kimi jsonl oracle prompt"));
    assert!(kimi_rendered.contains("src/kimi_oracle.txt"));
    assert!(!kimi_rendered.contains("usage record"));
    assert_search_hits_provider(
        &store,
        "kimi jsonl oracle prompt",
        CaptureProvider::KimiCodeCli,
    );
    assert_search_misses(&store, "usage record");
    assert_search_misses(&store, "KIMI_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH");

    let kimi_child = stored_provider_session_id(
        &store,
        CaptureProvider::KimiCodeCli,
        "kimi-smoke/agents/agent-1",
    );
    let kimi_parent =
        stored_provider_session_id(&store, CaptureProvider::KimiCodeCli, "kimi-smoke");
    assert_eq!(
        store.get_session(kimi_child).unwrap().parent_session_id,
        Some(kimi_parent)
    );
    assert!(store.runs_for_session(kimi_parent).unwrap().is_empty());

    let kimi_second = import_kimi_code_cli_history(
        &kimi,
        &mut store,
        KimiCodeCliImportOptions {
            ..KimiCodeCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(kimi_second.failed, 0, "{:?}", kimi_second.failures);
    assert_eq!(kimi_second.imported_sessions, 0);
    assert_eq!(kimi_second.imported_events, 0);
    assert_eq!(kimi_second.imported_edges, 0);
    assert_eq!(kimi_second.skipped_sessions, 2);
    assert_eq!(kimi_second.skipped_events, 6);
}

#[test]
fn native_kimi_rejects_oversized_wire_record() {
    let temp = tempdir();
    let kimi = write_kimi_smoke_fixture(&temp);

    let wire_path = kimi.join("sessions/wd_demo_abc123/kimi-smoke/agents/main/wire.jsonl");
    let original_wire = fs::read(&wire_path).unwrap();
    let first_line_end = original_wire
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap();
    let mut wire_bytes = Vec::new();
    wire_bytes.extend_from_slice(&original_wire[..first_line_end]);
    wire_bytes.extend_from_slice(&oversized_jsonl_line());
    wire_bytes.extend_from_slice(&original_wire[first_line_end..]);
    fs::write(&wire_path, wire_bytes).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_kimi_code_cli_history(
        &kimi,
        &mut store,
        KimiCodeCliImportOptions {
            imported_at: "2026-07-04T15:30:00Z".parse().unwrap(),
            ..KimiCodeCliImportOptions::default()
        },
    )
    .unwrap();

    assert_structural_oversize_failure(&summary, 2);
    assert_eq!(summary.skipped, 2);
    assert_eq!(summary.skipped_sessions, 2);
    assert_eq!(summary.skipped_events, 0);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 6);
    let session_id = stored_provider_session_id(&store, CaptureProvider::KimiCodeCli, "kimi-smoke");
    let source = store
        .capture_source_by_external_session(CaptureProvider::KimiCodeCli, "kimi-smoke")
        .unwrap()
        .unwrap();
    assert_eq!(source.descriptor.cwd.as_deref(), Some("/workspace/kimi"));
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(
        events.len(),
        4,
        "main wire events should resume after the oversized record"
    );
    assert_event_type_count(&events, EventType::ToolCall, 1);
    assert_event_type_count(&events, EventType::ToolOutput, 0);
    assert!(store.runs_for_session(session_id).unwrap().is_empty());
}

#[test]
fn native_jsonl_tree_rejects_headerless_native_files() {
    let temp = tempdir();
    let root = temp.path().join("gemini/.gemini/tmp/project/chats");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("headerless.jsonl"),
        "{\"type\":\"user\",\"content\":\"missing session header\"}\n",
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_gemini_cli_history(
        temp.path().join("gemini/.gemini"),
        &mut store,
        GeminiCliImportOptions {
            ..GeminiCliImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert_eq!(summary.imported_events, 0);
    assert_eq!(summary.failures[0].line, 1);
    assert!(summary.failures[0]
        .error
        .ends_with(": record appeared before an importable native JSONL session header"));
}

#[test]
fn native_jsonl_tree_accepts_empty_native_files_without_scaffolding() {
    let temp = tempdir();
    let root = temp.path().join("gemini/.gemini/tmp/project/chats");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("empty.jsonl"), "").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_gemini_cli_history(
        temp.path().join("gemini/.gemini"),
        &mut store,
        GeminiCliImportOptions {
            ..GeminiCliImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert!(summary.failures.is_empty());
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_jsonl_tree_tolerates_unimportable_siblings_for_shared_providers() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let gemini = write_gemini_smoke_fixture(&temp);
    write_unimportable_jsonl_siblings(
        &temp.path().join("gemini/.gemini/tmp/project/chats"),
        "gemini",
    );
    let gemini_summary = import_gemini_cli_history(
        &gemini,
        &mut store,
        GeminiCliImportOptions {
            ..GeminiCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(gemini_summary.failed, 2, "{:?}", gemini_summary.failures);
    assert_eq!(gemini_summary.imported_sessions, 2);
    assert_eq!(gemini_summary.imported_events, 5);
    assert_native_jsonl_failures_include_headerless_and_malformed(&gemini_summary);

    let droid = write_droid_smoke_fixture(&temp);
    write_unimportable_jsonl_siblings(&temp.path().join("droid/sessions/project"), "droid");
    let droid_summary = import_factory_ai_droid_sessions(
        &droid,
        &mut store,
        FactoryAiDroidImportOptions {
            ..FactoryAiDroidImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(droid_summary.failed, 2, "{:?}", droid_summary.failures);
    assert_eq!(droid_summary.imported_sessions, 2);
    assert_eq!(droid_summary.imported_events, 5);
    assert_native_jsonl_failures_include_headerless_and_malformed(&droid_summary);

    let copilot = write_copilot_smoke_fixture(&temp);
    write_unimportable_copilot_siblings(&temp.path().join("copilot/session-state"));
    let copilot_summary = import_copilot_cli_session_events(
        &copilot,
        &mut store,
        CopilotCliImportOptions {
            ..CopilotCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(copilot_summary.failed, 2, "{:?}", copilot_summary.failures);
    assert_eq!(copilot_summary.imported_sessions, 2);
    assert_eq!(copilot_summary.imported_events, 6);
    assert_native_jsonl_failures_include_headerless_and_malformed(&copilot_summary);
}

fn assert_native_jsonl_failures_include_headerless_and_malformed(summary: &ProviderImportSummary) {
    assert!(summary
        .failures
        .iter()
        .any(|failure| failure.error.contains("malformed JSONL: ")));
    assert!(summary.failures.iter().any(|failure| failure
        .error
        .ends_with(": record appeared before an importable native JSONL session header")));
}
