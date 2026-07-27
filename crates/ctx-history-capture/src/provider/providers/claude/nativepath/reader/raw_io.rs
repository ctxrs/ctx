use super::*;

pub(super) fn page_certificate(
    source: &DiscoveredClaudeSession,
    frontier: &ClaudeNativeFrontier,
) -> ClaudePageCertificate {
    ClaudePageCertificate {
        canonical_route: source.canonical_path.clone(),
        observation_sha256: source.fingerprint.observation_sha256(),
        physical_file_id: source.fingerprint.physical_file_id,
        certified_prefix_end: frontier.complete_offset,
        certified_prefix_chain_sha256: frontier.complete_record_chain_sha256,
    }
}

#[derive(Default)]
pub(super) struct BoundaryWindow {
    pub(super) bytes: Vec<u8>,
}

impl BoundaryWindow {
    pub(super) fn push_line(&mut self, line_tail: &[u8], observed_bytes: u64) {
        if observed_bytes >= CLAUDE_BOUNDARY_PROOF_BYTES as u64 {
            self.bytes.clear();
            self.bytes.extend_from_slice(line_tail);
        } else {
            push_bounded_tail(&mut self.bytes, line_tail);
        }
    }
}

pub(super) fn verify_committed_prefix(
    file: &mut File,
    frontier: &ClaudeNativeFrontier,
    path: &std::path::Path,
    stats: &mut ParseStats,
) -> Result<Option<BoundaryWindow>, ClaudeNativePathError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| ClaudeNativePathError::Io {
            path: path.to_path_buf(),
            source: error,
        })?;
    let mut reader = BufReader::new(file);
    let mut observed = 0_u64;
    let mut ordinal = 0_u64;
    let mut chain = initial_record_chain();
    let mut boundary_window = BoundaryWindow::default();
    while observed < frontier.complete_offset {
        let Some(raw_line) = read_raw_line(&mut reader, path)? else {
            return Ok(None);
        };
        stats.prefix_verification_bytes = stats
            .prefix_verification_bytes
            .checked_add(raw_line.observed_bytes)
            .ok_or(ClaudeNativePathError::PositionOverflow)?;
        stats.source_bytes_read = stats
            .source_bytes_read
            .checked_add(raw_line.observed_bytes)
            .ok_or(ClaudeNativePathError::PositionOverflow)?;
        stats.prefix_verification_records = stats
            .prefix_verification_records
            .checked_add(1)
            .ok_or(ClaudeNativePathError::PositionOverflow)?;
        observed = observed
            .checked_add(raw_line.observed_bytes)
            .ok_or(ClaudeNativePathError::PositionOverflow)?;
        if observed > frontier.complete_offset || !raw_line.terminated {
            return Ok(None);
        }
        chain = advance_record_chain(&chain, ordinal, &raw_line.raw_sha256);
        boundary_window.push_line(&raw_line.boundary_tail, raw_line.observed_bytes);
        ordinal = ordinal
            .checked_add(1)
            .ok_or(ClaudeNativePathError::PositionOverflow)?;
    }
    let expected_len = usize::try_from(frontier.boundary_proof_len)
        .map_err(|_| ClaudeNativePathError::PositionOverflow)?;
    let matches = observed == frontier.complete_offset
        && ordinal == frontier.next_raw_ordinal
        && chain == frontier.complete_record_chain_sha256
        && expected_len == boundary_window.bytes.len()
        && frontier.boundary_proof_sha256 == boundary_proof_hash(&boundary_window.bytes);
    Ok(matches.then_some(boundary_window))
}

pub(super) fn boundary_proof_hash(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(CLAUDE_BOUNDARY_PROOF_DOMAIN);
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(bytes);
    hasher.finalize().into()
}

pub(super) fn read_raw_line(
    reader: &mut impl BufRead,
    path: &std::path::Path,
) -> Result<Option<RawLine>, ClaudeNativePathError> {
    let mut buffer = Vec::new();
    let mut boundary_tail = Vec::new();
    let mut observed_bytes = 0_u64;
    let mut terminated = false;
    let mut oversized = false;
    let mut raw_hasher = Sha256::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| ClaudeNativePathError::Io {
                path: path.to_path_buf(),
                source: error,
            })?;
        if available.is_empty() {
            break;
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consume = newline.map_or(available.len(), |index| index + 1);
        let consumed = &available[..consume];
        raw_hasher.update(consumed);
        push_bounded_tail(&mut boundary_tail, consumed);
        observed_bytes = observed_bytes
            .checked_add(
                u64::try_from(consume).map_err(|_| ClaudeNativePathError::PositionOverflow)?,
            )
            .ok_or(ClaudeNativePathError::PositionOverflow)?;
        if !oversized {
            let next_len = buffer.len().saturating_add(consume);
            if next_len > MAX_PROVIDER_JSONL_LINE_BYTES {
                buffer.clear();
                oversized = true;
            } else {
                buffer.extend_from_slice(consumed);
            }
        }
        reader.consume(consume);
        if newline.is_some() {
            terminated = true;
            break;
        }
    }
    if observed_bytes == 0 {
        return Ok(None);
    }
    Ok(Some(RawLine {
        buffer,
        boundary_tail,
        observed_bytes,
        terminated,
        oversized,
        raw_sha256: raw_hasher.finalize().into(),
    }))
}

pub(super) fn push_bounded_tail(tail: &mut Vec<u8>, bytes: &[u8]) {
    if bytes.len() >= CLAUDE_BOUNDARY_PROOF_BYTES {
        tail.clear();
        tail.extend_from_slice(&bytes[bytes.len() - CLAUDE_BOUNDARY_PROOF_BYTES..]);
        return;
    }
    let excess = tail
        .len()
        .saturating_add(bytes.len())
        .saturating_sub(CLAUDE_BOUNDARY_PROOF_BYTES);
    if excess > 0 {
        tail.drain(..excess);
    }
    tail.extend_from_slice(bytes);
}

pub(super) fn observe_parse_io(
    stats: &mut ParseStats,
    observed_bytes: u64,
) -> Result<(), ClaudeNativePathError> {
    stats.parsed_source_bytes = stats
        .parsed_source_bytes
        .checked_add(observed_bytes)
        .ok_or(ClaudeNativePathError::PositionOverflow)?;
    stats.source_bytes_read = stats
        .source_bytes_read
        .checked_add(observed_bytes)
        .ok_or(ClaudeNativePathError::PositionOverflow)?;
    Ok(())
}

pub(super) fn json_record_bytes(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    if bytes.get(end.saturating_sub(1)) == Some(&b'\n') {
        end = end.saturating_sub(1);
        if bytes.get(end.saturating_sub(1)) == Some(&b'\r') {
            end = end.saturating_sub(1);
        }
    }
    &bytes[..end]
}

pub(super) fn initial_record_chain() -> [u8; 32] {
    Sha256::digest(CLAUDE_RECORD_CHAIN_DOMAIN).into()
}

pub(super) fn advance_record_chain(
    previous: &[u8; 32],
    raw_ordinal: u64,
    raw_sha256: &[u8; 32],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(CLAUDE_RECORD_CHAIN_DOMAIN);
    hasher.update(previous);
    hasher.update(raw_ordinal.to_be_bytes());
    hasher.update(raw_sha256);
    hasher.finalize().into()
}

pub(super) fn initial_identity_chain() -> [u8; 32] {
    Sha256::digest(CLAUDE_IDENTITY_HASH_DOMAIN).into()
}

pub(super) fn advance_identity_chain(
    previous: &[u8; 32],
    raw_ordinal: u64,
    identity_kind: &[u8],
    native_record_id: Option<&str>,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(CLAUDE_IDENTITY_HASH_DOMAIN);
    hasher.update(previous);
    hasher.update(raw_ordinal.to_be_bytes());
    update_identity_part(&mut hasher, identity_kind);
    update_identity_part(&mut hasher, native_record_id.unwrap_or_default().as_bytes());
    hasher.finalize().into()
}

pub(super) fn update_identity_part(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(value);
}
