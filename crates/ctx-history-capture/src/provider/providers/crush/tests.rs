use serde_json::json;

use super::projection::crush_normalized_result_content;

#[test]
fn result_content_uses_only_ordered_schema_owned_fields() {
    let parts = json!([
        {"type": "text", "data": {"output": "not a result"}},
        {"type": "tool_result", "data": {
            "content": "tool body",
            "output": "lower priority"
        }},
        {"type": "shell_command", "data": {
            "stdout": "shell body",
            "stderr": "lower priority"
        }},
        {"type": "unknown", "data": {"output": "not discovered"}}
    ]);
    assert_eq!(
        crush_normalized_result_content(&parts),
        Some("tool body\nshell body".to_owned())
    );
}
