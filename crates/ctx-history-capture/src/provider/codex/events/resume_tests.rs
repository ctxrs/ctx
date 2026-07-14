use super::*;

#[test]
fn codex_tool_call_resume_frontier_evicts_oldest_with_bounded_encoding() {
    let mut contexts = CodexToolCallContexts::default();
    for index in 0..(CODEX_RESUME_MAX_PENDING_TOOL_CALLS + 9) {
        contexts.insert(
            format!("call-{index:03}"),
            CodexToolCallContext {
                tool_name: "exec_command".to_owned(),
                command_preview: Some(format!("command-{index:03}")),
                arguments_preview: None,
            },
        );
    }

    let state = contexts.resume_state();
    assert_eq!(contexts.len(), CODEX_RESUME_MAX_PENDING_TOOL_CALLS);
    assert_eq!(state.dropped_tool_calls, 9);
    assert_eq!(state.pending_tool_calls[0].call_id, "call-009");
    assert_eq!(
        state.pending_tool_calls.last().unwrap().call_id,
        format!("call-{:03}", CODEX_RESUME_MAX_PENDING_TOOL_CALLS + 8)
    );
    assert!(state.encoded_len().unwrap() <= CODEX_RESUME_MAX_ENCODED_BYTES);

    let restarted = CodexToolCallContexts::from_resume_state(state.clone());
    assert_eq!(restarted.resume_state(), state);
}

#[test]
fn codex_tool_call_resume_frontier_evicts_an_oversized_context() {
    let mut contexts = CodexToolCallContexts::default();
    contexts.insert(
        "oversized".to_owned(),
        CodexToolCallContext {
            tool_name: "exec_command".to_owned(),
            command_preview: Some("x".repeat(CODEX_RESUME_MAX_ENCODED_BYTES)),
            arguments_preview: None,
        },
    );

    let state = contexts.resume_state();
    assert_eq!(contexts.len(), 0);
    assert_eq!(state.dropped_tool_calls, 1);
    assert!(state.pending_tool_calls.is_empty());
    assert!(state.encoded_len().unwrap() <= CODEX_RESUME_MAX_ENCODED_BYTES);
}
