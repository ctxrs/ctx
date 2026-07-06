#[allow(unused_imports)]
use super::*;

#[test]

pub(crate) fn native_jsonl_tree_imports_gemini_droid_and_copilot_smokes() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let gemini = write_gemini_smoke_fixture(&temp);
    let gemini_summary = import_gemini_cli_history(
        &gemini,
        &mut store,
        GeminiCliImportOptions {
            allow_partial_failures: true,
            ..GeminiCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(gemini_summary.failed, 0);
    assert_eq!(gemini_summary.imported_sessions, 2);
    assert_eq!(gemini_summary.imported_edges, 1);

    let tabnine = provider_history_fixture("tabnine-cli/.tabnine/agent");
    let tabnine_summary = import_tabnine_cli_history(
        &tabnine,
        &mut store,
        TabnineCliImportOptions {
            allow_partial_failures: true,
            ..TabnineCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(tabnine_summary.failed, 0, "{:?}", tabnine_summary.failures);
    assert_eq!(tabnine_summary.imported_sessions, 2);
    assert_eq!(tabnine_summary.imported_events, 6);
    assert_eq!(tabnine_summary.imported_edges, 1);

    let tabnine_events = store
        .events_for_session(provider_session_uuid(
            CaptureProvider::Tabnine,
            "tabnine-root",
        ))
        .unwrap();
    assert!(tabnine_events
        .iter()
        .any(|event| event.role == Some(EventRole::Assistant)));
    assert!(tabnine_events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    let tabnine_rendered = serde_json::to_string(&tabnine_events).unwrap();
    assert!(tabnine_rendered.contains("tabnine jsonl oracle prompt"));
    assert!(tabnine_rendered.contains("tabnine jsonl oracle answer"));
    assert!(tabnine_rendered.contains("src/tabnine_oracle.txt"));

    let tabnine_child = provider_session_uuid(CaptureProvider::Tabnine, "tabnine-child");
    let tabnine_parent = provider_session_uuid(CaptureProvider::Tabnine, "tabnine-root");
    assert_eq!(
        store.get_session(tabnine_child).unwrap().parent_session_id,
        Some(tabnine_parent)
    );

    let droid = write_droid_smoke_fixture(&temp);
    let droid_summary = import_factory_ai_droid_sessions(
        &droid,
        &mut store,
        FactoryAiDroidImportOptions {
            allow_partial_failures: true,
            ..FactoryAiDroidImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(droid_summary.failed, 0);
    assert_eq!(droid_summary.imported_sessions, 2);
    assert_eq!(droid_summary.imported_edges, 1);

    let copilot = write_copilot_smoke_fixture(&temp);
    let copilot_summary = import_copilot_cli_session_events(
        &copilot,
        &mut store,
        CopilotCliImportOptions {
            allow_partial_failures: true,
            ..CopilotCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(copilot_summary.failed, 0);
    assert_eq!(copilot_summary.imported_sessions, 1);
    assert_eq!(copilot_summary.imported_events, 5);
}

pub(crate) fn write_unimportable_copilot_siblings(root: &Path) {
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

pub(crate) fn write_copilot_smoke_fixture(temp: &TempDir) -> PathBuf {
    let root = temp.path().join("copilot/session-state/copilot-root");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("events.jsonl"),
        concat!(
            "{\"id\":\"copilot-1\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"type\":\"session.start\",\"data\":{\"sessionId\":\"copilot-root\",\"startTime\":\"2026-06-24T12:00:00Z\",\"selectedModel\":\"gpt-5-mini\",\"context\":{\"cwd\":\"/workspace\"}}}\n",
            "{\"id\":\"copilot-2\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"type\":\"user.message\",\"data\":{\"content\":\"status\"}}\n",
            "{\"id\":\"copilot-3\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"type\":\"assistant.message\",\"data\":{\"content\":\"running\",\"toolRequests\":[{\"toolCallId\":\"tool-1\",\"name\":\"bash\"}]}}\n",
            "{\"id\":\"copilot-4\",\"timestamp\":\"2026-06-24T12:00:03Z\",\"type\":\"tool.execution_start\",\"data\":{\"toolCallId\":\"tool-1\",\"toolName\":\"bash\"}}\n",
            "{\"id\":\"copilot-5\",\"timestamp\":\"2026-06-24T12:00:04Z\",\"type\":\"tool.execution_complete\",\"data\":{\"toolCallId\":\"tool-1\",\"success\":true,\"result\":{\"content\":\"ok\"}}}\n",
        ),
    )
    .unwrap();
    temp.path().join("copilot/session-state")
}
