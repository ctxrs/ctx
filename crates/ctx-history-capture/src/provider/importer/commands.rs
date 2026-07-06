use ctx_history_core::{
    CaptureProvider, EventType, ProviderEventEnvelope, Run, RunStatus, RunType,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{CaptureError, Result};

use super::ids::{provider_run_uuid, provider_source_run_uuid, provider_sync_metadata, timestamps};

pub(crate) struct ProviderCommandRunInput<'a> {
    pub(crate) provider: CaptureProvider,
    pub(crate) provider_session_id: &'a str,
    pub(crate) session_id: Uuid,
    pub(crate) source_id: Uuid,
    pub(crate) run_source_id: Option<Uuid>,
    pub(crate) history_record_id: Option<Uuid>,
    pub(crate) event: &'a ProviderEventEnvelope,
    pub(crate) payload: &'a Value,
    pub(crate) event_hash: &'a str,
}

pub(crate) fn provider_command_run_from_event(
    input: ProviderCommandRunInput<'_>,
) -> Result<Option<Run>> {
    let ProviderCommandRunInput {
        provider,
        provider_session_id,
        session_id,
        source_id,
        run_source_id,
        history_record_id,
        event,
        payload,
        event_hash,
    } = input;
    if event.event_type != EventType::CommandOutput {
        return Ok(None);
    }
    let command_preview = payload
        .get("command")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    let call_id = payload.get("call_id").and_then(Value::as_str);
    let key = call_id.unwrap_or(event_hash);
    let duration_ms = provider_command_duration_ms(payload)?;
    let ended_at = Some(event.occurred_at);
    let started_at = match duration_ms {
        Some(duration) => {
            let duration_value = duration;
            let duration = chrono::Duration::try_milliseconds(duration_value).ok_or_else(|| {
                CaptureError::InvalidPayload(format!(
                    "duration_ms is not representable as milliseconds: {duration_value}"
                ))
            })?;
            event
                .occurred_at
                .checked_sub_signed(duration)
                .ok_or_else(|| {
                    CaptureError::InvalidPayload(format!(
                        "duration_ms moves command start before representable time: {}",
                        duration_value
                    ))
                })?
        }
        None => event.occurred_at,
    };
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
        cwd: None,
        command_preview,
        input_blob_id: None,
        output_blob_id: None,
        timestamps: timestamps(event.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            event.fidelity,
            json!({
                "provider_session_id": provider_session_id,
                "provider_event_index": event.provider_event_index,
                "provider_event_hash": event_hash,
                "call_id": call_id,
                "source": "provider_command_output",
            }),
        ),
    }))
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
        None => RunStatus::Partial,
    }
}
