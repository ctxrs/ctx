use super::*;

pub(super) fn probe_timestamp(
    probe: &CodexRecordProbe<'_>,
    fallback: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    match probe.timestamp.as_deref() {
        Some(timestamp) => DateTime::parse_from_rfc3339(timestamp)
            .ok()
            .map(|timestamp| timestamp.with_timezone(&Utc)),
        None => Some(fallback),
    }
}

pub(super) fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

pub(super) fn bound_tool_context(mut context: CodexToolCallContext) -> CodexToolCallContext {
    context.tool_name = truncate_utf8(&context.tool_name, MAX_CODEX_TOOL_NAME_BYTES);
    context.command_preview = context
        .command_preview
        .as_deref()
        .map(|value| truncate_utf8(value, MAX_CODEX_TOOL_PREVIEW_BYTES));
    context.arguments_preview = context
        .arguments_preview
        .as_deref()
        .map(|value| truncate_utf8(value, MAX_CODEX_TOOL_PREVIEW_BYTES));
    if context
        .exact_command
        .as_ref()
        .is_some_and(|value| value.len() > 1024 * 1024)
    {
        context.exact_command = None;
    }
    if context
        .session_cwd
        .as_ref()
        .is_some_and(|value| value.len() > 16 * 1024)
    {
        context.session_cwd = None;
    }
    if context
        .declared_workdir
        .as_ref()
        .is_some_and(|value| value.len() > 16 * 1024)
    {
        context.declared_workdir = None;
    }
    if context.continuation_cell_id.as_ref().is_some_and(|value| {
        value.len() > MAX_CODEX_CONTINUATION_CELL_ID_BYTES
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            })
    }) {
        context.continuation_cell_id = None;
    }
    if context.continuation_call_id_sha256.len() > MAX_CODEX_TOOL_CONTEXTS {
        context.continuation_capacity_exceeded = true;
        context
            .continuation_call_id_sha256
            .truncate(MAX_CODEX_TOOL_CONTEXTS);
    }
    context
}
