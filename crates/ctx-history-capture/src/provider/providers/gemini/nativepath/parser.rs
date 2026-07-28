use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
};

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

use super::dto::{
    GeminiCheckpoint, GeminiCompleteness, GeminiEventBody, GeminiEventIdentity,
    GeminiFileObservation, GeminiLifecycleSignals, GeminiNativeOrder, GeminiNativePathProfile,
    GeminiPageFrontier, GeminiPageIdentity, GeminiParserMetrics, GeminiPreviousSource,
    GeminiPublicationShape, GeminiRejection, GeminiRejectionKind, GeminiRetainedEvent,
    GeminiScanError, GeminiScanOutcome, GeminiScanResult, GeminiSession, GeminiSourceChange,
    GeminiSourceLocator, GeminiSourceRecordEvidence, GeminiToolCall, GeminiTouchOverflow,
    GeminiTranscriptLayout, GeminiTranscriptSource, GEMINI_NATIVEPATH_PARSER_REVISION,
    GEMINI_NATIVEPATH_POLICY_REVISION,
};

#[cfg(test)]
use super::dto::GeminiOutputPageIdentity;

mod identity;
mod paging;
mod reader;
mod resume;
mod selective;
mod source;

use paging::*;
use selective::*;
use source::*;

pub(super) use identity::GeminiNativeEventIds;
pub(crate) use reader::GeminiNativePageReader;
pub(crate) use resume::read_gemini_transcript_pages_with_profile;
#[cfg(test)]
pub(crate) use resume::{read_gemini_transcript_pages, read_gemini_transcript_pages_from_frontier};

const BODY_HASH_DOMAIN: &[u8] = b"ctx-gemini-nativepath-retained-body-v1\0";
const RESULT_STRING_HASH_DOMAIN: &[u8] = b"ctx-gemini-nativepath-result-string-v1\0";
const RESULT_FALLBACK_ID_DOMAIN: &[u8] = b"ctx-gemini-nativepath-result-fallback-id-v1\0";
const OUTPUT_UNIT_KEY_DOMAIN: &[u8] = b"ctx-gemini-nativepath-output-unit-key-v1\0";
const PREFIX_HASH_DOMAIN: &[u8] = b"ctx-gemini-nativepath-complete-prefix-v1\0";
const CORE_PAGE_IDENTITY_DOMAIN: &[u8] = b"ctx-gemini-nativepath-core-page-v2\0";
#[cfg(test)]
const OUTPUT_PAGE_IDENTITY_DOMAIN: &[u8] = b"ctx-gemini-nativepath-output-page-v2\0";
const PREFIX_HASH_BUFFER_BYTES: usize = 64 * 1024;
const PAGE_ENVELOPE_FIXED_BYTES: usize = 4 * 1024;
const EVENT_ENVELOPE_FIXED_BYTES: usize = 1024;
const OUTPUT_ENVELOPE_FIXED_BYTES: usize = 1024;
const REJECTION_ENVELOPE_FIXED_BYTES: usize = 512;
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
    static TEST_RESULT_FULL_HYDRATIONS: Cell<u64> = const { Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_gemini_parse_counters() {
    TEST_RECORD_READS.set(0);
    TEST_PREFIX_BYTES_HASHED.set(0);
    TEST_RESULT_SELECTIVE_PASSES.set(0);
    TEST_RESULT_FULL_HYDRATIONS.set(0);
}

#[cfg(test)]
pub(super) fn gemini_parse_counters() -> (u64, u64, u64) {
    (
        TEST_RECORD_READS.get(),
        TEST_RESULT_SELECTIVE_PASSES.get(),
        TEST_RESULT_FULL_HYDRATIONS.get(),
    )
}

#[cfg(test)]
pub(super) fn gemini_resume_work_counters() -> (u64, u64) {
    (TEST_RECORD_READS.get(), TEST_PREFIX_BYTES_HASHED.get())
}

/// One independently bounded transient-output page nested under a canonical
/// Core page. Every Core page has at least one of these in the output-enabled
/// profile, including when no output observations were produced.
#[derive(Debug)]
pub(crate) struct GeminiNativeOutputPage {
    #[cfg(test)]
    pub(crate) identity: GeminiOutputPageIdentity,
    #[cfg(test)]
    pub(crate) page_ordinal: u32,
    pub(crate) outputs: Vec<ProOutputObservation>,
    pub(crate) logical_units: usize,
    pub(crate) conservative_serialized_bytes: usize,
}

/// A bounded canonical Core group of Gemini records. Empty Core payloads make
/// physical scan progress observable when native records produce no retained
/// events. Transient output pages are bounded independently and never
/// participate in this page's segmentation or identity.
#[derive(Debug)]
pub(crate) struct GeminiNativePage {
    pub(crate) identity: GeminiPageIdentity,
    pub(crate) expected_frontier: GeminiPageFrontier,
    pub(crate) next_safe_frontier: GeminiPageFrontier,
    /// EOF was reached and final source metadata was revalidated while
    /// producing this page. Catalog completion remains a coordinator concern.
    pub(crate) terminal: bool,
    pub(crate) events: Vec<GeminiRetainedEvent>,
    pub(crate) output_pages: Vec<GeminiNativeOutputPage>,
    /// Deterministic structural rejections durably carried by this page.
    pub(crate) rejections: Vec<GeminiRejection>,
    pub(crate) physical_records: usize,
    /// Core events plus durable structural rejections only.
    pub(crate) logical_units: usize,
    pub(crate) retained_event_bytes: usize,
    /// Conservative serialized bytes for the canonical Core page only.
    pub(crate) conservative_serialized_bytes: usize,
}
