use super::*;

#[test]
fn released_v025_ordinals_and_hashes_match_cline_roo_flattening() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("roo-data");
    let task = root.join("tasks").join("roo-task");
    fs::create_dir_all(&task).expect("task directory");
    write_json(
        &task.join("history_item.json"),
        &json!({"id": "released-task", "task": "released compatibility"}),
    );
    write_json(
        &task.join("api_conversation_history.json"),
        &json!([
            {"id": "api-native", "role": "user", "content": "api id"},
            {"role": "assistant", "content": "api ordinal"}
        ]),
    );
    write_json(
        &task.join("ui_messages.json"),
        &json!([{"id": "ui-native", "type": "say", "text": "ui"}]),
    );
    write_json(
        &task.join("claude_messages.json"),
        &json!([{"role": "user", "content": "fallback"}]),
    );

    let result = read_all_roo(&root, ClineNativeProfile::CoreOnly);
    let identities = result
        .pages
        .iter()
        .filter_map(|page| {
            page.core.events.first().map(|event| {
                super::vertical::released_v025_event_identity(&page.source, event)
                    .expect("released identity")
                    .expect("first released subrecord")
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        identities,
        [
            (
                0,
                "released-task:api_conversation_history:api-native".to_owned()
            ),
            (
                1,
                "released-task:api_conversation_history:api_conversation_history-1".to_owned()
            ),
            (2, "released-task:ui_messages:ui-native".to_owned()),
            (
                3,
                "released-task:claude_messages:claude_messages-0".to_owned()
            ),
        ]
    );
}
