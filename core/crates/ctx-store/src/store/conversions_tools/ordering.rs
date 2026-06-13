pub(super) fn summarize_session_turn_tool(tool: &SessionTurnTool) -> SessionTurnToolSummary {
    SessionTurnToolSummary {
        session_id: tool.session_id,
        tool_call_id: tool.tool_call_id.clone(),
        turn_id: tool.turn_id,
        tool_kind: tool.tool_kind.clone(),
        provider_tool_name: tool.provider_tool_name.clone(),
        title: tool.title.clone(),
        subtitle: tool.subtitle.clone(),
        status: tool.status.clone(),
        input_preview: tool_input_preview_from_value(tool.input_json.as_ref()),
        output_preview: tool.output_text.clone(),
        order_seq: tool.order_seq,
        first_event_seq: tool.first_event_seq,
        input_truncated: tool.input_truncated,
        input_original_bytes: tool.input_original_bytes,
        output_truncated: tool.output_truncated,
        output_original_bytes: tool.output_original_bytes,
        created_at: tool.created_at,
        updated_at: tool.updated_at,
    }
}

pub(super) fn compare_tool_summary_order(
    a: &SessionTurnToolSummary,
    b: &SessionTurnToolSummary,
) -> std::cmp::Ordering {
    a.order_seq
        .cmp(&b.order_seq)
        .then_with(|| a.created_at.cmp(&b.created_at))
        .then_with(|| a.tool_call_id.cmp(&b.tool_call_id))
}

pub(super) fn compare_tool_order(a: &SessionTurnTool, b: &SessionTurnTool) -> std::cmp::Ordering {
    a.order_seq
        .cmp(&b.order_seq)
        .then_with(|| a.created_at.cmp(&b.created_at))
        .then_with(|| a.tool_call_id.cmp(&b.tool_call_id))
}

fn read_event_order_seq(payload: &Value) -> Option<i64> {
    payload
        .get("order_seq")
        .and_then(Value::as_i64)
        .or_else(|| payload.get("orderSeq").and_then(Value::as_i64))
}
