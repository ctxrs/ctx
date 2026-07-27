use chrono::{DateTime, Utc};
use ctx_history_core::{Confidence, EventType, Fidelity, FileChangeKind, ProviderEventEnvelope};
use serde_json::{json, Value};

use crate::provider::file_touches::{normalize_file_path, FileTouchDraft};
use crate::provider::normalization::{
    provider_capped_json, provider_policy_body, provider_policy_event_text,
    provider_required_timestamp_millis, provider_result_identifier_evidence,
    provider_result_outcome_evidence, provider_role, provider_value_text,
};
use crate::{fnv1a64, CaptureError, Result, PROVIDER_MAX_PREVIEW_CHARS};

use super::schema::{OpenCodeMessageRow, OpenCodeSqliteDialect};

pub(super) const OPENCODE_MESSAGE_PART_DEFAULT_ROLE: &str = "assistant";

pub(super) fn opencode_entry_type_from_data(fallback: &str, data: &str) -> String {
    if !fallback.trim().is_empty() && fallback != "message" {
        return fallback.to_owned();
    }
    serde_json::from_str::<Value>(data)
        .ok()
        .and_then(|value| opencode_message_type_from_data(&value))
        .unwrap_or_else(|| fallback.to_owned())
}

pub(super) fn opencode_part_type(column_type: Option<&str>, data: &Value) -> String {
    column_type
        .filter(|value| !value.trim().is_empty())
        .or_else(|| data.get("type").and_then(Value::as_str))
        .unwrap_or("part")
        .to_owned()
}

pub(super) fn opencode_message_part_role(data: &Value) -> String {
    data.get("role")
        .or_else(|| data.pointer("/message/role"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|role| matches!(*role, "assistant" | "user" | "system"))
        .unwrap_or(OPENCODE_MESSAGE_PART_DEFAULT_ROLE)
        .to_owned()
}

pub(super) fn opencode_text_part_text(part_type: &str, data: &Value) -> Option<String> {
    (part_type == "text")
        .then(|| data.get("text").and_then(Value::as_str))
        .flatten()
        .map(str::to_owned)
}

pub(super) fn opencode_tool_part_event_data(
    message_id: &str,
    part_id: &str,
    part_type: &str,
    time_created: i64,
    data: &Value,
) -> Option<Value> {
    if !matches!(part_type, "tool" | "tool_result" | "result") {
        return None;
    }
    let tool_name = opencode_tool_part_name(data);
    let status = opencode_tool_part_status(data);
    let exit_code = opencode_tool_part_exit_code(data);
    let is_error = opencode_tool_part_is_error(data, status.as_deref(), exit_code);
    let result_evidence = provider_result_identifier_evidence(EventType::ToolOutput, "", data);
    let result_outcome = provider_result_outcome_evidence(EventType::ToolOutput, data);
    Some(json!({
        "role": "tool",
        "time": { "created": time_created },
        "source_table": "message+part",
        "message_id": message_id,
        "part_id": part_id,
        "part_type": part_type,
        "tool_name": tool_name,
        "status": status,
        "exit_code": exit_code,
        "is_error": is_error,
        "result_evidence": result_evidence,
        "result_outcome": result_outcome,
        "output_retention": "metadata_only",
    }))
}

pub(super) fn opencode_tool_part_name(data: &Value) -> String {
    data.get("tool")
        .or_else(|| data.get("tool_name"))
        .or_else(|| data.get("name"))
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("tool")
        .to_owned()
}

pub(super) fn opencode_tool_part_status(data: &Value) -> Option<String> {
    data.pointer("/state/status")
        .or_else(|| data.get("status"))
        .and_then(Value::as_str)
        .filter(|status| !status.trim().is_empty())
        .map(str::to_owned)
}

pub(super) fn opencode_tool_part_exit_code(data: &Value) -> Option<i64> {
    data.pointer("/state/metadata/exit")
        .or_else(|| data.pointer("/state/metadata/exit_code"))
        .or_else(|| data.pointer("/state/metadata/exitCode"))
        .or_else(|| data.get("exit_code"))
        .or_else(|| data.get("exitCode"))
        .and_then(Value::as_i64)
}

pub(super) fn opencode_tool_part_is_error(
    data: &Value,
    status: Option<&str>,
    exit_code: Option<i64>,
) -> bool {
    data.get("is_error")
        .or_else(|| data.get("isError"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || exit_code.is_some_and(|code| code != 0)
        || status.is_some_and(|status| {
            matches!(
                status.trim().to_ascii_lowercase().as_str(),
                "failed" | "failure" | "error" | "errored" | "timeout" | "timed_out" | "timedout"
            )
        })
}

pub(super) fn opencode_message_part_identity_index(message_id: &str, part_id: &str) -> i64 {
    let key = format!("message+part:{message_id}:{part_id}");
    let index = fnv1a64(key.as_bytes()) & 0x0000_ffff_ffff;
    index.max(1) as i64
}

pub(super) fn opencode_patch_file_touch_drafts<'a>(
    data: &'a Value,
    part_id: &'a str,
    part_type: &'a str,
) -> impl Iterator<Item = FileTouchDraft> + 'a {
    let direct_path = data
        .get("path")
        .and_then(Value::as_str)
        .and_then(normalize_file_path)
        .into_iter();
    let file_paths = data
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|file| match file {
            Value::String(path) => normalize_file_path(path),
            Value::Object(object) => object
                .get("path")
                .and_then(Value::as_str)
                .and_then(normalize_file_path),
            _ => None,
        });
    direct_path
        .chain(file_paths)
        .map(move |path| FileTouchDraft {
            path,
            old_path: None,
            change_kind: Some(FileChangeKind::Modified),
            confidence: Confidence::Explicit,
            metadata: json!({
                "source": "opencode_message_part_metadata",
                "part_id": part_id,
                "part_type": part_type,
            }),
        })
}

pub(super) fn opencode_message_type_from_data(data: &Value) -> Option<String> {
    data.get("role")
        .or_else(|| data.get("type"))
        .or_else(|| data.pointer("/message/role"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

pub(super) fn opencode_event(
    row: &OpenCodeMessageRow,
    data: &Value,
    occurred_at: DateTime<Utc>,
    provider_event_index: u64,
    dialect: &OpenCodeSqliteDialect,
) -> ProviderEventEnvelope {
    let is_message_part = data.get("source_table").and_then(Value::as_str) == Some("message+part");
    let event_type = opencode_event_type(&row.entry_type, data);
    let role = Some(provider_role(Some(&row.entry_type)));
    let text = opencode_event_text(&row.entry_type, data, event_type, dialect);
    let body = if is_message_part {
        opencode_message_part_event_body(data)
    } else {
        data.clone()
    };
    let retained_text = provider_policy_event_text(event_type, &text, &body);
    let result_evidence = body
        .get("result_evidence")
        .cloned()
        .unwrap_or_else(|| provider_result_identifier_evidence(event_type, &text, &body));
    let result_outcome = body
        .get("result_outcome")
        .cloned()
        .unwrap_or_else(|| provider_result_outcome_evidence(event_type, &body));
    let payload = if is_message_part {
        json!({
            "entry_type": row.entry_type,
            "message_id": data
                .get("message_id")
                .and_then(Value::as_str)
                .unwrap_or(&row.id),
            "part_id": data.get("part_id").cloned(),
            "part_type": data.get("part_type").cloned(),
            "session_message_seq": row.seq,
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": result_evidence,
            "result_outcome": result_outcome,
            "body": provider_capped_json(&provider_policy_body(event_type, &body), PROVIDER_MAX_PREVIEW_CHARS),
        })
    } else {
        json!({
            "entry_type": row.entry_type,
            "message_id": row.id,
            "session_message_seq": row.seq,
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": result_evidence,
            "result_outcome": result_outcome,
            "body": provider_capped_json(&provider_policy_body(event_type, &body), PROVIDER_MAX_PREVIEW_CHARS),
        })
    };
    let metadata = if is_message_part {
        json!({
            "source": dialect.source_format,
            "source_format": dialect.source_format,
            "session_message_id": row.id,
            "session_message_seq": row.seq,
            "message_id": data.get("message_id").cloned(),
            "part_id": data.get("part_id").cloned(),
            "part_type": data.get("part_type").cloned(),
            "time_created": row.time_created,
            "time_updated": row.time_updated,
            "model": data.get("model").cloned(),
            "tokens": data.get("tokens").cloned(),
            "cost": data.get("cost").cloned(),
            "finish": data.get("finish").cloned(),
            "error": data.get("error").cloned(),
        })
    } else {
        json!({
            "source": dialect.source_format,
            "source_format": dialect.source_format,
            "session_message_id": row.id,
            "session_message_seq": row.seq,
            "time_created": row.time_created,
            "time_updated": row.time_updated,
            "model": data.get("model").cloned(),
            "tokens": data.get("tokens").cloned(),
            "cost": data.get("cost").cloned(),
            "finish": data.get("finish").cloned(),
            "error": data.get("error").cloned(),
        })
    };
    ProviderEventEnvelope {
        provider_event_index,
        provider_event_hash: Some(row.id.clone()),
        cursor: Some(opencode_event_cursor(row, data)),
        event_type,
        role,
        occurred_at,
        fidelity: Fidelity::Imported,
        idempotency_key: Some(format!(
            "provider-event:{}:{}:{}",
            dialect.provider.as_str(),
            row.session_id,
            row.id
        )),
        artifacts: Vec::new(),
        payload,
        metadata,
    }
}

pub(super) fn opencode_message_part_event_body(data: &Value) -> Value {
    json!({
        "role": data.get("role").cloned(),
        "time": data.get("time").cloned(),
        "text": data.get("text").cloned(),
        "source_table": data.get("source_table").cloned(),
        "message_id": data.get("message_id").cloned(),
        "part_id": data.get("part_id").cloned(),
        "part_type": data.get("part_type").cloned(),
        "tool_name": data.get("tool_name").cloned(),
        "status": data.get("status").cloned(),
        "exit_code": data.get("exit_code").cloned(),
        "is_error": data.get("is_error").cloned(),
        "result_evidence": data.get("result_evidence").cloned(),
        "result_outcome": data.get("result_outcome").cloned(),
        "output_retention": data.get("output_retention").cloned(),
    })
}

pub(super) fn opencode_event_cursor(row: &OpenCodeMessageRow, data: &Value) -> String {
    if data.get("source_table").and_then(Value::as_str) == Some("message+part") {
        return format!(
            "message:{}:part:{}",
            data.get("message_id")
                .and_then(Value::as_str)
                .unwrap_or(&row.id),
            data.get("part_id")
                .and_then(Value::as_str)
                .unwrap_or(&row.id)
        );
    }
    format!("session_message:{}:seq:{}", row.session_id, row.seq)
}

pub(super) fn opencode_event_type(entry_type: &str, data: &Value) -> EventType {
    match entry_type {
        "assistant" if opencode_content_has_tool(data) => EventType::ToolCall,
        "assistant" | "user" | "system" => EventType::Message,
        "tool" | "tool_result" => EventType::ToolOutput,
        "shell" => EventType::CommandOutput,
        _ => EventType::Notice,
    }
}

pub(super) fn opencode_event_text(
    entry_type: &str,
    data: &Value,
    event_type: EventType,
    dialect: &OpenCodeSqliteDialect,
) -> String {
    if let Some(text) = data.get("text").and_then(Value::as_str) {
        return text.to_owned();
    }
    if entry_type == "shell" {
        let command = data
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("shell");
        let output = data.get("output").and_then(Value::as_str).unwrap_or("");
        return format!("{command}\n{output}");
    }
    if matches!(entry_type, "tool" | "tool_result") {
        let tool_name = data
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let status = data
            .get("status")
            .and_then(Value::as_str)
            .map(|status| format!("\nstatus: {status}"))
            .unwrap_or_default();
        return format!("tool result: {tool_name}{status}");
    }
    if let Some(content) = data.get("content") {
        if let Some(text) = provider_value_text(content) {
            return text;
        }
    }
    if event_type == EventType::Notice {
        format!("{} event: {entry_type}", dialect.display_name)
    } else {
        String::new()
    }
}

pub(super) fn opencode_content_has_tool(data: &Value) -> bool {
    data.get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks.iter().any(|block| {
                matches!(
                    block.get("type").and_then(Value::as_str),
                    Some("tool" | "tool_use" | "toolCall")
                )
            })
        })
        .unwrap_or(false)
}

pub(super) fn opencode_event_time(
    data: &Value,
    dialect: &OpenCodeSqliteDialect,
) -> Result<Option<DateTime<Utc>>> {
    let Some(value) = data.pointer("/time/created") else {
        return Ok(None);
    };
    let millis = value.as_i64().ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "{} event time.created must be integer millis",
            dialect.display_name
        ))
    })?;
    provider_required_timestamp_millis(millis, dialect.event_time_created_field).map(Some)
}
