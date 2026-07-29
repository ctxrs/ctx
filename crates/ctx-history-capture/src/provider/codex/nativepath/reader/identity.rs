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
    context
}

pub(super) fn core_page_identity(page: &CodexNativePage) -> Result<CodexNativePageIdentity> {
    let mut hasher = Sha256::new();
    hasher.update(CODEX_SOURCE_BACKED_PAGE_IDENTITY_DOMAIN);
    hash_frontier(&mut hasher, &page.expected_frontier);
    hash_frontier(&mut hasher, &page.next_safe_frontier);
    hash_usize(&mut hasher, page.source_backed_rows.len())?;
    for row in &page.source_backed_rows {
        hasher.update(row.raw_ordinal.to_le_bytes());
        hasher.update(row.source_record.byte_offset.to_le_bytes());
        hasher.update(row.source_record.byte_length.to_le_bytes());
        hasher.update(row.source_record.record_digest);
        hasher.update(row.occurred_at.timestamp().to_le_bytes());
        hasher.update(row.occurred_at.timestamp_subsec_nanos().to_le_bytes());
        hash_text(&mut hasher, row.event_type.as_str())?;
        hash_optional_text(&mut hasher, row.role.map(|role| role.as_str()))?;
        hash_text(&mut hasher, &row.lexical_body)?;
        hash_usize(&mut hasher, row.touched_paths.len())?;
        for path in &row.touched_paths {
            hash_text(&mut hasher, path)?;
        }
    }
    hasher.update(page.physical_records.to_le_bytes());
    hash_usize(&mut hasher, page.serialized_bytes)?;
    hasher.update([u8::from(page.terminal)]);
    Ok(CodexNativePageIdentity(hasher.finalize().into()))
}

pub(super) fn hash_frontier(hasher: &mut Sha256, frontier: &CodexNativeFrontier) {
    hasher.update(frontier.complete_prefix_end.to_le_bytes());
    hasher.update(frontier.next_raw_ordinal.to_le_bytes());
    hasher.update(frontier.complete_prefix_sha256);
}

pub(super) fn hash_text(hasher: &mut Sha256, value: &str) -> Result<()> {
    hash_bytes(hasher, value.as_bytes())
}

pub(super) fn hash_bytes(hasher: &mut Sha256, value: &[u8]) -> Result<()> {
    let len = u64::try_from(value.len())
        .map_err(|_| CaptureError::SystemInvariant("Codex page identity length exceeds u64"))?;
    hasher.update(len.to_le_bytes());
    hasher.update(value);
    Ok(())
}

pub(super) fn hash_usize(hasher: &mut Sha256, value: usize) -> Result<()> {
    let value = u64::try_from(value)
        .map_err(|_| CaptureError::SystemInvariant("Codex page count exceeds u64"))?;
    hasher.update(value.to_le_bytes());
    Ok(())
}

pub(super) fn hash_optional_text(hasher: &mut Sha256, value: Option<&str>) -> Result<()> {
    match value {
        Some(value) => {
            hasher.update([1]);
            hash_text(hasher, value)
        }
        None => {
            hasher.update([0]);
            Ok(())
        }
    }
}
