use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeState {
    pub(super) started_at_ms: i64,
    pub(super) last_ts_ms: i64,
    pub(super) ended_at_ms: Option<i64>,
    pub(super) title: Option<String>,
    pub(super) cwd: Option<String>,
    pub(super) saw_supported_event: bool,
}

impl RuntimeState {
    pub(super) fn fresh(meta: &JunieIndexMeta, imported_at: DateTime<Utc>) -> Self {
        let started_at = provider_timestamp_millis(meta.created_at, imported_at);
        Self {
            started_at_ms: started_at.timestamp_millis(),
            last_ts_ms: started_at.timestamp_millis(),
            ended_at_ms: meta
                .updated_at
                .map(|value| provider_timestamp_millis(Some(value), started_at).timestamp_millis()),
            title: meta.task_name.clone(),
            cwd: meta.project_dir.clone(),
            saw_supported_event: false,
        }
    }

    pub(super) fn started_at(&self) -> DateTime<Utc> {
        timestamp(self.started_at_ms)
    }

    pub(super) fn last_ts(&self) -> DateTime<Utc> {
        timestamp(self.last_ts_ms)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Frontier {
    pub(super) offset: u64,
    pub(super) next_ordinal: u64,
    pub(super) next_event_index: u64,
    pub(super) prefix_sha256: [u8; 32],
    pub(super) state: RuntimeState,
}

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
    pub(super) fn observe(
        &mut self,
        ordinal: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
        payload: &[u8],
    ) {
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

pub(super) struct ParsedTurn {
    pub(super) rows: Vec<EventDraft>,
    pub(super) end_offset: u64,
    pub(super) end_ordinal: u64,
    pub(super) next_event_index: u64,
    pub(super) after_state: RuntimeState,
    pub(super) terminal: bool,
    pub(super) incomplete: bool,
    pub(super) after_prefix_sha256: [u8; 32],
    pub(super) rejection_count: u64,
}

pub(super) fn timestamp(millis: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp_millis(millis).unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

pub(super) fn parse_session_turn(
    session_path: &JunieSessionPath,
    frontier: &Frontier,
) -> Result<ParsedTurn> {
    let opened = session_path.open_events()?;
    let mut reader = BufReader::new(opened.file().try_clone()?);
    reader.seek(SeekFrom::Start(frontier.offset))?;
    let start_offset = frontier.offset;
    let start_ordinal = frontier.next_ordinal;
    let base_event_index = frontier.next_event_index;
    let mut ordinal = start_ordinal;
    let mut event_index = base_event_index;
    let mut state = frontier.state.clone();
    let mut buffer = JunieAssistantBuffer::default();
    let mut binding = RecordSetBinding::default();
    let mut rows = Vec::new();
    let mut rejection_count = 0_u64;
    let mut retained_turn_bytes = 0_usize;
    let mut line = Vec::new();
    let mut incomplete = false;
    let mut terminal = false;

    loop {
        let byte_start = reader.stream_position()?;
        let read =
            crate::common::io::read_provider_jsonl_line_or_skip_oversized(&mut reader, &mut line)?;
        let byte_end = reader.stream_position()?;
        if !matches!(&read, crate::common::io::ProviderJsonlLineRead::Eof)
            && byte_end.saturating_sub(start_offset) > MAX_JUNIE_TRANSIENT_TURN_BYTES as u64
        {
            rows.clear();
            record_rejection(&mut rejection_count);
            ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Junie source ordinal exhausted",
            ))?;
            break;
        }
        match read {
            crate::common::io::ProviderJsonlLineRead::Eof => {
                terminal = true;
                flush_assistant(&mut buffer, &binding, &state, &mut event_index, &mut rows)?;
                break;
            }
            crate::common::io::ProviderJsonlLineRead::Oversized { .. } => {
                record_rejection(&mut rejection_count);
                ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                    "Junie source ordinal exhausted",
                ))?;
                continue;
            }
            crate::common::io::ProviderJsonlLineRead::Line { .. } => {}
        }
        if line.last() != Some(&b'\n') {
            incomplete = true;
            record_rejection(&mut rejection_count);
            rows.clear();
            opened.revalidate()?;
            return Ok(ParsedTurn {
                rows,
                end_offset: start_offset,
                end_ordinal: start_ordinal,
                next_event_index: base_event_index,
                after_state: frontier.state.clone(),
                terminal: false,
                incomplete,
                after_prefix_sha256: frontier.prefix_sha256,
                rejection_count,
            });
        }
        let payload = strip_jsonl_ending(&line);
        let current_ordinal = ordinal;
        ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "Junie source ordinal exhausted",
        ))?;
        if payload.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let value = match serde_json::from_slice::<Value>(payload) {
            Ok(value) => value,
            Err(_) => {
                record_rejection(&mut rejection_count);
                continue;
            }
        };
        match value.get("kind").and_then(Value::as_str).unwrap_or("") {
            "UserPromptEvent" => {
                flush_assistant(&mut buffer, &binding, &state, &mut event_index, &mut rows)?;
                let prompt = value.get("prompt").and_then(Value::as_str).unwrap_or("");
                if !prompt.trim().is_empty() {
                    state.saw_supported_event = true;
                    let mut user_binding = RecordSetBinding::default();
                    user_binding.observe(current_ordinal, byte_start, byte_end, payload);
                    rows.push(EventDraft {
                        event_index,
                        event_type: EventType::Message,
                        role: Some(EventRole::User),
                        occurred_at: state.last_ts(),
                        text: prompt.to_owned(),
                        body: json!({
                            "kind": "UserPromptEvent",
                            "prompt": prompt,
                        }),
                        source_backed_binding: SourceBackedBinding {
                            records: user_binding,
                            target: SourceBackedTarget::UserPrompt,
                        },
                        file_change: None,
                    });
                    event_index =
                        event_index
                            .checked_add(1)
                            .ok_or(CaptureError::SystemInvariant(
                                "Junie provider event index exhausted",
                            ))?;
                }
                break;
            }
            "SessionA2uxEvent" => {
                if let Some(timestamp) = value
                    .get("timestampMs")
                    .and_then(json_i64)
                    .and_then(DateTime::<Utc>::from_timestamp_millis)
                {
                    state.last_ts_ms = timestamp.timestamp_millis();
                    state.ended_at_ms = Some(timestamp.timestamp_millis());
                }
                let agent = value
                    .get("event")
                    .and_then(|event| event.get("agentEvent"))
                    .unwrap_or(&Value::Null);
                let kind = agent.get("kind").and_then(Value::as_str).unwrap_or("");
                match kind {
                    "AgentTaskNameUpdatedEvent" => {
                        if let Some(title) = agent
                            .get("name")
                            .and_then(Value::as_str)
                            .filter(|value| !value.trim().is_empty())
                        {
                            state.title =
                                Some(provider_local_preview(title, PROVIDER_MAX_PREVIEW_CHARS).0);
                        }
                    }
                    "CurrentDirectoryUpdatedEvent" => {
                        if let Some(cwd) = agent
                            .get("currentDirectory")
                            .and_then(Value::as_str)
                            .filter(|value| !value.trim().is_empty())
                        {
                            state.cwd =
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
                        let retained = retained_turn_bytes.checked_add(payload.len());
                        if retained.is_none_or(|bytes| bytes > MAX_JUNIE_TRANSIENT_TURN_BYTES) {
                            buffer = JunieAssistantBuffer::default();
                            binding = RecordSetBinding::default();
                            retained_turn_bytes = 0;
                            record_rejection(&mut rejection_count);
                            continue;
                        }
                        retained_turn_bytes = retained.unwrap_or_default();
                        binding.observe(current_ordinal, byte_start, byte_end, payload);
                        if junie_merge_buffered_agent_event(
                            &mut buffer,
                            agent,
                            current_ordinal.saturating_add(1),
                            state.last_ts(),
                        ) {
                            state.saw_supported_event = true;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    let end_offset = reader.stream_position()?;
    let after_prefix_sha256 = hash_prefix(session_path, end_offset)?;
    opened.revalidate()?;
    Ok(ParsedTurn {
        rows,
        end_offset,
        end_ordinal: ordinal,
        next_event_index: event_index,
        after_state: state,
        terminal,
        incomplete,
        after_prefix_sha256,
        rejection_count,
    })
}

pub(super) fn strip_jsonl_ending(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n")
        .unwrap_or(line)
        .strip_suffix(b"\r")
        .unwrap_or_else(|| line.strip_suffix(b"\n").unwrap_or(line))
}

fn hash_prefix(session_path: &JunieSessionPath, length: u64) -> Result<[u8; 32]> {
    let opened = session_path.open_events()?;
    if opened.len() < length {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut file = opened.file().try_clone()?;
    let mut remaining = length;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    while remaining != 0 {
        let limit = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| CaptureError::SystemInvariant("Junie range exceeds usize"))?;
        let read = file.read(&mut buffer[..limit])?;
        if read == 0 {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        digest.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    opened.revalidate()?;
    Ok(digest.finalize().into())
}

pub(super) fn json_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_f64().map(|value| value.round() as i64))
}

fn record_rejection(rejection_count: &mut u64) {
    *rejection_count = rejection_count.saturating_add(1);
}

pub(super) fn flush_assistant(
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
            *event_index = event_index
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Junie provider event index exhausted",
                ))?;
            if let Some(projected) = junie_step_output_projection(step) {
                let retained = matches!(
                    projected.outcome,
                    JunieOutputOutcome::Failure | JunieOutputOutcome::Timeout
                );
                if retained {
                    rows.push(output_failure_event(
                        *event_index,
                        occurred_at,
                        step,
                        projected.details,
                        projected.outcome,
                        binding,
                    ));
                }
                *event_index = event_index
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Junie provider event index exhausted",
                    ))?;
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
                *event_index = event_index
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Junie provider event index exhausted",
                    ))?;
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
        *event_index = event_index
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Junie provider event index exhausted",
            ))?;
    }
    Ok(())
}

pub(super) fn step_event(
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

pub(super) fn output_failure_event(
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

pub(super) fn file_change_event(
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

pub(super) fn file_content_text(value: Option<&Value>) -> Option<String> {
    let value = value?;
    value
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| value.as_str())
        .map(str::to_owned)
}
