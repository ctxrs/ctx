use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::complete_content::CompleteContentBodyDigest;
use crate::native_source::NativeSqliteValue;
use crate::provider::normalization::{
    provider_capped_json, provider_json_text, provider_nonnegative_i64_to_u64,
    provider_policy_body, provider_policy_event_text, provider_required_timestamp_seconds,
    provider_result_identifier_evidence, provider_result_outcome_evidence, provider_role,
    provider_value_text,
};
use crate::{
    CaptureError, OutputOutcome, OutputOutcomeMetadata, Result, HERMES_SQLITE_SOURCE_FORMAT,
    PROVIDER_MAX_PREVIEW_CHARS,
};

mod layout;
pub(crate) mod source_backed;
mod sqlite;

use self::layout::{decode_hermes_message, HermesMessageRow, HermesSchema, HermesSqliteValue};

pub(super) const HERMES_CAPTURE_REVISION: u32 = 2;
pub(super) const HERMES_POLICY_REVISION: u32 = 6;

pub(crate) fn load_hermes_message_values_schema(conn: &rusqlite::Connection) -> Result<()> {
    HermesSchema::detect(conn).map(|_| ())
}

pub(crate) fn load_hermes_message_values(
    conn: &rusqlite::Connection,
    rowid: i64,
) -> Result<Vec<NativeSqliteValue>> {
    let schema = HermesSchema::detect(conn)?;
    let visibility = schema.message_visibility();
    let predicate = if visibility.is_empty() {
        String::new()
    } else {
        format!(" and {visibility}")
    };
    let sql = format!(
        "select {} from messages m where m.rowid = ?1{predicate}",
        schema.messages().projection()
    );
    conn.query_row(&sql, [rowid], |row| {
        schema
            .messages()
            .capture_values(row, 0)
            .map(|values| values.into_iter().map(native_source_value).collect())
    })
    .map_err(Into::into)
}

pub(crate) fn hermes_complete_message_with_normalized_hash(
    conn: &rusqlite::Connection,
    values: &[NativeSqliteValue],
) -> Result<(String, String, String, String)> {
    let schema = HermesSchema::detect(conn)?;
    let values = values
        .iter()
        .map(hermes_sqlite_value)
        .collect::<Result<Vec<_>>>()?;
    let row = decode_hermes_message(&schema, &values)?;
    let content = hermes_decode_content(row.content.as_deref());
    let text = provider_value_text(&content).unwrap_or_else(|| {
        row.tool_name
            .as_ref()
            .map(|name| format!("tool: {name}"))
            .unwrap_or_else(|| format!("Hermes {}", row.role))
    });
    let normalized_hash = hermes_message_revision(&row)?;
    let provider_hash = format!("message:{}", row.id);
    Ok((row.session_id, provider_hash, normalized_hash, text))
}

fn native_source_value(value: HermesSqliteValue) -> NativeSqliteValue {
    match value {
        HermesSqliteValue::Null => NativeSqliteValue::Null,
        HermesSqliteValue::Integer(value) => NativeSqliteValue::Integer(value),
        HermesSqliteValue::RealBits(value) => NativeSqliteValue::RealBits(value),
        HermesSqliteValue::Text(value) => NativeSqliteValue::Text(value),
    }
}

fn hermes_sqlite_value(value: &NativeSqliteValue) -> Result<HermesSqliteValue> {
    match value {
        NativeSqliteValue::Null => Ok(HermesSqliteValue::Null),
        NativeSqliteValue::Integer(value) => Ok(HermesSqliteValue::Integer(*value)),
        NativeSqliteValue::RealBits(value) => Ok(HermesSqliteValue::RealBits(*value)),
        NativeSqliteValue::Text(value) => Ok(HermesSqliteValue::Text(value.clone())),
        NativeSqliteValue::Blob(_) => Err(CaptureError::InvalidPayload(
            "Hermes logical rows do not accept SQLite blobs".to_owned(),
        )),
    }
}

#[derive(Clone, Debug)]
struct HermesPreparedCoreMessage {
    native: HermesNativeEvent,
    record_digest: CompleteContentBodyDigest,
}

impl HermesPreparedCoreMessage {
    fn owned_bytes(&self) -> usize {
        serde_json::to_vec(&self.native.payload)
            .map(|bytes| bytes.len())
            .unwrap_or(usize::MAX)
            .saturating_add(
                serde_json::to_vec(&self.native.metadata)
                    .map(|bytes| bytes.len())
                    .unwrap_or(usize::MAX),
            )
            .saturating_add(self.native.cursor.len())
            .saturating_add(4 * 1024)
    }
}

fn prepare_hermes_core_message(
    row: &HermesMessageRow,
    source_record_ordinal: u64,
    values: &[HermesSqliteValue],
) -> Result<HermesPreparedCoreMessage> {
    let mut native = hermes_native_event(row, source_record_ordinal)?;
    let record_digest = hermes_layout_record_digest(values);
    native.complete_text.clear();
    Ok(HermesPreparedCoreMessage {
        native,
        record_digest,
    })
}

fn hermes_message_revision(row: &HermesMessageRow) -> Result<String> {
    let event = hermes_native_event(row, 0)?;
    ctx_history_core::compute_payload_hash(&event.payload).map_err(Into::into)
}

#[derive(Clone, Debug)]
pub(super) struct HermesNativeEvent {
    pub(super) provider_event_index: u64,
    // Preserve the provider hash in the exact native event shape for staging
    // Pro and diagnostic materializers.
    #[allow(dead_code)]
    pub(super) provider_event_hash: Option<String>,
    pub(super) cursor: String,
    pub(super) event_type: EventType,
    pub(super) role: Option<EventRole>,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) payload: Value,
    pub(super) metadata: Value,
    pub(super) complete_text: String,
}

pub(in crate::provider::providers::hermes) fn hermes_native_event(
    row: &HermesMessageRow,
    source_record_ordinal: u64,
) -> Result<HermesNativeEvent> {
    let content = hermes_decode_content(row.content.as_deref());
    let output_outcome = (row.role == "tool").then(|| hermes_output_outcome(row, &content));
    let text = hermes_normalized_result_content(&row.role, &content)
        .or_else(|| provider_value_text(&content))
        .unwrap_or_else(|| {
            row.tool_name
                .as_ref()
                .map(|name| format!("tool: {name}"))
                .unwrap_or_else(|| format!("Hermes {}", row.role))
        });
    let occurred_at =
        provider_required_timestamp_seconds(row.timestamp, "Hermes message timestamp")?;
    let event_type = hermes_event_type(row);
    let body = json!({
            "message_id": row.id,
            "role": row.role,
            "content": content,
            "tool_call_id": row.tool_call_id,
            "tool_calls": row.tool_calls.as_deref().map(provider_json_text),
            "tool_name": row.tool_name,
            "status": row.finish_reason,
            "timed_out": output_outcome.as_ref().is_some_and(
                |outcome| outcome.outcome == OutputOutcome::Timeout
            ),
            "is_error": output_outcome.as_ref().is_some_and(
                |outcome| outcome.outcome == OutputOutcome::Failure
            ),
            "reasoning": row.reasoning,
            "reasoning_content": row.reasoning_content,
            "reasoning_details": row.reasoning_details.as_deref().map(provider_json_text),
            "codex_reasoning_items": row.codex_reasoning_items.as_deref().map(provider_json_text),
            "codex_message_items": row.codex_message_items.as_deref().map(provider_json_text),
    });
    let retained_text = provider_policy_event_text(event_type, &text, &body);
    let retained_body = provider_policy_body(event_type, &body);
    let result_evidence = provider_result_identifier_evidence(event_type, &text, &body);
    let result_outcome = provider_result_outcome_evidence(event_type, &body);
    Ok(HermesNativeEvent {
        provider_event_index: provider_nonnegative_i64_to_u64(row.id, "Hermes message id")?,
        provider_event_hash: Some(format!("message:{}", row.id)),
        cursor: format!("messages:id:{}", row.id),
        event_type,
        role: Some(provider_role(Some(&row.role))),
        occurred_at,
        payload: json!({
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": result_evidence,
            "result_outcome": result_outcome,
            "source_format": HERMES_SQLITE_SOURCE_FORMAT,
            "body": provider_capped_json(&retained_body, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": "hermes_state_db",
            "source_format": HERMES_SQLITE_SOURCE_FORMAT,
            "message_id": row.id,
            "platform_message_id": row.platform_message_id,
            "token_count": row.token_count,
            "finish_reason": row.finish_reason,
            "observed": row.observed != 0,
            "active": row.active != 0,
            "compacted": row.compacted != 0,
            "source_record_ordinal": source_record_ordinal,
            "source_record_subrecord_index": 0,
        }),
        complete_text: text,
    })
}

fn hermes_record_digest(values: &[NativeSqliteValue]) -> CompleteContentBodyDigest {
    const DOMAIN: &[u8] = b"ctx-complete-content-sqlite-logical-row-v1\0";
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
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
    CompleteContentBodyDigest::parse(format!("{:x}", digest.finalize()))
        .expect("SHA-256 formatter must return a valid digest")
}

fn hermes_layout_record_digest(values: &[HermesSqliteValue]) -> CompleteContentBodyDigest {
    const DOMAIN: &[u8] = b"ctx-complete-content-sqlite-logical-row-v1\0";
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        match value {
            HermesSqliteValue::Null => digest.update([0]),
            HermesSqliteValue::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            HermesSqliteValue::RealBits(value) => {
                digest.update([2]);
                digest.update(value.to_be_bytes());
            }
            HermesSqliteValue::Text(value) => {
                digest.update([3]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value.as_bytes());
            }
        }
    }
    CompleteContentBodyDigest::parse(format!("{:x}", digest.finalize()))
        .expect("SHA-256 formatter must return a valid digest")
}

pub(crate) fn hermes_decode_content(raw: Option<&str>) -> Value {
    let Some(raw) = raw else {
        return Value::Null;
    };
    if let Some(json) = raw.strip_prefix("\0json:") {
        return provider_json_text(json);
    }
    Value::String(raw.to_owned())
}

fn hermes_output_outcome(row: &HermesMessageRow, content: &Value) -> OutputOutcomeMetadata {
    let mut evidence = HermesOutputEvidence::default();
    if let Some(status) = row.finish_reason.as_deref() {
        hermes_classify_status(status, &mut evidence);
    }
    let mut remaining = 4_096;
    hermes_collect_output_evidence(content, &mut remaining, &mut evidence);
    OutputOutcomeMetadata {
        outcome: if evidence.timeout {
            OutputOutcome::Timeout
        } else if evidence.failure {
            OutputOutcome::Failure
        } else if evidence.success {
            OutputOutcome::Success
        } else {
            OutputOutcome::Unknown
        },
        exit_code: evidence.exit_code,
        duration_ms: evidence.duration_ms,
    }
}

#[derive(Default)]
struct HermesOutputEvidence {
    success: bool,
    failure: bool,
    timeout: bool,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
}

fn hermes_collect_output_evidence(
    value: &Value,
    remaining: &mut usize,
    evidence: &mut HermesOutputEvidence,
) {
    if *remaining == 0 {
        return;
    }
    *remaining -= 1;
    match value {
        Value::Array(values) => {
            for value in values {
                hermes_collect_output_evidence(value, remaining, evidence);
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
                    "durationms" => {
                        evidence.duration_ms = value.as_u64();
                    }
                    "success" | "ok" => {
                        if let Some(success) = value.as_bool() {
                            evidence.success |= success;
                            evidence.failure |= !success;
                        }
                    }
                    "iserror" => {
                        evidence.failure |= value.as_bool().unwrap_or(false);
                    }
                    "timedout" | "timeout" => {
                        evidence.timeout |= value.as_bool().unwrap_or(false);
                    }
                    "status" | "state" | "outcome" => {
                        if let Some(status) = value.as_str() {
                            hermes_classify_status(status, evidence);
                        }
                    }
                    "error" if hermes_error_value_is_present(value) => {
                        evidence.failure = true;
                    }
                    _ => {}
                }
                hermes_collect_output_evidence(value, remaining, evidence);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn hermes_classify_status(status: &str, evidence: &mut HermesOutputEvidence) {
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

fn hermes_error_value_is_present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.trim().is_empty(),
        Value::Number(value) => value.as_i64().is_some_and(|value| value != 0),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
    }
}

/// Returns the complete normalized result body for one Hermes tool-role row.
///
/// Hermes owns the `content` column as the result body, so no nested field-name
/// search is needed. The caller owns any byte bound.
pub(crate) fn hermes_normalized_result_content(role: &str, content: &Value) -> Option<String> {
    (role == "tool")
        .then(|| provider_value_text(content))
        .flatten()
}

fn hermes_event_type(row: &HermesMessageRow) -> EventType {
    if row.role == "tool" {
        EventType::ToolOutput
    } else if row
        .tool_calls
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || row
            .tool_name
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    {
        EventType::ToolCall
    } else {
        EventType::Message
    }
}

#[cfg(test)]
#[path = "hermes/tests.rs"]
mod tests;
