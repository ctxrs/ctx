use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    provider::{
        normalization::{provider_local_preview, provider_timestamp_millis},
        source_backed::family::jsonl::JsonlRecordRef,
    },
    CaptureError, Result, PROVIDER_MAX_PREVIEW_CHARS,
};

use super::super::{
    assistant::{
        junie_buffer_result_text, junie_merge_buffered_agent_event, junie_step_output_projection,
        JunieAssistantBuffer, JunieOutputOutcome, JunieStepAgg,
    },
    session_tree::JunieIndexMeta,
    MAX_JUNIE_TRANSIENT_TURN_BYTES,
};

pub(super) const MAX_RECORD_SET_ENTRIES: usize = 64;
pub(super) const RECORD_SET_DIGEST_DOMAIN: &[u8] = b"ctx-junie-jsonl-record-set-v1\0";

#[derive(Debug, Clone)]
pub(super) struct BindingEntry {
    pub(super) ordinal: u64,
    pub(super) byte_start: u64,
    pub(super) byte_end_exclusive: u64,
    pub(super) payload_sha256: [u8; 32],
}

#[derive(Debug, Clone, Default)]
pub(super) struct RecordSetBinding {
    pub(super) entries: Vec<BindingEntry>,
    pub(super) unavailable: bool,
}

impl RecordSetBinding {
    fn observe(&mut self, ordinal: u64, byte_start: u64, byte_end_exclusive: u64, payload: &[u8]) {
        if self.unavailable {
            return;
        }
        if byte_start >= byte_end_exclusive
            || self.entries.len() >= MAX_RECORD_SET_ENTRIES
            || self.entries.last().is_some_and(|prior| {
                prior.ordinal >= ordinal || prior.byte_end_exclusive > byte_start
            })
        {
            self.entries.clear();
            self.unavailable = true;
            return;
        }
        self.entries.push(BindingEntry {
            ordinal,
            byte_start,
            byte_end_exclusive,
            payload_sha256: Sha256::digest(payload).into(),
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SourceBackedTarget {
    UserPrompt,
    AssistantMessage,
    StepCall { step_order: u32 },
    StepOutput { step_order: u32 },
    FileChange { step_order: u32, change_index: u32 },
}

#[derive(Debug, Clone)]
pub(super) struct SourceBackedBinding {
    pub(super) records: RecordSetBinding,
    pub(super) target: SourceBackedTarget,
}

#[derive(Debug, Clone)]
pub(super) struct FileChangeDraft {
    pub(super) path: String,
}

#[derive(Debug, Clone)]
pub(super) struct EventDraft {
    pub(super) event_index: u64,
    pub(super) event_type: EventType,
    pub(super) role: Option<EventRole>,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) text: String,
    pub(super) body: Value,
    pub(super) source_backed_binding: SourceBackedBinding,
    pub(super) file_change: Option<FileChangeDraft>,
}

#[derive(Debug, Clone)]
struct RuntimeState {
    started_at_ms: i64,
    last_ts_ms: i64,
    cwd: Option<String>,
    saw_supported_event: bool,
}

impl RuntimeState {
    fn fresh(meta: &JunieIndexMeta, imported_at: DateTime<Utc>) -> Self {
        let started_at = provider_timestamp_millis(meta.created_at, imported_at);
        Self {
            started_at_ms: started_at.timestamp_millis(),
            last_ts_ms: started_at.timestamp_millis(),
            cwd: meta.project_dir.clone(),
            saw_supported_event: false,
        }
    }

    fn started_at(&self) -> DateTime<Utc> {
        timestamp(self.started_at_ms)
    }

    fn last_ts(&self) -> DateTime<Utc> {
        timestamp(self.last_ts_ms)
    }
}

pub(super) struct JunieProjection {
    state: RuntimeState,
    buffer: JunieAssistantBuffer,
    binding: RecordSetBinding,
    retained_turn_bytes: usize,
    turn_start: u64,
    next_event_index: u64,
    rejected_records: u64,
    require_supported_events: bool,
}

impl JunieProjection {
    pub(super) fn new(
        meta: &JunieIndexMeta,
        require_supported_events: bool,
        imported_at: DateTime<Utc>,
    ) -> Self {
        Self {
            state: RuntimeState::fresh(meta, imported_at),
            buffer: JunieAssistantBuffer::default(),
            binding: RecordSetBinding::default(),
            retained_turn_bytes: 0,
            turn_start: 0,
            next_event_index: 0,
            rejected_records: 0,
            require_supported_events,
        }
    }

    pub(super) fn cwd(&self) -> Option<&str> {
        self.state.cwd.as_deref()
    }

    pub(super) fn project(&mut self, record: JsonlRecordRef<'_>) -> Result<Vec<EventDraft>> {
        let evidence = record.evidence();
        if evidence
            .byte_end_exclusive()
            .saturating_sub(self.turn_start)
            > MAX_JUNIE_TRANSIENT_TURN_BYTES as u64
        {
            self.reset_turn(evidence.byte_end_exclusive());
            self.reject();
            return Ok(Vec::new());
        }
        let payload = record.bytes();
        if payload.iter().all(u8::is_ascii_whitespace) {
            return Ok(Vec::new());
        }
        let value = match serde_json::from_slice::<Value>(payload) {
            Ok(value) => value,
            Err(_) => {
                self.reject();
                return Ok(Vec::new());
            }
        };
        let mut rows = Vec::new();
        match value.get("kind").and_then(Value::as_str).unwrap_or("") {
            "UserPromptEvent" => {
                flush_assistant(
                    &mut self.buffer,
                    &self.binding,
                    &self.state,
                    &mut self.next_event_index,
                    &mut rows,
                )?;
                let prompt = value.get("prompt").and_then(Value::as_str).unwrap_or("");
                if !prompt.trim().is_empty() {
                    self.state.saw_supported_event = true;
                    let mut binding = RecordSetBinding::default();
                    binding.observe(
                        evidence.physical_ordinal(),
                        evidence.byte_start(),
                        evidence.byte_end_exclusive(),
                        payload,
                    );
                    rows.push(EventDraft {
                        event_index: self.next_event_index,
                        event_type: EventType::Message,
                        role: Some(EventRole::User),
                        occurred_at: self.state.last_ts(),
                        text: prompt.to_owned(),
                        body: json!({
                            "kind": "UserPromptEvent",
                            "prompt": prompt,
                        }),
                        source_backed_binding: SourceBackedBinding {
                            records: binding,
                            target: SourceBackedTarget::UserPrompt,
                        },
                        file_change: None,
                    });
                    self.next_event_index = self.next_event_index.checked_add(1).ok_or(
                        CaptureError::SystemInvariant("Junie provider event index exhausted"),
                    )?;
                }
                self.reset_turn(evidence.byte_end_exclusive());
            }
            "SessionA2uxEvent" => {
                if let Some(timestamp) = value
                    .get("timestampMs")
                    .and_then(json_i64)
                    .and_then(DateTime::<Utc>::from_timestamp_millis)
                {
                    self.state.last_ts_ms = timestamp.timestamp_millis();
                }
                let agent = value
                    .get("event")
                    .and_then(|event| event.get("agentEvent"))
                    .unwrap_or(&Value::Null);
                match agent.get("kind").and_then(Value::as_str).unwrap_or("") {
                    "CurrentDirectoryUpdatedEvent" => {
                        if let Some(cwd) = agent
                            .get("currentDirectory")
                            .and_then(Value::as_str)
                            .filter(|value| !value.trim().is_empty())
                        {
                            self.state.cwd =
                                Some(provider_local_preview(cwd, PROVIDER_MAX_PREVIEW_CHARS).0);
                        }
                    }
                    "LlmResponseMetadataEvent"
                    | "ResultBlockUpdatedEvent"
                    | "AgentFailureEvent"
                    | "ToolBlockUpdatedEvent"
                    | "TerminalBlockUpdatedEvent"
                    | "ViewFilesBlockUpdatedEvent"
                    | "FileChangesBlockUpdatedEvent" => {
                        let retained = self.retained_turn_bytes.checked_add(payload.len());
                        if retained.is_none_or(|bytes| bytes > MAX_JUNIE_TRANSIENT_TURN_BYTES) {
                            self.reset_turn(evidence.byte_end_exclusive());
                            self.reject();
                            return Ok(Vec::new());
                        }
                        self.retained_turn_bytes = retained.unwrap_or_default();
                        self.binding.observe(
                            evidence.physical_ordinal(),
                            evidence.byte_start(),
                            evidence.byte_end_exclusive(),
                            payload,
                        );
                        if junie_merge_buffered_agent_event(
                            &mut self.buffer,
                            agent,
                            evidence.physical_ordinal().saturating_add(1),
                            self.state.last_ts(),
                        ) {
                            self.state.saw_supported_event = true;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Ok(rows)
    }

    pub(super) fn finish(&mut self) -> Result<Vec<EventDraft>> {
        let mut rows = Vec::new();
        flush_assistant(
            &mut self.buffer,
            &self.binding,
            &self.state,
            &mut self.next_event_index,
            &mut rows,
        )?;
        if self.require_supported_events
            && !self.state.saw_supported_event
            && self.rejected_records == 0
        {
            return Err(CaptureError::InvalidPayload(
                "Junie events.jsonl contained no supported session events".to_owned(),
            ));
        }
        Ok(rows)
    }

    fn reset_turn(&mut self, next_start: u64) {
        self.buffer = JunieAssistantBuffer::default();
        self.binding = RecordSetBinding::default();
        self.retained_turn_bytes = 0;
        self.turn_start = next_start;
    }

    fn reject(&mut self) {
        self.rejected_records = self.rejected_records.saturating_add(1);
    }
}

fn timestamp(millis: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp_millis(millis).unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

fn json_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_f64().map(|value| value.round() as i64))
}

fn flush_assistant(
    buffer: &mut JunieAssistantBuffer,
    binding: &RecordSetBinding,
    state: &RuntimeState,
    event_index: &mut u64,
    rows: &mut Vec<EventDraft>,
) -> Result<()> {
    if !buffer.open {
        return Ok(());
    }
    let buffer = std::mem::take(buffer);
    let occurred_at = buffer.turn_ts.unwrap_or_else(|| state.started_at());
    for step_id in &buffer.step_ids_in_order {
        let step = buffer
            .steps
            .get(step_id)
            .ok_or(CaptureError::SystemInvariant(
                "Junie buffered step ordering lost a step",
            ))?;
        if step.changes.is_empty() {
            rows.push(step_event(*event_index, occurred_at, step, binding));
            increment_event_index(event_index)?;
            if let Some(projected) = junie_step_output_projection(step) {
                if matches!(
                    projected.outcome,
                    JunieOutputOutcome::Failure | JunieOutputOutcome::Timeout
                ) {
                    rows.push(output_failure_event(
                        *event_index,
                        occurred_at,
                        step,
                        projected.details,
                        projected.outcome,
                        binding,
                    ));
                }
                increment_event_index(event_index)?;
            }
            continue;
        }
        for (change_index, change) in step.changes.iter().enumerate() {
            if let Some(event) = file_change_event(
                *event_index,
                occurred_at,
                step,
                change_index,
                change,
                binding,
            ) {
                rows.push(event);
                increment_event_index(event_index)?;
            }
        }
    }
    let final_text = junie_buffer_result_text(&buffer);
    if !final_text.is_empty() {
        rows.push(EventDraft {
            event_index: *event_index,
            event_type: EventType::Message,
            role: Some(EventRole::Assistant),
            occurred_at,
            text: final_text.clone(),
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
            source_backed_binding: SourceBackedBinding {
                records: binding.clone(),
                target: SourceBackedTarget::AssistantMessage,
            },
            file_change: None,
        });
        increment_event_index(event_index)?;
    }
    Ok(())
}

fn increment_event_index(event_index: &mut u64) -> Result<()> {
    *event_index = event_index
        .checked_add(1)
        .ok_or(CaptureError::SystemInvariant(
            "Junie provider event index exhausted",
        ))?;
    Ok(())
}

fn step_event(
    event_index: u64,
    occurred_at: DateTime<Utc>,
    step: &JunieStepAgg,
    binding: &RecordSetBinding,
) -> EventDraft {
    let (text, body) = if let Some(command) = &step.command {
        (
            format!("Bash: {command}"),
            json!({
                "tool_name": "Bash",
                "command": command,
                "label": step.label,
                "status": step.status,
            }),
        )
    } else if let Some(files) = &step.files {
        (
            step.label
                .clone()
                .unwrap_or_else(|| "View files".to_owned()),
            json!({
                "tool_name": "view",
                "label": step.label,
                "files": files,
                "status": step.status,
            }),
        )
    } else {
        (
            step.label
                .clone()
                .unwrap_or_else(|| "Junie tool step".to_owned()),
            json!({
                "tool_name": "tool",
                "label": step.label,
                "status": step.status,
            }),
        )
    };
    EventDraft {
        event_index,
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at,
        text,
        body,
        source_backed_binding: SourceBackedBinding {
            records: binding.clone(),
            target: SourceBackedTarget::StepCall {
                step_order: u32::try_from(step.order).unwrap_or(u32::MAX),
            },
        },
        file_change: None,
    }
}

fn output_failure_event(
    event_index: u64,
    occurred_at: DateTime<Utc>,
    step: &JunieStepAgg,
    details: &str,
    outcome: JunieOutputOutcome,
    binding: &RecordSetBinding,
) -> EventDraft {
    let timed_out = outcome == JunieOutputOutcome::Timeout;
    let tool_name = if step.command.is_some() {
        "Bash"
    } else if step.files.is_some() {
        "view"
    } else {
        "tool"
    };
    EventDraft {
        event_index,
        event_type: if step.command.is_some() {
            EventType::CommandOutput
        } else {
            EventType::ToolOutput
        },
        role: Some(EventRole::Tool),
        occurred_at,
        text: provider_local_preview(details, PROVIDER_MAX_PREVIEW_CHARS).0,
        body: json!({
            "tool_name": tool_name,
            "details": details,
            "output_preview": provider_local_preview(details, PROVIDER_MAX_PREVIEW_CHARS).0,
            "status": step.status,
            "call_id": format!("step:{}", step.order),
            "provider_step_id": step.provider_step_id,
            "command": step.command,
            "exit_code": step.exit_code,
            "duration_ms": step.duration_ms,
            "timed_out": timed_out,
            "result_outcome": "failure",
        }),
        source_backed_binding: SourceBackedBinding {
            records: binding.clone(),
            target: SourceBackedTarget::StepOutput {
                step_order: u32::try_from(step.order).unwrap_or(u32::MAX),
            },
        },
        file_change: None,
    }
}

fn file_change_event(
    event_index: u64,
    occurred_at: DateTime<Utc>,
    step: &JunieStepAgg,
    change_index: usize,
    change: &Value,
    binding: &RecordSetBinding,
) -> Option<EventDraft> {
    let before_path = change.get("beforeRelativePath").and_then(Value::as_str);
    let after_path = change.get("afterRelativePath").and_then(Value::as_str);
    let path = after_path
        .or(before_path)
        .filter(|path| !path.trim().is_empty())?;
    let change_kind = match (before_path, after_path) {
        (None, Some(_)) => "created",
        (Some(_), None) => "deleted",
        (Some(before), Some(after)) if before != after => "renamed",
        _ => "modified",
    };
    Some(EventDraft {
        event_index,
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at,
        text: format!("Edit: {path}"),
        body: json!({
            "tool_name": "Edit",
            "file_path": path,
            "old_string": file_content_text(change.get("beforeContent")),
            "new_string": file_content_text(change.get("afterContent")),
            "before_relative_path": before_path,
            "after_relative_path": after_path,
            "change_kind": change_kind,
            "status": step.status,
        }),
        source_backed_binding: SourceBackedBinding {
            records: binding.clone(),
            target: SourceBackedTarget::FileChange {
                step_order: u32::try_from(step.order).unwrap_or(u32::MAX),
                change_index: u32::try_from(change_index).unwrap_or(u32::MAX),
            },
        },
        file_change: Some(FileChangeDraft {
            path: path.to_owned(),
        }),
    })
}

fn file_content_text(value: Option<&Value>) -> Option<String> {
    let value = value?;
    value
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| value.as_str())
        .map(str::to_owned)
}
