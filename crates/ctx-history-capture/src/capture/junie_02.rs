#[allow(unused_imports)]
use super::*;

pub(crate) fn junie_merge_step(
    buffer: &mut JunieAssistantBuffer,
    agent_event: &Value,
    occurred_at: DateTime<Utc>,
) {
    let Some(step_id) = agent_event
        .get("stepId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    junie_ensure_assistant(buffer, occurred_at);
    let next_order = buffer.steps.len();
    let step = buffer
        .steps
        .entry(step_id.to_owned())
        .or_insert_with(|| JunieStepAgg {
            order: next_order,
            label: None,
            command: None,
            files: None,
            changes: Vec::new(),
            details: None,
            status: None,
        });
    if let Some(text) = agent_event.get("text").and_then(Value::as_str) {
        if !text.trim().is_empty() {
            step.label = Some(text.to_owned());
        }
    }
    if let Some(command) = agent_event.get("command").and_then(Value::as_str) {
        if !command.trim().is_empty() {
            step.command = Some(command.to_owned());
        }
    }
    if let Some(files) = agent_event.get("files").filter(|value| value.is_array()) {
        step.files = Some(files.clone());
    }
    if let Some(changes) = agent_event.get("changes").and_then(Value::as_array) {
        step.changes = changes.clone();
    }
    if let Some(details) = agent_event.get("details").and_then(Value::as_str) {
        if !details.trim().is_empty() {
            step.details = Some(details.to_owned());
        }
    }
    if let Some(status) = agent_event.get("status").and_then(Value::as_str) {
        if !status.trim().is_empty() {
            step.status = Some(status.to_owned());
        }
    }
}

pub(crate) fn junie_flush_assistant(
    buffer: &mut JunieAssistantBuffer,
    base_draft: &NativeSessionDraft,
    context: &ProviderAdapterContext,
    result: &mut ProviderNormalizationResult,
    line_number: usize,
    provider_event_index: &mut u64,
) {
    if !buffer.open {
        return;
    }
    let occurred_at = buffer.turn_ts.unwrap_or(base_draft.started_at);
    let mut steps = buffer.steps.values().cloned().collect::<Vec<_>>();
    steps.sort_by_key(|step| step.order);
    for step in &steps {
        if !step.changes.is_empty() {
            junie_emit_file_changes(
                base_draft,
                context,
                result,
                line_number,
                provider_event_index,
                occurred_at,
                step,
            );
        } else {
            junie_emit_step_events(
                base_draft,
                context,
                result,
                line_number,
                provider_event_index,
                occurred_at,
                step,
            );
        }
    }
    let final_text = buffer
        .results
        .values()
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n");
    if !final_text.trim().is_empty() {
        let index = *provider_event_index;
        let event = native_event(NativeEventDraft {
            provider: CaptureProvider::Junie,
            source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            provider_session_id: base_draft.provider_session_id.clone(),
            provider_event_index: index,
            provider_event_hash: Some(format!("assistant-result:{index}")),
            cursor: format!(
                "{}:line:{line_number}:event:{index}",
                base_draft.raw_source_path
            ),
            event_type: EventType::Message,
            role: Some(EventRole::Assistant),
            occurred_at,
            text: final_text,
            body: json!({
                "result_blocks": buffer.results.clone(),
                "model": buffer.usage.model.clone(),
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
                "model": buffer.usage.model.clone(),
                "usage": {
                    "input_tokens": buffer.usage.input_tokens,
                    "output_tokens": buffer.usage.output_tokens,
                    "cache_read_tokens": buffer.usage.cache_read_tokens,
                    "cache_write_tokens": buffer.usage.cache_write_tokens,
                },
            }),
        });
        *provider_event_index = (*provider_event_index).saturating_add(1);
        result.captures.push((
            line_number,
            native_provider_capture(base_draft.clone(), context, Some(event)),
        ));
    }
    *buffer = JunieAssistantBuffer::default();
}

pub(crate) fn junie_emit_step_events(
    base_draft: &NativeSessionDraft,
    context: &ProviderAdapterContext,
    result: &mut ProviderNormalizationResult,
    line_number: usize,
    provider_event_index: &mut u64,
    occurred_at: DateTime<Utc>,
    step: &JunieStepAgg,
) {
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
    let tool_index = *provider_event_index;
    let tool_event = native_event(NativeEventDraft {
        provider: CaptureProvider::Junie,
        source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        provider_session_id: base_draft.provider_session_id.clone(),
        provider_event_index: tool_index,
        provider_event_hash: Some(format!("step:{}:tool", step.order)),
        cursor: format!(
            "{}:line:{line_number}:event:{tool_index}",
            base_draft.raw_source_path
        ),
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at,
        text,
        body: body.clone(),
        metadata: json!({
            "source": "junie_step",
            "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            "tool_name": tool_name,
        }),
    });
    *provider_event_index = (*provider_event_index).saturating_add(1);
    result.captures.push((
        line_number,
        native_provider_capture(base_draft.clone(), context, Some(tool_event)),
    ));

    if let Some(details) = &step.details {
        if !details.trim().is_empty() {
            let output_index = *provider_event_index;
            let output_event = native_event(NativeEventDraft {
                provider: CaptureProvider::Junie,
                source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                provider_session_id: base_draft.provider_session_id.clone(),
                provider_event_index: output_index,
                provider_event_hash: Some(format!("step:{}:output", step.order)),
                cursor: format!(
                    "{}:line:{line_number}:event:{output_index}",
                    base_draft.raw_source_path
                ),
                event_type: if step.command.is_some() {
                    EventType::CommandOutput
                } else {
                    EventType::ToolOutput
                },
                role: Some(EventRole::Tool),
                occurred_at,
                text: details.clone(),
                body: json!({
                    "tool_name": tool_name,
                    "details": details,
                    "status": step.status,
                }),
                metadata: json!({
                    "source": "junie_step_details",
                    "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                    "tool_name": tool_name,
                }),
            });
            *provider_event_index = (*provider_event_index).saturating_add(1);
            result.captures.push((
                line_number,
                native_provider_capture(base_draft.clone(), context, Some(output_event)),
            ));
        }
    }
}

pub(crate) fn junie_emit_file_changes(
    base_draft: &NativeSessionDraft,
    context: &ProviderAdapterContext,
    result: &mut ProviderNormalizationResult,
    line_number: usize,
    provider_event_index: &mut u64,
    occurred_at: DateTime<Utc>,
    step: &JunieStepAgg,
) {
    for (change_index, change) in step.changes.iter().enumerate() {
        let before_path = change.get("beforeRelativePath").and_then(Value::as_str);
        let after_path = change.get("afterRelativePath").and_then(Value::as_str);
        let Some(path) = after_path.or(before_path) else {
            continue;
        };
        if path.trim().is_empty() {
            continue;
        }
        let change_kind = match (before_path, after_path) {
            (None, Some(_)) => FileChangeKind::Created,
            (Some(_), None) => FileChangeKind::Deleted,
            (Some(before), Some(after)) if before != after => FileChangeKind::Renamed,
            _ => FileChangeKind::Modified,
        };
        let event_index = *provider_event_index;
        let event = native_event(NativeEventDraft {
            provider: CaptureProvider::Junie,
            source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            provider_session_id: base_draft.provider_session_id.clone(),
            provider_event_index: event_index,
            provider_event_hash: Some(format!("step:{}:change:{change_index}", step.order)),
            cursor: format!(
                "{}:line:{line_number}:event:{event_index}",
                base_draft.raw_source_path
            ),
            event_type: EventType::ToolCall,
            role: Some(EventRole::Assistant),
            occurred_at,
            text: format!("Edit: {path}"),
            body: json!({
                "tool_name": "Edit",
                "file_path": path,
                "old_string": junie_file_content_text(change.get("beforeContent")),
                "new_string": junie_file_content_text(change.get("afterContent")),
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
        });
        *provider_event_index = (*provider_event_index).saturating_add(1);
        result.captures.push((
            line_number,
            native_provider_capture(base_draft.clone(), context, Some(event)),
        ));
        result.files_touched.push((
            line_number,
            ProviderFileTouchedEnvelope {
                provider: CaptureProvider::Junie,
                provider_session_id: base_draft.provider_session_id.clone(),
                provider_touch_index: event_index
                    .saturating_mul(1_000)
                    .saturating_add(change_index as u64),
                provider_event_index: Some(event_index),
                raw_source_path: Some(base_draft.raw_source_path.clone()),
                path: path.to_owned(),
                change_kind: Some(change_kind),
                old_path: before_path
                    .filter(|before| after_path.is_some_and(|after| after != *before))
                    .map(str::to_owned),
                line_count_delta: None,
                confidence: Confidence::Explicit,
                occurred_at,
                source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT.to_owned(),
                metadata: json!({
                    "source": "junie_file_change",
                    "step_order": step.order,
                    "change_index": change_index,
                }),
            },
        ));
    }
}

pub(crate) fn junie_file_content_text(value: Option<&Value>) -> Option<String> {
    let value = value?;
    value
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| value.as_str())
        .map(str::to_owned)
}
