use std::path::Path;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, Fidelity, ProviderCaptureEnvelope, ProviderCursorCheckpoint, ProviderCursorRange,
    ProviderEventEnvelope, ProviderSessionEnvelope, ProviderSourceEnvelope, ProviderSourceTrust,
    SessionStatus, PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, NativePosition, SourceObservation,
};
use crate::complete_content::structured::{
    attach_structured_complete_content_locator, attach_structured_result_content_locator,
};
use crate::provider::file_touches::{
    visit_provider_file_touches_from_raw_value, ProviderFileTouchSourceContext,
    PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
};
use crate::provider::importer::{
    emit_projected_normalization_units, provider_cursor_stream, BoundedParserCheckpoint,
    CapturedBatchCursorFinish, CapturedBatchProjector, CertifiedProviderCursor,
    ProviderProjectionFatal, ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::provider::normalization::{provider_capped_json, provider_local_preview};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportSummary, ProviderNormalizationResult,
    Result, PROVIDER_MAX_PREVIEW_CHARS, PROVIDER_MAX_TEXT_CHARS,
};

use super::dialect::{
    task_json_decode_locator, task_json_decode_position, TaskJsonMessagePhase,
    TaskJsonProviderSpec, TaskJsonRecordClass, TaskJsonStreamPosition, TASK_JSON_RECORD_KIND,
    TASK_JSON_TERMINAL_PHASE,
};
use super::normalization::{
    task_json_event, task_json_event_text, task_json_event_time, task_json_event_type,
    task_json_history_item_event, task_json_string_field, task_json_time_field, TaskJsonEventInput,
};
use super::scanner::TaskJsonByteReader;
use super::source::{read_task_json_value, TaskJsonTaskObservation};

#[derive(Debug, Clone)]
pub(super) struct TaskJsonStateFragment {
    pub(super) id: Option<String>,
    started_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
    cwd: Option<String>,
    completed: bool,
    preview: Value,
    pub(super) fallback_event: Option<Value>,
}

impl TaskJsonStateFragment {
    fn from_value(value: &Value, history_item: bool) -> Self {
        Self {
            id: task_json_bounded_string_field(value, &["taskId", "id"]),
            started_at: task_json_time_field(
                value,
                &["createdAt", "created_at", "ts", "timestamp"],
            ),
            ended_at: task_json_time_field(
                value,
                &["lastModified", "updatedAt", "completedAt", "last_modified"],
            ),
            cwd: task_json_bounded_string_field(
                value,
                &[
                    "cwd",
                    "workspace",
                    "workspacePath",
                    "cwdOnTaskInitialization",
                ],
            ),
            completed: value
                .get("isCompleted")
                .or_else(|| value.get("completed"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            preview: provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS),
            fallback_event: history_item
                .then(|| task_json_history_item_event_bounded(value))
                .flatten(),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct TaskJsonSessionState {
    pub(super) task_id: String,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    cwd: Option<String>,
    is_done: bool,
    task_metadata: Option<Value>,
    history_item: Option<Value>,
    index_item: Option<Value>,
    files: Vec<String>,
    fallback_event: Option<Value>,
    provider_content_hash: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskJsonSessionCheckpoint {
    pub(super) task_id: String,
    imported_at: DateTime<Utc>,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    cwd: Option<String>,
    is_done: bool,
    files: Vec<String>,
    provider_content_hash: [u8; 32],
}

impl TaskJsonSessionState {
    fn checkpoint(&self, imported_at: DateTime<Utc>) -> TaskJsonSessionCheckpoint {
        TaskJsonSessionCheckpoint {
            task_id: self.task_id.clone(),
            imported_at,
            started_at: self.started_at,
            ended_at: self.ended_at,
            cwd: self.cwd.clone(),
            is_done: self.is_done,
            files: self.files.clone(),
            provider_content_hash: self.provider_content_hash,
        }
    }

    fn rehydrate(
        checkpoint: &TaskJsonSessionCheckpoint,
        observed: TaskJsonSessionState,
    ) -> Result<Self> {
        if observed.task_id != checkpoint.task_id
            || observed.files != checkpoint.files
            || observed.provider_content_hash != checkpoint.provider_content_hash
        {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(Self {
            task_id: checkpoint.task_id.clone(),
            started_at: checkpoint.started_at,
            ended_at: checkpoint.ended_at,
            cwd: checkpoint.cwd.clone(),
            is_done: checkpoint.is_done,
            task_metadata: observed.task_metadata,
            history_item: observed.history_item,
            index_item: observed.index_item,
            files: checkpoint.files.clone(),
            fallback_event: observed.fallback_event,
            provider_content_hash: checkpoint.provider_content_hash,
        })
    }
}

fn task_json_provider_content_hash(
    task_metadata: &Option<Value>,
    history_item: &Option<Value>,
    index_item: &Option<Value>,
    fallback_event: &Option<Value>,
) -> Result<[u8; 32]> {
    Ok(Sha256::digest(serde_json::to_vec(&(
        task_metadata,
        history_item,
        index_item,
        fallback_event,
    ))?)
    .into())
}

fn task_json_state_failures_hash(failures: &[String]) -> Result<[u8; 32]> {
    Ok(Sha256::digest(serde_json::to_vec(failures)?).into())
}

fn task_json_bounded_string_field(value: &Value, fields: &[&str]) -> Option<String> {
    task_json_string_field(value, fields)
        .map(|value| provider_local_preview(&value, PROVIDER_MAX_TEXT_CHARS).0)
}

fn task_json_history_item_event_bounded(value: &Value) -> Option<Value> {
    let mut event = task_json_history_item_event(value)?;
    if let Some(content) = event.get_mut("content").and_then(|value| value.as_str()) {
        let bounded = provider_local_preview(content, PROVIDER_MAX_TEXT_CHARS).0;
        event["content"] = Value::String(bounded);
    }
    Some(event)
}

fn task_json_read_state_fragment(
    observation: &TaskJsonTaskObservation,
    spec: TaskJsonProviderSpec,
    file_name: Option<&str>,
    history_item: bool,
    failures: &mut Vec<String>,
) -> Option<TaskJsonStateFragment> {
    let file_name = file_name?;
    let observed = observation.marker_file(spec, file_name)?;
    observed.frozen.as_ref()?;
    match read_task_json_value(
        &observed.path,
        &ProviderAdapterContext {
            machine_id: String::new(),
            source_path: None,
            source_root: None,
            imported_at: DateTime::<Utc>::UNIX_EPOCH,
        },
    ) {
        Ok(value) => Some(TaskJsonStateFragment::from_value(&value, history_item)),
        Err(error) => {
            failures.push(format!("{file_name}: {error}"));
            None
        }
    }
}

pub(super) fn task_json_root_history_fragment(
    observation: &TaskJsonTaskObservation,
    task_id: &str,
) -> Result<Option<TaskJsonStateFragment>> {
    for observed in &observation.root_history_files {
        let Some(frozen) = observed.frozen.as_ref() else {
            continue;
        };
        let mut reader = TaskJsonByteReader::open(&observed.path, frozen, 0)?;
        reader.skip_whitespace()?;
        if reader.read_byte()? != Some(b'[') {
            continue;
        }
        let mut any_id = false;
        let mut matched = None;
        let mut valid = true;
        loop {
            reader.skip_whitespace()?;
            if reader.peek_byte()? == Some(b']') {
                reader.read_byte()?;
                break;
            }
            let Some(item) = reader.scanned_value(true)? else {
                valid = false;
                break;
            };
            if !item.complete || !item.retained_all() {
                valid = false;
                break;
            }
            let Ok(value) = serde_json::from_slice::<Value>(&item.bytes) else {
                valid = false;
                break;
            };
            if let Some(id) = task_json_string_field(&value, &["id", "taskId"]) {
                any_id = true;
                if id == task_id {
                    matched = Some(TaskJsonStateFragment::from_value(&value, true));
                }
            }
            reader.skip_whitespace()?;
            match reader.read_byte()? {
                Some(b',') => continue,
                Some(b']') => break,
                _ => {
                    valid = false;
                    break;
                }
            }
        }
        if valid && any_id {
            return Ok(matched);
        }
    }
    Ok(None)
}

pub(super) fn task_json_session_state(
    task_dir: &Path,
    observation: &TaskJsonTaskObservation,
    context: &ProviderAdapterContext,
    spec: TaskJsonProviderSpec,
) -> Result<(TaskJsonSessionState, Vec<String>)> {
    let mut failures = Vec::new();
    let metadata = task_json_read_state_fragment(
        observation,
        spec,
        Some(spec.metadata_file),
        false,
        &mut failures,
    );
    let history = task_json_read_state_fragment(
        observation,
        spec,
        spec.history_item_file,
        true,
        &mut failures,
    );
    let index =
        task_json_read_state_fragment(observation, spec, spec.index_file, false, &mut failures);
    let task_id = metadata
        .as_ref()
        .and_then(|fragment| fragment.id.clone())
        .or_else(|| history.as_ref().and_then(|fragment| fragment.id.clone()))
        .or_else(|| index.as_ref().and_then(|fragment| fragment.id.clone()))
        .or_else(|| {
            task_dir
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.trim().is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown-task".to_owned());
    let root_history = task_json_root_history_fragment(observation, &task_id)?;
    let selected_history = history.as_ref().or(root_history.as_ref());
    let started_at = metadata
        .as_ref()
        .and_then(|fragment| fragment.started_at)
        .or_else(|| history.as_ref().and_then(|fragment| fragment.started_at))
        .or_else(|| index.as_ref().and_then(|fragment| fragment.started_at))
        .or_else(|| {
            root_history
                .as_ref()
                .and_then(|fragment| fragment.started_at)
        })
        .unwrap_or(context.imported_at);
    let ended_at = metadata
        .as_ref()
        .and_then(|fragment| fragment.ended_at)
        .or_else(|| history.as_ref().and_then(|fragment| fragment.ended_at))
        .or_else(|| index.as_ref().and_then(|fragment| fragment.ended_at));
    let cwd = metadata
        .as_ref()
        .and_then(|fragment| fragment.cwd.clone())
        .or_else(|| history.as_ref().and_then(|fragment| fragment.cwd.clone()))
        .or_else(|| index.as_ref().and_then(|fragment| fragment.cwd.clone()))
        .or_else(|| {
            root_history
                .as_ref()
                .and_then(|fragment| fragment.cwd.clone())
        });

    let mut files = Vec::new();
    if metadata.is_some() {
        files.push(spec.metadata_file.to_owned());
    }
    if history.is_some() {
        if let Some(file) = spec.history_item_file {
            files.push(file.to_owned());
        }
    }
    if index.is_some() {
        if let Some(file) = spec.index_file {
            files.push(file.to_owned());
        }
    }
    for phase in [
        TaskJsonMessagePhase::Api,
        TaskJsonMessagePhase::Ui,
        TaskJsonMessagePhase::Fallback,
    ] {
        if observation.message_file(spec, phase).is_some() {
            if let Some(file) = phase.file_name(spec) {
                files.push(file.to_owned());
            }
        }
    }

    let task_metadata = metadata.map(|fragment| fragment.preview);
    let history_item = selected_history.map(|fragment| fragment.preview.clone());
    let index_item = index.map(|fragment| fragment.preview);
    let fallback_event = selected_history.and_then(|fragment| fragment.fallback_event.clone());
    let provider_content_hash = task_json_provider_content_hash(
        &task_metadata,
        &history_item,
        &index_item,
        &fallback_event,
    )?;

    Ok((
        TaskJsonSessionState {
            task_id,
            started_at,
            ended_at,
            cwd,
            is_done: selected_history
                .map(|fragment| fragment.completed)
                .unwrap_or(false),
            task_metadata,
            history_item,
            index_item,
            files,
            fallback_event,
            provider_content_hash,
        },
        failures,
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskJsonParserCheckpoint {
    pub(super) session: TaskJsonSessionCheckpoint,
    pub(super) state_failures_count: u64,
    pub(super) state_failures_hash: [u8; 32],
    pub(super) state_failures_reported: bool,
    pub(super) next_record_ordinal: u64,
    pub(super) next_event_ordinal: u64,
    pub(super) accepted_captures: u64,
    pub(super) accepted_events: u64,
    pub(super) accepted_file_touches: u64,
    pub(super) rejected_records: u64,
    pub(super) terminal_seen: bool,
}

pub(super) struct TaskJsonCapturedBatchProjector {
    spec: TaskJsonProviderSpec,
    context: ProviderAdapterContext,
    raw_source_path: String,
    pub(super) session: TaskJsonSessionState,
    state_failures: Vec<String>,
    pub(super) checkpoint: TaskJsonParserCheckpoint,
}

struct TaskJsonProjectEventInput<'a> {
    raw: Value,
    source: &'static str,
    native_index: usize,
    source_record_ordinal: u64,
    record_bytes: &'a [u8],
    line_number: usize,
}

impl TaskJsonCapturedBatchProjector {
    pub(super) fn fresh_checkpoint(
        session: &TaskJsonSessionState,
        state_failures: &[String],
        imported_at: DateTime<Utc>,
    ) -> Result<TaskJsonParserCheckpoint> {
        let state_failures_count = u64::try_from(state_failures.len()).map_err(|_| {
            CaptureError::SystemInvariant("task JSON state failure count exceeds u64")
        })?;
        Ok(TaskJsonParserCheckpoint {
            session: session.checkpoint(imported_at),
            state_failures_count,
            state_failures_hash: task_json_state_failures_hash(state_failures)?,
            state_failures_reported: false,
            next_record_ordinal: 0,
            next_event_ordinal: 0,
            accepted_captures: 0,
            accepted_events: 0,
            accepted_file_touches: 0,
            rejected_records: 0,
            terminal_seen: false,
        })
    }

    pub(super) fn fresh(
        spec: TaskJsonProviderSpec,
        context: ProviderAdapterContext,
        raw_source_path: String,
        session: TaskJsonSessionState,
        state_failures: Vec<String>,
    ) -> Result<Self> {
        let checkpoint = Self::fresh_checkpoint(&session, &state_failures, context.imported_at)?;
        Ok(Self {
            spec,
            context,
            raw_source_path,
            session,
            state_failures,
            checkpoint,
        })
    }

    pub(super) fn resume(
        spec: TaskJsonProviderSpec,
        mut context: ProviderAdapterContext,
        raw_source_path: String,
        observed_session: TaskJsonSessionState,
        state_failures: Vec<String>,
        cursor: &CertifiedProviderCursor,
    ) -> Result<Self> {
        let checkpoint = Self::checkpoint_for_cursor(cursor)?;
        let state_failures_count = u64::try_from(state_failures.len()).map_err(|_| {
            CaptureError::SystemInvariant("task JSON state failure count exceeds u64")
        })?;
        if state_failures_count != checkpoint.state_failures_count
            || task_json_state_failures_hash(&state_failures)? != checkpoint.state_failures_hash
        {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let session = TaskJsonSessionState::rehydrate(&checkpoint.session, observed_session)?;
        context.imported_at = checkpoint.session.imported_at;
        Ok(Self {
            spec,
            context,
            raw_source_path,
            session,
            state_failures,
            checkpoint,
        })
    }

    fn checkpoint_for_cursor(cursor: &CertifiedProviderCursor) -> Result<TaskJsonParserCheckpoint> {
        let checkpoint: TaskJsonParserCheckpoint = cursor.parser_checkpoint().deserialize()?;
        let position = task_json_decode_position(cursor.native_position())?;
        if checkpoint.next_record_ordinal != position.ordinal {
            return Err(CaptureError::InvalidPayload(
                "task JSON parser checkpoint does not match its native position".to_owned(),
            ));
        }
        Ok(checkpoint)
    }

    fn line_number(&mut self, record: &CapturedRecord) -> Result<usize> {
        if record.ordinal() != self.checkpoint.next_record_ordinal {
            return Err(CaptureError::SystemInvariant(
                "task JSON projector received a noncontiguous record ordinal",
            ));
        }
        self.checkpoint.next_record_ordinal =
            record
                .ordinal()
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "task JSON projected record ordinal overflowed",
                ))?;
        usize::try_from(record.ordinal())
            .ok()
            .and_then(|ordinal| ordinal.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "task JSON record ordinal exceeds platform limits",
            ))
    }

    fn reject(
        &mut self,
        output: &mut dyn ProviderProjectionOutput,
        line_number: usize,
        reason: String,
    ) -> ProviderProjectionResult<()> {
        self.checkpoint.rejected_records = self
            .checkpoint
            .rejected_records
            .checked_add(1)
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "task JSON projected rejection count overflowed",
                )
            })?;
        output.reject_record(line_number, reason);
        Ok(())
    }

    fn report_state_failures(
        &mut self,
        output: &mut dyn ProviderProjectionOutput,
        line_number: usize,
    ) -> ProviderProjectionResult<()> {
        if self.checkpoint.state_failures_reported {
            return Ok(());
        }
        for failure in self.state_failures.clone() {
            self.reject(output, line_number, failure)?;
        }
        self.checkpoint.state_failures_reported = true;
        Ok(())
    }

    fn project_event(
        &mut self,
        input: TaskJsonProjectEventInput<'_>,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let TaskJsonProjectEventInput {
            raw,
            source,
            native_index,
            source_record_ordinal,
            record_bytes,
            line_number,
        } = input;
        let event_ordinal = usize::try_from(self.checkpoint.next_event_ordinal)
            .map_err(|_| {
                CaptureError::SystemInvariant("task JSON event ordinal exceeds platform limits")
            })
            .map_err(ProviderProjectionFatal::new)?;
        let fallback_millis = i64::try_from(self.checkpoint.next_event_ordinal)
            .map_err(|_| CaptureError::SystemInvariant("task JSON event time offset exceeds i64"))
            .map_err(ProviderProjectionFatal::new)?;
        let occurred_at = task_json_event_time(&raw).unwrap_or_else(|| {
            self.session.started_at + chrono::Duration::milliseconds(fallback_millis)
        });
        let mut event = task_json_event(
            self.spec,
            &self.session.task_id,
            TaskJsonEventInput {
                source,
                native_index,
                raw: raw.clone(),
            },
            event_ordinal,
            occurred_at,
        );
        let event_type = task_json_event_type(&raw, source);
        let complete_text = task_json_event_text(&raw, source, event_type);
        let native_id = event.provider_event_hash.clone().unwrap_or_default();
        attach_structured_complete_content_locator(
            self.spec.provider,
            &mut event,
            source_record_ordinal,
            0,
            &native_id,
            record_bytes,
            &complete_text,
        )
        .map_err(ProviderProjectionFatal::new)?;
        if let Some(content) = super::task_json_result_content(&raw, source) {
            attach_structured_result_content_locator(
                self.spec.provider,
                &mut event,
                source_record_ordinal,
                0,
                &native_id,
                record_bytes,
                &content,
            )
            .map_err(ProviderProjectionFatal::new)?;
        }
        let source_root = self.context.source_root_display();
        let capture = task_json_capture_from_state(
            self.spec,
            Some(self.raw_source_path.as_str()),
            &self.context,
            &self.session,
            Some(event.clone()),
        );
        output.use_explicit_file_touches();
        emit_projected_normalization_units(
            output,
            ProviderNormalizationResult {
                captures: vec![(line_number, capture)],
                ..ProviderNormalizationResult::default()
            },
        )?;
        let file_touch_outcome = visit_provider_file_touches_from_raw_value(
            ProviderFileTouchSourceContext::new(
                self.spec.provider,
                &self.session.task_id,
                self.spec.source_format,
                Some(self.raw_source_path.as_str()),
                source_root.as_deref(),
            ),
            &raw,
            &event,
            line_number,
            |file_touch| {
                output.emit_normalization(ProviderNormalizationResult {
                    files_touched: vec![file_touch],
                    ..ProviderNormalizationResult::default()
                })
            },
        )?;
        if file_touch_outcome.limit_exceeded() {
            self.reject(
                output,
                line_number,
                PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
            )?;
        }
        let file_touch_count = u64::try_from(file_touch_outcome.emitted())
            .map_err(|_| CaptureError::SystemInvariant("task JSON file-touch count exceeds u64"))
            .map_err(ProviderProjectionFatal::new)?;
        self.checkpoint.accepted_captures = self
            .checkpoint
            .accepted_captures
            .checked_add(1)
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "task JSON projected capture count overflowed",
                )
            })?;
        self.checkpoint.accepted_events = self
            .checkpoint
            .accepted_events
            .checked_add(1)
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "task JSON projected event count overflowed",
                )
            })?;
        self.checkpoint.accepted_file_touches = self
            .checkpoint
            .accepted_file_touches
            .checked_add(file_touch_count)
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "task JSON projected file-touch count overflowed",
                )
            })?;
        self.checkpoint.next_event_ordinal = self
            .checkpoint
            .next_event_ordinal
            .checked_add(1)
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "task JSON projected event ordinal overflowed",
                )
            })?;
        Ok(())
    }

    fn replay_summary_for_checkpoint(
        checkpoint: &TaskJsonParserCheckpoint,
    ) -> Result<ProviderImportSummary> {
        let skipped_sessions = usize::from(checkpoint.accepted_captures != 0);
        let skipped_events = usize::try_from(checkpoint.accepted_events).map_err(|_| {
            CaptureError::SystemInvariant("task JSON replay event count exceeds platform limits")
        })?;
        let skipped_file_touches =
            usize::try_from(checkpoint.accepted_file_touches).map_err(|_| {
                CaptureError::SystemInvariant(
                    "task JSON replay file-touch count exceeds platform limits",
                )
            })?;
        let skipped = skipped_sessions
            .checked_add(skipped_events)
            .and_then(|value| value.checked_add(skipped_file_touches))
            .ok_or(CaptureError::SystemInvariant(
                "task JSON replay summary count overflowed",
            ))?;
        let failed = usize::try_from(checkpoint.rejected_records).map_err(|_| {
            CaptureError::SystemInvariant(
                "task JSON replay rejection count exceeds platform limits",
            )
        })?;
        Ok(ProviderImportSummary {
            skipped,
            failed,
            skipped_sessions,
            skipped_events,
            accepted_content_records: skipped_events.saturating_add(skipped_file_touches),
            ..ProviderImportSummary::default()
        })
    }

    pub(super) fn replay_summary_from_cursor(
        cursor: &CertifiedProviderCursor,
    ) -> Result<ProviderImportSummary> {
        let checkpoint = Self::checkpoint_for_cursor(cursor)?;
        Self::replay_summary_for_checkpoint(&checkpoint)
    }

    pub(super) fn replay_summary(&self) -> Result<ProviderImportSummary> {
        Self::replay_summary_for_checkpoint(&self.checkpoint)
    }
}

impl CapturedBatchProjector for TaskJsonCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if record.record_kind().as_str() != TASK_JSON_RECORD_KIND {
            return Err(ProviderProjectionFatal::system_invariant(
                "task JSON projector received an unexpected record kind",
            ));
        }
        let line_number = self
            .line_number(record)
            .map_err(ProviderProjectionFatal::new)?;
        self.report_state_failures(output, line_number)?;
        let (phase, class, native_index, _) =
            task_json_decode_locator(record.locator()).map_err(ProviderProjectionFatal::new)?;
        let CapturedRecordPayload::NativeBytes(bytes) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "task JSON projector requires native bytes",
            ));
        };
        match class {
            TaskJsonRecordClass::FileError => {
                let reason = String::from_utf8_lossy(bytes).into_owned();
                self.reject(output, line_number, reason)
            }
            TaskJsonRecordClass::Event => {
                let phase =
                    TaskJsonMessagePhase::decode(phase).map_err(ProviderProjectionFatal::new)?;
                let raw = match serde_json::from_slice::<Value>(bytes) {
                    Ok(value) => value,
                    Err(error) => {
                        return self.reject(
                            output,
                            line_number,
                            format!(
                                "{}: malformed task message JSON: {error}",
                                phase.file_name(self.spec).unwrap_or("task JSON")
                            ),
                        );
                    }
                };
                let native_index = usize::try_from(native_index)
                    .map_err(|_| {
                        CaptureError::SystemInvariant(
                            "task JSON native message index exceeds platform limits",
                        )
                    })
                    .map_err(ProviderProjectionFatal::new)?;
                self.project_event(
                    TaskJsonProjectEventInput {
                        raw,
                        source: phase.source(),
                        native_index,
                        source_record_ordinal: record.ordinal(),
                        record_bytes: bytes,
                        line_number,
                    },
                    output,
                )
            }
            TaskJsonRecordClass::Terminal => {
                if phase != TASK_JSON_TERMINAL_PHASE || self.checkpoint.terminal_seen {
                    return Err(ProviderProjectionFatal::system_invariant(
                        "task JSON projector received an invalid terminal record",
                    ));
                }
                self.checkpoint.terminal_seen = true;
                if self.checkpoint.accepted_events != 0 {
                    return Ok(());
                }
                if let Some(fallback) = self.session.fallback_event.clone() {
                    return self.project_event(
                        TaskJsonProjectEventInput {
                            raw: fallback,
                            source: "history_item",
                            native_index: 0,
                            source_record_ordinal: record.ordinal(),
                            record_bytes: bytes,
                            line_number,
                        },
                        output,
                    );
                }
                if self.checkpoint.rejected_records == 0 {
                    self.reject(
                        output,
                        line_number,
                        "provider source contained no real conversation message".to_owned(),
                    )?;
                }
                Ok(())
            }
        }
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        if task_json_decode_position(position)? != TaskJsonStreamPosition::initial() {
            return Err(CaptureError::SystemInvariant(
                "task JSON initial cursor candidate requires the initial native position",
            ));
        }
        CertifiedProviderCursor::new(
            source.source_revision(),
            source.capture_revision(),
            source.policy_revision(),
            position.clone(),
            BoundedParserCheckpoint::from_serializable(&Self::fresh_checkpoint(
                &self.session,
                &self.state_failures,
                self.context.imported_at,
            )?)?,
        )
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        let position = task_json_decode_position(batch.range_end())?;
        if position.ordinal != self.checkpoint.next_record_ordinal {
            return Err(CaptureError::SystemInvariant(
                "task JSON projector checkpoint did not reach the captured batch end",
            ));
        }
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                batch.range_end().clone(),
                BoundedParserCheckpoint::from_serializable(&self.checkpoint)?,
            )?,
        ))
    }
}

fn task_json_capture_from_state(
    spec: TaskJsonProviderSpec,
    raw_source_path: Option<&str>,
    context: &ProviderAdapterContext,
    state: &TaskJsonSessionState,
    event: Option<ProviderEventEnvelope>,
) -> ProviderCaptureEnvelope {
    let task_id = state.task_id.as_str();
    ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider: spec.provider,
        source: ProviderSourceEnvelope {
            source_format: spec.source_format.to_owned(),
            machine_id: context.machine_id.clone(),
            observed_at: context.imported_at,
            raw_source_path: raw_source_path.map(str::to_owned),
            source_root: context
                .source_root_display()
                .or_else(|| raw_source_path.map(str::to_owned)),
            trust: ProviderSourceTrust::ProviderNative,
            fidelity: Fidelity::Imported,
            cursor: event.as_ref().map(|event| ProviderCursorRange {
                before: None,
                after: Some(ProviderCursorCheckpoint {
                    stream: provider_cursor_stream(spec.provider, spec.source_format),
                    cursor: event.cursor.clone().unwrap_or_else(|| task_id.to_owned()),
                    observed_at: event.occurred_at,
                }),
            }),
            idempotency_key: Some(format!(
                "provider-source:{}:{}:{task_id}",
                spec.provider.as_str(),
                spec.source_format
            )),
            metadata: json!({
                "adapter": spec.source_format,
                "native_task_id": task_id,
                "files": state.files.clone(),
            }),
        },
        session: ProviderSessionEnvelope {
            provider_session_id: task_id.to_owned(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: if state.is_done {
                SessionStatus::Completed
            } else {
                SessionStatus::Imported
            },
            started_at: state.started_at,
            ended_at: state.ended_at,
            cwd: state.cwd.clone(),
            fidelity: Fidelity::Imported,
            idempotency_key: Some(format!(
                "provider-session:{}:{task_id}",
                spec.provider.as_str()
            )),
            artifacts: Vec::new(),
            metadata: json!({
                "source_format": spec.source_format,
                "provider": spec.provider.as_str(),
                "display_name": spec.display_name,
                "native_task_id": task_id,
                "task_metadata": state.task_metadata.clone(),
                "history_item": state.history_item.clone(),
                "index": state.index_item.clone(),
                "files": state.files.clone(),
                "limitations": [
                    "VS Code extension globalState databases are not parsed; ctx reads file-backed task directories",
                    "binary attachments and checkpoints are preserved only as native JSON metadata when present",
                    "message timestamps are inferred from task metadata when individual messages omit timestamps"
                ],
            }),
        },
        event,
    }
}
