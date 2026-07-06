use std::{
    collections::BTreeMap,
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderEventEnvelope, ProviderSourceTrust,
};
use serde_json::{json, Value};

use crate::provider::providers::native_jsonl::native_jsonl_missing_reason;
use crate::provider::providers::openclaw::provider_path_has_component;

use crate::common::io::{
    collect_jsonl_paths, ensure_regular_provider_transcript_file, read_provider_jsonl_line,
    read_text_file_limited,
};
use crate::common::time::parse_rfc3339_utc;
use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::file_touches::provider_file_touches_from_raw_value;
use crate::provider::native::{
    native_event, native_provider_capture, provider_capped_json, provider_role,
    provider_timestamp_seconds_to_datetime, provider_value_text, NativeEventDraft,
    NativeSessionDraft,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    KIMI_CODE_CLI_SOURCE_FORMAT, MAX_PROVIDER_JSONL_LINE_BYTES, PROVIDER_MAX_PREVIEW_CHARS,
};

pub(crate) struct KimiSessionIndexEntry {
    pub(crate) session_id: String,
    pub(crate) session_dir: Option<String>,
    pub(crate) work_dir: Option<String>,
}

pub(crate) fn normalize_kimi_code_cli_history(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let mut paths = Vec::new();
    collect_jsonl_paths(path, &mut paths)?;
    paths.retain(|candidate| {
        candidate.file_name().and_then(|name| name.to_str()) == Some("wire.jsonl")
            && provider_path_has_component(candidate, "agents")
    });
    paths.sort();
    if paths.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: native_jsonl_missing_reason(CaptureProvider::KimiCodeCli),
        });
    }

    let index = kimi_session_index(path);
    let mut merged = ProviderNormalizationResult::default();
    for wire_path in paths {
        let mut result = normalize_kimi_wire_jsonl_file(&wire_path, context, &index)?;
        merged.summary.merge(result.summary);
        merged.captures.append(&mut result.captures);
        merged.files_touched.append(&mut result.files_touched);
    }
    Ok(merged)
}

pub(crate) fn normalize_kimi_wire_jsonl_file(
    path: &Path,
    context: &ProviderAdapterContext,
    index: &BTreeMap<String, KimiSessionIndexEntry>,
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
                push_provider_import_failure(
                    &mut result.summary,
                    line_number,
                    format!("malformed JSONL: {err}"),
                );
                continue;
            }
        };
        rows.push((line_number, value));
    }

    let Some((session_dir, agent_id, session_id)) = kimi_wire_path_parts(path) else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Kimi Code CLI wire path must be sessions/<workDirKey>/<sessionId>/agents/<agentId>/wire.jsonl",
        });
    };
    let state = read_kimi_state(&session_dir);
    let index_entry = index.get(&session_id);
    let metadata_created_at = rows
        .iter()
        .find(|(_, value)| value.get("type").and_then(Value::as_str) == Some("metadata"))
        .and_then(|(_, value)| value.get("created_at"))
        .and_then(Value::as_i64)
        .and_then(DateTime::<Utc>::from_timestamp_millis);
    let first_row_time = rows
        .iter()
        .find_map(|(_, value)| kimi_record_timestamp(value, context.imported_at));
    let started_at = kimi_state_timestamp(&state, &["createdAt", "created_at"])
        .or(metadata_created_at)
        .or(first_row_time)
        .unwrap_or(context.imported_at);
    let ended_at = kimi_state_timestamp(&state, &["updatedAt", "updated_at"]);
    let raw_source_path = path.display().to_string();
    let agent_state = state
        .get("agents")
        .and_then(|agents| agents.get(&agent_id))
        .cloned()
        .unwrap_or(Value::Null);
    let (provider_session_id, parent_provider_session_id, root_provider_session_id, agent_type) =
        kimi_provider_session_ids(&session_id, &agent_id, &agent_state);
    let cwd = index_entry
        .and_then(|entry| entry.work_dir.clone())
        .or_else(|| {
            state
                .get("workDir")
                .or_else(|| state.get("cwd"))
                .and_then(Value::as_str)
                .filter(|cwd| !cwd.trim().is_empty())
                .map(str::to_owned)
        });

    result.captures.push((
        0,
        kimi_capture(
            KimiCaptureDraft {
                provider_session_id: &provider_session_id,
                parent_provider_session_id: parent_provider_session_id.clone(),
                root_provider_session_id: root_provider_session_id.clone(),
                agent_id: &agent_id,
                agent_type,
                started_at,
                ended_at,
                cwd: cwd.clone(),
                path,
                state: &state,
                index_entry,
                agent_state: &agent_state,
                event: None,
            },
            context,
        ),
    ));

    for (line_number, value) in rows {
        if value.get("type").and_then(Value::as_str) == Some("metadata") {
            continue;
        }
        let occurred_at = kimi_record_timestamp(&value, started_at).unwrap_or(started_at);
        let event = kimi_event(&provider_session_id, line_number, &value, occurred_at, path);
        result
            .files_touched
            .extend(provider_file_touches_from_raw_value(
                CaptureProvider::KimiCodeCli,
                &provider_session_id,
                KIMI_CODE_CLI_SOURCE_FORMAT,
                Some(raw_source_path.as_str()),
                &value,
                &event,
                line_number,
            ));
        result.captures.push((
            line_number,
            kimi_capture(
                KimiCaptureDraft {
                    provider_session_id: &provider_session_id,
                    parent_provider_session_id: parent_provider_session_id.clone(),
                    root_provider_session_id: root_provider_session_id.clone(),
                    agent_id: &agent_id,
                    agent_type,
                    started_at,
                    ended_at,
                    cwd: cwd.clone(),
                    path,
                    state: &state,
                    index_entry,
                    agent_state: &agent_state,
                    event: Some(event),
                },
                context,
            ),
        ));
    }

    Ok(result)
}

pub(crate) struct KimiCaptureDraft<'a> {
    pub(crate) provider_session_id: &'a str,
    pub(crate) parent_provider_session_id: Option<String>,
    pub(crate) root_provider_session_id: Option<String>,
    pub(crate) agent_id: &'a str,
    pub(crate) agent_type: AgentType,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) cwd: Option<String>,
    pub(crate) path: &'a Path,
    pub(crate) state: &'a Value,
    pub(crate) index_entry: Option<&'a KimiSessionIndexEntry>,
    pub(crate) agent_state: &'a Value,
    pub(crate) event: Option<ProviderEventEnvelope>,
}

pub(crate) fn kimi_capture(
    draft: KimiCaptureDraft<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::KimiCodeCli,
            source_format: KIMI_CODE_CLI_SOURCE_FORMAT,
            provider_session_id: draft.provider_session_id.to_owned(),
            parent_provider_session_id: draft.parent_provider_session_id,
            root_provider_session_id: draft.root_provider_session_id,
            external_agent_id: Some(draft.agent_id.to_owned()),
            agent_type: draft.agent_type,
            role_hint: Some(if draft.agent_id == "main" {
                "main".to_owned()
            } else {
                "subagent".to_owned()
            }),
            is_primary: draft.agent_id == "main",
            started_at: draft.started_at,
            ended_at: draft.ended_at,
            cwd: draft.cwd,
            fidelity: Fidelity::Imported,
            raw_source_path: draft.path.display().to_string(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": KIMI_CODE_CLI_SOURCE_FORMAT,
                "source_path": draft.path.display().to_string(),
                "session_index": draft.index_entry.map(kimi_session_index_metadata),
            }),
            session_metadata: json!({
                "source_format": KIMI_CODE_CLI_SOURCE_FORMAT,
                "agent_id": draft.agent_id,
                "state": provider_capped_json(draft.state, PROVIDER_MAX_PREVIEW_CHARS),
                "agent_state": provider_capped_json(draft.agent_state, PROVIDER_MAX_PREVIEW_CHARS),
                "title": draft.state.get("title").or_else(|| draft.state.get("customTitle")).and_then(Value::as_str),
                "last_prompt": draft.state.get("lastPrompt").and_then(Value::as_str),
                "archived": draft.state.get("archived").and_then(Value::as_bool),
            }),
        },
        context,
        draft.event,
    )
}

pub(crate) fn kimi_event(
    provider_session_id: &str,
    line_number: usize,
    value: &Value,
    occurred_at: DateTime<Utc>,
    path: &Path,
) -> ProviderEventEnvelope {
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let event_type = kimi_event_type(record_type, value);
    let role = kimi_event_role(record_type, value, event_type);
    let text = kimi_event_text(record_type, value, event_type);
    native_event(NativeEventDraft {
        provider: CaptureProvider::KimiCodeCli,
        source_format: KIMI_CODE_CLI_SOURCE_FORMAT,
        provider_session_id: provider_session_id.to_owned(),
        provider_event_index: (line_number - 1) as u64,
        provider_event_hash: Some(format!(
            "{}:{}",
            record_type,
            value
                .get("time")
                .and_then(Value::as_i64)
                .map(|time| time.to_string())
                .unwrap_or_else(|| line_number.to_string())
        )),
        cursor: format!("{}:line:{line_number}", path.display()),
        event_type,
        role: Some(role),
        occurred_at,
        text,
        body: value.clone(),
        metadata: json!({
            "source": "kimi_code_cli_wire_jsonl",
            "source_format": KIMI_CODE_CLI_SOURCE_FORMAT,
            "line": line_number,
            "record_type": record_type,
            "model": value.get("model").cloned(),
            "usage": value.get("usage").cloned(),
        }),
    })
}

pub(crate) fn kimi_event_type(record_type: &str, value: &Value) -> EventType {
    match record_type {
        "turn.prompt" | "turn.steer" | "context.append_message" => EventType::Message,
        "context.append_loop_event" => {
            let loop_type = value.pointer("/event/type").and_then(Value::as_str);
            match loop_type {
                Some(kind) if kind.contains("tool.call") || kind.contains("tool.start") => {
                    EventType::ToolCall
                }
                Some(kind) if kind.contains("tool.result") || kind.contains("tool.finish") => {
                    EventType::ToolOutput
                }
                Some(kind) if kind.contains("message") => EventType::Message,
                _ if value.pointer("/event/toolName").is_some()
                    || value.pointer("/event/tool_name").is_some() =>
                {
                    EventType::ToolCall
                }
                _ => EventType::Notice,
            }
        }
        "usage.record" | "context.apply_compaction" | "full_compaction.complete" => {
            EventType::Summary
        }
        _ => EventType::Notice,
    }
}

pub(crate) fn kimi_event_role(
    record_type: &str,
    value: &Value,
    event_type: EventType,
) -> EventRole {
    match record_type {
        "turn.prompt" | "turn.steer" => EventRole::User,
        "context.append_message" => provider_role(
            value
                .pointer("/message/role")
                .or_else(|| value.pointer("/message/source"))
                .and_then(Value::as_str),
        ),
        "context.append_loop_event"
            if matches!(event_type, EventType::ToolCall | EventType::ToolOutput) =>
        {
            EventRole::Tool
        }
        "context.append_loop_event" => provider_role(
            value
                .pointer("/event/role")
                .or_else(|| value.pointer("/event/source"))
                .and_then(Value::as_str),
        ),
        _ => EventRole::System,
    }
}

pub(crate) fn kimi_event_text(record_type: &str, value: &Value, event_type: EventType) -> String {
    match record_type {
        "turn.prompt" | "turn.steer" => value
            .get("input")
            .and_then(provider_value_text)
            .unwrap_or_else(|| format!("Kimi Code CLI {record_type}")),
        "context.append_message" => value
            .pointer("/message/content")
            .or_else(|| value.get("message"))
            .and_then(provider_value_text)
            .unwrap_or_else(|| "Kimi Code CLI message".to_owned()),
        "context.append_loop_event" => value
            .pointer("/event/content")
            .or_else(|| value.pointer("/event/text"))
            .or_else(|| value.pointer("/event/output"))
            .or_else(|| value.pointer("/event/result"))
            .or_else(|| value.pointer("/event/message"))
            .and_then(provider_value_text)
            .or_else(|| {
                value
                    .pointer("/event/toolName")
                    .or_else(|| value.pointer("/event/tool_name"))
                    .and_then(Value::as_str)
                    .map(|tool| match event_type {
                        EventType::ToolOutput => format!("tool result: {tool}"),
                        EventType::ToolCall => format!("tool call: {tool}"),
                        _ => format!("tool: {tool}"),
                    })
            })
            .unwrap_or_else(|| format!("Kimi Code CLI {record_type}")),
        "usage.record" => value
            .get("model")
            .and_then(Value::as_str)
            .map(|model| format!("usage record: {model}"))
            .unwrap_or_else(|| "usage record".to_owned()),
        "tools.set_active_tools" => value
            .get("names")
            .and_then(Value::as_array)
            .map(|names| {
                let names = names
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("active tools: {names}")
            })
            .unwrap_or_else(|| "active tools updated".to_owned()),
        "permission.set_mode" => value
            .get("mode")
            .and_then(Value::as_str)
            .map(|mode| format!("permission mode: {mode}"))
            .unwrap_or_else(|| "permission mode updated".to_owned()),
        _ => format!("Kimi Code CLI {record_type}"),
    }
}

pub(crate) fn kimi_record_timestamp(
    value: &Value,
    fallback: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    value
        .get("time")
        .and_then(Value::as_i64)
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .or_else(|| {
            value
                .get("timestamp")
                .and_then(|timestamp| match timestamp {
                    Value::String(raw) => parse_rfc3339_utc(raw),
                    Value::Number(number) => number
                        .as_f64()
                        .and_then(provider_timestamp_seconds_to_datetime),
                    _ => None,
                })
        })
        .or_else(|| {
            value
                .get("created_at")
                .and_then(Value::as_i64)
                .and_then(DateTime::<Utc>::from_timestamp_millis)
        })
        .or(Some(fallback))
}

pub(crate) fn kimi_state_timestamp(value: &Value, fields: &[&str]) -> Option<DateTime<Utc>> {
    fields.iter().find_map(|field| {
        value.get(*field).and_then(|timestamp| match timestamp {
            Value::String(raw) => parse_rfc3339_utc(raw).or_else(|| {
                raw.parse::<f64>()
                    .ok()
                    .and_then(provider_timestamp_seconds_to_datetime)
            }),
            Value::Number(number) => number
                .as_f64()
                .and_then(provider_timestamp_seconds_to_datetime),
            _ => None,
        })
    })
}

pub(crate) fn kimi_provider_session_ids(
    session_id: &str,
    agent_id: &str,
    agent_state: &Value,
) -> (String, Option<String>, Option<String>, AgentType) {
    if agent_id == "main" {
        return (session_id.to_owned(), None, None, AgentType::Primary);
    }
    let provider_session_id = format!("{session_id}/agents/{agent_id}");
    let parent = agent_state
        .get("parentAgentId")
        .or_else(|| agent_state.get("parent_agent_id"))
        .and_then(Value::as_str)
        .filter(|parent| !parent.trim().is_empty())
        .map(|parent| {
            if parent == "main" {
                session_id.to_owned()
            } else {
                format!("{session_id}/agents/{parent}")
            }
        })
        .or_else(|| Some(session_id.to_owned()));
    (
        provider_session_id,
        parent,
        Some(session_id.to_owned()),
        AgentType::Subagent,
    )
}

pub(crate) fn kimi_wire_path_parts(path: &Path) -> Option<(PathBuf, String, String)> {
    let agent_dir = path.parent()?;
    let agent_id = agent_dir.file_name()?.to_str()?.to_owned();
    let agents_dir = agent_dir.parent()?;
    if agents_dir.file_name().and_then(|name| name.to_str()) != Some("agents") {
        return None;
    }
    let session_dir = agents_dir.parent()?.to_path_buf();
    let session_id = session_dir.file_name()?.to_str()?.to_owned();
    Some((session_dir, agent_id, session_id))
}

pub(crate) fn read_kimi_state(session_dir: &Path) -> Value {
    read_text_file_limited(
        &session_dir.join("state.json"),
        MAX_PROVIDER_JSONL_LINE_BYTES,
        "Kimi Code CLI state.json",
    )
    .ok()
    .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
    .unwrap_or(Value::Null)
}

pub(crate) fn kimi_session_index(path: &Path) -> BTreeMap<String, KimiSessionIndexEntry> {
    let mut current = if path.is_file() {
        path.parent().map(Path::to_path_buf)
    } else {
        Some(path.to_path_buf())
    };
    while let Some(dir) = current {
        let index_path = dir.join("session_index.jsonl");
        if index_path.is_file() {
            return read_kimi_session_index(&index_path);
        }
        current = dir.parent().map(Path::to_path_buf);
    }
    BTreeMap::new()
}

pub(crate) fn read_kimi_session_index(path: &Path) -> BTreeMap<String, KimiSessionIndexEntry> {
    let Ok(text) = read_text_file_limited(
        path,
        MAX_PROVIDER_JSONL_LINE_BYTES,
        "Kimi Code CLI session_index.jsonl",
    ) else {
        return BTreeMap::new();
    };
    let mut entries = BTreeMap::new();
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(session_id) = value
            .get("sessionId")
            .or_else(|| value.get("session_id"))
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
        else {
            continue;
        };
        entries.insert(
            session_id.to_owned(),
            KimiSessionIndexEntry {
                session_id: session_id.to_owned(),
                session_dir: value
                    .get("sessionDir")
                    .or_else(|| value.get("session_dir"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                work_dir: value
                    .get("workDir")
                    .or_else(|| value.get("work_dir"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            },
        );
    }
    entries
}

pub(crate) fn kimi_session_index_metadata(entry: &KimiSessionIndexEntry) -> Value {
    json!({
        "session_id": entry.session_id,
        "session_dir": entry.session_dir,
        "work_dir": entry.work_dir,
    })
}
