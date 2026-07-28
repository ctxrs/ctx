use super::*;

#[derive(Serialize)]
pub(super) struct CodexOutputSourceLocator<'a> {
    pub(super) source_root: &'a str,
    pub(super) source_path: &'a Path,
    pub(super) byte_start: u64,
    pub(super) byte_end_exclusive: u64,
    pub(super) raw_ordinal: u64,
}

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

pub(super) fn serialized_owner_bytes(owner: &CodexSessionRow) -> Result<usize> {
    Ok(serde_json::to_vec(owner)?.len().saturating_add(1))
}

pub(super) fn new_pro_page(expected_frontier: CodexNativeFrontier) -> CodexNativeProOutputPage {
    CodexNativeProOutputPage {
        identity: CodexNativeProOutputPageIdentity::default(),
        next_safe_frontier: expected_frontier.clone(),
        expected_frontier,
        outputs: Vec::new(),
        serialized_bytes: 0,
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct CodexLegacyPageIdentityOperations {
    pub(super) owner_json_serializations: u64,
    pub(super) row_json_serializations: u64,
}

pub(super) fn core_page_identity(
    page: &CodexNativePage,
) -> Result<(CodexNativePageIdentity, CodexLegacyPageIdentityOperations)> {
    if page.projection_mode == CodexProjectionMode::SourceBackedV0 {
        return source_backed_page_identity(page);
    }
    let mut hasher = Sha256::new();
    hasher.update(CODEX_CORE_PAGE_IDENTITY_DOMAIN);
    hash_frontier(&mut hasher, &page.expected_frontier);
    hash_frontier(&mut hasher, &page.next_safe_frontier);
    hash_optional_serialized(&mut hasher, page.owner.as_ref())?;
    hash_usize(&mut hasher, page.core_rows.len())?;
    for row in &page.core_rows {
        hash_serialized(&mut hasher, row)?;
    }
    hasher.update(page.physical_records.to_le_bytes());
    hash_usize(&mut hasher, page.serialized_bytes)?;
    hasher.update([u8::from(page.terminal)]);
    Ok((
        CodexNativePageIdentity(hasher.finalize().into()),
        CodexLegacyPageIdentityOperations {
            owner_json_serializations: u64::from(page.owner.is_some()),
            row_json_serializations: u64::try_from(page.core_rows.len()).unwrap_or(u64::MAX),
        },
    ))
}

fn source_backed_page_identity(
    page: &CodexNativePage,
) -> Result<(CodexNativePageIdentity, CodexLegacyPageIdentityOperations)> {
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
    Ok((
        CodexNativePageIdentity(hasher.finalize().into()),
        CodexLegacyPageIdentityOperations::default(),
    ))
}

pub(super) fn pro_page_identity(
    page: &CodexNativeProOutputPage,
) -> Result<CodexNativeProOutputPageIdentity> {
    let mut hasher = Sha256::new();
    hasher.update(CODEX_PRO_PAGE_IDENTITY_DOMAIN);
    hash_frontier(&mut hasher, &page.expected_frontier);
    hash_frontier(&mut hasher, &page.next_safe_frontier);
    hash_usize(&mut hasher, page.outputs.len())?;
    for output in &page.outputs {
        hash_pro_output(&mut hasher, output)?;
    }
    hash_usize(&mut hasher, page.serialized_bytes)?;
    Ok(CodexNativeProOutputPageIdentity(hasher.finalize().into()))
}

pub(super) fn hash_frontier(hasher: &mut Sha256, frontier: &CodexNativeFrontier) {
    hasher.update(frontier.complete_prefix_end.to_le_bytes());
    hasher.update(frontier.next_raw_ordinal.to_le_bytes());
    hasher.update(frontier.complete_prefix_sha256);
}

pub(super) fn hash_serialized<T: Serialize>(hasher: &mut Sha256, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    hash_bytes(hasher, &bytes)
}

pub(super) fn hash_optional_serialized<T: Serialize>(
    hasher: &mut Sha256,
    value: Option<&T>,
) -> Result<()> {
    match value {
        Some(value) => {
            hasher.update([1]);
            hash_serialized(hasher, value)
        }
        None => {
            hasher.update([0]);
            Ok(())
        }
    }
}

pub(super) fn hash_pro_output(hasher: &mut Sha256, output: &ProOutputObservation) -> Result<()> {
    hasher.update([match output.kind {
        OutputObservationKind::Command => 1,
        OutputObservationKind::Tool => 2,
    }]);
    hash_text(hasher, &output.coordinate.unit_key)?;
    hasher.update(output.coordinate.native_sequence.to_le_bytes());
    hash_optional_text(hasher, output.coordinate.native_record_id.as_deref())?;
    hash_optional_u64(hasher, output.coordinate.source_record_ordinal);
    hash_optional_u32(hasher, output.coordinate.source_record_subrecord_index);
    hash_optional_u64(hasher, output.coordinate.byte_start);
    hash_optional_u64(hasher, output.coordinate.byte_end_exclusive);
    hash_optional_i64(hasher, output.occurred_at_unix_ms);
    hash_text(hasher, &output.associations.direct_session_id)?;
    hash_text(hasher, &output.associations.root_session_id)?;
    hash_optional_text(hasher, output.associations.parent_session_id.as_deref())?;
    hash_optional_text(hasher, output.associations.provider_session_id.as_deref())?;
    hash_optional_text(hasher, output.associations.agent_id.as_deref())?;
    match output.associations.repository.as_ref() {
        Some(repository) => {
            hasher.update([1]);
            hash_text(hasher, &repository.repository_id)?;
            hash_optional_text(hasher, repository.checkout_id.as_deref())?;
            hash_optional_text(hasher, repository.worktree_id.as_deref())?;
            hash_optional_text(hasher, repository.object_format.as_deref())?;
        }
        None => hasher.update([0]),
    }
    hash_optional_text(hasher, output.call_id.as_deref())?;
    match output.command.as_ref() {
        Some(command) => {
            hasher.update([1]);
            hash_text(hasher, &command.tool_name)?;
            hash_text(hasher, &command.command)?;
            hash_optional_text(hasher, command.working_directory.as_deref())?;
        }
        None => hasher.update([0]),
    }
    hasher.update([match output.outcome.outcome {
        OutputOutcome::Success => 1,
        OutputOutcome::Failure => 2,
        OutputOutcome::Timeout => 3,
        OutputOutcome::Unknown => 4,
    }]);
    hash_optional_i32(hasher, output.outcome.exit_code);
    hash_optional_u64(hasher, output.outcome.duration_ms);
    hasher.update(output.locator.version.to_le_bytes());
    hash_text(hasher, &output.locator.kind)?;
    hash_bytes(hasher, &output.locator.payload)?;
    hash_bytes(hasher, &output.content)
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

pub(super) fn hash_optional_u64(hasher: &mut Sha256, value: Option<u64>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        None => hasher.update([0]),
    }
}

pub(super) fn hash_optional_u32(hasher: &mut Sha256, value: Option<u32>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        None => hasher.update([0]),
    }
}

pub(super) fn hash_optional_i64(hasher: &mut Sha256, value: Option<i64>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        None => hasher.update([0]),
    }
}

pub(super) fn hash_optional_i32(hasher: &mut Sha256, value: Option<i32>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        None => hasher.update([0]),
    }
}

pub(super) fn estimated_output_wire_bytes(output: &ProOutputObservation) -> Option<usize> {
    let mut total = PRO_OUTPUT_FIXED_WIRE_BYTES;
    for value in [
        Some(output.coordinate.unit_key.as_str()),
        output.coordinate.native_record_id.as_deref(),
        Some(output.associations.direct_session_id.as_str()),
        Some(output.associations.root_session_id.as_str()),
        output.associations.parent_session_id.as_deref(),
        output.associations.provider_session_id.as_deref(),
        output.associations.agent_id.as_deref(),
        output.call_id.as_deref(),
        output
            .command
            .as_ref()
            .map(|command| command.tool_name.as_str()),
        output
            .command
            .as_ref()
            .map(|command| command.command.as_str()),
        output
            .command
            .as_ref()
            .and_then(|command| command.working_directory.as_deref()),
        Some(output.locator.kind.as_str()),
    ]
    .into_iter()
    .flatten()
    {
        total = total.checked_add(worst_case_json_string_bytes(value.len())?)?;
    }
    total = total.checked_add(base64_json_bytes(output.locator.payload.len())?)?;
    total.checked_add(base64_json_bytes(output.content.len())?)
}

pub(super) fn worst_case_json_string_bytes(bytes: usize) -> Option<usize> {
    bytes.checked_mul(6)?.checked_add(2)
}

pub(super) fn base64_json_bytes(bytes: usize) -> Option<usize> {
    bytes
        .checked_add(2)?
        .checked_div(3)?
        .checked_mul(4)?
        .checked_add(2)
}
