use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{AgentType, EventRole, EventType};
use serde::{
    de::{IgnoredAny, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
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
    GeminiSourceLocator, GeminiToolCall, GeminiTouchOverflow, GeminiTranscriptLayout,
    GeminiTranscriptSource, GEMINI_NATIVEPATH_PARSER_REVISION, GEMINI_NATIVEPATH_POLICY_REVISION,
};

#[cfg(test)]
use super::dto::GeminiOutputPageIdentity;

const BODY_HASH_DOMAIN: &[u8] = b"ctx-gemini-nativepath-retained-body-v1\0";
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

pub(crate) struct GeminiNativePageReader<'a> {
    source: &'a GeminiTranscriptSource,
    previous: Option<&'a GeminiPreviousSource>,
    initial_observation: GeminiFileObservation,
    source_hasher: Sha256,
    resumed_prefix: bool,
    skip_scan: bool,
    reader: BufReader<File>,
    prefix_hasher: Sha256,
    offset: u64,
    raw_ordinal: u64,
    complete_prefix_end: u64,
    append_boundary_safe: bool,
    terminal: bool,
    retained_event_count: u64,
    state: ScanState<'a>,
    profile: GeminiNativePathProfile,
    outcome: Option<GeminiScanOutcome>,
}

#[cfg(test)]
pub(crate) fn read_gemini_transcript_pages<'a>(
    source: &'a GeminiTranscriptSource,
    previous: Option<&'a GeminiPreviousSource>,
) -> GeminiScanResult<GeminiNativePageReader<'a>> {
    read_gemini_transcript_pages_with_profile(source, previous, GeminiNativePathProfile::CoreOnly)
}

pub(crate) fn read_gemini_transcript_pages_with_profile<'a>(
    source: &'a GeminiTranscriptSource,
    previous: Option<&'a GeminiPreviousSource>,
    profile: GeminiNativePathProfile,
) -> GeminiScanResult<GeminiNativePageReader<'a>> {
    read_gemini_transcript_pages_from(source, previous, profile, None)
}

/// Reopens a source at a previously emitted safe page frontier. This is the
/// retry seam for a lagging Core or Pro consumer: the prefix digest and parser
/// revisions must still match, and growth is accepted only from an
/// append-safe boundary.
#[cfg(test)]
pub(crate) fn read_gemini_transcript_pages_from_frontier<'a>(
    source: &'a GeminiTranscriptSource,
    frontier: &GeminiPageFrontier,
    profile: GeminiNativePathProfile,
) -> GeminiScanResult<GeminiNativePageReader<'a>> {
    read_gemini_transcript_pages_from(source, None, profile, Some(frontier))
}

fn read_gemini_transcript_pages_from<'a>(
    source: &'a GeminiTranscriptSource,
    previous: Option<&'a GeminiPreviousSource>,
    profile: GeminiNativePathProfile,
    resume_frontier: Option<&GeminiPageFrontier>,
) -> GeminiScanResult<GeminiNativePageReader<'a>> {
    let initial_observation = GeminiFileObservation::read(&source.path)?;
    if initial_observation != source.observation {
        return Err(CaptureError::SourceChangedDuringCapture.into());
    }

    let mut file = open_gemini_transcript(&source.path)?;
    if GeminiFileObservation::from_metadata(&file.metadata()?)? != initial_observation {
        return Err(CaptureError::SourceChangedDuringCapture.into());
    }

    let mut prefix_hasher = new_prefix_hasher();
    let mut source_hasher = prefix_hasher.clone();
    let mut resumed_prefix = false;
    let mut skip_scan = false;
    let mut resume_boundary_safe = true;
    let mut terminal = true;
    let mut scan_start = 0_u64;
    let mut next_raw_ordinal = 0_u64;
    let mut retained_event_count = 0_u64;
    let mut rejected_records = 0_u64;
    let mut session = None;

    if let Some(frontier) = resume_frontier {
        if frontier.parser_revision != GEMINI_NATIVEPATH_PARSER_REVISION
            || frontier.policy_revision != GEMINI_NATIVEPATH_POLICY_REVISION
            || initial_observation.length < frontier.complete_prefix_end
            || (frontier.complete_prefix_end > 0 && frontier.session.is_none())
            || (initial_observation.length > frontier.complete_prefix_end
                && !frontier.append_boundary_safe)
            || !frontier_file_identity_matches(frontier, &initial_observation)
        {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        let observed_prefix = hash_gemini_prefix(&mut file, frontier.complete_prefix_end)?;
        if prefix_digest(&observed_prefix) != frontier.complete_prefix_sha256 {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        prefix_hasher = observed_prefix;
        source_hasher = prefix_hasher.clone();
        resumed_prefix = true;
        resume_boundary_safe = frontier.append_boundary_safe;
        scan_start = frontier.complete_prefix_end;
        next_raw_ordinal = frontier.next_raw_ordinal;
        retained_event_count = frontier.retained_event_count;
        rejected_records = frontier.rejected_records;
        session.clone_from(&frontier.session);
    } else if let Some(previous) = previous.filter(|previous| {
        previous.checkpoint.source_path == source.path
            && previous.checkpoint.parser_revision == GEMINI_NATIVEPATH_PARSER_REVISION
            && previous.checkpoint.policy_revision == GEMINI_NATIVEPATH_POLICY_REVISION
            && initial_observation.length >= previous.checkpoint.complete_prefix_end
            && previous.checkpoint.session.is_some()
    }) {
        let checkpoint = &previous.checkpoint;
        let exact_observation = initial_observation == checkpoint.source_observation;
        let append_observation = initial_observation.length > checkpoint.source_observation.length
            && checkpoint.append_boundary_safe
            && same_physical_file(&checkpoint.source_observation, &initial_observation);
        let observed_prefix = hash_gemini_prefix(&mut file, checkpoint.complete_prefix_end)?;
        if (exact_observation || append_observation)
            && prefix_digest(&observed_prefix) == checkpoint.complete_prefix_sha256
        {
            prefix_hasher = observed_prefix;
            source_hasher = prefix_hasher.clone();
            resumed_prefix = true;
            skip_scan = exact_observation
                && checkpoint.terminal
                && initial_observation.length == checkpoint.complete_prefix_end;
            resume_boundary_safe = checkpoint.append_boundary_safe;
            terminal = if skip_scan { checkpoint.terminal } else { true };
            scan_start = checkpoint.complete_prefix_end;
            next_raw_ordinal = checkpoint.next_raw_ordinal;
            retained_event_count = checkpoint.retained_event_count;
            rejected_records = checkpoint.rejected_records;
            session.clone_from(&checkpoint.session);
        }
    }

    if !resumed_prefix {
        file.seek(SeekFrom::Start(0))?;
        prefix_hasher = new_prefix_hasher();
        source_hasher = prefix_hasher.clone();
        scan_start = 0;
        next_raw_ordinal = 0;
        retained_event_count = 0;
        rejected_records = 0;
        session = None;
    } else {
        file.seek(SeekFrom::Start(scan_start))?;
    }

    let state = ScanState {
        source,
        session,
        metrics: GeminiParserMetrics::default(),
        rejected_records,
        rejections: Vec::new(),
        retained_rows_this_scan: 0,
        emitted_rows_this_scan: 0,
    };
    Ok(GeminiNativePageReader {
        source,
        previous,
        initial_observation,
        source_hasher,
        resumed_prefix,
        skip_scan,
        reader: BufReader::new(file),
        prefix_hasher,
        offset: scan_start,
        raw_ordinal: next_raw_ordinal,
        complete_prefix_end: scan_start,
        append_boundary_safe: if resumed_prefix {
            resume_boundary_safe
        } else {
            true
        },
        terminal,
        retained_event_count,
        state,
        profile,
        outcome: None,
    })
}

#[derive(Debug)]
pub(super) struct GeminiNativeEventIds {
    first_raw_ordinals: BTreeMap<String, u64>,
    retained_bytes: usize,
    max_count: usize,
    max_bytes: usize,
}

impl Default for GeminiNativeEventIds {
    fn default() -> Self {
        Self {
            first_raw_ordinals: BTreeMap::new(),
            retained_bytes: 0,
            max_count: MAX_GEMINI_NATIVE_EVENT_IDS,
            max_bytes: MAX_GEMINI_NATIVE_EVENT_ID_BYTES,
        }
    }
}

impl GeminiNativeEventIds {
    #[cfg(test)]
    pub(super) fn with_limits(max_count: usize, max_bytes: usize) -> Self {
        Self {
            first_raw_ordinals: BTreeMap::new(),
            retained_bytes: 0,
            max_count,
            max_bytes,
        }
    }

    #[cfg(test)]
    pub(super) fn insert(
        &mut self,
        native_event_id: String,
        raw_ordinal: u64,
    ) -> GeminiScanResult<()> {
        self.validate(&native_event_id, raw_ordinal)?;
        self.commit_at(native_event_id, raw_ordinal);
        Ok(())
    }

    fn validate(&self, native_event_id: &str, raw_ordinal: u64) -> GeminiScanResult<()> {
        if let Some(first_raw_ordinal) = self.first_raw_ordinals.get(native_event_id) {
            return Err(GeminiScanError::DuplicateNativeEventId {
                native_event_id: native_event_id.to_owned(),
                first_raw_ordinal: *first_raw_ordinal,
                duplicate_raw_ordinal: raw_ordinal,
            });
        }
        if self.first_raw_ordinals.len() >= self.max_count {
            return Err(GeminiScanError::NativeEventIdentityCountOverflow {
                limit: self.max_count,
            });
        }
        let next_bytes = self
            .retained_bytes
            .checked_add(native_event_id.len())
            .ok_or(GeminiScanError::NativeEventIdentityBytesOverflow {
                limit: self.max_bytes,
            })?;
        if next_bytes > self.max_bytes {
            return Err(GeminiScanError::NativeEventIdentityBytesOverflow {
                limit: self.max_bytes,
            });
        }
        Ok(())
    }

    fn commit_at(&mut self, native_event_id: String, raw_ordinal: u64) {
        self.retained_bytes = self.retained_bytes.saturating_add(native_event_id.len());
        self.first_raw_ordinals.insert(native_event_id, raw_ordinal);
    }
}

struct ScanState<'a> {
    source: &'a GeminiTranscriptSource,
    session: Option<GeminiSession>,
    metrics: GeminiParserMetrics,
    rejected_records: u64,
    rejections: Vec<GeminiRejection>,
    retained_rows_this_scan: u64,
    emitted_rows_this_scan: u64,
}

struct GeminiReaderPosition {
    prefix_hasher: Sha256,
    source_hasher: Sha256,
    offset: u64,
    raw_ordinal: u64,
    complete_prefix_end: u64,
    append_boundary_safe: bool,
    terminal: bool,
    retained_event_count: u64,
    metrics: GeminiParserMetrics,
    rejected_records: u64,
    rejection_details: usize,
    retained_rows_this_scan: u64,
    emitted_rows_this_scan: u64,
    session_was_absent: bool,
}

struct ScannedGeminiRecord {
    events: Vec<(GeminiRetainedEvent, usize)>,
    transient_outputs: Vec<(ProOutputObservation, usize)>,
    transient_output_reservations: Vec<usize>,
    rejections: Vec<GeminiRejection>,
    native_event_id: Option<String>,
    completed: bool,
}

impl<'a> GeminiNativePageReader<'a> {
    /// True when the selected provider checkpoint was certified and the scan
    /// continues from it. Provider-owned output adapters use this to choose
    /// append/resume versus a new output epoch without a second source scan.
    pub(crate) fn resumed_from_previous(&self) -> bool {
        self.resumed_prefix
    }

    /// Returns the next bounded page. The caller must drain through `None` to
    /// obtain the final source revalidation and scanner outcome.
    pub(crate) fn next_page(&mut self) -> GeminiScanResult<Option<GeminiNativePage>> {
        if self.outcome.is_some() {
            return Ok(None);
        }
        if self.skip_scan {
            self.finish()?;
            return Ok(None);
        }

        let expected_frontier = self.frontier();
        let initial_page_bytes =
            core_page_conservative_bytes(&expected_frontier, &expected_frontier, 0, 0).ok_or(
                GeminiScanError::Capture(CaptureError::SystemInvariant(
                    "Gemini page accounting overflowed",
                )),
            )?;
        let mut page = GeminiNativePage {
            identity: GeminiPageIdentity([0; 32]),
            expected_frontier: expected_frontier.clone(),
            next_safe_frontier: expected_frontier,
            terminal: false,
            events: Vec::new(),
            output_pages: Vec::new(),
            rejections: Vec::new(),
            physical_records: 0,
            logical_units: 0,
            retained_event_bytes: 0,
            conservative_serialized_bytes: initial_page_bytes,
        };
        let mut transient_outputs = Vec::new();
        // Cross-restart/source-wide duplicate authority belongs to canonical
        // event IDs at the bounded consumer. The provider rejects only IDs
        // that conflict inside the independently retryable page it owns.
        let mut page_native_event_ids = GeminiNativeEventIds::default();

        while page.physical_records < MAX_GEMINI_NATIVE_PAGE_RECORDS {
            let position = self.position();
            let mut record = match self.scan_next_record(&page_native_event_ids) {
                Ok(Some(record)) => record,
                Ok(None) => {
                    self.finish()?;
                    break;
                }
                Err(error) => {
                    self.restore(position)?;
                    if page.physical_records == 0 {
                        return Err(error);
                    }
                    break;
                }
            };
            if !record.completed {
                self.finish()?;
                break;
            }

            let record_units = record
                .events
                .len()
                .checked_add(record.rejections.len())
                .ok_or(GeminiScanError::Capture(CaptureError::SystemInvariant(
                    "Gemini Core page logical-unit accounting overflowed",
                )))?;
            let record_event_bytes = record
                .events
                .iter()
                .try_fold(0_usize, |total, (_, bytes)| total.checked_add(*bytes))
                .ok_or(GeminiScanError::Capture(CaptureError::SystemInvariant(
                    "Gemini retained-event page byte count overflowed",
                )))?;
            let record_rejection_bytes = record
                .rejections
                .iter()
                .try_fold(0_usize, |total, rejection| {
                    total.checked_add(rejection_wire_bytes(rejection)?)
                });
            let record_rejection_bytes = record_rejection_bytes.ok_or(GeminiScanError::Capture(
                CaptureError::SystemInvariant(
                    "Gemini structural rejection page byte count overflowed",
                ),
            ))?;
            let page_rejection_bytes = page
                .rejections
                .iter()
                .try_fold(record_rejection_bytes, |total, rejection| {
                    total.checked_add(rejection_wire_bytes(rejection)?)
                });
            let page_rejection_bytes = page_rejection_bytes.ok_or(GeminiScanError::Capture(
                CaptureError::SystemInvariant(
                    "Gemini structural rejection page byte count overflowed",
                ),
            ))?;
            let next_units =
                page.logical_units
                    .checked_add(record_units)
                    .ok_or(GeminiScanError::Capture(CaptureError::SystemInvariant(
                        "Gemini page logical-unit accounting overflowed",
                    )))?;
            let next_event_bytes = page
                .retained_event_bytes
                .checked_add(record_event_bytes)
                .ok_or(GeminiScanError::Capture(CaptureError::SystemInvariant(
                    "Gemini retained-event page byte count overflowed",
                )))?;
            let next_safe_frontier = self.frontier();
            let next_page_bytes = core_page_conservative_bytes(
                &page.expected_frontier,
                &next_safe_frontier,
                next_event_bytes,
                page_rejection_bytes,
            )
            .ok_or(GeminiScanError::Capture(CaptureError::SystemInvariant(
                "Gemini Core page accounting overflowed",
            )))?;
            let unrepresentable_output = record.transient_output_reservations.iter().try_fold(
                None,
                |unrepresentable, reserved_bytes| -> GeminiScanResult<_> {
                    if unrepresentable.is_some() {
                        return Ok(unrepresentable);
                    }
                    let page_bytes = output_page_conservative_bytes(
                        &next_safe_frontier,
                        &next_safe_frontier,
                        *reserved_bytes,
                    )
                    .ok_or(GeminiScanError::Capture(
                        CaptureError::SystemInvariant(
                            "Gemini output admission accounting overflowed",
                        ),
                    ))?;
                    Ok((page_bytes > MAX_GEMINI_NATIVE_PAGE_BYTES).then_some(page_bytes))
                },
            )?;
            let too_many_units = next_units > MAX_GEMINI_NATIVE_PAGE_RECORDS;
            let too_many_bytes = next_page_bytes > MAX_GEMINI_NATIVE_PAGE_BYTES;
            if too_many_units || too_many_bytes || unrepresentable_output.is_some() {
                let raw_ordinal = position.raw_ordinal;
                let byte_start = position.offset;
                let byte_end_exclusive = self.offset;
                if page.physical_records != 0 {
                    self.restore(position)?;
                    break;
                }
                let reason = if too_many_units {
                    format!(
                        "Gemini native record expands to {record_units} logical units; \
                         page maximum is {MAX_GEMINI_NATIVE_PAGE_RECORDS}"
                    )
                } else if let Some(output_page_bytes) = unrepresentable_output {
                    format!(
                        "Gemini transient output conservatively expands to \
                         {output_page_bytes} serialized page bytes; page maximum is \
                         {MAX_GEMINI_NATIVE_PAGE_BYTES}"
                    )
                } else {
                    format!(
                        "Gemini native record expands to {next_page_bytes} conservative Core \
                         serialized bytes; page maximum is {MAX_GEMINI_NATIVE_PAGE_BYTES}"
                    )
                };
                self.restore(position)?;
                return Err(GeminiScanError::UncommittedRecord {
                    raw_ordinal,
                    byte_start,
                    byte_end_exclusive,
                    reason,
                });
            }

            page.physical_records =
                page.physical_records
                    .checked_add(1)
                    .ok_or(GeminiScanError::Capture(CaptureError::SystemInvariant(
                        "Gemini physical-record page count overflowed",
                    )))?;
            if let Some(native_event_id) = record.native_event_id.take() {
                page_native_event_ids.commit_at(native_event_id, position.raw_ordinal);
            }
            page.logical_units = next_units;
            page.retained_event_bytes = next_event_bytes;
            let emitted_events = record.events.len() as u64;
            page.events
                .extend(record.events.into_iter().map(|(event, _)| event));
            self.state.emitted_rows_this_scan = self
                .state
                .emitted_rows_this_scan
                .saturating_add(emitted_events);
            transient_outputs.extend(record.transient_outputs);
            page.rejections.extend(record.rejections);
            page.next_safe_frontier = next_safe_frontier;
            page.conservative_serialized_bytes = next_page_bytes;
        }

        if self.outcome.is_none()
            && page.physical_records == MAX_GEMINI_NATIVE_PAGE_RECORDS
            && self.reader.fill_buf()?.is_empty()
        {
            self.finish()?;
        }
        if page.physical_records == 0 {
            Ok(None)
        } else {
            self.certify_source_range()?;
            page.terminal = self
                .outcome
                .as_ref()
                .is_some_and(|outcome| outcome.checkpoint.terminal);
            page.identity = derive_page_identity(
                &page.expected_frontier,
                &page.next_safe_frontier,
                &page.events,
                &page.rejections,
                page.terminal,
            );
            if self.profile == GeminiNativePathProfile::CoreAndTransientOutputs {
                page.output_pages = build_output_pages(
                    &page.expected_frontier,
                    &page.next_safe_frontier,
                    transient_outputs,
                    page.terminal,
                )?;
            }
            debug_assert!(page.physical_records <= MAX_GEMINI_NATIVE_PAGE_RECORDS);
            debug_assert!(page.logical_units <= MAX_GEMINI_NATIVE_PAGE_RECORDS);
            debug_assert!(page.conservative_serialized_bytes <= MAX_GEMINI_NATIVE_PAGE_BYTES);
            Ok(Some(page))
        }
    }

    pub(crate) fn outcome(&self) -> Option<&GeminiScanOutcome> {
        self.outcome.as_ref()
    }

    fn certify_source_range(&self) -> GeminiScanResult<()> {
        if GeminiFileObservation::from_metadata(&self.reader.get_ref().metadata()?)?
            != self.initial_observation
            || GeminiFileObservation::read(&self.source.path)? != self.initial_observation
        {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        Ok(())
    }

    fn frontier(&self) -> GeminiPageFrontier {
        GeminiPageFrontier {
            parser_revision: GEMINI_NATIVEPATH_PARSER_REVISION,
            policy_revision: GEMINI_NATIVEPATH_POLICY_REVISION,
            complete_prefix_end: self.complete_prefix_end,
            complete_prefix_sha256: prefix_digest(&self.prefix_hasher),
            source_device: self.initial_observation.device,
            source_inode: self.initial_observation.inode,
            next_raw_ordinal: self.raw_ordinal,
            retained_event_count: self
                .retained_event_count
                .saturating_add(self.state.retained_rows_this_scan),
            rejected_records: self.state.rejected_records,
            append_boundary_safe: self.append_boundary_safe,
            session: self.state.session.clone(),
        }
    }

    fn position(&self) -> GeminiReaderPosition {
        GeminiReaderPosition {
            prefix_hasher: self.prefix_hasher.clone(),
            source_hasher: self.source_hasher.clone(),
            offset: self.offset,
            raw_ordinal: self.raw_ordinal,
            complete_prefix_end: self.complete_prefix_end,
            append_boundary_safe: self.append_boundary_safe,
            terminal: self.terminal,
            retained_event_count: self.retained_event_count,
            metrics: self.state.metrics.clone(),
            rejected_records: self.state.rejected_records,
            rejection_details: self.state.rejections.len(),
            retained_rows_this_scan: self.state.retained_rows_this_scan,
            emitted_rows_this_scan: self.state.emitted_rows_this_scan,
            session_was_absent: self.state.session.is_none(),
        }
    }

    fn restore(&mut self, position: GeminiReaderPosition) -> GeminiScanResult<()> {
        self.reader.seek(SeekFrom::Start(position.offset))?;
        self.prefix_hasher = position.prefix_hasher;
        self.source_hasher = position.source_hasher;
        self.offset = position.offset;
        self.raw_ordinal = position.raw_ordinal;
        self.complete_prefix_end = position.complete_prefix_end;
        self.append_boundary_safe = position.append_boundary_safe;
        self.terminal = position.terminal;
        self.retained_event_count = position.retained_event_count;
        self.state.metrics = position.metrics;
        self.state.rejected_records = position.rejected_records;
        self.state.rejections.truncate(position.rejection_details);
        self.state.retained_rows_this_scan = position.retained_rows_this_scan;
        self.state.emitted_rows_this_scan = position.emitted_rows_this_scan;
        if position.session_was_absent {
            self.state.session = None;
        }
        // Once present, the session is immutable: later headers are rejected
        // without replacing it. Only the absent-to-present transition needs
        // explicit rollback here, avoiding a per-record session clone.
        Ok(())
    }

    fn scan_next_record(
        &mut self,
        page_native_event_ids: &GeminiNativeEventIds,
    ) -> GeminiScanResult<Option<ScannedGeminiRecord>> {
        let mut line = Vec::new();
        let prefix_before_record = self.prefix_hasher.clone();
        let Some(record) = read_record(
            &mut self.reader,
            &mut line,
            &mut self.prefix_hasher,
            &mut self.source_hasher,
        )?
        else {
            return Ok(None);
        };
        let byte_start = self.offset;
        self.offset = self.offset.saturating_add(record.bytes_observed);
        let byte_end_exclusive = self.offset;
        let payload = trim_jsonl_ending(&line);

        // Gemini appends records in place. No unterminated final physical
        // record is committed, even if its current bytes form valid JSON or
        // exceed the line limit.
        if !record.terminated {
            self.prefix_hasher = prefix_before_record;
            self.terminal = false;
            return Ok(Some(ScannedGeminiRecord {
                events: Vec::new(),
                transient_outputs: Vec::new(),
                transient_output_reservations: Vec::new(),
                rejections: Vec::new(),
                native_event_id: None,
                completed: false,
            }));
        }

        if record.oversized || payload.len() > MAX_PROVIDER_JSONL_LINE_BYTES {
            let rejection = self.state.reject(
                self.raw_ordinal,
                byte_start,
                byte_end_exclusive,
                format!(
                    "provider record exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit \
                     (observed {} bytes)",
                    record.bytes_observed
                ),
            );
            self.observe_native_record(record.bytes_observed);
            self.complete_record(record.terminated);
            return Ok(Some(ScannedGeminiRecord {
                events: Vec::new(),
                transient_outputs: Vec::new(),
                transient_output_reservations: Vec::new(),
                rejections: vec![rejection],
                native_event_id: None,
                completed: true,
            }));
        }
        if payload.iter().all(u8::is_ascii_whitespace) {
            self.complete_prefix_end = self.offset;
            self.append_boundary_safe = record.terminated;
            return Ok(Some(ScannedGeminiRecord {
                events: Vec::new(),
                transient_outputs: Vec::new(),
                transient_output_reservations: Vec::new(),
                rejections: Vec::new(),
                native_event_id: None,
                completed: true,
            }));
        }

        let probe = match serde_json::from_slice::<GeminiRecordProbe>(payload) {
            Ok(probe) => probe,
            Err(error) => {
                let rejection = self.state.reject(
                    self.raw_ordinal,
                    byte_start,
                    byte_end_exclusive,
                    format!("malformed Gemini JSONL: {error}"),
                );
                self.observe_native_record(record.bytes_observed);
                self.complete_record(record.terminated);
                return Ok(Some(ScannedGeminiRecord {
                    events: Vec::new(),
                    transient_outputs: Vec::new(),
                    transient_output_reservations: Vec::new(),
                    rejections: vec![rejection],
                    native_event_id: None,
                    completed: true,
                }));
            }
        };

        self.observe_native_record(record.bytes_observed);
        let class = probe.classify();
        let native_event_id = (class != GeminiRecordClass::Header)
            .then(|| nonempty(probe.id.clone()))
            .flatten();
        if let Some(native_event_id) = native_event_id.as_deref() {
            if let Err(error) = page_native_event_ids.validate(native_event_id, self.raw_ordinal) {
                let rejection = self.state.reject(
                    self.raw_ordinal,
                    byte_start,
                    byte_end_exclusive,
                    error.to_string(),
                );
                self.complete_record(record.terminated);
                return Ok(Some(ScannedGeminiRecord {
                    events: Vec::new(),
                    transient_outputs: Vec::new(),
                    transient_output_reservations: Vec::new(),
                    rejections: vec![rejection],
                    native_event_id: None,
                    completed: true,
                }));
            }
        }
        if class != GeminiRecordClass::Header && self.state.session.is_none() {
            let rejection = self.state.reject(
                self.raw_ordinal,
                byte_start,
                byte_end_exclusive,
                format!(
                    "{}: record appeared before an importable native JSONL session header",
                    self.source.path.display()
                ),
            );
            self.complete_record(record.terminated);
            return Ok(Some(ScannedGeminiRecord {
                events: Vec::new(),
                transient_outputs: Vec::new(),
                transient_output_reservations: Vec::new(),
                rejections: vec![rejection],
                native_event_id: None,
                completed: true,
            }));
        }
        let mut events = Vec::new();
        let mut transient_outputs = Vec::new();
        let mut transient_output_reservations = Vec::new();
        let mut rejections = Vec::new();
        match class {
            GeminiRecordClass::Header => {
                if self.state.session.is_some() {
                    return Err(GeminiScanError::UncommittedRecord {
                        raw_ordinal: self.raw_ordinal,
                        byte_start,
                        byte_end_exclusive,
                        reason: "a second Gemini session header appeared in one transcript"
                            .to_owned(),
                    });
                } else {
                    let session =
                        hydrate_header(payload, &self.state.source.layout).map_err(|reason| {
                            GeminiScanError::UncommittedRecord {
                                raw_ordinal: self.raw_ordinal,
                                byte_start,
                                byte_end_exclusive,
                                reason,
                            }
                        })?;
                    self.state.session = Some(session);
                    self.state.metrics.header_records =
                        self.state.metrics.header_records.saturating_add(1);
                }
            }
            GeminiRecordClass::Result => {
                self.state.metrics.native_result_records_observed = self
                    .state
                    .metrics
                    .native_result_records_observed
                    .saturating_add(1);
                self.state.metrics.native_result_record_bytes_observed = self
                    .state
                    .metrics
                    .native_result_record_bytes_observed
                    .saturating_add(record.bytes_observed);
                let Some(session) = self.state.session.as_ref() else {
                    return Err(GeminiScanError::UncommittedRecord {
                        raw_ordinal: self.raw_ordinal,
                        byte_start,
                        byte_end_exclusive,
                        reason: "Gemini result appeared before an importable session header"
                            .to_owned(),
                    });
                };
                let mut hydrated = hydrate_result_record(
                    payload,
                    self.profile,
                    self.source,
                    session,
                    self.raw_ordinal,
                    byte_start,
                    byte_end_exclusive,
                )
                .map_err(|reason| GeminiScanError::UncommittedRecord {
                    raw_ordinal: self.raw_ordinal,
                    byte_start,
                    byte_end_exclusive,
                    reason,
                })?;
                let output_frontier = self.frontier();
                let mut oversized_subrecords = BTreeSet::new();
                for (sub_ordinal, reserved_bytes) in &hydrated.output_reservations {
                    let page_bytes = output_page_conservative_bytes(
                        &output_frontier,
                        &output_frontier,
                        *reserved_bytes,
                    )
                    .ok_or(GeminiScanError::Capture(
                        CaptureError::SystemInvariant(
                            "Gemini output admission accounting overflowed",
                        ),
                    ))?;
                    if page_bytes > MAX_GEMINI_NATIVE_PAGE_BYTES {
                        oversized_subrecords.insert(*sub_ordinal);
                        rejections.push(self.state.reject(
                            self.raw_ordinal,
                            byte_start,
                            byte_end_exclusive,
                            format!(
                                "Gemini output subrecord {sub_ordinal} conservatively expands to \
                                 {page_bytes} serialized page bytes; page maximum is \
                                 {MAX_GEMINI_NATIVE_PAGE_BYTES}"
                            ),
                        ));
                    }
                }
                if !oversized_subrecords.is_empty() {
                    hydrated.events.retain(|(event, _)| {
                        !oversized_subrecords.contains(&event.native_order.sub_ordinal)
                    });
                    hydrated.outputs.retain(|(output, _)| {
                        output
                            .coordinate
                            .source_record_subrecord_index
                            .is_none_or(|sub_ordinal| !oversized_subrecords.contains(&sub_ordinal))
                    });
                    hydrated
                        .output_reservations
                        .retain(|(sub_ordinal, _)| !oversized_subrecords.contains(sub_ordinal));
                    hydrated.failure_diagnostics = hydrated.events.len();
                    hydrated.failure_previews = hydrated
                        .events
                        .iter()
                        .filter(|(event, _)| {
                            matches!(
                                &event.body,
                                GeminiEventBody::OutputDiagnostic {
                                    output_preview: Some(_),
                                    ..
                                }
                            )
                        })
                        .count();
                }
                self.state.metrics.result_body_bytes_decoded_or_allocated = self
                    .state
                    .metrics
                    .result_body_bytes_decoded_or_allocated
                    .saturating_add(hydrated.decoded_body_bytes);
                self.state.metrics.result_body_hashes_created = self
                    .state
                    .metrics
                    .result_body_hashes_created
                    .saturating_add(hydrated.failure_diagnostics as u64);
                self.state.metrics.result_previews_created = self
                    .state
                    .metrics
                    .result_previews_created
                    .saturating_add(hydrated.failure_previews as u64);
                self.state.metrics.result_handoffs_created = self
                    .state
                    .metrics
                    .result_handoffs_created
                    .saturating_add(hydrated.outputs.len() as u64);
                for (event, event_bytes) in &hydrated.events {
                    self.state.count_retained(event);
                    if *event_bytes > MAX_GEMINI_NATIVE_PAGE_BYTES {
                        return Err(GeminiScanError::UncommittedRecord {
                            raw_ordinal: self.raw_ordinal,
                            byte_start,
                            byte_end_exclusive,
                            reason: format!(
                                "Gemini retained output diagnostic exceeds the \
                                 {MAX_GEMINI_NATIVE_PAGE_BYTES} byte page limit"
                            ),
                        });
                    }
                }
                events = hydrated.events;
                transient_outputs = hydrated.outputs;
                transient_output_reservations = hydrated
                    .output_reservations
                    .into_iter()
                    .map(|(_, reserved_bytes)| reserved_bytes)
                    .collect();
            }
            GeminiRecordClass::Message
            | GeminiRecordClass::ToolCall
            | GeminiRecordClass::StateNotice
            | GeminiRecordClass::RewindNotice => {
                if self.state.session.is_none() {
                    return Err(GeminiScanError::UncommittedRecord {
                        raw_ordinal: self.raw_ordinal,
                        byte_start,
                        byte_end_exclusive,
                        reason: "record appeared before an importable Gemini session header"
                            .to_owned(),
                    });
                } else {
                    match hydrate_retained_event(payload, class, self.raw_ordinal) {
                        Ok(Some(mut hydrated)) => {
                            if hydrated.event.occurred_at.is_none() {
                                hydrated.event.occurred_at = self
                                    .state
                                    .session
                                    .as_ref()
                                    .and_then(|session| session.started_at);
                            }
                            match retained_event_bytes(&hydrated) {
                                Err(reason) => {
                                    return Err(GeminiScanError::UncommittedRecord {
                                        raw_ordinal: self.raw_ordinal,
                                        byte_start,
                                        byte_end_exclusive,
                                        reason,
                                    });
                                }
                                Ok(event_bytes) if event_bytes > MAX_GEMINI_NATIVE_PAGE_BYTES => {
                                    return Err(GeminiScanError::UncommittedRecord {
                                        raw_ordinal: self.raw_ordinal,
                                        byte_start,
                                        byte_end_exclusive,
                                        reason: format!(
                                            "Gemini retained event exceeds the {MAX_GEMINI_NATIVE_PAGE_BYTES} byte page limit"
                                        ),
                                    });
                                }
                                Ok(event_bytes) => {
                                    self.state.count_retained(&hydrated.event);
                                    events.push((hydrated.event, event_bytes));
                                }
                            }
                        }
                        Ok(None) => {
                            self.state.metrics.ignored_records =
                                self.state.metrics.ignored_records.saturating_add(1);
                        }
                        Err(GeminiHydrationError::Invalid(reason)) => {
                            return Err(GeminiScanError::UncommittedRecord {
                                raw_ordinal: self.raw_ordinal,
                                byte_start,
                                byte_end_exclusive,
                                reason,
                            });
                        }
                        Err(GeminiHydrationError::TouchOverflow(error)) => {
                            return Err(GeminiScanError::UncommittedRecord {
                                raw_ordinal: self.raw_ordinal,
                                byte_start,
                                byte_end_exclusive,
                                reason: error.to_string(),
                            });
                        }
                    }
                }
            }
            GeminiRecordClass::Ignored => {
                self.state.metrics.ignored_records =
                    self.state.metrics.ignored_records.saturating_add(1);
            }
        }
        self.complete_record(record.terminated);
        Ok(Some(ScannedGeminiRecord {
            events,
            transient_outputs,
            transient_output_reservations,
            rejections,
            native_event_id,
            completed: true,
        }))
    }

    fn observe_native_record(&mut self, bytes_observed: u64) {
        self.state.metrics.native_records_observed =
            self.state.metrics.native_records_observed.saturating_add(1);
        self.state.metrics.native_record_bytes_observed = self
            .state
            .metrics
            .native_record_bytes_observed
            .saturating_add(bytes_observed);
    }

    fn complete_record(&mut self, terminated: bool) {
        self.raw_ordinal = self.raw_ordinal.saturating_add(1);
        self.complete_prefix_end = self.offset;
        self.append_boundary_safe = terminated;
    }

    fn finish(&mut self) -> GeminiScanResult<()> {
        self.certify_source_range()?;
        let final_observation = GeminiFileObservation::read(&self.source.path)?;
        self.retained_event_count = self
            .retained_event_count
            .saturating_add(self.state.retained_rows_this_scan);
        let checkpoint = GeminiCheckpoint {
            parser_revision: GEMINI_NATIVEPATH_PARSER_REVISION,
            policy_revision: GEMINI_NATIVEPATH_POLICY_REVISION,
            source_path: self.source.path.clone(),
            source_observation: final_observation.clone(),
            session: self.state.session.clone(),
            complete_prefix_end: self.complete_prefix_end,
            complete_prefix_sha256: prefix_digest(&self.prefix_hasher),
            source_sha256: prefix_digest(&self.source_hasher),
            next_raw_ordinal: self.raw_ordinal,
            retained_event_count: self.retained_event_count,
            rejected_records: self.state.rejected_records,
            append_boundary_safe: self.append_boundary_safe,
            terminal: self.terminal,
        };
        let cross_path_change = classify_cross_path_source(&checkpoint, self.previous);
        let signals = lifecycle_signals(
            &checkpoint,
            self.previous,
            self.resumed_prefix,
            self.state.emitted_rows_this_scan,
            cross_path_change,
        );
        self.outcome = Some(GeminiScanOutcome {
            checkpoint,
            signals,
            metrics: self.state.metrics.clone(),
            rejected_records: self.state.rejected_records,
            rejections: self.state.rejections.clone(),
            terminal_source_observation: final_observation,
        });
        Ok(())
    }
}

impl ScanState<'_> {
    fn reject(
        &mut self,
        raw_ordinal: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
        reason: String,
    ) -> GeminiRejection {
        let rejection = GeminiRejection {
            raw_ordinal,
            byte_start,
            byte_end_exclusive,
            kind: GeminiRejectionKind::InvalidRecord,
            reason,
        };
        self.rejected_records = self.rejected_records.saturating_add(1);
        if self.rejections.len() < MAX_REJECTION_DETAILS {
            self.rejections.push(rejection.clone());
        }
        rejection
    }

    fn count_retained(&mut self, event: &GeminiRetainedEvent) {
        match event.event_type {
            EventType::Message => {
                self.metrics.retained_messages = self.metrics.retained_messages.saturating_add(1);
            }
            EventType::ToolCall => {
                self.metrics.retained_tool_calls =
                    self.metrics.retained_tool_calls.saturating_add(1);
            }
            EventType::Notice | EventType::Summary => {
                self.metrics.retained_notices = self.metrics.retained_notices.saturating_add(1);
            }
            _ => {}
        }
        self.metrics.retained_rows = self.metrics.retained_rows.saturating_add(1);
        self.retained_rows_this_scan = self.retained_rows_this_scan.saturating_add(1);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GeminiRecordClass {
    Header,
    Message,
    ToolCall,
    Result,
    StateNotice,
    RewindNotice,
    Ignored,
}

#[derive(Debug, Default)]
struct Presence(bool);

impl<'de> Deserialize<'de> for Presence {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        IgnoredAny::deserialize(deserializer)?;
        Ok(Self(true))
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiRecordProbe {
    id: Option<String>,
    session_id: Option<String>,
    #[serde(rename = "type")]
    record_type: Option<String>,
    #[serde(default)]
    tool_calls: Option<GeminiToolCallSummary>,
    #[serde(rename = "$set", default)]
    set: Presence,
    #[serde(rename = "$rewindTo", default)]
    rewind_to: Presence,
    #[serde(default)]
    result: Presence,
}

#[derive(Debug, Default)]
struct GeminiToolCallProbe {
    result: Presence,
}

impl<'de> Deserialize<'de> for GeminiToolCallProbe {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ToolCallProbeVisitor;

        impl<'de> Visitor<'de> for ToolCallProbeVisitor {
            type Value = GeminiToolCallProbe;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("one tolerant Gemini tool call")
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut probe = GeminiToolCallProbe::default();
                while let Some(key) = map.next_key::<String>()? {
                    if key == "result" {
                        probe.result = map.next_value()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(probe)
            }

            fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(GeminiToolCallProbe::default())
            }

            fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(GeminiToolCallProbe::default())
            }

            fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
                Ok(GeminiToolCallProbe::default())
            }

            fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
                Ok(GeminiToolCallProbe::default())
            }

            fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
                Ok(GeminiToolCallProbe::default())
            }

            fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
                Ok(GeminiToolCallProbe::default())
            }

            fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
                Ok(GeminiToolCallProbe::default())
            }

            fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                while sequence.next_element::<IgnoredAny>()?.is_some() {}
                Ok(GeminiToolCallProbe::default())
            }
        }

        deserializer.deserialize_any(ToolCallProbeVisitor)
    }
}

#[derive(Debug, Default)]
struct GeminiToolCallSummary {
    has_calls: bool,
    has_result: bool,
}

impl<'de> Deserialize<'de> for GeminiToolCallSummary {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SummaryVisitor;

        impl<'de> Visitor<'de> for SummaryVisitor {
            type Value = GeminiToolCallSummary;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a Gemini toolCalls array")
            }

            fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut summary = GeminiToolCallSummary::default();
                while let Some(call) = sequence.next_element::<GeminiToolCallProbe>()? {
                    summary.has_calls = true;
                    summary.has_result |= call.result.0;
                }
                Ok(summary)
            }
        }

        deserializer.deserialize_seq(SummaryVisitor)
    }
}

impl GeminiRecordProbe {
    fn classify(&self) -> GeminiRecordClass {
        if self
            .session_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            return GeminiRecordClass::Header;
        }
        let has_calls = self
            .tool_calls
            .as_ref()
            .is_some_and(|calls| calls.has_calls);
        let has_result = self.result.0
            || self
                .tool_calls
                .as_ref()
                .is_some_and(|calls| calls.has_result);
        if has_result {
            GeminiRecordClass::Result
        } else if has_calls {
            GeminiRecordClass::ToolCall
        } else if self.set.0 {
            GeminiRecordClass::StateNotice
        } else if self.rewind_to.0 {
            GeminiRecordClass::RewindNotice
        } else if matches!(self.record_type.as_deref(), Some("user" | "gemini")) {
            GeminiRecordClass::Message
        } else {
            GeminiRecordClass::Ignored
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiHeaderDto {
    session_id: String,
    start_time: Option<String>,
    kind: Option<String>,
    #[serde(default)]
    directories: Vec<String>,
}

fn hydrate_header(
    payload: &[u8],
    layout: &GeminiTranscriptLayout,
) -> std::result::Result<GeminiSession, String> {
    let header: GeminiHeaderDto = serde_json::from_slice(payload)
        .map_err(|error| format!("invalid Gemini header: {error}"))?;
    let native_session_id = header.session_id.trim();
    if native_session_id.is_empty() {
        return Err("Gemini header has an empty sessionId".to_owned());
    }
    let (parent_native_session_id, path_agent_type) = match layout {
        GeminiTranscriptLayout::Primary => (None, AgentType::Primary),
        GeminiTranscriptLayout::Subagent {
            parent_native_session_id_hint,
        } => (
            Some(parent_native_session_id_hint.clone()),
            AgentType::Subagent,
        ),
    };
    let agent_type =
        if parent_native_session_id.is_some() || header.kind.as_deref() == Some("subagent") {
            AgentType::Subagent
        } else {
            path_agent_type
        };
    Ok(GeminiSession {
        native_session_id: native_session_id.to_owned(),
        parent_native_session_id,
        agent_type,
        started_at: header.start_time.as_deref().and_then(parse_timestamp),
        cwd: header
            .directories
            .into_iter()
            .find(|directory| !directory.trim().is_empty()),
        native_kind: header.kind,
    })
}

#[derive(Debug, Deserialize)]
struct GeminiMessageDto {
    id: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "type")]
    record_type: Option<String>,
    content: Option<String>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiToolCallRecordDto {
    id: Option<String>,
    timestamp: Option<String>,
    #[serde(default)]
    tool_calls: Vec<GeminiToolCallDto>,
}

#[derive(Debug, Deserialize)]
struct GeminiToolCallDto {
    id: Option<String>,
    name: Option<String>,
    args: Option<Value>,
    #[serde(default)]
    result: Presence,
}

#[derive(Debug, Default, Clone)]
struct GeminiOutputOutcomeDto {
    error: FailureMarker,
    success: BoolMarker,
    ok: BoolMarker,
    status: StatusMarker,
    state: StatusMarker,
    outcome: StatusMarker,
    is_error: BoolMarker,
    timed_out: BoolMarker,
    timeout: BoolMarker,
    exit_code: I64Marker,
    status_code: I64Marker,
    duration_ms: U64Marker,
    redacted: RedactionMarker,
    is_redacted: RedactionMarker,
}

impl GeminiOutputOutcomeDto {
    fn merge_nested(&mut self, other: Self) {
        self.error.0 |= other.error.0;
        self.success.0 = self.success.0.or(other.success.0);
        self.ok.0 = self.ok.0.or(other.ok.0);
        self.status.merge_nested(other.status);
        self.state.merge_nested(other.state);
        self.outcome.merge_nested(other.outcome);
        self.is_error.0 = self.is_error.0.or(other.is_error.0);
        self.timed_out.0 = self.timed_out.0.or(other.timed_out.0);
        self.timeout.0 = self.timeout.0.or(other.timeout.0);
        self.exit_code.0 = self.exit_code.0.or(other.exit_code.0);
        self.status_code.0 = self.status_code.0.or(other.status_code.0);
        self.duration_ms.0 = self.duration_ms.0.or(other.duration_ms.0);
    }

    fn combined_metadata(&self, inner: &Self) -> OutputOutcomeMetadata {
        let timeout = self.timed_out.0 == Some(true)
            || self.timeout.0 == Some(true)
            || inner.timed_out.0 == Some(true)
            || inner.timeout.0 == Some(true);
        let failure = self.error.0
            || self.success.0 == Some(false)
            || self.is_error.0 == Some(true)
            || self.exit_code.0.is_some_and(|code| code != 0)
            || self.status_code.0.is_some_and(|code| code >= 400)
            || self.status.failure
            || self.state.failure
            || self.outcome.failure
            || inner.error.0
            || inner.success.0 == Some(false)
            || inner.is_error.0 == Some(true)
            || inner.exit_code.0.is_some_and(|code| code != 0)
            || inner.status_code.0.is_some_and(|code| code >= 400)
            || inner.status.failure
            || inner.state.failure
            || inner.outcome.failure;
        let success = self.success.0 == Some(true)
            || self.ok.0 == Some(true)
            || self.is_error.0 == Some(false)
            || self.timed_out.0 == Some(false)
            || self.timeout.0 == Some(false)
            || self.exit_code.0 == Some(0)
            || self
                .status_code
                .0
                .is_some_and(|code| (200..400).contains(&code))
            || self.status.success
            || self.state.success
            || self.outcome.success
            || inner.success.0 == Some(true)
            || inner.ok.0 == Some(true)
            || inner.is_error.0 == Some(false)
            || inner.timed_out.0 == Some(false)
            || inner.timeout.0 == Some(false)
            || inner.exit_code.0 == Some(0)
            || inner
                .status_code
                .0
                .is_some_and(|code| (200..400).contains(&code))
            || inner.status.success
            || inner.state.success
            || inner.outcome.success;
        OutputOutcomeMetadata {
            outcome: if timeout {
                OutputOutcome::Timeout
            } else if failure {
                OutputOutcome::Failure
            } else if success {
                OutputOutcome::Success
            } else {
                OutputOutcome::Unknown
            },
            exit_code: inner
                .exit_code
                .0
                .or(self.exit_code.0)
                .and_then(|code| i32::try_from(code).ok()),
            duration_ms: inner.duration_ms.0.or(self.duration_ms.0),
        }
    }

    fn redacted_with(&self, inner: &Self) -> bool {
        self.is_redacted() || inner.is_redacted()
    }

    fn is_redacted(&self) -> bool {
        self.redacted.0 || self.is_redacted.0 || self.status.redacted || self.state.redacted
    }
}

#[derive(Debug, Default, Clone)]
struct FailureMarker(bool);

#[derive(Debug, Default, Clone, Copy)]
struct BoolMarker(Option<bool>);

#[derive(Debug, Default, Clone, Copy)]
struct RedactionMarker(bool);

#[derive(Debug, Default, Clone, Copy)]
struct I64Marker(Option<i64>);

#[derive(Debug, Default, Clone, Copy)]
struct U64Marker(Option<u64>);

#[derive(Debug, Default, Clone, Copy)]
struct StatusMarker {
    success: bool,
    failure: bool,
    redacted: bool,
}

impl StatusMarker {
    fn merge_nested(&mut self, other: Self) {
        self.success |= other.success;
        self.failure |= other.failure;
    }
}

#[derive(Debug, Default, Clone)]
struct GeminiBoundedContent {
    preview: Option<String>,
    decoded_bytes: usize,
}

#[derive(Default)]
enum GeminiSelectedContent {
    #[default]
    Absent,
    String(GeminiBoundedContent),
    Null,
    Unsupported,
}

struct ProbedGeminiOutput {
    call_id: Option<String>,
    tool_name: Option<String>,
    outcome: OutputOutcomeMetadata,
    redacted: bool,
    diagnostic_preview: Option<String>,
    content: Option<String>,
    content_bytes: usize,
    has_output_content: bool,
}

struct ProbedGeminiResult {
    native_record_id: Option<String>,
    occurred_at_unix_ms: Option<i64>,
    outputs: Vec<ProbedGeminiOutput>,
}

struct HydratedGeminiResult {
    events: Vec<(GeminiRetainedEvent, usize)>,
    outputs: Vec<(ProOutputObservation, usize)>,
    output_reservations: Vec<(u32, usize)>,
    decoded_body_bytes: u64,
    failure_diagnostics: usize,
    failure_previews: usize,
}

const MAX_GEMINI_STRUCTURAL_DEPTH: usize = 128;
const MAX_GEMINI_STRUCTURAL_KEY_CHARS: usize = 64;

struct GeminiRawJson<'a> {
    bytes: &'a [u8],
    offset: usize,
    capture_full_content: bool,
}

struct GeminiRawString {
    retained: String,
    decoded_bytes: usize,
    truncated: bool,
    non_whitespace: bool,
}

impl GeminiRawString {
    fn bounded_content(self) -> GeminiBoundedContent {
        GeminiBoundedContent {
            preview: Some(self.retained),
            decoded_bytes: self.decoded_bytes,
        }
    }

    fn exact(self) -> Option<String> {
        (!self.truncated).then_some(self.retained)
    }
}

impl<'a> GeminiRawJson<'a> {
    fn new(bytes: &'a [u8], capture_full_content: bool) -> Self {
        Self {
            bytes,
            offset: 0,
            capture_full_content,
        }
    }

    fn finish(mut self) -> std::result::Result<(), String> {
        self.whitespace();
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err("Gemini result record has trailing JSON data".to_owned())
        }
    }

    fn whitespace(&mut self) {
        while self
            .bytes
            .get(self.offset)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.offset = self.offset.saturating_add(1);
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.offset).copied()
    }

    fn take(&mut self, expected: u8) -> std::result::Result<(), String> {
        if self.peek() != Some(expected) {
            return Err(format!(
                "invalid Gemini result JSON near byte {}",
                self.offset
            ));
        }
        self.offset = self.offset.saturating_add(1);
        Ok(())
    }

    fn consume_literal(&mut self, literal: &[u8]) -> std::result::Result<(), String> {
        if self
            .bytes
            .get(self.offset..self.offset.saturating_add(literal.len()))
            != Some(literal)
        {
            return Err(format!(
                "invalid Gemini result JSON literal near byte {}",
                self.offset
            ));
        }
        self.offset = self.offset.saturating_add(literal.len());
        Ok(())
    }

    fn string(&mut self, retain_chars: usize) -> std::result::Result<GeminiRawString, String> {
        self.take(b'"')?;
        let mut retained = String::new();
        let mut retained_chars = 0_usize;
        let mut decoded_chars = 0_usize;
        let mut decoded_bytes = 0_usize;
        let mut non_whitespace = false;
        loop {
            let byte = self
                .peek()
                .ok_or_else(|| "unterminated string in Gemini result JSON".to_owned())?;
            match byte {
                b'"' => {
                    self.offset = self.offset.saturating_add(1);
                    return Ok(GeminiRawString {
                        retained,
                        decoded_bytes,
                        truncated: retained_chars < decoded_chars,
                        non_whitespace,
                    });
                }
                b'\\' => {
                    self.offset = self.offset.saturating_add(1);
                    let escaped = self
                        .peek()
                        .ok_or_else(|| "unterminated escape in Gemini result JSON".to_owned())?;
                    self.offset = self.offset.saturating_add(1);
                    let character = match escaped {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{0008}',
                        b'f' => '\u{000c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => self.unicode_escape()?,
                        _ => {
                            return Err(format!(
                                "invalid escape in Gemini result JSON near byte {}",
                                self.offset.saturating_sub(1)
                            ));
                        }
                    };
                    decoded_bytes = decoded_bytes
                        .checked_add(character.len_utf8())
                        .ok_or_else(|| "Gemini result string length overflowed".to_owned())?;
                    decoded_chars = decoded_chars.saturating_add(1);
                    non_whitespace |= !character.is_whitespace();
                    if retained_chars < retain_chars {
                        retained.push(character);
                        retained_chars = retained_chars.saturating_add(1);
                    }
                }
                0x00..=0x1f => {
                    return Err(format!(
                        "control byte in Gemini result JSON string near byte {}",
                        self.offset
                    ));
                }
                byte if byte.is_ascii() => {
                    let start = self.offset;
                    while self.peek().is_some_and(|byte| {
                        byte.is_ascii() && !matches!(byte, b'"' | b'\\' | 0x00..=0x1f)
                    }) {
                        self.offset = self.offset.saturating_add(1);
                    }
                    let run = &self.bytes[start..self.offset];
                    decoded_bytes = decoded_bytes
                        .checked_add(run.len())
                        .ok_or_else(|| "Gemini result string length overflowed".to_owned())?;
                    decoded_chars = decoded_chars.saturating_add(run.len());
                    non_whitespace |= run.iter().any(|byte| !byte.is_ascii_whitespace());
                    let retained_bytes = retain_chars.saturating_sub(retained_chars).min(run.len());
                    if retained_bytes != 0 {
                        retained
                            .push_str(std::str::from_utf8(&run[..retained_bytes]).map_err(
                                |_| "Gemini result JSON string is not UTF-8".to_owned(),
                            )?);
                        retained_chars = retained_chars.saturating_add(retained_bytes);
                    }
                }
                _ => {
                    let width = match byte {
                        0xc2..=0xdf => 2,
                        0xe0..=0xef => 3,
                        0xf0..=0xf4 => 4,
                        _ => {
                            return Err("Gemini result JSON string is not UTF-8".to_owned());
                        }
                    };
                    let end = self
                        .offset
                        .checked_add(width)
                        .ok_or_else(|| "Gemini result string offset overflowed".to_owned())?;
                    let encoded = self
                        .bytes
                        .get(self.offset..end)
                        .ok_or_else(|| "unterminated UTF-8 in Gemini result JSON".to_owned())?;
                    let character = std::str::from_utf8(encoded)
                        .ok()
                        .and_then(|value| value.chars().next())
                        .ok_or_else(|| "Gemini result JSON string is not UTF-8".to_owned())?;
                    self.offset = end;
                    decoded_bytes = decoded_bytes
                        .checked_add(character.len_utf8())
                        .ok_or_else(|| "Gemini result string length overflowed".to_owned())?;
                    decoded_chars = decoded_chars.saturating_add(1);
                    non_whitespace |= !character.is_whitespace();
                    if retained_chars < retain_chars {
                        retained.push(character);
                        retained_chars = retained_chars.saturating_add(1);
                    }
                }
            }
        }
    }

    fn unicode_escape(&mut self) -> std::result::Result<char, String> {
        let first = self.hex_quad()?;
        let code = if (0xd800..=0xdbff).contains(&first) {
            self.take(b'\\')?;
            self.take(b'u')?;
            let second = self.hex_quad()?;
            if !(0xdc00..=0xdfff).contains(&second) {
                return Err("invalid Unicode surrogate pair in Gemini result JSON".to_owned());
            }
            0x1_0000 + (u32::from(first - 0xd800) << 10) + u32::from(second - 0xdc00)
        } else if (0xdc00..=0xdfff).contains(&first) {
            return Err("invalid Unicode surrogate pair in Gemini result JSON".to_owned());
        } else {
            u32::from(first)
        };
        char::from_u32(code)
            .ok_or_else(|| "invalid Unicode scalar in Gemini result JSON".to_owned())
    }

    fn hex_quad(&mut self) -> std::result::Result<u16, String> {
        let mut value = 0_u16;
        for _ in 0..4 {
            let digit = self.peek().and_then(|byte| (byte as char).to_digit(16));
            let Some(digit) = digit else {
                return Err(format!(
                    "invalid Unicode escape in Gemini result JSON near byte {}",
                    self.offset
                ));
            };
            self.offset = self.offset.saturating_add(1);
            value = (value << 4) | u16::try_from(digit).unwrap_or_default();
        }
        Ok(value)
    }

    fn key(&mut self) -> std::result::Result<Option<String>, String> {
        let key = self.string(MAX_GEMINI_STRUCTURAL_KEY_CHARS)?;
        Ok(key.exact())
    }

    fn optional_string(&mut self) -> std::result::Result<Option<String>, String> {
        self.whitespace();
        if self.peek() == Some(b'"') {
            return self
                .string(usize::MAX)?
                .exact()
                .ok_or_else(|| {
                    "Gemini result metadata string exceeded addressable memory".to_owned()
                })
                .map(Some);
        }
        self.skip_value(0)?;
        Ok(None)
    }

    fn number(&mut self) -> std::result::Result<&'a str, String> {
        let start = self.offset;
        while self.peek().is_some_and(|byte| {
            byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E')
        }) {
            self.offset = self.offset.saturating_add(1);
        }
        let value = std::str::from_utf8(&self.bytes[start..self.offset])
            .map_err(|_| "Gemini result number is not UTF-8".to_owned())?;
        if value.is_empty() {
            Err(format!(
                "invalid Gemini result number near byte {}",
                self.offset
            ))
        } else {
            Ok(value)
        }
    }

    fn skip_value(&mut self, depth: usize) -> std::result::Result<(), String> {
        if depth > MAX_GEMINI_STRUCTURAL_DEPTH {
            return Err(format!(
                "Gemini result JSON exceeds structural depth {MAX_GEMINI_STRUCTURAL_DEPTH}"
            ));
        }
        self.whitespace();
        match self.peek() {
            Some(b'"') => {
                self.string(0)?;
            }
            Some(b'{') => {
                self.take(b'{')?;
                self.whitespace();
                if self.peek() == Some(b'}') {
                    self.take(b'}')?;
                    return Ok(());
                }
                loop {
                    self.string(0)?;
                    self.whitespace();
                    self.take(b':')?;
                    self.skip_value(depth.saturating_add(1))?;
                    self.whitespace();
                    match self.peek() {
                        Some(b',') => {
                            self.take(b',')?;
                            self.whitespace();
                        }
                        Some(b'}') => {
                            self.take(b'}')?;
                            break;
                        }
                        _ => {
                            return Err(format!(
                                "invalid Gemini result object near byte {}",
                                self.offset
                            ));
                        }
                    }
                }
            }
            Some(b'[') => {
                self.take(b'[')?;
                self.whitespace();
                if self.peek() == Some(b']') {
                    self.take(b']')?;
                    return Ok(());
                }
                loop {
                    self.skip_value(depth.saturating_add(1))?;
                    self.whitespace();
                    match self.peek() {
                        Some(b',') => {
                            self.take(b',')?;
                            self.whitespace();
                        }
                        Some(b']') => {
                            self.take(b']')?;
                            break;
                        }
                        _ => {
                            return Err(format!(
                                "invalid Gemini result array near byte {}",
                                self.offset
                            ));
                        }
                    }
                }
            }
            Some(b't') => self.consume_literal(b"true")?,
            Some(b'f') => self.consume_literal(b"false")?,
            Some(b'n') => self.consume_literal(b"null")?,
            Some(_) => {
                self.number()?;
            }
            None => return Err("missing value in Gemini result JSON".to_owned()),
        }
        Ok(())
    }
}

#[derive(Default)]
struct GeminiRawOutput {
    outcome: GeminiOutputOutcomeDto,
    content: GeminiSelectedContent,
}

impl GeminiRawJson<'_> {
    fn output_value(&mut self, depth: usize) -> std::result::Result<GeminiRawOutput, String> {
        if depth > MAX_GEMINI_STRUCTURAL_DEPTH {
            return Err(format!(
                "Gemini result JSON exceeds structural depth {MAX_GEMINI_STRUCTURAL_DEPTH}"
            ));
        }
        self.whitespace();
        match self.peek() {
            Some(b'"') => Ok(GeminiRawOutput {
                content: GeminiSelectedContent::String(
                    self.string(if self.capture_full_content {
                        usize::MAX
                    } else {
                        PROVIDER_MAX_PREVIEW_CHARS
                    })?
                    .bounded_content(),
                ),
                ..GeminiRawOutput::default()
            }),
            Some(b'{') => self.output_object(depth.saturating_add(1)),
            Some(b'n') => {
                self.consume_literal(b"null")?;
                Ok(GeminiRawOutput {
                    content: GeminiSelectedContent::Null,
                    ..GeminiRawOutput::default()
                })
            }
            Some(_) => {
                self.skip_value(depth)?;
                Ok(GeminiRawOutput {
                    content: GeminiSelectedContent::Unsupported,
                    ..GeminiRawOutput::default()
                })
            }
            None => Err("missing Gemini output value".to_owned()),
        }
    }

    fn output_object(&mut self, depth: usize) -> std::result::Result<GeminiRawOutput, String> {
        self.take(b'{')?;
        self.whitespace();
        let mut output = GeminiRawOutput::default();
        let mut content = None;
        let mut output_alias = None;
        let mut text = None;
        if self.peek() == Some(b'}') {
            self.take(b'}')?;
            return Ok(output);
        }
        loop {
            let key = self.key()?;
            self.whitespace();
            self.take(b':')?;
            self.whitespace();
            match key.as_deref() {
                Some("content") => {
                    content = Some(self.content_candidate(depth)?);
                }
                Some("output") => {
                    output_alias = Some(self.content_candidate(depth)?);
                }
                Some("text") => {
                    text = Some(self.content_candidate(depth)?);
                }
                Some(key) => {
                    if !self.outcome_field(key, &mut output.outcome, depth)? {
                        let nested = self.nested_outcome_value(depth.saturating_add(1))?;
                        output.outcome.merge_nested(nested.outcome);
                    }
                }
                None => {
                    let nested = self.nested_outcome_value(depth.saturating_add(1))?;
                    output.outcome.merge_nested(nested.outcome);
                }
            }
            self.whitespace();
            match self.peek() {
                Some(b',') => {
                    self.take(b',')?;
                    self.whitespace();
                }
                Some(b'}') => {
                    self.take(b'}')?;
                    break;
                }
                _ => {
                    return Err(format!(
                        "invalid Gemini result object near byte {}",
                        self.offset
                    ));
                }
            }
        }
        output.content = content
            .or(output_alias)
            .or(text)
            .unwrap_or(GeminiSelectedContent::Absent);
        Ok(output)
    }

    fn nested_outcome_value(
        &mut self,
        depth: usize,
    ) -> std::result::Result<GeminiRawOutput, String> {
        if depth > MAX_GEMINI_STRUCTURAL_DEPTH {
            return Err(format!(
                "Gemini result JSON exceeds structural depth {MAX_GEMINI_STRUCTURAL_DEPTH}"
            ));
        }
        self.whitespace();
        let mut output = GeminiRawOutput::default();
        match self.peek() {
            Some(b'{') => {
                self.take(b'{')?;
                self.whitespace();
                if self.peek() == Some(b'}') {
                    self.take(b'}')?;
                    return Ok(output);
                }
                loop {
                    let key = self.key()?;
                    self.whitespace();
                    self.take(b':')?;
                    self.whitespace();
                    match key.as_deref() {
                        Some(key) if self.outcome_field(key, &mut output.outcome, depth)? => {}
                        _ => {
                            let nested = self.nested_outcome_value(depth.saturating_add(1))?;
                            output.outcome.merge_nested(nested.outcome);
                        }
                    }
                    self.whitespace();
                    match self.peek() {
                        Some(b',') => {
                            self.take(b',')?;
                            self.whitespace();
                        }
                        Some(b'}') => {
                            self.take(b'}')?;
                            break;
                        }
                        _ => {
                            return Err(format!(
                                "invalid Gemini result object near byte {}",
                                self.offset
                            ));
                        }
                    }
                }
            }
            Some(b'[') => {
                self.take(b'[')?;
                self.whitespace();
                if self.peek() == Some(b']') {
                    self.take(b']')?;
                    return Ok(output);
                }
                loop {
                    let nested = self.nested_outcome_value(depth.saturating_add(1))?;
                    output.outcome.merge_nested(nested.outcome);
                    self.whitespace();
                    match self.peek() {
                        Some(b',') => {
                            self.take(b',')?;
                            self.whitespace();
                        }
                        Some(b']') => {
                            self.take(b']')?;
                            break;
                        }
                        _ => {
                            return Err(format!(
                                "invalid Gemini result array near byte {}",
                                self.offset
                            ));
                        }
                    }
                }
            }
            Some(_) => self.skip_value(depth)?,
            None => return Err("missing Gemini nested outcome value".to_owned()),
        }
        Ok(output)
    }

    fn content_candidate(
        &mut self,
        depth: usize,
    ) -> std::result::Result<GeminiSelectedContent, String> {
        self.whitespace();
        match self.peek() {
            Some(b'"') => self
                .string(if self.capture_full_content {
                    usize::MAX
                } else {
                    PROVIDER_MAX_PREVIEW_CHARS
                })
                .map(GeminiRawString::bounded_content)
                .map(GeminiSelectedContent::String),
            Some(b'n') => {
                self.consume_literal(b"null")?;
                Ok(GeminiSelectedContent::Null)
            }
            _ => {
                self.skip_value(depth)?;
                Ok(GeminiSelectedContent::Unsupported)
            }
        }
    }

    fn outcome_field(
        &mut self,
        key: &str,
        outcome: &mut GeminiOutputOutcomeDto,
        depth: usize,
    ) -> std::result::Result<bool, String> {
        match key {
            "error" => outcome.error = FailureMarker(self.failure_marker(depth)?),
            "success" => outcome.success = BoolMarker(self.bool_marker(depth)?),
            "ok" => outcome.ok = BoolMarker(self.bool_marker(depth)?),
            "status" => outcome.status = self.status_marker(depth)?,
            "state" => outcome.state = self.status_marker(depth)?,
            "outcome" => outcome.outcome = self.status_marker(depth)?,
            "isError" | "is_error" => {
                outcome.is_error = BoolMarker(self.bool_marker(depth)?);
            }
            "timedOut" | "timed_out" => {
                outcome.timed_out = BoolMarker(self.bool_marker(depth)?);
            }
            "timeout" => outcome.timeout = BoolMarker(self.bool_marker(depth)?),
            "exitCode" | "exit_code" => {
                outcome.exit_code = I64Marker(self.i64_marker(depth)?);
            }
            "statusCode" | "status_code" => {
                outcome.status_code = I64Marker(self.i64_marker(depth)?);
            }
            "durationMs" | "duration_ms" | "duration" => {
                outcome.duration_ms = U64Marker(self.u64_marker(depth)?);
            }
            "redacted" => {
                outcome.redacted = RedactionMarker(self.redaction_marker(depth)?);
            }
            "isRedacted" | "is_redacted" => {
                outcome.is_redacted = RedactionMarker(self.redaction_marker(depth)?);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn bool_marker(&mut self, depth: usize) -> std::result::Result<Option<bool>, String> {
        self.whitespace();
        match self.peek() {
            Some(b't') => {
                self.consume_literal(b"true")?;
                Ok(Some(true))
            }
            Some(b'f') => {
                self.consume_literal(b"false")?;
                Ok(Some(false))
            }
            _ => {
                self.skip_value(depth)?;
                Ok(None)
            }
        }
    }

    fn redaction_marker(&mut self, depth: usize) -> std::result::Result<bool, String> {
        self.whitespace();
        if self.peek() == Some(b'f') {
            self.consume_literal(b"false")?;
            Ok(false)
        } else {
            self.skip_value(depth)?;
            Ok(true)
        }
    }

    fn failure_marker(&mut self, depth: usize) -> std::result::Result<bool, String> {
        self.whitespace();
        match self.peek() {
            Some(b'n') => {
                self.consume_literal(b"null")?;
                Ok(false)
            }
            Some(b't') => {
                self.consume_literal(b"true")?;
                Ok(true)
            }
            Some(b'f') => {
                self.consume_literal(b"false")?;
                Ok(false)
            }
            Some(b'"') => {
                let value = self.string(64)?;
                Ok(value.non_whitespace)
            }
            Some(b'{') | Some(b'[') => {
                let start = self.offset;
                self.skip_value(depth)?;
                let end = self.offset;
                Ok(self.bytes[start.saturating_add(1)..end.saturating_sub(1)]
                    .iter()
                    .any(|byte| !byte.is_ascii_whitespace()))
            }
            Some(_) => {
                let number = self.number()?;
                Ok(number.parse::<i64>().is_ok_and(|value| value != 0))
            }
            None => Err("missing Gemini failure marker".to_owned()),
        }
    }

    fn status_marker(&mut self, depth: usize) -> std::result::Result<StatusMarker, String> {
        self.whitespace();
        if self.peek() != Some(b'"') {
            self.skip_value(depth)?;
            return Ok(StatusMarker::default());
        }
        let value = self.string(64)?;
        if value.truncated {
            return Ok(StatusMarker::default());
        }
        let redacted = matches!(value.retained.as_str(), "redacted" | "output-redacted");
        let status = value.retained.trim().to_ascii_lowercase();
        Ok(StatusMarker {
            success: matches!(
                status.as_str(),
                "success" | "succeeded" | "complete" | "completed" | "ok" | "passed"
            ),
            failure: matches!(
                status.as_str(),
                "failed"
                    | "failure"
                    | "error"
                    | "errored"
                    | "timeout"
                    | "timed_out"
                    | "timedout"
                    | "cancelled"
                    | "canceled"
            ),
            redacted,
        })
    }

    fn i64_marker(&mut self, depth: usize) -> std::result::Result<Option<i64>, String> {
        self.whitespace();
        if self
            .peek()
            .is_none_or(|byte| !(byte.is_ascii_digit() || byte == b'-'))
        {
            self.skip_value(depth)?;
            return Ok(None);
        }
        let number = self.number()?;
        Ok(number.parse::<i64>().ok().or_else(|| {
            number
                .parse::<u64>()
                .ok()
                .and_then(|value| value.try_into().ok())
        }))
    }

    fn u64_marker(&mut self, depth: usize) -> std::result::Result<Option<u64>, String> {
        self.whitespace();
        if self
            .peek()
            .is_none_or(|byte| !(byte.is_ascii_digit() || byte == b'-'))
        {
            self.skip_value(depth)?;
            return Ok(None);
        }
        let number = self.number()?;
        Ok(number.parse::<u64>().ok().or_else(|| {
            number
                .parse::<i64>()
                .ok()
                .and_then(|value| value.try_into().ok())
        }))
    }
}

struct GeminiRawResultCall {
    id: Option<String>,
    name: Option<String>,
    result: Option<GeminiRawOutput>,
    outcome: GeminiOutputOutcomeDto,
}

fn parse_result_record_selectively(
    payload: &[u8],
    capture_full_content: bool,
) -> std::result::Result<ProbedGeminiResult, String> {
    let mut parser = GeminiRawJson::new(payload, capture_full_content);
    parser.whitespace();
    parser.take(b'{')?;
    parser.whitespace();
    let mut id = None;
    let mut timestamp = None;
    let mut top_result = None;
    let mut calls = Vec::new();
    let mut outcome = GeminiOutputOutcomeDto::default();
    let mut saw_id = false;
    let mut saw_timestamp = false;
    let mut saw_result = false;
    let mut saw_tool_calls = false;

    if parser.peek() != Some(b'}') {
        loop {
            let key = parser.key()?;
            parser.whitespace();
            parser.take(b':')?;
            parser.whitespace();
            match key.as_deref() {
                Some("id") => {
                    if saw_id {
                        return Err("duplicate id field in Gemini result record".to_owned());
                    }
                    saw_id = true;
                    id = parser.strict_optional_string()?;
                }
                Some("timestamp") => {
                    if saw_timestamp {
                        return Err("duplicate timestamp field in Gemini result record".to_owned());
                    }
                    saw_timestamp = true;
                    timestamp = parser.strict_optional_string()?;
                }
                Some("result") => {
                    if saw_result {
                        return Err("duplicate result field in Gemini result record".to_owned());
                    }
                    saw_result = true;
                    top_result = Some(parser.output_value(1)?);
                }
                Some("toolCalls") => {
                    if saw_tool_calls {
                        return Err("duplicate toolCalls field in Gemini result record".to_owned());
                    }
                    saw_tool_calls = true;
                    calls = parser.result_calls(1)?;
                }
                Some(key) => {
                    if !parser.outcome_field(key, &mut outcome, 1)? {
                        parser.skip_value(1)?;
                    }
                }
                None => parser.skip_value(1)?,
            }
            parser.whitespace();
            match parser.peek() {
                Some(b',') => {
                    parser.take(b',')?;
                    parser.whitespace();
                }
                Some(b'}') => break,
                _ => {
                    return Err(format!(
                        "invalid Gemini result object near byte {}",
                        parser.offset
                    ));
                }
            }
        }
    }
    parser.take(b'}')?;
    parser.whitespace();
    parser.finish()?;

    let mut outputs = Vec::new();
    let mut invalid_selected_shape = false;
    let mut output_count = 0_usize;
    let record_redacted = outcome.is_redacted();
    if let Some(result) = top_result {
        output_count = output_count.saturating_add(1);
        if let Some(output) =
            finish_probed_output(None, None, false, &outcome, result, capture_full_content)
        {
            outputs.push(output);
        } else {
            invalid_selected_shape = true;
        }
    }
    for call in calls {
        let Some(result) = call.result else {
            continue;
        };
        if output_count >= MAX_GEMINI_NATIVE_PAGE_RECORDS {
            return Err(format!(
                "Gemini result record exceeds the {MAX_GEMINI_NATIVE_PAGE_RECORDS} output limit"
            ));
        }
        output_count = output_count.saturating_add(1);
        if let Some(output) = finish_probed_output(
            nonempty(call.id),
            nonempty(call.name),
            record_redacted,
            &call.outcome,
            result,
            capture_full_content,
        ) {
            outputs.push(output);
        } else {
            invalid_selected_shape = true;
        }
    }
    // The shared legacy extractor abstains from the complete result record
    // when any selected alias has an unsupported shape.
    if invalid_selected_shape {
        outputs.clear();
    }

    Ok(ProbedGeminiResult {
        native_record_id: nonempty(id),
        occurred_at_unix_ms: timestamp
            .as_deref()
            .and_then(parse_timestamp)
            .map(|timestamp| timestamp.timestamp_millis()),
        outputs,
    })
}

fn finish_probed_output(
    call_id: Option<String>,
    tool_name: Option<String>,
    record_redacted: bool,
    outer_outcome: &GeminiOutputOutcomeDto,
    result: GeminiRawOutput,
    capture_full_content: bool,
) -> Option<ProbedGeminiOutput> {
    let (retained, content_bytes, has_output_content) = match result.content {
        GeminiSelectedContent::String(content) => (content.preview, content.decoded_bytes, true),
        GeminiSelectedContent::Absent | GeminiSelectedContent::Null => (None, 0, false),
        GeminiSelectedContent::Unsupported => return None,
    };
    let diagnostic_preview = retained.as_deref().map(|content| {
        content
            .chars()
            .take(PROVIDER_MAX_PREVIEW_CHARS)
            .collect::<String>()
    });
    Some(ProbedGeminiOutput {
        call_id,
        tool_name,
        outcome: outer_outcome.combined_metadata(&result.outcome),
        redacted: record_redacted || outer_outcome.redacted_with(&result.outcome),
        diagnostic_preview,
        content: capture_full_content.then_some(retained).flatten(),
        content_bytes,
        has_output_content,
    })
}

impl GeminiRawJson<'_> {
    fn strict_optional_string(&mut self) -> std::result::Result<Option<String>, String> {
        self.whitespace();
        match self.peek() {
            Some(b'"') => self
                .string(usize::MAX)?
                .exact()
                .ok_or_else(|| "Gemini result metadata string overflowed".to_owned())
                .map(Some),
            Some(b'n') => {
                self.consume_literal(b"null")?;
                Ok(None)
            }
            _ => Err(format!(
                "Gemini result metadata field is not a string near byte {}",
                self.offset
            )),
        }
    }

    fn result_calls(
        &mut self,
        depth: usize,
    ) -> std::result::Result<Vec<GeminiRawResultCall>, String> {
        self.whitespace();
        self.take(b'[')?;
        self.whitespace();
        let mut calls = Vec::new();
        if self.peek() == Some(b']') {
            self.take(b']')?;
            return Ok(calls);
        }
        loop {
            if let Some(call) = self.result_call(depth.saturating_add(1))? {
                calls.push(call);
            }
            self.whitespace();
            match self.peek() {
                Some(b',') => {
                    self.take(b',')?;
                    self.whitespace();
                }
                Some(b']') => {
                    self.take(b']')?;
                    break;
                }
                _ => {
                    return Err(format!(
                        "invalid Gemini toolCalls array near byte {}",
                        self.offset
                    ));
                }
            }
        }
        Ok(calls)
    }

    fn result_call(
        &mut self,
        depth: usize,
    ) -> std::result::Result<Option<GeminiRawResultCall>, String> {
        self.whitespace();
        if self.peek() != Some(b'{') {
            self.skip_value(depth)?;
            return Ok(None);
        }
        self.take(b'{')?;
        self.whitespace();
        let mut call = GeminiRawResultCall {
            id: None,
            name: None,
            result: None,
            outcome: GeminiOutputOutcomeDto::default(),
        };
        if self.peek() == Some(b'}') {
            self.take(b'}')?;
            return Ok(Some(call));
        }
        loop {
            let key = self.key()?;
            self.whitespace();
            self.take(b':')?;
            self.whitespace();
            match key.as_deref() {
                Some("id") => call.id = self.optional_string()?,
                Some("name") => call.name = self.optional_string()?,
                Some("result") => call.result = Some(self.output_value(depth.saturating_add(1))?),
                Some(key) => {
                    if !self.outcome_field(key, &mut call.outcome, depth)? {
                        self.skip_value(depth)?;
                    }
                }
                None => self.skip_value(depth)?,
            }
            self.whitespace();
            match self.peek() {
                Some(b',') => {
                    self.take(b',')?;
                    self.whitespace();
                }
                Some(b'}') => {
                    self.take(b'}')?;
                    break;
                }
                _ => {
                    return Err(format!(
                        "invalid Gemini result tool call near byte {}",
                        self.offset
                    ));
                }
            }
        }
        Ok(Some(call))
    }
}

#[allow(clippy::too_many_arguments)]
fn hydrate_result_record(
    payload: &[u8],
    profile: GeminiNativePathProfile,
    source: &GeminiTranscriptSource,
    session: &GeminiSession,
    raw_ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
) -> std::result::Result<HydratedGeminiResult, String> {
    #[cfg(test)]
    TEST_RESULT_SELECTIVE_PASSES.set(TEST_RESULT_SELECTIVE_PASSES.get().saturating_add(1));
    let capture_full_content = profile == GeminiNativePathProfile::CoreAndTransientOutputs;
    #[cfg(test)]
    if capture_full_content {
        TEST_RESULT_FULL_HYDRATIONS.set(TEST_RESULT_FULL_HYDRATIONS.get().saturating_add(1));
    }
    // This is the record's sole hydration pass. CoreOnly retains only the
    // bounded preview while the same visitor captures full Pro content.
    let result = parse_result_record_selectively(payload, capture_full_content)?;
    let occurred_at_unix_ms = result.occurred_at_unix_ms;
    let native_record_id = result.native_record_id;
    let probed = result.outputs;
    if probed.len() > MAX_GEMINI_NATIVE_PAGE_RECORDS {
        return Err(format!(
            "Gemini result record exceeds the {MAX_GEMINI_NATIVE_PAGE_RECORDS} output limit"
        ));
    }
    let mut hydrated = HydratedGeminiResult {
        events: Vec::new(),
        outputs: Vec::new(),
        output_reservations: Vec::new(),
        decoded_body_bytes: 0,
        failure_diagnostics: 0,
        failure_previews: 0,
    };
    for (index, mut probed) in probed.into_iter().enumerate() {
        let content = probed.content.take();
        let diagnostic_content = content.as_deref().or(probed.diagnostic_preview.as_deref());
        let retained_failure = !probed.redacted
            && matches!(
                probed.outcome.outcome,
                OutputOutcome::Failure | OutputOutcome::Timeout
            );
        hydrated.decoded_body_bytes = hydrated.decoded_body_bytes.saturating_add(
            if profile == GeminiNativePathProfile::CoreAndTransientOutputs {
                content.as_ref().map_or(0, |content| content.len() as u64)
            } else if retained_failure {
                diagnostic_content.map_or(0, |content| content.len() as u64)
            } else {
                0
            },
        );
        let sub_ordinal = u32::try_from(index)
            .map_err(|_| "Gemini result subrecord ordinal overflowed".to_owned())?;
        if !probed.redacted && probed.has_output_content {
            hydrated.output_reservations.push((
                sub_ordinal,
                conservative_transient_output_reservation(
                    probed.content_bytes,
                    probed.call_id.as_deref(),
                    sub_ordinal,
                    source,
                    session,
                    raw_ordinal,
                    byte_start,
                    byte_end_exclusive,
                    native_record_id.as_deref(),
                )?,
            ));
        }
        if retained_failure {
            let event = hydrate_output_diagnostic(
                native_record_id.as_deref(),
                occurred_at_unix_ms,
                raw_ordinal,
                sub_ordinal,
                &probed,
                diagnostic_content,
            )?;
            let event_bytes = retained_event_bytes(&event)?;
            hydrated.failure_diagnostics = hydrated.failure_diagnostics.saturating_add(1);
            if diagnostic_content.is_some() {
                hydrated.failure_previews = hydrated.failure_previews.saturating_add(1);
            }
            hydrated.events.push((event.event, event_bytes));
        }
        if profile == GeminiNativePathProfile::CoreAndTransientOutputs
            && !probed.redacted
            && probed.has_output_content
        {
            push_transient_output(
                &mut hydrated.outputs,
                content.unwrap_or_default(),
                probed.outcome,
                probed.call_id,
                sub_ordinal,
                source,
                session,
                raw_ordinal,
                byte_start,
                byte_end_exclusive,
                occurred_at_unix_ms,
                native_record_id.as_deref(),
            )?;
        }
    }
    Ok(hydrated)
}

fn hydrate_output_diagnostic(
    native_record_id: Option<&str>,
    occurred_at_unix_ms: Option<i64>,
    raw_ordinal: u64,
    sub_ordinal: u32,
    output: &ProbedGeminiOutput,
    content: Option<&str>,
) -> std::result::Result<HydratedGeminiEvent, String> {
    let output_preview = content.map(|content| {
        content
            .chars()
            .take(PROVIDER_MAX_PREVIEW_CHARS)
            .collect::<String>()
    });
    let outcome = match output.outcome.outcome {
        OutputOutcome::Failure => "failure",
        OutputOutcome::Timeout => "timeout",
        OutputOutcome::Success => "success",
        OutputOutcome::Unknown => "unknown",
    }
    .to_owned();
    let body = GeminiEventBody::OutputDiagnostic {
        call_id: output.call_id.clone(),
        tool_name: output.tool_name.clone(),
        outcome,
        exit_code: output.outcome.exit_code,
        duration_ms: output.outcome.duration_ms,
        output_preview: output_preview.clone(),
    };
    let body_bytes = serde_json::to_vec(&body)
        .map_err(|error| format!("failed to encode Gemini output diagnostic: {error}"))?;
    let mut hasher = Sha256::new();
    hasher.update(BODY_HASH_DOMAIN);
    hasher.update(&body_bytes);
    let searchable_text = output_preview.unwrap_or_default();
    let identity = native_record_id
        .map(str::to_owned)
        .unwrap_or_else(|| format!("raw-{raw_ordinal}"));
    Ok(HydratedGeminiEvent {
        event: GeminiRetainedEvent {
            identity: GeminiEventIdentity::NativeRecordId(format!(
                "{identity}:subrecord:{sub_ordinal}"
            )),
            native_order: GeminiNativeOrder {
                raw_ordinal,
                sub_ordinal,
            },
            event_type: EventType::ToolOutput,
            role: EventRole::Tool,
            occurred_at: occurred_at_unix_ms.and_then(DateTime::<Utc>::from_timestamp_millis),
            body,
            body_sha256: hasher.finalize().into(),
            preview: searchable_text.clone(),
            searchable_text,
            safe_file_touches: Vec::new(),
        },
        serialized_body_bytes: body_bytes.len(),
    })
}

#[allow(clippy::too_many_arguments)]
fn conservative_transient_output_reservation(
    output_content_bytes: usize,
    call_id: Option<&str>,
    sub_ordinal: u32,
    source: &GeminiTranscriptSource,
    session: &GeminiSession,
    raw_ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
    native_record_id: Option<&str>,
) -> std::result::Result<usize, String> {
    let source_locator = GeminiSourceLocator {
        path: source.path.clone(),
        byte_start,
        byte_end_exclusive,
    };
    let locator_payload = serde_json::to_vec(&source_locator)
        .map_err(|error| format!("failed to encode Gemini output source locator: {error}"))?;
    let unit_key = format!(
        "gemini/nativepath/{}/{raw_ordinal}/{sub_ordinal}",
        session.native_session_id
    );
    let root_session_id = session
        .parent_native_session_id
        .as_deref()
        .unwrap_or(&session.native_session_id);
    let mut total = OUTPUT_ENVELOPE_FIXED_BYTES;
    for value in [
        Some(unit_key.as_str()),
        native_record_id,
        Some(session.native_session_id.as_str()),
        Some(root_session_id),
        session.parent_native_session_id.as_deref(),
        Some(session.native_session_id.as_str()),
        call_id,
        Some("gemini/nativepath/jsonl-result"),
    ]
    .into_iter()
    .flatten()
    {
        total = total
            .checked_add(estimated_json_string_wire_bytes(value).ok_or_else(|| {
                "Gemini transient output reservation byte count overflowed".to_owned()
            })?)
            .ok_or_else(|| {
                "Gemini transient output reservation byte count overflowed".to_owned()
            })?;
    }
    total = total
        .checked_add(
            estimated_base64_wire_bytes(locator_payload.len()).ok_or_else(|| {
                "Gemini transient output reservation byte count overflowed".to_owned()
            })?,
        )
        .ok_or_else(|| "Gemini transient output reservation byte count overflowed".to_owned())?;
    total
        .checked_add(
            estimated_base64_wire_bytes(output_content_bytes).ok_or_else(|| {
                "Gemini transient output reservation byte count overflowed".to_owned()
            })?,
        )
        .ok_or_else(|| "Gemini transient output reservation byte count overflowed".to_owned())
}

#[allow(clippy::too_many_arguments)]
fn push_transient_output(
    outputs: &mut Vec<(ProOutputObservation, usize)>,
    content: String,
    outcome: OutputOutcomeMetadata,
    call_id: Option<String>,
    sub_ordinal: u32,
    source: &GeminiTranscriptSource,
    session: &GeminiSession,
    raw_ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
    occurred_at_unix_ms: Option<i64>,
    native_record_id: Option<&str>,
) -> std::result::Result<(), String> {
    if outputs.len() >= MAX_GEMINI_NATIVE_PAGE_RECORDS {
        return Err(format!(
            "Gemini result record exceeds the {MAX_GEMINI_NATIVE_PAGE_RECORDS} output limit"
        ));
    }
    let source_locator = GeminiSourceLocator {
        path: source.path.clone(),
        byte_start,
        byte_end_exclusive,
    };
    let locator_payload = serde_json::to_vec(&source_locator)
        .map_err(|error| format!("failed to encode Gemini output source locator: {error}"))?;
    let root_session_id = session
        .parent_native_session_id
        .clone()
        .unwrap_or_else(|| session.native_session_id.clone());
    let observation = ProOutputObservation {
        kind: OutputObservationKind::Tool,
        coordinate: OutputNativeCoordinate {
            unit_key: format!(
                "gemini/nativepath/{}/{raw_ordinal}/{sub_ordinal}",
                session.native_session_id
            ),
            native_sequence: raw_ordinal,
            native_record_id: native_record_id.map(str::to_owned),
            source_record_ordinal: Some(raw_ordinal),
            source_record_subrecord_index: Some(sub_ordinal),
            byte_start: Some(byte_start),
            byte_end_exclusive: Some(byte_end_exclusive),
        },
        occurred_at_unix_ms,
        associations: OutputAssociations {
            direct_session_id: session.native_session_id.clone(),
            root_session_id,
            parent_session_id: session.parent_native_session_id.clone(),
            provider_session_id: Some(session.native_session_id.clone()),
            agent_id: None,
            repository: None,
        },
        call_id,
        command: None,
        outcome,
        locator: OutputSourceLocator {
            version: 1,
            kind: "gemini/nativepath/jsonl-result".to_owned(),
            payload: locator_payload,
        },
        content: content.into_bytes(),
    };
    let serialized_bytes = transient_output_bytes(&observation)?;
    outputs.push((observation, serialized_bytes));
    Ok(())
}

fn transient_output_bytes(
    observation: &ProOutputObservation,
) -> std::result::Result<usize, String> {
    let mut total = OUTPUT_ENVELOPE_FIXED_BYTES;
    for value in [
        Some(observation.coordinate.unit_key.as_str()),
        observation.coordinate.native_record_id.as_deref(),
        Some(observation.associations.direct_session_id.as_str()),
        Some(observation.associations.root_session_id.as_str()),
        observation.associations.parent_session_id.as_deref(),
        observation.associations.provider_session_id.as_deref(),
        observation.associations.agent_id.as_deref(),
        observation.call_id.as_deref(),
        Some(observation.locator.kind.as_str()),
    ]
    .into_iter()
    .flatten()
    {
        total = total
            .checked_add(estimated_json_string_wire_bytes(value).ok_or_else(|| {
                "Gemini transient output serialized byte count overflowed".to_owned()
            })?)
            .ok_or_else(|| "Gemini transient output serialized byte count overflowed".to_owned())?;
    }
    if let Some(command) = &observation.command {
        for value in [
            Some(command.tool_name.as_str()),
            Some(command.command.as_str()),
            command.working_directory.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            total = total
                .checked_add(estimated_json_string_wire_bytes(value).ok_or_else(|| {
                    "Gemini transient output serialized byte count overflowed".to_owned()
                })?)
                .ok_or_else(|| {
                    "Gemini transient output serialized byte count overflowed".to_owned()
                })?;
        }
    }
    total = total
        .checked_add(
            estimated_base64_wire_bytes(observation.locator.payload.len()).ok_or_else(|| {
                "Gemini transient output serialized byte count overflowed".to_owned()
            })?,
        )
        .ok_or_else(|| "Gemini transient output serialized byte count overflowed".to_owned())?;
    total
        .checked_add(
            estimated_base64_wire_bytes(observation.content.len()).ok_or_else(|| {
                "Gemini transient output serialized byte count overflowed".to_owned()
            })?,
        )
        .ok_or_else(|| "Gemini transient output serialized byte count overflowed".to_owned())
}

#[derive(Debug, Deserialize)]
struct GeminiStateNoticeDto {
    id: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "$set")]
    set: GeminiStateSetDto,
}

#[derive(Debug, Deserialize)]
struct GeminiStateSetDto {
    summary: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiRewindNoticeDto {
    id: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "$rewindTo")]
    rewind_to: String,
}

#[derive(Debug)]
enum GeminiHydrationError {
    Invalid(String),
    TouchOverflow(GeminiTouchOverflow),
}

struct HydratedGeminiEvent {
    event: GeminiRetainedEvent,
    serialized_body_bytes: usize,
}

impl From<String> for GeminiHydrationError {
    fn from(error: String) -> Self {
        Self::Invalid(error)
    }
}

fn hydrate_retained_event(
    payload: &[u8],
    class: GeminiRecordClass,
    raw_ordinal: u64,
) -> std::result::Result<Option<HydratedGeminiEvent>, GeminiHydrationError> {
    let (id, occurred_at, event_type, role, body, searchable_text, safe_file_touches) = match class
    {
        GeminiRecordClass::Message => {
            let dto: GeminiMessageDto = serde_json::from_slice(payload)
                .map_err(|error| format!("invalid Gemini message: {error}"))?;
            let Some(text) = dto.content.filter(|text| !text.is_empty()) else {
                return Ok(None);
            };
            let role = match dto.record_type.as_deref() {
                Some("user") => EventRole::User,
                Some("gemini") => EventRole::Assistant,
                _ => return Ok(None),
            };
            (
                required_record_id(dto.id)?,
                dto.timestamp.as_deref().and_then(parse_timestamp),
                EventType::Message,
                role,
                GeminiEventBody::Message {
                    text: text.clone(),
                    model: dto.model,
                },
                text,
                Vec::new(),
            )
        }
        GeminiRecordClass::ToolCall => {
            let dto: GeminiToolCallRecordDto = serde_json::from_slice(payload)
                .map_err(|error| format!("invalid Gemini tool call: {error}"))?;
            if dto.tool_calls.iter().any(|call| call.result.0) {
                return Err(GeminiHydrationError::Invalid(
                    "Gemini result-bearing tool call reached retained hydration".to_owned(),
                ));
            }
            let calls: Vec<_> = dto
                .tool_calls
                .into_iter()
                .map(|call| GeminiToolCall {
                    id: nonempty(call.id),
                    name: nonempty(call.name),
                    args: call.args,
                })
                .collect();
            if calls.is_empty() {
                return Ok(None);
            }
            let searchable_text = tool_call_search_text(&calls);
            let safe_file_touches =
                safe_file_touches(&calls).map_err(GeminiHydrationError::TouchOverflow)?;
            (
                required_record_id(dto.id)?,
                dto.timestamp.as_deref().and_then(parse_timestamp),
                EventType::ToolCall,
                EventRole::Assistant,
                GeminiEventBody::ToolCall { calls },
                searchable_text,
                safe_file_touches,
            )
        }
        GeminiRecordClass::StateNotice => {
            let dto: GeminiStateNoticeDto = serde_json::from_slice(payload)
                .map_err(|error| format!("invalid Gemini state notice: {error}"))?;
            let summary = dto.set.summary;
            (
                required_record_id(dto.id)?,
                dto.timestamp.as_deref().and_then(parse_timestamp),
                EventType::Notice,
                EventRole::System,
                GeminiEventBody::StateNotice {
                    summary: summary.clone(),
                },
                summary.unwrap_or_default(),
                Vec::new(),
            )
        }
        GeminiRecordClass::RewindNotice => {
            let dto: GeminiRewindNoticeDto = serde_json::from_slice(payload)
                .map_err(|error| format!("invalid Gemini rewind notice: {error}"))?;
            let target = dto.rewind_to.trim().to_owned();
            if target.is_empty() {
                return Ok(None);
            }
            (
                required_record_id(dto.id)?,
                dto.timestamp.as_deref().and_then(parse_timestamp),
                EventType::Notice,
                EventRole::System,
                GeminiEventBody::RewindNotice {
                    target_native_record_id: target.clone(),
                },
                format!("rewind to {target}"),
                Vec::new(),
            )
        }
        GeminiRecordClass::Header | GeminiRecordClass::Result | GeminiRecordClass::Ignored => {
            return Ok(None)
        }
    };

    let body_bytes = serde_json::to_vec(&body)
        .map_err(|error| format!("failed to encode retained Gemini body: {error}"))?;
    let mut hasher = Sha256::new();
    hasher.update(BODY_HASH_DOMAIN);
    hasher.update(&body_bytes);
    let body_sha256 = hasher.finalize().into();
    let preview = searchable_text
        .chars()
        .take(PROVIDER_MAX_PREVIEW_CHARS)
        .collect();
    Ok(Some(HydratedGeminiEvent {
        event: GeminiRetainedEvent {
            identity: GeminiEventIdentity::NativeRecordId(id),
            native_order: GeminiNativeOrder {
                raw_ordinal,
                sub_ordinal: 0,
            },
            event_type,
            role,
            occurred_at,
            body,
            body_sha256,
            preview,
            searchable_text,
            safe_file_touches,
        },
        serialized_body_bytes: body_bytes.len(),
    }))
}

fn retained_event_bytes(event: &HydratedGeminiEvent) -> std::result::Result<usize, String> {
    let mut total = EVENT_ENVELOPE_FIXED_BYTES
        .checked_add(event.serialized_body_bytes)
        .ok_or_else(|| "Gemini retained event byte count overflowed".to_owned())?;
    let GeminiEventIdentity::NativeRecordId(identity) = &event.event.identity;
    for value in [
        identity.as_str(),
        event.event.preview.as_str(),
        event.event.searchable_text.as_str(),
    ]
    .into_iter()
    .chain(event.event.safe_file_touches.iter().map(String::as_str))
    {
        total =
            total
                .checked_add(estimated_json_string_wire_bytes(value).ok_or_else(|| {
                    "Gemini retained event string byte count overflowed".to_owned()
                })?)
                .ok_or_else(|| "Gemini retained event byte count overflowed".to_owned())?;
    }
    Ok(total)
}

fn core_page_conservative_bytes(
    expected: &GeminiPageFrontier,
    next: &GeminiPageFrontier,
    retained_event_bytes: usize,
    rejection_bytes: usize,
) -> Option<usize> {
    PAGE_ENVELOPE_FIXED_BYTES
        .checked_add(frontier_wire_bytes(expected)?)?
        .checked_add(frontier_wire_bytes(next)?)?
        .checked_add(retained_event_bytes)?
        .checked_add(rejection_bytes)
}

fn output_page_conservative_bytes(
    expected: &GeminiPageFrontier,
    next: &GeminiPageFrontier,
    transient_output_bytes: usize,
) -> Option<usize> {
    PAGE_ENVELOPE_FIXED_BYTES
        .checked_add(frontier_wire_bytes(expected)?)?
        .checked_add(frontier_wire_bytes(next)?)?
        .checked_add(transient_output_bytes)
}

fn build_output_pages(
    expected: &GeminiPageFrontier,
    next: &GeminiPageFrontier,
    outputs: Vec<(ProOutputObservation, usize)>,
    terminal: bool,
) -> GeminiScanResult<Vec<GeminiNativeOutputPage>> {
    let mut pages = Vec::new();
    let mut page_outputs = Vec::new();
    let mut page_output_bytes = 0_usize;

    for (output, output_bytes) in outputs {
        let next_units = page_outputs
            .len()
            .checked_add(1)
            .ok_or(GeminiScanError::Capture(CaptureError::SystemInvariant(
                "Gemini output page unit accounting overflowed",
            )))?;
        let next_output_bytes =
            page_output_bytes
                .checked_add(output_bytes)
                .ok_or(GeminiScanError::Capture(CaptureError::SystemInvariant(
                    "Gemini output page byte accounting overflowed",
                )))?;
        let next_page_bytes = output_page_conservative_bytes(expected, next, next_output_bytes)
            .ok_or(GeminiScanError::Capture(CaptureError::SystemInvariant(
                "Gemini output page accounting overflowed",
            )))?;
        if next_units > MAX_GEMINI_NATIVE_PAGE_RECORDS
            || next_page_bytes > MAX_GEMINI_NATIVE_PAGE_BYTES
        {
            if page_outputs.is_empty() {
                return Err(GeminiScanError::Capture(CaptureError::SystemInvariant(
                    "Gemini output passed admission but cannot fit an empty output page",
                )));
            }
            pages.push(finish_output_page(
                expected,
                next,
                pages.len(),
                page_outputs,
                page_output_bytes,
                terminal,
            )?);
            page_outputs = Vec::new();
            page_output_bytes = 0;
        }

        page_output_bytes =
            page_output_bytes
                .checked_add(output_bytes)
                .ok_or(GeminiScanError::Capture(CaptureError::SystemInvariant(
                    "Gemini output page byte accounting overflowed",
                )))?;
        let single_page_bytes = output_page_conservative_bytes(expected, next, page_output_bytes)
            .ok_or(GeminiScanError::Capture(CaptureError::SystemInvariant(
            "Gemini output page accounting overflowed",
        )))?;
        if single_page_bytes > MAX_GEMINI_NATIVE_PAGE_BYTES {
            return Err(GeminiScanError::Capture(CaptureError::SystemInvariant(
                "Gemini output passed admission but exceeds an empty output page",
            )));
        }
        page_outputs.push(output);
    }

    if !page_outputs.is_empty() || pages.is_empty() {
        pages.push(finish_output_page(
            expected,
            next,
            pages.len(),
            page_outputs,
            page_output_bytes,
            terminal,
        )?);
    }
    Ok(pages)
}

fn finish_output_page(
    expected: &GeminiPageFrontier,
    next: &GeminiPageFrontier,
    _page_ordinal: usize,
    outputs: Vec<ProOutputObservation>,
    transient_output_bytes: usize,
    _terminal: bool,
) -> GeminiScanResult<GeminiNativeOutputPage> {
    #[cfg(test)]
    let page_ordinal = u32::try_from(_page_ordinal).map_err(|_| {
        GeminiScanError::Capture(CaptureError::SystemInvariant(
            "Gemini output page ordinal overflowed",
        ))
    })?;
    let conservative_serialized_bytes =
        output_page_conservative_bytes(expected, next, transient_output_bytes).ok_or(
            GeminiScanError::Capture(CaptureError::SystemInvariant(
                "Gemini output page accounting overflowed",
            )),
        )?;
    if outputs.len() > MAX_GEMINI_NATIVE_PAGE_RECORDS
        || conservative_serialized_bytes > MAX_GEMINI_NATIVE_PAGE_BYTES
    {
        return Err(GeminiScanError::Capture(CaptureError::SystemInvariant(
            "Gemini output page exceeded its admitted bounds",
        )));
    }
    #[cfg(test)]
    let identity = derive_output_page_identity(expected, next, page_ordinal, &outputs, _terminal);
    Ok(GeminiNativeOutputPage {
        #[cfg(test)]
        identity,
        #[cfg(test)]
        page_ordinal,
        logical_units: outputs.len(),
        outputs,
        conservative_serialized_bytes,
    })
}

fn frontier_wire_bytes(frontier: &GeminiPageFrontier) -> Option<usize> {
    let mut total = 1024_usize;
    if let Some(session) = &frontier.session {
        for value in [
            Some(session.native_session_id.as_str()),
            session.parent_native_session_id.as_deref(),
            session.cwd.as_deref(),
            session.native_kind.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            total = total.checked_add(estimated_json_string_wire_bytes(value)?)?;
        }
    }
    Some(total)
}

fn estimated_json_string_wire_bytes(value: &str) -> Option<usize> {
    value.chars().try_fold(2_usize, |total, character| {
        let escaped_bytes = match character {
            '"' | '\\' | '\u{0008}' | '\u{0009}' | '\n' | '\u{000c}' | '\r' => 2,
            '\u{0000}'..='\u{001f}' => 6,
            _ => character.len_utf8(),
        };
        total.checked_add(escaped_bytes)
    })
}

fn estimated_base64_wire_bytes(decoded_bytes: usize) -> Option<usize> {
    decoded_bytes
        .checked_add(2)?
        .checked_div(3)?
        .checked_mul(4)?
        .checked_add(2)
}

fn rejection_wire_bytes(rejection: &GeminiRejection) -> Option<usize> {
    REJECTION_ENVELOPE_FIXED_BYTES.checked_add(estimated_json_string_wire_bytes(&rejection.reason)?)
}

fn derive_page_identity(
    expected: &GeminiPageFrontier,
    next: &GeminiPageFrontier,
    events: &[GeminiRetainedEvent],
    rejections: &[GeminiRejection],
    terminal: bool,
) -> GeminiPageIdentity {
    let mut hasher = Sha256::new();
    hasher.update(CORE_PAGE_IDENTITY_DOMAIN);
    hash_page_frontier(&mut hasher, expected);
    hash_page_frontier(&mut hasher, next);
    hasher.update([u8::from(terminal)]);
    hasher.update(
        u64::try_from(events.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    for event in events {
        hasher.update(b"event\0");
        match &event.identity {
            GeminiEventIdentity::NativeRecordId(value) => hash_page_text(&mut hasher, value),
        }
        hasher.update(event.native_order.raw_ordinal.to_le_bytes());
        hasher.update(event.native_order.sub_ordinal.to_le_bytes());
        hash_page_text(&mut hasher, event.event_type.as_str());
        hash_page_text(&mut hasher, event.role.as_str());
        hash_page_optional_i64(
            &mut hasher,
            event.occurred_at.map(|value| value.timestamp_millis()),
        );
        hasher.update(event.body_sha256);
        hash_page_text(&mut hasher, &event.preview);
        hash_page_text(&mut hasher, &event.searchable_text);
        hasher.update(
            u64::try_from(event.safe_file_touches.len())
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        for touch in &event.safe_file_touches {
            hash_page_text(&mut hasher, touch);
        }
    }
    hasher.update(
        u64::try_from(rejections.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    for rejection in rejections {
        hasher.update(b"rejection\0");
        hasher.update(rejection.raw_ordinal.to_le_bytes());
        hasher.update(rejection.byte_start.to_le_bytes());
        hasher.update(rejection.byte_end_exclusive.to_le_bytes());
        match &rejection.kind {
            GeminiRejectionKind::InvalidRecord => hasher.update([0]),
        }
        hash_page_text(&mut hasher, &rejection.reason);
    }
    GeminiPageIdentity(hasher.finalize().into())
}

#[cfg(test)]
fn derive_output_page_identity(
    expected: &GeminiPageFrontier,
    next: &GeminiPageFrontier,
    page_ordinal: u32,
    outputs: &[ProOutputObservation],
    terminal: bool,
) -> GeminiOutputPageIdentity {
    let mut hasher = Sha256::new();
    hasher.update(OUTPUT_PAGE_IDENTITY_DOMAIN);
    hash_page_frontier(&mut hasher, expected);
    hash_page_frontier(&mut hasher, next);
    hasher.update(page_ordinal.to_le_bytes());
    hasher.update([u8::from(terminal)]);
    hasher.update(
        u64::try_from(outputs.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    for output in outputs {
        hasher.update(b"output\0");
        hasher.update([match output.kind {
            OutputObservationKind::Command => 0,
            OutputObservationKind::Tool => 1,
        }]);
        hash_page_text(&mut hasher, &output.coordinate.unit_key);
        hasher.update(output.coordinate.native_sequence.to_le_bytes());
        hash_page_optional_text(&mut hasher, output.coordinate.native_record_id.as_deref());
        hash_page_optional_u64(&mut hasher, output.coordinate.source_record_ordinal);
        hash_page_optional_u32(&mut hasher, output.coordinate.source_record_subrecord_index);
        hash_page_optional_u64(&mut hasher, output.coordinate.byte_start);
        hash_page_optional_u64(&mut hasher, output.coordinate.byte_end_exclusive);
        hash_page_optional_i64(&mut hasher, output.occurred_at_unix_ms);
        hash_page_text(&mut hasher, &output.associations.direct_session_id);
        hash_page_text(&mut hasher, &output.associations.root_session_id);
        hash_page_optional_text(
            &mut hasher,
            output.associations.parent_session_id.as_deref(),
        );
        hash_page_optional_text(
            &mut hasher,
            output.associations.provider_session_id.as_deref(),
        );
        hash_page_optional_text(&mut hasher, output.associations.agent_id.as_deref());
        if let Some(repository) = &output.associations.repository {
            hasher.update([1]);
            hash_page_text(&mut hasher, &repository.repository_id);
            hash_page_optional_text(&mut hasher, repository.checkout_id.as_deref());
            hash_page_optional_text(&mut hasher, repository.worktree_id.as_deref());
            hash_page_optional_text(&mut hasher, repository.object_format.as_deref());
        } else {
            hasher.update([0]);
        }
        hash_page_optional_text(&mut hasher, output.call_id.as_deref());
        if let Some(command) = &output.command {
            hasher.update([1]);
            hash_page_text(&mut hasher, &command.tool_name);
            hash_page_text(&mut hasher, &command.command);
            hash_page_optional_text(&mut hasher, command.working_directory.as_deref());
        } else {
            hasher.update([0]);
        }
        hasher.update([match output.outcome.outcome {
            OutputOutcome::Success => 0,
            OutputOutcome::Failure => 1,
            OutputOutcome::Timeout => 2,
            OutputOutcome::Unknown => 3,
        }]);
        hash_page_optional_i32(&mut hasher, output.outcome.exit_code);
        hash_page_optional_u64(&mut hasher, output.outcome.duration_ms);
        hasher.update(output.locator.version.to_le_bytes());
        hash_page_text(&mut hasher, &output.locator.kind);
        hash_page_bytes(&mut hasher, &output.locator.payload);
        hash_page_bytes(&mut hasher, &output.content);
    }
    GeminiOutputPageIdentity(hasher.finalize().into())
}

fn hash_page_frontier(hasher: &mut Sha256, frontier: &GeminiPageFrontier) {
    hasher.update(frontier.parser_revision.to_le_bytes());
    hasher.update(frontier.policy_revision.to_le_bytes());
    hasher.update(frontier.complete_prefix_end.to_le_bytes());
    hasher.update(frontier.complete_prefix_sha256);
    hash_page_optional_u64(hasher, frontier.source_device);
    hash_page_optional_u64(hasher, frontier.source_inode);
    hasher.update(frontier.next_raw_ordinal.to_le_bytes());
    hasher.update(frontier.retained_event_count.to_le_bytes());
    hasher.update(frontier.rejected_records.to_le_bytes());
    hasher.update([u8::from(frontier.append_boundary_safe)]);
    if let Some(session) = &frontier.session {
        hasher.update([1]);
        hash_page_text(hasher, &session.native_session_id);
        hash_page_optional_text(hasher, session.parent_native_session_id.as_deref());
        hash_page_text(hasher, session.agent_type.as_str());
        hash_page_optional_i64(
            hasher,
            session.started_at.map(|value| value.timestamp_millis()),
        );
        hash_page_optional_text(hasher, session.cwd.as_deref());
        hash_page_optional_text(hasher, session.native_kind.as_deref());
    } else {
        hasher.update([0]);
    }
}

fn hash_page_text(hasher: &mut Sha256, value: &str) {
    hash_page_bytes(hasher, value.as_bytes());
}

fn hash_page_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(value);
}

fn hash_page_optional_text(hasher: &mut Sha256, value: Option<&str>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hash_page_text(hasher, value);
    }
}

fn hash_page_optional_u64(hasher: &mut Sha256, value: Option<u64>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_le_bytes());
    }
}

#[cfg(test)]
fn hash_page_optional_u32(hasher: &mut Sha256, value: Option<u32>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_le_bytes());
    }
}

fn hash_page_optional_i64(hasher: &mut Sha256, value: Option<i64>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_le_bytes());
    }
}

#[cfg(test)]
fn hash_page_optional_i32(hasher: &mut Sha256, value: Option<i32>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_le_bytes());
    }
}

fn required_record_id(id: Option<String>) -> std::result::Result<String, String> {
    nonempty(id).ok_or_else(|| "Gemini event is missing a nonempty native id".to_owned())
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn tool_call_search_text(calls: &[GeminiToolCall]) -> String {
    let mut text = String::new();
    for call in calls {
        if let Some(name) = call.name.as_deref() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(name);
        }
        if let Some(args) = call.args.as_ref() {
            if let Ok(args) = serde_json::to_string(args) {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&args);
            }
        }
    }
    text
}

fn safe_file_touches(
    calls: &[GeminiToolCall],
) -> std::result::Result<Vec<String>, GeminiTouchOverflow> {
    let mut touches = BTreeSet::new();
    let mut touch_bytes = 0_usize;
    for call in calls {
        let Some(Value::Object(args)) = call.args.as_ref() else {
            continue;
        };
        for key in ["path", "file_path", "filePath"] {
            if let Some(Value::String(path)) = args.get(key) {
                if path.trim().is_empty() || touches.contains(path) {
                    continue;
                }
                if touches.len() >= MAX_GEMINI_FILE_TOUCHES_PER_EVENT {
                    return Err(GeminiTouchOverflow::Count {
                        limit: MAX_GEMINI_FILE_TOUCHES_PER_EVENT,
                    });
                }
                let next_bytes =
                    touch_bytes
                        .checked_add(path.len())
                        .ok_or(GeminiTouchOverflow::Bytes {
                            limit: MAX_GEMINI_FILE_TOUCH_BYTES_PER_EVENT,
                        })?;
                if next_bytes > MAX_GEMINI_FILE_TOUCH_BYTES_PER_EVENT {
                    return Err(GeminiTouchOverflow::Bytes {
                        limit: MAX_GEMINI_FILE_TOUCH_BYTES_PER_EVENT,
                    });
                }
                touch_bytes = next_bytes;
                touches.insert(path.clone());
            }
        }
    }
    Ok(touches.into_iter().collect())
}

struct RecordRead {
    bytes_observed: u64,
    terminated: bool,
    oversized: bool,
}

fn read_record(
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

fn trim_jsonl_ending(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn new_prefix_hasher() -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(PREFIX_HASH_DOMAIN);
    hasher
}

fn prefix_digest(hasher: &Sha256) -> [u8; 32] {
    hasher.clone().finalize().into()
}

fn hash_gemini_prefix(file: &mut File, complete_prefix_end: u64) -> GeminiScanResult<Sha256> {
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

fn same_physical_file(previous: &GeminiFileObservation, current: &GeminiFileObservation) -> bool {
    match (
        previous.device.zip(previous.inode),
        current.device.zip(current.inode),
    ) {
        (Some(previous), Some(current)) => previous == current,
        (None, None) => true,
        _ => false,
    }
}

fn frontier_file_identity_matches(
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

fn lifecycle_signals(
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

fn classify_cross_path_source(
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

fn classify_source_change(
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

fn open_gemini_transcript(path: &Path) -> Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        Ok(OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)?)
    }
    #[cfg(not(unix))]
    {
        Ok(OpenOptions::new().read(true).open(path)?)
    }
}
