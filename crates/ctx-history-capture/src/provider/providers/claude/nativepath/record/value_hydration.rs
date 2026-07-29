use super::*;

pub(super) fn sparse_output_rows(
    raw_ordinal: u64,
    locator: &ClaudePhysicalLocator,
    native_record_id: Option<String>,
    timestamp: Option<String>,
    core_subrecord_base: u64,
    outputs: &[ClaudeOutputDescriptor],
) -> Vec<ClaudeRetainedRow> {
    outputs
        .iter()
        .filter_map(|output| {
            let outcome = match output.outcome.outcome {
                OutputOutcome::Failure => ClaudeOutputOutcome::Failure,
                OutputOutcome::Timeout => ClaudeOutputOutcome::Timeout,
                OutputOutcome::Success | OutputOutcome::Unknown => return None,
            };
            let subrecord_index =
                core_subrecord_base.saturating_add(u64::from(output.subrecord_index));
            let identity = identity(raw_ordinal, subrecord_index);
            Some(ClaudeRetainedRow {
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
                complete_body_ref: None,
                tool_call: None,
                sparse_output: Some(ClaudeSparseOutputDiagnostic {
                    call_id: output.call_id.clone(),
                    outcome,
                    exit_code: output.outcome.exit_code,
                    duration_ms: output.outcome.duration_ms,
                }),
                locator: locator.clone(),
            })
        })
        .collect()
}
