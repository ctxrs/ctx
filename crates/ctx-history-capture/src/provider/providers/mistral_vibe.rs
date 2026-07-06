use std::{
    fs::{self, File},
    io::BufReader,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderEventEnvelope, ProviderSourceTrust,
};
use serde_json::{json, Value};

use crate::provider::providers::native_jsonl::{
    native_jsonl_missing_reason, native_jsonl_timestamp,
};

use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
    read_provider_jsonl_line, read_text_file_limited,
};
use crate::common::time::parse_rfc3339_utc;
use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::file_touches::provider_file_touches_from_raw_value;
use crate::provider::native::{
    native_event, native_provider_capture, provider_capped_json_value, provider_role,
    provider_value_text, NativeEventDraft, NativeSessionDraft,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportSummary, ProviderNormalizationResult,
    Result, MAX_PROVIDER_JSONL_LINE_BYTES, MISTRAL_VIBE_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

pub(crate) struct MistralVibeSessionSource {
    pub(crate) session_dir: PathBuf,
    pub(crate) metadata_path: PathBuf,
    pub(crate) messages_path: PathBuf,
}

pub(crate) fn normalize_mistral_vibe_sessions(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let mut session_sources = Vec::new();
    collect_mistral_vibe_session_sources(path, &mut session_sources)?;
    session_sources.sort_by(|left, right| left.messages_path.cmp(&right.messages_path));
    session_sources.dedup_by(|left, right| left.messages_path == right.messages_path);
    if session_sources.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: native_jsonl_missing_reason(CaptureProvider::MistralVibe),
        });
    }

    let mut merged = ProviderNormalizationResult::default();
    for source in session_sources {
        let mut result = normalize_mistral_vibe_session_source(&source, context)?;
        merged.summary.merge(result.summary);
        merged.captures.append(&mut result.captures);
        merged.files_touched.append(&mut result.files_touched);
    }
    Ok(merged)
}

pub(crate) fn collect_mistral_vibe_session_sources(
    root: &Path,
    sessions: &mut Vec<MistralVibeSessionSource>,
) -> Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "symlinked provider transcript roots are rejected",
        });
    }
    ensure_provider_path_parents_are_not_symlinks(root)?;
    if file_type.is_file() {
        ensure_regular_provider_transcript_file(root)?;
        if root.file_name().and_then(|name| name.to_str()) == Some("messages.jsonl") {
            if let Some(session_dir) = root.parent() {
                if let Some(source) = mistral_vibe_session_source_from_dir(session_dir)? {
                    sessions.push(source);
                }
            }
        }
        return Ok(());
    }
    if !file_type.is_dir() {
        return Ok(());
    }

    if let Some(source) = mistral_vibe_session_source_from_dir(root)? {
        sessions.push(source);
        return Ok(());
    }

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_mistral_vibe_session_sources(&path, sessions)?;
        }
    }
    Ok(())
}

pub(crate) fn mistral_vibe_session_source_from_dir(
    dir: &Path,
) -> Result<Option<MistralVibeSessionSource>> {
    let metadata_path = dir.join("meta.json");
    let messages_path = dir.join("messages.jsonl");
    if !metadata_path.is_file() || !messages_path.is_file() {
        return Ok(None);
    }
    ensure_regular_provider_transcript_file(&metadata_path)?;
    ensure_regular_provider_transcript_file(&messages_path)?;
    Ok(Some(MistralVibeSessionSource {
        session_dir: dir.to_path_buf(),
        metadata_path,
        messages_path,
    }))
}

pub(crate) fn normalize_mistral_vibe_session_source(
    source: &MistralVibeSessionSource,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let mut result = ProviderNormalizationResult::default();
    let metadata = read_mistral_vibe_metadata(&source.metadata_path, &mut result.summary);
    let mut rows = Vec::new();
    let file = File::open(&source.messages_path)?;
    let mut reader = BufReader::new(file);
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

    let provider_session_id = mistral_vibe_metadata_string(&metadata, "session_id")
        .or_else(|| {
            source
                .session_dir
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.trim().is_empty())
                .map(str::to_owned)
        })
        .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
            path: source.session_dir.clone(),
            reason: "Mistral Vibe session directory is missing a session id",
        })?;
    let started_at = mistral_vibe_metadata_timestamp(&metadata, "start_time")
        .or_else(|| {
            rows.iter()
                .find_map(|(_, value)| native_jsonl_timestamp(value))
        })
        .unwrap_or(context.imported_at);
    let ended_at = mistral_vibe_metadata_timestamp(&metadata, "end_time");
    let cwd = mistral_vibe_metadata_pointer_string(&metadata, &["/environment/working_directory"]);
    let parent_provider_session_id = mistral_vibe_metadata_string(&metadata, "parent_session_id");
    let agent_type = if parent_provider_session_id.is_some() {
        AgentType::Subagent
    } else {
        AgentType::Primary
    };
    let role_hint = if parent_provider_session_id.is_some() {
        "subagent"
    } else {
        "primary"
    };
    let raw_source_path = source.messages_path.display().to_string();

    if rows.is_empty() {
        result.captures.push((
            0,
            mistral_vibe_capture(
                MistralVibeCaptureDraft {
                    provider_session_id,
                    parent_provider_session_id,
                    agent_type,
                    role_hint: role_hint.to_owned(),
                    is_primary: agent_type == AgentType::Primary,
                    started_at,
                    ended_at,
                    cwd,
                    metadata: &metadata,
                    source,
                    event: None,
                },
                context,
            ),
        ));
        return Ok(result);
    }

    for (line_number, value) in rows {
        let occurred_at = native_jsonl_timestamp(&value).unwrap_or(started_at);
        let event = mistral_vibe_event(
            &provider_session_id,
            line_number,
            &value,
            occurred_at,
            &source.messages_path,
            &metadata,
        );
        result
            .files_touched
            .extend(provider_file_touches_from_raw_value(
                CaptureProvider::MistralVibe,
                &provider_session_id,
                MISTRAL_VIBE_SOURCE_FORMAT,
                Some(raw_source_path.as_str()),
                &value,
                &event,
                line_number,
            ));
        result.captures.push((
            line_number,
            mistral_vibe_capture(
                MistralVibeCaptureDraft {
                    provider_session_id: provider_session_id.clone(),
                    parent_provider_session_id: parent_provider_session_id.clone(),
                    agent_type,
                    role_hint: role_hint.to_owned(),
                    is_primary: agent_type == AgentType::Primary,
                    started_at,
                    ended_at,
                    cwd: cwd.clone(),
                    metadata: &metadata,
                    source,
                    event: Some(event),
                },
                context,
            ),
        ));
    }

    Ok(result)
}

pub(crate) struct MistralVibeCaptureDraft<'a> {
    pub(crate) provider_session_id: String,
    pub(crate) parent_provider_session_id: Option<String>,
    pub(crate) agent_type: AgentType,
    pub(crate) role_hint: String,
    pub(crate) is_primary: bool,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) cwd: Option<String>,
    pub(crate) metadata: &'a Value,
    pub(crate) source: &'a MistralVibeSessionSource,
    pub(crate) event: Option<ProviderEventEnvelope>,
}

pub(crate) fn mistral_vibe_capture(
    draft: MistralVibeCaptureDraft<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::MistralVibe,
            source_format: MISTRAL_VIBE_SOURCE_FORMAT,
            provider_session_id: draft.provider_session_id.clone(),
            parent_provider_session_id: draft.parent_provider_session_id.clone(),
            root_provider_session_id: draft.parent_provider_session_id.clone(),
            external_agent_id: mistral_vibe_metadata_pointer_string(
                draft.metadata,
                &["/agent_profile/name"],
            ),
            agent_type: draft.agent_type,
            role_hint: Some(draft.role_hint),
            is_primary: draft.is_primary,
            started_at: draft.started_at,
            ended_at: draft.ended_at,
            cwd: draft.cwd,
            fidelity: Fidelity::Imported,
            raw_source_path: draft.source.messages_path.display().to_string(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": MISTRAL_VIBE_SOURCE_FORMAT,
                "source_path": draft.source.messages_path.display().to_string(),
                "metadata_path": draft.source.metadata_path.display().to_string(),
                "session_dir": draft.source.session_dir.display().to_string(),
            }),
            session_metadata: json!({
                "source_format": MISTRAL_VIBE_SOURCE_FORMAT,
                "provider": CaptureProvider::MistralVibe.as_str(),
                "session_id": draft.provider_session_id,
                "title": mistral_vibe_metadata_string(draft.metadata, "title"),
                "title_source": mistral_vibe_metadata_string(draft.metadata, "title_source"),
                "git_branch": mistral_vibe_metadata_string(draft.metadata, "git_branch"),
                "git_commit": mistral_vibe_metadata_string(draft.metadata, "git_commit"),
                "total_messages": draft.metadata.get("total_messages").and_then(Value::as_u64),
                "agent_profile": draft.metadata.get("agent_profile").cloned(),
                "stats": draft.metadata.get("stats").cloned(),
                "loops": draft.metadata.get("loops").cloned(),
                "experiments": draft.metadata.get("experiments").cloned(),
            }),
        },
        context,
        draft.event,
    )
}

pub(crate) fn mistral_vibe_event(
    provider_session_id: &str,
    line_number: usize,
    value: &Value,
    occurred_at: DateTime<Utc>,
    path: &Path,
    metadata: &Value,
) -> ProviderEventEnvelope {
    let role = value
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let event_type = mistral_vibe_event_type(role, value);
    native_event(NativeEventDraft {
        provider: CaptureProvider::MistralVibe,
        source_format: MISTRAL_VIBE_SOURCE_FORMAT,
        provider_session_id: provider_session_id.to_owned(),
        provider_event_index: (line_number - 1) as u64,
        provider_event_hash: Some(mistral_vibe_event_id(value, line_number, role)),
        cursor: format!("{}:line:{line_number}", path.display()),
        event_type,
        role: Some(provider_role(Some(role))),
        occurred_at,
        text: mistral_vibe_event_text(role, value, event_type),
        body: value.clone(),
        metadata: json!({
            "source": MISTRAL_VIBE_SOURCE_FORMAT,
            "source_format": MISTRAL_VIBE_SOURCE_FORMAT,
            "line": line_number,
            "role": role,
            "message_id": value.get("message_id").and_then(Value::as_str),
            "reasoning_message_id": value.get("reasoning_message_id").and_then(Value::as_str),
            "tool_call_id": value.get("tool_call_id").and_then(Value::as_str),
            "name": value.get("name").and_then(Value::as_str),
            "tool_calls": value.get("tool_calls").map(|calls| provider_capped_json_value(calls, PROVIDER_MAX_PREVIEW_CHARS)),
            "images": value.get("images").map(|images| provider_capped_json_value(images, PROVIDER_MAX_PREVIEW_CHARS)),
            "agent_profile": metadata.pointer("/agent_profile/name").and_then(Value::as_str),
        }),
    })
}

pub(crate) fn mistral_vibe_event_type(role: &str, value: &Value) -> EventType {
    if role == "tool" || value.get("tool_call_id").is_some() {
        EventType::ToolOutput
    } else if value
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|calls| !calls.is_empty())
    {
        EventType::ToolCall
    } else if role == "system" {
        EventType::Notice
    } else {
        EventType::Message
    }
}

pub(crate) fn mistral_vibe_event_text(role: &str, value: &Value, event_type: EventType) -> String {
    let mut parts = Vec::new();
    if let Some(content) = value.get("content").and_then(provider_value_text) {
        parts.push(content);
    }
    if let Some(reasoning) = value.get("reasoning_content").and_then(provider_value_text) {
        parts.push(reasoning);
    }
    if let Some(tool_calls) = value
        .get("tool_calls")
        .and_then(mistral_vibe_tool_calls_text)
    {
        parts.push(tool_calls);
    }
    if let Some(images) = value.get("images").and_then(provider_value_text) {
        parts.push(images);
    }
    if !parts.is_empty() {
        return parts.join("\n");
    }
    match event_type {
        EventType::ToolOutput => format!("Mistral Vibe {role} output"),
        EventType::ToolCall => format!("Mistral Vibe {role} tool call"),
        _ => format!("Mistral Vibe {role} message"),
    }
}

pub(crate) fn mistral_vibe_tool_calls_text(value: &Value) -> Option<String> {
    let calls = value.as_array()?;
    let names = calls
        .iter()
        .filter_map(|call| {
            call.pointer("/function/name")
                .or_else(|| call.get("name"))
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
        })
        .collect::<Vec<_>>();
    if names.is_empty() {
        Some(provider_value_text(value)?)
    } else {
        Some(format!("tool calls: {}", names.join(", ")))
    }
}

pub(crate) fn mistral_vibe_event_id(value: &Value, line_number: usize, role: &str) -> String {
    value
        .get("message_id")
        .or_else(|| value.get("tool_call_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{role}:line-{line_number}"))
}

pub(crate) fn read_mistral_vibe_metadata(
    path: &Path,
    summary: &mut ProviderImportSummary,
) -> Value {
    match read_text_file_limited(
        path,
        MAX_PROVIDER_JSONL_LINE_BYTES,
        "Mistral Vibe meta.json",
    ) {
        Ok(raw) => match serde_json::from_str::<Value>(&raw) {
            Ok(value) if value.is_object() => value,
            Ok(_) => {
                push_provider_import_failure(
                    summary,
                    0,
                    "Mistral Vibe meta.json must contain a JSON object".to_owned(),
                );
                Value::Null
            }
            Err(err) => {
                push_provider_import_failure(
                    summary,
                    0,
                    format!("invalid Mistral Vibe meta.json: {err}"),
                );
                Value::Null
            }
        },
        Err(err) => {
            push_provider_import_failure(
                summary,
                0,
                format!("could not read Mistral Vibe meta.json: {err}"),
            );
            Value::Null
        }
    }
}

pub(crate) fn mistral_vibe_metadata_string(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|raw| !raw.trim().is_empty())
        .map(str::to_owned)
}

pub(crate) fn mistral_vibe_metadata_pointer_string(
    value: &Value,
    pointers: &[&str],
) -> Option<String> {
    pointers.iter().find_map(|pointer| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .filter(|raw| !raw.trim().is_empty())
            .map(str::to_owned)
    })
}

pub(crate) fn mistral_vibe_metadata_timestamp(value: &Value, field: &str) -> Option<DateTime<Utc>> {
    value
        .get(field)
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_utc)
}
