use std::{
    collections::BTreeMap,
    fs::{self},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderEventEnvelope,
    ProviderSourceTrust,
};
use serde_json::{json, Value};

use crate::provider::native::OpenHandsEventFile;
use crate::provider::providers::openclaw::provider_path_has_component;

use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
    read_json_file_limited,
};
use crate::common::time::parse_rfc3339_utc;
use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::file_touches::provider_file_touches_from_raw_value;
use crate::provider::native::{
    native_event, native_provider_capture, provider_role, provider_value_text, NativeEventDraft,
    NativeSessionDraft,
};
use crate::{
    fnv1a64, CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    MAX_PROVIDER_JSONL_LINE_BYTES, OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
};

pub(crate) fn normalize_openhands_file_events(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let mut event_paths = Vec::new();
    collect_openhands_event_paths(path, &mut event_paths)?;
    event_paths.sort();
    if event_paths.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no OpenHands event JSON files found under v1_conversations",
        });
    }

    let mut result = ProviderNormalizationResult::default();
    let mut events_by_session = BTreeMap::<String, Vec<OpenHandsEventFile>>::new();
    for event_path in event_paths {
        let line_number = openhands_line_number(&event_path);
        let Some(session_id) = openhands_conversation_id_from_path(&event_path) else {
            continue;
        };
        let value = match read_json_file_limited(
            &event_path,
            MAX_PROVIDER_JSONL_LINE_BYTES,
            "OpenHands event JSON",
        ) {
            Ok(value) => value,
            Err(err) => {
                push_provider_import_failure(&mut result.summary, line_number, err.to_string());
                continue;
            }
        };
        let event_id = openhands_event_id(&event_path, &value);
        let timestamp = match openhands_event_timestamp(&value) {
            Some(timestamp) => timestamp,
            None => {
                push_provider_import_failure(
                    &mut result.summary,
                    line_number,
                    format!("OpenHands event {event_id} missing valid timestamp"),
                );
                continue;
            }
        };
        let user_id = openhands_user_id_from_path(&event_path);
        events_by_session
            .entry(session_id.clone())
            .or_default()
            .push(OpenHandsEventFile {
                path: event_path,
                line_number,
                session_id,
                user_id,
                event_id,
                timestamp,
                value,
            });
    }

    for events in events_by_session.values_mut() {
        events.sort_by(|left, right| {
            left.timestamp
                .cmp(&right.timestamp)
                .then_with(|| left.event_id.cmp(&right.event_id))
                .then_with(|| left.path.cmp(&right.path))
        });
        let started_at = events
            .first()
            .map(|event| event.timestamp)
            .unwrap_or(context.imported_at);
        let ended_at = events.last().map(|event| event.timestamp);
        let session_id = events
            .first()
            .map(|event| event.session_id.clone())
            .unwrap_or_else(|| "unknown-conversation".to_owned());
        let user_id = events.iter().find_map(|event| event.user_id.clone());
        let raw_source_path = events
            .first()
            .and_then(|event| event.path.parent())
            .unwrap_or(path)
            .display()
            .to_string();
        let cwd = events.iter().find_map(openhands_event_cwd);

        for (index, event_file) in events.iter().enumerate() {
            let provider_event_index = index as u64;
            let event = openhands_provider_event(&session_id, event_file, provider_event_index);
            result
                .files_touched
                .extend(provider_file_touches_from_raw_value(
                    CaptureProvider::OpenHands,
                    &session_id,
                    OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                    Some(raw_source_path.as_str()),
                    &event_file.value,
                    &event,
                    event_file.line_number,
                ));
            result.captures.push((
                event_file.line_number,
                native_provider_capture(
                    NativeSessionDraft {
                        provider: CaptureProvider::OpenHands,
                        source_format: OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                        provider_session_id: session_id.clone(),
                        parent_provider_session_id: None,
                        root_provider_session_id: None,
                        external_agent_id: user_id.clone(),
                        agent_type: AgentType::Primary,
                        role_hint: Some("primary".to_owned()),
                        is_primary: true,
                        started_at,
                        ended_at,
                        cwd: cwd.clone(),
                        fidelity: Fidelity::Imported,
                        raw_source_path: raw_source_path.clone(),
                        trust: ProviderSourceTrust::ProviderNative,
                        source_metadata: json!({
                            "adapter": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                            "storage": "filesystem_event_service",
                            "conversation_dir": raw_source_path,
                        }),
                        session_metadata: json!({
                            "source_format": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                            "provider": "openhands",
                            "conversation_id": session_id,
                            "user_id": user_id,
                            "event_count": events.len(),
                        }),
                    },
                    context,
                    Some(event),
                ),
            ));
        }
    }

    Ok(result)
}

pub(crate) fn collect_openhands_event_paths(root: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
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
        if openhands_json_path_is_event(root) {
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
            collect_openhands_event_paths(&path, paths)?;
        } else if openhands_json_path_is_event(&path) {
            ensure_regular_provider_transcript_file(&path)?;
            paths.push(path);
        }
    }
    Ok(())
}

pub(crate) fn openhands_json_path_is_event(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("json")
        && provider_path_has_component(path, "v1_conversations")
}

pub(crate) fn openhands_conversation_id_from_path(path: &Path) -> Option<String> {
    let mut components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str());
    while let Some(component) = components.next() {
        if component == "v1_conversations" {
            return components
                .next()
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned);
        }
    }
    None
}

pub(crate) fn openhands_user_id_from_path(path: &Path) -> Option<String> {
    let components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    components.windows(2).find_map(|window| {
        (window[1] == "v1_conversations" && !window[0].trim().is_empty())
            .then(|| window[0].to_owned())
    })
}

pub(crate) fn openhands_event_id(path: &Path, value: &Value) -> String {
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.trim().is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| path.display().to_string())
}

pub(crate) fn openhands_event_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_utc)
}

pub(crate) fn openhands_line_number(path: &Path) -> usize {
    fnv1a64(path.display().to_string().as_bytes()) as usize
}

pub(crate) fn openhands_event_cwd(event: &OpenHandsEventFile) -> Option<String> {
    event
        .value
        .pointer("/observation/metadata/working_dir")
        .or_else(|| event.value.pointer("/observation/metadata/cwd"))
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.trim().is_empty())
        .map(str::to_owned)
}

pub(crate) fn openhands_provider_event(
    session_id: &str,
    event_file: &OpenHandsEventFile,
    provider_event_index: u64,
) -> ProviderEventEnvelope {
    let entry_type = openhands_entry_type(&event_file.value);
    let event_type = openhands_event_type(&event_file.value, &entry_type);
    let role = Some(openhands_role(&event_file.value, &entry_type));
    let text = openhands_event_text(&event_file.value, &entry_type, event_type);
    native_event(NativeEventDraft {
        provider: CaptureProvider::OpenHands,
        source_format: OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
        provider_session_id: session_id.to_owned(),
        provider_event_index,
        provider_event_hash: Some(event_file.event_id.clone()),
        cursor: format!("{}:{}", event_file.path.display(), event_file.event_id),
        event_type,
        role,
        occurred_at: event_file.timestamp,
        text,
        body: event_file.value.clone(),
        metadata: json!({
            "source": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
            "source_format": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
            "event_id": event_file.event_id,
            "entry_type": entry_type,
            "event_path": event_file.path.display().to_string(),
            "conversation_id": session_id,
            "tool_name": event_file.value.get("tool_name").and_then(Value::as_str),
            "tool_call_id": event_file.value.get("tool_call_id").and_then(Value::as_str),
            "action_id": event_file.value.get("action_id").and_then(Value::as_str),
        }),
    })
}

pub(crate) fn openhands_entry_type(value: &Value) -> String {
    if let Some(entry_type) = value
        .get("kind")
        .or_else(|| value.get("type"))
        .and_then(Value::as_str)
    {
        return entry_type.to_owned();
    }
    if value.get("llm_message").is_some() {
        "MessageEvent".to_owned()
    } else if value.get("action").is_some() {
        "ActionEvent".to_owned()
    } else if value.get("observation").is_some() {
        "ObservationEvent".to_owned()
    } else {
        "OpenHandsEvent".to_owned()
    }
}

pub(crate) fn openhands_event_type(value: &Value, entry_type: &str) -> EventType {
    if value.get("llm_message").is_some() || entry_type == "MessageEvent" {
        return EventType::Message;
    }
    if value.get("action").is_some() || entry_type == "ActionEvent" {
        return match value.pointer("/action/kind").and_then(Value::as_str) {
            Some("FinishAction") => EventType::Message,
            Some("ThinkAction") => EventType::Summary,
            Some("FileEditorAction" | "StrReplaceEditorAction" | "PlanningFileEditorAction") => {
                EventType::ToolCall
            }
            _ => EventType::ToolCall,
        };
    }
    if value.get("observation").is_some() || entry_type == "ObservationEvent" {
        return match value.pointer("/observation/kind").and_then(Value::as_str) {
            Some(
                "FileEditorObservation"
                | "StrReplaceEditorObservation"
                | "PlanningFileEditorObservation",
            ) => EventType::FileTouched,
            Some("ExecuteBashObservation" | "TerminalObservation") => EventType::CommandOutput,
            _ => EventType::ToolOutput,
        };
    }
    match entry_type {
        "StreamingDeltaEvent" => EventType::Message,
        "CondensationSummaryEvent" | "CondensationEvent" => EventType::Summary,
        "AgentErrorEvent" | "ConversationErrorEvent" | "ServerErrorEvent" => EventType::ToolOutput,
        _ => EventType::Notice,
    }
}

pub(crate) fn openhands_role(value: &Value, entry_type: &str) -> EventRole {
    if let Some(role) = value.pointer("/llm_message/role").and_then(Value::as_str) {
        return provider_role(Some(role));
    }
    match value.get("source").and_then(Value::as_str) {
        Some("user") => EventRole::User,
        Some("agent") => EventRole::Assistant,
        Some("environment" | "hook") => EventRole::Tool,
        Some(source) => provider_role(Some(source)),
        None if entry_type == "ActionEvent" => EventRole::Assistant,
        None if entry_type == "ObservationEvent" => EventRole::Tool,
        _ => EventRole::Unknown,
    }
}

pub(crate) fn openhands_event_text(
    value: &Value,
    entry_type: &str,
    event_type: EventType,
) -> String {
    if let Some(text) = value
        .pointer("/llm_message/content")
        .and_then(provider_value_text)
    {
        return text;
    }
    if let Some(text) = value.get("content").and_then(provider_value_text) {
        return text;
    }
    if let Some(text) = value.pointer("/action/message").and_then(Value::as_str) {
        return text.to_owned();
    }
    if let Some(text) = value.pointer("/action/thought").and_then(Value::as_str) {
        return text.to_owned();
    }
    if let Some(command) = value.pointer("/action/command").and_then(Value::as_str) {
        return command.to_owned();
    }
    if let Some(path) = value.pointer("/action/path").and_then(Value::as_str) {
        let command = value
            .pointer("/action/command")
            .and_then(Value::as_str)
            .unwrap_or("file");
        return format!("{command} {path}");
    }
    if let Some(content) = value
        .pointer("/observation/content")
        .and_then(provider_value_text)
    {
        return content;
    }
    if let Some(output) = value.pointer("/observation/output").and_then(Value::as_str) {
        return output.to_owned();
    }
    if let Some(error) = value
        .pointer("/observation/error")
        .and_then(Value::as_str)
        .or_else(|| value.get("error").and_then(Value::as_str))
    {
        return error.to_owned();
    }
    if let Some(prompt) = value.pointer("/action/prompt").and_then(Value::as_str) {
        return prompt.to_owned();
    }
    if event_type == EventType::Notice {
        format!("OpenHands event: {entry_type}")
    } else {
        serde_json::to_string(value).unwrap_or_else(|_| entry_type.to_owned())
    }
}
