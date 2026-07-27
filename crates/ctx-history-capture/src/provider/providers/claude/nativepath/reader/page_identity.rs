use super::*;

#[derive(Serialize)]
struct CorePageEncoding<'a> {
    session: &'a ClaudeSessionMetadata,
    expected_frontier: &'a ClaudeNativeFrontier,
    next_safe_frontier: &'a ClaudeNativeFrontier,
    rows: &'a [ClaudeRetainedRow],
    rejections: &'a [RecordRejection],
    rejected_records: u64,
    logical_units: usize,
    terminal: bool,
    certificate: &'a ClaudePageCertificate,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn core_encoded_bytes(
    session: &ClaudeSessionMetadata,
    expected_frontier: &ClaudeNativeFrontier,
    next_safe_frontier: &ClaudeNativeFrontier,
    rows: &[ClaudeRetainedRow],
    rejections: &[RecordRejection],
    rejected_records: u64,
    logical_units: usize,
    terminal: bool,
    certificate: &ClaudePageCertificate,
) -> Result<usize, ClaudeNativePathError> {
    exact_json_encoded_bytes(&CorePageEncoding {
        session,
        expected_frontier,
        next_safe_frontier,
        rows,
        rejections,
        rejected_records,
        logical_units,
        terminal,
        certificate,
    })?
    .checked_add(CLAUDE_PAGE_ENCODING_ALLOWANCE)
    .ok_or(ClaudeNativePathError::PositionOverflow)
}

pub(super) fn core_candidate_bytes(
    page: &CorePageBuilder,
    session: &ClaudeSessionMetadata,
    added_row_bytes: usize,
    added_rejection: Option<&RecordRejection>,
    next: &ClaudeNativeFrontier,
    terminal: bool,
    source: &DiscoveredClaudeSession,
) -> Option<usize> {
    let certificate = page_certificate(source, next);
    let added_rejection_bytes = added_rejection
        .map(exact_json_encoded_bytes)
        .transpose()
        .ok()?
        .unwrap_or_default();
    CLAUDE_PAGE_ENCODING_ALLOWANCE
        .checked_add(exact_json_encoded_bytes(session).ok()?)?
        .checked_add(exact_json_encoded_bytes(&page.expected_frontier).ok()?)?
        .checked_add(exact_json_encoded_bytes(next).ok()?)?
        .checked_add(exact_json_encoded_bytes(&certificate).ok()?)?
        .checked_add(page.encoded_row_bytes)?
        .checked_add(added_row_bytes)?
        .checked_add(page.encoded_rejection_bytes)?
        .checked_add(added_rejection_bytes)?
        .checked_add(usize::from(terminal))
}

pub(crate) fn exact_json_encoded_bytes<T: Serialize>(
    value: &T,
) -> Result<usize, ClaudeNativePathError> {
    let mut counter = CountingWriter::default();
    serde_json::to_writer(&mut counter, value).map_err(|error| {
        ClaudeNativePathError::InvalidCheckpoint {
            reason: format!("Claude page encoding failed: {error}"),
        }
    })?;
    Ok(counter.bytes)
}

#[derive(Default)]
struct CountingWriter {
    bytes: usize,
}

impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("Claude encoded byte count overflow"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct DigestWriter<'a>(&'a mut Sha256);

impl Write for DigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn core_page_identity(
    session: &ClaudeSessionMetadata,
    page: &CorePageBuilder,
    terminal: bool,
    certificate: &ClaudePageCertificate,
) -> Result<ClaudeNativePageIdentity, ClaudeNativePathError> {
    let mut hasher = Sha256::new();
    hasher.update(CLAUDE_CORE_PAGE_IDENTITY_DOMAIN);
    serde_json::to_writer(
        DigestWriter(&mut hasher),
        &CorePageEncoding {
            session,
            expected_frontier: &page.expected_frontier,
            next_safe_frontier: &page.next_safe_frontier,
            rows: &page.rows,
            rejections: &page.rejections,
            rejected_records: page.rejected_records,
            logical_units: page.logical_units,
            terminal,
            certificate,
        },
    )
    .map_err(|error| ClaudeNativePathError::InvalidCheckpoint {
        reason: format!("Claude Core page identity encoding failed: {error}"),
    })?;
    Ok(ClaudeNativePageIdentity(hasher.finalize().into()))
}

pub(super) fn pro_candidate_bytes(
    page: &ProPageBuilder,
    parsed: &ParsedClaudeRecord,
    source: &DiscoveredClaudeSession,
) -> Option<usize> {
    let mut bytes = pro_builder_fixed_bytes(page, source)?;
    for output in &parsed.outputs {
        bytes = bytes.checked_add(parsed_output_wire_bytes(output, source)?)?;
    }
    Some(bytes)
}

pub(super) fn pro_page_encoded_bytes(
    page: &ProPageBuilder,
    source: &DiscoveredClaudeSession,
    certificate: &ClaudePageCertificate,
) -> Result<usize, ClaudeNativePathError> {
    let mut bytes =
        pro_builder_fixed_bytes(page, source).ok_or(ClaudeNativePathError::PositionOverflow)?;
    bytes = bytes
        .checked_add(
            exact_json_encoded_bytes(certificate)?
                .checked_add(CLAUDE_PAGE_ENCODING_ALLOWANCE)
                .ok_or(ClaudeNativePathError::PositionOverflow)?,
        )
        .ok_or(ClaudeNativePathError::PositionOverflow)?;
    Ok(bytes)
}

pub(super) fn pro_builder_fixed_bytes(
    page: &ProPageBuilder,
    source: &DiscoveredClaudeSession,
) -> Option<usize> {
    CLAUDE_PAGE_ENCODING_ALLOWANCE
        .checked_add(exact_json_encoded_bytes(&page.expected_frontier).ok()?)?
        .checked_add(exact_json_encoded_bytes(&page.next_safe_frontier).ok()?)?
        .checked_add(source.key.provider_session_id().len())?
        .checked_add(source.key.root_session_id.len())?
        .checked_add(page.encoded_output_bytes)?
        .checked_add(page.encoded_rejection_bytes)
}

pub(super) fn parsed_output_wire_bytes(
    output: &super::super::record::ParsedClaudeOutput,
    source: &DiscoveredClaudeSession,
) -> Option<usize> {
    let content = output.content.as_ref()?.len();
    CLAUDE_PRO_OUTPUT_ENCODING_ALLOWANCE
        .checked_add(content)?
        .checked_add(output.call_id.as_ref().map_or(0, String::len))?
        .checked_add(source.key.provider_session_id().len())?
        .checked_add(source.key.root_session_id.len())
}

pub(super) fn output_wire_bytes(output: &ProOutputObservation) -> usize {
    CLAUDE_PRO_OUTPUT_ENCODING_ALLOWANCE
        .saturating_add(output.content.len())
        .saturating_add(output.coordinate.unit_key.len())
        .saturating_add(
            output
                .coordinate
                .native_record_id
                .as_ref()
                .map_or(0, String::len),
        )
        .saturating_add(output.associations.direct_session_id.len())
        .saturating_add(output.associations.root_session_id.len())
        .saturating_add(
            output
                .associations
                .parent_session_id
                .as_ref()
                .map_or(0, String::len),
        )
        .saturating_add(output.call_id.as_ref().map_or(0, String::len))
        .saturating_add(output.locator.kind.len())
        .saturating_add(output.locator.payload.len())
}

pub(super) fn pro_page_identity(
    page: &ProPageBuilder,
    terminal: bool,
    certificate: &ClaudePageCertificate,
) -> Result<ClaudeNativeProOutputPageIdentity, ClaudeNativePathError> {
    pro_page_identity_claims(
        &page.expected_frontier,
        &page.next_safe_frontier,
        &page.outputs,
        &page.rejections,
        page.rejected_outputs,
        page.logical_units,
        terminal,
        certificate,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn pro_page_identity_claims(
    expected_frontier: &ClaudeNativeFrontier,
    next_safe_frontier: &ClaudeNativeFrontier,
    outputs: &[ProOutputObservation],
    rejections: &[RecordRejection],
    rejected_outputs: u64,
    logical_units: usize,
    terminal: bool,
    certificate: &ClaudePageCertificate,
) -> Result<ClaudeNativeProOutputPageIdentity, ClaudeNativePathError> {
    let mut hasher = Sha256::new();
    hasher.update(CLAUDE_PRO_PAGE_IDENTITY_DOMAIN);
    hash_canonical_json(&mut hasher, b"expected-frontier\0", expected_frontier)?;
    hash_canonical_json(&mut hasher, b"next-safe-frontier\0", next_safe_frontier)?;
    hash_usize(&mut hasher, logical_units)?;
    hash_usize(&mut hasher, outputs.len())?;
    hasher.update(rejected_outputs.to_be_bytes());
    hash_canonical_json(&mut hasher, b"rejections\0", &rejections)?;
    hash_canonical_json(&mut hasher, b"certificate\0", certificate)?;
    hasher.update([u8::from(terminal)]);
    for output in outputs {
        hash_pro_output_claim(&mut hasher, output)?;
    }
    Ok(ClaudeNativeProOutputPageIdentity(hasher.finalize().into()))
}

#[cfg(test)]
pub(crate) fn pro_page_identity_for_test(
    page: &ClaudeNativeProOutputPage,
) -> Result<ClaudeNativeProOutputPageIdentity, ClaudeNativePathError> {
    pro_page_identity_claims(
        &page.expected_frontier,
        &page.next_safe_frontier,
        &page.outputs,
        &page.rejections,
        page.rejected_outputs,
        page.logical_units,
        page.terminal,
        &page.certificate,
    )
}

pub(super) fn hash_canonical_json<T: Serialize>(
    hasher: &mut Sha256,
    domain: &[u8],
    value: &T,
) -> Result<(), ClaudeNativePathError> {
    hasher.update(domain);
    hasher.update(
        u64::try_from(exact_json_encoded_bytes(value)?)
            .map_err(|_| ClaudeNativePathError::PositionOverflow)?
            .to_be_bytes(),
    );
    serde_json::to_writer(DigestWriter(hasher), value).map_err(|error| {
        ClaudeNativePathError::InvalidCheckpoint {
            reason: format!("Claude Pro identity encoding failed: {error}"),
        }
    })
}

pub(super) fn hash_pro_output_claim(
    hasher: &mut Sha256,
    output: &ProOutputObservation,
) -> Result<(), ClaudeNativePathError> {
    hasher.update(b"output\0");
    hasher.update([match output.kind {
        OutputObservationKind::Command => 1,
        OutputObservationKind::Tool => 2,
    }]);
    hash_text(hasher, &output.coordinate.unit_key)?;
    hasher.update(output.coordinate.native_sequence.to_be_bytes());
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
    hasher.update([u8::from(output.associations.repository.is_some())]);
    if let Some(repository) = &output.associations.repository {
        hash_text(hasher, &repository.repository_id)?;
        hash_optional_text(hasher, repository.checkout_id.as_deref())?;
        hash_optional_text(hasher, repository.worktree_id.as_deref())?;
        hash_optional_text(hasher, repository.object_format.as_deref())?;
    }

    hash_optional_text(hasher, output.call_id.as_deref())?;
    hasher.update([u8::from(output.command.is_some())]);
    if let Some(command) = &output.command {
        hash_text(hasher, &command.tool_name)?;
        hash_text(hasher, &command.command)?;
        hash_optional_text(hasher, command.working_directory.as_deref())?;
    }
    hasher.update([match output.outcome.outcome {
        OutputOutcome::Success => 1,
        OutputOutcome::Failure => 2,
        OutputOutcome::Timeout => 3,
        OutputOutcome::Unknown => 4,
    }]);
    hash_optional_i32(hasher, output.outcome.exit_code);
    hash_optional_u64(hasher, output.outcome.duration_ms);
    hasher.update(output.locator.version.to_be_bytes());
    hash_text(hasher, &output.locator.kind)?;
    hash_bytes(hasher, &output.locator.payload)?;
    hasher.update(
        u64::try_from(output.content.len())
            .map_err(|_| ClaudeNativePathError::PositionOverflow)?
            .to_be_bytes(),
    );
    hasher.update(Sha256::digest(&output.content));
    Ok(())
}

pub(super) fn hash_optional_text(
    hasher: &mut Sha256,
    value: Option<&str>,
) -> Result<(), ClaudeNativePathError> {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hash_text(hasher, value)?;
    }
    Ok(())
}

pub(super) fn hash_text(hasher: &mut Sha256, value: &str) -> Result<(), ClaudeNativePathError> {
    hash_bytes(hasher, value.as_bytes())
}

pub(super) fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) -> Result<(), ClaudeNativePathError> {
    hasher.update(
        u64::try_from(bytes.len())
            .map_err(|_| ClaudeNativePathError::PositionOverflow)?
            .to_be_bytes(),
    );
    hasher.update(bytes);
    Ok(())
}

pub(super) fn hash_usize(hasher: &mut Sha256, value: usize) -> Result<(), ClaudeNativePathError> {
    hasher.update(
        u64::try_from(value)
            .map_err(|_| ClaudeNativePathError::PositionOverflow)?
            .to_be_bytes(),
    );
    Ok(())
}

pub(super) fn hash_optional_u64(hasher: &mut Sha256, value: Option<u64>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_be_bytes());
    }
}

pub(super) fn hash_optional_u32(hasher: &mut Sha256, value: Option<u32>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_be_bytes());
    }
}

pub(super) fn hash_optional_i64(hasher: &mut Sha256, value: Option<i64>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_be_bytes());
    }
}

pub(super) fn hash_optional_i32(hasher: &mut Sha256, value: Option<i32>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_be_bytes());
    }
}
