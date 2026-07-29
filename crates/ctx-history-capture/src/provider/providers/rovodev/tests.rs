use serde_json::json;

use super::rovodev_result_content;

#[test]
fn result_profile_selects_only_explicit_tool_result_parts() {
    let message = json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "not a result"},
            {"type": "tool_result", "content": [
                {"text": "first"},
                {"output": "second"}
            ]}
        ]
    });
    assert_eq!(
        rovodev_result_content(&message).as_deref(),
        Some("first\nsecond")
    );
}
