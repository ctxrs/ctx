#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_jsonl_tree_imports_qwen_and_kimi_smokes_are_idempotent() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let qwen = write_qwen_smoke_fixture(&temp);
    let qwen_summary = import_qwen_code_history(
        &qwen,
        &mut store,
        QwenCodeImportOptions {
            allow_partial_failures: true,
            ..QwenCodeImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(qwen_summary.failed, 0, "{:?}", qwen_summary.failures);
    assert_eq!(qwen_summary.imported_sessions, 1);
    assert_eq!(qwen_summary.imported_events, 3);

    let qwen_events = store
        .events_for_session(provider_session_uuid(
            CaptureProvider::QwenCode,
            "qwen-smoke",
        ))
        .unwrap();
    assert_eq!(qwen_events.len(), 3);
    assert!(qwen_events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(qwen_events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));
    let qwen_rendered = serde_json::to_string(&qwen_events).unwrap();
    assert!(qwen_rendered.contains("qwen jsonl oracle prompt"));
    assert!(qwen_rendered.contains("src/qwen_oracle.txt"));

    let qwen_second = import_qwen_code_history(
        &qwen,
        &mut store,
        QwenCodeImportOptions {
            allow_partial_failures: true,
            ..QwenCodeImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(qwen_second.failed, 0, "{:?}", qwen_second.failures);
    assert_eq!(qwen_second.imported_sessions, 0);
    assert_eq!(qwen_second.imported_events, 0);

    let kimi = write_kimi_smoke_fixture(&temp);
    let kimi_summary = import_kimi_code_cli_history(
        &kimi,
        &mut store,
        KimiCodeCliImportOptions {
            allow_partial_failures: true,
            ..KimiCodeCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(kimi_summary.failed, 0, "{:?}", kimi_summary.failures);
    assert_eq!(kimi_summary.imported_sessions, 2);
    assert_eq!(kimi_summary.imported_events, 7);
    assert_eq!(kimi_summary.imported_edges, 1);

    let kimi_events = store
        .events_for_session(provider_session_uuid(
            CaptureProvider::KimiCodeCli,
            "kimi-smoke",
        ))
        .unwrap();
    assert_eq!(kimi_events.len(), 5);
    assert!(kimi_events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(kimi_events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));
    let kimi_rendered = serde_json::to_string(&kimi_events).unwrap();
    assert!(kimi_rendered.contains("kimi jsonl oracle prompt"));
    assert!(kimi_rendered.contains("src/kimi_oracle.txt"));

    let kimi_child =
        provider_session_uuid(CaptureProvider::KimiCodeCli, "kimi-smoke/agents/agent-1");
    let kimi_parent = provider_session_uuid(CaptureProvider::KimiCodeCli, "kimi-smoke");
    assert_eq!(
        store.get_session(kimi_child).unwrap().parent_session_id,
        Some(kimi_parent)
    );

    let kimi_second = import_kimi_code_cli_history(
        &kimi,
        &mut store,
        KimiCodeCliImportOptions {
            allow_partial_failures: true,
            ..KimiCodeCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(kimi_second.failed, 0, "{:?}", kimi_second.failures);
    assert_eq!(kimi_second.imported_sessions, 0);
    assert_eq!(kimi_second.imported_events, 0);
    assert_eq!(kimi_second.imported_edges, 0);
}

pub(crate) fn write_kimi_smoke_fixture(temp: &TempDir) -> PathBuf {
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
            "{\"type\":\"context.append_loop_event\",\"time\":1783170004000,\"event\":{\"type\":\"tool.result\",\"toolName\":\"Write\",\"output\":\"wrote src/kimi_oracle.txt\"}}\n",
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
