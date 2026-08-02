use super::claude_tool_result_body;
use crate::provider::providers::claude::nativepath::{
    invocation_evidence::normalized_tool_call_body,
    rows::{ClaudeEventKind, ClaudeRetainedRow},
};

pub(super) fn lexical_body(row: &ClaudeRetainedRow) -> String {
    let text = row
        .body
        .clone()
        .or_else(|| {
            row.tool_call.as_ref().and_then(|call| {
                normalized_tool_call_body(
                    call.call_id.as_deref(),
                    call.tool_name.as_deref(),
                    &call.input,
                )
            })
        })
        .or_else(|| row.tool_result.as_ref().map(claude_tool_result_body))
        .unwrap_or_else(|| event_kind(row.kind).to_owned());
    if text.trim().is_empty() {
        event_kind(row.kind).to_owned()
    } else {
        text
    }
}

pub(super) fn event_kind(kind: ClaudeEventKind) -> &'static str {
    match kind {
        ClaudeEventKind::Message => "message",
        ClaudeEventKind::Summary => "summary",
        ClaudeEventKind::Notice => "notice",
        ClaudeEventKind::ToolCall => "tool_call",
        ClaudeEventKind::ToolOutput => "tool_output",
    }
}
