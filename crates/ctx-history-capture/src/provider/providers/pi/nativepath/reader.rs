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
    MAX_PROVIDER_JSONL_LINE_BYTES, PROVIDER_MAX_PREVIEW_CHARS, PROVIDER_MAX_TEXT_CHARS,
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

    pub(crate) fn opened_source(&self) -> Arc<OpenedProviderSourceFile> {
        self.source.opened()
    }

    pub(crate) fn revalidate_source(&self) -> Result<(), PiNativePathError> {
        self.source.fence(self.reader.get_ref())
    }

    pub(crate) fn core_lifecycle(&self) -> Option<PiSourceLifecycle> {
        self.core.as_ref().map(|lane| lane.lifecycle)
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
}
