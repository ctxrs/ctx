use std::{
    fs,
    io::{self, BufRead, BufReader, Seek, SeekFrom},
    path::Path,
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, ContentRef, EventType};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    common::io::OpenedProviderSourceFile,
    complete_content::{
        attach_verified_content_locator, jsonl::JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        verified_content_address_supported, verified_content_profile, CompleteContentBodyDigest,
        CompleteContentSourceFamily, VerifiedContentLocatorV1, VerifiedContentRole,
        COMPLETE_CONTENT_MAX_BODY_BYTES,
    },
    fnv1a64,
    provider::{
        file_touches::{
            visit_provider_file_touch_drafts_with_limit, MAX_PACKED_PROVIDER_EVENT_INDEX,
        },
        native_ingestion::{NativeIngestionPage, NativePageAccounting},
        normalization::{
            provider_capped_json, provider_policy_body, provider_policy_event_text,
            provider_result_identifier_evidence, provider_result_outcome_evidence,
        },
    },
    CaptureError, ProviderAdapterContext, MAX_PROVIDER_JSONL_LINE_BYTES,
    PROVIDER_MAX_PREVIEW_CHARS, PROVIDER_MAX_TEXT_CHARS,
};

use super::{
    super::{
        pi_complete_content_message_record,
        text::{pi_entry_text, pi_event_role, pi_message_has_tool_call, pi_result_content},
        PI_SOURCE_FORMAT,
    },
    checkpoint::{
        initial_prefix_hasher, prefix_digest, PiNativeCheckpoint, PI_NATIVEPATH_PARSER_REVISION,
        PI_NATIVEPATH_POLICY_REVISION,
    },
    rows::{
        core_units_encoded_bytes, PiCorePageBuilder, PiNativeCorePage, PiNativeCoreUnit,
        PiNativeEventRow, PiNativeFileTouchRow, PiNativePhysicalLocator, PiNativeRejection,
        PiNativeRejectionKind, PiNativeSessionRow, PI_NATIVE_PAGE_ENCODING_RESERVE,
        PI_NATIVE_PAGE_MAX_BYTES, PI_NATIVE_PAGE_MAX_UNITS,
    },
    source::{PiFrozenSource, PiNativePathError},
};

mod page;
mod record;
mod support;

use support::*;

const PI_CORE_TOUCH_LIMIT: usize = PI_NATIVE_PAGE_MAX_UNITS - 1;

#[derive(Clone, Debug)]
struct PiNativeSessionHeader {
    id: String,
    version: Option<u64>,
    timestamp: DateTime<Utc>,
    cwd: Option<String>,
    parent_session: Option<String>,
    raw: Value,
}

fn parse_pi_session_header(value: Value) -> Result<PiNativeSessionHeader, CaptureError> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| CaptureError::InvalidPayload("pi session header missing id".to_owned()))?
        .to_owned();
    let timestamp = value
        .get("timestamp")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CaptureError::InvalidPayload("pi session header missing timestamp".to_owned())
        })
        .and_then(|timestamp| {
            DateTime::parse_from_rfc3339(timestamp)
                .map(|time| time.with_timezone(&Utc))
                .map_err(CaptureError::from)
        })?;
    Ok(PiNativeSessionHeader {
        id,
        version: value.get("version").and_then(Value::as_u64),
        timestamp,
        cwd: value.get("cwd").and_then(Value::as_str).map(str::to_owned),
        parent_session: value
            .get("parentSession")
            .and_then(Value::as_str)
            .map(str::to_owned),
        raw: value,
    })
}

pub(super) fn pi_native_event_type(entry_type: &str, message: Option<&Value>) -> EventType {
    match entry_type {
        "compaction" | "branch_summary" => EventType::Summary,
        "message" => match message
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
        {
            "toolResult" => EventType::ToolOutput,
            "bashExecution" => EventType::CommandOutput,
            "assistant" if message.is_some_and(pi_message_has_tool_call) => EventType::ToolCall,
            _ => EventType::Message,
        },
        "model_change"
        | "thinking_level_change"
        | "custom"
        | "custom_message"
        | "label"
        | "session_info" => EventType::Notice,
        _ => EventType::Notice,
    }
}

fn pi_native_event_payload(entry: &Value, event_type: EventType) -> Value {
    let entry_type = entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let message = entry.get("message");
    let message_role = message
        .and_then(|message| message.get("role"))
        .and_then(Value::as_str);
    let text = pi_entry_text(entry, message).unwrap_or_default();
    let retained_text = provider_policy_event_text(event_type, &text, entry);
    let result_evidence = provider_result_identifier_evidence(event_type, &text, entry);
    let result_outcome = provider_result_outcome_evidence(event_type, entry);
    let command = message
        .and_then(|value| value.get("command"))
        .and_then(Value::as_str);
    let exit_code = message
        .and_then(|value| value.get("exitCode"))
        .and_then(Value::as_i64);
    json!({
        "entry_type": entry_type,
        "entry_id": entry.get("id").and_then(Value::as_str),
        "parent_id": entry.get("parentId").and_then(Value::as_str),
        "message_role": message_role,
        "command": command,
        "exit_code": exit_code,
        "text": retained_text.text,
        "text_retention": retained_text.retention.as_json(),
        "result_evidence": result_evidence,
        "result_outcome": result_outcome,
        "body": provider_capped_json(
            &provider_policy_body(event_type, entry),
            PROVIDER_MAX_PREVIEW_CHARS,
        ),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PiResultOutcome {
    Success,
    Failure,
    Timeout,
    Unknown,
}

#[derive(Clone, Copy, Debug)]
struct PiResultOutcomeMetadata {
    outcome: PiResultOutcome,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
}

fn pi_result_outcome(entry: &Value, event_type: EventType) -> PiResultOutcomeMetadata {
    let message = entry.get("message").unwrap_or(entry);
    let exit_code = message
        .get("exitCode")
        .or_else(|| message.get("exit_code"))
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    let duration_ms = message
        .get("durationMs")
        .or_else(|| message.get("duration_ms"))
        .and_then(Value::as_u64);
    let timed_out = ["timedOut", "timed_out", "timeout"]
        .into_iter()
        .any(|key| message.get(key).and_then(Value::as_bool).unwrap_or(false))
        || message
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| {
                matches!(
                    status.trim().to_ascii_lowercase().as_str(),
                    "timeout" | "timed_out" | "timedout"
                )
            });
    let classified = provider_result_outcome_evidence(event_type, entry);
    let outcome = if timed_out {
        PiResultOutcome::Timeout
    } else {
        match classified.as_str() {
            Some("success") => PiResultOutcome::Success,
            Some("failure") => PiResultOutcome::Failure,
            _ => PiResultOutcome::Unknown,
        }
    };
    PiResultOutcomeMetadata {
        outcome,
        exit_code,
        duration_ms,
    }
}

fn pi_command_output_is_supported(entry: &Value) -> bool {
    let Some(message) = entry.get("message") else {
        return false;
    };
    if message.get("role").and_then(Value::as_str) != Some("bashExecution") {
        return false;
    }
    message
        .get("command")
        .and_then(Value::as_str)
        .filter(|command| !command.is_empty() && command.len() <= 64 * 1024)
        .is_some_and(|command| !command.contains('\0'))
}

fn pi_provider_event_identity_index(header: &PiNativeSessionHeader, entry: &Value) -> Option<u64> {
    entry
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(|id| fnv1a64(format!("pi:{}:{id}", header.id).as_bytes()))
}

fn pi_event_idempotency_key(
    header: &PiNativeSessionHeader,
    entry: &Value,
    line_number: usize,
) -> String {
    entry
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(|id| format!("provider-event:pi:{}:{id}", header.id))
        .unwrap_or_else(|| format!("provider-event:pi:{}:{line_number}", header.id))
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PiNativeResume {
    pub(crate) core: Option<PiNativeCheckpoint>,
}

#[derive(Clone, Debug)]
pub(crate) struct PiNativeScanOptions {
    pub(crate) context: ProviderAdapterContext,
    pub(crate) resume: PiNativeResume,
}

impl PiNativeScanOptions {
    pub(crate) fn new(context: ProviderAdapterContext) -> Self {
        Self {
            context,
            resume: PiNativeResume::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PiSourceLifecycle {
    Fresh,
    NoOp,
    Append,
    Rewrite,
    Truncate,
    Replace,
    Relocate,
    Copy,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PiNativeScanStats {
    pub(crate) source_file_opens: u64,
    pub(crate) source_fences: u64,
    pub(crate) prefix_bytes_hashed: u64,
    pub(crate) prefix_header_records_parsed: u64,
    pub(crate) source_bytes_read: u64,
    pub(crate) parsed_delta_bytes: u64,
    pub(crate) semantic_records_parsed: u64,
    pub(crate) complete_records: u64,
    pub(crate) incomplete_tail_bytes: u64,
    pub(crate) malformed_records: u64,
    pub(crate) oversized_records: u64,
    pub(crate) native_result_records: u64,
    pub(crate) native_result_success: u64,
    pub(crate) native_result_failure: u64,
    pub(crate) native_result_timeout: u64,
    pub(crate) native_result_unknown: u64,
    pub(crate) result_body_extractions: u64,
    pub(crate) core_pages: u64,
    pub(crate) peak_core_page_units: usize,
    pub(crate) peak_core_page_bytes: usize,
    pub(crate) peak_ready_page_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PiNativeScanOutcome {
    pub(crate) complete: bool,
    pub(crate) core_lifecycle: Option<PiSourceLifecycle>,
    pub(crate) core_checkpoint: Option<PiNativeCheckpoint>,
    pub(crate) stats: PiNativeScanStats,
}

pub(crate) enum PiNativeOpenOutcome {
    Ready(Box<PiNativeScanner>),
    Deleted,
}

struct LanePlan {
    checkpoint: PiNativeCheckpoint,
    lifecycle: PiSourceLifecycle,
    verify_prefix: bool,
}

struct CoreLane {
    emitted: PiNativeCheckpoint,
    current: PiNativeCheckpoint,
    activate_at: u64,
    active: bool,
    lifecycle: PiSourceLifecycle,
    builder: PiCorePageBuilder,
}

struct PendingRecord {
    core_units: Vec<PiNativeCoreUnit>,
    core_encoded_bytes: usize,
    checkpoint: PiNativeCheckpoint,
}

struct RawLine {
    bytes: Vec<u8>,
    observed_bytes: u64,
    terminated: bool,
    oversized: bool,
}

pub(crate) struct PiNativeScanner {
    reader: BufReader<fs::File>,
    source: PiFrozenSource,
    context: ProviderAdapterContext,
    source_revision: String,
    core: Option<CoreLane>,
    header: Option<PiNativeSessionHeader>,
    stream_hasher: Sha256,
    scan_offset: u64,
    scan_ordinal: u64,
    pending: Option<PendingRecord>,
    ready_core: Option<NativeIngestionPage<PiNativeCorePage>>,
    eof: bool,
    complete: bool,
    finished: bool,
    stats: PiNativeScanStats,
}

pub(crate) fn open_pi_native_session(
    path: &Path,
    options: PiNativeScanOptions,
) -> Result<PiNativeOpenOutcome, PiNativePathError> {
    let opened = match PiFrozenSource::open(path) {
        Ok(source) => source,
        Err(PiNativePathError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(PiNativeOpenOutcome::Deleted);
        }
        Err(error) => return Err(error),
    };
    open_pi_native_session_from_frozen(path, opened, options)
}

pub(crate) fn open_pi_native_session_retained(
    path: &Path,
    opened: Arc<OpenedProviderSourceFile>,
    options: PiNativeScanOptions,
) -> Result<PiNativeOpenOutcome, PiNativePathError> {
    let frozen = match PiFrozenSource::from_opened(path, opened) {
        Ok(source) => source,
        Err(PiNativePathError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(PiNativeOpenOutcome::Deleted);
        }
        Err(error) => return Err(error),
    };
    open_pi_native_session_from_frozen(path, frozen, options)
}

fn open_pi_native_session_from_frozen(
    path: &Path,
    (file, source): (fs::File, PiFrozenSource),
    options: PiNativeScanOptions,
) -> Result<PiNativeOpenOutcome, PiNativePathError> {
    let source_revision = source.source_revision();
    let mut core_plan = Some(plan_lane(options.resume.core.as_ref(), &source));
    let mut reader = BufReader::new(file);
    let mut stats = PiNativeScanStats {
        source_file_opens: 1,
        ..PiNativeScanStats::default()
    };
    let verification =
        verify_planned_prefixes(&mut reader, &source, core_plan.as_ref(), &mut stats)?;
    apply_prefix_verification(core_plan.as_mut(), verification.core_valid, &source);

    let scan_offset = core_plan
        .as_ref()
        .map(|plan| plan.checkpoint.complete_offset)
        .unwrap_or(0);
    let scan_ordinal = core_plan
        .as_ref()
        .map(|plan| plan.checkpoint.next_ordinal)
        .unwrap_or(0);
    let stream_hasher = verification
        .states
        .into_iter()
        .find(|(offset, _)| *offset == scan_offset)
        .map(|(_, hasher)| hasher)
        .unwrap_or_else(initial_prefix_hasher);
    let header = verification
        .headers
        .into_iter()
        .filter(|(offset, _)| *offset <= scan_offset)
        .max_by_key(|(offset, _)| *offset)
        .map(|(_, header)| header);
    reader
        .seek(SeekFrom::Start(scan_offset))
        .map_err(|source_error| PiNativePathError::Io {
            path: path.to_path_buf(),
            source: source_error,
        })?;

    let core = core_plan.map(|plan| {
        let current = current_checkpoint_for_plan(&plan, &source);
        CoreLane {
            activate_at: plan.checkpoint.complete_offset,
            active: plan.checkpoint.complete_offset == scan_offset,
            emitted: plan.checkpoint,
            current,
            lifecycle: plan.lifecycle,
            builder: PiCorePageBuilder::default(),
        }
    });

    Ok(PiNativeOpenOutcome::Ready(Box::new(PiNativeScanner {
        reader,
        source,
        context: options.context,
        source_revision,
        core,
        header,
        stream_hasher,
        scan_offset,
        scan_ordinal,
        pending: None,
        ready_core: None,
        eof: false,
        complete: false,
        finished: false,
        stats,
    })))
}

impl PiNativeScanner {
    pub(crate) fn source_revision(&self) -> &str {
        &self.source_revision
    }

    pub(crate) fn opened_source(&self) -> Arc<OpenedProviderSourceFile> {
        self.source.opened()
    }

    pub(crate) fn revalidate_source(&self) -> Result<(), PiNativePathError> {
        self.source.fence(self.reader.get_ref())
    }

    pub(crate) fn provider_session_id(&self) -> Option<&str> {
        self.header.as_ref().map(|header| header.id.as_str())
    }

    pub(crate) fn session_cwd(&self) -> Option<&str> {
        self.header
            .as_ref()
            .and_then(|header| header.cwd.as_deref())
    }

    pub(crate) fn parent_provider_session_id(&self) -> Option<&str> {
        self.header
            .as_ref()
            .and_then(|header| header.parent_session.as_deref())
    }

    pub(crate) fn next_page(
        &mut self,
    ) -> Result<Option<NativeIngestionPage<PiNativeCorePage>>, PiNativePathError> {
        loop {
            if self.ready_core.is_some() {
                self.fence_before_exposure()?;
                if let Some(page) = self.ready_core.take() {
                    self.observe_ready_bytes();
                    return Ok(Some(page));
                }
            }
            if self.finished {
                return Ok(None);
            }
            if let Some(pending) = self.pending.take() {
                if self.pending_requires_flush(&pending)? {
                    self.pending = Some(pending);
                    continue;
                }
                self.commit_pending(pending)?;
                self.flush_full_lanes()?;
                continue;
            }
            if self.eof {
                self.queue_terminal_pages()?;
                self.finished = self.ready_core.is_none()
                    && self
                        .core
                        .as_ref()
                        .is_none_or(|lane| lane.emitted == lane.current);
                continue;
            }
            self.activate_lanes_at_current_offset()?;
            let byte_start = self.scan_offset;
            let hasher_before = self.stream_hasher.clone();
            let line = read_bounded_line(
                &mut self.reader,
                &mut self.stream_hasher,
                MAX_PROVIDER_JSONL_LINE_BYTES,
            )
            .map_err(|source| PiNativePathError::Io {
                path: self.source.path.clone(),
                source,
            })?;
            self.stats.source_bytes_read = self
                .stats
                .source_bytes_read
                .saturating_add(line.observed_bytes);
            if line.observed_bytes == 0 {
                self.eof = true;
                self.complete = self.scan_offset == self.source.len;
                continue;
            }
            if !line.terminated {
                self.stream_hasher = hasher_before;
                self.stats.incomplete_tail_bytes = line.observed_bytes;
                self.eof = true;
                self.complete = false;
                continue;
            }
            let byte_end_exclusive = byte_start
                .checked_add(line.observed_bytes)
                .ok_or(PiNativePathError::PositionOverflow)?;
            let next_ordinal = self
                .scan_ordinal
                .checked_add(1)
                .ok_or(PiNativePathError::PositionOverflow)?;
            let checkpoint = PiNativeCheckpoint {
                parser_revision: PI_NATIVEPATH_PARSER_REVISION,
                policy_revision: PI_NATIVEPATH_POLICY_REVISION,
                route_sha256: self.source.route_sha256,
                physical_file_id: self.source.physical_file_id,
                observed_file_len: self.source.len,
                complete_offset: byte_end_exclusive,
                next_ordinal,
                committed_prefix_sha256: prefix_digest(&self.stream_hasher),
                terminal: false,
            };
            self.scan_offset = byte_end_exclusive;
            let ordinal = self.scan_ordinal;
            self.scan_ordinal = next_ordinal;
            self.stats.complete_records = self.stats.complete_records.saturating_add(1);
            self.stats.parsed_delta_bytes = self
                .stats
                .parsed_delta_bytes
                .saturating_add(line.observed_bytes);
            let pending = if line.oversized {
                self.stats.oversized_records = self.stats.oversized_records.saturating_add(1);
                self.oversized_pending(ordinal, byte_start, byte_end_exclusive, checkpoint)?
            } else {
                self.stats.semantic_records_parsed =
                    self.stats.semantic_records_parsed.saturating_add(1);
                self.parse_pending(
                    ordinal,
                    byte_start,
                    byte_end_exclusive,
                    json_record_bytes(&line.bytes),
                    checkpoint,
                )?
            };
            self.pending = Some(pending);
        }
    }

    pub(crate) fn outcome(&self) -> Option<PiNativeScanOutcome> {
        self.finished.then(|| PiNativeScanOutcome {
            complete: self.complete,
            core_lifecycle: self.core.as_ref().map(|lane| lane.lifecycle),
            core_checkpoint: self.core.as_ref().map(|lane| lane.emitted.clone()),
            stats: self.stats.clone(),
        })
    }

    fn activate_lanes_at_current_offset(&mut self) -> Result<(), PiNativePathError> {
        if let Some(lane) = self.core.as_mut() {
            if !lane.active && lane.activate_at == self.scan_offset {
                if lane.current.committed_prefix_sha256 != prefix_digest(&self.stream_hasher) {
                    return Err(PiNativePathError::SourceChanged);
                }
                lane.active = true;
            }
        }
        Ok(())
    }
}
