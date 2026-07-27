use super::*;

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

    pub(super) fn encoded(&self, tag: u8, target: u32) -> Option<Vec<u8>> {
        if self.unavailable || self.entries.is_empty() {
            return None;
        }
        let count = u16::try_from(self.entries.len()).ok()?;
        let mut encoded = Vec::with_capacity(7 + self.entries.len() * 24);
        encoded.extend_from_slice(&count.to_be_bytes());
        encoded.push(tag);
        encoded.extend_from_slice(&target.to_be_bytes());
        for entry in &self.entries {
            encoded.extend_from_slice(&entry.ordinal.to_be_bytes());
            encoded.extend_from_slice(&entry.byte_start.to_be_bytes());
            encoded.extend_from_slice(&entry.byte_end_exclusive.to_be_bytes());
        }
        crate::complete_content::jsonl::valid_junie_record_set_locator(&encoded).then_some(encoded)
    }

    pub(super) fn record_digest(&self) -> Option<CompleteContentBodyDigest> {
        if self.unavailable || self.entries.is_empty() {
            return None;
        }
        let mut digest = Sha256::new();
        digest.update(RECORD_SET_DIGEST_DOMAIN);
        digest.update((self.entries.len() as u64).to_be_bytes());
        for entry in &self.entries {
            digest.update(entry.ordinal.to_be_bytes());
            digest.update(entry.byte_start.to_be_bytes());
            digest.update(entry.byte_end_exclusive.to_be_bytes());
            digest.update(entry.payload_sha256);
        }
        CompleteContentBodyDigest::parse(format!("{:x}", digest.finalize()))
    }

    pub(super) fn native_record_id(&self, target: &str) -> Option<String> {
        Some(format!(
            "junie-records-{}-{}-{target}",
            self.entries.first()?.ordinal,
            self.entries.last()?.ordinal
        ))
    }
}

#[derive(Debug, Clone)]
pub(super) struct FileChangeDraft {
    pub(super) path: String,
    pub(super) old_path: Option<String>,
    pub(super) change_kind: FileChangeKind,
    pub(super) touch_index: u64,
}

#[derive(Debug, Clone)]
pub(super) struct EventDraft {
    pub(super) event_index: u64,
    pub(super) event_hash: String,
    pub(super) event_type: EventType,
    pub(super) role: Option<EventRole>,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) text: String,
    pub(super) body: Value,
    pub(super) metadata: Value,
    pub(super) source_ordinal: u64,
    pub(super) source_subrecord: u32,
    pub(super) binding: Option<(RecordSetBinding, VerifiedContentRole, u8, u32, String)>,
    pub(super) file_change: Option<FileChangeDraft>,
}

#[derive(Debug, Clone)]
pub(super) struct OutputDraft {
    pub(super) event_index: u64,
    pub(super) source_ordinal: u64,
    pub(super) source_subrecord: u32,
    pub(super) byte_start: u64,
    pub(super) byte_end_exclusive: u64,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) call_id: String,
    pub(super) tool_name: String,
    pub(super) command: Option<String>,
    pub(super) outcome: OutputOutcome,
    pub(super) exit_code: Option<i32>,
    pub(super) duration_ms: Option<u64>,
    pub(super) locator_payload: Vec<u8>,
    pub(super) native_record_id: String,
    pub(super) content: Vec<u8>,
}

pub(super) struct ParsedTurn {
    pub(super) rows: Vec<EventDraft>,
    pub(super) outputs: Vec<OutputDraft>,
    pub(super) start_offset: u64,
    pub(super) end_offset: u64,
    pub(super) start_ordinal: u64,
    pub(super) end_ordinal: u64,
    pub(super) base_event_index: u64,
    pub(super) next_event_index: u64,
    pub(super) after_state: RuntimeState,
    pub(super) terminal: bool,
    pub(super) incomplete: bool,
    pub(super) turn_sha256: [u8; 32],
    pub(super) after_prefix_sha256: [u8; 32],
    pub(super) rejection_count: u64,
    pub(super) rejections: Vec<ProviderImportFailure>,
}

pub(super) fn timestamp(millis: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp_millis(millis).unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

pub(super) fn parse_turn(path: &Path, frontier: &Frontier) -> Result<ParsedTurn> {
    crate::common::io::ensure_regular_provider_transcript_file(path)?;
    let mut reader = BufReader::new(File::open(path)?);
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
    let mut outputs = Vec::new();
    let mut failures = Vec::new();
    let mut rejection_count = 0_u64;
    let mut retained_turn_bytes = 0_usize;
    let mut line = Vec::new();
    let mut incomplete = false;
    let mut terminal = false;

    loop {
        if let Some(pending) = &frontier.pending {
            if reader.stream_position()? == pending.end_offset {
                terminal = pending.terminal;
                flush_assistant(
                    &mut buffer,
                    &binding,
                    &state,
                    ordinal.saturating_sub(1),
                    &mut event_index,
                    &mut rows,
                    &mut outputs,
                )?;
                break;
            }
        }
        let byte_start = reader.stream_position()?;
        let read =
            crate::common::io::read_provider_jsonl_line_or_skip_oversized(&mut reader, &mut line)?;
        let byte_end = reader.stream_position()?;
        if !matches!(&read, crate::common::io::ProviderJsonlLineRead::Eof)
            && byte_end.saturating_sub(start_offset) > MAX_JUNIE_TRANSIENT_TURN_BYTES as u64
        {
            rows.clear();
            outputs.clear();
            record_rejection(
                &mut failures,
                &mut rejection_count,
                failure(
                    ordinal,
                    format!(
                        "Junie turn scan exceeds the {MAX_JUNIE_TRANSIENT_TURN_BYTES} byte safe-boundary limit"
                    ),
                ),
            );
            ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Junie source ordinal exhausted",
            ))?;
            break;
        }
        match read {
            crate::common::io::ProviderJsonlLineRead::Eof => {
                terminal = true;
                flush_assistant(
                    &mut buffer,
                    &binding,
                    &state,
                    ordinal.saturating_sub(1),
                    &mut event_index,
                    &mut rows,
                    &mut outputs,
                )?;
                break;
            }
            crate::common::io::ProviderJsonlLineRead::Oversized { .. } => {
                record_rejection(
                    &mut failures,
                    &mut rejection_count,
                    failure(
                        ordinal,
                        format!(
                            "Junie events JSONL line exceeds the {} byte limit",
                            crate::MAX_PROVIDER_JSONL_LINE_BYTES
                        ),
                    ),
                );
                ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                    "Junie source ordinal exhausted",
                ))?;
                continue;
            }
            crate::common::io::ProviderJsonlLineRead::Line { .. } => {}
        }
        if line.last() != Some(&b'\n') {
            incomplete = true;
            record_rejection(
                &mut failures,
                &mut rejection_count,
                failure(ordinal, "incomplete trailing Junie JSONL record".to_owned()),
            );
            rows.clear();
            outputs.clear();
            return Ok(ParsedTurn {
                rows,
                outputs,
                start_offset,
                end_offset: start_offset,
                start_ordinal,
                end_ordinal: start_ordinal,
                base_event_index,
                next_event_index: base_event_index,
                after_state: frontier.state.clone(),
                terminal: false,
                incomplete,
                turn_sha256: Sha256::digest([]).into(),
                after_prefix_sha256: frontier.prefix_sha256,
                rejection_count,
                rejections: failures,
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
            Err(error) => {
                record_rejection(
                    &mut failures,
                    &mut rejection_count,
                    failure(
                        current_ordinal,
                        format!("malformed Junie events JSONL: {error}"),
                    ),
                );
                continue;
            }
        };
        match value.get("kind").and_then(Value::as_str).unwrap_or("") {
            "UserPromptEvent" => {
                flush_assistant(
                    &mut buffer,
                    &binding,
                    &state,
                    current_ordinal.saturating_sub(1),
                    &mut event_index,
                    &mut rows,
                    &mut outputs,
                )?;
                let prompt = value.get("prompt").and_then(Value::as_str).unwrap_or("");
                if !prompt.trim().is_empty() {
                    state.saw_supported_event = true;
                    let mut user_binding = RecordSetBinding::default();
                    user_binding.observe(current_ordinal, byte_start, byte_end, payload);
                    rows.push(EventDraft {
                        event_index,
                        event_hash: format!("line:{}:user", current_ordinal.saturating_add(1)),
                        event_type: EventType::Message,
                        role: Some(EventRole::User),
                        occurred_at: state.last_ts(),
                        text: prompt.to_owned(),
                        body: json!({
                            "kind": "UserPromptEvent",
                            "prompt": prompt,
                        }),
                        metadata: json!({
                            "source": "junie_user_prompt",
                            "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                        }),
                        source_ordinal: current_ordinal,
                        source_subrecord: 0,
                        binding: Some((
                            user_binding,
                            VerifiedContentRole::MessageBody,
                            3,
                            0,
                            "user-prompt".to_owned(),
                        )),
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
                            record_rejection(&mut failures, &mut rejection_count, failure(
                                current_ordinal,
                                format!(
                                    "Junie assistant turn exceeds the {MAX_JUNIE_TRANSIENT_TURN_BYTES} byte transient buffer limit"
                                ),
                            ));
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
    let after_prefix_sha256 = hash_prefix(path, end_offset)?;
    let turn_sha256 = hash_range(path, start_offset, end_offset)?;
    Ok(ParsedTurn {
        rows,
        outputs,
        start_offset,
        end_offset,
        start_ordinal,
        end_ordinal: ordinal,
        base_event_index,
        next_event_index: event_index,
        after_state: state,
        terminal,
        incomplete,
        turn_sha256,
        after_prefix_sha256,
        rejection_count,
        rejections: failures,
    })
}

pub(super) fn strip_jsonl_ending(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n")
        .unwrap_or(line)
        .strip_suffix(b"\r")
        .unwrap_or_else(|| line.strip_suffix(b"\n").unwrap_or(line))
}

pub(super) fn hash_range(path: &Path, start: u64, end: u64) -> Result<[u8; 32]> {
    let length = end
        .checked_sub(start)
        .ok_or(CaptureError::SystemInvariant("Junie range moved backwards"))?;
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
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
    Ok(digest.finalize().into())
}

pub(super) fn json_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_f64().map(|value| value.round() as i64))
}

pub(super) fn failure(ordinal: u64, mut error: String) -> ProviderImportFailure {
    if error.len() > MAX_JUNIE_FAILURE_BYTES {
        let mut boundary = MAX_JUNIE_FAILURE_BYTES;
        while !error.is_char_boundary(boundary) {
            boundary = boundary.saturating_sub(1);
        }
        error.truncate(boundary);
    }
    ProviderImportFailure {
        line: usize::try_from(ordinal.saturating_add(1)).unwrap_or(usize::MAX),
        error,
    }
}

pub(super) fn record_rejection(
    failures: &mut Vec<ProviderImportFailure>,
    rejection_count: &mut u64,
    failure: ProviderImportFailure,
) {
    *rejection_count = rejection_count.saturating_add(1);
    if failures.len() < MAX_JUNIE_FAILURES {
        failures.push(failure);
    }
}

pub(super) fn flush_assistant(
    buffer: &mut JunieAssistantBuffer,
    binding: &RecordSetBinding,
    state: &RuntimeState,
    source_ordinal: u64,
    event_index: &mut u64,
    rows: &mut Vec<EventDraft>,
    outputs: &mut Vec<OutputDraft>,
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
            rows.push(step_event(*event_index, occurred_at, source_ordinal, step));
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
                let locator = binding
                    .encoded(2, u32::try_from(step.order).unwrap_or(u32::MAX))
                    .zip(binding.native_record_id(&format!("step-output-{}", step.order)));
                if retained {
                    rows.push(output_failure_event(
                        *event_index,
                        occurred_at,
                        source_ordinal,
                        step,
                        projected.details,
                        projected.outcome,
                    ));
                } else if let Some((locator_payload, native_record_id)) = locator {
                    let first = binding
                        .entries
                        .first()
                        .ok_or(CaptureError::SystemInvariant(
                            "Junie output binding lost its first source record",
                        ))?;
                    let last = binding.entries.last().ok_or(CaptureError::SystemInvariant(
                        "Junie output binding lost its last source record",
                    ))?;
                    outputs.push(OutputDraft {
                        event_index: *event_index,
                        source_ordinal: first.ordinal,
                        source_subrecord: u32::try_from(step.order).unwrap_or(u32::MAX),
                        byte_start: first.byte_start,
                        byte_end_exclusive: last.byte_end_exclusive,
                        occurred_at,
                        call_id: projected.call_id,
                        tool_name: projected.tool_name.to_owned(),
                        command: projected.command.map(str::to_owned),
                        outcome: match projected.outcome {
                            JunieOutputOutcome::Success => OutputOutcome::Success,
                            JunieOutputOutcome::Failure => OutputOutcome::Failure,
                            JunieOutputOutcome::Timeout => OutputOutcome::Timeout,
                            JunieOutputOutcome::Unknown => OutputOutcome::Unknown,
                        },
                        exit_code: projected.exit_code,
                        duration_ms: projected.duration_ms,
                        locator_payload,
                        native_record_id,
                        content: projected.details.as_bytes().to_vec(),
                    });
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
                source_ordinal,
                step,
                change_index,
                change,
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
            event_hash: format!("assistant-result:{}", *event_index),
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
            source_ordinal,
            source_subrecord: u32::try_from(rows.len()).unwrap_or(u32::MAX),
            binding: Some((
                binding.clone(),
                VerifiedContentRole::MessageBody,
                1,
                0,
                "message".to_owned(),
            )),
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
    source_ordinal: u64,
    step: &JunieStepAgg,
) -> EventDraft {
    let (tool_name, text, body) = if let Some(command) = &step.command {
        (
            "Bash",
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
            "view",
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
            "tool",
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
        event_hash: format!("step:{}:tool", step.order),
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at,
        text,
        body,
        metadata: json!({
            "source": "junie_step",
            "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            "tool_name": tool_name,
        }),
        source_ordinal,
        source_subrecord: u32::try_from(step.order).unwrap_or(u32::MAX),
        binding: None,
        file_change: None,
    }
}

pub(super) fn output_failure_event(
    event_index: u64,
    occurred_at: DateTime<Utc>,
    source_ordinal: u64,
    step: &JunieStepAgg,
    details: &str,
    outcome: JunieOutputOutcome,
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
        event_hash: format!("step:{}:output", step.order),
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
        metadata: json!({
            "source": "junie_step_details",
            "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            "tool_name": tool_name,
        }),
        source_ordinal,
        source_subrecord: u32::try_from(step.order.saturating_add(1)).unwrap_or(u32::MAX),
        binding: None,
        file_change: None,
    }
}

pub(super) fn file_change_event(
    event_index: u64,
    occurred_at: DateTime<Utc>,
    source_ordinal: u64,
    step: &JunieStepAgg,
    change_index: usize,
    change: &Value,
) -> Option<EventDraft> {
    let before_path = change.get("beforeRelativePath").and_then(Value::as_str);
    let after_path = change.get("afterRelativePath").and_then(Value::as_str);
    let path = after_path
        .or(before_path)
        .filter(|path| !path.trim().is_empty())?;
    let change_kind = match (before_path, after_path) {
        (None, Some(_)) => FileChangeKind::Created,
        (Some(_), None) => FileChangeKind::Deleted,
        (Some(before), Some(after)) if before != after => FileChangeKind::Renamed,
        _ => FileChangeKind::Modified,
    };
    Some(EventDraft {
        event_index,
        event_hash: format!("step:{}:change:{change_index}", step.order),
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
            "change_kind": change_kind.as_str(),
            "status": step.status,
        }),
        metadata: json!({
            "source": "junie_file_change",
            "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            "tool_name": "Edit",
            "change_kind": change_kind.as_str(),
        }),
        source_ordinal,
        source_subrecord: u32::try_from(change_index).unwrap_or(u32::MAX),
        binding: None,
        file_change: Some(FileChangeDraft {
            path: path.to_owned(),
            old_path: before_path
                .filter(|before| after_path.is_some_and(|after| after != *before))
                .map(str::to_owned),
            change_kind,
            touch_index: event_index
                .saturating_mul(1_000)
                .saturating_add(change_index as u64),
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
