use std::{
    collections::BTreeSet,
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
    provider_capped_json, provider_capped_json_value, provider_policy_body,
    provider_policy_event_text, provider_result_identifier_evidence,
    provider_result_outcome_evidence,
};
use crate::{
    CaptureError, OutputOutcome, Result, MAX_PROVIDER_JSONL_LINE_BYTES, PROVIDER_MAX_PREVIEW_CHARS,
};

use super::super::{
    dialect::{native_jsonl_record_starts_session, validate_direct_native_jsonl_provider},
    normalization,
    normalization::{
        antigravity_session_id_from_path, native_jsonl_entry_type, native_jsonl_event_id,
        native_jsonl_event_text, native_jsonl_event_type, native_jsonl_header_cwd,
        native_jsonl_header_session_id, native_jsonl_header_start_time, native_jsonl_model,
        native_jsonl_path_session, native_jsonl_role,
        native_jsonl_session_metadata_from_normalized_header, native_jsonl_session_status,
        native_jsonl_timestamp, native_jsonl_tokens,
    },
    result_content,
    result_content::{
        enumerate_native_jsonl_result_subrecords, native_jsonl_result_content_profile,
        NativeJsonlResultExtractionError,
    },
};
use super::{
    copilot, enumerate_factory_droid_results, factory_droid_event_identity,
    factory_droid_event_text, factory_droid_event_type, factory_droid_model, factory_droid_role,
    qoder_parser, qwen_code, tabnine, windsurf, DirectJsonlCheckpoint, DirectJsonlEvent,
    DirectJsonlFileObservation, DirectJsonlObservedTime, DirectJsonlOutput, DirectJsonlPage,
    DirectJsonlRejection, DirectJsonlScanOutcome, DirectJsonlSession, DirectJsonlSourceChange,
    DirectJsonlSourceRecord, DirectJsonlTouch, DIRECT_JSONL_NATIVEPATH_PARSER_REVISION,
    DIRECT_JSONL_NATIVEPATH_POLICY_REVISION,
};

const DIRECT_JSONL_PREFIX_HASH_DOMAIN: &[u8] = b"ctx-direct-jsonl-nativepath-prefix-v1\0";
// Scanner pages own the provider contract directly. Publication mechanics are
// accounted separately by the Store and must not reduce this 64-unit bound.
const DIRECT_JSONL_PAGE_MAX_RECORDS: usize = 64;
pub(super) const DIRECT_JSONL_PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;
const DIRECT_JSONL_PAGE_ENVELOPE_BYTES: usize = 2 * 1024;
const DIRECT_JSONL_EVENT_ENVELOPE_BYTES: usize = 1024;
// One event reconciliation and its file-touch upserts share a Store group.
// Keep the event itself inside the normalized-row page bound.
pub(super) const DIRECT_JSONL_MAX_FILE_TOUCHES_PER_RECORD: usize =
    DIRECT_JSONL_PAGE_MAX_RECORDS - 1;

#[path = "reader_projection.rs"]
mod projection;
pub(crate) use projection::direct_jsonl_complete_message_provider_event_hash;

#[path = "reader_source.rs"]
mod source;
pub(crate) use source::{direct_jsonl_prefix_sha256, direct_jsonl_source_revision, observe_file};
use source::{
    event_wire_bytes, hash_prefix, new_prefix_hasher, observe_metadata, output_wire_bytes,
    prefix_digest, read_bounded_jsonl_line, rejection_wire_bytes, revalidate_file,
    same_file_identity, DirectLine,
};

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
    rejection_checkpoint: Option<DirectJsonlCheckpoint>,
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
        rejection_checkpoint: None,
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
            let checkpoint_before = self.checkpoint(false);
            let hasher_before = self.prefix_hasher.clone();
            let line = read_bounded_jsonl_line(
                &mut self.reader,
                &mut self.prefix_hasher,
                self.observation.length,
                start,
            )?;
            let (bytes, end, record_digest) = match line {
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
                    self.remember_rejection_checkpoint(checkpoint_before);
                    physical_records = physical_records.saturating_add(1);
                    logical_units = logical_units.saturating_add(1);
                    serialized_bytes = serialized_bytes.saturating_add(rejection_bytes);
                    rejections.push(rejection);
                    continue;
                }
                DirectLine::Complete {
                    bytes,
                    end,
                    record_digest,
                } => (bytes, end, record_digest),
            };

            let projected = self.project_line(&bytes, ordinal, start, end, record_digest)?;
            let projected_units = projected
                .events
                .iter()
                .map(|event| 1_usize.saturating_add(event.touches.len()))
                .sum::<usize>()
                .saturating_add(projected.outputs.len())
                .saturating_add(projected.rejections.len())
                .max(1);
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
            if !projected.rejections.is_empty() {
                self.remember_rejection_checkpoint(checkpoint_before);
            }
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
            next_checkpoint: self.publication_checkpoint(terminal),
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

    fn remember_rejection_checkpoint(&mut self, checkpoint: DirectJsonlCheckpoint) {
        if self.rejection_checkpoint.is_none() {
            self.rejection_checkpoint = Some(checkpoint);
        }
    }

    fn publication_checkpoint(&self, terminal: bool) -> DirectJsonlCheckpoint {
        self.rejection_checkpoint
            .clone()
            .unwrap_or_else(|| self.checkpoint(terminal))
    }

    fn finish_terminal(&mut self) -> Result<()> {
        revalidate_file(&self.path, &self.observation)?;
        let checkpoint = self.publication_checkpoint(true);
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
        let checkpoint = self.publication_checkpoint(false);
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
