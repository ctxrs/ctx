use std::{
    borrow::Cow,
    collections::{BTreeMap, VecDeque},
};

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
    provider_sources::{CODEX_RESUME_MAX_ENCODED_BYTES, CODEX_RESUME_MAX_PENDING_TOOL_CALLS},
    CaptureError, CodexSessionJsonlResumeState, CodexToolCallResumeContext, ProviderAdapterContext,
    ProviderFileTouchedEnvelope, Result, CODEX_SESSION_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
    PROVIDER_MAX_TEXT_CHARS,
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CodexToolCallContext {
    pub(crate) tool_name: String,
    pub(crate) command_preview: Option<String>,
    pub(crate) arguments_preview: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CodexToolCallContexts {
    by_id: BTreeMap<String, CodexToolCallContext>,
    insertion_order: VecDeque<String>,
    dropped_tool_calls: u64,
}

impl CodexToolCallContexts {
    pub(crate) fn from_resume_state(state: CodexSessionJsonlResumeState) -> Self {
        let mut contexts = Self {
            dropped_tool_calls: state.dropped_tool_calls,
            ..Self::default()
        };
        for context in state.pending_tool_calls {
            contexts.insertion_order.push_back(context.call_id.clone());
            contexts.by_id.insert(
                context.call_id,
                CodexToolCallContext {
                    tool_name: context.tool_name,
                    command_preview: context.command_preview,
                    arguments_preview: context.arguments_preview,
                },
            );
        }
        contexts
    }

    pub(crate) fn resume_state(&self) -> CodexSessionJsonlResumeState {
        CodexSessionJsonlResumeState::new(
            self.insertion_order
                .iter()
                .filter_map(|call_id| {
                    self.by_id
                        .get(call_id)
                        .map(|context| CodexToolCallResumeContext {
                            call_id: call_id.clone(),
                            tool_name: context.tool_name.clone(),
                            command_preview: context.command_preview.clone(),
                            arguments_preview: context.arguments_preview.clone(),
                        })
                })
                .collect(),
            self.dropped_tool_calls,
        )
    }

    fn insert(&mut self, call_id: String, context: CodexToolCallContext) {
        if self.by_id.remove(&call_id).is_some() {
            self.insertion_order.retain(|existing| existing != &call_id);
        }
        self.insertion_order.push_back(call_id.clone());
        self.by_id.insert(call_id, context);
        while self.by_id.len() > CODEX_RESUME_MAX_PENDING_TOOL_CALLS
            || self.resume_state().encoded_len().unwrap_or(usize::MAX)
                > CODEX_RESUME_MAX_ENCODED_BYTES
        {
            let Some(oldest) = self.insertion_order.pop_front() else {
                break;
            };
            if self.by_id.remove(&oldest).is_some() {
                self.dropped_tool_calls = self.dropped_tool_calls.saturating_add(1);
            }
        }
    }

    fn remove(&mut self, call_id: &str) -> Option<CodexToolCallContext> {
        let context = self.by_id.remove(call_id)?;
        self.insertion_order.retain(|existing| existing != call_id);
        Some(context)
    }

    pub(crate) fn clear(&mut self) {
        self.by_id.clear();
        self.insertion_order.clear();
        self.dropped_tool_calls = 0;
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.by_id.len()
    }
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
    codex_session_capture_with_source_format(
        header,
        event,
        line_number,
        occurred_at,
        context,
        CODEX_SESSION_SOURCE_FORMAT,
    )
}

pub(crate) fn codex_session_capture_with_source_format(
    header: &CodexSessionHeader,
    event: Option<ProviderEventEnvelope>,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    context: &ProviderAdapterContext,
    source_format: &str,
) -> ProviderCaptureEnvelope {
    let cursor = Some(ProviderCursorRange {
        before: None,
        after: Some(ProviderCursorCheckpoint {
            stream: provider_cursor_stream(CaptureProvider::Codex, source_format),
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
            source_format: source_format.to_owned(),
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
                "provider-source:codex:{source_format}:{}",
                header.id
            )),
            metadata: json!({
                "adapter": source_format,
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
                "source_format": source_format,
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
    pub(crate) source_format: &'a str,
}
pub(crate) fn codex_session_line_capture(
    header: &CodexSessionHeader,
    value: &Value,
    call_contexts: &mut CodexToolCallContexts,
    context: CodexSessionLineContext<'_>,
) -> CodexSessionLineCapture {
    let CodexSessionLineContext {
        line_number,
        occurred_at,
        raw_source_path,
        source_root,
        source_format,
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
            source_format,
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
    call_contexts: &mut CodexToolCallContexts,
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

pub(crate) fn codex_close_matching_tool_output_context(
    value: &Value,
    call_contexts: &mut CodexToolCallContexts,
) -> bool {
    if value.get("type").and_then(Value::as_str) != Some("response_item") {
        return false;
    }
    let Some(payload) = value.get("payload") else {
        return false;
    };
    if !matches!(
        payload.get("type").and_then(Value::as_str),
        Some("function_call_output" | "custom_tool_call_output" | "tool_search_output")
    ) {
        return false;
    }
    let Some(call_id) = payload.get("call_id").and_then(Value::as_str) else {
        return false;
    };
    call_contexts.remove(call_id).is_some()
}

pub(crate) fn codex_response_item_event(
    payload: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    call_contexts: &mut CodexToolCallContexts,
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
    call_contexts: &mut CodexToolCallContexts,
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

    if let Some(call_id) = call_id.filter(|call_id| !call_id.trim().is_empty()) {
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
    call_contexts: &mut CodexToolCallContexts,
) -> Option<ProviderEventEnvelope> {
    let item_type = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("tool_output");
    let call_id = payload.get("call_id").and_then(Value::as_str);
    let context = call_id.and_then(|call_id| call_contexts.remove(call_id));
    let tool_name = context
        .as_ref()
        .map(|context| context.tool_name.clone())
        .unwrap_or_else(|| codex_tool_name(payload, item_type));
    let output_value = payload
        .get("output")
        .or_else(|| payload.get("tools"))
        .or_else(|| payload.get("result"));
    let output_text = output_value.map(codex_output_text);
    let command_preview = context
        .as_ref()
        .and_then(|context| context.command_preview.clone());
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
            "arguments_preview": context.as_ref().and_then(|context| context.arguments_preview.clone()),
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

include!("events/payload.rs");

#[cfg(test)]
include!("events/tests.rs");
