use std::{
    cell::{Cell, RefCell},
    fs::{self, File},
    io::{BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventRole, ProviderCaptureEnvelope};
use ctx_history_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::captured_batch::jsonl::{
    initial_jsonl_position, jsonl_position_offset, verify_jsonl_append_boundary, JsonlBatchError,
    JsonlBatchProducer,
};
use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, NativePosition, ProviderRecordKind,
    SourceObservation, StructuralRejectionKind,
};
use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
};
use crate::common::time::parse_rfc3339_utc;
use crate::provider::importer::{
    captured_batch_cursor_stream, drain_captured_batches, emit_projected_normalization_units,
    provider_path_identity, provider_source_cursor_stream_for_path, BoundedParserCheckpoint,
    CapturedBatchCursorFinish, CapturedBatchCursorMode, CapturedBatchProjector,
    CapturedSourceAdmission, CertifiedProviderCursor, ProviderProjectionFatal,
    ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::provider::normalization::{provider_role, provider_value_text};
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    ProviderNormalizationResult, Result, CODEBUDDY_SOURCE_FORMAT, MAX_PROVIDER_JSONL_LINE_BYTES,
};

use super::normalization::{
    codebuddy_bounded_checkpoint_text, codebuddy_capture, codebuddy_capture_envelope,
    codebuddy_captured_batch_error, codebuddy_checkpoint_time, codebuddy_clean_content,
    codebuddy_mark_skipped_session, codebuddy_title_from_text, CodeBuddyBoundedFailure,
    CodeBuddyCaptureDraft, CodeBuddyEventInput, CodeBuddyNativeShape, CodeBuddyProjectionCounts,
};
use super::source::CodeBuddyFrozenFile;
use super::{
    CODEBUDDY_CAPTURE_REVISION, CODEBUDDY_CLI_LOCATOR_KIND, CODEBUDDY_CLI_POLICY_REVISION,
    CODEBUDDY_CLI_RECORD_KIND, CODEBUDDY_CLI_TITLE_ANCHOR_HASH_DOMAIN,
    CODEBUDDY_MAX_CHECKPOINT_FAILURES,
};

pub(super) mod complete_content;

use complete_content::CodeBuddyCliCompleteContentBinding;

pub(super) fn visit_jsonl_files(
    root: &Path,
    visit: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<usize> {
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_file() {
        ensure_regular_provider_transcript_file(root)?;
        if root.extension().and_then(|extension| extension.to_str()) == Some("jsonl") {
            visit(root)?;
            return Ok(1);
        }
        return Ok(0);
    }
    if !metadata.file_type().is_dir() {
        return Ok(0);
    }
    ensure_provider_path_parents_are_not_symlinks(root)?;
    let scan_root = if root.join("projects").is_dir() {
        root.join("projects")
    } else if root.file_name().and_then(|name| name.to_str()) == Some("projects")
        || root
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            == Some("projects")
    {
        root.to_path_buf()
    } else {
        return Ok(0);
    };
    visit_codebuddy_jsonl_tree(&scan_root, visit)
}

fn visit_codebuddy_jsonl_tree(
    root: &Path,
    visit: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<usize> {
    let mut visited = 0_usize;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            visited = visited.saturating_add(visit_codebuddy_jsonl_tree(&path, visit)?);
        } else if file_type.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
        {
            ensure_regular_provider_transcript_file(&path)?;
            visit(&path)?;
            visited = visited.saturating_add(1);
        }
    }
    Ok(visited)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeBuddyCliParserCheckpoint {
    next_ordinal: u64,
    native_session_id: String,
    discovered_session_id: bool,
    cwd: Option<String>,
    started_at: Option<String>,
    ended_at: Option<String>,
    generated_title_record: Option<CodeBuddyCliRecordAnchor>,
    row_count: u64,
    counts: CodeBuddyProjectionCounts,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeBuddyCliRecordAnchor {
    ordinal: u64,
    start: u64,
    end: u64,
    payload_sha256: [u8; 32],
}

struct CodeBuddyCliCapturedBatchProjector {
    context: ProviderAdapterContext,
    path: PathBuf,
    session_ordinal: usize,
    project_hash: String,
    next_ordinal: Cell<u64>,
    native_session_id: String,
    discovered_session_id: bool,
    cwd: Option<String>,
    started_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
    title: Option<String>,
    generated_title_record: Option<CodeBuddyCliRecordAnchor>,
    row_count: u64,
    counts: CodeBuddyProjectionCounts,
    structural_rejections: Cell<u64>,
    structural_failures: RefCell<Vec<CodeBuddyBoundedFailure>>,
    last_counted_ordinal_in_batch: Option<u64>,
    emitted_metadata_refresh: bool,
    complete_content_binding: CodeBuddyCliCompleteContentBinding,
}

impl CodeBuddyCliCapturedBatchProjector {
    fn fresh(
        context: ProviderAdapterContext,
        path: PathBuf,
        session_ordinal: usize,
        complete_content_binding: CodeBuddyCliCompleteContentBinding,
    ) -> Self {
        let fallback_session_id = path
            .file_stem()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("unknown-session")
            .to_owned();
        Self {
            project_hash: codebuddy_cli_project_hash(&path),
            native_session_id: fallback_session_id.clone(),
            context,
            path,
            session_ordinal,
            next_ordinal: Cell::new(0),
            discovered_session_id: false,
            cwd: None,
            started_at: None,
            ended_at: None,
            title: None,
            generated_title_record: None,
            row_count: 0,
            counts: CodeBuddyProjectionCounts::default(),
            structural_rejections: Cell::new(0),
            structural_failures: RefCell::new(Vec::new()),
            last_counted_ordinal_in_batch: None,
            emitted_metadata_refresh: false,
            complete_content_binding,
        }
    }

    fn resume(
        context: ProviderAdapterContext,
        path: PathBuf,
        session_ordinal: usize,
        cursor: &CertifiedProviderCursor,
        complete_content_binding: CodeBuddyCliCompleteContentBinding,
    ) -> Result<Option<Self>> {
        let checkpoint: CodeBuddyCliParserCheckpoint = cursor.parser_checkpoint().deserialize()?;
        let title = checkpoint
            .generated_title_record
            .as_ref()
            .map(|anchor| {
                codebuddy_cli_generated_title_at(
                    &path,
                    *anchor,
                    checkpoint.next_ordinal,
                    jsonl_position_offset(cursor.native_position())
                        .map_err(codebuddy_jsonl_error)?,
                )
            })
            .transpose()?
            .flatten();
        if checkpoint.generated_title_record.is_some() && title.is_none() {
            return Ok(None);
        }
        let mut counts = checkpoint.counts;
        counts.rejected_records = counts.rejected_records.max(cursor.rejected_records());
        Ok(Some(Self {
            project_hash: codebuddy_cli_project_hash(&path),
            context,
            path,
            session_ordinal,
            next_ordinal: Cell::new(checkpoint.next_ordinal),
            native_session_id: checkpoint.native_session_id,
            discovered_session_id: checkpoint.discovered_session_id,
            cwd: checkpoint.cwd,
            started_at: codebuddy_checkpoint_time(checkpoint.started_at, "CLI start time")?,
            ended_at: codebuddy_checkpoint_time(checkpoint.ended_at, "CLI end time")?,
            title,
            generated_title_record: checkpoint.generated_title_record,
            row_count: checkpoint.row_count,
            counts,
            structural_rejections: Cell::new(0),
            structural_failures: RefCell::new(Vec::new()),
            last_counted_ordinal_in_batch: None,
            emitted_metadata_refresh: false,
            complete_content_binding,
        }))
    }

    fn replay_summary(&self) -> Result<ProviderImportSummary> {
        let counts = self.counts_for_cursor()?;
        let mut summary = counts.replay_summary()?;
        if counts.accepted_captures == 0
            && counts.rejected_records == 0
            && self.next_ordinal.get() != 0
        {
            codebuddy_mark_skipped_session(&mut summary);
        }
        Ok(summary)
    }

    fn is_empty_session(&self) -> bool {
        self.counts.accepted_captures == 0
            && self.counts.rejected_records == 0
            && self.structural_rejections.get() == 0
    }

    fn counts_for_cursor(&self) -> Result<CodeBuddyProjectionCounts> {
        let mut counts = self.counts.clone();
        counts.rejected_records = counts
            .rejected_records
            .checked_add(self.structural_rejections.get())
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy structural rejection count overflowed",
            ))?;
        let remaining = CODEBUDDY_MAX_CHECKPOINT_FAILURES.saturating_sub(counts.failures.len());
        counts.failures.extend(
            self.structural_failures
                .borrow()
                .iter()
                .take(remaining)
                .cloned(),
        );
        Ok(counts)
    }

    fn observe_structural_rejections(&self, batch: &CapturedBatch) -> Result<()> {
        let mut failures = self.structural_failures.borrow_mut();
        for record in batch.records() {
            let CapturedRecordPayload::StructuralRejection {
                kind: StructuralRejectionKind::OversizeRecord,
                observed_bytes,
            } = record.payload()
            else {
                continue;
            };
            self.structural_rejections
                .set(self.structural_rejections.get().checked_add(1).ok_or(
                    CaptureError::SystemInvariant(
                        "CodeBuddy structural rejection count overflowed",
                    ),
                )?);
            if self.counts.failures.len().saturating_add(failures.len())
                < CODEBUDDY_MAX_CHECKPOINT_FAILURES
            {
                let line = usize::try_from(record.ordinal())
                    .ok()
                    .and_then(|ordinal| ordinal.checked_add(1))
                    .ok_or(CaptureError::SystemInvariant(
                        "CodeBuddy structural rejection line exceeds platform limits",
                    ))?;
                failures.push(CodeBuddyBoundedFailure {
                    line,
                    error: format!(
                        "provider record exceeds the {} byte limit (observed {observed_bytes} bytes)",
                        MAX_PROVIDER_JSONL_LINE_BYTES
                    ),
                });
            }
        }
        Ok(())
    }

    fn capture_for_event(
        &self,
        event: CodeBuddyEventInput,
    ) -> ProviderProjectionResult<ProviderCaptureEnvelope> {
        let provider_session_id = format!("{}/{}", self.project_hash, self.native_session_id);
        let source_path = self.path.display().to_string();
        let file_names = ["projects/*/*.jsonl"];
        let session_index = json!({
            "source": "codebuddy_cli_jsonl",
            "path": source_path,
            "rows": self.row_count,
        });
        let started_at = self.started_at.ok_or_else(|| {
            ProviderProjectionFatal::system_invariant("CodeBuddy CLI projector lost its start time")
        })?;
        Ok(codebuddy_capture(
            &CodeBuddyCaptureDraft {
                provider_session_id: &provider_session_id,
                native_session_id: &self.native_session_id,
                project_hash: &self.project_hash,
                raw_source_path: &source_path,
                context: &self.context,
                started_at,
                ended_at: self.ended_at,
                title: self.title.as_deref(),
                cwd: self.cwd.as_deref(),
                project_index: None,
                conversation: None,
                session_index: &session_index,
                file_names: &file_names,
                shape: CodeBuddyNativeShape::Cli,
            },
            event,
        ))
    }

    fn final_metadata_for_record(
        &self,
        record: &CapturedRecord,
        physical_line: usize,
        value: &Value,
    ) -> ProviderProjectionResult<ProviderCaptureEnvelope> {
        let provider_session_id = format!("{}/{}", self.project_hash, self.native_session_id);
        let source_path = self.path.display().to_string();
        let file_names = ["projects/*/*.jsonl"];
        let session_index = json!({
            "source": "codebuddy_cli_jsonl",
            "path": source_path,
            "rows": self.row_count,
        });
        let started_at = self.started_at.ok_or_else(|| {
            ProviderProjectionFatal::system_invariant("CodeBuddy CLI projector lost its start time")
        })?;
        let native_record_id = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("line-{physical_line}"));
        let cursor = format!("{provider_session_id}:{native_record_id}");
        let observed_at = codebuddy_cli_message_time(value, self.context.imported_at);
        let capture = codebuddy_capture_envelope(
            &CodeBuddyCaptureDraft {
                provider_session_id: &provider_session_id,
                native_session_id: &self.native_session_id,
                project_hash: &self.project_hash,
                raw_source_path: &source_path,
                context: &self.context,
                started_at,
                ended_at: self.ended_at,
                title: self.title.as_deref(),
                cwd: self.cwd.as_deref(),
                project_index: None,
                conversation: None,
                session_index: &session_index,
                file_names: &file_names,
                shape: CodeBuddyNativeShape::Cli,
            },
            cursor,
            observed_at,
            None,
        );
        if record.ordinal().checked_add(1) != u64::try_from(physical_line).ok() {
            return Err(ProviderProjectionFatal::system_invariant(
                "CodeBuddy CLI final metadata line does not match its record ordinal",
            ));
        }
        Ok(capture)
    }
}

impl CapturedBatchProjector for CodeBuddyCliCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if record.record_kind().as_str() != CODEBUDDY_CLI_RECORD_KIND {
            return Err(ProviderProjectionFatal::system_invariant(
                "CodeBuddy CLI projector received an unexpected record kind",
            ));
        }
        let expected = self.next_ordinal.get();
        if record.ordinal() < expected {
            return Err(ProviderProjectionFatal::system_invariant(
                "CodeBuddy CLI captured record ordinal moved backwards",
            ));
        }
        self.next_ordinal
            .set(record.ordinal().checked_add(1).ok_or_else(|| {
                ProviderProjectionFatal::system_invariant("CodeBuddy CLI record ordinal overflowed")
            })?);
        let physical_line = usize::try_from(record.ordinal())
            .ok()
            .and_then(|ordinal| ordinal.checked_add(1))
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "CodeBuddy CLI line number exceeds platform limits",
                )
            })?;
        let CapturedRecordPayload::NativeBytes(bytes) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "CodeBuddy CLI projector requires native JSONL bytes",
            ));
        };
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let value = match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => value,
            Err(error) => {
                return self.counts.reject(
                    output,
                    physical_line,
                    format!("{}: malformed JSONL: {error}", self.path.display()),
                );
            }
        };
        self.row_count = self.row_count.checked_add(1).ok_or_else(|| {
            ProviderProjectionFatal::system_invariant("CodeBuddy CLI row count overflowed")
        })?;
        self.last_counted_ordinal_in_batch = Some(record.ordinal());
        if !self.discovered_session_id {
            if let Some(session_id) = value
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .and_then(codebuddy_bounded_checkpoint_text)
            {
                self.native_session_id = session_id;
                self.discovered_session_id = true;
            }
        }
        if self.cwd.is_none() {
            self.cwd = value
                .get("cwd")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|cwd| !cwd.is_empty())
                .and_then(codebuddy_bounded_checkpoint_text);
        }
        let Some(event) = codebuddy_cli_event(
            record,
            physical_line,
            value.clone(),
            self.context.imported_at,
        ) else {
            return Ok(());
        };
        let occurred_at = event.occurred_at;
        self.started_at = Some(
            self.started_at
                .map(|started_at| started_at.min(occurred_at))
                .unwrap_or(occurred_at),
        );
        self.ended_at = Some(
            self.ended_at
                .map(|ended_at| ended_at.max(occurred_at))
                .unwrap_or(occurred_at),
        );
        if self.title.is_none() && provider_role(event.role.as_deref()) == EventRole::User {
            self.title = codebuddy_title_from_text(&event.text);
            if self.title.is_some() {
                self.generated_title_record = Some(
                    codebuddy_cli_record_anchor(record).map_err(ProviderProjectionFatal::new)?,
                );
            }
        }
        let line_number = self
            .session_ordinal
            .saturating_mul(10_000)
            .saturating_add(physical_line);
        let mut capture = self.capture_for_event(event)?;
        self.complete_content_binding
            .attach(&mut capture, &value, record, physical_line)
            .map_err(ProviderProjectionFatal::new)?;
        emit_projected_normalization_units(
            output,
            ProviderNormalizationResult {
                captures: vec![(line_number, capture)],
                ..ProviderNormalizationResult::default()
            },
        )?;
        self.counts.accept()?;
        Ok(())
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        if *position != initial_jsonl_position().map_err(codebuddy_jsonl_error)?
            || self.next_ordinal.get() != 0
        {
            return Err(CaptureError::InvalidPayload(
                "CodeBuddy CLI initial cursor candidate is not at the JSONL source start"
                    .to_owned(),
            ));
        }
        CertifiedProviderCursor::new(
            source.source_revision(),
            source.capture_revision(),
            source.policy_revision(),
            position.clone(),
            BoundedParserCheckpoint::from_serializable(&CodeBuddyCliParserCheckpoint {
                next_ordinal: 0,
                native_session_id: self.native_session_id.clone(),
                discovered_session_id: false,
                cwd: None,
                started_at: None,
                ended_at: None,
                generated_title_record: None,
                row_count: 0,
                counts: CodeBuddyProjectionCounts::default(),
            })?,
        )
    }

    fn final_metadata_capture(
        &mut self,
        batch: &CapturedBatch,
    ) -> ProviderProjectionResult<Option<(usize, ProviderCaptureEnvelope)>> {
        let Some(ordinal) = self.last_counted_ordinal_in_batch.take() else {
            return Ok(None);
        };
        if self.counts.accepted_captures == 0 {
            return Ok(None);
        }
        let record = batch
            .records()
            .iter()
            .find(|record| record.ordinal() == ordinal)
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "CodeBuddy CLI final metadata record is outside the captured batch",
                )
            })?;
        let CapturedRecordPayload::NativeBytes(bytes) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "CodeBuddy CLI final metadata requires native JSONL bytes",
            ));
        };
        let value = serde_json::from_slice::<Value>(bytes).map_err(|_| {
            ProviderProjectionFatal::system_invariant(
                "CodeBuddy CLI accepted final metadata record is not valid JSON",
            )
        })?;
        let physical_line = usize::try_from(ordinal)
            .ok()
            .and_then(|ordinal| ordinal.checked_add(1))
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "CodeBuddy CLI final metadata ordinal exceeds platform limits",
                )
            })?;
        let capture = self.final_metadata_for_record(record, physical_line, &value)?;
        self.emitted_metadata_refresh = true;
        Ok(Some((physical_line, capture)))
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        self.observe_structural_rejections(batch)?;
        let next_ordinal = batch
            .records()
            .last()
            .and_then(|record| record.ordinal().checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy CLI captured batch did not have a next ordinal",
            ))?;
        if self.next_ordinal.get() > next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "CodeBuddy CLI projector advanced beyond the captured batch",
            ));
        }
        self.next_ordinal.set(next_ordinal);
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                batch.range_end().clone(),
                BoundedParserCheckpoint::from_serializable(&CodeBuddyCliParserCheckpoint {
                    next_ordinal,
                    native_session_id: self.native_session_id.clone(),
                    discovered_session_id: self.discovered_session_id,
                    cwd: self.cwd.clone(),
                    started_at: self.started_at.map(|value| value.to_rfc3339()),
                    ended_at: self.ended_at.map(|value| value.to_rfc3339()),
                    generated_title_record: self.generated_title_record,
                    row_count: self.row_count,
                    counts: self.counts_for_cursor()?,
                })?,
            )?,
        ))
    }
}

fn codebuddy_cli_event(
    record: &CapturedRecord,
    physical_line: usize,
    value: Value,
    imported_at: DateTime<Utc>,
) -> Option<CodeBuddyEventInput> {
    let text = codebuddy_cli_message_text(&value);
    if value.get("type").and_then(Value::as_str) != Some("message") || text.trim().is_empty() {
        return None;
    }
    Some(CodeBuddyEventInput {
        provider_event_index: record.ordinal(),
        native_message_id: value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("line-{physical_line}")),
        role: value.get("role").and_then(Value::as_str).map(str::to_owned),
        ref_type: value.get("type").and_then(Value::as_str).map(str::to_owned),
        occurred_at: codebuddy_cli_message_time(&value, imported_at),
        text,
        raw_message: value.clone(),
        decoded_message: value,
    })
}

fn codebuddy_cli_record_anchor(record: &CapturedRecord) -> Result<CodeBuddyCliRecordAnchor> {
    let locator = record.locator();
    if locator.kind() != CODEBUDDY_CLI_LOCATOR_KIND {
        return Err(CaptureError::InvalidPayload(
            "CodeBuddy CLI record has an invalid JSONL locator".to_owned(),
        ));
    }
    let value = locator.value();
    let source_length_bytes = value.get(..4).ok_or_else(|| {
        CaptureError::InvalidPayload("CodeBuddy CLI JSONL locator is truncated".to_owned())
    })?;
    let source_length = usize::try_from(u32::from_be_bytes(
        source_length_bytes.try_into().map_err(|_| {
            CaptureError::InvalidPayload(
                "CodeBuddy CLI JSONL locator has an invalid source length".to_owned(),
            )
        })?,
    ))
    .map_err(|_| {
        CaptureError::InvalidPayload(
            "CodeBuddy CLI JSONL locator source length exceeds platform limits".to_owned(),
        )
    })?;
    let range_start = 4_usize.checked_add(source_length).ok_or_else(|| {
        CaptureError::InvalidPayload("CodeBuddy CLI JSONL locator length overflowed".to_owned())
    })?;
    let expected_length = range_start.checked_add(16).ok_or_else(|| {
        CaptureError::InvalidPayload("CodeBuddy CLI JSONL locator length overflowed".to_owned())
    })?;
    if value.len() != expected_length {
        return Err(CaptureError::InvalidPayload(
            "CodeBuddy CLI JSONL locator has an invalid length".to_owned(),
        ));
    }
    let start = u64::from_be_bytes(value[range_start..range_start + 8].try_into().map_err(
        |_| CaptureError::InvalidPayload("CodeBuddy CLI JSONL locator start is invalid".to_owned()),
    )?);
    let end = u64::from_be_bytes(value[range_start + 8..expected_length].try_into().map_err(
        |_| CaptureError::InvalidPayload("CodeBuddy CLI JSONL locator end is invalid".to_owned()),
    )?);
    if start >= end {
        return Err(CaptureError::InvalidPayload(
            "CodeBuddy CLI JSONL locator range is invalid".to_owned(),
        ));
    }
    let CapturedRecordPayload::NativeBytes(payload) = record.payload() else {
        return Err(CaptureError::SystemInvariant(
            "CodeBuddy CLI title anchor requires native JSONL bytes",
        ));
    };
    Ok(CodeBuddyCliRecordAnchor {
        ordinal: record.ordinal(),
        start,
        end,
        payload_sha256: codebuddy_cli_title_anchor_digest(payload),
    })
}

fn codebuddy_cli_title_anchor_digest(payload: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(CODEBUDDY_CLI_TITLE_ANCHOR_HASH_DOMAIN);
    hasher.update((payload.len() as u64).to_be_bytes());
    hasher.update(payload);
    hasher.finalize().into()
}

fn codebuddy_cli_generated_title_at(
    path: &Path,
    anchor: CodeBuddyCliRecordAnchor,
    next_ordinal: u64,
    cursor_offset: u64,
) -> Result<Option<String>> {
    if anchor.ordinal >= next_ordinal || anchor.end > cursor_offset || anchor.start >= anchor.end {
        return Err(CaptureError::InvalidPayload(
            "CodeBuddy CLI title anchor exceeds its certified cursor".to_owned(),
        ));
    }
    let length = usize::try_from(anchor.end - anchor.start).map_err(|_| {
        CaptureError::InvalidPayload(
            "CodeBuddy CLI title anchor length exceeds platform limits".to_owned(),
        )
    })?;
    if length > MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2) {
        return Err(CaptureError::InvalidPayload(
            "CodeBuddy CLI title anchor exceeds the provider record limit".to_owned(),
        ));
    }
    ensure_regular_provider_transcript_file(path)?;
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(anchor.start))?;
    let mut bytes = vec![0_u8; length];
    file.read_exact(&mut bytes)?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if codebuddy_cli_title_anchor_digest(&bytes) != anchor.payload_sha256 {
        return Ok(None);
    }
    let value = serde_json::from_slice::<Value>(&bytes).map_err(|_| {
        CaptureError::InvalidPayload(
            "CodeBuddy CLI title anchor is not valid JSONL content".to_owned(),
        )
    })?;
    let role = value.get("role").and_then(Value::as_str);
    if value.get("type").and_then(Value::as_str) != Some("message")
        || provider_role(role) != EventRole::User
    {
        return Err(CaptureError::InvalidPayload(
            "CodeBuddy CLI title anchor does not identify a user message".to_owned(),
        ));
    }
    codebuddy_title_from_text(&codebuddy_cli_message_text(&value))
        .map(Some)
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "CodeBuddy CLI title anchor does not identify title text".to_owned(),
            )
        })
}

pub(super) fn import_jsonl_file_batched(
    path: &Path,
    session_ordinal: usize,
    store: &mut Store,
    context: &ProviderAdapterContext,
    import_options: &NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let frozen = CodeBuddyFrozenFile::read(path)?;
    let canonical_path = fs::canonicalize(path)?;
    let path_identity = provider_path_identity(&canonical_path)?;
    let file_context = ProviderAdapterContext {
        machine_id: context.machine_id.clone(),
        source_path: Some(path.to_path_buf()),
        source_root: context
            .source_root
            .clone()
            .or_else(|| context.source_path.clone()),
        imported_at: context.imported_at,
    };
    let source = SourceObservation::new(
        CaptureProvider::CodeBuddy,
        CODEBUDDY_SOURCE_FORMAT,
        format!("codebuddy-cli-jsonl:{path_identity}"),
        frozen.source_revision_with_policy("cli-jsonl", CODEBUDDY_CLI_POLICY_REVISION),
        provider_source_cursor_stream_for_path(
            CaptureProvider::CodeBuddy,
            CODEBUDDY_SOURCE_FORMAT,
            &path_identity,
        ),
        CODEBUDDY_CAPTURE_REVISION,
        CODEBUDDY_CLI_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(codebuddy_captured_batch_error)?;
    let record_kind = ProviderRecordKind::new(CODEBUDDY_CLI_RECORD_KIND)
        .map_err(codebuddy_captured_batch_error)?;
    let initial_position = initial_jsonl_position().map_err(codebuddy_jsonl_error)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let had_expected_store_cursor = expected_store_cursor.is_some();
    let mut cursor_mode = CapturedBatchCursorMode::Resume;
    let mut start_offset = 0_u64;
    let mut start_ordinal = 0_u64;
    let mut resumed_projector = None;
    let complete_content_binding =
        CodeBuddyCliCompleteContentBinding::for_source(&source, &path_identity);

    if let Some(stored_cursor) = expected_store_cursor.as_ref() {
        match CertifiedProviderCursor::decode_if_certified(&stored_cursor.cursor)? {
            Some(certified)
                if certified.parser_revision() == source.capture_revision()
                    && certified.policy_revision() == source.policy_revision() =>
            {
                let can_resume = if certified.source_revision() == source.source_revision() {
                    true
                } else {
                    let file = File::open(path)?;
                    if CodeBuddyFrozenFile::from_metadata(&file.metadata()?)? != frozen {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    let mut reader = BufReader::new(file);
                    match verify_jsonl_append_boundary(
                        &mut reader,
                        certified.native_position(),
                        &source,
                        frozen.length,
                    ) {
                        Ok(verified) => {
                            cursor_mode = CapturedBatchCursorMode::ResumeAppend(verified);
                            true
                        }
                        Err(JsonlBatchError::Io(error)) => return Err(CaptureError::Io(error)),
                        Err(_) => false,
                    }
                };
                if can_resume {
                    let projector = CodeBuddyCliCapturedBatchProjector::resume(
                        file_context.clone(),
                        path.to_path_buf(),
                        session_ordinal,
                        &certified,
                        complete_content_binding.clone(),
                    )?;
                    if let Some(projector) = projector {
                        start_offset = jsonl_position_offset(certified.native_position())
                            .map_err(codebuddy_jsonl_error)?;
                        start_ordinal = projector.next_ordinal.get();
                        resumed_projector = Some(projector);
                    } else if certified.source_revision() == source.source_revision() {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    } else {
                        cursor_mode = CapturedBatchCursorMode::ResetChangedSource;
                    }
                } else {
                    cursor_mode = CapturedBatchCursorMode::ResetChangedSource;
                }
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }

    let mut projector = resumed_projector.unwrap_or_else(|| {
        CodeBuddyCliCapturedBatchProjector::fresh(
            file_context.clone(),
            path.to_path_buf(),
            session_ordinal,
            complete_content_binding,
        )
    });
    if !frozen.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let file = File::open(path)?;
    if CodeBuddyFrozenFile::from_metadata(&file.metadata()?)? != frozen {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut producer = JsonlBatchProducer::new(
        BufReader::new(file),
        source.clone(),
        path_identity.into_bytes(),
        record_kind,
        frozen.length,
        start_offset,
        start_ordinal,
        true,
    )
    .map_err(codebuddy_jsonl_error)?;
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &file_context)?;
    let mut imported_any = false;
    let mut summary = drain_captured_batches(
        store,
        &admission,
        import_options.clone(),
        &context.machine_id,
        context.imported_at,
        expected_store_cursor,
        &initial_position,
        cursor_mode,
        &stream,
        &mut projector,
        || {
            let batch = producer.next_batch().map_err(codebuddy_jsonl_error)?;
            imported_any |= batch.is_some();
            Ok(batch)
        },
        || frozen.revalidate(path),
    )?;
    if projector.emitted_metadata_refresh && !summary.has_accepted_content() && summary.failed == 0
    {
        summary.accepted_content_records = 1;
    }
    if !imported_any && had_expected_store_cursor {
        projector.replay_summary()
    } else {
        if summary.failed == 0 && projector.is_empty_session() {
            codebuddy_mark_skipped_session(&mut summary);
        }
        Ok(summary)
    }
}

fn codebuddy_jsonl_error(error: JsonlBatchError) -> CaptureError {
    match error {
        JsonlBatchError::Io(error) => CaptureError::Io(error),
        JsonlBatchError::SourceChangedDuringRead { .. } => CaptureError::SourceChangedDuringCapture,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

fn codebuddy_cli_project_hash(path: &Path) -> String {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty() && *name != "projects")
        .map(str::to_owned)
        .unwrap_or_else(|| "unknown-project".to_owned())
}

fn codebuddy_cli_message_text(value: &Value) -> String {
    let text = value
        .get("content")
        .and_then(provider_value_text)
        .or_else(|| {
            value
                .pointer("/message/content")
                .and_then(provider_value_text)
        })
        .unwrap_or_default();
    codebuddy_clean_content(&text)
}

fn codebuddy_cli_message_time(value: &Value, fallback: DateTime<Utc>) -> DateTime<Utc> {
    value
        .get("timestamp")
        .and_then(Value::as_i64)
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .or_else(|| {
            value
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_rfc3339_utc)
        })
        .or_else(|| {
            value
                .get("__timestamp")
                .and_then(Value::as_str)
                .and_then(parse_rfc3339_utc)
        })
        .unwrap_or(fallback)
}

#[cfg(test)]
mod tests;
