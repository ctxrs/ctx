use chrono::{DateTime, NaiveDateTime, Utc};
use ctx_history_core::{CaptureProvider, ContentRef, EventRole, EventType};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::common::time::parse_rfc3339_utc;
use crate::complete_content::{
    attach_verified_content_locator, verified_content_profile, CompleteContentBodyDigest,
    CompleteContentSourceFamily, VerifiedContentLocatorV1, VerifiedContentRole,
};
use crate::native_source::NativeSqliteValue;
use crate::provider::normalization::{
    provider_capped_json, provider_json_text, provider_local_preview, provider_policy_body,
    provider_policy_event_text, provider_result_identifier_evidence,
    provider_result_outcome_evidence,
};
use crate::{
    CaptureError, OutputAssociations, OutputNativeCoordinate, OutputObservationKind, OutputOutcome,
    OutputOutcomeMetadata, OutputSourceLocator, ProOutputObservation, ProviderAdapterContext,
    Result, PROVIDER_MAX_PREVIEW_CHARS, SHELLEY_SQLITE_SOURCE_FORMAT,
};

use super::relationships::{
    shelley_event_index, shelley_event_role, shelley_event_type, shelley_message_body,
    shelley_message_complete_text, shelley_message_text, shelley_verified_record_values,
    ShelleyConversationRow, ShelleyMessageRow,
};

pub(super) fn shelley_core_event(
    message: &ShelleyMessageRow,
    conversation: &ShelleyConversationRow,
    context: &ProviderAdapterContext,
    parent_bearing: bool,
) -> Result<Option<ShelleyCoreEvent>> {
    let output_classification = shelley_output_classification(message);
    if output_classification
        .as_ref()
        .is_some_and(|classification| {
            !matches!(
                classification.outcome.outcome,
                OutputOutcome::Failure | OutputOutcome::Timeout
            )
        })
    {
        return Ok(None);
    }

    let started_at = shelley_timestamp(conversation.created_at.as_deref(), context.imported_at);
    let occurred_at = shelley_timestamp(message.created_at.as_deref(), started_at);
    let mut event = shelley_native_event(message, occurred_at);
    if let Some(classification) = output_classification.as_ref() {
        shelley_apply_failure_diagnostic(&mut event, message, classification)?;
    }
    let needs_locator = event.event_type == EventType::Message
        && event
            .payload
            .pointer("/text_retention/truncated")
            .and_then(Value::as_bool)
            == Some(true);
    if needs_locator {
        let complete_text = shelley_message_complete_text(message)
            .unwrap_or_else(|| format!("Shelley {} message", message.entry_type));
        attach_shelley_core_content_locator(
            &mut event,
            message,
            conversation,
            parent_bearing,
            &complete_text,
        )?;
    }
    Ok(Some(event))
}

pub(super) struct ShelleyOutputClassification {
    pub(super) outcome: OutputOutcomeMetadata,
    call_id: Option<String>,
}

pub(super) fn shelley_output_classification(
    message: &ShelleyMessageRow,
) -> Option<ShelleyOutputClassification> {
    let body = shelley_message_body(message);
    if !matches!(
        shelley_event_type(message, &body),
        EventType::ToolOutput | EventType::CommandOutput
    ) {
        return None;
    }
    let mut evidence = ShelleyOutputEvidence::default();
    let mut remaining = 4_096;
    shelley_collect_result_evidence(&body, false, &mut remaining, &mut evidence);
    if message.entry_type == "tool" && !evidence.found_result {
        let mut remaining = 4_096;
        shelley_collect_output_fields(&body, &mut remaining, &mut evidence);
    }
    let outcome = if evidence.timeout {
        OutputOutcome::Timeout
    } else if evidence.failure {
        OutputOutcome::Failure
    } else if evidence.success {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    };
    Some(ShelleyOutputClassification {
        outcome: OutputOutcomeMetadata {
            outcome,
            exit_code: evidence.exit_code,
            duration_ms: evidence.duration_ms,
        },
        call_id: evidence.call_id,
    })
}

pub(super) fn shelley_output_observation(
    message: &ShelleyMessageRow,
    conversation: &ShelleyConversationRow,
    parent_bearing: bool,
    context: &ProviderAdapterContext,
    classification: &ShelleyOutputClassification,
) -> Result<ProOutputObservation> {
    let mut locator_payload = Vec::with_capacity(17);
    locator_payload.push(if parent_bearing { 1 } else { 2 });
    locator_payload.extend_from_slice(&(message.rowid as u64 ^ (1_u64 << 63)).to_be_bytes());
    locator_payload.extend_from_slice(&(conversation.rowid as u64 ^ (1_u64 << 63)).to_be_bytes());
    let started_at = shelley_timestamp(conversation.created_at.as_deref(), context.imported_at);
    let occurred_at = shelley_timestamp(message.created_at.as_deref(), started_at);
    Ok(ProOutputObservation {
        kind: OutputObservationKind::Tool,
        coordinate: OutputNativeCoordinate {
            unit_key: format!(
                "shelley:{}:message:{}:output",
                message.conversation_id, message.message_id
            ),
            native_sequence: shelley_event_index(message),
            native_record_id: Some(message.message_id.clone()),
            source_record_ordinal: None,
            source_record_subrecord_index: None,
            byte_start: None,
            byte_end_exclusive: None,
        },
        occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: message.conversation_id.clone(),
            root_session_id: conversation
                .parent_conversation_id
                .clone()
                .unwrap_or_else(|| message.conversation_id.clone()),
            parent_session_id: conversation.parent_conversation_id.clone(),
            provider_session_id: Some(message.conversation_id.clone()),
            agent_id: None,
            repository: None,
        },
        call_id: classification.call_id.clone(),
        command: None,
        outcome: classification.outcome.clone(),
        locator: OutputSourceLocator {
            version: 1,
            kind: "shelley-compound-message-row-v1".to_owned(),
            payload: locator_payload,
        },
        content: shelley_message_complete_text(message)
            .unwrap_or_default()
            .into_bytes(),
    })
}

fn shelley_apply_failure_diagnostic(
    event: &mut ShelleyCoreEvent,
    message: &ShelleyMessageRow,
    classification: &ShelleyOutputClassification,
) -> Result<()> {
    if !matches!(
        classification.outcome.outcome,
        OutputOutcome::Failure | OutputOutcome::Timeout
    ) {
        return Ok(());
    }
    let payload = event
        .payload
        .as_object_mut()
        .ok_or(CaptureError::SystemInvariant(
            "Shelley failure event payload must be an object",
        ))?;
    payload.insert("result_outcome".to_owned(), json!("failure"));
    payload.insert(
        "timed_out".to_owned(),
        json!(classification.outcome.outcome == OutputOutcome::Timeout),
    );
    if let Some(exit_code) = classification.outcome.exit_code {
        payload.insert("exit_code".to_owned(), json!(exit_code));
    }
    if let Some(duration_ms) = classification.outcome.duration_ms {
        payload.insert("duration_ms".to_owned(), json!(duration_ms));
    }
    if let Some(call_id) = classification.call_id.as_ref() {
        payload.insert("call_id".to_owned(), Value::String(call_id.clone()));
    }
    if let Some(content) = shelley_message_complete_text(message) {
        payload.insert("output_bytes".to_owned(), json!(content.len()));
        let (preview, _) = provider_local_preview(&content, PROVIDER_MAX_PREVIEW_CHARS);
        if !preview.trim().is_empty() {
            payload.insert("output_preview".to_owned(), Value::String(preview));
        }
    }
    Ok(())
}

#[derive(Default)]
struct ShelleyOutputEvidence {
    found_result: bool,
    success: bool,
    failure: bool,
    timeout: bool,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    call_id: Option<String>,
}

fn shelley_collect_result_evidence(
    value: &Value,
    inside_result: bool,
    remaining: &mut usize,
    evidence: &mut ShelleyOutputEvidence,
) {
    if *remaining == 0 {
        return;
    }
    *remaining -= 1;
    match value {
        Value::Array(values) => {
            for value in values {
                shelley_collect_result_evidence(value, inside_result, remaining, evidence);
            }
        }
        Value::Object(values) => {
            let is_result = shelley_result_content_type(value).is_some_and(|kind| {
                matches!(
                    kind.as_str(),
                    "tool_result" | "web_search_tool_result" | "web_search_result"
                )
            });
            let inside_result = inside_result || is_result;
            evidence.found_result |= is_result;
            if inside_result {
                shelley_collect_output_fields(value, remaining, evidence);
            } else {
                for value in values.values() {
                    shelley_collect_result_evidence(value, false, remaining, evidence);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn shelley_collect_output_fields(
    value: &Value,
    remaining: &mut usize,
    evidence: &mut ShelleyOutputEvidence,
) {
    if *remaining == 0 {
        return;
    }
    *remaining -= 1;
    match value {
        Value::Array(values) => {
            for value in values {
                shelley_collect_output_fields(value, remaining, evidence);
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
                    "callid" | "toolcallid" | "toolresultid" | "tooluseid"
                        if evidence.call_id.is_none() =>
                    {
                        evidence.call_id = value
                            .as_str()
                            .filter(|value| !value.is_empty())
                            .map(str::to_owned);
                    }
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
                            shelley_classify_status(status, evidence);
                        }
                    }
                    "error" if shelley_error_value_is_present(value) => evidence.failure = true,
                    _ => {}
                }
                shelley_collect_output_fields(value, remaining, evidence);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn shelley_result_content_type(value: &Value) -> Option<String> {
    let raw = value.get("Type")?;
    if let Some(text) = raw.as_str() {
        let normalized = text.trim().to_ascii_lowercase();
        return match normalized.as_str() {
            "contenttypetoolresult" => Some("tool_result".to_owned()),
            "contenttypewebsearchtoolresult" => Some("web_search_tool_result".to_owned()),
            "contenttypewebsearchresult" => Some("web_search_result".to_owned()),
            _ => Some(normalized),
        };
    }
    raw.as_i64().and_then(|kind| {
        match kind {
            6 => Some("tool_result"),
            8 => Some("web_search_tool_result"),
            9 => Some("web_search_result"),
            _ => None,
        }
        .map(str::to_owned)
    })
}

fn shelley_classify_status(status: &str, evidence: &mut ShelleyOutputEvidence) {
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

fn shelley_error_value_is_present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.trim().is_empty(),
        Value::Number(value) => value.as_i64().is_some_and(|value| value != 0),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
    }
}

/// Returns only the migration fields needed to verify a released
/// complete-content locator.
pub(crate) fn shelley_complete_event(
    message: &ShelleyMessageRow,
    occurred_at: DateTime<Utc>,
) -> (u64, String, String, EventType, Value) {
    let event = shelley_native_event(message, occurred_at);
    (
        event.provider_event_index,
        event.provider_event_hash,
        event.cursor,
        event.event_type,
        event.payload,
    )
}

#[derive(Debug)]
pub(super) struct ShelleyCoreEvent {
    pub(super) provider_event_index: u64,
    pub(super) provider_event_hash: String,
    pub(super) cursor: String,
    pub(super) event_type: EventType,
    pub(super) role: Option<EventRole>,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) payload: Value,
    pub(super) metadata: Value,
}

fn shelley_native_event(
    message: &ShelleyMessageRow,
    occurred_at: DateTime<Utc>,
) -> ShelleyCoreEvent {
    let body = shelley_message_body(message);
    let text = shelley_message_text(message, &body)
        .unwrap_or_else(|| format!("Shelley {} message", message.entry_type));
    let event_type = shelley_event_type(message, &body);
    let role = shelley_event_role(&message.entry_type);
    let retained_text = provider_policy_event_text(event_type, &text, &body);
    let retained_body = provider_policy_body(event_type, &body);
    let result_evidence = provider_result_identifier_evidence(event_type, &text, &body);
    let result_outcome = provider_result_outcome_evidence(event_type, &body);
    ShelleyCoreEvent {
        provider_event_index: shelley_event_index(message),
        provider_event_hash: message.message_id.clone(),
        cursor: format!(
            "conversation:{}:sequence:{}:message:{}",
            message.conversation_id, message.sequence_id, message.message_id
        ),
        event_type,
        role,
        occurred_at,
        payload: json!({
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": result_evidence,
            "result_outcome": result_outcome,
            "source_format": SHELLEY_SQLITE_SOURCE_FORMAT,
            "body": provider_capped_json(&retained_body, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": "shelley_messages",
            "source_format": SHELLEY_SQLITE_SOURCE_FORMAT,
            "message_id": message.message_id,
            "conversation_id": message.conversation_id,
            "sequence_id": message.sequence_id,
            "rowid": message.rowid,
            "message_type": message.entry_type,
            "generation": message.generation,
            "excluded_from_context": message.excluded_from_context,
            "usage": message.usage_data.as_deref().map(provider_json_text),
            "llm_api_url": message.llm_api_url,
            "model_name": message.model_name,
            "forked_from_message_id": message.forked_from_message_id,
        }),
    }
}

fn attach_shelley_core_content_locator(
    event: &mut ShelleyCoreEvent,
    message: &ShelleyMessageRow,
    conversation: &ShelleyConversationRow,
    parent_bearing: bool,
    complete_text: &str,
) -> Result<()> {
    if event.event_type != EventType::Message
        || event
            .payload
            .pointer("/text_retention/truncated")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Ok(());
    }
    let mut coordinate = Vec::with_capacity(17);
    coordinate.push(if parent_bearing { 1 } else { 2 });
    coordinate.extend_from_slice(&(message.rowid as u64 ^ (1_u64 << 63)).to_be_bytes());
    coordinate.extend_from_slice(&(conversation.rowid as u64 ^ (1_u64 << 63)).to_be_bytes());
    let values = shelley_verified_record_values(message, conversation, parent_bearing);
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("Shelley content length exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        CaptureProvider::Shelley,
        SHELLEY_SQLITE_SOURCE_FORMAT,
        CompleteContentSourceFamily::Sqlite,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Shelley message route must have a verified-content profile",
    ))?;
    let persisted = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Sqlite,
        "shelley-compound-message-row-v1",
        &coordinate,
        event.provider_event_hash.clone(),
        shelley_logical_record_digest(&values)?,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Shelley complete-content locator exceeds the bounded canonical schema",
    ))?;
    attach_verified_content_locator(&mut event.metadata, persisted).ok_or(
        CaptureError::SystemInvariant("Shelley verified-content locator collection is malformed"),
    )?;
    Ok(())
}

fn shelley_logical_record_digest(
    values: &[NativeSqliteValue],
) -> Result<CompleteContentBodyDigest> {
    let mut digest = Sha256::new();
    digest.update(b"ctx-complete-content-sqlite-logical-row-v1\0");
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        match value {
            NativeSqliteValue::Null => digest.update([0]),
            NativeSqliteValue::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::RealBits(value) => {
                digest.update([2]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::Text(value) => {
                digest.update([3]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value.as_bytes());
            }
            NativeSqliteValue::Blob(value) => {
                digest.update([4]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value);
            }
        }
    }
    CompleteContentBodyDigest::parse(format!("{:x}", digest.finalize())).ok_or(
        CaptureError::SystemInvariant("Shelley SHA-256 formatting produced an invalid digest"),
    )
}

pub(super) fn shelley_timestamp(raw: Option<&str>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return fallback;
    };
    parse_rfc3339_utc(raw)
        .or_else(|| {
            NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f")
                .ok()
                .map(|naive| DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
        })
        .unwrap_or(fallback)
}
