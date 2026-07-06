use std::{
    collections::BTreeMap,
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderCursorCheckpoint, ProviderCursorRange, ProviderEventEnvelope, ProviderRawRetention,
    ProviderRedactionBoundary, ProviderSessionEnvelope, ProviderSourceEnvelope,
    ProviderSourceTrust, RedactionState, SessionStatus, PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use serde_json::{json, Value};

use crate::common::io::{
    collect_jsonl_paths, ensure_regular_provider_transcript_file, read_provider_jsonl_line,
};
use crate::common::time::parse_rfc3339_utc;
use crate::provider::file_touches::provider_file_touches_from_raw_value;
use crate::provider::importer::provider_cursor_stream;
use crate::provider::native::{
    antigravity_tool_call_text, provider_capped_json, provider_capped_json_value,
    provider_local_preview, provider_role, provider_value_text,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportFailure, ProviderNormalizationResult,
    Result, PROVIDER_MAX_PREVIEW_CHARS, PROVIDER_MAX_TEXT_CHARS,
};

mod windsurf;

pub(crate) use windsurf::{windsurf_event_text, windsurf_redacted_body};

pub(crate) fn normalize_jsonl_tree(
    path: &Path,
    context: &ProviderAdapterContext,
    provider: CaptureProvider,
    source_format: &'static str,
) -> Result<ProviderNormalizationResult> {
    let mut paths = Vec::new();
    collect_jsonl_paths(path, &mut paths)?;
    paths.retain(|path| provider_jsonl_path_is_native(provider, path));
    if provider == CaptureProvider::Antigravity {
        paths = antigravity_preferred_transcript_paths(paths);
    }
    paths.sort();
    if paths.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: native_jsonl_missing_reason(provider),
        });
    }

    let mut merged = ProviderNormalizationResult::default();
    for path in paths {
        match normalize_native_jsonl_session_file(&path, context, provider, source_format) {
            Ok(mut result) => {
                merged.summary.merge(result.summary);
                merged.captures.append(&mut result.captures);
                merged.files_touched.append(&mut result.files_touched);
            }
            Err(err) => return Err(err),
        }
    }
    Ok(merged)
}

pub(crate) fn native_jsonl_missing_reason(provider: CaptureProvider) -> &'static str {
    match provider {
        CaptureProvider::Pi => "no Pi session JSONL files found",
        CaptureProvider::Antigravity => {
            "no Antigravity transcript JSONL files found under brain/*/.system_generated/logs"
        }
        CaptureProvider::Gemini => "no Gemini CLI chat JSONL transcripts found under chats",
        CaptureProvider::Tabnine => "no Tabnine CLI chat JSONL transcripts found under chats",
        CaptureProvider::Cursor => {
            "no Cursor agent transcript JSONL files found under projects/*/agent-transcripts"
        }
        CaptureProvider::Windsurf => {
            "no Windsurf Cascade hook transcript JSONL files found under ~/.windsurf/transcripts"
        }
        CaptureProvider::Qoder => {
            "no Qoder transcript JSONL files found under ~/.qoder/projects/*/transcript"
        }
        CaptureProvider::CopilotCli => "no Copilot CLI session events.jsonl transcripts found",
        CaptureProvider::FactoryAiDroid => "no Factory AI Droid session JSONL transcripts found",
        CaptureProvider::QwenCode => "no Qwen Code chat JSONL transcripts found under chats",
        CaptureProvider::KimiCodeCli => "no Kimi Code CLI wire.jsonl transcripts found",
        CaptureProvider::MistralVibe => {
            "no Mistral Vibe meta.json/messages.jsonl session directories found"
        }
        CaptureProvider::Mux => "no Mux chat.jsonl or partial.json session files found",
        _ => "no native provider JSONL transcripts found",
    }
}

pub(crate) fn provider_jsonl_path_is_native(provider: CaptureProvider, path: &Path) -> bool {
    match provider {
        CaptureProvider::Antigravity => {
            matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("transcript_full.jsonl" | "transcript.jsonl")
            )
        }
        CaptureProvider::Gemini | CaptureProvider::Tabnine => path
            .components()
            .any(|component| component.as_os_str() == "chats"),
        CaptureProvider::Cursor => path
            .components()
            .any(|component| component.as_os_str() == "agent-transcripts"),
        CaptureProvider::Windsurf => path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"),
        CaptureProvider::Qoder => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "transcript")
        }
        CaptureProvider::CopilotCli => {
            path.file_name().and_then(|name| name.to_str()) == Some("events.jsonl")
        }
        CaptureProvider::QwenCode => path
            .components()
            .any(|component| component.as_os_str() == "chats"),
        CaptureProvider::KimiCodeCli => {
            path.file_name().and_then(|name| name.to_str()) == Some("wire.jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "agents")
        }
        _ => true,
    }
}

pub(crate) fn antigravity_preferred_transcript_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut by_session: BTreeMap<String, PathBuf> = BTreeMap::new();
    for path in paths {
        let session =
            antigravity_session_id_from_path(&path).unwrap_or_else(|| path.display().to_string());
        let prefer_new =
            path.file_name().and_then(|name| name.to_str()) == Some("transcript_full.jsonl");
        let replace = by_session
            .get(&session)
            .map(|current| {
                prefer_new
                    && current.file_name().and_then(|name| name.to_str())
                        != Some("transcript_full.jsonl")
            })
            .unwrap_or(true);
        if replace {
            by_session.insert(session, path);
        }
    }
    by_session.into_values().collect()
}

pub(crate) fn normalize_native_jsonl_session_file(
    path: &Path,
    context: &ProviderAdapterContext,
    provider: CaptureProvider,
    source_format: &'static str,
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
        rows.push((line_number, value));
    }

    let header_index = if matches!(
        provider,
        CaptureProvider::Antigravity | CaptureProvider::Windsurf
    ) {
        if rows.is_empty() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: native_jsonl_missing_reason(provider),
            });
        }
        0
    } else {
        if rows.is_empty() {
            return Ok(result);
        }
        let Some(header_index) = rows
            .iter()
            .position(|(_, value)| native_jsonl_header_session_id(provider, value).is_some())
        else {
            if let Some((line_number, _)) = rows.first() {
                result.summary.failed += 1;
                result.summary.failures.push(ProviderImportFailure {
                    line: *line_number,
                    error: "no importable native JSONL session header".to_owned(),
                });
                return Ok(result);
            }
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: native_jsonl_missing_reason(provider),
            });
        };
        header_index
    };

    let header = rows[header_index].1.clone();
    let native_session_id = match provider {
        CaptureProvider::Antigravity => {
            antigravity_session_id_from_path(path).unwrap_or_else(|| "unknown-session".to_owned())
        }
        CaptureProvider::Windsurf => {
            windsurf_session_id_from_path(path).unwrap_or_else(|| "unknown-session".to_owned())
        }
        _ => native_jsonl_header_session_id(provider, &header)
            .unwrap_or_else(|| "unknown-session".to_owned()),
    };
    let (provider_session_id, parent_provider_session_id, external_agent_id, agent_type) =
        native_jsonl_path_session(provider, path, &header, &native_session_id);
    let started_at = native_jsonl_timestamp(&header)
        .or_else(|| native_jsonl_header_start_time(provider, &header))
        .unwrap_or(context.imported_at);
    let cwd = native_jsonl_header_cwd(provider, &header);
    let is_subagent = parent_provider_session_id.is_some() || agent_type == AgentType::Subagent;
    let raw_source_path = path.display().to_string();

    for (line_number, value) in rows {
        let occurred_at = native_jsonl_timestamp(&value).unwrap_or(started_at);
        let event = native_jsonl_event(provider, source_format, &value, line_number, occurred_at);
        if let Some(event) = &event {
            result
                .files_touched
                .extend(provider_file_touches_from_raw_value(
                    provider,
                    &provider_session_id,
                    source_format,
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
                provider,
                source: ProviderSourceEnvelope {
                    source_format: source_format.to_owned(),
                    machine_id: context.machine_id.clone(),
                    observed_at: context.imported_at,
                    raw_source_path: Some(raw_source_path.clone()),
                    source_root: context
                        .source_root_display()
                        .or_else(|| Some(raw_source_path.clone())),
                    raw_retention: ProviderRawRetention::PathReference,
                    redaction_boundary: ProviderRedactionBoundary::BeforeExport,
                    trust: ProviderSourceTrust::ProviderNative,
                    fidelity: Fidelity::Imported,
                    cursor: Some(ProviderCursorRange {
                        before: None,
                        after: Some(ProviderCursorCheckpoint {
                            stream: provider_cursor_stream(provider, source_format),
                            cursor: format!("{}:line:{line_number}", path.display()),
                            observed_at: occurred_at,
                        }),
                    }),
                    idempotency_key: Some(format!(
                        "provider-source:{}:{source_format}:{provider_session_id}",
                        provider.as_str()
                    )),
                    metadata: json!({
                        "adapter": source_format,
                        "native_session_id": native_session_id,
                        "source_path": raw_source_path.clone(),
                    }),
                },
                session: ProviderSessionEnvelope {
                    provider_session_id: provider_session_id.clone(),
                    parent_provider_session_id: parent_provider_session_id.clone(),
                    root_provider_session_id: parent_provider_session_id.clone(),
                    external_agent_id: external_agent_id.clone(),
                    agent_type,
                    role_hint: Some(if is_subagent { "subagent" } else { "primary" }.to_owned()),
                    is_primary: !is_subagent,
                    status: native_jsonl_session_status(provider, &header),
                    started_at,
                    ended_at: None,
                    cwd: cwd.clone(),
                    fidelity: Fidelity::Imported,
                    idempotency_key: Some(format!(
                        "provider-session:{}:{provider_session_id}",
                        provider.as_str()
                    )),
                    artifacts: Vec::new(),
                    metadata: native_jsonl_session_metadata(provider, source_format, &header, path),
                },
                event,
            },
        ));
    }

    Ok(result)
}

pub(crate) fn native_jsonl_header_session_id(
    provider: CaptureProvider,
    value: &Value,
) -> Option<String> {
    match provider {
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            value.get("sessionId").and_then(Value::as_str)
        }
        CaptureProvider::FactoryAiDroid => (value.get("type").and_then(Value::as_str)
            == Some("session_start"))
        .then(|| value.get("sessionId").and_then(Value::as_str))
        .flatten(),
        CaptureProvider::CopilotCli => (value.get("type").and_then(Value::as_str)
            == Some("session.start"))
        .then(|| value.pointer("/data/sessionId").and_then(Value::as_str))
        .flatten(),
        CaptureProvider::QwenCode => value.get("sessionId").and_then(Value::as_str),
        CaptureProvider::Qoder => value.get("sessionId").and_then(Value::as_str),
        CaptureProvider::Cursor => (value.get("role").is_some()
            || value.get("event").is_some()
            || value.get("message").is_some())
        .then_some("cursor-path-session"),
        _ => None,
    }
    .filter(|id| !id.trim().is_empty())
    .map(str::to_owned)
}

pub(crate) fn native_jsonl_header_start_time(
    provider: CaptureProvider,
    value: &Value,
) -> Option<DateTime<Utc>> {
    match provider {
        CaptureProvider::Antigravity => value.get("created_at").and_then(Value::as_str),
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            value.get("startTime").and_then(Value::as_str)
        }
        CaptureProvider::CopilotCli => value.pointer("/data/startTime").and_then(Value::as_str),
        _ => None,
    }
    .and_then(parse_rfc3339_utc)
}

pub(crate) fn native_jsonl_header_cwd(provider: CaptureProvider, value: &Value) -> Option<String> {
    match provider {
        CaptureProvider::Gemini | CaptureProvider::Tabnine => value
            .get("directories")
            .and_then(Value::as_array)
            .and_then(|dirs| dirs.first())
            .and_then(Value::as_str),
        CaptureProvider::FactoryAiDroid => value.get("cwd").and_then(Value::as_str),
        CaptureProvider::CopilotCli => value.pointer("/data/context/cwd").and_then(Value::as_str),
        CaptureProvider::QwenCode => value.get("cwd").and_then(Value::as_str),
        CaptureProvider::Qoder => value.get("cwd").and_then(Value::as_str),
        _ => None,
    }
    .filter(|cwd| !cwd.trim().is_empty())
    .map(str::to_owned)
}

pub(crate) fn native_jsonl_path_session(
    provider: CaptureProvider,
    path: &Path,
    header: &Value,
    native_session_id: &str,
) -> (String, Option<String>, Option<String>, AgentType) {
    match provider {
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            let parent = path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str());
            if parent.is_some_and(|name| name != "chats") {
                return (
                    native_session_id.to_owned(),
                    parent.map(str::to_owned),
                    None,
                    AgentType::Subagent,
                );
            }
            (native_session_id.to_owned(), None, None, AgentType::Primary)
        }
        CaptureProvider::FactoryAiDroid => {
            let parent = header
                .get("parent")
                .or_else(|| header.get("callingSessionId"))
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .map(str::to_owned);
            let agent_type = if parent.is_some()
                || header.get("decompSessionType").and_then(Value::as_str) == Some("worker")
            {
                AgentType::Subagent
            } else {
                AgentType::Primary
            };
            (
                native_session_id.to_owned(),
                parent,
                header
                    .get("decompMissionId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                agent_type,
            )
        }
        CaptureProvider::Cursor => {
            let session = path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .unwrap_or(native_session_id)
                .to_owned();
            (session, None, None, AgentType::Primary)
        }
        _ => (native_session_id.to_owned(), None, None, AgentType::Primary),
    }
}

pub(crate) fn antigravity_session_id_from_path(path: &Path) -> Option<String> {
    let components: Vec<String> = path
        .components()
        .filter_map(|component| component.as_os_str().to_str().map(str::to_owned))
        .collect();
    components
        .windows(2)
        .find_map(|window| {
            (window[0] == "brain" && !window[1].trim().is_empty()).then(|| window[1].clone())
        })
        .or_else(|| {
            components.windows(2).find_map(|window| {
                (window[1] == ".system_generated" && !window[0].trim().is_empty())
                    .then(|| window[0].clone())
            })
        })
        .or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.trim().is_empty())
                .map(str::to_owned)
        })
}

pub(crate) fn windsurf_session_id_from_path(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.trim().is_empty())
        .map(str::to_owned)
}

pub(crate) fn native_jsonl_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_utc)
        .or_else(|| {
            value
                .get("created_at")
                .and_then(Value::as_str)
                .and_then(parse_rfc3339_utc)
        })
        .or_else(|| {
            value
                .pointer("/time/created")
                .and_then(Value::as_i64)
                .and_then(DateTime::<Utc>::from_timestamp_millis)
        })
}

pub(crate) fn native_jsonl_session_status(
    provider: CaptureProvider,
    header: &Value,
) -> SessionStatus {
    if provider == CaptureProvider::CopilotCli
        && header.get("type").and_then(Value::as_str) == Some("abort")
    {
        SessionStatus::Interrupted
    } else {
        SessionStatus::Imported
    }
}

pub(crate) fn native_jsonl_session_metadata(
    provider: CaptureProvider,
    source_format: &str,
    header: &Value,
    path: &Path,
) -> Value {
    json!({
        "source_format": source_format,
        "provider": provider.as_str(),
        "source_path": path.display().to_string(),
        "header": provider_capped_json(header, PROVIDER_MAX_PREVIEW_CHARS),
    })
}

pub(crate) fn native_jsonl_event(
    provider: CaptureProvider,
    source_format: &str,
    value: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
) -> Option<ProviderEventEnvelope> {
    let event_type = native_jsonl_event_type(provider, value);
    let entry_type = native_jsonl_entry_type(provider, value);
    let role = native_jsonl_role(provider, value);
    let text = native_jsonl_event_text(provider, value, event_type, &entry_type);
    let (text, truncated) = provider_local_preview(&text, PROVIDER_MAX_TEXT_CHARS);
    let event_id = native_jsonl_event_id(provider, value, line_number);
    let tool_calls = if provider == CaptureProvider::Antigravity {
        value
            .get("tool_calls")
            .map(|calls| provider_capped_json_value(calls, PROVIDER_MAX_PREVIEW_CHARS))
    } else {
        None
    };
    let body = if provider == CaptureProvider::Windsurf {
        windsurf_redacted_body(value)
    } else {
        provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS)
    };

    Some(ProviderEventEnvelope {
        provider_event_index: (line_number - 1) as u64,
        provider_event_hash: Some(event_id.clone()),
        cursor: Some(event_id.clone()),
        event_type,
        role: Some(role),
        occurred_at,
        fidelity: Fidelity::Imported,
        redaction_state: RedactionState::LocalPreview,
        idempotency_key: Some(format!(
            "provider-event:{}:{source_format}:{event_id}",
            provider.as_str()
        )),
        artifacts: Vec::new(),
        payload: json!({
            "entry_type": entry_type,
            "event_id": event_id,
            "native_step_index": value.get("step_index").and_then(Value::as_u64),
            "text": text,
            "truncated": truncated,
            "tool_calls": tool_calls,
            "body": body,
        }),
        metadata: json!({
            "source": source_format,
            "source_format": source_format,
            "line": line_number,
            "entry_type": entry_type,
            "status": value.get("status").and_then(Value::as_str),
            "model": native_jsonl_model(provider, value),
            "tokens": native_jsonl_tokens(provider, value),
        }),
    })
}

pub(crate) fn native_jsonl_event_id(
    provider: CaptureProvider,
    value: &Value,
    line_number: usize,
) -> String {
    if provider == CaptureProvider::Antigravity {
        if let Some(step_index) = value.get("step_index").and_then(Value::as_u64) {
            return format!("step-{step_index}");
        }
    }
    value
        .get("id")
        .or_else(|| value.get("uuid"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("line-{line_number}"))
}

pub(crate) fn native_jsonl_entry_type(provider: CaptureProvider, value: &Value) -> String {
    match provider {
        CaptureProvider::Antigravity => value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            if value.get("$set").is_some() {
                "$set"
            } else if value.get("$rewindTo").is_some() {
                "$rewindTo"
            } else {
                value
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            }
        }
        _ => value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
    }
    .to_owned()
}

pub(crate) fn native_jsonl_event_type(provider: CaptureProvider, value: &Value) -> EventType {
    match provider {
        CaptureProvider::Antigravity => match value.get("type").and_then(Value::as_str) {
            Some("USER_INPUT" | "CONVERSATION_HISTORY") => EventType::Message,
            Some("PLANNER_RESPONSE") => {
                if value.get("tool_calls").is_some() {
                    EventType::ToolCall
                } else {
                    EventType::Message
                }
            }
            Some("CODE_ACTION") => EventType::ToolCall,
            Some("CHECKPOINT") => EventType::Summary,
            Some("SYSTEM_MESSAGE") => EventType::Notice,
            _ => EventType::Notice,
        },
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            if value.get("$set").is_some() || value.get("$rewindTo").is_some() {
                EventType::Notice
            } else if value.get("toolCalls").is_some() {
                if gemini_tool_calls_have_result(value) {
                    EventType::ToolOutput
                } else {
                    EventType::ToolCall
                }
            } else {
                match value.get("type").and_then(Value::as_str) {
                    Some("user" | "gemini" | "tabnine") => EventType::Message,
                    _ => EventType::Notice,
                }
            }
        }
        CaptureProvider::FactoryAiDroid => match value.get("type").and_then(Value::as_str) {
            Some("message") if droid_content_has(value, "tool_use") => EventType::ToolCall,
            Some("message") if droid_content_has(value, "tool_result") => EventType::ToolOutput,
            Some("message") => EventType::Message,
            Some("compaction_state") => EventType::Summary,
            Some("todo_state" | "session_start") => EventType::Notice,
            _ => EventType::Notice,
        },
        CaptureProvider::CopilotCli => match value.get("type").and_then(Value::as_str) {
            Some("user.message" | "assistant.message") => EventType::Message,
            Some("tool.execution_start") => EventType::ToolCall,
            Some("tool.execution_complete") => EventType::ToolOutput,
            Some("session.truncation") => EventType::Summary,
            Some("abort") => EventType::Notice,
            _ => EventType::Notice,
        },
        CaptureProvider::Cursor => {
            if native_jsonl_content_has(value, "tool_result") {
                EventType::ToolOutput
            } else if native_jsonl_content_has(value, "tool_use") {
                EventType::ToolCall
            } else {
                match value
                    .get("event")
                    .or_else(|| value.get("type"))
                    .or_else(|| value.get("role"))
                    .and_then(Value::as_str)
                {
                    Some("turn_ended" | "summary") => EventType::Summary,
                    Some("user" | "assistant") => EventType::Message,
                    _ => EventType::Notice,
                }
            }
        }
        CaptureProvider::Windsurf => match value.get("type").and_then(Value::as_str) {
            Some("user_input" | "planner_response") => EventType::Message,
            Some("code_action") => EventType::ToolCall,
            Some("summary" | "checkpoint") => EventType::Summary,
            _ => EventType::Notice,
        },
        CaptureProvider::Qoder => match value.get("type").and_then(Value::as_str) {
            Some("assistant") if native_jsonl_content_has(value, "tool_use") => EventType::ToolCall,
            Some("user") if native_jsonl_content_has(value, "tool_result") => EventType::ToolOutput,
            Some("user" | "assistant") => EventType::Message,
            Some("progress") => EventType::Notice,
            Some("session_meta") => EventType::Notice,
            _ if value.get("toolUseResult").is_some() => EventType::ToolOutput,
            _ => EventType::Notice,
        },
        CaptureProvider::QwenCode => match value.get("type").and_then(Value::as_str) {
            Some("user" | "assistant") if native_jsonl_content_has(value, "tool_use") => {
                EventType::ToolCall
            }
            Some("tool_result") => EventType::ToolOutput,
            Some("user" | "assistant") => EventType::Message,
            Some("system") => EventType::Notice,
            _ if value.get("toolCallResult").is_some() => EventType::ToolOutput,
            _ => EventType::Notice,
        },
        _ => EventType::Notice,
    }
}

pub(crate) fn native_jsonl_role(provider: CaptureProvider, value: &Value) -> EventRole {
    match provider {
        CaptureProvider::Antigravity => match value.get("source").and_then(Value::as_str) {
            Some("user") => EventRole::User,
            Some("planner" | "agent" | "assistant") => EventRole::Assistant,
            Some("tool" | "executor") => EventRole::Tool,
            Some("system") => EventRole::System,
            _ => match value.get("type").and_then(Value::as_str) {
                Some("USER_INPUT") => EventRole::User,
                Some("SYSTEM_MESSAGE" | "CHECKPOINT") => EventRole::System,
                _ => EventRole::Assistant,
            },
        },
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            match value.get("type").and_then(Value::as_str) {
                Some("user") => EventRole::User,
                Some("gemini" | "tabnine") => EventRole::Assistant,
                _ => EventRole::System,
            }
        }
        CaptureProvider::FactoryAiDroid => provider_role(value.get("role").and_then(Value::as_str)),
        CaptureProvider::CopilotCli => match value.get("type").and_then(Value::as_str) {
            Some("user.message") => EventRole::User,
            Some("assistant.message") => EventRole::Assistant,
            Some("tool.execution_start" | "tool.execution_complete") => EventRole::Tool,
            _ => EventRole::System,
        },
        CaptureProvider::Cursor => provider_role(
            value
                .get("role")
                .or_else(|| value.pointer("/message/role"))
                .and_then(Value::as_str),
        ),
        CaptureProvider::Windsurf => match value.get("type").and_then(Value::as_str) {
            Some("user_input") => EventRole::User,
            Some("planner_response") => EventRole::Assistant,
            Some("code_action") => EventRole::Tool,
            _ => EventRole::Unknown,
        },
        CaptureProvider::Qoder => provider_role(
            value
                .pointer("/message/role")
                .or_else(|| value.get("type"))
                .and_then(Value::as_str),
        ),
        CaptureProvider::QwenCode => provider_role(
            value
                .pointer("/message/role")
                .or_else(|| value.get("type"))
                .and_then(Value::as_str),
        ),
        _ => EventRole::Unknown,
    }
}

pub(crate) fn native_jsonl_event_text(
    provider: CaptureProvider,
    value: &Value,
    event_type: EventType,
    entry_type: &str,
) -> String {
    match provider {
        CaptureProvider::Antigravity => value
            .get("content")
            .and_then(provider_value_text)
            .map(|content| {
                value
                    .get("tool_calls")
                    .and_then(antigravity_tool_call_text)
                    .map(|tools| format!("{content}\n{tools}"))
                    .unwrap_or(content)
            })
            .or_else(|| value.get("thinking").and_then(provider_value_text))
            .or_else(|| value.get("tool_calls").and_then(antigravity_tool_call_text))
            .unwrap_or_else(|| format!("Antigravity event: {entry_type}")),
        CaptureProvider::Gemini | CaptureProvider::Tabnine => value
            .get("content")
            .and_then(provider_value_text)
            .or_else(|| value.get("toolCalls").and_then(provider_value_text))
            .or_else(|| value.get("$set").and_then(provider_value_text))
            .or_else(|| {
                value
                    .get("$rewindTo")
                    .and_then(Value::as_str)
                    .map(|id| format!("rewind to {id}"))
            })
            .unwrap_or_else(|| {
                let name = if provider == CaptureProvider::Tabnine {
                    "Tabnine"
                } else {
                    "Gemini"
                };
                format!("{name} event: {entry_type}")
            }),
        CaptureProvider::FactoryAiDroid => value
            .get("content")
            .and_then(provider_value_text)
            .or_else(|| {
                value
                    .get("summary")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| value.get("items").and_then(provider_value_text))
            .unwrap_or_else(|| format!("Factory AI Droid event: {entry_type}")),
        CaptureProvider::CopilotCli => value
            .pointer("/data/content")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                value
                    .pointer("/data/result/content")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| {
                value
                    .pointer("/data/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| {
                value
                    .pointer("/data/toolName")
                    .and_then(Value::as_str)
                    .map(|tool| format!("tool {tool}"))
            })
            .unwrap_or_else(|| format!("Copilot CLI event: {entry_type}")),
        CaptureProvider::Cursor => value
            .pointer("/message/content")
            .or_else(|| value.get("content"))
            .and_then(provider_value_text)
            .or_else(|| value.get("text").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| format!("Cursor event: {entry_type}")),
        CaptureProvider::Windsurf => windsurf_event_text(value, entry_type),
        CaptureProvider::Qoder => {
            let primary = if event_type == EventType::ToolOutput {
                value
                    .get("toolUseResult")
                    .or_else(|| value.pointer("/message/content"))
            } else {
                value
                    .pointer("/message/content")
                    .or_else(|| value.get("toolUseResult"))
            };
            primary
                .or_else(|| value.pointer("/data/content"))
                .and_then(provider_value_text)
                .unwrap_or_else(|| format!("Qoder event: {entry_type}"))
        }
        CaptureProvider::QwenCode => value
            .pointer("/message/content")
            .or_else(|| value.get("message"))
            .and_then(provider_value_text)
            .or_else(|| value.get("toolCallResult").and_then(provider_value_text))
            .or_else(|| value.get("content").and_then(provider_value_text))
            .unwrap_or_else(|| format!("Qwen Code event: {entry_type}")),
        _ => serde_json::to_string(value).unwrap_or_else(|_| entry_type.to_owned()),
    }
}

pub(crate) fn native_jsonl_model(provider: CaptureProvider, value: &Value) -> Option<Value> {
    match provider {
        CaptureProvider::Antigravity => value.get("model").cloned(),
        CaptureProvider::Gemini | CaptureProvider::Tabnine => value.get("model").cloned(),
        CaptureProvider::FactoryAiDroid => value
            .get("model")
            .cloned()
            .or_else(|| value.pointer("/metadata/model").cloned()),
        CaptureProvider::CopilotCli => value.pointer("/data/selectedModel").cloned(),
        CaptureProvider::QwenCode => value
            .get("model")
            .cloned()
            .or_else(|| value.pointer("/message/model").cloned()),
        CaptureProvider::Qoder => value
            .get("model")
            .cloned()
            .or_else(|| value.pointer("/message/model").cloned()),
        _ => None,
    }
}

pub(crate) fn native_jsonl_tokens(_provider: CaptureProvider, value: &Value) -> Option<Value> {
    value
        .get("tokens")
        .or_else(|| value.get("usageMetadata"))
        .cloned()
}

pub(crate) fn gemini_tool_calls_have_result(value: &Value) -> bool {
    value
        .get("toolCalls")
        .and_then(Value::as_array)
        .map(|calls| calls.iter().any(|call| call.get("result").is_some()))
        .unwrap_or(false)
}

pub(crate) fn droid_content_has(value: &Value, expected: &str) -> bool {
    value
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some(expected))
        })
        .unwrap_or(false)
}

pub(crate) fn native_jsonl_content_has(value: &Value, expected: &str) -> bool {
    value
        .pointer("/message/content")
        .or_else(|| value.get("content"))
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some(expected))
        })
        .unwrap_or(false)
}
