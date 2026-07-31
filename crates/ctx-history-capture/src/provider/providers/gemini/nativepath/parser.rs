use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::{BufRead, BufReader, Seek, SeekFrom},
};
#[cfg(test)]
use std::{fs::File, io::Read};

use chrono::{DateTime, Utc};
use ctx_history_core::{AgentType, EventRole, EventType};
use serde::{
    de::{IgnoredAny, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[cfg(test)]
use std::cell::Cell;

use crate::{
    CaptureError, OutputAssociations, OutputNativeCoordinate, OutputObservationKind, OutputOutcome,
    OutputOutcomeMetadata, OutputSourceLocator, ProOutputObservation, Result,
    MAX_PROVIDER_JSONL_LINE_BYTES, PROVIDER_MAX_PREVIEW_CHARS,
};

#[cfg(test)]
use super::dto::{
    GeminiCheckpoint, GeminiCompleteness, GeminiLifecycleSignals, GeminiPageFrontier,
    GeminiPageIdentity, GeminiParserMetrics, GeminiPreviousSource, GeminiPublicationShape,
    GeminiRejection, GeminiRejectionKind, GeminiScanOutcome, GeminiSourceChange,
    GEMINI_NATIVEPATH_PARSER_REVISION, GEMINI_NATIVEPATH_POLICY_REVISION,
};
use super::dto::{
    GeminiEventBody, GeminiEventIdentity, GeminiFileObservation, GeminiNativeOrder,
    GeminiNativePathProfile, GeminiRetainedEvent, GeminiScanError, GeminiScanResult, GeminiSession,
    GeminiSourceLocator, GeminiSourceRecordEvidence, GeminiToolCall, GeminiTouchOverflow,
    GeminiTranscriptLayout, GeminiTranscriptSource,
};

mod identity;
mod paging;
#[cfg(test)]
mod reader;
#[cfg(test)]
mod resume;
mod selective;
mod source;

use paging::*;
use selective::*;
use source::*;

pub(super) use identity::GeminiNativeEventIds;
#[cfg(test)]
pub(crate) use reader::GeminiNativePageReader;
#[cfg(test)]
pub(crate) use resume::read_gemini_transcript_pages;
#[cfg(test)]
pub(crate) use resume::read_gemini_transcript_pages_from_frontier;

const BODY_HASH_DOMAIN: &[u8] = b"ctx-gemini-nativepath-retained-body-v1\0";
const RESULT_STRING_HASH_DOMAIN: &[u8] = b"ctx-gemini-nativepath-result-string-v1\0";
const RESULT_FALLBACK_ID_DOMAIN: &[u8] = b"ctx-gemini-nativepath-result-fallback-id-v1\0";
const OUTPUT_UNIT_KEY_DOMAIN: &[u8] = b"ctx-gemini-nativepath-output-unit-key-v1\0";
const PREFIX_HASH_DOMAIN: &[u8] = b"ctx-gemini-nativepath-complete-prefix-v1\0";
#[cfg(test)]
const CORE_PAGE_IDENTITY_DOMAIN: &[u8] = b"ctx-gemini-nativepath-core-page-v2\0";
#[cfg(test)]
const PREFIX_HASH_BUFFER_BYTES: usize = 64 * 1024;
#[cfg(test)]
const PAGE_ENVELOPE_FIXED_BYTES: usize = 4 * 1024;
const EVENT_ENVELOPE_FIXED_BYTES: usize = 1024;
const OUTPUT_ENVELOPE_FIXED_BYTES: usize = 1024;
#[cfg(test)]
const REJECTION_ENVELOPE_FIXED_BYTES: usize = 512;
#[cfg(test)]
const MAX_REJECTION_DETAILS: usize = 32;
pub(super) const MAX_GEMINI_FILE_TOUCHES_PER_EVENT: usize = 256;
pub(super) const MAX_GEMINI_FILE_TOUCH_BYTES_PER_EVENT: usize = 64 * 1024;
pub(super) const MAX_GEMINI_NATIVE_PAGE_RECORDS: usize = 64;
pub(super) const MAX_GEMINI_NATIVE_PAGE_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAX_GEMINI_NATIVE_EVENT_IDS: usize = MAX_GEMINI_NATIVE_PAGE_RECORDS;
pub(super) const MAX_GEMINI_NATIVE_EVENT_ID_BYTES: usize = MAX_GEMINI_NATIVE_PAGE_BYTES;

#[cfg(test)]
thread_local! {
    static TEST_RECORD_READS: Cell<u64> = const { Cell::new(0) };
    static TEST_PREFIX_BYTES_HASHED: Cell<u64> = const { Cell::new(0) };
    static TEST_RESULT_SELECTIVE_PASSES: Cell<u64> = const { Cell::new(0) };
    static TEST_RESULT_FULL_DECODINGS: Cell<u64> = const { Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_gemini_parse_counters() {
    TEST_RECORD_READS.set(0);
    TEST_PREFIX_BYTES_HASHED.set(0);
    TEST_RESULT_SELECTIVE_PASSES.set(0);
    TEST_RESULT_FULL_DECODINGS.set(0);
}

#[cfg(test)]
pub(super) fn gemini_parse_counters() -> (u64, u64, u64) {
    (
        TEST_RECORD_READS.get(),
        TEST_RESULT_SELECTIVE_PASSES.get(),
        TEST_RESULT_FULL_DECODINGS.get(),
    )
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
    ) -> GeminiScanResult<Vec<GeminiRetainedEvent>> {
        if self.page_physical_records == MAX_GEMINI_NATIVE_PAGE_RECORDS {
            self.page_native_event_ids = GeminiNativeEventIds::default();
            self.page_physical_records = 0;
        }
        self.page_physical_records = self.page_physical_records.saturating_add(1);

        if payload.iter().all(u8::is_ascii_whitespace) {
            return Ok(Vec::new());
        }
        let probe = match serde_json::from_slice::<GeminiRecordProbe>(payload) {
            Ok(probe) => probe,
            Err(_) => return Ok(Vec::new()),
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
                return Err(CaptureError::SourceChangedDuringCapture.into());
            }
            self.header_seen = true;
            return Ok(Vec::new());
        }
        if !self.header_seen {
            return Ok(Vec::new());
        }

        let native_event_id = nonempty(probe.id.clone());
        if let Some(native_event_id) = native_event_id.as_deref() {
            if self
                .page_native_event_ids
                .validate(native_event_id, raw_ordinal)
                .is_err()
            {
                return Ok(Vec::new());
            }
        }

        let source_record = GeminiSourceRecordEvidence {
            byte_offset: byte_start,
            byte_length: byte_end_exclusive.saturating_sub(byte_start),
            record_digest,
        };
        let events = match class {
            GeminiRecordClass::Result => {
                let decoded = match decode_result_record(
                    payload,
                    GeminiNativePathProfile::CoreOnly,
                    &self.source,
                    &self.session,
                    raw_ordinal,
                    source_record,
                    byte_start,
                    byte_end_exclusive,
                ) {
                    Ok(decoded) => decoded,
                    Err(_) => return Ok(Vec::new()),
                };
                if decoded
                    .events
                    .iter()
                    .any(|(_, bytes)| *bytes > MAX_GEMINI_NATIVE_PAGE_BYTES)
                {
                    return Ok(Vec::new());
                }
                decoded.events.into_iter().map(|(event, _)| event).collect()
            }
            GeminiRecordClass::Message
            | GeminiRecordClass::ToolCall
            | GeminiRecordClass::StateNotice
            | GeminiRecordClass::RewindNotice => {
                let decoded =
                    match decode_retained_event(payload, class, raw_ordinal, source_record) {
                        Ok(Some(decoded)) => decoded,
                        Ok(None) => {
                            if let Some(native_event_id) = native_event_id {
                                self.page_native_event_ids
                                    .commit_at(native_event_id, raw_ordinal);
                            }
                            return Ok(Vec::new());
                        }
                        Err(GeminiDecodingError::Invalid(reason)) => {
                            drop(reason);
                            return Ok(Vec::new());
                        }
                        Err(GeminiDecodingError::TouchOverflow(error)) => {
                            drop(error.to_string());
                            return Ok(Vec::new());
                        }
                    };
                let Ok(event_bytes) = retained_event_bytes(&decoded) else {
                    return Ok(Vec::new());
                };
                if event_bytes > MAX_GEMINI_NATIVE_PAGE_BYTES {
                    return Ok(Vec::new());
                }
                let mut event = decoded.event;
                if event.occurred_at.is_none() {
                    event.occurred_at = self.session.started_at;
                }
                vec![event]
            }
            GeminiRecordClass::Ignored | GeminiRecordClass::Header => Vec::new(),
        };
        if let Some(native_event_id) = native_event_id {
            self.page_native_event_ids
                .commit_at(native_event_id, raw_ordinal);
        }
        Ok(events)
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
        return Err(CaptureError::SourceChangedDuringCapture.into());
    }
    let mut file = source_file.file().try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut prefix_hasher = new_prefix_hasher();
    let mut source_hasher = prefix_hasher.clone();
    let mut line = Vec::new();
    let mut raw_ordinal = 0_u64;
    let mut offset = 0_u64;

    loop {
        let Some(record) = read_record(
            &mut reader,
            &mut line,
            &mut prefix_hasher,
            &mut source_hasher,
        )?
        else {
            return Err(GeminiScanError::UncommittedRecord {
                raw_ordinal,
                byte_start: offset,
                byte_end_exclusive: offset,
                reason: "Gemini source has no importable native session header".to_owned(),
            });
        };
        let byte_start = offset;
        offset = offset.saturating_add(record.bytes_observed);
        if !record.terminated {
            return Err(GeminiScanError::UncommittedRecord {
                raw_ordinal,
                byte_start,
                byte_end_exclusive: offset,
                reason: "Gemini session header record is incomplete".to_owned(),
            });
        }
        if !record.oversized {
            let payload = trim_jsonl_ending(&line);
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
                            return Err(CaptureError::SourceChangedDuringCapture.into());
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
}
