use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderSourceTrust,
};
use serde_json::{json, Value};

use crate::captured_batch::jsonl::jsonl_position_offset;
use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, NativePosition, SourceObservation,
};
use crate::provider::importer::{
    BoundedParserCheckpoint, CapturedBatchCursorFinish, CapturedBatchProjector,
    CertifiedProviderCursor, ProviderProjectionFatal, ProviderProjectionOutput,
    ProviderProjectionResult,
};
use crate::provider::normalization::{
    native_event, native_provider_capture, provider_capped_json_value, provider_timestamp_millis,
    NativeEventDraft, NativeSessionDraft,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportFailure, ProviderImportSummary,
    ProviderNormalizationResult, Result, JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
    PROVIDER_MAX_PREVIEW_CHARS,
};

use super::{
    assistant::{
        junie_ensure_assistant, junie_merge_step, junie_merge_usage, JunieAssistantBuffer,
    },
    checkpoint::{
        bounded_junie_failure, junie_metadata_anchor, junie_parser_state_is_bounded,
        junie_read_anchored_metadata, JunieCheckpointFailure, JunieMetadataAnchor,
        JunieParserCheckpoint,
    },
    junie_jsonl_batch_error,
    normalize::{
        junie_file_change_has_path, junie_file_change_normalization, junie_step_normalization,
        junie_step_output_normalization,
    },
    session_tree::{
        bounded_junie_index_meta, junie_provider_session_id, junie_timestamp_millis_field,
        JunieIndexMeta, JunieSessionPath,
    },
    JUNIE_END_RECORD_KIND, JUNIE_RECORD_KIND, MAX_JUNIE_CHECKPOINT_FAILURES,
    MAX_JUNIE_FAILURE_BYTES, MAX_JUNIE_TRANSIENT_TURN_BYTES,
};

pub(super) struct JunieCapturedBatchProjector {
    context: ProviderAdapterContext,
    index_meta: JunieIndexMeta,
    provider_session_id: String,
    started_at: DateTime<Utc>,
    raw_source_path: String,
    base_line: usize,
    require_supported_events: bool,
    pub(super) state: JunieParserCheckpoint,
    pub(super) buffer: JunieAssistantBuffer,
    cwd: Option<String>,
    title: Option<String>,
    metadata_is_resumable: bool,
}

impl JunieCapturedBatchProjector {
    pub(super) fn fresh(
        session_path: &JunieSessionPath,
        context: ProviderAdapterContext,
        session_ordinal: usize,
        auxiliary_revision: u64,
    ) -> Result<Self> {
        let provider_session_id = junie_provider_session_id(session_path)?;
        let index_meta = bounded_junie_index_meta(&session_path.index_meta);
        let started_at = provider_timestamp_millis(index_meta.created_at, context.imported_at);
        let ended_at = index_meta
            .updated_at
            .map(|timestamp| provider_timestamp_millis(Some(timestamp), started_at));
        let projector = Self {
            context,
            index_meta: index_meta.clone(),
            provider_session_id,
            started_at,
            raw_source_path: session_path.events_path.display().to_string(),
            base_line: session_ordinal.saturating_mul(100_000),
            require_supported_events: session_path.require_supported_events,
            state: JunieParserCheckpoint {
                next_ordinal: 0,
                next_line_number: 0,
                provider_event_index: 0,
                started_at,
                last_ts: started_at,
                ended_at,
                title_anchor: None,
                cwd_anchor: None,
                saw_supported_event: false,
                metadata_dirty: false,
                source_ended: false,
                auxiliary_revision,
                accepted_captures: 0,
                accepted_events: 0,
                accepted_file_touches: 0,
                structural_rejections: 0,
                rejected_records: 0,
                failures: Vec::new(),
            },
            buffer: JunieAssistantBuffer::default(),
            cwd: index_meta.project_dir,
            title: index_meta.task_name,
            metadata_is_resumable: true,
        };
        if !junie_parser_state_is_bounded(&projector.state) {
            return Err(CaptureError::InvalidPayload(
                "Junie initial parser state exceeds its provider-local bound".to_owned(),
            ));
        }
        Ok(projector)
    }

    pub(super) fn resume(
        session_path: &JunieSessionPath,
        context: ProviderAdapterContext,
        session_ordinal: usize,
        cursor: &CertifiedProviderCursor,
        reset_on_metadata_mismatch: bool,
    ) -> Result<Option<Self>> {
        let state: JunieParserCheckpoint = cursor.parser_checkpoint().deserialize()?;
        let cursor_offset =
            jsonl_position_offset(cursor.native_position()).map_err(junie_jsonl_batch_error)?;
        let anchors_are_valid = [&state.title_anchor, &state.cwd_anchor]
            .into_iter()
            .flatten()
            .all(|anchor| {
                anchor.start < anchor.end
                    && anchor.end <= cursor_offset
                    && anchor.end - anchor.start
                        <= crate::MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2) as u64
            });
        if state.failures.len() > MAX_JUNIE_CHECKPOINT_FAILURES
            || state
                .failures
                .iter()
                .any(|failure| failure.error.len() > MAX_JUNIE_FAILURE_BYTES)
            || u64::try_from(state.failures.len()).unwrap_or(u64::MAX) > state.rejected_records
            || !anchors_are_valid
            || !junie_parser_state_is_bounded(&state)
        {
            return Err(CaptureError::InvalidPayload(
                "Junie parser checkpoint has invalid bounded state".to_owned(),
            ));
        }
        let provider_session_id = junie_provider_session_id(session_path)?;
        let started_at = state.started_at;
        let index_meta = bounded_junie_index_meta(&session_path.index_meta);
        let rehydrate = || -> Result<(Option<String>, Option<String>)> {
            let title = match state.title_anchor.as_ref() {
                Some(anchor) => junie_read_anchored_metadata(
                    &session_path.events_path,
                    anchor,
                    "AgentTaskNameUpdatedEvent",
                    "name",
                )?,
                None => index_meta.task_name.clone(),
            };
            let cwd = match state.cwd_anchor.as_ref() {
                Some(anchor) => junie_read_anchored_metadata(
                    &session_path.events_path,
                    anchor,
                    "CurrentDirectoryUpdatedEvent",
                    "currentDirectory",
                )?,
                None => index_meta.project_dir.clone(),
            };
            Ok((title, cwd))
        };
        let (title, cwd) = match rehydrate() {
            Ok(metadata) => metadata,
            Err(CaptureError::SourceChangedDuringCapture) if reset_on_metadata_mismatch => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        Ok(Some(Self {
            context,
            cwd,
            title,
            index_meta,
            provider_session_id,
            started_at,
            raw_source_path: session_path.events_path.display().to_string(),
            base_line: session_ordinal.saturating_mul(100_000),
            require_supported_events: session_path.require_supported_events,
            state,
            buffer: JunieAssistantBuffer::default(),
            metadata_is_resumable: true,
        }))
    }

    fn base_draft(&self) -> NativeSessionDraft {
        NativeSessionDraft {
            provider: CaptureProvider::Junie,
            source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            provider_session_id: self.provider_session_id.clone(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            started_at: self.started_at,
            ended_at: self.state.ended_at,
            cwd: self.cwd.clone(),
            fidelity: Fidelity::Imported,
            raw_source_path: self.raw_source_path.clone(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                "source_path": self.raw_source_path,
                "storage": "~/.junie/sessions/index.jsonl + session-*/events.jsonl",
                "upstream_schema_anchor": {
                    "source": "vladar107/claudescope",
                    "connector": "packages/server/src/connectors/junie",
                    "notes": "event-sourced UI render stream with UserPromptEvent and SessionA2uxEvent agentEvent blocks"
                },
            }),
            session_metadata: json!({
                "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                "session_id": self.provider_session_id,
                "title": self.title,
                "project_dir": self.cwd,
                "index": provider_capped_json_value(&self.index_meta.raw, PROVIDER_MAX_PREVIEW_CHARS),
                "limitations": [
                    "ctx imports Junie events.jsonl UI stream blocks, not a provider conversational message log",
                    "custom attachment image files are not read by the native importer",
                    "unknown SessionA2uxEvent agentEvent kinds are skipped"
                ],
            }),
        }
    }

    fn advance_raw_record(&mut self, ordinal: u64) -> Result<(usize, usize)> {
        self.advance_to_ordinal(ordinal, true)?;
        let line_number =
            self.state
                .next_line_number
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Junie source line number overflowed",
                ))?;
        self.state.next_line_number = line_number;
        let line_number = usize::try_from(line_number).map_err(|_| {
            CaptureError::SystemInvariant("Junie source line number exceeds platform limits")
        })?;
        Ok((line_number, self.base_line.saturating_add(line_number)))
    }

    fn advance_to_ordinal(&mut self, ordinal: u64, current_is_raw: bool) -> Result<()> {
        if ordinal < self.state.next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "Junie captured record ordinal moved backwards",
            ));
        }
        let structural_rejections = ordinal - self.state.next_ordinal;
        self.state.structural_rejections = self
            .state
            .structural_rejections
            .checked_add(structural_rejections)
            .ok_or(CaptureError::SystemInvariant(
                "Junie structural-rejection count overflowed",
            ))?;
        self.state.next_line_number = self
            .state
            .next_line_number
            .checked_add(structural_rejections)
            .ok_or(CaptureError::SystemInvariant(
                "Junie source line number overflowed",
            ))?;
        if structural_rejections != 0 || current_is_raw {
            self.state.source_ended = false;
        }
        self.state.next_ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "Junie captured record ordinal overflowed",
        ))?;
        Ok(())
    }

    fn accept(
        &mut self,
        output: &mut dyn ProviderProjectionOutput,
        normalization: ProviderNormalizationResult,
    ) -> ProviderProjectionResult<()> {
        if normalization.captures.len() > 1 || normalization.files_touched.len() > 1 {
            return Err(ProviderProjectionFatal::system_invariant(
                "Junie projection must stream at most one capture and one file touch",
            ));
        }
        let captures = u64::try_from(normalization.captures.len())
            .map_err(|_| CaptureError::SystemInvariant("Junie capture count exceeds u64"))
            .map_err(ProviderProjectionFatal::new)?;
        let events = u64::try_from(
            normalization
                .captures
                .iter()
                .filter(|(_, capture)| capture.event.is_some())
                .count(),
        )
        .map_err(|_| CaptureError::SystemInvariant("Junie event count exceeds u64"))
        .map_err(ProviderProjectionFatal::new)?;
        let file_touches = u64::try_from(normalization.files_touched.len())
            .map_err(|_| CaptureError::SystemInvariant("Junie file-touch count exceeds u64"))
            .map_err(ProviderProjectionFatal::new)?;
        output.emit_normalization(normalization)?;
        self.state.accepted_captures = self
            .state
            .accepted_captures
            .checked_add(captures)
            .ok_or(CaptureError::SystemInvariant(
                "Junie projected capture count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        self.state.accepted_events = self
            .state
            .accepted_events
            .checked_add(events)
            .ok_or(CaptureError::SystemInvariant(
                "Junie projected event count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        self.state.accepted_file_touches = self
            .state
            .accepted_file_touches
            .checked_add(file_touches)
            .ok_or(CaptureError::SystemInvariant(
                "Junie projected file-touch count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        Ok(())
    }

    fn reject_record(
        &mut self,
        output: &mut dyn ProviderProjectionOutput,
        line: usize,
        error: String,
    ) -> ProviderProjectionResult<()> {
        let error = bounded_junie_failure(error);
        self.state.rejected_records = self
            .state
            .rejected_records
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Junie rejection count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        let previous_failure_count = self.state.failures.len();
        if self.state.failures.len() < MAX_JUNIE_CHECKPOINT_FAILURES {
            self.state.failures.push(JunieCheckpointFailure {
                line,
                error: error.clone(),
            });
        }
        if !junie_parser_state_is_bounded(&self.state) {
            self.state.failures.truncate(previous_failure_count);
        }
        output.reject_record(line, error);
        Ok(())
    }

    fn flush_assistant(
        &mut self,
        output: &mut dyn ProviderProjectionOutput,
        line_number: usize,
    ) -> ProviderProjectionResult<bool> {
        if !self.buffer.open {
            return Ok(false);
        }
        let mut buffer = std::mem::take(&mut self.buffer);
        let occurred_at = buffer.turn_ts.unwrap_or(self.started_at);
        let base_draft = self.base_draft();
        let context = self.context.clone();
        let mut emitted = false;

        for next_key in std::mem::take(&mut buffer.step_ids_in_order) {
            let Some(step) = buffer.steps.remove(&next_key) else {
                return Err(ProviderProjectionFatal::system_invariant(
                    "Junie buffered step disappeared during streaming flush",
                ));
            };
            if step.changes.is_empty() {
                let event_index = self.next_provider_event_index()?;
                self.accept(
                    output,
                    junie_step_normalization(
                        &base_draft,
                        &context,
                        line_number,
                        event_index,
                        occurred_at,
                        &step,
                    ),
                )?;
                emitted = true;
                if let Some(details) = step
                    .details
                    .as_deref()
                    .filter(|details| !details.trim().is_empty())
                {
                    let event_index = self.next_provider_event_index()?;
                    self.accept(
                        output,
                        junie_step_output_normalization(
                            &base_draft,
                            &context,
                            line_number,
                            event_index,
                            occurred_at,
                            &step,
                            details,
                        ),
                    )?;
                }
                continue;
            }

            for (change_index, change) in step.changes.iter().enumerate() {
                if !junie_file_change_has_path(change) {
                    continue;
                }
                let event_index = self.next_provider_event_index()?;
                self.accept(
                    output,
                    junie_file_change_normalization(
                        &base_draft,
                        &context,
                        line_number,
                        event_index,
                        occurred_at,
                        step.order,
                        change_index,
                        change,
                        step.status.as_deref(),
                    ),
                )?;
                emitted = true;
            }
        }
        if !buffer.steps.is_empty() {
            return Err(ProviderProjectionFatal::system_invariant(
                "Junie buffered step ordering did not cover every step",
            ));
        }

        let mut final_text = String::new();
        for result in buffer.results.values() {
            if result.trim().is_empty() {
                continue;
            }
            if !final_text.is_empty() {
                final_text.push_str("\n\n");
            }
            final_text.push_str(result);
        }
        if !final_text.is_empty() {
            let event_index = self.next_provider_event_index()?;
            let event = native_event(NativeEventDraft {
                provider: CaptureProvider::Junie,
                source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                provider_session_id: self.provider_session_id.clone(),
                provider_event_index: event_index,
                provider_event_hash: Some(format!("assistant-result:{event_index}")),
                cursor: format!(
                    "{}:line:{line_number}:event:{event_index}",
                    self.raw_source_path
                ),
                event_type: EventType::Message,
                role: Some(EventRole::Assistant),
                occurred_at,
                text: final_text,
                body: json!({
                    "result_blocks": buffer.results,
                    "model": buffer.usage.model,
                    "usage": {
                        "input_tokens": buffer.usage.input_tokens,
                        "output_tokens": buffer.usage.output_tokens,
                        "cache_read_tokens": buffer.usage.cache_read_tokens,
                        "cache_write_tokens": buffer.usage.cache_write_tokens,
                    },
                }),
                metadata: json!({
                    "source": "junie_result_blocks",
                    "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                    "model": buffer.usage.model,
                    "usage": {
                        "input_tokens": buffer.usage.input_tokens,
                        "output_tokens": buffer.usage.output_tokens,
                        "cache_read_tokens": buffer.usage.cache_read_tokens,
                        "cache_write_tokens": buffer.usage.cache_write_tokens,
                    },
                }),
            });
            self.accept(
                output,
                ProviderNormalizationResult {
                    captures: vec![(
                        line_number,
                        native_provider_capture(self.base_draft(), &self.context, Some(event)),
                    )],
                    ..ProviderNormalizationResult::default()
                },
            )?;
            emitted = true;
        }
        Ok(emitted)
    }

    fn project_user_prompt(
        &mut self,
        value: &Value,
        line_number: usize,
        import_line: usize,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        self.flush_assistant(output, import_line)?;
        let prompt = value.get("prompt").and_then(Value::as_str).unwrap_or("");
        if prompt.trim().is_empty() {
            return Ok(());
        }
        let event_index = self.next_provider_event_index()?;
        let event = native_event(NativeEventDraft {
            provider: CaptureProvider::Junie,
            source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            provider_session_id: self.provider_session_id.clone(),
            provider_event_index: event_index,
            provider_event_hash: Some(format!("line:{line_number}:user")),
            cursor: format!(
                "{}:line:{line_number}:event:{event_index}",
                self.raw_source_path
            ),
            event_type: EventType::Message,
            role: Some(EventRole::User),
            occurred_at: self.state.last_ts,
            text: prompt.to_owned(),
            body: json!({
                "kind": "UserPromptEvent",
                "prompt": prompt,
            }),
            metadata: json!({
                "source": "junie_user_prompt",
                "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            }),
        });
        self.state.saw_supported_event = true;
        self.accept(
            output,
            ProviderNormalizationResult {
                captures: vec![(
                    import_line,
                    native_provider_capture(self.base_draft(), &self.context, Some(event)),
                )],
                ..ProviderNormalizationResult::default()
            },
        )
    }

    fn next_provider_event_index(&mut self) -> ProviderProjectionResult<u64> {
        let event_index = self.state.provider_event_index;
        self.state.provider_event_index = event_index
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Junie provider event index overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        Ok(event_index)
    }

    pub(super) fn project_session_event(
        &mut self,
        value: &Value,
        retained_source_bytes: usize,
        metadata_anchor: Option<&JunieMetadataAnchor>,
    ) -> bool {
        if let Some(timestamp) = junie_timestamp_millis_field(value, "timestampMs")
            .and_then(DateTime::<Utc>::from_timestamp_millis)
        {
            self.state.last_ts = timestamp;
            if self.state.ended_at != Some(timestamp) {
                self.state.ended_at = Some(timestamp);
                self.state.metadata_dirty = true;
            }
        }
        let agent_event = value
            .get("event")
            .and_then(|event| event.get("agentEvent"))
            .unwrap_or(&Value::Null);
        let agent_kind = agent_event
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("");
        let buffers_turn = matches!(
            agent_kind,
            "LlmResponseMetadataEvent"
                | "ResultBlockUpdatedEvent"
                | "AgentFailureEvent"
                | "ToolBlockUpdatedEvent"
                | "TerminalBlockUpdatedEvent"
                | "ViewFilesBlockUpdatedEvent"
                | "FileChangesBlockUpdatedEvent"
        );
        if buffers_turn {
            let retained = self
                .buffer
                .retained_source_bytes
                .checked_add(retained_source_bytes);
            if retained.is_none_or(|retained| retained > MAX_JUNIE_TRANSIENT_TURN_BYTES) {
                self.buffer = JunieAssistantBuffer::default();
                return false;
            }
            self.buffer.retained_source_bytes = retained.unwrap_or_default();
        }
        match agent_kind {
            "LlmResponseMetadataEvent" => {
                junie_ensure_assistant(&mut self.buffer, self.state.last_ts);
                junie_merge_usage(&mut self.buffer.usage, agent_event);
                self.state.saw_supported_event = true;
            }
            "AgentTaskNameUpdatedEvent" => {
                if let Some(name) = agent_event
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.trim().is_empty())
                {
                    let name = Some(name.to_owned());
                    if self.title != name {
                        self.title = name;
                        self.state.title_anchor = metadata_anchor.cloned();
                        self.metadata_is_resumable &= metadata_anchor.is_some();
                        self.state.metadata_dirty = true;
                    }
                }
            }
            "CurrentDirectoryUpdatedEvent" if self.cwd.is_none() => {
                let cwd = agent_event
                    .get("currentDirectory")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .map(str::to_owned);
                if cwd.is_some() {
                    self.cwd = cwd;
                    self.state.cwd_anchor = metadata_anchor.cloned();
                    self.metadata_is_resumable &= metadata_anchor.is_some();
                    self.state.metadata_dirty = true;
                }
            }
            "ResultBlockUpdatedEvent" => {
                if let Some(text) = agent_event
                    .get("result")
                    .and_then(Value::as_str)
                    .filter(|text| !text.trim().is_empty())
                {
                    let step_id = agent_event
                        .get("stepId")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("result-{}", self.state.next_line_number));
                    self.project_assistant_result(step_id, text.to_owned());
                }
            }
            "AgentFailureEvent" => {
                if let Some(message) = agent_event
                    .get("message")
                    .and_then(Value::as_str)
                    .filter(|message| !message.trim().is_empty())
                {
                    let step_id = agent_event
                        .get("errorCode")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                        .map(|value| format!("failure-{value}-{}", self.state.next_line_number))
                        .unwrap_or_else(|| format!("failure-{}", self.state.next_line_number));
                    self.project_assistant_result(step_id, format!("Junie failed: {message}"));
                }
            }
            "ToolBlockUpdatedEvent"
            | "TerminalBlockUpdatedEvent"
            | "ViewFilesBlockUpdatedEvent"
            | "FileChangesBlockUpdatedEvent" => {
                self.project_step_event(agent_event);
                self.state.saw_supported_event = true;
            }
            _ => {}
        }
        true
    }

    fn project_assistant_result(&mut self, step_id: String, text: String) {
        junie_ensure_assistant(&mut self.buffer, self.state.last_ts);
        self.buffer.results.insert(step_id, text);
        self.state.saw_supported_event = true;
    }

    fn project_step_event(&mut self, agent_event: &Value) {
        junie_merge_step(&mut self.buffer, agent_event, self.state.last_ts);
    }

    fn finish_source(
        &mut self,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let next_line = self
            .state
            .next_line_number
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Junie final line number overflowed",
            ))
            .and_then(|line| {
                usize::try_from(line).map_err(|_| {
                    CaptureError::SystemInvariant("Junie final line number exceeds platform limits")
                })
            })
            .map_err(ProviderProjectionFatal::new)?;
        let import_line = self.base_line.saturating_add(next_line);
        let accepted_before = self.state.accepted_captures;
        let flushed = self.flush_assistant(output, import_line)?;
        if !flushed
            && accepted_before != 0
            && self.state.metadata_dirty
            && self.state.ended_at.is_some()
        {
            self.accept(
                output,
                ProviderNormalizationResult {
                    captures: vec![(
                        import_line,
                        native_provider_capture(self.base_draft(), &self.context, None),
                    )],
                    ..ProviderNormalizationResult::default()
                },
            )?;
        }
        if self.state.accepted_captures == 0
            && !self.state.saw_supported_event
            && self.require_supported_events
        {
            self.reject_record(
                output,
                self.base_line,
                "Junie events.jsonl contained no supported UserPromptEvent or SessionA2uxEvent blocks"
                    .to_owned(),
            )?;
        }
        self.state.metadata_dirty = false;
        self.state.source_ended = true;
        Ok(())
    }

    pub(super) fn replay_summary(&self) -> Result<ProviderImportSummary> {
        let skipped_sessions = usize::from(self.state.accepted_captures != 0);
        let accepted_events = usize::try_from(self.state.accepted_events).map_err(|_| {
            CaptureError::SystemInvariant("Junie replay event count exceeds platform limits")
        })?;
        let skipped_file_touches =
            usize::try_from(self.state.accepted_file_touches).map_err(|_| {
                CaptureError::SystemInvariant(
                    "Junie replay file-touch count exceeds platform limits",
                )
            })?;
        let skipped = skipped_sessions
            .checked_add(accepted_events)
            .and_then(|value| value.checked_add(skipped_file_touches))
            .ok_or(CaptureError::SystemInvariant(
                "Junie replay summary count overflowed",
            ))?;
        let failed = self
            .state
            .rejected_records
            .checked_add(self.state.structural_rejections)
            .ok_or(CaptureError::SystemInvariant(
                "Junie replay rejection count overflowed",
            ))
            .and_then(|failed| {
                usize::try_from(failed).map_err(|_| {
                    CaptureError::SystemInvariant(
                        "Junie replay rejection count exceeds platform limits",
                    )
                })
            })?;
        let failures = self
            .state
            .failures
            .iter()
            .cloned()
            .map(|failure| ProviderImportFailure {
                line: failure.line,
                error: failure.error,
            })
            .collect();
        Ok(ProviderImportSummary {
            skipped,
            failed,
            skipped_sessions,
            skipped_events: accepted_events,
            accepted_content_records: accepted_events.saturating_add(skipped_file_touches),
            failures,
            ..ProviderImportSummary::default()
        })
    }
}

impl CapturedBatchProjector for JunieCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if record.record_kind().as_str() == JUNIE_END_RECORD_KIND {
            self.advance_to_ordinal(record.ordinal(), false)
                .map_err(ProviderProjectionFatal::new)?;
            if !matches!(record.payload(), CapturedRecordPayload::NativeBytes(bytes) if bytes.is_empty())
            {
                return Err(ProviderProjectionFatal::system_invariant(
                    "Junie end record carried an unexpected payload",
                ));
            }
            return self.finish_source(output);
        }
        if record.record_kind().as_str() != JUNIE_RECORD_KIND {
            return Err(ProviderProjectionFatal::system_invariant(
                "Junie projector received an unexpected record kind",
            ));
        }
        let (line_number, import_line) = self
            .advance_raw_record(record.ordinal())
            .map_err(ProviderProjectionFatal::new)?;
        let bytes = match record.payload() {
            CapturedRecordPayload::NativeBytes(bytes) => bytes,
            CapturedRecordPayload::StructuralRejection { .. } => {
                return Err(ProviderProjectionFatal::system_invariant(
                    "Junie structural rejections must be handled by the batch importer",
                ));
            }
            CapturedRecordPayload::SqliteValues(_) => {
                return Err(ProviderProjectionFatal::system_invariant(
                    "Junie projector requires native JSONL bytes",
                ));
            }
        };
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let value = match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => value,
            Err(error) => {
                return self.reject_record(
                    output,
                    import_line,
                    format!("malformed Junie events JSONL: {error}"),
                );
            }
        };
        match value.get("kind").and_then(Value::as_str).unwrap_or("") {
            "UserPromptEvent" => {
                self.project_user_prompt(&value, line_number, import_line, output)?;
            }
            "SessionA2uxEvent" => {
                let metadata_anchor = junie_metadata_anchor(record.locator(), bytes);
                if !self.project_session_event(&value, bytes.len(), metadata_anchor.as_ref()) {
                    return self.reject_record(
                        output,
                        import_line,
                        format!(
                            "Junie assistant turn exceeds the {MAX_JUNIE_TRANSIENT_TURN_BYTES} byte transient buffer limit"
                        ),
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        if self.buffer.open {
            return Err(CaptureError::SystemInvariant(
                "Junie initial cursor candidate has an open transient assistant turn",
            ));
        }
        CertifiedProviderCursor::new(
            source.source_revision(),
            source.capture_revision(),
            source.policy_revision(),
            position.clone(),
            BoundedParserCheckpoint::from_serializable(&self.state)?,
        )
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        let next_ordinal = batch
            .records()
            .last()
            .and_then(|record| record.ordinal().checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "Junie captured batch did not have a next ordinal",
            ))?;
        if self.state.next_ordinal > next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "Junie projector advanced beyond the captured batch",
            ));
        }
        if self.buffer.open || !self.metadata_is_resumable {
            return Ok(CapturedBatchCursorFinish::RetainPrior);
        }
        let mut checkpoint = self.state.clone();
        let trailing_structural_rejections = next_ordinal - checkpoint.next_ordinal;
        checkpoint.structural_rejections = checkpoint
            .structural_rejections
            .checked_add(trailing_structural_rejections)
            .ok_or(CaptureError::SystemInvariant(
                "Junie structural-rejection count overflowed",
            ))?;
        checkpoint.next_line_number = checkpoint
            .next_line_number
            .checked_add(trailing_structural_rejections)
            .ok_or(CaptureError::SystemInvariant(
                "Junie source line number overflowed",
            ))?;
        checkpoint.next_ordinal = next_ordinal;
        if trailing_structural_rejections != 0 {
            checkpoint.source_ended = false;
        }
        if !junie_parser_state_is_bounded(&checkpoint) {
            return Err(CaptureError::InvalidPayload(
                "Junie parser state exceeds its provider-local bound".to_owned(),
            ));
        }
        CertifiedProviderCursor::new(
            batch.source().source_revision(),
            batch.source().capture_revision(),
            batch.source().policy_revision(),
            batch.range_end().clone(),
            BoundedParserCheckpoint::from_serializable(&checkpoint)?,
        )
        .map(CapturedBatchCursorFinish::Advance)
    }
}
