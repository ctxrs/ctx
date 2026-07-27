use std::io::{self, BufRead};

use ctx_history_core::EventType;
use sha2::{Digest, Sha256};

use crate::{Result, MAX_PROVIDER_JSONL_LINE_BYTES};

use super::super::{
    checkpoint::{CursorCheckpoint, CursorPrefixBuilder, CursorSessionCheckpoint},
    projection::{
        project_cursor_record, retained_body_bytes, update_cursor_session_checkpoint,
        CursorNativeEvent, CursorPageBuffer, CursorPublicationSink,
    },
};
use super::{
    classify_cursor_line, decode_sanitized_record, CursorRecordRejection, CursorRejectionKind,
    CursorRejectionSummary,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CursorParserStats {
    pub(crate) bytes_read: u64,
    pub(crate) verification_bytes_read: u64,
    pub(crate) projected_bytes_read: u64,
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
    pub(crate) publication_pages: u64,
    pub(crate) nativepath_publication_rows: u64,
    pub(crate) publication_serialized_bytes: u64,
    pub(crate) max_publication_page_rows: usize,
    pub(crate) max_publication_page_bytes: usize,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum CursorParserPlan<'a> {
    FullSnapshot,
    VerifyPrefixAndResume(&'a CursorCheckpoint),
}

#[derive(Debug)]
pub(crate) enum CursorParserOutcome {
    Parsed(Box<CursorParsedGeneration>),
    PrefixMismatch(Box<CursorParserStats>),
}

#[derive(Debug)]
pub(crate) struct CursorParsedGeneration {
    #[cfg(test)]
    pub(crate) events: Vec<super::super::projection::CursorNativeEvent>,
    pub(crate) rejections: CursorRejectionSummary,
    pub(crate) checkpoint: CursorCheckpoint,
    pub(crate) stats: CursorParserStats,
    pub(crate) resumed: bool,
}

pub(crate) fn scan_cursor_reader(
    reader: &mut impl BufRead,
    plan: CursorParserPlan<'_>,
    sink: &mut dyn CursorPublicationSink,
) -> Result<CursorParserOutcome> {
    scan_cursor_reader_with_limit(reader, plan, sink, MAX_PROVIDER_JSONL_LINE_BYTES)
}

pub(super) fn scan_cursor_reader_with_limit(
    reader: &mut impl BufRead,
    plan: CursorParserPlan<'_>,
    sink: &mut dyn CursorPublicationSink,
    max_line_bytes: usize,
) -> Result<CursorParserOutcome> {
    let resume = match plan {
        CursorParserPlan::FullSnapshot => None,
        CursorParserPlan::VerifyPrefixAndResume(checkpoint) if checkpoint.is_supported() => {
            Some(checkpoint)
        }
        CursorParserPlan::VerifyPrefixAndResume(_) => {
            return Ok(CursorParserOutcome::PrefixMismatch(Box::default()));
        }
    };
    let verification_offset = resume.map_or(0, |checkpoint| checkpoint.next_byte_offset);
    let mut prefix = CursorPrefixBuilder::new();
    let mut rejections = CursorRejectionSummary::default();
    let mut stats = CursorParserStats::default();
    let mut session = CursorSessionCheckpoint::default();
    let initial_page_checkpoint = resume
        .cloned()
        .unwrap_or_else(|| CursorCheckpoint::new(prefix.proof(), session.clone(), false, false));
    let mut pages = CursorPageBuffer::new(sink, initial_page_checkpoint);
    let mut saw_incomplete = false;
    let mut verified_prefix = resume.is_none() || verification_offset == 0;
    if let Some(checkpoint) = resume.filter(|_| verification_offset == 0) {
        if prefix.proof() != checkpoint.prefix || session != checkpoint.session {
            return Ok(CursorParserOutcome::PrefixMismatch(Box::new(stats)));
        }
    }

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
        let next_offset = prefix.complete_bytes().saturating_add(line.consumed_bytes);
        if resume.is_some() && !verified_prefix && next_offset > verification_offset {
            return Ok(CursorParserOutcome::PrefixMismatch(Box::new(stats)));
        }
        let verifying = resume.is_some() && next_offset <= verification_offset;
        if verifying {
            stats.verification_bytes_read = stats
                .verification_bytes_read
                .saturating_add(line.consumed_bytes);
        } else {
            stats.projected_bytes_read = stats
                .projected_bytes_read
                .saturating_add(line.consumed_bytes);
        }
        process_complete_line(
            &line,
            verifying,
            &mut prefix,
            &mut pages,
            &mut rejections,
            &mut stats,
            &mut session,
        )?;
        if let Some(checkpoint) =
            resume.filter(|_| !verified_prefix && next_offset == verification_offset)
        {
            if prefix.proof() != checkpoint.prefix || session != checkpoint.session {
                return Ok(CursorParserOutcome::PrefixMismatch(Box::new(stats)));
            }
            verified_prefix = true;
        }
    }

    if let Some(checkpoint) = resume {
        if !verified_prefix || prefix.proof().complete_bytes < checkpoint.next_byte_offset {
            return Ok(CursorParserOutcome::PrefixMismatch(Box::new(stats)));
        }
    }
    let proof = prefix.finish();
    let checkpoint = CursorCheckpoint::new(proof, session, !saw_incomplete, rejections.total > 0);
    let page_stats = pages.finish(checkpoint.clone(), rejections.total, &rejections.samples)?;
    stats.publication_pages = page_stats.pages;
    stats.nativepath_publication_rows = page_stats.rows;
    stats.publication_serialized_bytes = page_stats.serialized_bytes;
    stats.max_publication_page_rows = page_stats.max_page_rows;
    stats.max_publication_page_bytes = page_stats.max_page_bytes;
    Ok(CursorParserOutcome::Parsed(Box::new(
        CursorParsedGeneration {
            #[cfg(test)]
            events: Vec::new(),
            rejections,
            checkpoint,
            stats,
            resumed: resume.is_some(),
        },
    )))
}

#[cfg_attr(test, allow(clippy::too_many_arguments))]
fn process_complete_line(
    line: &BoundedLine,
    verifying: bool,
    prefix: &mut CursorPrefixBuilder,
    pages: &mut CursorPageBuffer<'_>,
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
    let sanitized = match decode_sanitized_record(payload, semantic_ordinal, &classification) {
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
    if !verifying {
        let next_checkpoint =
            CursorCheckpoint::new(prefix.proof(), session.clone(), false, rejections.total > 0);
        for event in projected {
            pages.push(
                event,
                &next_checkpoint,
                rejections.total,
                &rejections.samples,
            )?;
        }
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
