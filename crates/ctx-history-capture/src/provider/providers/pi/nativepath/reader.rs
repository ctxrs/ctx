use std::{
    fs,
    io::{self, BufRead, BufReader, Seek, SeekFrom},
    path::Path,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventType};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    fnv1a64,
    provider::{
        file_touches::{
            visit_provider_file_touch_drafts_with_limit, MAX_PACKED_PROVIDER_EVENT_INDEX,
        },
        importer::{provider_path_identity, provider_source_cursor_stream_for_path},
        native_ingestion::{
            NativeIngestionPage, NativePageAccounting, NativeProOutputPage, NativeProReplayPage,
            NativeSafeFrontier, NativeSourceIdentity,
        },
        normalization::{
            provider_capped_json, provider_policy_body, provider_policy_event_text,
            provider_result_identifier_evidence, provider_result_outcome_evidence,
        },
    },
    CaptureError, OutputAssociations, OutputCommandContext, OutputNativeCoordinate,
    OutputObservationKind, OutputOutcome, OutputOutcomeMetadata, OutputSourceIdentity,
    OutputSourceLocator, ProOutputObservation, ProOutputSourceDisposition, ProviderAdapterContext,
    MAX_PROVIDER_JSONL_LINE_BYTES, PROVIDER_MAX_PREVIEW_CHARS,
};

use super::{
    super::{
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

const PI_OUTPUT_PAGE_ENCODING_RESERVE: usize = 64 * 1024;
const PI_OUTPUT_BODY_MAX_BYTES: usize =
    PI_NATIVE_PAGE_MAX_BYTES - (2 * PI_OUTPUT_PAGE_ENCODING_RESERVE);
const PI_CORE_TOUCH_LIMIT: usize = PI_NATIVE_PAGE_MAX_UNITS - 1;
const PI_OUTPUT_MATERIALIZER_REVISION: &str = "pi-output-materializer-v1";
const PI_OUTPUT_LOCATOR_KIND: &str = "jsonl-source-item-byte-range-v1";

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

fn pi_native_event_type(entry_type: &str, message: Option<&Value>) -> EventType {
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

fn pi_output_outcome(entry: &Value, event_type: EventType) -> OutputOutcomeMetadata {
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
        OutputOutcome::Timeout
    } else {
        match classified.as_str() {
            Some("success") => OutputOutcome::Success,
            Some("failure") => OutputOutcome::Failure,
            _ => OutputOutcome::Unknown,
        }
    };
    OutputOutcomeMetadata {
        outcome,
        exit_code,
        duration_ms,
    }
}

fn pi_output_native_record_id(entry: &Value, line_number: usize) -> String {
    entry
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| valid_pi_output_token(value, 4 * 1024))
        .map(str::to_owned)
        .unwrap_or_else(|| format!("line-{line_number}"))
}

fn pi_output_call_id(entry: &Value) -> Option<String> {
    entry
        .get("message")
        .and_then(|message| message.get("toolCallId").or_else(|| message.get("callId")))
        .and_then(Value::as_str)
        .filter(|value| valid_pi_output_token(value, 256))
        .map(str::to_owned)
}

fn pi_output_command_context(
    entry: &Value,
    header: &PiNativeSessionHeader,
) -> Option<OutputCommandContext> {
    let message = entry.get("message")?;
    if message.get("role").and_then(Value::as_str) != Some("bashExecution") {
        return None;
    }
    let command = message
        .get("command")
        .and_then(Value::as_str)
        .filter(|command| !command.is_empty() && command.len() <= 64 * 1024)
        .filter(|command| !command.contains('\0'))?;
    Some(OutputCommandContext {
        tool_name: "bash".to_owned(),
        command: command.to_owned(),
        working_directory: header.cwd.clone(),
    })
}

fn valid_pi_output_token(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        })
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PiNativeProfile {
    CoreOnly,
    CoreAndPro,
    #[allow(dead_code)]
    ProReplayOnly,
}

impl PiNativeProfile {
    fn includes_core(self) -> bool {
        matches!(self, Self::CoreOnly | Self::CoreAndPro)
    }

    fn includes_output(self) -> bool {
        matches!(self, Self::CoreAndPro | Self::ProReplayOnly)
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PiNativeResume {
    pub(crate) core: Option<PiNativeCheckpoint>,
    pub(crate) output: Option<PiNativeCheckpoint>,
}

#[derive(Clone, Debug)]
pub(crate) struct PiNativeScanOptions {
    pub(crate) context: ProviderAdapterContext,
    pub(crate) profile: PiNativeProfile,
    pub(crate) resume: PiNativeResume,
    pub(crate) inventory_generation: u64,
    pub(crate) output_source_epoch: u64,
    pub(crate) rewrite_output_source_epoch: u64,
    pub(crate) expected_prior_output_source_epoch: Option<u64>,
    pub(crate) expected_prior_output_frontier: Option<NativeSafeFrontier>,
    pub(crate) force_output_rewrite: bool,
    pub(crate) output_materializer_revision: String,
}

impl PiNativeScanOptions {
    pub(crate) fn new(context: ProviderAdapterContext, profile: PiNativeProfile) -> Self {
        Self {
            context,
            profile,
            resume: PiNativeResume::default(),
            inventory_generation: 0,
            output_source_epoch: 0,
            rewrite_output_source_epoch: 0,
            expected_prior_output_source_epoch: None,
            expected_prior_output_frontier: None,
            force_output_rewrite: false,
            output_materializer_revision: PI_OUTPUT_MATERIALIZER_REVISION.to_owned(),
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
    pub(crate) pro_result_body_bytes: u64,
    pub(crate) successful_or_unknown_core_bodies: u64,
    pub(crate) successful_or_unknown_core_hashes: u64,
    pub(crate) successful_or_unknown_core_previews: u64,
    pub(crate) successful_or_unknown_core_touches: u64,
    pub(crate) successful_or_unknown_core_fts_documents: u64,
    pub(crate) core_pages: u64,
    pub(crate) output_pages: u64,
    pub(crate) peak_core_page_units: usize,
    pub(crate) peak_core_page_bytes: usize,
    pub(crate) peak_output_page_units: usize,
    pub(crate) peak_output_page_bytes: usize,
    pub(crate) peak_ready_page_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PiNativeScanOutcome {
    pub(crate) complete: bool,
    pub(crate) core_lifecycle: Option<PiSourceLifecycle>,
    pub(crate) output_lifecycle: Option<PiSourceLifecycle>,
    pub(crate) core_checkpoint: Option<PiNativeCheckpoint>,
    pub(crate) output_checkpoint: Option<PiNativeCheckpoint>,
    pub(crate) stats: PiNativeScanStats,
}

pub(crate) enum PiNativeOpenOutcome {
    Ready(Box<PiNativeScanner>),
    Deleted,
}

#[derive(Debug)]
pub(crate) enum PiNativeOwnedPage {
    Core(NativeIngestionPage<PiNativeCorePage>),
    Output(Box<NativeProReplayPage>),
}

struct LanePlan {
    checkpoint: PiNativeCheckpoint,
    lifecycle: PiSourceLifecycle,
    verify_prefix: bool,
}

struct CoreLane {
    published: PiNativeCheckpoint,
    current: PiNativeCheckpoint,
    activate_at: u64,
    active: bool,
    lifecycle: PiSourceLifecycle,
    builder: PiCorePageBuilder,
}

struct OutputLane {
    published: PiNativeCheckpoint,
    current: PiNativeCheckpoint,
    activate_at: u64,
    active: bool,
    lifecycle: PiSourceLifecycle,
    observations: Vec<ProOutputObservation>,
    estimated_bytes: usize,
    disposition: ProOutputSourceDisposition,
    expected_prior_source_epoch: Option<u64>,
    expected_prior_frontier: Option<NativeSafeFrontier>,
}

struct PendingRecord {
    core_units: Vec<PiNativeCoreUnit>,
    core_encoded_bytes: usize,
    output: Option<ProOutputObservation>,
    output_estimated_bytes: usize,
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
    native_source_identity: NativeSourceIdentity,
    output_source_identity: OutputSourceIdentity,
    locator_source_item: Vec<u8>,
    core: Option<CoreLane>,
    output: Option<OutputLane>,
    header: Option<PiNativeSessionHeader>,
    stream_hasher: Sha256,
    scan_offset: u64,
    scan_ordinal: u64,
    pending: Option<PendingRecord>,
    ready_core: Option<NativeIngestionPage<PiNativeCorePage>>,
    ready_output: Option<NativeProReplayPage>,
    eof: bool,
    complete: bool,
    finished: bool,
    inventory_generation: u64,
    output_source_epoch: u64,
    output_materializer_revision: String,
    stats: PiNativeScanStats,
    #[cfg(test)]
    before_exposure: Option<Box<dyn FnMut()>>,
}

pub(crate) fn open_pi_native_session(
    path: &Path,
    options: PiNativeScanOptions,
) -> Result<PiNativeOpenOutcome, PiNativePathError> {
    let (file, source) = match PiFrozenSource::open(path) {
        Ok(source) => source,
        Err(PiNativePathError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(PiNativeOpenOutcome::Deleted);
        }
        Err(error) => return Err(error),
    };
    let source_revision = source.source_revision();
    let cursor_path = options.context.source_path.as_deref().unwrap_or(path);
    let cursor_path_identity = provider_path_identity(cursor_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Pi,
        PI_SOURCE_FORMAT,
        &cursor_path_identity,
    );
    let canonical_identity = provider_path_identity(&source.canonical_path)?;
    let locator_source_item = canonical_identity.as_bytes().to_vec();
    let source_identity = format!("pi-jsonl-file:{canonical_identity}");
    let native_source_identity =
        NativeSourceIdentity::new(CaptureProvider::Pi.as_str(), source_identity.clone());
    let output_source_identity = OutputSourceIdentity {
        provider: CaptureProvider::Pi.as_str().to_owned(),
        namespace_id: cursor_stream,
        source_id: source_identity,
    };

    let mut core_plan = options
        .profile
        .includes_core()
        .then(|| plan_lane(options.resume.core.as_ref(), &source));
    let mut output_plan = options
        .profile
        .includes_output()
        .then(|| plan_lane(options.resume.output.as_ref(), &source));
    let mut reader = BufReader::new(file);
    let mut stats = PiNativeScanStats {
        source_file_opens: 1,
        ..PiNativeScanStats::default()
    };
    let verification = verify_planned_prefixes(
        &mut reader,
        &source,
        core_plan.as_ref(),
        output_plan.as_ref(),
        &mut stats,
    )?;
    apply_prefix_verification(core_plan.as_mut(), verification.core_valid, &source);
    apply_prefix_verification(output_plan.as_mut(), verification.output_valid, &source);

    let scan_offset = core_plan
        .as_ref()
        .map(|plan| plan.checkpoint.complete_offset)
        .into_iter()
        .chain(
            output_plan
                .as_ref()
                .map(|plan| plan.checkpoint.complete_offset),
        )
        .min()
        .unwrap_or(0);
    let scan_ordinal = core_plan
        .as_ref()
        .filter(|plan| plan.checkpoint.complete_offset == scan_offset)
        .map(|plan| plan.checkpoint.next_ordinal)
        .or_else(|| {
            output_plan
                .as_ref()
                .filter(|plan| plan.checkpoint.complete_offset == scan_offset)
                .map(|plan| plan.checkpoint.next_ordinal)
        })
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
            published: plan.checkpoint,
            current,
            lifecycle: plan.lifecycle,
            builder: PiCorePageBuilder::default(),
        }
    });
    let output_lifecycle = output_plan.as_ref().map(|plan| plan.lifecycle);
    let output_rewrite = options.force_output_rewrite
        || output_lifecycle.is_some_and(|lifecycle| {
            matches!(
                lifecycle,
                PiSourceLifecycle::Rewrite
                    | PiSourceLifecycle::Truncate
                    | PiSourceLifecycle::Replace
                    | PiSourceLifecycle::Copy
            )
        });
    let output_source_epoch = if output_rewrite {
        options.rewrite_output_source_epoch
    } else {
        options.output_source_epoch
    };
    let output = output_plan.map(|plan| {
        let expected_prior_frontier =
            options.expected_prior_output_frontier.clone().or_else(|| {
                options
                    .resume
                    .output
                    .as_ref()
                    .and_then(|checkpoint| checkpoint.safe_frontier().ok())
            });
        let disposition = if options.force_output_rewrite {
            ProOutputSourceDisposition::Rewrite
        } else {
            match plan.lifecycle {
                PiSourceLifecycle::Fresh | PiSourceLifecycle::Copy => {
                    ProOutputSourceDisposition::NewSource
                }
                PiSourceLifecycle::Rewrite
                | PiSourceLifecycle::Truncate
                | PiSourceLifecycle::Replace => ProOutputSourceDisposition::Rewrite,
                PiSourceLifecycle::NoOp
                | PiSourceLifecycle::Append
                | PiSourceLifecycle::Relocate => ProOutputSourceDisposition::AppendOrResume,
            }
        };
        let current = current_checkpoint_for_plan(&plan, &source);
        OutputLane {
            activate_at: plan.checkpoint.complete_offset,
            active: plan.checkpoint.complete_offset == scan_offset,
            published: plan.checkpoint,
            current,
            lifecycle: plan.lifecycle,
            observations: Vec::new(),
            estimated_bytes: 0,
            disposition,
            expected_prior_source_epoch: options.expected_prior_output_source_epoch,
            expected_prior_frontier,
        }
    });

    Ok(PiNativeOpenOutcome::Ready(Box::new(PiNativeScanner {
        reader,
        source,
        context: options.context,
        source_revision,
        native_source_identity,
        output_source_identity,
        locator_source_item,
        core,
        output,
        header,
        stream_hasher,
        scan_offset,
        scan_ordinal,
        pending: None,
        ready_core: None,
        ready_output: None,
        eof: false,
        complete: false,
        finished: false,
        inventory_generation: options.inventory_generation,
        output_source_epoch,
        output_materializer_revision: options.output_materializer_revision,
        stats,
        #[cfg(test)]
        before_exposure: None,
    })))
}

impl PiNativeScanner {
    pub(crate) fn source_revision(&self) -> &str {
        &self.source_revision
    }

    pub(crate) fn core_lifecycle(&self) -> Option<PiSourceLifecycle> {
        self.core.as_ref().map(|lane| lane.lifecycle)
    }

    pub(crate) fn next_page(&mut self) -> Result<Option<PiNativeOwnedPage>, PiNativePathError> {
        loop {
            if self.ready_core.is_some() || self.ready_output.is_some() {
                self.fence_before_exposure()?;
                if let Some(page) = self.ready_core.take() {
                    self.observe_ready_bytes();
                    return Ok(Some(PiNativeOwnedPage::Core(page)));
                }
                if let Some(page) = self.ready_output.take() {
                    self.observe_ready_bytes();
                    return Ok(Some(PiNativeOwnedPage::Output(Box::new(page))));
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
                    && self.ready_output.is_none()
                    && self
                        .core
                        .as_ref()
                        .is_none_or(|lane| lane.published == lane.current)
                    && self
                        .output
                        .as_ref()
                        .is_none_or(|lane| lane.published == lane.current);
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
            output_lifecycle: self.output.as_ref().map(|lane| lane.lifecycle),
            core_checkpoint: self.core.as_ref().map(|lane| lane.published.clone()),
            output_checkpoint: self.output.as_ref().map(|lane| lane.published.clone()),
            stats: self.stats.clone(),
        })
    }

    #[cfg(test)]
    pub(super) fn set_before_exposure(&mut self, hook: impl FnMut() + 'static) {
        self.before_exposure = Some(Box::new(hook));
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
        if let Some(lane) = self.output.as_mut() {
            if !lane.active && lane.activate_at == self.scan_offset {
                if lane.current.committed_prefix_sha256 != prefix_digest(&self.stream_hasher) {
                    return Err(PiNativePathError::SourceChanged);
                }
                lane.active = true;
            }
        }
        Ok(())
    }

    fn parse_pending(
        &mut self,
        ordinal: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
        bytes: &[u8],
        checkpoint: PiNativeCheckpoint,
    ) -> Result<PendingRecord, PiNativePathError> {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(PendingRecord {
                core_units: Vec::new(),
                core_encoded_bytes: 0,
                output: None,
                output_estimated_bytes: 0,
                checkpoint,
            });
        }
        let line_number = ordinal.saturating_add(1);
        let value = match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => value,
            Err(error) => {
                self.stats.malformed_records = self.stats.malformed_records.saturating_add(1);
                return self.rejection_pending(
                    PiNativeRejectionKind::MalformedJson,
                    ordinal,
                    line_number,
                    byte_start,
                    byte_end_exclusive,
                    error.to_string(),
                    checkpoint,
                );
            }
        };
        let entry_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if entry_type == "session" {
            return match parse_pi_session_header(value) {
                Ok(header) => {
                    let row = self
                        .core_is_active()
                        .then(|| self.session_row(&header))
                        .transpose()?;
                    self.header = Some(header);
                    self.core_units_pending(
                        row.into_iter().map(PiNativeCoreUnit::Session).collect(),
                        checkpoint,
                    )
                }
                Err(error) => self.rejection_pending(
                    PiNativeRejectionKind::InvalidHeader,
                    ordinal,
                    line_number,
                    byte_start,
                    byte_end_exclusive,
                    error.to_string(),
                    checkpoint,
                ),
            };
        }

        let event_type = pi_native_event_type(entry_type, value.get("message"));
        if matches!(event_type, EventType::ToolOutput | EventType::CommandOutput) {
            return self.output_pending(
                &value,
                event_type,
                ordinal,
                line_number,
                byte_start,
                byte_end_exclusive,
                checkpoint,
            );
        }
        let Some(header) = self.header.as_ref() else {
            return self.rejection_pending(
                PiNativeRejectionKind::BeforeHeader,
                ordinal,
                line_number,
                byte_start,
                byte_end_exclusive,
                "pi session entry appeared before session header",
                checkpoint,
            );
        };
        if !self.core_is_active() {
            return self.core_units_pending(Vec::new(), checkpoint);
        }
        let mut units = match self.event_and_touches(
            header,
            &value,
            ordinal,
            line_number,
            byte_start,
            byte_end_exclusive,
            None,
        ) {
            Ok(units) => units,
            Err(error) => {
                return self.rejection_pending(
                    PiNativeRejectionKind::InvalidRecord,
                    ordinal,
                    line_number,
                    byte_start,
                    byte_end_exclusive,
                    error.to_string(),
                    checkpoint,
                );
            }
        };
        self.bound_core_units(
            &mut units,
            ordinal,
            line_number,
            byte_start,
            byte_end_exclusive,
            checkpoint,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn output_pending(
        &mut self,
        entry: &Value,
        event_type: EventType,
        ordinal: u64,
        line_number: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
        checkpoint: PiNativeCheckpoint,
    ) -> Result<PendingRecord, PiNativePathError> {
        self.stats.native_result_records = self.stats.native_result_records.saturating_add(1);
        let Some(header) = self.header.as_ref() else {
            return self.rejection_pending(
                PiNativeRejectionKind::BeforeHeader,
                ordinal,
                line_number,
                byte_start,
                byte_end_exclusive,
                "pi session entry appeared before session header",
                checkpoint,
            );
        };
        let outcome = pi_output_outcome(entry, event_type);
        let occurred_at = match entry
            .get("timestamp")
            .and_then(Value::as_str)
            .ok_or_else(|| "pi session event missing timestamp".to_owned())
            .and_then(|timestamp| {
                chrono::DateTime::parse_from_rfc3339(timestamp)
                    .map(|timestamp| timestamp.with_timezone(&Utc))
                    .map_err(|error| error.to_string())
            }) {
            Ok(occurred_at) => occurred_at,
            Err(error) => {
                return self.rejection_pending(
                    PiNativeRejectionKind::InvalidRecord,
                    ordinal,
                    line_number,
                    byte_start,
                    byte_end_exclusive,
                    error,
                    checkpoint,
                );
            }
        };
        match outcome.outcome {
            OutputOutcome::Success => {
                self.stats.native_result_success =
                    self.stats.native_result_success.saturating_add(1)
            }
            OutputOutcome::Failure => {
                self.stats.native_result_failure =
                    self.stats.native_result_failure.saturating_add(1)
            }
            OutputOutcome::Timeout => {
                self.stats.native_result_timeout =
                    self.stats.native_result_timeout.saturating_add(1)
            }
            OutputOutcome::Unknown => {
                self.stats.native_result_unknown =
                    self.stats.native_result_unknown.saturating_add(1)
            }
        }
        let explicit_failure = matches!(
            outcome.outcome,
            OutputOutcome::Failure | OutputOutcome::Timeout
        );
        let command = pi_output_command_context(entry, header);
        let retained_failure =
            explicit_failure && (event_type != EventType::CommandOutput || command.is_some());
        let wants_output = self.output.as_ref().is_some_and(|lane| lane.active);
        let wants_core = self.core_is_active();
        let result_body = (wants_output || retained_failure)
            .then(|| {
                self.stats.result_body_extractions =
                    self.stats.result_body_extractions.saturating_add(1);
                pi_result_content(entry)
            })
            .flatten();
        let mut output = None;
        let mut output_estimated_bytes = 0;
        if wants_output {
            if let Some(content) = result_body.as_ref() {
                if content.len() <= PI_OUTPUT_BODY_MAX_BYTES {
                    let observation = output_observation(
                        header,
                        entry,
                        event_type,
                        ordinal,
                        line_number,
                        byte_start,
                        byte_end_exclusive,
                        &self.locator_source_item,
                        occurred_at,
                        command.clone(),
                        outcome.clone(),
                        content,
                    )?;
                    output_estimated_bytes = output_estimated_bytes_for(&observation);
                    if PI_OUTPUT_PAGE_ENCODING_RESERVE.saturating_add(output_estimated_bytes)
                        <= PI_NATIVE_PAGE_MAX_BYTES
                    {
                        self.stats.pro_result_body_bytes = self
                            .stats
                            .pro_result_body_bytes
                            .saturating_add(u64::try_from(content.len()).unwrap_or(u64::MAX));
                        output = Some(observation);
                    } else {
                        self.stats.oversized_records =
                            self.stats.oversized_records.saturating_add(1);
                        output_estimated_bytes = 0;
                    }
                } else {
                    self.stats.oversized_records = self.stats.oversized_records.saturating_add(1);
                }
            }
        }

        let mut core_units = if wants_core && retained_failure {
            match self.event_and_touches(
                header,
                entry,
                ordinal,
                line_number,
                byte_start,
                byte_end_exclusive,
                Some((&outcome, result_body.as_deref())),
            ) {
                Ok(units) => units,
                Err(error) => {
                    return self.rejection_pending(
                        PiNativeRejectionKind::InvalidRecord,
                        ordinal,
                        line_number,
                        byte_start,
                        byte_end_exclusive,
                        error.to_string(),
                        checkpoint,
                    );
                }
            }
        } else {
            Vec::new()
        };
        if !retained_failure {
            debug_assert!(core_units.is_empty());
            debug_assert_eq!(self.stats.successful_or_unknown_core_bodies, 0);
            debug_assert_eq!(self.stats.successful_or_unknown_core_hashes, 0);
            debug_assert_eq!(self.stats.successful_or_unknown_core_previews, 0);
            debug_assert_eq!(self.stats.successful_or_unknown_core_touches, 0);
            debug_assert_eq!(self.stats.successful_or_unknown_core_fts_documents, 0);
        }
        let core_encoded_bytes = self.bound_core_units_encoded(
            &mut core_units,
            ordinal,
            line_number,
            byte_start,
            byte_end_exclusive,
        )?;
        Ok(PendingRecord {
            core_units,
            core_encoded_bytes,
            output,
            output_estimated_bytes,
            checkpoint,
        })
    }

    fn session_row(
        &self,
        header: &PiNativeSessionHeader,
    ) -> Result<PiNativeSessionRow, PiNativePathError> {
        Ok(PiNativeSessionRow {
            provider_session_id: header.id.clone(),
            version: header.version,
            started_at: header.timestamp,
            cwd: header.cwd.clone(),
            parent_session: header.parent_session.clone(),
            source_metadata: json!({
                "adapter": PI_SOURCE_FORMAT,
                "source_fidelity": "documented_session_jsonl",
            }),
            session_metadata: json!({
                "source_format": PI_SOURCE_FORMAT,
                "source_fidelity": "documented_session_jsonl",
                "version": header.version,
                "parent_session": header.parent_session,
                "header": header.raw,
                "limitations": [
                    "message branch parentId values are preserved as event metadata, not ctx session edges",
                    "files touched are available only when Pi message payloads include them",
                    "raw image content is not expanded into artifacts by this importer"
                ],
            }),
            source_idempotency_key: format!("provider-source:pi:{PI_SOURCE_FORMAT}:{}", header.id),
            session_idempotency_key: format!("provider-session:pi:{}", header.id),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn event_and_touches(
        &self,
        header: &PiNativeSessionHeader,
        entry: &Value,
        ordinal: u64,
        line_number: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
        failure: Option<(
            &crate::provider::importer::OutputOutcomeMetadata,
            Option<&str>,
        )>,
    ) -> Result<Vec<PiNativeCoreUnit>, PiNativePathError> {
        let line_number_usize =
            usize::try_from(line_number).map_err(|_| PiNativePathError::PositionOverflow)?;
        let entry_type = entry
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let message = entry.get("message");
        let message_role = message
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str);
        let occurred_at = entry
            .get("timestamp")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                PiNativePathError::Normalization(CaptureError::InvalidPayload(
                    "pi session event missing timestamp".to_owned(),
                ))
            })
            .and_then(|timestamp| {
                DateTime::parse_from_rfc3339(timestamp)
                    .map(|time| time.with_timezone(&Utc))
                    .map_err(CaptureError::from)
                    .map_err(PiNativePathError::from)
            })?;
        let event_type = pi_native_event_type(entry_type, message);
        let role = message_role.map(pi_event_role);
        let mut payload = pi_native_event_payload(entry, event_type);
        if let Some((outcome, content)) = failure {
            let payload = payload.as_object_mut().ok_or_else(|| {
                PiNativePathError::Normalization(CaptureError::SystemInvariant(
                    "Pi failure event payload must be an object",
                ))
            })?;
            payload.insert("result_outcome".to_owned(), json!("failure"));
            payload.insert(
                "timed_out".to_owned(),
                json!(outcome.outcome == OutputOutcome::Timeout),
            );
            if let Some(exit_code) = outcome.exit_code {
                payload.insert("exit_code".to_owned(), json!(exit_code));
            }
            if let Some(duration_ms) = outcome.duration_ms {
                payload.insert("duration_ms".to_owned(), json!(duration_ms));
            }
            if let Some(content) = content {
                payload.insert("output_bytes".to_owned(), json!(content.len()));
                let (preview, _) = crate::provider::normalization::provider_local_preview(
                    content,
                    PROVIDER_MAX_PREVIEW_CHARS,
                );
                if !preview.trim().is_empty() {
                    payload.insert("output_preview".to_owned(), Value::String(preview));
                }
            }
        }
        let provider_event_identity_index =
            pi_provider_event_identity_index(header, entry).unwrap_or(ordinal);
        let locator = PiNativePhysicalLocator {
            path: self.source.path.clone(),
            source_record_ordinal: ordinal,
            line_number,
            byte_start,
            byte_end_exclusive,
        };
        let event_row = PiNativeEventRow {
            provider_session_id: header.id.clone(),
            provider_event_index: ordinal,
            provider_event_identity_index,
            cursor: entry.get("id").and_then(Value::as_str).map(str::to_owned),
            event_type,
            role,
            occurred_at,
            idempotency_key: pi_event_idempotency_key(header, entry, line_number_usize),
            payload,
            metadata: json!({
                "source": "pi_session",
                "source_format": PI_SOURCE_FORMAT,
                "line": line_number,
                "entry_type": entry_type,
                "entry_id": entry.get("id").and_then(Value::as_str),
                "parent_id": entry.get("parentId").and_then(Value::as_str),
                "provider_event_identity_index": provider_event_identity_index,
                "message_role": message_role,
                "model": message
                    .and_then(|message| message.get("model"))
                    .and_then(Value::as_str),
                "provider": message
                    .and_then(|message| message.get("provider"))
                    .and_then(Value::as_str),
                "usage": message.and_then(|message| message.get("usage")).cloned(),
            }),
            locator,
        };
        let mut units = vec![PiNativeCoreUnit::Event(event_row)];
        let provider_touch_base_index = ordinal
            .checked_shl(16)
            .ok_or(PiNativePathError::PositionOverflow)?;
        let raw_source_path = self
            .context
            .source_path
            .as_ref()
            .map(|path| path.display().to_string());
        let source_root = self.context.source_root_display();
        let occurred_at = event_row_occurred_at(&units)?;
        let outcome = visit_provider_file_touch_drafts_with_limit(
            entry,
            false,
            PI_CORE_TOUCH_LIMIT,
            |(touch_ordinal, touch)| {
                let provider_touch_index = if ordinal > MAX_PACKED_PROVIDER_EVENT_INDEX {
                    touch_ordinal
                } else {
                    provider_touch_base_index | touch_ordinal
                };
                units.push(PiNativeCoreUnit::FileTouch(PiNativeFileTouchRow {
                    provider_session_id: header.id.clone(),
                    provider_touch_index,
                    provider_event_index: Some(ordinal),
                    raw_source_path: raw_source_path.clone(),
                    source_root: source_root.clone(),
                    path: touch.path,
                    change_kind: touch.change_kind,
                    old_path: touch.old_path,
                    line_count_delta: None,
                    confidence: touch.confidence,
                    occurred_at,
                    source_format: PI_SOURCE_FORMAT.to_owned(),
                    metadata: touch.metadata,
                }));
                Ok::<(), PiNativePathError>(())
            },
        )?;
        if outcome.limit_exceeded() {
            return Err(PiNativePathError::InvalidSource {
                path: self.source.path.clone(),
                reason: "Pi normalized record exceeds the NativePath Core unit limit".to_owned(),
            });
        }
        Ok(units)
    }

    fn core_units_pending(
        &self,
        units: Vec<PiNativeCoreUnit>,
        checkpoint: PiNativeCheckpoint,
    ) -> Result<PendingRecord, PiNativePathError> {
        let encoded_bytes = core_units_encoded_bytes(&units)?;
        Ok(PendingRecord {
            core_units: units,
            core_encoded_bytes: encoded_bytes,
            output: None,
            output_estimated_bytes: 0,
            checkpoint,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn bound_core_units(
        &self,
        units: &mut Vec<PiNativeCoreUnit>,
        ordinal: u64,
        line_number: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
        checkpoint: PiNativeCheckpoint,
    ) -> Result<PendingRecord, PiNativePathError> {
        let encoded_bytes = self.bound_core_units_encoded(
            units,
            ordinal,
            line_number,
            byte_start,
            byte_end_exclusive,
        )?;
        Ok(PendingRecord {
            core_units: std::mem::take(units),
            core_encoded_bytes: encoded_bytes,
            output: None,
            output_estimated_bytes: 0,
            checkpoint,
        })
    }

    fn bound_core_units_encoded(
        &self,
        units: &mut Vec<PiNativeCoreUnit>,
        ordinal: u64,
        line_number: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
    ) -> Result<usize, PiNativePathError> {
        let encoded_bytes = core_units_encoded_bytes(units)?;
        if units.len() <= PI_NATIVE_PAGE_MAX_UNITS
            && PI_NATIVE_PAGE_ENCODING_RESERVE.saturating_add(encoded_bytes)
                <= PI_NATIVE_PAGE_MAX_BYTES
        {
            return Ok(encoded_bytes);
        }
        let kind = if units.len() > PI_NATIVE_PAGE_MAX_UNITS {
            PiNativeRejectionKind::TooManyCoreUnits
        } else {
            PiNativeRejectionKind::OversizedCoreUnit
        };
        *units = vec![PiNativeCoreUnit::Rejection(PiNativeRejection::new(
            kind,
            ordinal,
            line_number,
            byte_start,
            byte_end_exclusive,
            "Pi normalized record exceeds the bounded NativePath Core page",
        ))];
        Ok(core_units_encoded_bytes(units)?)
    }

    #[allow(clippy::too_many_arguments)]
    fn rejection_pending(
        &self,
        kind: PiNativeRejectionKind,
        ordinal: u64,
        line_number: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
        diagnostic: impl AsRef<str>,
        checkpoint: PiNativeCheckpoint,
    ) -> Result<PendingRecord, PiNativePathError> {
        let units = self.core_is_active().then(|| {
            PiNativeCoreUnit::Rejection(PiNativeRejection::new(
                kind,
                ordinal,
                line_number,
                byte_start,
                byte_end_exclusive,
                diagnostic,
            ))
        });
        self.core_units_pending(units.into_iter().collect(), checkpoint)
    }

    fn oversized_pending(
        &self,
        ordinal: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
        checkpoint: PiNativeCheckpoint,
    ) -> Result<PendingRecord, PiNativePathError> {
        self.rejection_pending(
            PiNativeRejectionKind::OversizedRecord,
            ordinal,
            ordinal.saturating_add(1),
            byte_start,
            byte_end_exclusive,
            format!("Pi JSONL record exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit"),
            checkpoint,
        )
    }

    fn pending_requires_flush(
        &mut self,
        pending: &PendingRecord,
    ) -> Result<bool, PiNativePathError> {
        let core_needs_flush = self.core.as_ref().is_some_and(|lane| {
            lane.active
                && !lane.builder.is_empty()
                && !lane
                    .builder
                    .can_push(&pending.core_units, pending.core_encoded_bytes)
        });
        let output_needs_flush = self.output.as_ref().is_some_and(|lane| {
            lane.active
                && !lane.observations.is_empty()
                && pending.output.is_some()
                && (lane.observations.len() == PI_NATIVE_PAGE_MAX_UNITS
                    || PI_OUTPUT_PAGE_ENCODING_RESERVE
                        .saturating_add(lane.estimated_bytes)
                        .saturating_add(pending.output_estimated_bytes)
                        > PI_NATIVE_PAGE_MAX_BYTES)
        });
        if core_needs_flush {
            self.finish_core_page(false)?;
        }
        if output_needs_flush {
            self.finish_output_page(false)?;
        }
        Ok(core_needs_flush || output_needs_flush)
    }

    fn commit_pending(&mut self, pending: PendingRecord) -> Result<(), PiNativePathError> {
        if let Some(lane) = self.core.as_mut().filter(|lane| lane.active) {
            if !lane
                .builder
                .can_push(&pending.core_units, pending.core_encoded_bytes)
            {
                return Err(PiNativePathError::Page(
                    "single Pi Core record exceeds the page bound".to_owned(),
                ));
            }
            lane.builder
                .push(pending.core_units, pending.core_encoded_bytes);
            lane.current = pending.checkpoint.clone();
        }
        if let Some(lane) = self.output.as_mut().filter(|lane| lane.active) {
            if let Some(output) = pending.output {
                let next = PI_OUTPUT_PAGE_ENCODING_RESERVE
                    .saturating_add(lane.estimated_bytes)
                    .saturating_add(pending.output_estimated_bytes);
                if lane.observations.len() == PI_NATIVE_PAGE_MAX_UNITS
                    || next > PI_NATIVE_PAGE_MAX_BYTES
                {
                    return Err(PiNativePathError::Page(
                        "single Pi output record exceeds the page bound".to_owned(),
                    ));
                }
                lane.estimated_bytes = lane
                    .estimated_bytes
                    .saturating_add(pending.output_estimated_bytes);
                lane.observations.push(output);
            }
            lane.current = pending.checkpoint;
        }
        Ok(())
    }

    fn flush_full_lanes(&mut self) -> Result<(), PiNativePathError> {
        let core_full = self
            .core
            .as_ref()
            .is_some_and(|lane| lane.builder.units.len() == PI_NATIVE_PAGE_MAX_UNITS);
        let output_full = self
            .output
            .as_ref()
            .is_some_and(|lane| lane.observations.len() == PI_NATIVE_PAGE_MAX_UNITS);
        if core_full {
            self.finish_core_page(false)?;
            return Ok(());
        }
        if output_full {
            self.finish_output_page(false)?;
        }
        Ok(())
    }

    fn queue_terminal_pages(&mut self) -> Result<(), PiNativePathError> {
        let terminal = self.complete;
        if let Some(lane) = self.core.as_mut().filter(|lane| lane.active) {
            lane.current.terminal = terminal;
        }
        if let Some(lane) = self.output.as_mut().filter(|lane| lane.active) {
            lane.current.terminal = terminal;
        }
        let core_changed = self
            .core
            .as_ref()
            .is_some_and(|lane| lane.active && lane.published != lane.current);
        let output_changed = self
            .output
            .as_ref()
            .is_some_and(|lane| lane.active && lane.published != lane.current);
        if core_changed {
            self.finish_core_page(terminal)?;
            return Ok(());
        }
        if output_changed {
            self.finish_output_page(terminal)?;
        }
        Ok(())
    }

    fn finish_core_page(&mut self, terminal: bool) -> Result<(), PiNativePathError> {
        if self.ready_core.is_some() {
            return Err(PiNativePathError::Page(
                "Pi scanner retained more than one ready Core page".to_owned(),
            ));
        }
        let lane = self
            .core
            .as_mut()
            .ok_or_else(|| PiNativePathError::Page("Pi Core lane is not enabled".to_owned()))?;
        let expected = lane.published.safe_frontier().map_err(page_error)?;
        let mut next_checkpoint = lane.current.clone();
        next_checkpoint.terminal = terminal;
        let next = next_checkpoint.safe_frontier().map_err(page_error)?;
        let core = lane.builder.take();
        let accounting = NativePageAccounting {
            logical_units: core.units.len(),
            conservative_serialized_bytes: PI_NATIVE_PAGE_ENCODING_RESERVE
                .saturating_add(core.encoded_bytes)
                .saturating_add(expected.bytes.len())
                .saturating_add(next.bytes.len()),
        };
        let page = NativeIngestionPage::new(expected, next, terminal, accounting, core)
            .map_err(page_error)?;
        self.stats.core_pages = self.stats.core_pages.saturating_add(1);
        self.stats.peak_core_page_units = self
            .stats
            .peak_core_page_units
            .max(page.accounting.logical_units);
        self.stats.peak_core_page_bytes = self
            .stats
            .peak_core_page_bytes
            .max(page.accounting.conservative_serialized_bytes);
        lane.current = next_checkpoint.clone();
        lane.published = next_checkpoint;
        self.ready_core = Some(page);
        self.observe_ready_bytes();
        Ok(())
    }

    fn finish_output_page(&mut self, terminal: bool) -> Result<(), PiNativePathError> {
        if self.ready_output.is_some() {
            return Err(PiNativePathError::Page(
                "Pi scanner retained more than one ready output page".to_owned(),
            ));
        }
        let lane = self
            .output
            .as_mut()
            .ok_or_else(|| PiNativePathError::Page("Pi output lane is not enabled".to_owned()))?;
        let expected = lane.published.safe_frontier().map_err(page_error)?;
        let mut next_checkpoint = lane.current.clone();
        next_checkpoint.terminal = terminal;
        let next = next_checkpoint.safe_frontier().map_err(page_error)?;
        let observations = std::mem::take(&mut lane.observations);
        let observation_count = observations.len();
        let output_bytes = std::mem::take(&mut lane.estimated_bytes);
        let output = NativeProOutputPage {
            inventory_generation: self.inventory_generation,
            source: self.output_source_identity.clone(),
            source_epoch: self.output_source_epoch,
            observed_revision: self.source_revision.clone(),
            parser_revision: format!(
                "pi-nativepath:{PI_NATIVEPATH_PARSER_REVISION}:{PI_NATIVEPATH_POLICY_REVISION}"
            ),
            materializer_revision: self.output_materializer_revision.clone(),
            disposition: lane.disposition,
            expected_prior_source_epoch: lane.expected_prior_source_epoch,
            expected_prior_frontier: lane.expected_prior_frontier.clone(),
            observations,
        };
        let accounting = NativePageAccounting {
            logical_units: observation_count,
            conservative_serialized_bytes: PI_OUTPUT_PAGE_ENCODING_RESERVE
                .saturating_add(output_bytes)
                .saturating_add(expected.bytes.len())
                .saturating_add(next.bytes.len()),
        };
        let page = NativeProReplayPage::new_with_source_identity(
            self.native_source_identity.clone(),
            expected,
            next,
            terminal,
            accounting,
            output,
        )
        .map_err(page_error)?;
        self.stats.output_pages = self.stats.output_pages.saturating_add(1);
        self.stats.peak_output_page_units = self
            .stats
            .peak_output_page_units
            .max(page.accounting.logical_units);
        self.stats.peak_output_page_bytes = self
            .stats
            .peak_output_page_bytes
            .max(page.accounting.conservative_serialized_bytes);
        lane.current = next_checkpoint.clone();
        lane.published = next_checkpoint.clone();
        lane.disposition = ProOutputSourceDisposition::AppendOrResume;
        lane.expected_prior_source_epoch = Some(self.output_source_epoch);
        lane.expected_prior_frontier = Some(next_checkpoint.safe_frontier().map_err(page_error)?);
        self.ready_output = Some(page);
        self.observe_ready_bytes();
        Ok(())
    }

    fn fence_before_exposure(&mut self) -> Result<(), PiNativePathError> {
        #[cfg(test)]
        if let Some(mut hook) = self.before_exposure.take() {
            hook();
        }
        self.source.fence(self.reader.get_ref())?;
        self.stats.source_fences = self.stats.source_fences.saturating_add(1);
        Ok(())
    }

    fn observe_ready_bytes(&mut self) {
        let bytes = self
            .ready_core
            .as_ref()
            .map_or(0, |page| page.accounting.conservative_serialized_bytes)
            .saturating_add(
                self.ready_output
                    .as_ref()
                    .map_or(0, |page| page.accounting.conservative_serialized_bytes),
            );
        self.stats.peak_ready_page_bytes = self.stats.peak_ready_page_bytes.max(bytes);
    }

    fn core_is_active(&self) -> bool {
        self.core.as_ref().is_some_and(|lane| lane.active)
    }
}

struct PrefixVerification {
    core_valid: bool,
    output_valid: bool,
    states: Vec<(u64, Sha256)>,
    headers: Vec<(u64, PiNativeSessionHeader)>,
}

fn verify_planned_prefixes(
    reader: &mut BufReader<fs::File>,
    source: &PiFrozenSource,
    core: Option<&LanePlan>,
    output: Option<&LanePlan>,
    stats: &mut PiNativeScanStats,
) -> Result<PrefixVerification, PiNativePathError> {
    let targets = [core, output]
        .into_iter()
        .flatten()
        .filter(|plan| plan.verify_prefix)
        .map(|plan| plan.checkpoint.complete_offset)
        .collect::<Vec<_>>();
    let max_target = targets.iter().copied().max().unwrap_or(0);
    let mut hasher = initial_prefix_hasher();
    let mut states = vec![(0, hasher.clone())];
    let mut headers = Vec::new();
    let mut offset = 0_u64;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|error| PiNativePathError::Io {
            path: source.path.clone(),
            source: error,
        })?;
    while offset < max_target {
        let line = read_bounded_line(reader, &mut hasher, MAX_PROVIDER_JSONL_LINE_BYTES).map_err(
            |error| PiNativePathError::Io {
                path: source.path.clone(),
                source: error,
            },
        )?;
        if line.observed_bytes == 0 || !line.terminated {
            break;
        }
        offset = offset
            .checked_add(line.observed_bytes)
            .ok_or(PiNativePathError::PositionOverflow)?;
        stats.prefix_bytes_hashed = stats
            .prefix_bytes_hashed
            .saturating_add(line.observed_bytes);
        if !line.oversized && might_be_session_header(&line.bytes) {
            if let Ok(value) = serde_json::from_slice::<Value>(json_record_bytes(&line.bytes)) {
                if value.get("type").and_then(Value::as_str) == Some("session") {
                    if let Ok(header) = parse_pi_session_header(value) {
                        stats.prefix_header_records_parsed =
                            stats.prefix_header_records_parsed.saturating_add(1);
                        headers.push((offset, header));
                    }
                }
            }
        }
        if targets.contains(&offset) {
            states.push((offset, hasher.clone()));
        }
        if offset > max_target {
            break;
        }
    }
    let valid = |plan: Option<&LanePlan>| {
        plan.is_some_and(|plan| {
            !plan.verify_prefix
                || states.iter().any(|(offset, hasher)| {
                    *offset == plan.checkpoint.complete_offset
                        && prefix_digest(hasher) == plan.checkpoint.committed_prefix_sha256
                })
        })
    };
    Ok(PrefixVerification {
        core_valid: core.is_none() || valid(core),
        output_valid: output.is_none() || valid(output),
        states,
        headers,
    })
}

fn plan_lane(previous: Option<&PiNativeCheckpoint>, source: &PiFrozenSource) -> LanePlan {
    let initial =
        || PiNativeCheckpoint::initial(source.route_sha256, source.physical_file_id, source.len);
    let Some(previous) = previous else {
        return LanePlan {
            checkpoint: initial(),
            lifecycle: PiSourceLifecycle::Fresh,
            verify_prefix: false,
        };
    };
    if !previous.revisions_match() {
        return LanePlan {
            checkpoint: initial(),
            lifecycle: PiSourceLifecycle::Rewrite,
            verify_prefix: false,
        };
    }
    let same_route = previous.route_sha256 == source.route_sha256;
    let same_physical = previous.physical_file_id == source.physical_file_id
        && (previous.physical_file_id.is_some() || same_route);
    if source.len < previous.complete_offset {
        return LanePlan {
            checkpoint: initial(),
            lifecycle: PiSourceLifecycle::Truncate,
            verify_prefix: false,
        };
    }
    if !same_physical {
        return LanePlan {
            checkpoint: initial(),
            lifecycle: if same_route {
                PiSourceLifecycle::Replace
            } else {
                PiSourceLifecycle::Copy
            },
            verify_prefix: false,
        };
    }
    let lifecycle = if !same_route {
        PiSourceLifecycle::Relocate
    } else if source.len == previous.complete_offset && previous.terminal {
        PiSourceLifecycle::NoOp
    } else {
        PiSourceLifecycle::Append
    };
    LanePlan {
        checkpoint: previous.clone(),
        lifecycle,
        verify_prefix: true,
    }
}

fn apply_prefix_verification(plan: Option<&mut LanePlan>, valid: bool, source: &PiFrozenSource) {
    let Some(plan) = plan else {
        return;
    };
    if valid {
        return;
    }
    plan.checkpoint =
        PiNativeCheckpoint::initial(source.route_sha256, source.physical_file_id, source.len);
    plan.lifecycle = PiSourceLifecycle::Rewrite;
    plan.verify_prefix = false;
}

fn current_checkpoint_for_plan(plan: &LanePlan, source: &PiFrozenSource) -> PiNativeCheckpoint {
    let mut checkpoint = plan.checkpoint.clone();
    if plan.lifecycle == PiSourceLifecycle::Relocate {
        checkpoint.route_sha256 = source.route_sha256;
        checkpoint.physical_file_id = source.physical_file_id;
        checkpoint.observed_file_len = source.len;
    }
    checkpoint
}

fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    stream_hasher: &mut Sha256,
    max_retained_bytes: usize,
) -> io::Result<RawLine> {
    let mut bytes = Vec::new();
    let mut observed_bytes = 0_u64;
    let mut terminated = false;
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index.saturating_add(1));
        let chunk = &available[..take];
        stream_hasher.update(chunk);
        observed_bytes = observed_bytes
            .checked_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX))
            .ok_or_else(|| io::Error::other("Pi JSONL line position overflowed"))?;
        if !oversized {
            let remaining = max_retained_bytes.saturating_sub(bytes.len());
            if chunk.len() <= remaining {
                bytes.extend_from_slice(chunk);
            } else {
                bytes.clear();
                oversized = true;
            }
        }
        terminated = chunk.last() == Some(&b'\n');
        reader.consume(take);
        if terminated {
            break;
        }
    }
    Ok(RawLine {
        bytes,
        observed_bytes,
        terminated,
        oversized,
    })
}

fn json_record_bytes(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    bytes.strip_suffix(b"\r").unwrap_or(bytes)
}

fn might_be_session_header(bytes: &[u8]) -> bool {
    bytes
        .windows(b"session".len())
        .any(|window| window == b"session")
}

#[allow(clippy::too_many_arguments)]
fn output_observation(
    header: &PiNativeSessionHeader,
    entry: &Value,
    event_type: EventType,
    ordinal: u64,
    line_number: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
    locator_source_item: &[u8],
    occurred_at: chrono::DateTime<Utc>,
    command: Option<crate::provider::importer::OutputCommandContext>,
    outcome: crate::provider::importer::OutputOutcomeMetadata,
    content: &str,
) -> Result<ProOutputObservation, PiNativePathError> {
    let line_number_usize =
        usize::try_from(line_number).map_err(|_| PiNativePathError::PositionOverflow)?;
    let source_item_len = u32::try_from(locator_source_item.len())
        .map_err(|_| PiNativePathError::PositionOverflow)?;
    let mut locator = Vec::with_capacity(20_usize.saturating_add(locator_source_item.len()));
    locator.extend_from_slice(&source_item_len.to_be_bytes());
    locator.extend_from_slice(locator_source_item);
    locator.extend_from_slice(&byte_start.to_be_bytes());
    locator.extend_from_slice(&byte_end_exclusive.to_be_bytes());
    Ok(ProOutputObservation {
        kind: if event_type == EventType::CommandOutput {
            OutputObservationKind::Command
        } else {
            OutputObservationKind::Tool
        },
        coordinate: OutputNativeCoordinate {
            unit_key: format!("line-{line_number}:output"),
            native_sequence: ordinal,
            native_record_id: Some(pi_output_native_record_id(entry, line_number_usize)),
            source_record_ordinal: Some(ordinal),
            source_record_subrecord_index: Some(0),
            byte_start: Some(byte_start),
            byte_end_exclusive: Some(byte_end_exclusive),
        },
        occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: header.id.clone(),
            root_session_id: header.id.clone(),
            parent_session_id: None,
            provider_session_id: Some(header.id.clone()),
            agent_id: None,
            repository: None,
        },
        call_id: pi_output_call_id(entry),
        command,
        outcome,
        locator: OutputSourceLocator {
            version: 1,
            kind: PI_OUTPUT_LOCATOR_KIND.to_owned(),
            payload: locator,
        },
        content: content.as_bytes().to_vec(),
    })
}

fn output_estimated_bytes_for(output: &ProOutputObservation) -> usize {
    let associations = &output.associations;
    let repository_bytes = associations.repository.as_ref().map_or(0, |repository| {
        repository
            .repository_id
            .len()
            .saturating_add(repository.checkout_id.as_ref().map_or(0, String::len))
            .saturating_add(repository.worktree_id.as_ref().map_or(0, String::len))
            .saturating_add(repository.object_format.as_ref().map_or(0, String::len))
    });
    let command_bytes = output.command.as_ref().map_or(0, |command| {
        command
            .tool_name
            .len()
            .saturating_add(command.command.len())
            .saturating_add(command.working_directory.as_ref().map_or(0, String::len))
    });
    let text_bytes = output
        .coordinate
        .unit_key
        .len()
        .saturating_add(
            output
                .coordinate
                .native_record_id
                .as_ref()
                .map_or(0, String::len),
        )
        .saturating_add(associations.direct_session_id.len())
        .saturating_add(associations.root_session_id.len())
        .saturating_add(
            associations
                .parent_session_id
                .as_ref()
                .map_or(0, String::len),
        )
        .saturating_add(
            associations
                .provider_session_id
                .as_ref()
                .map_or(0, String::len),
        )
        .saturating_add(associations.agent_id.as_ref().map_or(0, String::len))
        .saturating_add(repository_bytes)
        .saturating_add(output.call_id.as_ref().map_or(0, String::len))
        .saturating_add(command_bytes)
        .saturating_add(output.locator.kind.len())
        .saturating_add(output.locator.payload.len());
    text_bytes
        .saturating_mul(6)
        .saturating_add(1_024)
        .saturating_add(output.content.len())
}

fn event_row_occurred_at(
    units: &[PiNativeCoreUnit],
) -> Result<chrono::DateTime<Utc>, PiNativePathError> {
    units
        .iter()
        .find_map(|unit| match unit {
            PiNativeCoreUnit::Event(event) => Some(event.occurred_at),
            _ => None,
        })
        .ok_or_else(|| {
            PiNativePathError::Normalization(CaptureError::SystemInvariant(
                "Pi NativePath event unit is missing",
            ))
        })
}

fn page_error(error: impl std::fmt::Display) -> PiNativePathError {
    PiNativePathError::Page(error.to_string())
}
