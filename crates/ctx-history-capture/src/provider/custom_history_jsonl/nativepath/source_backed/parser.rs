use super::*;

pub(super) fn parse_projection(bytes: &[u8]) -> CustomHistorySourceBackedResult<ParsedProjection> {
    let (prefix_len, terminal) = complete_prefix(bytes);
    let prefix = &bytes[..prefix_len];
    let content_digest = prefix_digest(prefix);
    let mut parsed = parse_custom_history(std::io::Cursor::new(prefix))?;
    let ordered = ordered_sessions(&parsed.sessions, &mut parsed.summary);
    let valid_sessions = ordered.into_iter().collect::<BTreeSet<_>>();
    let lines = complete_lines(prefix)?;
    let event_lines = parsed
        .events
        .iter()
        .filter(|(_, event)| {
            valid_sessions.contains(&(event.source_id.clone(), event.session_id.clone()))
        })
        .map(|(line, event)| {
            (
                (
                    event.source_id.clone(),
                    event.session_id.clone(),
                    event.event_index,
                ),
                *line,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut rejected_lines = parsed
        .summary
        .failures
        .iter()
        .filter_map(|failure| (failure.line != 0).then_some(failure.line))
        .collect::<BTreeSet<_>>();
    rejected_lines.extend(
        lines
            .iter()
            .filter_map(|line| line.oversized.then_some(line.line_number)),
    );
    for (line, event) in &parsed.events {
        if !valid_sessions.contains(&(event.source_id.clone(), event.session_id.clone())) {
            rejected_lines.insert(*line);
        }
    }

    let complete_records =
        u64::try_from(lines.len()).map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
    let retained_records = u64::try_from(event_lines.len())
        .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
    let retained_lines = event_lines.values().copied().collect::<BTreeSet<_>>();
    let rejected_records = u64::try_from(
        rejected_lines
            .iter()
            .filter(|line| **line <= lines.len() && !retained_lines.contains(*line))
            .count(),
    )
    .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
    let ignored_records = complete_records
        .checked_sub(retained_records)
        .and_then(|value| value.checked_sub(rejected_records))
        .ok_or(CustomHistorySourceBackedError::CountMismatch)?;
    let certified_prefix_bytes =
        u64::try_from(prefix_len).map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
    let counts = ScannedSourceCounts {
        complete_records,
        retained_records,
        rejected_records,
        ignored_records,
        indexed_documents: retained_records,
        certified_bytes: certified_prefix_bytes,
    };
    Ok(ParsedProjection {
        parsed,
        lines,
        valid_sessions,
        event_lines,
        counts,
        checkpoint: CustomHistoryCheckpoint {
            version: CUSTOM_CHECKPOINT_VERSION,
            certified_prefix_bytes,
            complete_records,
            terminal,
        },
        content_digest,
    })
}

fn ordered_sessions(
    sessions: &BTreeMap<(String, String), (usize, CtxHistoryJsonlSessionRecord)>,
    summary: &mut ProviderImportSummary,
) -> Vec<(String, String)> {
    let mut remaining = sessions.keys().cloned().collect::<BTreeSet<_>>();
    let mut emitted = BTreeSet::new();
    let mut ordered = Vec::new();
    loop {
        let ready = remaining
            .iter()
            .filter(|key| {
                let session = &sessions[*key].1;
                [
                    session.parent_session_id.as_ref(),
                    session.root_session_id.as_ref(),
                ]
                .into_iter()
                .flatten()
                .all(|dependency| {
                    dependency == &session.session_id
                        || emitted.contains(&(session.source_id.clone(), dependency.clone()))
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        if ready.is_empty() {
            break;
        }
        for key in ready {
            remaining.remove(&key);
            emitted.insert(key.clone());
            ordered.push(key);
        }
    }
    for key in remaining {
        let line = sessions[&key].0;
        push_provider_import_failure(
            summary,
            line,
            format!(
                "session `{}` in source `{}` has a cyclic parent/root relationship",
                key.1, key.0
            ),
        );
    }
    ordered
}

fn complete_prefix(bytes: &[u8]) -> (usize, bool) {
    if bytes.is_empty() || bytes.last() == Some(&b'\n') {
        return (bytes.len(), true);
    }
    (
        bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index.saturating_add(1)),
        false,
    )
}

fn complete_lines(prefix: &[u8]) -> CustomHistorySourceBackedResult<Vec<CompleteLine>> {
    let mut lines = Vec::new();
    let mut start = 0_usize;
    for (end, _) in prefix
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'\n')
    {
        let end = end.saturating_add(1);
        let bytes = &prefix[start..end];
        let line_number = lines.len().saturating_add(1);
        lines.push(CompleteLine {
            line_number,
            byte_offset: u64::try_from(start)
                .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?,
            byte_length: u64::try_from(bytes.len())
                .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?,
            physical_ordinal: u64::try_from(line_number.saturating_sub(1))
                .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?,
            record_digest: Sha256::digest(bytes).into(),
            oversized: bytes.len() > MAX_PROVIDER_JSONL_LINE_BYTES,
        });
        start = end;
    }
    if start != prefix.len() {
        return Err(CustomHistorySourceBackedError::CountMismatch);
    }
    Ok(lines)
}

pub(super) fn prefix_digest(prefix: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(SOURCE_DIGEST_DOMAIN);
    digest.update((prefix.len() as u64).to_be_bytes());
    digest.update(prefix);
    digest.finalize().into()
}
