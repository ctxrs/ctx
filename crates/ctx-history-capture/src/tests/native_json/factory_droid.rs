use crate::tests::support::assertions::{
    assert_event_type_count, assert_event_with_role, assert_events_have_provider_citations,
};
use crate::tests::support::paths::tempdir;
use crate::tests::support::provider_state::stored_provider_session_id;
use crate::{import_factory_ai_droid_sessions, FactoryAiDroidImportOptions};
use ctx_history_core::{CaptureProvider, EventRole, EventType};
use ctx_history_store::Store;
use std::fs;

#[test]
fn native_factory_ai_droid_supports_new_session_format() {
    let temp = tempdir();
    let root = temp.path().join("droid/sessions/project");
    fs::create_dir_all(&root).unwrap();

    // Test the new format: "id" instead of "sessionId", nested message.content/role
    let new_content = concat!(
        "{\"type\":\"session_start\",\"id\":\"droid-new\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\",\"model\":\"factory/droid\"}\n",
        "{\"type\":\"message\",\"id\":\"msg-1\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"droid new format prompt\"}]}}\n",
        "{\"type\":\"message\",\"id\":\"msg-2\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"droid_worker\"}]}}\n",
        "{\"type\":\"message\",\"id\":\"msg-3\",\"timestamp\":\"2026-06-24T12:00:03Z\",\"message\":{\"role\":\"tool\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"tool-1\",\"is_error\":false,\"content\":\"result\"}]}}\n",
    );
    fs::write(root.join("droid-new-format.jsonl"), new_content).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_factory_ai_droid_sessions(
        &root,
        &mut store,
        FactoryAiDroidImportOptions {
            source_path: Some(root.clone()),
            ..FactoryAiDroidImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 4);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::FactoryAiDroid, "droid-new");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 4);
    assert_event_type_count(&events, EventType::Message, 1);
    assert_event_type_count(&events, EventType::ToolCall, 1);
    assert_event_type_count(&events, EventType::ToolOutput, 1);
    assert_event_with_role(&events, EventType::ToolOutput, EventRole::Tool);
    assert_events_have_provider_citations(&events);

    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("droid new format prompt"));
    assert!(rendered.contains("droid_worker"));
    assert!(!rendered.contains("DROID_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH"));
}

#[test]
fn native_factory_ai_droid_supports_legacy_session_format() {
    let temp = tempdir();
    let root = temp.path().join("droid/sessions/project");
    fs::create_dir_all(&root).unwrap();

    // Test the legacy format: "sessionId" at root, "content" and "role" at message root
    let legacy_content = concat!(
        "{\"type\":\"session_start\",\"sessionId\":\"droid-legacy\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\",\"model\":\"factory/droid\"}\n",
        "{\"type\":\"message\",\"id\":\"msg-1\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"droid legacy prompt\"}]}\n",
        "{\"type\":\"message\",\"id\":\"msg-2\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"droid_worker\"}]}\n",
        "{\"type\":\"message\",\"id\":\"msg-3\",\"timestamp\":\"2026-06-24T12:00:03Z\",\"role\":\"tool\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"tool-1\",\"content\":\"legacy result\"}]}\n",
    );
    fs::write(root.join("droid-legacy.jsonl"), legacy_content).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_factory_ai_droid_sessions(
        &root,
        &mut store,
        FactoryAiDroidImportOptions {
            source_path: Some(root.clone()),
            ..FactoryAiDroidImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 4);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::FactoryAiDroid, "droid-legacy");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 4);
    assert_event_type_count(&events, EventType::Message, 1);
    assert_event_type_count(&events, EventType::ToolCall, 1);
    assert_event_type_count(&events, EventType::ToolOutput, 1);
    assert_event_with_role(&events, EventType::ToolOutput, EventRole::Tool);
    assert_events_have_provider_citations(&events);

    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("droid legacy prompt"));
    assert!(rendered.contains("droid_worker"));
    assert!(!rendered.contains("DROID_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH"));
}
