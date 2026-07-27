use chrono::{DateTime, Utc};
use ctx_history_core::{
    CaptureProvider, Event, EventRole, EventType, Fidelity, ProviderArtifactDescriptor,
    ProviderSourceTrust, Run, RunStatus, RunType,
};
use ctx_history_store::{ProviderEventHashAuthority, Store};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::common::time::parse_optional_rfc3339_field;
use crate::complete_content::{VerifiedContentLocatorsV1, VERIFIED_CONTENT_LOCATORS_METADATA_KEY};
use crate::provider::importer::{
    compact_provider_result_payload, provider_sync_metadata, timestamps,
    ProviderEventImportIdentity,
};
use crate::provider::normalization::capped_text;
use crate::{
    stable_capture_uuid, CaptureError, Result, CODEX_SESSION_SOURCE_FORMAT, PROVIDER_MAX_TEXT_CHARS,
};

mod retention;
mod tool;

use retention::codex_event_role;
pub(crate) use retention::{
    codex_command_preview, codex_content_text, codex_is_command_tool, codex_local_preview,
    codex_tool_arguments_preview, codex_tool_name, CodexExitCodeParser, CodexWallTimeParser,
};
#[cfg(test)]
pub(crate) use tool::{codex_output_text, codex_tool_output_event, codex_tool_output_outcome};
pub(crate) use tool::{codex_result_content, codex_sparse_tool_output_event, CodexToolCallContext};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct CodexNativeEvent {
    pub(crate) provider_event_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) provider_event_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cursor: Option<String>,
    #[serde(default)]
    pub(crate) event_type: EventType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) role: Option<EventRole>,
    pub(crate) occurred_at: DateTime<Utc>,
    #[serde(default)]
    pub(crate) fidelity: Fidelity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) idempotency_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) artifacts: Vec<ProviderArtifactDescriptor>,
    #[serde(default = "crate::common::json::default_metadata")]
    pub(crate) payload: Value,
    #[serde(default = "crate::common::json::default_metadata")]
    pub(crate) metadata: Value,
}

pub(crate) fn codex_session_line_timestamp(
    value: &Value,
    fallback: DateTime<Utc>,
) -> Result<DateTime<Utc>> {
    Ok(parse_optional_rfc3339_field(value, "timestamp")?.unwrap_or(fallback))
}

pub(crate) fn codex_message_body(payload: &Value) -> Option<(EventRole, Value)> {
    let role_text = payload
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if !matches!(role_text, "user" | "assistant" | "developer" | "system") {
        return None;
    }
    let text = payload.get("content").and_then(codex_content_text)?;
    let (text, truncated) = capped_text(&text, PROVIDER_MAX_TEXT_CHARS);
    Some((
        codex_event_role(role_text),
        json!({
            "item_type": "message",
            "message_role": role_text,
            "phase": payload.get("phase").and_then(Value::as_str),
            "text": text,
            "truncated": truncated,
        }),
    ))
}

/// Rebuilds the exact normalized message payload used by complete-content
/// verification. NativePath Core construction shares `codex_message_body`.
pub(crate) fn codex_message_event(
    payload: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
) -> Option<CodexNativeEvent> {
    let (role, body) = codex_message_body(payload)?;
    let role_text = body.get("message_role").and_then(Value::as_str)?.to_owned();
    Some(codex_provider_event(
        line_number,
        occurred_at,
        EventType::Message,
        Some(role),
        body,
        json!({
            "source": "codex_session",
            "source_format": CODEX_SESSION_SOURCE_FORMAT,
            "import_scope": "fast_transcript_index",
            "line": line_number,
            "item_type": "message",
            "message_role": role_text,
        }),
    ))
}
pub(crate) fn codex_provider_event(
    line_number: usize,
    occurred_at: DateTime<Utc>,
    event_type: EventType,
    role: Option<EventRole>,
    payload: Value,
    metadata: Value,
) -> CodexNativeEvent {
    CodexNativeEvent {
        provider_event_index: (line_number - 1) as u64,
        provider_event_hash: None,
        cursor: Some(format!("line:{line_number}")),
        event_type,
        role,
        occurred_at,
        fidelity: Fidelity::Imported,
        idempotency_key: Some(format!("provider-event:codex-session:{line_number}")),
        artifacts: Vec::new(),
        payload,
        metadata,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn codex_canonical_event(
    provider_session_id: &str,
    source_format: &str,
    source_trust: ProviderSourceTrust,
    imported_at: DateTime<Utc>,
    history_record_id: Option<Uuid>,
    source_id: Uuid,
    session_id: Uuid,
    line_number: usize,
    native: &CodexNativeEvent,
    event_hash: &str,
    event_hash_authority: ProviderEventHashAuthority,
    identity: &ProviderEventImportIdentity,
) -> Result<(Event, Option<Run>)> {
    let payload = native.payload.clone();
    let mut provider_metadata = native.metadata.clone();
    let source_record_coordinates = take_source_record_coordinates(&mut provider_metadata)?;
    let verified_content_locators = take_verified_content_locators(&mut provider_metadata)?;
    let command_run = codex_command_run(
        provider_session_id,
        session_id,
        source_id,
        identity.run_source_id,
        history_record_id,
        native,
        &payload,
        event_hash,
    )?;
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or_else(|| identity.dedupe_key.clone());
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": native.provider_event_index,
        "provider_event_hash": event_hash,
        "provider_event_hash_authority": event_hash_authority.as_str(),
        "cursor": native.cursor,
        "source_format": source_format,
        "source_trust": source_trust,
        "fixture_line": line_number,
        "imported_at": imported_at,
        "event_idempotency_key": native.idempotency_key,
        "source_record_ordinal": source_record_coordinates
            .as_ref()
            .map(|coordinates| coordinates.0),
        "source_record_subrecord_index": source_record_coordinates
            .as_ref()
            .map(|coordinates| coordinates.1),
        "metadata": provider_metadata,
    });
    if let Some(locators) = verified_content_locators {
        if let Some(metadata) = sync_metadata.as_object_mut() {
            metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
        }
    }
    let event = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id,
        session_id: Some(session_id),
        run_id: command_run.as_ref().map(|run| run.id),
        event_type: native.event_type,
        role: native.role,
        occurred_at: native.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::Codex.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": native.provider_event_index,
            "provider_event_hash": event_hash,
            "cursor": native.cursor,
            "artifacts": native.artifacts,
            "body": compact_provider_result_payload(native.event_type, &payload),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(native.fidelity, sync_metadata),
    };
    Ok((event, command_run))
}

#[allow(clippy::too_many_arguments)]
fn codex_command_run(
    provider_session_id: &str,
    session_id: Uuid,
    source_id: Uuid,
    run_source_id: Option<Uuid>,
    history_record_id: Option<Uuid>,
    native: &CodexNativeEvent,
    payload: &Value,
    event_hash: &str,
) -> Result<Option<Run>> {
    if native.event_type != EventType::CommandOutput {
        return Ok(None);
    }
    let arguments_preview = payload.get("arguments_preview");
    let command_preview = payload
        .get("command")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| arguments_preview.and_then(crate::provider::tool_input::command));
    let cwd = payload
        .get("workdir")
        .or_else(|| payload.get("cwd"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| arguments_preview.and_then(crate::provider::tool_input::working_directory));
    let call_id = payload.get("call_id").and_then(Value::as_str);
    let key = call_id.unwrap_or(event_hash);
    let started_at = command_started_at(native.occurred_at, payload)?;
    let id = run_source_id.map_or_else(
        || {
            stable_capture_uuid(
                &format!(
                    "provider:{}:{provider_session_id}:run:{key}",
                    CaptureProvider::Codex.as_str()
                ),
                "run",
            )
        },
        |run_source_id| {
            stable_capture_uuid(&format!("provider-source:{run_source_id}:run:{key}"), "run")
        },
    );
    Ok(Some(Run {
        id,
        history_record_id,
        session_id: Some(session_id),
        run_type: RunType::Command,
        status: command_run_status(payload),
        started_at,
        ended_at: Some(native.occurred_at),
        exit_code: payload
            .get("exit_code")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok()),
        cwd,
        command_preview,
        input_blob_id: None,
        output_blob_id: None,
        timestamps: timestamps(native.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            native.fidelity,
            json!({
                "provider_session_id": provider_session_id,
                "provider_event_index": native.provider_event_index,
                "provider_event_hash": event_hash,
                "call_id": call_id,
                "source": "provider_command_output",
            }),
        ),
    }))
}

fn command_started_at(occurred_at: DateTime<Utc>, payload: &Value) -> Result<DateTime<Utc>> {
    let Some(value) = payload.get("duration_ms") else {
        return Ok(occurred_at);
    };
    if value.is_null() {
        return Ok(occurred_at);
    }
    let duration = value
        .as_i64()
        .ok_or_else(|| CaptureError::InvalidPayload("duration_ms must be an integer".to_owned()))?;
    if duration < 0 {
        return Err(CaptureError::InvalidPayload(format!(
            "duration_ms must be nonnegative, got {duration}"
        )));
    }
    let span = chrono::Duration::try_milliseconds(duration).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "duration_ms is not representable as milliseconds: {duration}"
        ))
    })?;
    occurred_at.checked_sub_signed(span).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "duration_ms moves command start before representable time: {duration}"
        ))
    })
}

fn command_run_status(payload: &Value) -> RunStatus {
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

fn take_verified_content_locators(metadata: &mut Value) -> Result<Option<Value>> {
    let Some(object) = metadata.as_object_mut() else {
        return Ok(None);
    };
    let Some(value) = object.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY) else {
        return Ok(None);
    };
    let locators = VerifiedContentLocatorsV1::from_metadata_value(&value).ok_or_else(|| {
        CaptureError::InvalidPayload("verified content locator annotation is malformed".to_owned())
    })?;
    Ok(Some(locators.to_metadata_value()))
}

fn take_source_record_coordinates(metadata: &mut Value) -> Result<Option<(u64, u32)>> {
    let Some(object) = metadata.as_object_mut() else {
        return Ok(None);
    };
    let ordinal = object.remove("source_record_ordinal");
    let subrecord = object.remove("source_record_subrecord_index");
    if ordinal.is_none() && subrecord.is_none() {
        return Ok(None);
    }
    let ordinal = ordinal.and_then(|value| value.as_u64()).ok_or_else(|| {
        CaptureError::InvalidPayload("source record ordinal annotation is malformed".to_owned())
    })?;
    let subrecord = subrecord
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "source record subrecord annotation is malformed".to_owned(),
            )
        })?;
    Ok(Some((ordinal, subrecord)))
}
