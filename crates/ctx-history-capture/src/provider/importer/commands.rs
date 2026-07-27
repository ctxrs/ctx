use ctx_history_core::{
    compact_result_payload, CaptureProvider, EventType, Fidelity, Run, RunStatus, RunType,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::provider::tool_input;
use crate::{CaptureError, Result};

use super::ids::{provider_run_uuid, provider_source_run_uuid, provider_sync_metadata, timestamps};

/// Removes provider result bodies before the canonical Store write.
///
/// Typed correlation and outcome metadata survive. Sparse failures and
/// timeouts may also retain one bounded preview; successful and unknown
/// outputs never do.
pub(crate) fn compact_provider_result_payload(event_type: EventType, payload: &Value) -> Value {
    if !matches!(event_type, EventType::ToolOutput | EventType::CommandOutput) {
        return payload.clone();
    }
    let mut compact = compact_result_payload(payload);
    let retained_failure = compact
        .get("result_outcome")
        .and_then(Value::as_str)
        .is_some_and(|outcome| outcome == "failure")
        || compact
            .get("timed_out")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || compact
            .get("exit_code")
            .and_then(Value::as_i64)
            .is_some_and(|exit_code| exit_code != 0);
    if retained_failure {
        let preview = payload
            .get("output_preview")
            .or_else(|| {
                payload
                    .get("body")
                    .and_then(|body| body.get("output_preview"))
            })
            .and_then(Value::as_str)
            .filter(|preview| !preview.trim().is_empty())
            .map(|preview| {
                preview
                    .chars()
                    .take(crate::PROVIDER_MAX_PREVIEW_CHARS)
                    .collect::<String>()
            });
        if let (Some(compact), Some(preview)) = (compact.as_object_mut(), preview) {
            compact.insert("output_preview".to_owned(), Value::String(preview));
        }
    }
    compact
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn provider_command_run(
    provider: CaptureProvider,
    provider_session_id: &str,
    session_id: Uuid,
    source_id: Uuid,
    run_source_id: Option<Uuid>,
    history_record_id: Option<Uuid>,
    event_type: EventType,
    occurred_at: chrono::DateTime<chrono::Utc>,
    fidelity: Fidelity,
    provider_event_index: u64,
    payload: &Value,
    event_hash: &str,
) -> Result<Option<Run>> {
    if event_type != EventType::CommandOutput {
        return Ok(None);
    }
    let arguments_preview = payload.get("arguments_preview");
    let command_preview = payload
        .get("command")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| arguments_preview.and_then(tool_input::command));
    let cwd = payload
        .get("workdir")
        .or_else(|| payload.get("cwd"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| arguments_preview.and_then(tool_input::working_directory));
    let call_id = payload.get("call_id").and_then(Value::as_str);
    let key = call_id.unwrap_or(event_hash);
    let started_at = provider_event_started_at(event_type, occurred_at, payload)?;
    let ended_at = Some(occurred_at);
    Ok(Some(Run {
        id: run_source_id
            .map(|source_id| provider_source_run_uuid(source_id, key))
            .unwrap_or_else(|| provider_run_uuid(provider, provider_session_id, key)),
        history_record_id,
        session_id: Some(session_id),
        run_type: RunType::Command,
        status: provider_command_run_status(payload),
        started_at,
        ended_at,
        exit_code: payload
            .get("exit_code")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok()),
        cwd,
        command_preview,
        input_blob_id: None,
        output_blob_id: None,
        timestamps: timestamps(occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            fidelity,
            json!({
                "provider_session_id": provider_session_id,
                "provider_event_index": provider_event_index,
                "provider_event_hash": event_hash,
                "call_id": call_id,
                "source": "provider_command_output",
            }),
        ),
    }))
}

fn provider_event_started_at(
    event_type: EventType,
    occurred_at: chrono::DateTime<chrono::Utc>,
    payload: &Value,
) -> Result<chrono::DateTime<chrono::Utc>> {
    if event_type != EventType::CommandOutput {
        return Ok(occurred_at);
    }
    Ok(match provider_command_duration_ms(payload)? {
        Some(duration) => {
            let duration_value = duration;
            let duration = chrono::Duration::try_milliseconds(duration_value).ok_or_else(|| {
                CaptureError::InvalidPayload(format!(
                    "duration_ms is not representable as milliseconds: {duration_value}"
                ))
            })?;
            occurred_at.checked_sub_signed(duration).ok_or_else(|| {
                CaptureError::InvalidPayload(format!(
                    "duration_ms moves command start before representable time: {duration_value}"
                ))
            })?
        }
        None => occurred_at,
    })
}

pub(crate) fn provider_command_duration_ms(payload: &Value) -> Result<Option<i64>> {
    let Some(value) = payload.get("duration_ms") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let duration = value
        .as_i64()
        .ok_or_else(|| CaptureError::InvalidPayload("duration_ms must be an integer".to_owned()))?;
    if duration < 0 {
        return Err(CaptureError::InvalidPayload(format!(
            "duration_ms must be nonnegative, got {duration}"
        )));
    }
    Ok(Some(duration))
}

pub(crate) fn provider_command_run_status(payload: &Value) -> RunStatus {
    if payload
        .get("timed_out")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return RunStatus::Cancelled;
    }
    match payload.get("exit_code").and_then(Value::as_i64) {
        Some(0) => RunStatus::Succeeded,
        Some(_) => RunStatus::Failed,
        None => {
            let outcome = payload
                .get("result_outcome")
                .or_else(|| payload.get("outcome"))
                .or_else(|| payload.get("status"))
                .and_then(Value::as_str)
                .map(str::trim)
                .map(str::to_ascii_lowercase);
            match outcome.as_deref() {
                Some("timeout" | "timed_out" | "timedout" | "cancelled" | "canceled") => {
                    RunStatus::Cancelled
                }
                Some("failure" | "failed" | "error" | "errored") => RunStatus::Failed,
                Some("success" | "succeeded" | "complete" | "completed" | "ok" | "passed") => {
                    RunStatus::Succeeded
                }
                _ => RunStatus::Partial,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn sqlite_boundary_drops_malformed_and_oversized_result_fields() {
        let malformed = json!({
            "tool": "x".repeat(257),
            "call_id": "x".repeat(257),
            "exit_code": i64::from(i32::MAX) + 1,
            "duration_ms": "42",
            "timed_out": 0,
            "output_bytes": -1,
            "result_outcome": ["success"],
            "result_evidence": (0..33)
                .map(|index| json!({"kind": "call_id", "value": format!("call-{index}")}))
                .collect::<Vec<_>>(),
            "result_content_ref": {"sha256": "not-a-digest", "byte_len": 1},
            "text": "raw result body",
            "output_preview": "raw result preview"
        });

        assert_eq!(
            compact_provider_result_payload(EventType::CommandOutput, &malformed),
            json!({})
        );
        assert_eq!(
            compact_provider_result_payload(EventType::Message, &malformed),
            malformed
        );
    }

    #[test]
    fn explicit_status_classifies_runs_without_inventing_exit_codes() {
        assert_eq!(
            provider_command_run_status(&json!({"status": "FAILED"})),
            RunStatus::Failed
        );
        assert_eq!(
            provider_command_run_status(&json!({"result_outcome": "timeout"})),
            RunStatus::Cancelled
        );
        assert_eq!(
            provider_command_run_status(&json!({"outcome": "completed"})),
            RunStatus::Succeeded
        );
        assert_eq!(
            provider_command_run_status(&json!({"status": "running"})),
            RunStatus::Partial
        );
    }
}
