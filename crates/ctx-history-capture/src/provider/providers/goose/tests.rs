use serde_json::json;

use super::normalization::{goose_normalized_result_content, goose_output_projection};

#[test]
fn goose_result_content_is_unbounded_ordered_and_does_not_search_wrappers() {
    let long = "x".repeat(crate::PROVIDER_MAX_TEXT_CHARS + 37);
    let content = json!([
        {"type": "text", "output": "not a result"},
        {"type": "toolResponse", "toolResult": long.clone(), "result": "lower priority"},
        [{"type": "toolResponse", "content": ["second", 2]}],
        {"type": "wrapper", "content": {"type": "toolResponse", "result": "not discovered"}}
    ]);

    assert_eq!(
        goose_normalized_result_content(&content),
        Some(format!("{long}\nsecond\n2"))
    );
    assert_eq!(
        goose_normalized_result_content(&json!({
            "wrapper": {"type": "toolResponse", "result": "not discovered"}
        })),
        None
    );
}

#[test]
fn goose_output_body_and_outcome_use_the_same_direct_tool_responses() {
    let content = json!([
        {
            "type": "toolResponse",
            "toolCallId": "call-1",
            "toolResult": "exact failure body",
            "exitCode": 9,
            "durationMs": 42
        },
        {
            "type": "wrapper",
            "content": {
                "type": "toolResponse",
                "toolResult": "must not affect body or outcome",
                "success": true
            }
        }
    ]);

    let output = goose_output_projection(&content);
    assert_eq!(
        goose_normalized_result_content(&content).as_deref(),
        Some("exact failure body")
    );
    assert_eq!(output.call_id.as_deref(), Some("call-1"));
    assert_eq!(output.outcome.outcome, crate::OutputOutcome::Failure);
    assert_eq!(output.outcome.exit_code, Some(9));
    assert_eq!(output.outcome.duration_ms, Some(42));
}

mod source_backed;
