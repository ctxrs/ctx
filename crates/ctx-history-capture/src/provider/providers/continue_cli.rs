use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderEventEnvelope, ProviderSourceTrust,
};
use serde_json::{json, Value};

use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
    read_text_file_limited,
};
use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::file_touches::provider_file_touches_from_raw_value;
use crate::provider::native::{
    native_event, native_provider_capture, provider_role, provider_timestamp_value,
    provider_value_text, NativeEventDraft, NativeSessionDraft,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    CONTINUE_CLI_SOURCE_FORMAT, MAX_PROVIDER_JSONL_LINE_BYTES,
};

pub(crate) fn normalize_continue_cli_sessions(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let mut paths = Vec::new();
    collect_continue_session_json_paths(path, &mut paths)?;
    paths.sort();
    if paths.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no Continue CLI session JSON files found",
        });
    }

    let session_index = continue_session_index(&paths);
    let mut result = ProviderNormalizationResult::default();

    for (path_index, path) in paths.into_iter().enumerate() {
        let source_line = path_index.saturating_add(1);
        let raw_source_path = path.display().to_string();
        let text = match read_text_file_limited(
            &path,
            MAX_PROVIDER_JSONL_LINE_BYTES,
            "Continue CLI session JSON",
        ) {
            Ok(text) => text,
            Err(err) => {
                push_provider_import_failure(&mut result.summary, source_line, err.to_string());
                continue;
            }
        };
        let session: Value = match serde_json::from_str(&text) {
            Ok(session) => session,
            Err(err) => {
                push_provider_import_failure(
                    &mut result.summary,
                    source_line,
                    format!("invalid Continue CLI session JSON: {err}"),
                );
                continue;
            }
        };
        let Some(provider_session_id) = continue_session_id(&session, &path) else {
            push_provider_import_failure(
                &mut result.summary,
                source_line,
                "Continue CLI session is missing sessionId and has no JSON file stem".to_owned(),
            );
            continue;
        };
        let indexed_metadata = session_index.get(&provider_session_id);
        let started_at =
            continue_session_started_at(&session, indexed_metadata, context.imported_at);
        let history = session
            .get("history")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        if history.is_empty() {
            result.captures.push((
                source_line,
                continue_capture(
                    &provider_session_id,
                    &session,
                    indexed_metadata,
                    started_at,
                    &raw_source_path,
                    context,
                    None,
                ),
            ));
            continue;
        }

        for (item_index, item) in history.iter().enumerate() {
            let provider_event_index = item_index.saturating_add(1) as u64;
            let line = source_line
                .saturating_mul(1_000_000)
                .saturating_add(item_index)
                .saturating_add(1);
            let fallback_time = started_at + chrono::Duration::milliseconds(item_index as i64);
            let occurred_at = continue_history_item_timestamp(item, fallback_time);
            let event = continue_history_item_event(
                &provider_session_id,
                item,
                provider_event_index,
                occurred_at,
            );
            result
                .files_touched
                .extend(provider_file_touches_from_raw_value(
                    CaptureProvider::Continue,
                    &provider_session_id,
                    CONTINUE_CLI_SOURCE_FORMAT,
                    Some(raw_source_path.as_str()),
                    item,
                    &event,
                    line,
                ));
            result.captures.push((
                line,
                continue_capture(
                    &provider_session_id,
                    &session,
                    indexed_metadata,
                    started_at,
                    &raw_source_path,
                    context,
                    Some(event),
                ),
            ));
        }
    }

    Ok(result)
}

pub(crate) fn collect_continue_session_json_paths(
    root: &Path,
    paths: &mut Vec<PathBuf>,
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
        if continue_session_json_path(root) {
            ensure_regular_provider_transcript_file(root)?;
            paths.push(root.to_path_buf());
        }
        return Ok(());
    }
    if !file_type.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_continue_session_json_paths(&path, paths)?;
        } else if file_type.is_file() && continue_session_json_path(&path) {
            ensure_regular_provider_transcript_file(&path)?;
            paths.push(path);
        }
    }
    Ok(())
}

pub(crate) fn continue_session_json_path(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("json")
        && path.file_name().and_then(|name| name.to_str()) != Some("sessions.json")
}

pub(crate) fn continue_session_index(paths: &[PathBuf]) -> BTreeMap<String, Value> {
    let mut index = BTreeMap::new();
    let mut checked = BTreeSet::new();
    for path in paths {
        let Some(parent) = path.parent() else {
            continue;
        };
        if !checked.insert(parent.to_path_buf()) {
            continue;
        }
        let index_path = parent.join("sessions.json");
        let Ok(text) = read_text_file_limited(
            &index_path,
            MAX_PROVIDER_JSONL_LINE_BYTES,
            "Continue CLI sessions index",
        ) else {
            continue;
        };
        let Ok(Value::Array(entries)) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        for entry in entries {
            if let Some(session_id) = entry
                .get("sessionId")
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
            {
                index.entry(session_id.to_owned()).or_insert(entry);
            }
        }
    }
    index
}

pub(crate) fn continue_session_id(session: &Value, path: &Path) -> Option<String> {
    session
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            path.file_stem()
                .and_then(|name| name.to_str())
                .filter(|id| !id.trim().is_empty())
                .map(str::to_owned)
        })
}

pub(crate) fn continue_session_started_at(
    session: &Value,
    indexed_metadata: Option<&Value>,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    session
        .get("createdAt")
        .or_else(|| session.get("startedAt"))
        .or_else(|| indexed_metadata.and_then(|metadata| metadata.get("dateCreated")))
        .map(|value| provider_timestamp_value(Some(value), fallback))
        .unwrap_or(fallback)
}

pub(crate) fn continue_history_item_timestamp(
    item: &Value,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    item.get("timestamp")
        .or_else(|| item.get("createdAt"))
        .or_else(|| item.pointer("/message/timestamp"))
        .map(|value| provider_timestamp_value(Some(value), fallback))
        .unwrap_or(fallback)
}

pub(crate) fn continue_capture(
    provider_session_id: &str,
    session: &Value,
    indexed_metadata: Option<&Value>,
    started_at: DateTime<Utc>,
    raw_source_path: &str,
    context: &ProviderAdapterContext,
    event: Option<ProviderEventEnvelope>,
) -> ProviderCaptureEnvelope {
    let title = session.get("title").and_then(Value::as_str);
    let cwd = session
        .get("workspaceDirectory")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.trim().is_empty())
        .map(str::to_owned);
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::Continue,
            source_format: CONTINUE_CLI_SOURCE_FORMAT,
            provider_session_id: provider_session_id.to_owned(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("continue-cli".to_owned()),
            is_primary: true,
            started_at,
            ended_at: None,
            cwd,
            fidelity: Fidelity::Imported,
            raw_source_path: raw_source_path.to_owned(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": CONTINUE_CLI_SOURCE_FORMAT,
                "source_format": CONTINUE_CLI_SOURCE_FORMAT,
            }),
            session_metadata: json!({
                "source_format": CONTINUE_CLI_SOURCE_FORMAT,
                "title": title,
                "mode": session.get("mode").cloned(),
                "chat_model_title": session.get("chatModelTitle").cloned(),
                "usage": session.get("usage").cloned(),
                "session_index": indexed_metadata.cloned(),
            }),
        },
        context,
        event,
    )
}

pub(crate) fn continue_history_item_event(
    provider_session_id: &str,
    item: &Value,
    provider_event_index: u64,
    occurred_at: DateTime<Utc>,
) -> ProviderEventEnvelope {
    let role_text = item.pointer("/message/role").and_then(Value::as_str);
    let role = Some(provider_role(role_text));
    let has_tool_calls = item
        .get("toolCallStates")
        .and_then(Value::as_array)
        .is_some_and(|states| !states.is_empty());
    let event_type = if has_tool_calls {
        EventType::ToolCall
    } else {
        EventType::Message
    };
    native_event(NativeEventDraft {
        provider: CaptureProvider::Continue,
        source_format: CONTINUE_CLI_SOURCE_FORMAT,
        provider_session_id: provider_session_id.to_owned(),
        provider_event_index,
        provider_event_hash: item
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned),
        cursor: format!("history:{provider_session_id}:{provider_event_index}"),
        event_type,
        role,
        occurred_at,
        text: continue_history_item_text(item)
            .unwrap_or_else(|| "Continue CLI history item".to_owned()),
        body: item.clone(),
        metadata: json!({
            "source": CONTINUE_CLI_SOURCE_FORMAT,
            "source_format": CONTINUE_CLI_SOURCE_FORMAT,
            "message_role": role_text,
            "has_tool_calls": has_tool_calls,
        }),
    })
}

pub(crate) fn continue_history_item_text(item: &Value) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(text) = item
        .pointer("/message/content")
        .and_then(provider_value_text)
        .or_else(|| item.get("editorState").and_then(provider_value_text))
    {
        parts.push(text);
    }
    if let Some(text) = item
        .get("contextItems")
        .and_then(continue_context_items_text)
    {
        parts.push(text);
    }
    if let Some(text) = item
        .get("toolCallStates")
        .and_then(continue_tool_states_text)
    {
        parts.push(text);
    }
    if let Some(text) = item.get("conversationSummary").and_then(Value::as_str) {
        parts.push(text.to_owned());
    }
    let text = parts
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(text)
}

pub(crate) fn continue_context_items_text(value: &Value) -> Option<String> {
    let items = value.as_array()?;
    let mut parts = Vec::new();
    for item in items {
        if let Some(content) = item.get("content").and_then(provider_value_text) {
            parts.push(content);
        } else if let Some(name) = item.get("name").and_then(Value::as_str) {
            parts.push(name.to_owned());
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

pub(crate) fn continue_tool_states_text(value: &Value) -> Option<String> {
    let states = value.as_array()?;
    let mut parts = Vec::new();
    for state in states {
        let name = state
            .pointer("/toolCall/function/name")
            .or_else(|| state.pointer("/toolCall/name"))
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let status = state
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        parts.push(format!("tool call: {name} ({status})"));
        if let Some(output) = state.get("output").and_then(provider_value_text) {
            parts.push(output);
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}
