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
