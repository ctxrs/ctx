use super::*;

pub(super) fn complete_output_rows(
    raw_ordinal: u64,
    locator: &ClaudePhysicalLocator,
    native_record_id: Option<String>,
    timestamp: Option<String>,
    outputs: &[ClaudeOutputDescriptor],
    value: &Value,
) -> Vec<ClaudeRetainedRow> {
    let blocks = value
        .pointer("/message/content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let result_blocks = blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        .collect::<Vec<_>>();
    let mut consumed_result_blocks = vec![false; result_blocks.len()];
    outputs
        .iter()
        .map(|output| {
            let block = output.call_id.as_deref().and_then(|call_id| {
                result_blocks
                    .iter()
                    .enumerate()
                    .find(|(index, block)| {
                        !consumed_result_blocks[*index]
                            && block.get("tool_use_id").and_then(Value::as_str) == Some(call_id)
                    })
                    .map(|(index, block)| {
                        consumed_result_blocks[index] = true;
                        *block
                    })
            });
            let outcome = match output.outcome.outcome {
                OutputOutcome::Success => ClaudeOutputOutcome::Success,
                OutputOutcome::Failure => ClaudeOutputOutcome::Failure,
                OutputOutcome::Timeout => ClaudeOutputOutcome::Timeout,
                OutputOutcome::Unknown => ClaudeOutputOutcome::Unknown,
            };
            let identity = identity(raw_ordinal, u64::from(output.subrecord_index));
            ClaudeRetainedRow {
                identity,
                native_order: order(identity),
                native_record_id: native_record_id.clone(),
                parent_native_record_id: None,
                kind: ClaudeEventKind::ToolOutput,
                role: Some("tool".to_owned()),
                occurred_at: timestamp.clone(),
                body: None,
                body_sha256: None,
                body_text_retention: None,
                tool_call: None,
                tool_result: Some(ClaudeToolResult {
                    call_id: output.call_id.clone(),
                    outcome,
                    exit_code: output.outcome.exit_code,
                    duration_ms: output.outcome.duration_ms,
                    content: block
                        .and_then(|block| block.get("content"))
                        .cloned()
                        .unwrap_or(Value::Null),
                    tool_use_result: value.get("toolUseResult").cloned(),
                }),
                locator: locator.clone(),
            }
        })
        .collect()
}
