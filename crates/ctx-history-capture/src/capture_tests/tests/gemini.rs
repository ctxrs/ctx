#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_jsonl_tree_skips_headerless_native_files() {
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
            allow_partial_failures: true,
            ..GeminiCliImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert_eq!(summary.imported_events, 0);
    assert!(summary.failures[0]
        .error
        .contains("no importable native JSONL session header"));
}

#[test]
pub(crate) fn native_jsonl_tree_tolerates_unimportable_siblings_for_shared_providers() {
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
            allow_partial_failures: true,
            ..GeminiCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(gemini_summary.failed, 2, "{:?}", gemini_summary.failures);
    assert_eq!(gemini_summary.imported_sessions, 2);
    assert_eq!(gemini_summary.imported_events, 5);
    assert_provider_failures_include_headerless_and_malformed(&gemini_summary);

    let droid = write_droid_smoke_fixture(&temp);
    write_unimportable_jsonl_siblings(&temp.path().join("droid/sessions/project"), "droid");
    let droid_summary = import_factory_ai_droid_sessions(
        &droid,
        &mut store,
        FactoryAiDroidImportOptions {
            allow_partial_failures: true,
            ..FactoryAiDroidImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(droid_summary.failed, 2, "{:?}", droid_summary.failures);
    assert_eq!(droid_summary.imported_sessions, 2);
    assert_eq!(droid_summary.imported_events, 5);
    assert_provider_failures_include_headerless_and_malformed(&droid_summary);

    let copilot = write_copilot_smoke_fixture(&temp);
    write_unimportable_copilot_siblings(&temp.path().join("copilot/session-state"));
    let copilot_summary = import_copilot_cli_session_events(
        &copilot,
        &mut store,
        CopilotCliImportOptions {
            allow_partial_failures: true,
            ..CopilotCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(copilot_summary.failed, 2, "{:?}", copilot_summary.failures);
    assert_eq!(copilot_summary.imported_sessions, 1);
    assert_eq!(copilot_summary.imported_events, 5);
    assert_provider_failures_include_headerless_and_malformed(&copilot_summary);
}

pub(crate) fn write_gemini_smoke_fixture(temp: &TempDir) -> PathBuf {
    let chats = temp.path().join("gemini/.gemini/tmp/project/chats");
    let child_dir = chats.join("gemini-root");
    fs::create_dir_all(&child_dir).unwrap();
    fs::write(
        chats.join("session-root.jsonl"),
        concat!(
            "{\"sessionId\":\"gemini-root\",\"startTime\":\"2026-06-24T12:00:00Z\",\"kind\":\"main\",\"directories\":[\"/workspace\"]}\n",
            "{\"id\":\"gemini-user\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"type\":\"user\",\"content\":\"hi\"}\n",
            "{\"id\":\"gemini-tool\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"type\":\"gemini\",\"toolCalls\":[{\"id\":\"call-1\",\"name\":\"run_subagent\"}]}\n",
        ),
    )
    .unwrap();
    fs::write(
        child_dir.join("gemini-child.jsonl"),
        concat!(
            "{\"sessionId\":\"gemini-child\",\"startTime\":\"2026-06-24T12:00:03Z\",\"kind\":\"subagent\",\"directories\":[\"/workspace\"]}\n",
            "{\"id\":\"gemini-child-user\",\"timestamp\":\"2026-06-24T12:00:04Z\",\"type\":\"user\",\"content\":\"inspect\"}\n",
        ),
    )
    .unwrap();
    temp.path().join("gemini/.gemini")
}
