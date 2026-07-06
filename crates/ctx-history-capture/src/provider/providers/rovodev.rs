use std::{
    fs::{self},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, Fidelity, ProviderCaptureEnvelope, ProviderEventEnvelope,
    ProviderSourceTrust,
};
use serde_json::{json, Value};

use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
};
use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::file_touches::provider_file_touches_from_raw_value;
use crate::provider::native::{
    native_event, native_provider_capture, provider_block_event_type, provider_block_text,
    provider_capped_json_value, provider_json_without_keys, provider_message_id,
    provider_message_parts, provider_optional_regular_file, provider_role_from_message,
    provider_string_field, provider_timestamp_from_fields, read_provider_json_file,
    NativeEventDraft, NativeSessionDraft,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    PROVIDER_MAX_PREVIEW_CHARS, ROVODEV_SOURCE_FORMAT,
};

pub(crate) struct RovoDevSessionSource {
    pub(crate) session_dir: PathBuf,
    pub(crate) context_path: PathBuf,
    pub(crate) metadata_path: Option<PathBuf>,
    pub(crate) provider_session_id: String,
}

pub(crate) fn normalize_rovodev_sessions(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let mut session_sources = Vec::new();
    collect_rovodev_session_sources(path, &mut session_sources)?;
    session_sources.sort_by(|left, right| left.context_path.cmp(&right.context_path));
    session_sources.dedup_by(|left, right| left.context_path == right.context_path);
    if session_sources.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no Rovo Dev session_context.json files found",
        });
    }

    let mut merged = ProviderNormalizationResult::default();
    for source in session_sources {
        let mut result = normalize_rovodev_session_source(&source, context)?;
        merged.summary.merge(result.summary);
        merged.captures.append(&mut result.captures);
        merged.files_touched.append(&mut result.files_touched);
    }
    Ok(merged)
}

pub(crate) fn collect_rovodev_session_sources(
    root: &Path,
    sessions: &mut Vec<RovoDevSessionSource>,
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
        if root.file_name().and_then(|name| name.to_str()) == Some("session_context.json") {
            if let Some(session_dir) = root.parent() {
                if let Some(source) = rovodev_session_source_from_dir(session_dir)? {
                    sessions.push(source);
                }
            }
        }
        return Ok(());
    }
    if !file_type.is_dir() {
        return Ok(());
    }

    if let Some(source) = rovodev_session_source_from_dir(root)? {
        sessions.push(source);
        return Ok(());
    }

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            collect_rovodev_session_sources(&entry.path(), sessions)?;
        }
    }
    Ok(())
}

pub(crate) fn rovodev_session_source_from_dir(dir: &Path) -> Result<Option<RovoDevSessionSource>> {
    let context_path = dir.join("session_context.json");
    if !context_path.is_file() {
        return Ok(None);
    }
    ensure_regular_provider_transcript_file(&context_path)?;
    let metadata_path = provider_optional_regular_file(&dir.join("metadata.json"))?;
    let provider_session_id = dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
            path: dir.to_path_buf(),
            reason: "Rovo Dev session directory is missing a session id",
        })?;
    Ok(Some(RovoDevSessionSource {
        session_dir: dir.to_path_buf(),
        context_path,
        metadata_path,
        provider_session_id,
    }))
}

pub(crate) fn normalize_rovodev_session_source(
    source: &RovoDevSessionSource,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let mut result = ProviderNormalizationResult::default();
    let context_json =
        match read_provider_json_file(&source.context_path, "Rovo Dev session_context.json") {
            Ok(value) => value,
            Err(err) => {
                push_provider_import_failure(&mut result.summary, 1, err.to_string());
                return Ok(result);
            }
        };
    let metadata = match source.metadata_path.as_deref() {
        Some(path) => match read_provider_json_file(path, "Rovo Dev metadata.json") {
            Ok(value) => value,
            Err(err) => {
                push_provider_import_failure(&mut result.summary, 1, err.to_string());
                Value::Null
            }
        },
        None => Value::Null,
    };
    let Some(messages) = rovodev_message_history(&context_json) else {
        push_provider_import_failure(
            &mut result.summary,
            1,
            "Rovo Dev session_context.json is missing message_history array".to_owned(),
        );
        return Ok(result);
    };
    let provider_session_id = provider_string_field(&metadata, &["session_id", "sessionId"])
        .or_else(|| provider_string_field(&context_json, &["session_id", "sessionId"]))
        .unwrap_or_else(|| source.provider_session_id.clone());
    let parent_provider_session_id = provider_string_field(
        &metadata,
        &[
            "parent_session_id",
            "parentSessionId",
            "forked_from_session_id",
            "forkedFromSessionId",
            "fork_parent_id",
        ],
    );
    let started_at = provider_timestamp_from_fields(
        &metadata,
        &["created_at", "createdAt", "started_at", "startedAt"],
    )
    .or_else(|| messages.iter().find_map(rovodev_message_timestamp))
    .unwrap_or(context.imported_at);
    let ended_at = provider_timestamp_from_fields(
        &metadata,
        &["updated_at", "updatedAt", "last_updated", "lastUpdated"],
    )
    .or_else(|| messages.iter().rev().find_map(rovodev_message_timestamp));
    let cwd = provider_string_field(
        &metadata,
        &[
            "workspace_path",
            "workspacePath",
            "working_directory",
            "workingDirectory",
            "cwd",
        ],
    );
    let raw_source_path = source.context_path.display().to_string();

    if messages.is_empty() {
        result.captures.push((
            0,
            rovodev_capture(
                RovoDevCaptureDraft {
                    provider_session_id,
                    parent_provider_session_id,
                    started_at,
                    ended_at,
                    cwd,
                    source,
                    context_json: &context_json,
                    metadata: &metadata,
                    message_count: 0,
                    event: None,
                },
                context,
            ),
        ));
        return Ok(result);
    }

    let message_count = messages.len();
    for (index, message) in messages.iter().enumerate() {
        let line = index + 1;
        let occurred_at = rovodev_message_timestamp(message).unwrap_or(started_at);
        let event = rovodev_event(
            &provider_session_id,
            index as u64,
            message,
            occurred_at,
            source,
        );
        result
            .files_touched
            .extend(provider_file_touches_from_raw_value(
                CaptureProvider::RovoDev,
                &provider_session_id,
                ROVODEV_SOURCE_FORMAT,
                Some(raw_source_path.as_str()),
                message,
                &event,
                line,
            ));
        result.captures.push((
            line,
            rovodev_capture(
                RovoDevCaptureDraft {
                    provider_session_id: provider_session_id.clone(),
                    parent_provider_session_id: parent_provider_session_id.clone(),
                    started_at,
                    ended_at,
                    cwd: cwd.clone(),
                    source,
                    context_json: &context_json,
                    metadata: &metadata,
                    message_count,
                    event: Some(event),
                },
                context,
            ),
        ));
    }
    Ok(result)
}

pub(crate) struct RovoDevCaptureDraft<'a> {
    pub(crate) provider_session_id: String,
    pub(crate) parent_provider_session_id: Option<String>,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) cwd: Option<String>,
    pub(crate) source: &'a RovoDevSessionSource,
    pub(crate) context_json: &'a Value,
    pub(crate) metadata: &'a Value,
    pub(crate) message_count: usize,
    pub(crate) event: Option<ProviderEventEnvelope>,
}

pub(crate) fn rovodev_capture(
    draft: RovoDevCaptureDraft<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let is_primary = draft.parent_provider_session_id.is_none();
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::RovoDev,
            source_format: ROVODEV_SOURCE_FORMAT,
            provider_session_id: draft.provider_session_id.clone(),
            parent_provider_session_id: draft.parent_provider_session_id.clone(),
            root_provider_session_id: draft.parent_provider_session_id.clone(),
            external_agent_id: provider_string_field(
                draft.metadata,
                &["agent_id", "agentId", "agent_name", "agentName"],
            ),
            agent_type: if is_primary {
                AgentType::Primary
            } else {
                AgentType::Subagent
            },
            role_hint: Some(if is_primary { "primary" } else { "subagent" }.to_owned()),
            is_primary,
            started_at: draft.started_at,
            ended_at: draft.ended_at,
            cwd: draft.cwd,
            fidelity: Fidelity::Imported,
            raw_source_path: draft.source.context_path.display().to_string(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": ROVODEV_SOURCE_FORMAT,
                "source_path": draft.source.context_path.display().to_string(),
                "metadata_path": draft.source.metadata_path.as_ref().map(|path| path.display().to_string()),
                "session_dir": draft.source.session_dir.display().to_string(),
                "upstream_schema_anchor": {
                    "docs": "https://support.atlassian.com/rovo/docs/manage-sessions-in-rovo-dev-cli/"
                },
            }),
            session_metadata: json!({
                "source_format": ROVODEV_SOURCE_FORMAT,
                "provider": CaptureProvider::RovoDev.as_str(),
                "session_id": draft.provider_session_id,
                "title": provider_string_field(draft.metadata, &["title", "name"]),
                "workspace_path": provider_string_field(draft.metadata, &["workspace_path", "workspacePath"]),
                "message_count": draft.message_count,
                "metadata": provider_capped_json_value(draft.metadata, PROVIDER_MAX_PREVIEW_CHARS),
                "context": provider_capped_json_value(&provider_json_without_keys(draft.context_json, &["message_history", "messages"]), PROVIDER_MAX_PREVIEW_CHARS),
            }),
        },
        context,
        draft.event,
    )
}

pub(crate) fn rovodev_event(
    provider_session_id: &str,
    event_index: u64,
    message: &Value,
    occurred_at: DateTime<Utc>,
    source: &RovoDevSessionSource,
) -> ProviderEventEnvelope {
    let role_text = message
        .get("role")
        .or_else(|| message.get("kind"))
        .or_else(|| message.get("type"))
        .and_then(Value::as_str);
    native_event(NativeEventDraft {
        provider: CaptureProvider::RovoDev,
        source_format: ROVODEV_SOURCE_FORMAT,
        provider_session_id: provider_session_id.to_owned(),
        provider_event_index: event_index,
        provider_event_hash: Some(provider_message_id(message, event_index)),
        cursor: format!(
            "{}:{}",
            source.context_path.display(),
            provider_message_id(message, event_index)
        ),
        event_type: provider_block_event_type(message, role_text),
        role: Some(provider_role_from_message(message, role_text)),
        occurred_at,
        text: provider_block_text(message).unwrap_or_else(|| "Rovo Dev message".to_owned()),
        body: message.clone(),
        metadata: json!({
            "source": ROVODEV_SOURCE_FORMAT,
            "source_format": ROVODEV_SOURCE_FORMAT,
            "message_id": provider_message_id(message, event_index),
            "role": role_text,
            "kind": message.get("kind").and_then(Value::as_str),
            "part_count": provider_message_parts(message).map(|parts| parts.len()),
        }),
    })
}

pub(crate) fn rovodev_message_history(value: &Value) -> Option<&Vec<Value>> {
    value
        .get("message_history")
        .or_else(|| value.pointer("/session_context/message_history"))
        .or_else(|| value.get("messages"))
        .or_else(|| value.pointer("/conversation/messages"))
        .and_then(Value::as_array)
}

pub(crate) fn rovodev_message_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    provider_timestamp_from_fields(
        value,
        &[
            "timestamp",
            "created_at",
            "createdAt",
            "updated_at",
            "updatedAt",
            "user_sent_time",
        ],
    )
}
