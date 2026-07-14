use std::{borrow::Cow, collections::BTreeMap};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderCursorCheckpoint, ProviderCursorRange, ProviderEventEnvelope, ProviderSessionEnvelope,
    ProviderSourceEnvelope, ProviderSourceTrust, SessionStatus,
    PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use serde_json::{json, Value};

use crate::provider::file_touches::{
    collect_patch_file_touches, collect_structured_file_touches, provider_file_touch_envelopes,
    ProviderFileTouchEnvelopeContext,
};

use crate::common::time::{parse_optional_rfc3339_field, parse_rfc3339_utc};
use crate::provider::file_touches::event_type_supports_structured_file_touches;
use crate::provider::importer::provider_cursor_stream;
use crate::provider::native::{
    capped_text, provider_output_event_is_failure,
    provider_output_preview_omitting_nested_patch_diff,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderFileTouchedEnvelope, Result,
    CODEX_SESSION_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS, PROVIDER_MAX_TEXT_CHARS,
};

#[derive(Debug, Clone)]
pub(crate) struct CodexSessionHeader {
    pub(crate) id: String,
    pub(crate) timestamp: DateTime<Utc>,
    pub(crate) cwd: Option<String>,
    pub(crate) originator: Option<String>,
    pub(crate) cli_version: Option<String>,
    pub(crate) source: Value,
    pub(crate) parent_session: Option<String>,
    pub(crate) agent_nickname: Option<String>,
    pub(crate) agent_role: Option<String>,
    pub(crate) model_provider: Option<String>,
    pub(crate) raw: Value,
}
#[derive(Debug, Clone, Default)]
pub(crate) struct CodexToolCallContext {
    pub(crate) tool_name: String,
    pub(crate) command_preview: Option<String>,
    pub(crate) arguments_preview: Option<String>,
}
#[derive(Debug, Clone, Default)]
pub(crate) struct CodexSessionLineCapture {
    pub(crate) event: Option<ProviderEventEnvelope>,
    pub(crate) files_touched: Vec<(usize, ProviderFileTouchedEnvelope)>,
}
pub(crate) fn codex_session_line_timestamp(
    value: &Value,
    fallback: DateTime<Utc>,
) -> Result<DateTime<Utc>> {
    Ok(parse_optional_rfc3339_field(value, "timestamp")?.unwrap_or(fallback))
}
pub(crate) fn codex_session_header(value: Value) -> Result<CodexSessionHeader> {
    let payload = value
        .get("payload")
        .ok_or_else(|| CaptureError::InvalidPayload("codex session_meta missing payload".into()))?;
    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| CaptureError::InvalidPayload("codex session_meta missing id".into()))?
        .to_owned();
    let timestamp = payload
        .get("timestamp")
        .and_then(Value::as_str)
        .or_else(|| value.get("timestamp").and_then(Value::as_str))
        .and_then(parse_rfc3339_utc)
        .ok_or_else(|| {
            CaptureError::InvalidPayload("codex session_meta missing timestamp".into())
        })?;
    let source = payload.get("source").cloned().unwrap_or(Value::Null);
    let parent_session = source
        .pointer("/subagent/thread_spawn/parent_thread_id")
        .or_else(|| source.pointer("/thread_spawn/parent_thread_id"))
        .or_else(|| source.get("parent_thread_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned);

    Ok(CodexSessionHeader {
        id,
        timestamp,
        cwd: payload
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::to_owned),
        originator: payload
            .get("originator")
            .and_then(Value::as_str)
            .map(str::to_owned),
        cli_version: payload
            .get("cli_version")
            .and_then(Value::as_str)
            .map(str::to_owned),
        source,
        parent_session,
        agent_nickname: payload
            .get("agent_nickname")
            .and_then(Value::as_str)
            .map(str::to_owned),
        agent_role: payload
            .get("agent_role")
            .and_then(Value::as_str)
            .map(str::to_owned),
        model_provider: payload
            .get("model_provider")
            .and_then(Value::as_str)
            .map(str::to_owned),
        raw: value,
    })
}
pub(crate) fn codex_session_capture(
    header: &CodexSessionHeader,
    event: Option<ProviderEventEnvelope>,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let cursor = Some(ProviderCursorRange {
        before: None,
        after: Some(ProviderCursorCheckpoint {
            stream: provider_cursor_stream(CaptureProvider::Codex, CODEX_SESSION_SOURCE_FORMAT),
            cursor: format!("line:{line_number}"),
            observed_at: occurred_at,
        }),
    });
    let is_subagent = header.parent_session.is_some();
    let role_hint = header
        .agent_role
        .clone()
        .or_else(|| is_subagent.then(|| "subagent".to_owned()))
        .or_else(|| Some("primary".to_owned()));

    ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider: CaptureProvider::Codex,
        source: ProviderSourceEnvelope {
            source_format: CODEX_SESSION_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            observed_at: context.imported_at,
            raw_source_path: context
                .source_path
                .as_ref()
                .map(|path| path.display().to_string()),
            source_root: context.source_root_display(),
            trust: ProviderSourceTrust::ProviderExport,
            fidelity: Fidelity::Imported,
            cursor,
            idempotency_key: Some(format!(
                "provider-source:codex:{CODEX_SESSION_SOURCE_FORMAT}:{}",
                header.id
            )),
            metadata: json!({
                "adapter": CODEX_SESSION_SOURCE_FORMAT,
                "source_fidelity": "codex_rollout_jsonl",
            }),
        },
        session: ProviderSessionEnvelope {
            provider_session_id: header.id.clone(),
            parent_provider_session_id: header.parent_session.clone(),
            root_provider_session_id: header.parent_session.clone(),
            external_agent_id: header.agent_nickname.clone(),
            agent_type: if is_subagent {
                AgentType::Subagent
            } else {
                AgentType::Primary
            },
            role_hint,
            is_primary: !is_subagent,
            status: SessionStatus::Imported,
            started_at: header.timestamp,
            ended_at: None,
            cwd: header.cwd.clone(),
            fidelity: Fidelity::Imported,
            idempotency_key: Some(format!("provider-session:codex:{}", header.id)),
            artifacts: Vec::new(),
            metadata: json!({
                "source_format": CODEX_SESSION_SOURCE_FORMAT,
                "source_fidelity": "codex_rollout_jsonl",
                "originator": header.originator,
                "cli_version": header.cli_version,
                "source": header.source,
                "agent_nickname": header.agent_nickname,
                "agent_role": header.agent_role,
                "model_provider": header.model_provider,
                "parent_session": header.parent_session,
                "raw_session_meta_keys": header.raw.as_object().map(|object| object.keys().cloned().collect::<Vec<_>>()),
                "import_profile": "default",
                "limitations": [
                    "default profile indexes session metadata, user and assistant messages, compacted context summaries, reasoning summaries, tool-call metadata, failed-output diagnostics, file touches, and parent-child session edges where present",
                    "successful command output, raw diffs, complete tool output, encrypted reasoning content, bootstrap context, lifecycle notices, and binary artifacts remain in the raw transcript referenced by raw_source_path",
                    "previews are capped before local indexing/export"
                ],
            }),
        },
        event,
    }
}
pub(crate) struct CodexSessionLineContext<'a> {
    pub(crate) line_number: usize,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) raw_source_path: Option<&'a str>,
    pub(crate) source_root: Option<&'a str>,
}
pub(crate) fn codex_session_line_capture(
    header: &CodexSessionHeader,
    value: &Value,
    call_contexts: &mut BTreeMap<String, CodexToolCallContext>,
    context: CodexSessionLineContext<'_>,
) -> CodexSessionLineCapture {
    let CodexSessionLineContext {
        line_number,
        occurred_at,
        raw_source_path,
        source_root,
    } = context;
    let event = codex_session_event(value, line_number, occurred_at, call_contexts);
    let mut drafts = Vec::new();
    collect_patch_file_touches(value, &mut drafts);
    if drafts.is_empty()
        && (event
            .as_ref()
            .is_some_and(|event| event_type_supports_structured_file_touches(event.event_type))
            || codex_value_is_tool_call(value))
    {
        collect_structured_file_touches(value, &mut drafts);
    }
    let files_touched = provider_file_touch_envelopes(
        ProviderFileTouchEnvelopeContext {
            provider: CaptureProvider::Codex,
            provider_session_id: &header.id,
            source_format: CODEX_SESSION_SOURCE_FORMAT,
            raw_source_path,
            source_root,
            occurred_at,
            provider_event_index: event.as_ref().map(|event| event.provider_event_index),
            provider_touch_base_index: (line_number as u64) << 16,
            line_number,
        },
        drafts,
    );
    CodexSessionLineCapture {
        event,
        files_touched,
    }
}
pub(crate) fn codex_value_is_tool_call(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("response_item")
        && matches!(
            value
                .get("payload")
                .and_then(|payload| payload.get("type"))
                .and_then(Value::as_str),
            Some("function_call" | "custom_tool_call")
        )
}
pub(crate) fn codex_session_event(
    value: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    call_contexts: &mut BTreeMap<String, CodexToolCallContext>,
) -> Option<ProviderEventEnvelope> {
    let entry_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    match entry_type {
        "response_item" => {
            let payload = value.get("payload")?;
            codex_response_item_event(payload, line_number, occurred_at, call_contexts)
        }
        "compacted" => {
            let text = value.get("payload").and_then(codex_content_text)?;
            let (text, truncated) = codex_local_preview(&text, PROVIDER_MAX_TEXT_CHARS);
            Some(codex_provider_event(
                line_number,
                occurred_at,
                EventType::Summary,
                Some(EventRole::System),
                json!({
                    "entry_type": entry_type,
                    "text": text,
                    "truncated": truncated,
                }),
                json!({
                    "source": "codex_session",
                    "source_format": CODEX_SESSION_SOURCE_FORMAT,
                    "line": line_number,
                    "entry_type": entry_type,
                }),
            ))
        }
        "event_msg" => {
            let payload = value.get("payload")?;
            let msg_type = payload
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            if matches!(
                msg_type,
                "task_started"
                    | "task_complete"
                    | "turn_aborted"
                    | "context_compacted"
                    | "token_count"
                    | "patch_apply_end"
                    | "web_search_end"
            ) {
                let body = codex_lifecycle_body(payload, msg_type);
                Some(codex_provider_event(
                    line_number,
                    occurred_at,
                    EventType::Notice,
                    Some(EventRole::System),
                    json!({
                        "entry_type": entry_type,
                        "event_msg_type": msg_type,
                        "body": body,
                    }),
                    json!({
                        "source": "codex_session",
                        "source_format": CODEX_SESSION_SOURCE_FORMAT,
                        "line": line_number,
                        "entry_type": entry_type,
                        "event_msg_type": msg_type,
                    }),
                ))
            } else {
                None
            }
        }
        _ => None,
    }
}
pub(crate) fn codex_response_item_event(
    payload: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    call_contexts: &mut BTreeMap<String, CodexToolCallContext>,
) -> Option<ProviderEventEnvelope> {
    let item_type = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    match item_type {
        "message" => codex_message_event(payload, line_number, occurred_at),
        "function_call" | "custom_tool_call" | "web_search_call" | "tool_search_call" => {
            codex_tool_call_event(payload, line_number, occurred_at, call_contexts)
        }
        "function_call_output" | "custom_tool_call_output" | "tool_search_output" => {
            codex_tool_output_event(payload, line_number, occurred_at, call_contexts)
        }
        "reasoning" => codex_reasoning_event(payload, line_number, occurred_at),
        _ => Some(codex_provider_event(
            line_number,
            occurred_at,
            EventType::Notice,
            None,
            json!({
                "item_type": item_type,
                "body": codex_capped_json(payload, PROVIDER_MAX_PREVIEW_CHARS),
            }),
            json!({
                "source": "codex_session",
                "source_format": CODEX_SESSION_SOURCE_FORMAT,
                "line": line_number,
                "item_type": item_type,
            }),
        )),
    }
}
pub(crate) fn codex_tool_call_event(
    payload: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    call_contexts: &mut BTreeMap<String, CodexToolCallContext>,
) -> Option<ProviderEventEnvelope> {
    let item_type = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("tool_call");
    let tool_name = codex_tool_name(payload, item_type);
    let call_id = payload.get("call_id").and_then(Value::as_str);
    let argument_value = payload
        .get("arguments")
        .or_else(|| payload.get("input"))
        .or_else(|| payload.get("action"))
        .or_else(|| payload.get("execution"));
    let command_preview = codex_command_preview(&tool_name, argument_value);
    let (arguments_preview, arguments_truncated, raw_arguments_retained) = argument_value
        .map(codex_tool_arguments_preview)
        .unwrap_or_else(|| (String::new(), false, false));
    let text = command_preview
        .as_ref()
        .map(|command| format!("{tool_name}: {command}"))
        .unwrap_or_else(|| {
            if arguments_preview.is_empty() {
                format!("{tool_name} tool call")
            } else {
                format!("{tool_name}: {arguments_preview}")
            }
        });
    let (text, text_truncated) = codex_local_preview(&text, PROVIDER_MAX_PREVIEW_CHARS);

    if let Some(call_id) = call_id {
        call_contexts.insert(
            call_id.to_owned(),
            CodexToolCallContext {
                tool_name: tool_name.clone(),
                command_preview: command_preview.clone(),
                arguments_preview: (!arguments_preview.is_empty())
                    .then_some(arguments_preview.clone()),
            },
        );
    }

    Some(codex_provider_event(
        line_number,
        occurred_at,
        EventType::ToolCall,
        Some(EventRole::Assistant),
        json!({
            "item_type": item_type,
            "tool": tool_name,
            "name": tool_name,
            "call_id": call_id,
            "command": command_preview,
            "arguments_preview": arguments_preview,
            "arguments_truncated": arguments_truncated,
            "raw_arguments_retained": raw_arguments_retained,
            "text": text,
            "truncated": text_truncated || arguments_truncated,
        }),
        json!({
            "source": "codex_session",
            "source_format": CODEX_SESSION_SOURCE_FORMAT,
            "line": line_number,
            "item_type": item_type,
            "tool": tool_name,
        }),
    ))
}
pub(crate) fn codex_tool_output_event(
    payload: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    call_contexts: &BTreeMap<String, CodexToolCallContext>,
) -> Option<ProviderEventEnvelope> {
    let item_type = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("tool_output");
    let call_id = payload.get("call_id").and_then(Value::as_str);
    let context = call_id.and_then(|call_id| call_contexts.get(call_id));
    let tool_name = context
        .map(|context| context.tool_name.clone())
        .unwrap_or_else(|| codex_tool_name(payload, item_type));
    let output_value = payload
        .get("output")
        .or_else(|| payload.get("tools"))
        .or_else(|| payload.get("result"));
    let output_text = output_value.map(codex_output_text);
    let command_preview = context.and_then(|context| context.command_preview.clone());
    let output_text_ref = output_text.as_deref();
    let exit_code = output_text_ref
        .and_then(codex_exit_code)
        .or_else(|| codex_output_exit_code(payload));
    let duration_ms = output_text_ref.and_then(codex_wall_time_ms);
    let output_bytes = output_text_ref.map(str::len).unwrap_or(0);
    let timed_out = codex_timed_out(payload).unwrap_or(false);
    let structured_failure = provider_output_event_is_failure(payload);
    if !timed_out && exit_code.is_none_or(|code| code == 0) && !structured_failure {
        return None;
    }
    let event_type = if codex_is_command_tool(&tool_name) {
        EventType::CommandOutput
    } else {
        EventType::ToolOutput
    };
    let retained_output_text = output_text_ref
        .map(|text| provider_output_preview_omitting_nested_patch_diff(payload, text));
    let (output_preview, output_truncated) = retained_output_text
        .as_deref()
        .map(|text| codex_local_preview(text, PROVIDER_MAX_PREVIEW_CHARS))
        .unwrap_or_else(|| (String::new(), false));
    let command = command_preview
        .as_deref()
        .map(|command| format!(" for `{command}`"))
        .unwrap_or_default();
    let status = exit_code
        .map(|code| format!("exit_code={code}"))
        .unwrap_or_else(|| "exit_code=unknown".to_owned());
    let duration = duration_ms
        .map(|ms| format!(", duration_ms={ms}"))
        .unwrap_or_default();
    let timeout = if timed_out { ", timed_out=true" } else { "" };
    let preview = if output_preview.is_empty() {
        String::new()
    } else {
        format!(": {output_preview}")
    };
    let text = format!(
        "{tool_name} output{command}: {status}{duration}, output_bytes={output_bytes}{timeout}{preview}"
    );
    let (text, text_truncated) = codex_local_preview(&text, PROVIDER_MAX_PREVIEW_CHARS);

    Some(codex_provider_event(
        line_number,
        occurred_at,
        event_type,
        Some(EventRole::Tool),
        json!({
            "item_type": item_type,
            "tool": tool_name,
            "name": tool_name,
            "call_id": call_id,
            "command": command_preview,
            "arguments_preview": context.and_then(|context| context.arguments_preview.clone()),
            "output_preview": output_preview,
            "output_retention": "failed_preview",
            "output_bytes": output_bytes,
            "output_truncated": output_truncated,
            "exit_code": exit_code,
            "duration_ms": duration_ms,
            "timed_out": timed_out,
            "text": text,
            "truncated": text_truncated || output_truncated,
        }),
        json!({
            "source": "codex_session",
            "source_format": CODEX_SESSION_SOURCE_FORMAT,
            "line": line_number,
            "item_type": item_type,
            "tool": tool_name,
        }),
    ))
}
pub(crate) fn codex_output_text(value: &Value) -> Cow<'_, str> {
    match value {
        Value::String(text) => Cow::Borrowed(text),
        Value::Null => Cow::Borrowed(""),
        other => Cow::Owned(serde_json::to_string(other).unwrap_or_else(|_| other.to_string())),
    }
}

fn codex_output_exit_code(value: &Value) -> Option<i32> {
    match value {
        Value::Object(object) => {
            for key in ["exit_code", "exitCode"] {
                if let Some(code) = object
                    .get(key)
                    .and_then(Value::as_i64)
                    .and_then(|code| i32::try_from(code).ok())
                {
                    return Some(code);
                }
            }
            object.values().find_map(codex_output_exit_code)
        }
        Value::Array(items) => items.iter().find_map(codex_output_exit_code),
        _ => None,
    }
}
pub(crate) fn codex_reasoning_event(
    payload: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
) -> Option<ProviderEventEnvelope> {
    let summary = payload
        .get("summary")
        .and_then(codex_content_text)
        .or_else(|| {
            payload
                .get("summary_text")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })?;
    let (summary, truncated) = codex_local_preview(&summary, PROVIDER_MAX_TEXT_CHARS);
    Some(codex_provider_event(
        line_number,
        occurred_at,
        EventType::Summary,
        Some(EventRole::Assistant),
        json!({
            "item_type": "reasoning",
            "summary": summary,
            "text": summary,
            "truncated": truncated,
            "encrypted_content_present": payload.get("encrypted_content").is_some(),
        }),
        json!({
            "source": "codex_session",
            "source_format": CODEX_SESSION_SOURCE_FORMAT,
            "line": line_number,
            "item_type": "reasoning",
        }),
    ))
}
pub(crate) fn codex_message_event(
    payload: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
) -> Option<ProviderEventEnvelope> {
    let role_text = payload
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if !matches!(role_text, "user" | "assistant" | "developer" | "system") {
        return None;
    }
    let text = payload.get("content").and_then(codex_content_text)?;
    let (text, truncated) = capped_text(&text, PROVIDER_MAX_TEXT_CHARS);
    Some(codex_provider_event(
        line_number,
        occurred_at,
        EventType::Message,
        Some(codex_event_role(role_text)),
        json!({
            "item_type": "message",
            "message_role": role_text,
            "phase": payload.get("phase").and_then(Value::as_str),
            "text": text,
            "truncated": truncated,
        }),
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
) -> ProviderEventEnvelope {
    ProviderEventEnvelope {
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
pub(crate) fn codex_lifecycle_body(payload: &Value, msg_type: &str) -> Value {
    let preview = payload
        .get("last_agent_message")
        .or_else(|| payload.get("message"))
        .or_else(|| payload.get("stdout"))
        .or_else(|| payload.get("stderr"))
        .and_then(codex_json_text)
        .unwrap_or_else(|| format!("Codex lifecycle: {msg_type}"));
    let (text, truncated) = codex_local_preview(&preview, PROVIDER_MAX_PREVIEW_CHARS);
    json!({
        "text": text,
        "event_msg_type": msg_type,
        "status": payload.get("status").and_then(Value::as_str),
        "success": payload.get("success").and_then(Value::as_bool),
        "duration_ms": payload.get("duration_ms").and_then(Value::as_i64),
        "time_to_first_token_ms": payload.get("time_to_first_token_ms").and_then(Value::as_i64),
        "truncated": truncated,
    })
}
pub(crate) fn codex_tool_name(payload: &Value, item_type: &str) -> String {
    payload
        .get("name")
        .or_else(|| payload.get("tool"))
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(item_type)
        .to_owned()
}
pub(crate) fn codex_is_command_tool(tool_name: &str) -> bool {
    matches!(tool_name, "exec_command" | "shell" | "bash" | "command")
}
pub(crate) fn codex_command_preview(
    tool_name: &str,
    argument_value: Option<&Value>,
) -> Option<String> {
    if !codex_is_command_tool(tool_name) {
        return None;
    }
    let value = argument_value?;
    let parsed = codex_parse_embedded_json(value).unwrap_or_else(|| value.clone());
    let command = parsed
        .get("cmd")
        .or_else(|| parsed.get("command"))
        .or_else(|| parsed.get("shell_command"))
        .and_then(Value::as_str)
        .or_else(|| value.as_str())?;
    Some(codex_local_preview(command, PROVIDER_MAX_PREVIEW_CHARS).0)
}
pub(crate) fn codex_value_preview(value: &Value, max_chars: usize) -> (String, bool) {
    let rendered = match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
    };
    codex_local_preview(&rendered, max_chars)
}
pub(crate) fn codex_tool_arguments_preview(value: &Value) -> (String, bool, bool) {
    let parsed = codex_parse_embedded_json(value).unwrap_or_else(|| value.clone());
    let mut file_touches = Vec::new();
    collect_patch_file_touches(&parsed, &mut file_touches);
    collect_structured_file_touches(&parsed, &mut file_touches);
    if !file_touches.is_empty() {
        return codex_file_touch_arguments_preview(&file_touches);
    }
    let (sanitized, redacted) = codex_sanitized_tool_argument_value(&parsed, None);
    let (preview, truncated) = codex_value_preview(&sanitized, PROVIDER_MAX_PREVIEW_CHARS);
    (preview, truncated, !redacted)
}
pub(crate) fn codex_file_touch_arguments_preview(
    file_touches: &[crate::provider::file_touches::FileTouchDraft],
) -> (String, bool, bool) {
    let paths = file_touches
        .iter()
        .take(12)
        .map(|touch| match touch.change_kind {
            Some(kind) => format!("{}:{}", kind.as_str(), touch.path),
            None => touch.path.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let omitted = file_touches.len().saturating_sub(12);
    let suffix = if omitted == 0 {
        String::new()
    } else {
        format!(", +{omitted} more")
    };
    (format!("file touches: {paths}{suffix}"), omitted > 0, false)
}
pub(crate) fn codex_sanitized_tool_argument_value(
    value: &Value,
    key: Option<&str>,
) -> (Value, bool) {
    if key.is_some_and(|key| codex_tool_argument_key_should_redact(key, value)) {
        return (codex_redacted_argument_value(value), true);
    }
    match value {
        Value::Array(items) => {
            let mut redacted = false;
            let items = items
                .iter()
                .map(|item| {
                    let (item, item_redacted) = codex_sanitized_tool_argument_value(item, key);
                    redacted |= item_redacted;
                    item
                })
                .collect();
            (Value::Array(items), redacted)
        }
        Value::Object(object) => {
            let mut redacted = false;
            let object = object
                .iter()
                .map(|(key, value)| {
                    let (value, value_redacted) =
                        codex_sanitized_tool_argument_value(value, Some(key));
                    redacted |= value_redacted;
                    (key.clone(), value)
                })
                .collect();
            (Value::Object(object), redacted)
        }
        _ => (value.clone(), false),
    }
}
pub(crate) fn codex_tool_argument_key_should_redact(key: &str, value: &Value) -> bool {
    let key = codex_normalized_key(key);
    matches!(
        key.as_str(),
        "content"
            | "text"
            | "body"
            | "diff"
            | "patch"
            | "oldstring"
            | "newstring"
            | "oldcontent"
            | "newcontent"
            | "beforecontent"
            | "aftercontent"
            | "beforetext"
            | "aftertext"
            | "replacement"
            | "oldstr"
            | "newstr"
            | "inputtext"
            | "outputtext"
    ) || (matches!(key.as_str(), "input" | "arguments" | "args" | "params")
        && codex_value_contains_patch_or_diff(value))
}
pub(crate) fn codex_redacted_argument_value(value: &Value) -> Value {
    json!({
        "content_retention": "metadata_only",
        "omitted_bytes": codex_value_approx_bytes(value),
        "contains_patch_or_diff": codex_value_contains_patch_or_diff(value),
    })
}
pub(crate) fn codex_normalized_key(key: &str) -> String {
    key.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
pub(crate) fn codex_value_approx_bytes(value: &Value) -> usize {
    match value {
        Value::String(text) => text.len(),
        _ => serde_json::to_string(value)
            .map(|text| text.len())
            .unwrap_or_default(),
    }
}
pub(crate) fn codex_value_contains_patch_or_diff(value: &Value) -> bool {
    match value {
        Value::String(text) => codex_text_contains_patch_or_diff(text),
        Value::Array(items) => items.iter().any(codex_value_contains_patch_or_diff),
        Value::Object(object) => object.values().any(codex_value_contains_patch_or_diff),
        _ => false,
    }
}
pub(crate) fn codex_text_contains_patch_or_diff(text: &str) -> bool {
    text.contains("*** Begin Patch")
        || text.contains("diff --git ")
        || text.starts_with("@@")
        || text.starts_with("+++ ")
        || text.starts_with("--- ")
        || text.contains("\n@@")
        || text.contains("\n+++ ")
        || text.contains("\n--- ")
}
pub(crate) fn codex_local_preview(value: &str, max_chars: usize) -> (String, bool) {
    capped_text(value, max_chars)
}
pub(crate) fn codex_parse_embedded_json(value: &Value) -> Option<Value> {
    match value {
        Value::String(text) => serde_json::from_str::<Value>(text).ok(),
        Value::Object(_) | Value::Array(_) => Some(value.clone()),
        _ => None,
    }
}
pub(crate) fn codex_timed_out(payload: &Value) -> Option<bool> {
    payload
        .get("timed_out")
        .and_then(Value::as_bool)
        .or_else(|| {
            payload
                .get("output")
                .and_then(codex_parse_embedded_json)
                .and_then(|value| {
                    value
                        .get("timed_out")
                        .and_then(Value::as_bool)
                        .or_else(|| value.pointer("/status/timed_out").and_then(Value::as_bool))
                })
        })
}
pub(crate) fn codex_exit_code(text: &str) -> Option<i32> {
    let marker = "Process exited with code ";
    let index = text.find(marker)? + marker.len();
    let tail = &text[index..];
    let digits = tail
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '-')
        .collect::<String>();
    digits.parse().ok()
}
pub(crate) fn codex_wall_time_ms(text: &str) -> Option<i64> {
    let marker = "Wall time: ";
    let index = text.find(marker)? + marker.len();
    let tail = &text[index..];
    let seconds_text = tail
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
        .collect::<String>();
    let seconds = seconds_text.parse::<f64>().ok()?;
    Some((seconds * 1000.0).round() as i64)
}
pub(crate) fn codex_event_role(role: &str) -> EventRole {
    match role {
        "user" => EventRole::User,
        "assistant" => EventRole::Assistant,
        "tool" => EventRole::Tool,
        "system" | "developer" => EventRole::System,
        _ => EventRole::Unknown,
    }
}
pub(crate) fn codex_content_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                if let Some(text) = block
                    .get("text")
                    .or_else(|| block.get("input_text"))
                    .or_else(|| block.get("output_text"))
                    .or_else(|| block.get("summary_text"))
                    .and_then(Value::as_str)
                {
                    parts.push(text.to_owned());
                    continue;
                }
                if let Some(text) = block.get("content").and_then(Value::as_str) {
                    parts.push(text.to_owned());
                    continue;
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        Value::Object(object) => {
            for key in [
                "text",
                "input_text",
                "output_text",
                "summary_text",
                "content",
            ] {
                if let Some(text) = object.get(key).and_then(Value::as_str) {
                    return Some(text.to_owned());
                }
                if let Some(text) = object.get(key).and_then(codex_content_text) {
                    return Some(text);
                }
            }
            None
        }
        _ => None,
    }
}
pub(crate) fn codex_json_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value).ok(),
        _ => Some(value.to_string()),
    }
}
pub(crate) fn codex_capped_json(value: &Value, max_chars: usize) -> Value {
    match value {
        Value::String(text) => {
            let (text, truncated) = capped_text(text, max_chars);
            json!({ "text": text, "truncated": truncated })
        }
        _ => {
            let rendered = serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned());
            let (text, truncated) = capped_text(&rendered, max_chars);
            json!({ "json": text, "truncated": truncated })
        }
    }
}
