use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde_json::Value;

#[derive(Debug, Clone)]
pub(super) struct JunieStepAgg {
    pub(super) order: usize,
    pub(super) provider_step_id: String,
    pub(super) label: Option<String>,
    pub(super) command: Option<String>,
    pub(super) files: Option<Value>,
    pub(super) changes: Vec<Value>,
    pub(super) details: Option<String>,
    pub(super) status: Option<String>,
    pub(super) exit_code: Option<i32>,
    pub(super) duration_ms: Option<u64>,
    pub(super) timed_out: bool,
    pub(super) invalid_output_shape: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JunieOutputOutcome {
    Success,
    Failure,
    Timeout,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JunieStepOutputProjection<'a> {
    pub(super) details: &'a str,
    pub(super) call_id: String,
    pub(super) tool_name: &'static str,
    pub(super) command: Option<&'a str>,
    pub(super) outcome: JunieOutputOutcome,
    pub(super) exit_code: Option<i32>,
    pub(super) duration_ms: Option<u64>,
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
            buffer.results.insert(step_id, message.to_owned());
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

pub(super) fn junie_step_output_projection(
    step: &JunieStepAgg,
) -> Option<JunieStepOutputProjection<'_>> {
    if !step.changes.is_empty() {
        return None;
    }
    if step.invalid_output_shape {
        return None;
    }
    let details = step
        .details
        .as_deref()
        .filter(|details| !details.trim().is_empty())?;
    let status = step
        .status
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase);
    let outcome = if step.timed_out
        || status
            .as_deref()
            .is_some_and(|status| matches!(status, "timeout" | "timed_out" | "timedout"))
    {
        JunieOutputOutcome::Timeout
    } else if step.exit_code.is_some_and(|code| code != 0)
        || status.as_deref().is_some_and(|status| {
            matches!(
                status,
                "failed" | "failure" | "error" | "errored" | "cancelled" | "canceled"
            )
        })
    {
        JunieOutputOutcome::Failure
    } else if step.exit_code == Some(0)
        || status.as_deref().is_some_and(|status| {
            matches!(
                status,
                "success" | "succeeded" | "complete" | "completed" | "ok" | "passed"
            )
        })
    {
        JunieOutputOutcome::Success
    } else {
        JunieOutputOutcome::Unknown
    };
    Some(JunieStepOutputProjection {
        details,
        // Associate output with the first-seen step order rather than a mutable provider update ID.
        call_id: format!("step:{}", step.order),
        tool_name: if step.command.is_some() {
            "Bash"
        } else if step.files.is_some() {
            "view"
        } else {
            "tool"
        },
        command: step.command.as_deref(),
        outcome,
        exit_code: step.exit_code,
        duration_ms: step.duration_ms,
    })
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
            provider_step_id: step_id.to_owned(),
            label: None,
            command: None,
            files: None,
            changes: Vec::new(),
            details: None,
            status: None,
            exit_code: None,
            duration_ms: None,
            timed_out: false,
            invalid_output_shape: false,
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
    if let Some(details) = agent_event.get("details") {
        match details {
            Value::String(details) if !details.trim().is_empty() => {
                step.details = Some(details.to_owned());
            }
            Value::Null | Value::String(_) => {}
            _ => step.invalid_output_shape = true,
        }
    }
    if let Some(status) = agent_event.get("status") {
        match status {
            Value::String(status) if !status.trim().is_empty() => {
                step.status = Some(status.to_owned());
            }
            Value::Null | Value::String(_) => {}
            _ => step.invalid_output_shape = true,
        }
    }
    let exit_codes = ["exitCode", "exit_code"]
        .iter()
        .filter_map(|key| agent_event.get(*key))
        .filter(|value| !value.is_null())
        .map(|value| value.as_i64().and_then(|code| i32::try_from(code).ok()))
        .collect::<Vec<_>>();
    if exit_codes.iter().any(Option::is_none)
        || exit_codes
            .iter()
            .flatten()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            > 1
    {
        step.invalid_output_shape = true;
    } else if let Some(code) = exit_codes.into_iter().flatten().next() {
        step.exit_code = Some(code);
    }
    let durations = ["durationMs", "duration_ms"]
        .iter()
        .filter_map(|key| agent_event.get(*key))
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_u64()
                .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
        })
        .collect::<Vec<_>>();
    if durations.iter().any(Option::is_none)
        || durations
            .iter()
            .flatten()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            > 1
    {
        step.invalid_output_shape = true;
    } else if let Some(duration) = durations.into_iter().flatten().next() {
        step.duration_ms = Some(duration);
    }
    step.timed_out |= ["timedOut", "timed_out", "timeout"].iter().any(|key| {
        agent_event
            .get(*key)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    });
}
