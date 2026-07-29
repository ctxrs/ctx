use std::io::{self, BufRead};

use ctx_history_core::EventType;
use sha2::{Digest, Sha256};

use crate::{Result, MAX_PROVIDER_JSONL_LINE_BYTES};

use super::super::{
    checkpoint::{CursorCheckpoint, CursorPrefixBuilder, CursorSessionCheckpoint},
    projection::{
        project_cursor_record, retained_body_bytes, update_cursor_session_checkpoint,
        CursorNativeEvent,
    },
};
use super::{
    classify_cursor_line, decode_sanitized_record, CursorRecordRejection, CursorRejectionKind,
    CursorRejectionSummary,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CursorParserStats {
    pub(crate) bytes_read: u64,
    pub(crate) complete_records: u64,
    pub(crate) blank_records: u64,
    pub(crate) malformed_records: u64,
    pub(crate) oversized_records: u64,
    pub(crate) incomplete_tail_records: u64,
    pub(crate) native_result_records: u64,
    pub(crate) native_result_bytes: u64,
    pub(crate) result_body_bytes_decoded_or_allocated: u64,
    pub(crate) result_hashes_created: u64,
    pub(crate) result_previews_created: u64,
    pub(crate) result_touches_created: u64,
    pub(crate) result_fts_created: u64,
    pub(crate) result_handoffs_created: u64,
    pub(crate) retained_messages: u64,
    pub(crate) retained_summaries: u64,
    pub(crate) retained_notices: u64,
    pub(crate) retained_tool_calls: u64,
    pub(crate) retained_body_bytes: u64,
    pub(crate) max_line_buffer_bytes: usize,
    pub(crate) projected_records: u64,
}

#[derive(Debug)]
pub(crate) struct CursorParsedGeneration {
    pub(crate) rejections: CursorRejectionSummary,
    pub(crate) checkpoint: CursorCheckpoint,
    pub(crate) stats: CursorParserStats,
}

pub(crate) fn scan_cursor_reader(
    reader: &mut impl BufRead,
    emit: &mut dyn FnMut(CursorNativeEvent) -> Result<()>,
) -> Result<CursorParsedGeneration> {
    scan_cursor_reader_with_limit(reader, emit, MAX_PROVIDER_JSONL_LINE_BYTES)
}

pub(super) fn scan_cursor_reader_with_limit(
    reader: &mut impl BufRead,
    emit: &mut dyn FnMut(CursorNativeEvent) -> Result<()>,
    max_line_bytes: usize,
) -> Result<CursorParsedGeneration> {
    let mut prefix = CursorPrefixBuilder::new();
    let mut rejections = CursorRejectionSummary::default();
    let mut stats = CursorParserStats::default();
    let mut session = CursorSessionCheckpoint::default();
    let mut saw_incomplete = false;

    loop {
        let line = read_bounded_line(reader, max_line_bytes)?;
        if line.consumed_bytes == 0 {
            break;
        }
        stats.bytes_read = stats.bytes_read.saturating_add(line.consumed_bytes);
        stats.max_line_buffer_bytes = stats.max_line_buffer_bytes.max(line.bytes.len());
        if !line.terminated {
            stats.incomplete_tail_records = stats.incomplete_tail_records.saturating_add(1);
            saw_incomplete = true;
            break;
        }
        process_complete_line(
            &line,
            &mut prefix,
            emit,
            &mut rejections,
            &mut stats,
            &mut session,
        )?;
    }

    let proof = prefix.finish();
    let checkpoint = CursorCheckpoint::new(proof, session, !saw_incomplete);
    Ok(CursorParsedGeneration {
        rejections,
        checkpoint,
        stats,
    })
}

#[cfg_attr(test, allow(clippy::too_many_arguments))]
fn process_complete_line(
    line: &BoundedLine,
    prefix: &mut CursorPrefixBuilder,
    emit: &mut dyn FnMut(CursorNativeEvent) -> Result<()>,
    rejections: &mut CursorRejectionSummary,
    stats: &mut CursorParserStats,
    session: &mut CursorSessionCheckpoint,
) -> Result<()> {
    stats.complete_records = stats.complete_records.saturating_add(1);
    let physical_line = prefix.physical_lines();
    if line.oversized {
        stats.oversized_records = stats.oversized_records.saturating_add(1);
        let rejection = CursorRecordRejection {
            physical_line,
            kind: CursorRejectionKind::Oversized,
            observed_bytes: line.payload_bytes,
        };
        prefix.record_rejection(rejection.kind, line.consumed_bytes, line.content_sha256);
        rejections.record(rejection);
        return Ok(());
    }
    let payload = strip_line_ending(&line.bytes);
    if payload.iter().all(u8::is_ascii_whitespace) {
        stats.blank_records = stats.blank_records.saturating_add(1);
        prefix.record_blank(line.consumed_bytes, line.content_sha256);
        return Ok(());
    }
    let classification = match classify_cursor_line(payload) {
        Ok(classification) => classification,
        Err(kind) => {
            match kind {
                CursorRejectionKind::MalformedJson => {
                    stats.malformed_records = stats.malformed_records.saturating_add(1);
                }
                CursorRejectionKind::UnsupportedShape => {}
                CursorRejectionKind::Oversized => unreachable!("line admission owns oversize"),
            }
            let rejection = CursorRecordRejection {
                physical_line,
                kind,
                observed_bytes: line.payload_bytes,
            };
            prefix.record_rejection(kind, line.consumed_bytes, line.content_sha256);
            rejections.record(rejection);
            return Ok(());
        }
    };
    let semantic_ordinal = prefix.semantic_records();
    let byte_start = prefix.complete_bytes();
    let byte_end_exclusive = byte_start.saturating_add(line.consumed_bytes);
    let sanitized = match decode_sanitized_record(
        payload,
        semantic_ordinal,
        physical_line,
        byte_start,
        byte_end_exclusive,
        &classification,
    ) {
        Ok(sanitized) => sanitized,
        Err(_) => {
            let rejection = CursorRecordRejection {
                physical_line,
                kind: CursorRejectionKind::UnsupportedShape,
                observed_bytes: line.payload_bytes,
            };
            prefix.record_rejection(rejection.kind, line.consumed_bytes, line.content_sha256);
            rejections.record(rejection);
            return Ok(());
        }
    };
    if classification.result_blocks > 0 {
        stats.native_result_records = stats.native_result_records.saturating_add(1);
        stats.native_result_bytes = stats.native_result_bytes.saturating_add(line.payload_bytes);
    }
    prefix.record_semantic(line.consumed_bytes, line.content_sha256, &sanitized)?;
    let projected = project_cursor_record(&sanitized)?;
    update_cursor_session_checkpoint(session, &projected);
    update_retained_stats(stats, &projected);
    stats.projected_records = stats
        .projected_records
        .saturating_add(projected.len() as u64);
    for event in projected {
        emit(event)?;
    }
    Ok(())
}

fn update_retained_stats(stats: &mut CursorParserStats, events: &[CursorNativeEvent]) {
    for event in events {
        match event.event_type {
            EventType::Message => {
                stats.retained_messages = stats.retained_messages.saturating_add(1);
            }
            EventType::Summary => {
                stats.retained_summaries = stats.retained_summaries.saturating_add(1);
            }
            EventType::Notice => {
                stats.retained_notices = stats.retained_notices.saturating_add(1);
            }
            EventType::ToolCall => {
                stats.retained_tool_calls = stats.retained_tool_calls.saturating_add(1);
            }
            _ => {}
        }
    }
    stats.retained_body_bytes = stats
        .retained_body_bytes
        .saturating_add(retained_body_bytes(events) as u64);
}

pub(super) struct BoundedLine {
    pub(super) bytes: Vec<u8>,
    pub(super) consumed_bytes: u64,
    pub(super) payload_bytes: u64,
    pub(super) terminated: bool,
    pub(super) oversized: bool,
    pub(super) content_sha256: [u8; 32],
}

pub(super) fn read_bounded_line(
    reader: &mut impl BufRead,
    max_line_bytes: usize,
) -> io::Result<BoundedLine> {
    let mut bytes = Vec::new();
    let mut consumed_bytes = 0_u64;
    let mut terminated = false;
    let mut oversized = false;
    let mut content_hasher = Sha256::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position.saturating_add(1));
        let remaining = max_line_bytes.saturating_add(2).saturating_sub(bytes.len());
        let copy = remaining.min(take);
        bytes.extend_from_slice(&available[..copy]);
        content_hasher.update(&available[..take]);
        if copy < take {
            oversized = true;
        }
        consumed_bytes = consumed_bytes.saturating_add(take as u64);
        terminated = available.get(take.saturating_sub(1)) == Some(&b'\n');
        reader.consume(take);
        if terminated {
            break;
        }
    }
    let ending_bytes = if terminated && bytes.ends_with(b"\r\n") {
        2
    } else if terminated {
        1
    } else {
        0
    };
    let payload_bytes = consumed_bytes.saturating_sub(ending_bytes);
    if payload_bytes > max_line_bytes as u64 {
        oversized = true;
    }
    Ok(BoundedLine {
        bytes,
        consumed_bytes,
        payload_bytes,
        terminated,
        oversized,
        content_sha256: content_hasher.finalize().into(),
    })
}

pub(super) fn strip_line_ending(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    bytes.strip_suffix(b"\r").unwrap_or(bytes)
}
