use std::{
    fs::{self, File, Metadata},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, ContentRef, EventType};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::provider::file_touches::visit_all_file_touch_drafts;
use crate::provider::normalization::{
    provider_capped_json, provider_capped_json_value, provider_local_preview, provider_policy_body,
    provider_policy_event_text, provider_result_identifier_evidence,
    provider_result_outcome_evidence,
};
use crate::{
    CaptureError, OutputOutcome, Result, MAX_PROVIDER_JSONL_LINE_BYTES, PROVIDER_MAX_PREVIEW_CHARS,
};

use super::super::{
    dialect::{native_jsonl_record_starts_session, validate_direct_native_jsonl_provider},
    normalization::{
        antigravity_session_id_from_path, native_jsonl_entry_type, native_jsonl_event_id,
        native_jsonl_event_text, native_jsonl_event_type, native_jsonl_header_cwd,
        native_jsonl_header_session_id, native_jsonl_header_start_time, native_jsonl_model,
        native_jsonl_path_session, native_jsonl_role,
        native_jsonl_session_metadata_from_normalized_header, native_jsonl_session_status,
        native_jsonl_timestamp, native_jsonl_tokens,
    },
    result_content::{
        enumerate_native_jsonl_result_subrecords, native_jsonl_result_content_profile,
        NativeJsonlResultExtractionError,
    },
};
use super::{
    DirectJsonlCheckpoint, DirectJsonlEvent, DirectJsonlFileObservation, DirectJsonlObservedTime,
    DirectJsonlOutput, DirectJsonlPage, DirectJsonlRejection, DirectJsonlScanOutcome,
    DirectJsonlSession, DirectJsonlSourceChange, DirectJsonlTouch,
    DIRECT_JSONL_NATIVEPATH_PARSER_REVISION, DIRECT_JSONL_NATIVEPATH_POLICY_REVISION,
};

const DIRECT_JSONL_PREFIX_HASH_DOMAIN: &[u8] = b"ctx-direct-jsonl-nativepath-prefix-v1\0";
const DIRECT_JSONL_PAGE_MAX_RECORDS: usize = 64;
const DIRECT_JSONL_PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;
const DIRECT_JSONL_PAGE_ENVELOPE_BYTES: usize = 2 * 1024;
const DIRECT_JSONL_EVENT_ENVELOPE_BYTES: usize = 1024;
const DIRECT_JSONL_FAILURE_PREVIEW_CHARS: usize = 512;

pub(crate) struct DirectJsonlPageReader {
    provider: CaptureProvider,
    source_format: String,
    path: PathBuf,
    source_root: Option<PathBuf>,
    imported_at: DateTime<Utc>,
    collect_transient_outputs: bool,
    observation: DirectJsonlFileObservation,
    reader: BufReader<File>,
    prefix_hasher: Sha256,
    complete_prefix_end: u64,
    next_raw_ordinal: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
    rejected_records: u64,
    session: Option<DirectJsonlSession>,
    source_change: DirectJsonlSourceChange,
    skip_scan: bool,
    finished: bool,
    outcome: Option<DirectJsonlScanOutcome>,
}

pub(crate) fn open_direct_jsonl_pages(
    provider: CaptureProvider,
    source_format: &str,
    path: &Path,
    source_root: Option<PathBuf>,
    imported_at: DateTime<Utc>,
    collect_transient_outputs: bool,
    previous: Option<&DirectJsonlCheckpoint>,
) -> Result<DirectJsonlPageReader> {
    validate_direct_native_jsonl_provider(provider)?;
    if provider == CaptureProvider::Gemini {
        return Err(CaptureError::SystemInvariant(
            "Gemini requires its bespoke NativePath reader",
        ));
    }
    let canonical_path = fs::canonicalize(path)?;
    let observation = observe_file(&canonical_path)?;
    let mut file = File::open(&canonical_path)?;
    if observe_metadata(&file.metadata()?)? != observation {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut prefix_hasher = new_prefix_hasher();
    let mut complete_prefix_end = 0_u64;
    let mut next_raw_ordinal = 0_u64;
    let mut accepted_events = 0_u64;
    let mut accepted_file_touches = 0_u64;
    let mut rejected_records = 0_u64;
    let mut session = None;
    let mut source_change = DirectJsonlSourceChange::Fresh;
    let mut skip_scan = false;

    if let Some(previous) =
        previous.filter(|checkpoint| checkpoint.is_supported_for(provider, source_format))
    {
        let same_path = previous.source_path == canonical_path;
        let same_physical = same_file_identity(&previous.source_observation, &observation);
        let enough_bytes = observation.length >= previous.complete_prefix_end;
        if same_path && same_physical && enough_bytes {
            let observed_prefix =
                hash_prefix(&mut file, previous.complete_prefix_end, new_prefix_hasher())?;
            if prefix_digest(&observed_prefix) == previous.complete_prefix_sha256 {
                prefix_hasher = observed_prefix;
                complete_prefix_end = previous.complete_prefix_end;
                next_raw_ordinal = previous.next_raw_ordinal;
                accepted_events = previous.accepted_events;
                accepted_file_touches = previous.accepted_file_touches;
                rejected_records = previous.rejected_records;
                session.clone_from(&previous.session);
                source_change = if observation == previous.source_observation
                    && previous.terminal
                    && observation.length == previous.complete_prefix_end
                {
                    skip_scan = true;
                    DirectJsonlSourceChange::Unchanged
                } else {
                    DirectJsonlSourceChange::Append
                };
            } else {
                source_change = DirectJsonlSourceChange::Rewrite;
            }
        } else if same_path && observation.length < previous.complete_prefix_end {
            source_change = DirectJsonlSourceChange::Truncation;
        } else if same_path {
            source_change = DirectJsonlSourceChange::Replacement;
        }
    }

    if complete_prefix_end == 0 {
        file.seek(SeekFrom::Start(0))?;
        prefix_hasher = new_prefix_hasher();
    } else {
        file.seek(SeekFrom::Start(complete_prefix_end))?;
    }

    Ok(DirectJsonlPageReader {
        provider,
        source_format: source_format.to_owned(),
        path: canonical_path,
        source_root,
        imported_at,
        collect_transient_outputs,
        observation,
        reader: BufReader::new(file),
        prefix_hasher,
        complete_prefix_end,
        next_raw_ordinal,
        accepted_events,
        accepted_file_touches,
        rejected_records,
        session,
        source_change,
        skip_scan,
        finished: false,
        outcome: None,
    })
}

impl DirectJsonlPageReader {
    pub(crate) fn next_page(&mut self) -> Result<Option<DirectJsonlPage>> {
        if self.finished {
            return Ok(None);
        }
        if self.skip_scan {
            self.finish_terminal()?;
            return Ok(None);
        }

        let expected_checkpoint = self.checkpoint(false);
        let mut events = Vec::new();
        let mut outputs = Vec::new();
        let mut rejections = Vec::new();
        let mut physical_records = 0_usize;
        let mut logical_units = 0_usize;
        let mut serialized_bytes = DIRECT_JSONL_PAGE_ENVELOPE_BYTES;

        while physical_records < DIRECT_JSONL_PAGE_MAX_RECORDS {
            let start = self.complete_prefix_end;
            let ordinal = self.next_raw_ordinal;
            let hasher_before = self.prefix_hasher.clone();
            let line = read_bounded_jsonl_line(
                &mut self.reader,
                &mut self.prefix_hasher,
                self.observation.length,
                start,
            )?;
            let (bytes, end) = match line {
                DirectLine::EndOfFile => {
                    self.finish_terminal()?;
                    break;
                }
                DirectLine::IncompleteTail => {
                    self.prefix_hasher = hasher_before;
                    self.reader.seek(SeekFrom::Start(start))?;
                    self.finish_nonterminal()?;
                    break;
                }
                DirectLine::Oversized { end } => {
                    let rejection = DirectJsonlRejection {
                        raw_ordinal: ordinal,
                        byte_start: start,
                        byte_end_exclusive: end,
                        reason: format!(
                            "{}:{} exceeds the {} byte JSONL record limit",
                            self.path.display(),
                            ordinal.saturating_add(1),
                            MAX_PROVIDER_JSONL_LINE_BYTES
                        ),
                    };
                    let rejection_bytes = rejection_wire_bytes(&rejection);
                    if physical_records != 0
                        && serialized_bytes.saturating_add(rejection_bytes)
                            > DIRECT_JSONL_PAGE_MAX_BYTES
                    {
                        self.prefix_hasher = hasher_before;
                        self.reader.seek(SeekFrom::Start(start))?;
                        break;
                    }
                    self.complete_prefix_end = end;
                    self.next_raw_ordinal = self.next_raw_ordinal.saturating_add(1);
                    self.rejected_records = self.rejected_records.saturating_add(1);
                    physical_records = physical_records.saturating_add(1);
                    logical_units = logical_units.saturating_add(1);
                    serialized_bytes = serialized_bytes.saturating_add(rejection_bytes);
                    rejections.push(rejection);
                    continue;
                }
                DirectLine::Complete { bytes, end } => (bytes, end),
            };

            let projected = self.project_line(&bytes, ordinal, start, end)?;
            let projected_units = projected
                .events
                .len()
                .saturating_add(projected.outputs.len())
                .saturating_add(projected.rejections.len());
            let projected_bytes = projected.serialized_bytes;
            if projected_units > DIRECT_JSONL_PAGE_MAX_RECORDS
                || projected_bytes > DIRECT_JSONL_PAGE_MAX_BYTES
            {
                self.prefix_hasher = hasher_before;
                self.reader.seek(SeekFrom::Start(start))?;
                return Err(CaptureError::InvalidPayload(format!(
                    "{}:{} expands past a certified direct JSONL page boundary",
                    self.path.display(),
                    ordinal.saturating_add(1)
                )));
            }
            if physical_records != 0
                && (logical_units.saturating_add(projected_units) > DIRECT_JSONL_PAGE_MAX_RECORDS
                    || serialized_bytes.saturating_add(projected_bytes)
                        > DIRECT_JSONL_PAGE_MAX_BYTES)
            {
                self.prefix_hasher = hasher_before;
                self.reader.seek(SeekFrom::Start(start))?;
                break;
            }

            self.complete_prefix_end = end;
            self.next_raw_ordinal = self.next_raw_ordinal.saturating_add(1);
            self.accepted_events = self
                .accepted_events
                .saturating_add(projected.events.len() as u64);
            self.accepted_file_touches = self.accepted_file_touches.saturating_add(
                projected
                    .events
                    .iter()
                    .map(|event| event.touches.len() as u64)
                    .sum::<u64>(),
            );
            self.rejected_records = self
                .rejected_records
                .saturating_add(projected.rejections.len() as u64);
            physical_records = physical_records.saturating_add(1);
            logical_units = logical_units.saturating_add(projected_units);
            serialized_bytes = serialized_bytes.saturating_add(projected_bytes);
            events.extend(projected.events);
            outputs.extend(projected.outputs);
            rejections.extend(projected.rejections);
        }

        if physical_records == 0 {
            return Ok(None);
        }
        let terminal = self.finished
            && self
                .outcome
                .as_ref()
                .is_some_and(|outcome| outcome.checkpoint.terminal);
        Ok(Some(DirectJsonlPage {
            expected_checkpoint,
            next_checkpoint: self.checkpoint(terminal),
            events,
            outputs,
            rejections,
            logical_units,
            conservative_serialized_bytes: serialized_bytes,
            terminal,
        }))
    }

    pub(crate) fn outcome(&self) -> Option<&DirectJsonlScanOutcome> {
        self.outcome.as_ref()
    }

    pub(crate) fn observation(&self) -> &DirectJsonlFileObservation {
        &self.observation
    }

    pub(crate) fn source_change(&self) -> DirectJsonlSourceChange {
        self.source_change
    }

    fn project_line(
        &mut self,
        bytes: &[u8],
        ordinal: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
    ) -> Result<ProjectedLine> {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(ProjectedLine::default());
        }
        let value = match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => value,
            Err(error) => {
                return Ok(ProjectedLine::rejection(DirectJsonlRejection {
                    raw_ordinal: ordinal,
                    byte_start,
                    byte_end_exclusive,
                    reason: format!(
                        "{}:{} malformed JSONL: {error}",
                        self.path.display(),
                        ordinal.saturating_add(1)
                    ),
                }));
            }
        };

        if self.session.is_none() {
            let starts_session = match self.provider {
                CaptureProvider::Qoder => {
                    super::qoder_parser::qoder_header_session_id(&value).is_some()
                }
                CaptureProvider::QwenCode => {
                    super::qwen_code::qwen_code_header_session_id(&value).is_some()
                }
                _ => native_jsonl_record_starts_session(self.provider, &value),
            };
            if !starts_session {
                return Ok(ProjectedLine::rejection(DirectJsonlRejection {
                    raw_ordinal: ordinal,
                    byte_start,
                    byte_end_exclusive,
                    reason: format!(
                        "{}:{} appeared before an importable native session header",
                        self.path.display(),
                        ordinal.saturating_add(1)
                    ),
                }));
            }
            self.session = Some(session_from_header(
                self.provider,
                &self.source_format,
                &self.path,
                self.source_root.as_deref(),
                self.imported_at,
                &value,
            ));
        }
        let session = self.session.as_ref().ok_or(CaptureError::SystemInvariant(
            "direct JSONL reader lost its provider session",
        ))?;
        let line_number = usize::try_from(ordinal)
            .ok()
            .and_then(|ordinal| ordinal.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "direct JSONL line number exceeds platform limits",
            ))?;
        let event_type = direct_jsonl_event_type(self.provider, &value);
        let occurred_at = native_jsonl_timestamp(&value).unwrap_or(session.started_at);

        if event_type == EventType::ToolOutput
            && matches!(
                self.provider,
                CaptureProvider::FactoryAiDroid
                    | CaptureProvider::Qoder
                    | CaptureProvider::QwenCode
            )
        {
            return self.project_result_line(
                None,
                &value,
                ordinal,
                line_number,
                byte_start,
                byte_end_exclusive,
                occurred_at,
            );
        }
        let result_profile = (event_type == EventType::ToolOutput)
            .then(|| native_jsonl_result_content_profile(self.provider))
            .flatten();
        if let Some(profile) = result_profile {
            return self.project_result_line(
                Some(profile),
                &value,
                ordinal,
                line_number,
                byte_start,
                byte_end_exclusive,
                occurred_at,
            );
        }

        let mut event = direct_event(
            self.provider,
            &self.source_format,
            &value,
            ordinal,
            0,
            line_number,
            occurred_at,
            false,
            None,
        )?;
        attach_direct_message_locator(
            &mut event,
            self.provider,
            &self.source_format,
            &value,
            bytes,
            byte_start,
            byte_end_exclusive,
            line_number,
        )?;
        Ok(ProjectedLine::event(event))
    }

    #[allow(clippy::too_many_arguments)]
    fn project_result_line(
        &self,
        profile: Option<&str>,
        value: &Value,
        ordinal: u64,
        line_number: usize,
        byte_start: u64,
        byte_end_exclusive: u64,
        occurred_at: DateTime<Utc>,
    ) -> Result<ProjectedLine> {
        let extracted = match self.provider {
            CaptureProvider::FactoryAiDroid => super::enumerate_factory_droid_results(value),
            CaptureProvider::Qoder => super::qoder_parser::enumerate_qoder_results(value),
            CaptureProvider::QwenCode => super::qwen_code::enumerate_qwen_code_results(value),
            _ => {
                let Some(profile) = profile else {
                    return Err(CaptureError::SystemInvariant(
                        "direct JSONL result reader has no provider parser",
                    ));
                };
                enumerate_native_jsonl_result_subrecords(profile, value)
            }
        };
        let subrecords = match extracted {
            Ok(subrecords) => subrecords,
            Err(
                NativeJsonlResultExtractionError::Redacted
                | NativeJsonlResultExtractionError::InvalidShape,
            ) => return Ok(ProjectedLine::default()),
            Err(
                NativeJsonlResultExtractionError::UnsupportedProfile
                | NativeJsonlResultExtractionError::Ambiguous,
            ) => {
                return Err(CaptureError::SystemInvariant(
                    "direct JSONL result reader used an invalid provider profile",
                ));
            }
        };
        let mut projected = ProjectedLine::default();
        for subrecord in subrecords {
            let sub_ordinal = u32::try_from(subrecord.subrecord_index).map_err(|_| {
                CaptureError::InvalidPayload(
                    "direct JSONL result subrecord index exceeds u32".to_owned(),
                )
            })?;
            if matches!(
                subrecord.outcome.outcome,
                OutputOutcome::Failure | OutputOutcome::Timeout
            ) {
                projected.events.push(direct_event(
                    self.provider,
                    &self.source_format,
                    value,
                    ordinal,
                    sub_ordinal,
                    line_number,
                    occurred_at,
                    true,
                    Some(&subrecord),
                )?);
            } else if self.collect_transient_outputs {
                let Some(content) = subrecord.content else {
                    continue;
                };
                projected.outputs.push(DirectJsonlOutput {
                    raw_ordinal: ordinal,
                    sub_ordinal,
                    byte_start,
                    byte_end_exclusive,
                    call_id: subrecord.call_id.map(str::to_owned),
                    tool_name: subrecord.tool_name.map(str::to_owned),
                    outcome: subrecord.outcome.outcome,
                    exit_code: subrecord.outcome.exit_code,
                    duration_ms: subrecord.outcome.duration_ms,
                    content: content.as_bytes().to_vec(),
                });
            }
        }
        projected.recompute_serialized_bytes();
        Ok(projected)
    }

    fn checkpoint(&self, terminal: bool) -> DirectJsonlCheckpoint {
        DirectJsonlCheckpoint {
            version: DirectJsonlCheckpoint::VERSION,
            parser_revision: DIRECT_JSONL_NATIVEPATH_PARSER_REVISION,
            policy_revision: DIRECT_JSONL_NATIVEPATH_POLICY_REVISION,
            provider: self.provider,
            source_format: self.source_format.clone(),
            source_path: self.path.clone(),
            source_observation: self.observation.clone(),
            complete_prefix_end: self.complete_prefix_end,
            complete_prefix_sha256: prefix_digest(&self.prefix_hasher),
            next_raw_ordinal: self.next_raw_ordinal,
            accepted_events: self.accepted_events,
            accepted_file_touches: self.accepted_file_touches,
            rejected_records: self.rejected_records,
            session: self.session.clone(),
            terminal,
        }
    }

    fn finish_terminal(&mut self) -> Result<()> {
        revalidate_file(&self.path, &self.observation)?;
        let checkpoint = self.checkpoint(true);
        let source_sha256 = prefix_digest(&self.prefix_hasher);
        self.outcome = Some(DirectJsonlScanOutcome {
            checkpoint,
            source_change: self.source_change,
            source_sha256,
            accepted_events: self.accepted_events,
            accepted_file_touches: self.accepted_file_touches,
            rejected_records: self.rejected_records,
        });
        self.finished = true;
        Ok(())
    }

    fn finish_nonterminal(&mut self) -> Result<()> {
        revalidate_file(&self.path, &self.observation)?;
        let checkpoint = self.checkpoint(false);
        let source_sha256 = prefix_digest(&self.prefix_hasher);
        self.outcome = Some(DirectJsonlScanOutcome {
            checkpoint,
            source_change: self.source_change,
            source_sha256,
            accepted_events: self.accepted_events,
            accepted_file_touches: self.accepted_file_touches,
            rejected_records: self.rejected_records,
        });
        self.finished = true;
        Ok(())
    }
}

fn direct_jsonl_event_type(provider: CaptureProvider, value: &Value) -> EventType {
    match provider {
        CaptureProvider::FactoryAiDroid => super::factory_droid_event_type(value),
        CaptureProvider::Qoder => super::qoder_parser::qoder_event_type(value),
        CaptureProvider::QwenCode => super::qwen_code::qwen_code_event_type(value),
        _ => native_jsonl_event_type(provider, value),
    }
}

fn direct_jsonl_role(provider: CaptureProvider, value: &Value) -> ctx_history_core::EventRole {
    match provider {
        CaptureProvider::FactoryAiDroid => super::factory_droid_role(value),
        CaptureProvider::Qoder => super::qoder_parser::qoder_role(value),
        CaptureProvider::QwenCode => super::qwen_code::qwen_code_role(value),
        _ => native_jsonl_role(provider, value),
    }
}

fn direct_jsonl_event_text(
    provider: CaptureProvider,
    value: &Value,
    event_type: EventType,
    entry_type: &str,
) -> String {
    match provider {
        CaptureProvider::FactoryAiDroid => super::factory_droid_event_text(value),
        CaptureProvider::Qoder => super::qoder_parser::qoder_event_text(value, event_type),
        CaptureProvider::QwenCode => super::qwen_code::qwen_code_event_text(value),
        _ => native_jsonl_event_text(provider, value, event_type, entry_type),
    }
}

fn direct_jsonl_model(provider: CaptureProvider, value: &Value) -> Option<Value> {
    match provider {
        CaptureProvider::FactoryAiDroid => super::factory_droid_model(value),
        CaptureProvider::Qoder => super::qoder_parser::qoder_model(value),
        CaptureProvider::QwenCode => super::qwen_code::qwen_code_model(value),
        _ => native_jsonl_model(provider, value),
    }
}

#[allow(clippy::too_many_arguments)]
fn attach_direct_message_locator(
    event: &mut DirectJsonlEvent,
    provider: CaptureProvider,
    source_format: &str,
    value: &Value,
    record_bytes: &[u8],
    byte_start: u64,
    byte_end_exclusive: u64,
    line_number: usize,
) -> Result<()> {
    use crate::complete_content::jsonl::JSONL_COMPLETE_CONTENT_LOCATOR_KIND;
    use crate::complete_content::{
        attach_verified_content_locator, verified_content_address_supported,
        verified_content_profile, CompleteContentBodyDigest, CompleteContentSourceFamily,
        VerifiedContentLocatorV1, VerifiedContentRole, COMPLETE_CONTENT_MAX_BODY_BYTES,
    };

    if event.event_type != EventType::Message
        || !verified_content_address_supported(
            provider,
            source_format,
            CompleteContentSourceFamily::Jsonl,
            VerifiedContentRole::MessageBody,
            JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        )
    {
        return Ok(());
    }
    let entry_type = native_jsonl_entry_type(provider, value);
    let text = direct_jsonl_event_text(provider, value, EventType::Message, &entry_type);
    if text.chars().count() <= crate::PROVIDER_MAX_TEXT_CHARS
        || text.len() > COMPLETE_CONTENT_MAX_BODY_BYTES
        || byte_start >= byte_end_exclusive
    {
        return Ok(());
    }
    let Some(content_ref) = ContentRef::from_bytes(text.as_bytes()) else {
        return Ok(());
    };
    let Some(profile) = verified_content_profile(
        provider,
        source_format,
        CompleteContentSourceFamily::Jsonl,
        VerifiedContentRole::MessageBody,
    ) else {
        return Err(CaptureError::SystemInvariant(
            "supported direct JSONL route has no complete-content profile",
        ));
    };
    let mut range = [0_u8; 16];
    range[..8].copy_from_slice(&byte_start.to_be_bytes());
    range[8..].copy_from_slice(&byte_end_exclusive.to_be_bytes());
    let Some(locator) = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Jsonl,
        JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        &range,
        native_jsonl_event_id(provider, value, line_number),
        CompleteContentBodyDigest::from_bytes(record_bytes),
    ) else {
        return Ok(());
    };
    attach_verified_content_locator(&mut event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("direct JSONL verified-content locator is malformed"),
    )?;
    Ok(())
}

#[derive(Default)]
struct ProjectedLine {
    events: Vec<DirectJsonlEvent>,
    outputs: Vec<DirectJsonlOutput>,
    rejections: Vec<DirectJsonlRejection>,
    serialized_bytes: usize,
}

impl ProjectedLine {
    fn event(event: DirectJsonlEvent) -> Self {
        let mut line = Self {
            events: vec![event],
            ..Self::default()
        };
        line.recompute_serialized_bytes();
        line
    }

    fn rejection(rejection: DirectJsonlRejection) -> Self {
        let serialized_bytes = rejection_wire_bytes(&rejection);
        Self {
            rejections: vec![rejection],
            serialized_bytes,
            ..Self::default()
        }
    }

    fn recompute_serialized_bytes(&mut self) {
        self.serialized_bytes = self
            .events
            .iter()
            .map(event_wire_bytes)
            .chain(self.outputs.iter().map(output_wire_bytes))
            .chain(self.rejections.iter().map(rejection_wire_bytes))
            .fold(0_usize, usize::saturating_add);
    }
}

#[allow(clippy::too_many_arguments)]
fn direct_event(
    provider: CaptureProvider,
    source_format: &str,
    value: &Value,
    raw_ordinal: u64,
    sub_ordinal: u32,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    retained_failure: bool,
    result: Option<&super::super::result_content::NativeJsonlResultSubrecord<'_>>,
) -> Result<DirectJsonlEvent> {
    let event_type = direct_jsonl_event_type(provider, value);
    let entry_type = native_jsonl_entry_type(provider, value);
    let role = direct_jsonl_role(provider, value);
    let body_value = if provider == CaptureProvider::Windsurf {
        super::windsurf::windsurf_event_body(value)
    } else {
        value.clone()
    };
    let text = if event_type == EventType::ToolOutput {
        String::new()
    } else {
        direct_jsonl_event_text(provider, value, event_type, &entry_type)
    };
    let retained_text = provider_policy_event_text(event_type, &text, &body_value);
    let event_id = native_jsonl_event_id(provider, value, line_number);
    let mut provider_event_hash = event_id.clone();
    let mut cursor = event_id.clone();
    let mut payload = json!({
        "entry_type": entry_type,
        "event_id": event_id,
        "native_step_index": value.get("step_index").and_then(Value::as_u64),
        "text": retained_text.text,
        "text_retention": retained_text.retention.as_json(),
        "result_evidence": provider_result_identifier_evidence(event_type, &text, &body_value),
        "result_outcome": provider_result_outcome_evidence(event_type, &body_value),
        "tool_calls": if provider == CaptureProvider::Antigravity {
            value.get("tool_calls").map(|calls| {
                provider_capped_json_value(
                    &provider_policy_body(EventType::ToolCall, calls),
                    PROVIDER_MAX_PREVIEW_CHARS,
                )
            })
        } else {
            None
        },
        "body": provider_capped_json(
            &provider_policy_body(event_type, &body_value),
            PROVIDER_MAX_PREVIEW_CHARS,
        ),
    });

    if retained_failure {
        let result = result.ok_or(CaptureError::SystemInvariant(
            "retained direct JSONL failure has no result subrecord",
        ))?;
        let suffix = format!(":subrecord:{}", result.subrecord_index);
        provider_event_hash.push_str(&suffix);
        cursor.push_str(&suffix);
        if let Some(payload) = payload.as_object_mut() {
            payload.insert(
                "result_outcome".to_owned(),
                Value::String("failure".to_owned()),
            );
            payload.insert(
                "timed_out".to_owned(),
                Value::Bool(result.outcome.outcome == OutputOutcome::Timeout),
            );
            payload.insert(
                "exit_code".to_owned(),
                result
                    .outcome
                    .exit_code
                    .map_or(Value::Null, |code| Value::Number(code.into())),
            );
            payload.insert(
                "duration_ms".to_owned(),
                result
                    .outcome
                    .duration_ms
                    .map_or(Value::Null, |duration| Value::Number(duration.into())),
            );
            payload.insert(
                "call_id".to_owned(),
                result
                    .call_id
                    .map_or(Value::Null, |value| Value::String(value.to_owned())),
            );
            payload.insert(
                "tool_name".to_owned(),
                result
                    .tool_name
                    .map_or(Value::Null, |value| Value::String(value.to_owned())),
            );
            if let Some(content) = result.content {
                let (preview, _) =
                    provider_local_preview(content, DIRECT_JSONL_FAILURE_PREVIEW_CHARS);
                payload.insert("output_preview".to_owned(), Value::String(preview));
            }
        }
    }

    let mut touches = Vec::new();
    if event_type != EventType::ToolOutput || retained_failure {
        visit_all_file_touch_drafts(value, |draft| {
            touches.push(DirectJsonlTouch {
                path: draft.path,
                old_path: draft.old_path,
                change_kind: draft.change_kind,
            });
            Ok::<(), CaptureError>(())
        })?;
    }
    let positional_event_index = if sub_ordinal == 0 {
        raw_ordinal
    } else {
        raw_ordinal
            .checked_mul(u64::from(u16::MAX) + 1)
            .and_then(|index| index.checked_add(u64::from(sub_ordinal)))
            .map(|index| index | (1_u64 << 63))
            .ok_or(CaptureError::SystemInvariant(
                "direct JSONL provider event identity index overflowed",
            ))?
    };
    let provider_event_index = direct_jsonl_native_event_identity(provider, value)
        .map(|event_identity| {
            direct_jsonl_event_identity_index(provider, event_identity, sub_ordinal)
        })
        .unwrap_or(positional_event_index);
    let provider_event_sequence_index = positional_event_index;
    Ok(DirectJsonlEvent {
        raw_ordinal,
        sub_ordinal,
        provider_event_index,
        provider_event_sequence_index,
        provider_event_hash,
        cursor,
        event_type,
        role,
        occurred_at,
        payload,
        metadata: json!({
            "source": source_format,
            "source_format": source_format,
            "line": line_number,
            "entry_type": entry_type,
            "status": value.get("status").and_then(Value::as_str),
            "model": direct_jsonl_model(provider, value),
            "tokens": native_jsonl_tokens(provider, value),
            "source_record_ordinal": raw_ordinal,
            "source_record_subrecord_index": sub_ordinal,
            "legacy_provider_event_index": raw_ordinal,
        }),
        touches,
    })
}

fn direct_jsonl_native_event_identity(provider: CaptureProvider, value: &Value) -> Option<&str> {
    match provider {
        CaptureProvider::CopilotCli => super::copilot::copilot_event_identity(value),
        CaptureProvider::FactoryAiDroid => super::factory_droid_event_identity(value),
        CaptureProvider::Qoder => super::qoder_parser::qoder_event_identity(value),
        CaptureProvider::QwenCode => super::qwen_code_event_identity(value),
        CaptureProvider::Tabnine => super::tabnine::tabnine_event_identity(value),
        _ => None,
    }
}

fn direct_jsonl_event_identity_index(
    provider: CaptureProvider,
    event_identity: &str,
    sub_ordinal: u32,
) -> u64 {
    let mut digest = Sha256::new();
    digest.update(b"ctx-direct-jsonl-provider-event-identity-v1\0");
    digest.update(provider.as_str().as_bytes());
    digest.update((event_identity.len() as u64).to_be_bytes());
    digest.update(event_identity.as_bytes());
    digest.update(sub_ordinal.to_be_bytes());
    u64::from_be_bytes(
        digest.finalize()[..8]
            .try_into()
            .expect("SHA-256 identity prefix is eight bytes"),
    )
}

fn session_from_header(
    provider: CaptureProvider,
    source_format: &str,
    path: &Path,
    _source_root: Option<&Path>,
    imported_at: DateTime<Utc>,
    header: &Value,
) -> DirectJsonlSession {
    let native_session_id = match provider {
        CaptureProvider::Antigravity => {
            antigravity_session_id_from_path(path).unwrap_or_else(|| "unknown-session".to_owned())
        }
        CaptureProvider::Windsurf => super::windsurf::windsurf_session_id_from_path(path)
            .unwrap_or_else(|| "unknown-session".to_owned()),
        CaptureProvider::Qoder => super::qoder_parser::qoder_header_session_id(header)
            .unwrap_or_else(|| "unknown-session".to_owned()),
        CaptureProvider::QwenCode => super::qwen_code::qwen_code_header_session_id(header)
            .unwrap_or_else(|| "unknown-session".to_owned()),
        _ => native_jsonl_header_session_id(provider, header)
            .unwrap_or_else(|| "unknown-session".to_owned()),
    };
    let (provider_session_id, parent_provider_session_id, external_agent_id, agent_type) =
        native_jsonl_path_session(provider, path, header, &native_session_id);
    let started_at = native_jsonl_timestamp(header)
        .or_else(|| native_jsonl_header_start_time(provider, header))
        .unwrap_or(imported_at);
    let cwd = match provider {
        CaptureProvider::Qoder => super::qoder_parser::qoder_header_cwd(header),
        CaptureProvider::QwenCode => super::qwen_code::qwen_code_header_cwd(header),
        _ => native_jsonl_header_cwd(provider, header),
    };
    let metadata = native_jsonl_session_metadata_from_normalized_header(
        provider,
        source_format,
        &super::super::normalization::native_jsonl_normalized_header_metadata(header),
        path,
    );
    let is_subagent =
        parent_provider_session_id.is_some() || agent_type == ctx_history_core::AgentType::Subagent;
    DirectJsonlSession {
        native_session_id,
        provider_session_id,
        root_provider_session_id: parent_provider_session_id.clone(),
        parent_provider_session_id,
        external_agent_id,
        agent_type,
        role_hint: Some(if is_subagent { "subagent" } else { "primary" }.to_owned()),
        is_primary: !is_subagent,
        status: native_jsonl_session_status(provider, header),
        started_at,
        ended_at: None,
        cwd,
        metadata,
    }
}

enum DirectLine {
    EndOfFile,
    IncompleteTail,
    Oversized { end: u64 },
    Complete { bytes: Vec<u8>, end: u64 },
}

fn read_bounded_jsonl_line(
    reader: &mut BufReader<File>,
    hasher: &mut Sha256,
    frozen_length: u64,
    start: u64,
) -> Result<DirectLine> {
    if start >= frozen_length {
        return Ok(DirectLine::EndOfFile);
    }
    let mut bytes = Vec::new();
    let mut total = 0_u64;
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(if total == 0 {
                DirectLine::EndOfFile
            } else {
                DirectLine::IncompleteTail
            });
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index.saturating_add(1));
        let chunk = &available[..take];
        hasher.update(chunk);
        total = total.saturating_add(chunk.len() as u64);
        if !oversized {
            if bytes.len().saturating_add(chunk.len())
                > MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2)
            {
                oversized = true;
                bytes.clear();
            } else {
                bytes.extend_from_slice(chunk);
            }
        }
        let complete = chunk.last() == Some(&b'\n');
        reader.consume(take);
        if complete {
            let end = start.saturating_add(total);
            if oversized {
                return Ok(DirectLine::Oversized { end });
            }
            if bytes.last() == Some(&b'\n') {
                bytes.pop();
                if bytes.last() == Some(&b'\r') {
                    bytes.pop();
                }
            }
            return Ok(DirectLine::Complete { bytes, end });
        }
    }
}

pub(crate) fn observe_file(path: &Path) -> Result<DirectJsonlFileObservation> {
    crate::common::io::ensure_regular_provider_transcript_file(path)?;
    observe_metadata(&fs::symlink_metadata(path)?)
}

pub(crate) fn direct_jsonl_source_revision(observation: &DirectJsonlFileObservation) -> String {
    let side = if observation.modified.before_epoch {
        '-'
    } else {
        '+'
    };
    format!(
        "native-jsonl-metadata-v1:length={};modified={side}{}.{:09};readonly={};device={};inode={}",
        observation.length,
        observation.modified.seconds,
        observation.modified.nanos,
        observation.readonly,
        observation
            .device
            .map_or_else(|| "none".to_owned(), |value| value.to_string()),
        observation
            .inode
            .map_or_else(|| "none".to_owned(), |value| value.to_string()),
    )
}

pub(crate) fn direct_jsonl_prefix_sha256(path: &Path, length: u64) -> Result<[u8; 32]> {
    let mut file = File::open(path)?;
    Ok(prefix_digest(&hash_prefix(
        &mut file,
        length,
        new_prefix_hasher(),
    )?))
}

fn observe_metadata(metadata: &Metadata) -> Result<DirectJsonlFileObservation> {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

    #[cfg(unix)]
    let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
    #[cfg(not(unix))]
    let (device, inode) = (None, None);
    Ok(DirectJsonlFileObservation {
        length: metadata.len(),
        modified: DirectJsonlObservedTime::from_system_time(metadata.modified()?),
        readonly: metadata.permissions().readonly(),
        device,
        inode,
    })
}

fn revalidate_file(path: &Path, expected: &DirectJsonlFileObservation) -> Result<()> {
    if &observe_file(path)? != expected {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(())
}

fn same_file_identity(
    previous: &DirectJsonlFileObservation,
    current: &DirectJsonlFileObservation,
) -> bool {
    match (
        previous.device,
        previous.inode,
        current.device,
        current.inode,
    ) {
        (Some(previous_device), Some(previous_inode), Some(device), Some(inode)) => {
            previous_device == device && previous_inode == inode
        }
        _ => previous.modified == current.modified && previous.readonly == current.readonly,
    }
}

fn new_prefix_hasher() -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(DIRECT_JSONL_PREFIX_HASH_DOMAIN);
    hasher
}

fn hash_prefix(file: &mut File, length: u64, mut hasher: Sha256) -> Result<Sha256> {
    file.seek(SeekFrom::Start(0))?;
    let mut remaining = length;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64)).map_err(|_| {
            CaptureError::SystemInvariant("direct JSONL prefix read length exceeds usize")
        })?;
        let read = file.read(&mut buffer[..requested])?;
        if read == 0 {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok(hasher)
}

fn prefix_digest(hasher: &Sha256) -> [u8; 32] {
    hasher.clone().finalize().into()
}

fn rejection_wire_bytes(rejection: &DirectJsonlRejection) -> usize {
    128_usize.saturating_add(rejection.reason.len())
}

fn event_wire_bytes(event: &DirectJsonlEvent) -> usize {
    DIRECT_JSONL_EVENT_ENVELOPE_BYTES
        .saturating_add(event.provider_event_hash.len())
        .saturating_add(event.cursor.len())
        .saturating_add(serde_json::to_vec(&event.payload).map_or(usize::MAX, |value| value.len()))
        .saturating_add(serde_json::to_vec(&event.metadata).map_or(usize::MAX, |value| value.len()))
        .saturating_add(
            event
                .touches
                .iter()
                .map(|touch| {
                    touch
                        .path
                        .len()
                        .saturating_add(touch.old_path.as_deref().map_or(0, str::len))
                })
                .sum::<usize>(),
        )
}

fn output_wire_bytes(output: &DirectJsonlOutput) -> usize {
    512_usize
        .saturating_add(output.call_id.as_deref().map_or(0, str::len))
        .saturating_add(output.tool_name.as_deref().map_or(0, str::len))
        .saturating_add(output.content.len())
}
