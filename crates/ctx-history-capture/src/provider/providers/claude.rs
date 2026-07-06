use std::{fs::File, io::BufReader, path::Path};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderCursorCheckpoint, ProviderCursorRange, ProviderEventEnvelope, ProviderRawRetention,
    ProviderRedactionBoundary, ProviderSessionEnvelope, ProviderSourceEnvelope,
    ProviderSourceTrust, RedactionState, SessionStatus, PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use serde_json::{json, Value};

use crate::common::io::{ensure_regular_provider_transcript_file, read_provider_jsonl_line};
use crate::common::time::parse_rfc3339_utc;
use crate::provider::file_touches::provider_file_touches_from_raw_value;
use crate::provider::importer::provider_cursor_stream;
use crate::provider::native::{
    provider_capped_json, provider_local_preview, provider_role, provider_value_text,
};
use crate::{
    ProviderAdapterContext, ProviderImportFailure, ProviderNormalizationResult, Result,
    CLAUDE_PROJECTS_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS, PROVIDER_MAX_TEXT_CHARS,
};

pub(crate) fn normalize_claude_projects_jsonl_file(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    ensure_regular_provider_transcript_file(path)?;
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut result = ProviderNormalizationResult::default();
    let mut rows = Vec::new();
    let mut line = Vec::new();
    let mut line_number = 0usize;

    while read_provider_jsonl_line(&mut reader, &mut line)? {
        line_number += 1;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let value: Value = match serde_json::from_slice(&line) {
            Ok(value) => value,
            Err(err) => {
                result.summary.failed += 1;
                result.summary.failures.push(ProviderImportFailure {
                    line: line_number,
                    error: format!("malformed JSONL: {err}"),
                });
                continue;
            }
        };
        let timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_utc)
            .unwrap_or(context.imported_at);
        rows.push((line_number, value, timestamp));
    }
    if rows.is_empty() {
        return Ok(result);
    }

    let first = &rows[0].1;
    let file_stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown-session");
    let native_session_id = first
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .unwrap_or(file_stem)
        .to_owned();
    let (provider_session_id, parent_provider_session_id, external_agent_id, is_subagent) =
        claude_path_session_ids(path, &native_session_id);
    let started_at = rows
        .iter()
        .map(|(_, _, timestamp)| *timestamp)
        .min()
        .unwrap_or(context.imported_at);
    let cwd = first
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.trim().is_empty())
        .map(str::to_owned);
    let version = first
        .get("version")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let git_branch = first
        .get("gitBranch")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let raw_source_path = path.display().to_string();

    for (line_number, value, occurred_at) in rows {
        let event = claude_event(&value, line_number, occurred_at);
        if let Some(event) = &event {
            result
                .files_touched
                .extend(provider_file_touches_from_raw_value(
                    CaptureProvider::Claude,
                    &provider_session_id,
                    CLAUDE_PROJECTS_SOURCE_FORMAT,
                    Some(raw_source_path.as_str()),
                    &value,
                    event,
                    line_number,
                ));
        }
        result.captures.push((
            line_number,
            ProviderCaptureEnvelope {
                schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
                provider: CaptureProvider::Claude,
                source: ProviderSourceEnvelope {
                    source_format: CLAUDE_PROJECTS_SOURCE_FORMAT.to_owned(),
                    machine_id: context.machine_id.clone(),
                    observed_at: context.imported_at,
                    raw_source_path: Some(raw_source_path.clone()),
                    raw_retention: ProviderRawRetention::PathReference,
                    redaction_boundary: ProviderRedactionBoundary::BeforeExport,
                    trust: ProviderSourceTrust::ProviderNative,
                    fidelity: Fidelity::Imported,
                    cursor: Some(ProviderCursorRange {
                        before: None,
                        after: Some(ProviderCursorCheckpoint {
                            stream: provider_cursor_stream(
                                CaptureProvider::Claude,
                                CLAUDE_PROJECTS_SOURCE_FORMAT,
                            ),
                            cursor: format!("{}:line:{line_number}", path.display()),
                            observed_at: occurred_at,
                        }),
                    }),
                    idempotency_key: Some(format!(
                        "provider-source:claude:{CLAUDE_PROJECTS_SOURCE_FORMAT}:{provider_session_id}"
                    )),
                    metadata: json!({
                        "adapter": CLAUDE_PROJECTS_SOURCE_FORMAT,
                        "native_session_id": native_session_id,
                        "source_path": raw_source_path.clone(),
                    }),
                },
                session: ProviderSessionEnvelope {
                    provider_session_id: provider_session_id.clone(),
                    parent_provider_session_id: parent_provider_session_id.clone(),
                    root_provider_session_id: parent_provider_session_id.clone(),
                    external_agent_id: external_agent_id.clone(),
                    agent_type: if is_subagent {
                        AgentType::Subagent
                    } else {
                        AgentType::Primary
                    },
                    role_hint: Some(if is_subagent { "subagent" } else { "primary" }.to_owned()),
                    is_primary: !is_subagent,
                    status: SessionStatus::Imported,
                    started_at,
                    ended_at: None,
                    cwd: cwd.clone(),
                    fidelity: Fidelity::Imported,
                    idempotency_key: Some(format!("provider-session:claude:{provider_session_id}")),
                    artifacts: Vec::new(),
                    metadata: json!({
                        "source_format": CLAUDE_PROJECTS_SOURCE_FORMAT,
                        "native_session_id": native_session_id,
                        "version": version,
                        "git_branch": git_branch,
                        "source_path": path.display().to_string(),
                        "limitations": [
                            "binary attachments are referenced by native payload metadata but not expanded",
                            "previews are capped before local indexing/export"
                        ],
                    }),
                },
                event,
            },
        ));
    }

    Ok(result)
}

pub(crate) fn claude_path_session_ids(
    path: &Path,
    native_session_id: &str,
) -> (String, Option<String>, Option<String>, bool) {
    let Some(parent) = path.parent() else {
        return (native_session_id.to_owned(), None, None, false);
    };
    if parent.file_name().and_then(|name| name.to_str()) == Some("subagents") {
        let parent_session_id = parent
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(native_session_id)
            .to_owned();
        let agent_id = path
            .file_stem()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("subagent")
            .to_owned();
        return (
            format!("{parent_session_id}/subagents/{agent_id}"),
            Some(parent_session_id),
            Some(agent_id),
            true,
        );
    }
    (native_session_id.to_owned(), None, None, false)
}

pub(crate) fn claude_event(
    value: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
) -> Option<ProviderEventEnvelope> {
    let entry_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let message = value.get("message").unwrap_or(value);
    let message_role = message
        .get("role")
        .and_then(Value::as_str)
        .or_else(|| value.get("role").and_then(Value::as_str));
    let null = Value::Null;
    let content = message.get("content").unwrap_or(&null);
    let event_type = claude_event_type(entry_type, message);
    let role = Some(provider_role(message_role));
    let text = provider_value_text(content).unwrap_or_else(|| {
        if event_type == EventType::Notice {
            format!("Claude event: {entry_type}")
        } else {
            String::new()
        }
    });
    let (text, truncated) = provider_local_preview(&text, PROVIDER_MAX_TEXT_CHARS);

    Some(ProviderEventEnvelope {
        provider_event_index: (line_number - 1) as u64,
        provider_event_hash: value.get("uuid").and_then(Value::as_str).map(str::to_owned),
        cursor: value.get("uuid").and_then(Value::as_str).map(str::to_owned),
        event_type,
        role,
        occurred_at,
        fidelity: Fidelity::Imported,
        redaction_state: RedactionState::LocalPreview,
        idempotency_key: value
            .get("uuid")
            .and_then(Value::as_str)
            .map(|uuid| format!("provider-event:claude:{uuid}")),
        artifacts: Vec::new(),
        payload: json!({
            "entry_type": entry_type,
            "uuid": value.get("uuid").and_then(Value::as_str),
            "parent_uuid": value.get("parentUuid").and_then(Value::as_str),
            "message_id": message.get("id").and_then(Value::as_str),
            "request_id": value.get("requestId").and_then(Value::as_str),
            "role": message_role,
            "text": text,
            "truncated": truncated,
            "content_preview": provider_capped_json(content, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": "claude_projects_jsonl",
            "source_format": CLAUDE_PROJECTS_SOURCE_FORMAT,
            "line": line_number,
            "entry_type": entry_type,
            "model": message.get("model").and_then(Value::as_str),
            "usage": message.get("usage").cloned(),
            "stop_reason": message.get("stop_reason").and_then(Value::as_str),
            "is_sidechain": value.get("isSidechain").and_then(Value::as_bool),
            "tool_use_result": value.get("toolUseResult").cloned(),
        }),
    })
}

pub(crate) fn claude_event_type(entry_type: &str, message: &Value) -> EventType {
    if claude_content_has_type(message.get("content"), "tool_result")
        || message.get("toolUseResult").is_some()
    {
        return EventType::ToolOutput;
    }
    if claude_content_has_type(message.get("content"), "tool_use") {
        return EventType::ToolCall;
    }
    match entry_type {
        "user" | "assistant" => EventType::Message,
        "system"
        | "progress"
        | "permission-mode"
        | "last-prompt"
        | "queue-operation"
        | "attachment"
        | "file-history-snapshot"
        | "ai-title" => EventType::Notice,
        _ => EventType::Notice,
    }
}

pub(crate) fn claude_content_has_type(content: Option<&Value>, expected: &str) -> bool {
    content
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some(expected))
        })
        .unwrap_or(false)
}
