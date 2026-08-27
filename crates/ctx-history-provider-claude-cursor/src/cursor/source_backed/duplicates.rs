use std::{
    collections::{BTreeMap, BTreeSet},
    io::BufReader,
    path::Path,
};

use ctx_history_provider_runtime::{
    observe_opened_file, read_bounded_record_unhashed, source_io::OpenedProviderSourceFile,
    CaptureError, JsonlFileObservation, JsonlRecordFraming, Result,
};
use ctx_history_source_io::MAX_PROVIDER_JSONL_LINE_BYTES;
use sha2::Digest;

use super::super::{
    layout::CursorTranscriptPath, parser::project_cursor_jsonl_record,
    projection::CursorNativeEvent,
};
#[cfg(any(test, feature = "test-support"))]
use super::CURSOR_SIGNATURE_RECORDS;

#[derive(Debug, Clone, PartialEq, Eq)]
struct CursorTranscriptSummary {
    observation: JsonlFileObservation,
    signature: [u8; 32],
    event_count: u64,
    latest_occurred_at_unix_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CursorTranscriptSelection {
    pub(super) selected_index: usize,
    pub(super) selected_observation: JsonlFileObservation,
    pub(super) route_observations: Vec<JsonlFileObservation>,
    pub(super) selected_signature: [u8; 32],
    pub(super) divergent_indices: BTreeSet<usize>,
}

pub(super) fn select_cursor_transcript(
    routes: &[CursorTranscriptPath],
) -> Result<CursorTranscriptSelection> {
    if routes.len() < 2 {
        return Err(CaptureError::SystemInvariant(
            "Cursor duplicate selection requires at least two routes",
        ));
    }
    let summaries = routes
        .iter()
        .map(cursor_transcript_summary)
        .collect::<Result<Vec<_>>>()?;
    let selected_index = (0..routes.len())
        .max_by(|left, right| {
            summaries[*left]
                .event_count
                .cmp(&summaries[*right].event_count)
                .then_with(|| {
                    summaries[*left]
                        .latest_occurred_at_unix_ms
                        .cmp(&summaries[*right].latest_occurred_at_unix_ms)
                })
                // `max_by` retains the later equal item, so reverse the path
                // comparison to make the lowest path the stable final winner.
                .then_with(|| routes[*right].path().cmp(routes[*left].path()))
        })
        .ok_or(CaptureError::SystemInvariant(
            "Cursor duplicate selection has no candidate",
        ))?;
    let selected = &summaries[selected_index];
    let prefix_lengths = summaries
        .iter()
        .enumerate()
        .filter(|(index, summary)| {
            *index != selected_index && summary.event_count < selected.event_count
        })
        .map(|(_, summary)| summary.event_count)
        .collect::<BTreeSet<_>>();
    let selected_prefixes =
        cursor_transcript_prefix_signatures(&routes[selected_index], &prefix_lengths)?;
    let divergent_indices = summaries
        .iter()
        .enumerate()
        .filter_map(|(index, summary)| {
            let comparable = index == selected_index
                || (summary.event_count == selected.event_count
                    && summary.signature == selected.signature)
                || (summary.event_count < selected.event_count
                    && selected_prefixes.get(&summary.event_count) == Some(&summary.signature));
            (!comparable).then_some(index)
        })
        .collect();
    Ok(CursorTranscriptSelection {
        selected_index,
        selected_observation: selected.observation.clone(),
        route_observations: summaries
            .iter()
            .map(|summary| summary.observation.clone())
            .collect(),
        selected_signature: selected.signature,
        divergent_indices,
    })
}

fn cursor_transcript_summary(transcript: &CursorTranscriptPath) -> Result<CursorTranscriptSummary> {
    let source = transcript
        .authority()
        .open_file(transcript.authority_relative_path())?;
    let observation = observe_opened_file(transcript.path(), &source)?;
    let mut digest = sha2::Sha256::new();
    digest.update(b"ctx.cursor.logical-transcript.v1\0");
    let mut event_count = 0_u64;
    let mut latest_occurred_at_unix_ms = None;
    visit_cursor_events(&source, |event| {
        #[cfg(any(test, feature = "test-support"))]
        CURSOR_SIGNATURE_RECORDS.set(CURSOR_SIGNATURE_RECORDS.get().saturating_add(1));
        digest.update(event_count.to_be_bytes());
        digest.update(event.provider_event_hash);
        if let Some(occurred_at) = event.occurred_at {
            latest_occurred_at_unix_ms = Some(
                latest_occurred_at_unix_ms.map_or(occurred_at.timestamp_millis(), |latest: i64| {
                    latest.max(occurred_at.timestamp_millis())
                }),
            );
        }
        event_count = event_count
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Cursor logical transcript event count overflowed",
            ))?;
        Ok(())
    })?;
    if observe_opened_file(transcript.path(), &source)? != observation {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(CursorTranscriptSummary {
        observation,
        signature: finish_cursor_transcript_signature(digest, event_count),
        event_count,
        latest_occurred_at_unix_ms,
    })
}

fn cursor_transcript_prefix_signatures(
    transcript: &CursorTranscriptPath,
    prefix_lengths: &BTreeSet<u64>,
) -> Result<BTreeMap<u64, [u8; 32]>> {
    if prefix_lengths.is_empty() {
        return Ok(BTreeMap::new());
    }
    let source = transcript
        .authority()
        .open_file(transcript.authority_relative_path())?;
    let mut digest = sha2::Sha256::new();
    digest.update(b"ctx.cursor.logical-transcript.v1\0");
    let mut event_count = 0_u64;
    let mut signatures = BTreeMap::new();
    if prefix_lengths.contains(&0) {
        signatures.insert(0, finish_cursor_transcript_signature(digest.clone(), 0));
    }
    visit_cursor_events(&source, |event| {
        digest.update(event_count.to_be_bytes());
        digest.update(event.provider_event_hash);
        event_count = event_count
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Cursor logical transcript event count overflowed",
            ))?;
        if prefix_lengths.contains(&event_count) {
            signatures.insert(
                event_count,
                finish_cursor_transcript_signature(digest.clone(), event_count),
            );
        }
        Ok(())
    })?;
    Ok(signatures)
}

fn finish_cursor_transcript_signature(mut digest: sha2::Sha256, event_count: u64) -> [u8; 32] {
    digest.update(event_count.to_be_bytes());
    digest.finalize().into()
}

pub(super) fn cursor_route_sha256(path: &Path) -> [u8; 32] {
    let mut digest = sha2::Sha256::new();
    digest.update(b"ctx.cursor.transcript-route.v1\0");
    digest.update(path.as_os_str().as_encoded_bytes());
    digest.finalize().into()
}

fn visit_cursor_events(
    source: &OpenedProviderSourceFile,
    mut visit: impl FnMut(CursorNativeEvent) -> Result<()>,
) -> Result<()> {
    let mut reader = BufReader::new(source.file().try_clone()?);
    let mut line = Vec::new();
    let mut physical_ordinal = 0_u64;
    let mut offset = 0_u64;
    let frozen_len = source.len();
    while offset < frozen_len {
        let record = read_bounded_record_unhashed(
            &mut reader,
            &mut line,
            frozen_len.saturating_sub(offset),
            JsonlRecordFraming::ordinary(),
            || CaptureError::SourceChangedDuringCapture,
        )?
        .ok_or(CaptureError::SourceChangedDuringCapture)?;
        offset = offset
            .checked_add(record.byte_len)
            .ok_or(CaptureError::SystemInvariant(
                "Cursor signature offset overflowed",
            ))?;
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if record.complete
            && !record.oversized
            && line.len() <= MAX_PROVIDER_JSONL_LINE_BYTES
            && !line.is_empty()
        {
            if let Some(events) = project_cursor_jsonl_record(
                &line,
                physical_ordinal,
                physical_ordinal,
                0,
                u64::try_from(line.len()).map_err(|_| {
                    CaptureError::InvalidPayload("Cursor line length exceeds u64".to_owned())
                })?,
            )? {
                for event in events {
                    visit(event)?;
                }
            }
        }
        physical_ordinal = physical_ordinal
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Cursor physical ordinal overflowed",
            ))?;
        if !record.complete {
            break;
        }
    }
    source.revalidate_leaf()?;
    Ok(())
}
