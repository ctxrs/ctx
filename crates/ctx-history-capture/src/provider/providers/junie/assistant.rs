use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde_json::Value;

#[derive(Debug, Clone)]
pub(super) struct JunieStepAgg {
    pub(super) order: usize,
    pub(super) label: Option<String>,
    pub(super) command: Option<String>,
    pub(super) files: Option<Value>,
    pub(super) changes: Vec<Value>,
    pub(super) details: Option<String>,
    pub(super) status: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct JunieUsage {
    pub(super) input_tokens: i64,
    pub(super) output_tokens: i64,
    pub(super) cache_read_tokens: i64,
    pub(super) cache_write_tokens: i64,
    pub(super) model: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct JunieAssistantBuffer {
    pub(super) open: bool,
    pub(super) turn_ts: Option<DateTime<Utc>>,
    pub(super) steps: BTreeMap<String, JunieStepAgg>,
    pub(super) step_ids_in_order: Vec<String>,
    pub(super) results: BTreeMap<String, String>,
    pub(super) usage: JunieUsage,
    pub(super) retained_source_bytes: usize,
    pub(super) source_binding: crate::complete_content::jsonl::JunieRecordSetBinding,
}

pub(crate) fn junie_merge_buffered_agent_event(
    buffer: &mut JunieAssistantBuffer,
    agent_event: &Value,
    source_line_number: u64,
    occurred_at: DateTime<Utc>,
) -> bool {
    match agent_event
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("")
    {
        "LlmResponseMetadataEvent" => {
            junie_ensure_assistant(buffer, occurred_at);
            junie_merge_usage(&mut buffer.usage, agent_event);
            true
        }
        "ResultBlockUpdatedEvent" => {
            let Some(text) = agent_event
                .get("result")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
            else {
                return false;
            };
            let step_id = agent_event
                .get("stepId")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("result-{source_line_number}"));
            junie_ensure_assistant(buffer, occurred_at);
            buffer.results.insert(step_id, text.to_owned());
            true
        }
        "AgentFailureEvent" => {
            let Some(message) = agent_event
                .get("message")
                .and_then(Value::as_str)
                .filter(|message| !message.trim().is_empty())
            else {
                return false;
            };
            let step_id = agent_event
                .get("errorCode")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(|value| format!("failure-{value}-{source_line_number}"))
                .unwrap_or_else(|| format!("failure-{source_line_number}"));
            junie_ensure_assistant(buffer, occurred_at);
            buffer
                .results
                .insert(step_id, format!("Junie failed: {message}"));
            true
        }
        "ToolBlockUpdatedEvent"
        | "TerminalBlockUpdatedEvent"
        | "ViewFilesBlockUpdatedEvent"
        | "FileChangesBlockUpdatedEvent" => {
            junie_merge_step(buffer, agent_event, occurred_at);
            true
        }
        _ => false,
    }
}

pub(crate) fn junie_buffer_result_text(buffer: &JunieAssistantBuffer) -> String {
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
    final_text
}

pub(crate) fn junie_buffer_step_output(
    buffer: &JunieAssistantBuffer,
    step_order: u32,
) -> Option<&str> {
    let step_id = buffer
        .step_ids_in_order
        .get(usize::try_from(step_order).ok()?)?;
    let step = buffer.steps.get(step_id)?;
    if !step.changes.is_empty() {
        return None;
    }
    step.details
        .as_deref()
        .filter(|details| !details.trim().is_empty())
}

pub(super) fn junie_ensure_assistant(
    buffer: &mut JunieAssistantBuffer,
    occurred_at: DateTime<Utc>,
) {
    if !buffer.open {
        buffer.open = true;
        buffer.turn_ts = Some(occurred_at);
    }
}

pub(super) fn junie_merge_usage(usage: &mut JunieUsage, agent_event: &Value) {
    let Some(items) = agent_event.get("modelUsage").and_then(Value::as_array) else {
        return;
    };
    for item in items {
        usage.input_tokens = usage
            .input_tokens
            .saturating_add(junie_i64_field(item, "inputTokens"));
        usage.output_tokens = usage
            .output_tokens
            .saturating_add(junie_i64_field(item, "outputTokens"));
        usage.cache_read_tokens = usage
            .cache_read_tokens
            .saturating_add(junie_i64_field(item, "cacheInputTokens"));
        usage.cache_write_tokens = usage
            .cache_write_tokens
            .saturating_add(junie_i64_field(item, "cacheCreateTokens"));
        if let Some(model) = item.get("model").and_then(Value::as_str) {
            if !model.trim().is_empty() {
                usage.model = Some(model.to_owned());
            }
        }
    }
}

fn junie_i64_field(value: &Value, field: &str) -> i64 {
    value
        .get(field)
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        })
        .unwrap_or(0)
}

pub(super) fn junie_merge_step(
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
    if !buffer.steps.contains_key(step_id) {
        buffer.step_ids_in_order.push(step_id.to_owned());
    }
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
