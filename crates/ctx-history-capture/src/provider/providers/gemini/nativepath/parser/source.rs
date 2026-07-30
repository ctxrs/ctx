use super::*;

pub(super) struct RecordRead {
    pub(super) bytes_observed: u64,
    pub(super) terminated: bool,
    pub(super) oversized: bool,
}

pub(super) fn read_record(
    reader: &mut impl BufRead,
    buffer: &mut Vec<u8>,
    prefix_hasher: &mut Sha256,
    source_hasher: &mut Sha256,
) -> Result<Option<RecordRead>> {
    buffer.clear();
    let mut bytes_observed = 0_u64;
    let mut terminated = false;
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        let consumed =
            available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |index| {
                    terminated = true;
                    index.saturating_add(1)
                });
        let chunk = &available[..consumed];
        prefix_hasher.update(chunk);
        source_hasher.update(chunk);
        bytes_observed =
            bytes_observed.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        if !oversized {
            let remaining = MAX_PROVIDER_JSONL_LINE_BYTES
                .saturating_add(2)
                .saturating_sub(buffer.len());
            if chunk.len() <= remaining {
                buffer.extend_from_slice(chunk);
            } else {
                buffer.extend_from_slice(&chunk[..remaining]);
                oversized = true;
            }
        }
        reader.consume(consumed);
        if terminated {
            break;
        }
    }
    if bytes_observed == 0 {
        Ok(None)
    } else {
        #[cfg(test)]
        TEST_RECORD_READS.set(TEST_RECORD_READS.get().saturating_add(1));
        Ok(Some(RecordRead {
            bytes_observed,
            terminated,
            oversized,
        }))
    }
}

pub(super) fn trim_jsonl_ending(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

pub(super) fn new_prefix_hasher() -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(PREFIX_HASH_DOMAIN);
    hasher
}

#[cfg(test)]
pub(super) fn prefix_digest(hasher: &Sha256) -> [u8; 32] {
    hasher.clone().finalize().into()
}

#[cfg(test)]
pub(super) fn hash_gemini_prefix(
    file: &mut File,
    complete_prefix_end: u64,
) -> GeminiScanResult<Sha256> {
    // Resume validation is deliberately O(prefix bytes) but constant-memory:
    // it never parses JSON or reconstructs source-wide identity state.
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = new_prefix_hasher();
    let mut remaining = complete_prefix_end;
    let mut buffer = [0_u8; PREFIX_HASH_BUFFER_BYTES];
    while remaining != 0 {
        let requested =
            usize::try_from(remaining.min(PREFIX_HASH_BUFFER_BYTES as u64)).map_err(|_| {
                GeminiScanError::Capture(CaptureError::SystemInvariant(
                    "Gemini prefix hash request exceeds platform limits",
                ))
            })?;
        let read = file.read(&mut buffer[..requested])?;
        if read == 0 {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
        #[cfg(test)]
        TEST_PREFIX_BYTES_HASHED.set(TEST_PREFIX_BYTES_HASHED.get().saturating_add(read as u64));
    }
    Ok(hasher)
}

#[cfg(test)]
pub(super) fn same_physical_file(
    previous: &GeminiFileObservation,
    current: &GeminiFileObservation,
) -> bool {
    match (
        previous.device.zip(previous.inode),
        current.device.zip(current.inode),
    ) {
        (Some(previous), Some(current)) => previous == current,
        (None, None) => true,
        _ => false,
    }
}

#[cfg(test)]
pub(super) fn frontier_file_identity_matches(
    frontier: &GeminiPageFrontier,
    current: &GeminiFileObservation,
) -> bool {
    match (
        frontier.source_device.zip(frontier.source_inode),
        current.device.zip(current.inode),
    ) {
        (Some(frontier), Some(current)) => frontier == current,
        (None, None) => true,
        _ => false,
    }
}

#[cfg(test)]
pub(super) fn lifecycle_signals(
    checkpoint: &GeminiCheckpoint,
    previous: Option<&GeminiPreviousSource>,
    resumed_prefix: bool,
    emitted_rows: u64,
    cross_path_change: Option<GeminiSourceChange>,
) -> GeminiLifecycleSignals {
    let source_change =
        classify_source_change(checkpoint, previous, resumed_prefix, cross_path_change);
    let publication_shape = match source_change {
        GeminiSourceChange::Unchanged => GeminiPublicationShape::ObservationOnly,
        GeminiSourceChange::Append if resumed_prefix => GeminiPublicationShape::AppendDelta,
        GeminiSourceChange::Ambiguous => GeminiPublicationShape::ObservationOnly,
        _ => GeminiPublicationShape::AuthoritativeSnapshot,
    };
    let completeness = if checkpoint.terminal {
        GeminiCompleteness::TerminalSnapshot
    } else {
        GeminiCompleteness::NonterminalCompletePrefix {
            end: checkpoint.complete_prefix_end,
        }
    };
    let content_changed = previous.is_none_or(|previous| {
        previous.checkpoint.complete_prefix_end != checkpoint.complete_prefix_end
            || previous.checkpoint.complete_prefix_sha256 != checkpoint.complete_prefix_sha256
            || previous.checkpoint.rejected_records != checkpoint.rejected_records
            || previous.checkpoint.terminal != checkpoint.terminal
            || previous.checkpoint.source_sha256 != checkpoint.source_sha256
    });
    GeminiLifecycleSignals {
        source_change,
        publication_shape,
        completeness,
        emitted_zero_rows: emitted_rows == 0,
        source_has_zero_retained_rows: checkpoint.retained_event_count == 0,
        cursor_advance_allowed: source_change != GeminiSourceChange::Ambiguous,
        content_changed,
    }
}

#[cfg(test)]
pub(super) fn classify_cross_path_source(
    checkpoint: &GeminiCheckpoint,
    previous: Option<&GeminiPreviousSource>,
) -> Option<GeminiSourceChange> {
    let previous = previous?;
    let old = &previous.checkpoint;
    if old.source_path == checkpoint.source_path {
        return None;
    }

    let compatible_session_relationship =
        old.session.is_some() && old.session == checkpoint.session;
    let exact_generation = old.parser_revision == GEMINI_NATIVEPATH_PARSER_REVISION
        && old.policy_revision == GEMINI_NATIVEPATH_POLICY_REVISION
        && old.source_observation.length == checkpoint.source_observation.length
        && old.source_sha256 == checkpoint.source_sha256;
    if exact_generation && compatible_session_relationship {
        return Some(if previous.prior_route_still_live {
            GeminiSourceChange::LiveCopy
        } else {
            GeminiSourceChange::Relocation
        });
    }

    // A different route that does not exactly match the prior generation is
    // an independent replacement source. Its valid records remain eligible;
    // only exact content plus the same session relationship can authorize a
    // relocation/live-copy alias.
    Some(GeminiSourceChange::Replacement)
}

#[cfg(test)]
pub(super) fn classify_source_change(
    checkpoint: &GeminiCheckpoint,
    previous: Option<&GeminiPreviousSource>,
    resumed_prefix: bool,
    cross_path_change: Option<GeminiSourceChange>,
) -> GeminiSourceChange {
    let Some(previous) = previous else {
        return GeminiSourceChange::Fresh;
    };
    let old = &previous.checkpoint;
    let same_path = old.source_path == checkpoint.source_path;
    let old_session_id = old
        .session
        .as_ref()
        .map(|session| session.native_session_id.as_str());
    let new_session_id = checkpoint
        .session
        .as_ref()
        .map(|session| session.native_session_id.as_str());

    if !same_path {
        return cross_path_change.unwrap_or(GeminiSourceChange::Ambiguous);
    }
    if old_session_id.is_some() && new_session_id.is_some() && old_session_id != new_session_id {
        return GeminiSourceChange::Replacement;
    }
    if resumed_prefix {
        if checkpoint.source_observation.length > old.source_observation.length {
            return GeminiSourceChange::Append;
        }
        if checkpoint.complete_prefix_end > old.complete_prefix_end {
            return GeminiSourceChange::Append;
        }
        if checkpoint.source_observation.length < old.source_observation.length {
            return GeminiSourceChange::Truncation;
        }
        if checkpoint.complete_prefix_sha256 == old.complete_prefix_sha256
            && checkpoint.complete_prefix_end == old.complete_prefix_end
            && checkpoint.rejected_records == old.rejected_records
            && checkpoint.terminal == old.terminal
            && checkpoint.source_observation.length == old.source_observation.length
            && checkpoint.source_sha256 == old.source_sha256
        {
            return GeminiSourceChange::Unchanged;
        }
        return GeminiSourceChange::Rewrite;
    }
    if checkpoint.complete_prefix_end < old.complete_prefix_end {
        GeminiSourceChange::Truncation
    } else {
        GeminiSourceChange::Rewrite
    }
}
