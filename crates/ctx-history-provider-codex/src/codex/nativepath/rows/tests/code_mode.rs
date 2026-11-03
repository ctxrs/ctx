use super::*;

#[test]
fn code_mode_output_captures_text_items_without_dropping_structured_content() {
    let payload = serde_json::json!({
        "type":"custom_tool_call_output", "call_id":"code-mode",
        "output":[
            {"type":"input_text", "text":"Script completed\nOutput:\n"},
            {"type":"input_image", "image_url":"data:image/png;base64,AA=="},
            {"type":"input_text", "text":"  synthetic result\n"}
        ]
    });
    let raw = serde_json::to_vec(&payload).unwrap();
    let audit = audit_codex_record(&raw).unwrap();
    let activity = codex_result_activity(
        Some("code-mode"),
        payload.get("output"),
        &payload,
        &audit,
        DateTime::<Utc>::UNIX_EPOCH,
    )
    .unwrap();
    let result = activity.result.unwrap();
    assert_eq!(
        result.text,
        ActivityTextCapture::Present {
            value: "Script completed\nOutput:\n\n  synthetic result\n".to_owned()
        }
    );
    assert_eq!(
        result.structured_content,
        ActivityJsonCapture::Present {
            value: payload["output"].clone()
        }
    );
}

#[test]
fn code_mode_ambiguous_output_selectors_withhold_result_authority() {
    for raw in [
        r#"{"type":"custom_tool_call_output","call_id":"call","output":[],"output":[{"type":"input_text","text":"last"}]}"#,
        r#"{"type":"custom_tool_call_output","call_id":"call","output":[{"type":"input_text","text":"first","text":"last"}]}"#,
        r#"{"type":"custom_tool_call_output","call_id":"call","output":[{"type":"input_image","type":"input_text","text":"last"}]}"#,
    ] {
        let payload: Value = serde_json::from_str(raw).unwrap();
        let audit = audit_codex_record(raw.as_bytes()).unwrap();
        let result = codex_result_activity(
            Some("call"),
            payload.get("output"),
            &payload,
            &audit,
            DateTime::<Utc>::UNIX_EPOCH,
        )
        .unwrap()
        .result
        .unwrap();
        assert_eq!(result.text, ActivityTextCapture::Unavailable);
        assert_eq!(result.structured_content, ActivityJsonCapture::Unavailable);
    }
}

#[test]
fn code_mode_output_ignores_nontext_items_and_preserves_whitespace() {
    for (output, expected) in [
        (serde_json::json!([]), None),
        (
            serde_json::json!([{"type":"input_image","text":"not text"}]),
            None,
        ),
        (serde_json::json!([{"type":"input_text","text":42}]), None),
        (
            serde_json::json!([{"type":"input_text","text":"  \n"}]),
            Some("  \n"),
        ),
        (
            serde_json::json!([{"type":"input_text","text":"first"},{"type":"input_text","text":"second"}]),
            Some("first\nsecond"),
        ),
    ] {
        assert_eq!(codex_result_text(&output).as_deref(), expected);
    }
}
