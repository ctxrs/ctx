use serde_json::json;

use super::*;

const TOOL_OUTPUT_BODY: &str = "AUGGIE_TOOL_OUTPUT_BODY_MUST_NOT_ENTER_CORE";
const UNKNOWN_BODY: &str = "AUGGIE_UNKNOWN_BODY_MUST_NOT_ENTER_CORE_OR_PRO";
const NUMERIC_BODY: &str = "AUGGIE_NUMERIC_BODY_MUST_NOT_ENTER_CORE_OR_PRO";

#[test]
fn certified_node_text_requires_an_exact_native_text_shape() {
    let exact = json!([
        {"text_node": {"content": "legacy text"}},
        {"type": 0, "text_node": {"content": "snake text"}},
        {"type": 0, "textNode": {"content": "camel text"}},
    ]);
    assert_eq!(
        auggie_nodes_text(Some(&exact)),
        Some("legacy text\nsnake text\ncamel text".to_owned())
    );

    for rejected in [
        json!([{"type": "text", "text_node": {"content": "unknown string kind"}}]),
        json!([{"type": 71, "text_node": {"content": NUMERIC_BODY}}]),
        json!([{"type": 0, "content": "generic content"}]),
        json!([{"type": 0, "text_node": {"content": 71}}]),
        json!([{
            "type": 0,
            "text_node": {"content": "apparently text"},
            "output": TOOL_OUTPUT_BODY,
        }]),
    ] {
        assert_eq!(auggie_nodes_text(Some(&rejected)), None);
    }

    assert_eq!(auggie_request_text(&json!({"message": UNKNOWN_BODY})), None);
    assert_eq!(
        auggie_response_text(&json!({"response": UNKNOWN_BODY})),
        None
    );
}
