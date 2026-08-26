#[cfg(test)]
use std::io::BufRead;
use std::{
    fmt,
    io::{BufReader, Seek, SeekFrom},
};
#[cfg(test)]
use std::{fs::File, io::Read};

use chrono::{DateTime, Utc};
use ctx_history_core::{AgentScope, EventRole, EventType};
use serde::{
    de::{IgnoredAny, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
use crate::GeminiResult;
use crate::{GeminiError, PROVIDER_MAX_PREVIEW_CHARS};
use ctx_history_jsonl::{read_bounded_record_unhashed, JsonlRecordFraming};
use ctx_history_source_io::MAX_PROVIDER_JSONL_LINE_BYTES;

#[cfg(test)]
use super::dto::{
    GeminiCheckpoint, GeminiCompleteness, GeminiLifecycleSignals, GeminiPageFrontier,
    GeminiPageIdentity, GeminiParserMetrics, GeminiPreviousSource, GeminiPublicationShape,
    GeminiRejection, GeminiRejectionKind, GeminiScanOutcome, GeminiSourceChange,
    GEMINI_NATIVEPATH_PARSER_REVISION, GEMINI_NATIVEPATH_POLICY_REVISION,
};
use super::dto::{
    GeminiEventBody, GeminiEventIdentity, GeminiFileObservation, GeminiNativeOrder,
    GeminiRetainedEvent, GeminiScanError, GeminiScanResult, GeminiSession,
    GeminiSourceRecordEvidence, GeminiToolCall, GeminiTranscriptLayout, GeminiTranscriptSource,
};

mod identity;
mod paging;
#[cfg(test)]
mod reader;
#[cfg(test)]
mod resume;
mod selective;
#[cfg(test)]
mod source;

use paging::*;
use selective::*;
#[cfg(test)]
use source::*;

pub(super) use identity::GeminiNativeEventIds;
#[cfg(test)]
pub(crate) use reader::GeminiNativePageReader;
#[cfg(test)]
pub(crate) use resume::read_gemini_transcript_pages;
#[cfg(test)]
pub(crate) use resume::read_gemini_transcript_pages_from_frontier;

const BODY_HASH_DOMAIN: &[u8] = b"ctx-gemini-nativepath-retained-body-v1\0";
const RESULT_FALLBACK_ID_DOMAIN: &[u8] = b"ctx-gemini-nativepath-result-fallback-id-v1\0";
#[cfg(test)]
const PREFIX_HASH_DOMAIN: &[u8] = b"ctx-gemini-nativepath-complete-prefix-v1\0";
#[cfg(test)]
const CORE_PAGE_IDENTITY_DOMAIN: &[u8] = b"ctx-gemini-nativepath-core-page-v2\0";
#[cfg(test)]
const PREFIX_HASH_BUFFER_BYTES: usize = 64 * 1024;
#[cfg(test)]
const PAGE_ENVELOPE_FIXED_BYTES: usize = 4 * 1024;
const EVENT_ENVELOPE_FIXED_BYTES: usize = 1024;
#[cfg(test)]
const REJECTION_ENVELOPE_FIXED_BYTES: usize = 512;
#[cfg(test)]
const MAX_REJECTION_DETAILS: usize = 32;
pub(super) const MAX_GEMINI_NATIVE_PAGE_RECORDS: usize = 64;
pub(super) const MAX_GEMINI_NATIVE_PAGE_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAX_GEMINI_SINGLE_RECORD_PAGE_BYTES: usize =
    MAX_PROVIDER_JSONL_LINE_BYTES * 2 + 1024 * 1024;
pub(super) const MAX_GEMINI_NATIVE_EVENT_IDS: usize = MAX_GEMINI_NATIVE_PAGE_RECORDS;
pub(super) const MAX_GEMINI_NATIVE_EVENT_ID_BYTES: usize = MAX_GEMINI_NATIVE_PAGE_BYTES;

#[cfg(test)]
thread_local! {
    static TEST_RECORD_READS: Cell<u64> = const { Cell::new(0) };
    static TEST_PREFIX_BYTES_HASHED: Cell<u64> = const { Cell::new(0) };
    static TEST_RESULT_SELECTIVE_PASSES: Cell<u64> = const { Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_gemini_parse_counters() {
    TEST_RECORD_READS.set(0);
    TEST_PREFIX_BYTES_HASHED.set(0);
    TEST_RESULT_SELECTIVE_PASSES.set(0);
}

#[cfg(test)]
pub(super) fn gemini_parse_counters() -> (u64, u64) {
    (TEST_RECORD_READS.get(), TEST_RESULT_SELECTIVE_PASSES.get())
}

#[cfg(test)]
pub(super) fn gemini_resume_work_counters() -> (u64, u64) {
    (TEST_RECORD_READS.get(), TEST_PREFIX_BYTES_HASHED.get())
}

/// A bounded canonical Core group of Gemini records. Empty Core payloads make
/// physical scan progress observable when native records produce no retained
/// events.
#[derive(Debug)]
#[cfg(test)]
pub(crate) struct GeminiNativePage {
    pub(crate) identity: GeminiPageIdentity,
    pub(crate) expected_frontier: GeminiPageFrontier,
    pub(crate) next_safe_frontier: GeminiPageFrontier,
    /// EOF was reached and final source metadata was revalidated while
    /// producing this page. Catalog completion remains a coordinator concern.
    pub(crate) terminal: bool,
    pub(crate) events: Vec<GeminiRetainedEvent>,
    /// Deterministic structural rejections durably carried by this page.
    pub(crate) rejections: Vec<GeminiRejection>,
    pub(crate) physical_records: usize,
    /// Core events plus durable structural rejections only.
    pub(crate) logical_units: usize,
    pub(crate) retained_event_bytes: usize,
    /// Conservative serialized bytes for the canonical Core page only.
    pub(crate) conservative_serialized_bytes: usize,
}

/// Provider-owned borrowed-record projection used by the shared JSONL
/// replacement family. The shared family owns physical scanning and
/// replacement lifecycle; this state retains Gemini's header, event parsing,
/// and bounded duplicate-identity semantics.
pub(crate) struct GeminiBorrowedRecordParser {
    source: GeminiTranscriptSource,
    session: GeminiSession,
    header_seen: bool,
    page_native_event_ids: GeminiNativeEventIds,
    page_physical_records: usize,
}

pub(crate) struct GeminiBorrowedRecordProjection {
    pub(crate) events: Vec<GeminiRetainedEvent>,
    pub(crate) rejection: Option<(GeminiRecordRejectionKind, String)>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum GeminiRecordRejectionKind {
    Malformed,
    Unsupported,
}

impl GeminiBorrowedRecordProjection {
    fn events(events: Vec<GeminiRetainedEvent>) -> Self {
        Self {
            events,
            rejection: None,
        }
    }

    fn ignored() -> Self {
        Self::events(Vec::new())
    }

    fn rejected(kind: GeminiRecordRejectionKind, reason: impl Into<String>) -> Self {
        Self {
            events: Vec::new(),
            rejection: Some((kind, reason.into())),
        }
    }
}

impl GeminiBorrowedRecordParser {
    pub(crate) fn new(source: GeminiTranscriptSource, session: GeminiSession) -> Self {
        Self {
            source,
            session,
            header_seen: false,
            page_native_event_ids: GeminiNativeEventIds::default(),
            page_physical_records: 0,
        }
    }

    pub(crate) fn project(
        &mut self,
        payload: &[u8],
        raw_ordinal: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
        record_digest: [u8; 32],
    ) -> GeminiScanResult<GeminiBorrowedRecordProjection> {
        if self.page_physical_records == MAX_GEMINI_NATIVE_PAGE_RECORDS {
            self.page_native_event_ids = GeminiNativeEventIds::default();
            self.page_physical_records = 0;
        }
        self.page_physical_records = self.page_physical_records.saturating_add(1);

        if payload.iter().all(u8::is_ascii_whitespace) {
            return Ok(GeminiBorrowedRecordProjection::ignored());
        }
        let probe = match serde_json::from_slice::<GeminiRecordProbe>(payload) {
            Ok(probe) => probe,
            Err(error) => {
                let kind = match error.classify() {
                    serde_json::error::Category::Syntax | serde_json::error::Category::Eof => {
                        GeminiRecordRejectionKind::Malformed
                    }
                    serde_json::error::Category::Data | serde_json::error::Category::Io => {
                        GeminiRecordRejectionKind::Unsupported
                    }
                };
                return Ok(GeminiBorrowedRecordProjection::rejected(
                    kind,
                    format!("Gemini JSONL record could not be decoded: {error}"),
                ));
            }
        };
        let class = probe.classify();
        if class == GeminiRecordClass::Header {
            if self.header_seen {
                return Err(GeminiScanError::UncommittedRecord {
                    raw_ordinal,
                    byte_start,
                    byte_end_exclusive,
                    reason: "a second Gemini session header appeared in one transcript".to_owned(),
                });
            }
            let session = decode_header(payload, &self.source.layout).map_err(|reason| {
                GeminiScanError::UncommittedRecord {
                    raw_ordinal,
                    byte_start,
                    byte_end_exclusive,
                    reason,
                }
            })?;
            if session != self.session {
                return Err(GeminiError::SourceChangedDuringCapture.into());
            }
            self.header_seen = true;
            return Ok(GeminiBorrowedRecordProjection::ignored());
        }
        if !self.header_seen {
            return Ok(GeminiBorrowedRecordProjection::ignored());
        }

        let native_event_id = nonempty(probe.id.clone());
        if let Some(native_event_id) = native_event_id.as_deref() {
            if self
                .page_native_event_ids
                .validate(native_event_id, raw_ordinal)
                .is_err()
            {
                return Ok(GeminiBorrowedRecordProjection::ignored());
            }
        }

        let source_record = GeminiSourceRecordEvidence {
            byte_offset: byte_start,
            byte_length: byte_end_exclusive.saturating_sub(byte_start),
            record_digest,
        };
        let events = match class {
            GeminiRecordClass::Result => {
                let decoded = match decode_result_record(payload, raw_ordinal, source_record) {
                    Ok(decoded) => decoded,
                    Err(reason) => {
                        return Ok(GeminiBorrowedRecordProjection::rejected(
                            GeminiRecordRejectionKind::Unsupported,
                            reason,
                        ))
                    }
                };
                if decoded
                    .events
                    .iter()
                    .any(|(_, bytes)| *bytes > MAX_GEMINI_SINGLE_RECORD_PAGE_BYTES)
                {
                    return Ok(GeminiBorrowedRecordProjection::rejected(
                        GeminiRecordRejectionKind::Unsupported,
                        "Gemini result record exceeds the bounded event size",
                    ));
                }
                decoded.events.into_iter().map(|(event, _)| event).collect()
            }
            GeminiRecordClass::Message
            | GeminiRecordClass::ToolCall
            | GeminiRecordClass::StateNotice
            | GeminiRecordClass::RewindNotice => {
                let decoded =
                    match decode_retained_event(payload, class, raw_ordinal, source_record) {
                        Ok(decoded) => decoded,
                        Err(GeminiDecodingError::Invalid(reason)) => {
                            return Ok(GeminiBorrowedRecordProjection::rejected(
                                GeminiRecordRejectionKind::Unsupported,
                                reason,
                            ));
                        }
                    };
                let mut events = Vec::with_capacity(decoded.len());
                for decoded in decoded {
                    let event_bytes = match retained_event_bytes(&decoded) {
                        Ok(event_bytes) => event_bytes,
                        Err(reason) => {
                            return Ok(GeminiBorrowedRecordProjection::rejected(
                                GeminiRecordRejectionKind::Unsupported,
                                reason,
                            ))
                        }
                    };
                    if event_bytes > MAX_GEMINI_SINGLE_RECORD_PAGE_BYTES {
                        return Ok(GeminiBorrowedRecordProjection::rejected(
                            GeminiRecordRejectionKind::Unsupported,
                            "Gemini record exceeds the bounded event size",
                        ));
                    }
                    let mut event = decoded.event;
                    if event.occurred_at.is_none() {
                        event.occurred_at = self.session.started_at;
                    }
                    events.push(event);
                }
                events
            }
            GeminiRecordClass::Ignored | GeminiRecordClass::Header => Vec::new(),
        };
        if let Some(native_event_id) = native_event_id {
            self.page_native_event_ids
                .commit_at(native_event_id, raw_ordinal);
        }
        Ok(GeminiBorrowedRecordProjection::events(events))
    }

    pub(crate) fn finish(&self) -> GeminiScanResult<()> {
        if self.header_seen {
            Ok(())
        } else {
            Err(GeminiScanError::UncommittedRecord {
                raw_ordinal: 0,
                byte_start: 0,
                byte_end_exclusive: 0,
                reason: "Gemini source has no importable native session header".to_owned(),
            })
        }
    }
}

/// Reads only through the first importable header. This is the bounded
/// identity probe needed to preserve Gemini's native-session source identity
/// without performing provider projection during discovery or an exact no-op.
pub(crate) fn read_gemini_session_header(
    source: &GeminiTranscriptSource,
) -> GeminiScanResult<GeminiSession> {
    let source_file = source.open()?;
    let opening = GeminiFileObservation::from_metadata(source_file.metadata())?;
    if opening != source.observation {
        return Err(GeminiError::SourceChangedDuringCapture.into());
    }
    let mut file = source_file.file().try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut raw_ordinal = 0_u64;
    let mut offset = 0_u64;

    while offset < opening.length {
        let record = read_bounded_record_unhashed(
            &mut reader,
            &mut line,
            opening.length.saturating_sub(offset),
            JsonlRecordFraming::new(MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(1), false),
            || GeminiError::SourceChangedDuringCapture,
        )?
        .ok_or_else(|| GeminiScanError::UncommittedRecord {
            raw_ordinal,
            byte_start: offset,
            byte_end_exclusive: offset,
            reason: "Gemini source has no importable native session header".to_owned(),
        })?;
        let byte_start = offset;
        offset = offset.saturating_add(record.byte_len);
        if !record.complete {
            return Err(GeminiScanError::UncommittedRecord {
                raw_ordinal,
                byte_start,
                byte_end_exclusive: offset,
                reason: "Gemini session header record is incomplete".to_owned(),
            });
        }
        if !record.oversized {
            let payload = line.strip_suffix(b"\r").unwrap_or(&line);
            if !payload.iter().all(u8::is_ascii_whitespace) {
                if let Ok(probe) = serde_json::from_slice::<GeminiRecordProbe>(payload) {
                    if probe.classify() == GeminiRecordClass::Header {
                        let session = decode_header(payload, &source.layout).map_err(|reason| {
                            GeminiScanError::UncommittedRecord {
                                raw_ordinal,
                                byte_start,
                                byte_end_exclusive: offset,
                                reason,
                            }
                        })?;
                        if GeminiFileObservation::from_metadata(&reader.get_ref().metadata()?)?
                            != opening
                        {
                            return Err(GeminiError::SourceChangedDuringCapture.into());
                        }
                        source_file.revalidate_leaf()?;
                        source.authority.revalidate()?;
                        return Ok(session);
                    }
                }
            }
        }
        raw_ordinal = raw_ordinal.saturating_add(1);
    }
    Err(GeminiScanError::UncommittedRecord {
        raw_ordinal,
        byte_start: offset,
        byte_end_exclusive: offset,
        reason: "Gemini source has no importable native session header".to_owned(),
    })
}
