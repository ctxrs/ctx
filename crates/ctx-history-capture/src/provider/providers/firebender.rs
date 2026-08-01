use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde_json::Value;

use crate::{
    provider::normalization::{provider_role, provider_timestamp_value},
    CaptureError, Result,
};

mod message_text;
pub(crate) mod native_path;

pub(crate) use message_text::{firebender_message_text, firebender_result_content};

pub(crate) fn firebender_chat_history_db_path(path: &Path) -> Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                return Err(CaptureError::InvalidProviderTranscriptPath {
                    path: path.to_path_buf(),
                    reason: "symlinked provider transcript roots are rejected",
                });
            }
            if file_type.is_file() {
                return Ok(path.to_path_buf());
            }
            if file_type.is_dir() {
                return Ok(path
                    .join(".idea")
                    .join("firebender")
                    .join("chat_history.db"));
            }
            Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Firebender import path must be chat_history.db or a project root",
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if path.file_name().and_then(|name| name.to_str()) == Some("chat_history.db") {
                Ok(path.to_path_buf())
            } else {
                Ok(path
                    .join(".idea")
                    .join("firebender")
                    .join("chat_history.db"))
            }
        }
        Err(error) => Err(CaptureError::Io(error)),
    }
}

pub(crate) fn firebender_message_time(message: &Value, fallback: DateTime<Utc>) -> DateTime<Utc> {
    provider_timestamp_value(
        message
            .get("timestamp")
            .or_else(|| message.get("created_at"))
            .or_else(|| message.get("updated_at")),
        fallback,
    )
}

#[derive(Default)]
pub(super) struct FirebenderOutputEvidence {
    pub(super) success: bool,
    pub(super) failure: bool,
    pub(super) timeout: bool,
    pub(super) exit_code: Option<i32>,
    pub(super) duration_ms: Option<u64>,
}

pub(super) fn firebender_output_evidence(message: &Value) -> FirebenderOutputEvidence {
    let mut evidence = FirebenderOutputEvidence::default();
    let mut remaining = 4_096;
    collect_output_evidence(message, &mut remaining, &mut evidence);
    evidence
}

fn collect_output_evidence(
    value: &Value,
    remaining: &mut usize,
    evidence: &mut FirebenderOutputEvidence,
) {
    if *remaining == 0 {
        return;
    }
    *remaining -= 1;
    match value {
        Value::Array(values) => {
            for value in values {
                collect_output_evidence(value, remaining, evidence);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                let normalized = key
                    .chars()
                    .filter(|ch| ch.is_ascii_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect::<String>();
                match normalized.as_str() {
                    "exitcode" => {
                        if let Some(code) = value.as_i64().and_then(|code| i32::try_from(code).ok())
                        {
                            evidence.exit_code = Some(code);
                            evidence.success |= code == 0;
                            evidence.failure |= code != 0;
                        }
                    }
                    "durationms" => evidence.duration_ms = value.as_u64(),
                    "success" | "ok" => {
                        if let Some(success) = value.as_bool() {
                            evidence.success |= success;
                            evidence.failure |= !success;
                        }
                    }
                    "iserror" => evidence.failure |= value.as_bool().unwrap_or(false),
                    "timedout" | "timeout" => {
                        evidence.timeout |= value.as_bool().unwrap_or(false);
                    }
                    "status" | "state" | "outcome" => {
                        if let Some(status) = value.as_str() {
                            classify_status(status, evidence);
                        }
                    }
                    "error" if error_value_is_present(value) => evidence.failure = true,
                    _ => {}
                }
                collect_output_evidence(value, remaining, evidence);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn classify_status(status: &str, evidence: &mut FirebenderOutputEvidence) {
    match status.trim().to_ascii_lowercase().as_str() {
        "success" | "succeeded" | "complete" | "completed" | "ok" | "passed" => {
            evidence.success = true;
        }
        "timeout" | "timed_out" | "timedout" => evidence.timeout = true,
        "failed" | "failure" | "error" | "errored" | "cancelled" | "canceled" => {
            evidence.failure = true;
        }
        _ => {}
    }
}

fn error_value_is_present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.trim().is_empty(),
        Value::Number(value) => value.as_i64().is_some_and(|value| value != 0),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
    }
}

pub(super) struct FirebenderEventParts {
    event_type: EventType,
    role: Option<EventRole>,
    occurred_at: DateTime<Utc>,
    text: String,
}

pub(super) fn firebender_event_parts(
    message: &Value,
    occurred_at: DateTime<Utc>,
) -> FirebenderEventParts {
    let role = message.get("role").and_then(Value::as_str);
    let tool_calls = message
        .get("tool_calls")
        .or_else(|| message.get("toolCalls"));
    let event_type = if role == Some("tool") {
        EventType::ToolOutput
    } else if tool_calls.is_some_and(|value| {
        value
            .as_array()
            .map(|items| !items.is_empty())
            .unwrap_or(true)
    }) {
        EventType::ToolCall
    } else {
        EventType::Message
    };
    FirebenderEventParts {
        event_type,
        role: Some(provider_role(role)),
        occurred_at,
        text: firebender_message_text(message)
            .unwrap_or_else(|| format!("Firebender {}", role.unwrap_or("message"))),
    }
}
