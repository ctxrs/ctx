#[test]
fn tool_result_retains_only_bounded_artifact_identifiers() {
    let event = claude_event(
        &json!({
            "type": "user",
            "uuid": "result-1",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "call-1",
                    "content": "Created commit 0123456789abcdef0123456789abcdef01234567 secret-prose-oracle"
                }]
            }
        }),
        1,
        "2026-07-21T00:00:00Z".parse().unwrap(),
    )
    .unwrap();
    assert_eq!(event.event_type, EventType::ToolOutput);
    let rendered = event.payload.to_string();
    assert!(rendered.contains("0123456789abcdef0123456789abcdef01234567"));
    assert!(rendered.contains("call-1"));
    assert!(!rendered.contains("Created commit"));
    assert!(!rendered.contains("secret-prose-oracle"));
    assert_eq!(event.payload["result_outcome"], Value::Null);
}

#[test]
fn tool_result_uses_only_explicit_native_outcome_semantics() {
    let make_event = |tool_use_result: Value| {
        claude_event(
            &json!({
                "type": "user",
                "uuid": "result-1",
                "message": {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "call-1",
                        "content": "bounded result"
                    }]
                },
                "toolUseResult": tool_use_result,
            }),
            1,
            "2026-07-21T00:00:00Z".parse().unwrap(),
        )
        .unwrap()
    };

    let absent = make_event(json!({"stdout": "ordinary output"}));
    assert_eq!(absent.payload["result_outcome"], Value::Null);

    let false_error_flag = make_event(json!({"is_error": false}));
    assert_eq!(false_error_flag.payload["result_outcome"], Value::Null);

    let failed = make_event(json!({"is_error": true}));
    assert_eq!(failed.payload["result_outcome"], "failure");

    let succeeded = make_event(json!({"exitCode": 0}));
    assert_eq!(succeeded.payload["result_outcome"], "success");
}
