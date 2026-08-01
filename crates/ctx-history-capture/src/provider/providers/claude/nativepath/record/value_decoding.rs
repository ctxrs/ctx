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
    let message = value.get("message").unwrap_or(value);
    let direct_content = message
        .get("content")
        .filter(|_| result_blocks.is_empty() && outputs.len() == 1);
    let direct_call_id = ["tool_use_id", "toolUseId", "toolCallId"]
        .into_iter()
        .find_map(|key| message.get(key).and_then(Value::as_str))
        .filter(|call_id| !call_id.is_empty() && call_id.len() <= 256);
    let tool_use_result = value
        .get("toolUseResult")
        .or_else(|| message.get("toolUseResult"));
    outputs
        .iter()
        .map(|output| {
            let block = output
                .call_id
                .as_deref()
                .and_then(|call_id| {
                    result_blocks.iter().enumerate().find(|(index, block)| {
                        !consumed_result_blocks[*index]
                            && block.get("tool_use_id").and_then(Value::as_str) == Some(call_id)
                    })
                })
                .or_else(|| {
                    output
                        .call_id
                        .is_none()
                        .then(|| {
                            result_blocks
                                .iter()
                                .enumerate()
                                .find(|(index, _)| !consumed_result_blocks[*index])
                        })
                        .flatten()
                })
                .map(|(index, block)| {
                    consumed_result_blocks[index] = true;
                    *block
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
                    call_id: output
                        .call_id
                        .clone()
                        .or_else(|| direct_call_id.map(str::to_owned)),
                    outcome,
                    exit_code: output.outcome.exit_code,
                    duration_ms: output.outcome.duration_ms,
                    content: block
                        .and_then(|block| block.get("content"))
                        .cloned()
                        .or_else(|| output.content.clone())
                        .or_else(|| direct_content.cloned())
                        .unwrap_or(Value::Null),
                    tool_use_result: tool_use_result.cloned(),
                }),
                locator: locator.clone(),
            }
        })
        .collect()
}
