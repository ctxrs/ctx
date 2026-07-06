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

use crate::provider::providers::native_jsonl::native_jsonl_missing_reason;

use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
    read_provider_jsonl_line, read_text_file_limited,
};
use crate::common::time::parse_rfc3339_utc;
use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::file_touches::provider_file_touches_from_raw_value;
use crate::provider::native::{
    native_event, native_provider_capture, provider_capped_json_value, provider_local_preview,
    provider_role, provider_timestamp_seconds_to_datetime, provider_value_text, NativeEventDraft,
    NativeSessionDraft,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportSummary, ProviderNormalizationResult,
    Result, MAX_PROVIDER_JSONL_LINE_BYTES, MUX_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

pub(crate) struct MuxSessionSource {
    pub(crate) session_dir: PathBuf,
    pub(crate) chat_path: Option<PathBuf>,
    pub(crate) partial_path: Option<PathBuf>,
    pub(crate) metadata_path: Option<PathBuf>,
    pub(crate) provider_session_id: String,
    pub(crate) parent_provider_session_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct MuxMessageRow {
    pub(crate) line_number: usize,
    pub(crate) source_path: PathBuf,
    pub(crate) value: Value,
    pub(crate) is_partial: bool,
}

pub(crate) fn normalize_mux_sessions(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let mut session_sources = Vec::new();
    collect_mux_session_sources(path, &mut session_sources)?;
    session_sources.sort_by(|left, right| left.session_dir.cmp(&right.session_dir));
    session_sources.dedup_by(|left, right| left.session_dir == right.session_dir);
    if session_sources.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: native_jsonl_missing_reason(CaptureProvider::Mux),
        });
    }

    let mut merged = ProviderNormalizationResult::default();
    for source in session_sources {
        let mut result = normalize_mux_session_source(&source, context)?;
        merged.summary.merge(result.summary);
        merged.captures.append(&mut result.captures);
        merged.files_touched.append(&mut result.files_touched);
    }
    Ok(merged)
}

pub(crate) fn collect_mux_session_sources(
    root: &Path,
    sessions: &mut Vec<MuxSessionSource>,
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
        if matches!(
            root.file_name().and_then(|name| name.to_str()),
            Some("chat.jsonl" | "partial.json")
        ) {
            if let Some(session_dir) = root.parent() {
                if let Some(source) = mux_session_source_from_dir(session_dir)? {
                    sessions.push(source);
                }
            }
        }
        return Ok(());
    }
    if !file_type.is_dir() {
        return Ok(());
    }

    if let Some(source) = mux_session_source_from_dir(root)? {
        sessions.push(source);
    }

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            collect_mux_session_sources(&entry.path(), sessions)?;
        }
    }
    Ok(())
}

pub(crate) fn mux_session_source_from_dir(dir: &Path) -> Result<Option<MuxSessionSource>> {
    let chat_path = mux_optional_regular_file(&dir.join("chat.jsonl"))?;
    let partial_path = mux_optional_regular_file(&dir.join("partial.json"))?;
    if chat_path.is_none() && partial_path.is_none() {
        return Ok(None);
    }
    let metadata_path = mux_optional_regular_file(&dir.join("metadata.json"))?;
    let provider_session_id = dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
            path: dir.to_path_buf(),
            reason: "Mux session directory is missing a workspace id",
        })?;
    let parent_provider_session_id = mux_parent_session_id_from_path(dir);
    Ok(Some(MuxSessionSource {
        session_dir: dir.to_path_buf(),
        chat_path,
        partial_path,
        metadata_path,
        provider_session_id,
        parent_provider_session_id,
    }))
}

pub(crate) fn mux_optional_regular_file(path: &Path) -> Result<Option<PathBuf>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            ensure_regular_provider_transcript_file(path)?;
            Ok(Some(path.to_path_buf()))
        }
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "symlinked provider transcript files are rejected",
            })
        }
        Ok(_) => Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Mux transcript files must be regular files",
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

pub(crate) fn mux_parent_session_id_from_path(dir: &Path) -> Option<String> {
    let parent = dir.parent()?;
    if parent.file_name().and_then(|name| name.to_str()) != Some("subagent-transcripts") {
        return None;
    }
    parent
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
}

pub(crate) fn normalize_mux_session_source(
    source: &MuxSessionSource,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let mut result = ProviderNormalizationResult::default();
    let metadata = read_mux_metadata(source.metadata_path.as_deref(), &mut result.summary);
    let mut rows = Vec::new();

    if let Some(chat_path) = &source.chat_path {
        let file = File::open(chat_path)?;
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let mut line_number = 0usize;
        while read_provider_jsonl_line(&mut reader, &mut line)? {
            line_number += 1;
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            match serde_json::from_slice::<Value>(&line) {
                Ok(value) if value.is_object() => rows.push(MuxMessageRow {
                    line_number,
                    source_path: chat_path.clone(),
                    value,
                    is_partial: false,
                }),
                Ok(_) => push_provider_import_failure(
                    &mut result.summary,
                    line_number,
                    "Mux chat.jsonl line must contain a JSON object".to_owned(),
                ),
                Err(err) => push_provider_import_failure(
                    &mut result.summary,
                    line_number,
                    format!("malformed JSONL: {err}"),
                ),
            }
        }
    }

    if let Some(partial_path) = &source.partial_path {
        match read_mux_partial_row(partial_path) {
            Ok(Some(partial)) => mux_merge_partial_row(&mut rows, partial),
            Ok(None) => {}
            Err(err) => push_provider_import_failure(
                &mut result.summary,
                1,
                format!("invalid Mux partial.json: {err}"),
            ),
        }
    }

    let provider_session_id =
        mux_session_id_from_rows(&rows).unwrap_or_else(|| source.provider_session_id.clone());
    let parent_provider_session_id = source.parent_provider_session_id.clone().or_else(|| {
        mux_string_pointer(
            &metadata,
            &[
                "/parentWorkspaceId",
                "/parentTaskId",
                "/parentSessionId",
                "/parent_session_id",
            ],
        )
    });
    let root_provider_session_id = mux_string_pointer(
        &metadata,
        &["/rootWorkspaceId", "/rootTaskId", "/rootSessionId"],
    )
    .or_else(|| parent_provider_session_id.clone());
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
    let started_at = mux_metadata_timestamp(&metadata)
        .or_else(|| {
            rows.iter()
                .filter_map(|row| mux_message_timestamp_opt(&row.value))
                .min()
        })
        .unwrap_or(context.imported_at);
    let ended_at = rows
        .iter()
        .rev()
        .find_map(|row| mux_message_timestamp_opt(&row.value));
    let cwd = mux_string_pointer(
        &metadata,
        &["/projectPath", "/workspacePath", "/cwd", "/repoPath"],
    );
    let model = mux_string_pointer(&metadata, &["/model"])
        .or_else(|| rows.iter().find_map(|row| mux_message_model(&row.value)));

    if rows.is_empty() {
        result.captures.push((
            0,
            mux_capture(
                MuxCaptureDraft {
                    provider_session_id,
                    parent_provider_session_id,
                    root_provider_session_id,
                    agent_type,
                    role_hint: role_hint.to_owned(),
                    is_primary: agent_type == AgentType::Primary,
                    started_at,
                    ended_at,
                    cwd,
                    model,
                    metadata: &metadata,
                    message_count: 0,
                    source,
                    event: None,
                },
                context,
            ),
        ));
        return Ok(result);
    }

    let message_count = rows.len();
    for (event_index, row) in rows.iter().enumerate() {
        let occurred_at = mux_message_timestamp_opt(&row.value).unwrap_or(started_at);
        let event = mux_event(
            &provider_session_id,
            event_index as u64,
            row,
            occurred_at,
            model.as_deref(),
        );
        let raw_source_path = row.source_path.display().to_string();
        result
            .files_touched
            .extend(provider_file_touches_from_raw_value(
                CaptureProvider::Mux,
                &provider_session_id,
                MUX_SOURCE_FORMAT,
                Some(raw_source_path.as_str()),
                &row.value,
                &event,
                row.line_number,
            ));
        result.captures.push((
            row.line_number,
            mux_capture(
                MuxCaptureDraft {
                    provider_session_id: provider_session_id.clone(),
                    parent_provider_session_id: parent_provider_session_id.clone(),
                    root_provider_session_id: root_provider_session_id.clone(),
                    agent_type,
                    role_hint: role_hint.to_owned(),
                    is_primary: agent_type == AgentType::Primary,
                    started_at,
                    ended_at,
                    cwd: cwd.clone(),
                    model: model.clone(),
                    metadata: &metadata,
                    message_count,
                    source,
                    event: Some(event),
                },
                context,
            ),
        ));
    }

    Ok(result)
}

pub(crate) fn read_mux_partial_row(path: &Path) -> Result<Option<MuxMessageRow>> {
    let raw = read_text_file_limited(path, MAX_PROVIDER_JSONL_LINE_BYTES, "Mux partial.json")?;
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(&raw)?;
    if !value.is_object() {
        return Err(CaptureError::InvalidPayload(
            "Mux partial.json must contain a JSON object".to_owned(),
        ));
    }
    Ok(Some(MuxMessageRow {
        line_number: 1,
        source_path: path.to_path_buf(),
        value,
        is_partial: true,
    }))
}

pub(crate) fn mux_merge_partial_row(rows: &mut Vec<MuxMessageRow>, partial: MuxMessageRow) {
    let Some(sequence) = mux_history_sequence(&partial.value) else {
        rows.push(partial);
        return;
    };
    if let Some(index) = rows
        .iter()
        .position(|row| mux_history_sequence(&row.value) == Some(sequence))
    {
        if mux_parts_len(&partial.value) > mux_parts_len(&rows[index].value) {
            rows[index] = partial;
        }
        return;
    }
    let insert_at = rows
        .iter()
        .position(|row| mux_history_sequence(&row.value).is_some_and(|seq| seq > sequence))
        .unwrap_or(rows.len());
    rows.insert(insert_at, partial);
}

pub(crate) struct MuxCaptureDraft<'a> {
    pub(crate) provider_session_id: String,
    pub(crate) parent_provider_session_id: Option<String>,
    pub(crate) root_provider_session_id: Option<String>,
    pub(crate) agent_type: AgentType,
    pub(crate) role_hint: String,
    pub(crate) is_primary: bool,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) cwd: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) metadata: &'a Value,
    pub(crate) message_count: usize,
    pub(crate) source: &'a MuxSessionSource,
    pub(crate) event: Option<ProviderEventEnvelope>,
}

pub(crate) fn mux_capture(
    draft: MuxCaptureDraft<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let primary_path = mux_source_primary_path(draft.source);
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::Mux,
            source_format: MUX_SOURCE_FORMAT,
            provider_session_id: draft.provider_session_id.clone(),
            parent_provider_session_id: draft.parent_provider_session_id.clone(),
            root_provider_session_id: draft.root_provider_session_id,
            external_agent_id: mux_string_pointer(draft.metadata, &["/agentId", "/agent_id"]),
            agent_type: draft.agent_type,
            role_hint: Some(draft.role_hint),
            is_primary: draft.is_primary,
            started_at: draft.started_at,
            ended_at: draft.ended_at,
            cwd: draft.cwd,
            fidelity: Fidelity::Imported,
            raw_source_path: primary_path.display().to_string(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": MUX_SOURCE_FORMAT,
                "source_path": primary_path.display().to_string(),
                "chat_path": draft.source.chat_path.as_ref().map(|path| path.display().to_string()),
                "partial_path": draft.source.partial_path.as_ref().map(|path| path.display().to_string()),
                "metadata_path": draft.source.metadata_path.as_ref().map(|path| path.display().to_string()),
                "session_dir": draft.source.session_dir.display().to_string(),
            }),
            session_metadata: json!({
                "source_format": MUX_SOURCE_FORMAT,
                "provider": CaptureProvider::Mux.as_str(),
                "workspace_id": draft.provider_session_id,
                "parent_workspace_id": draft.parent_provider_session_id,
                "model": draft.model,
                "message_count": draft.message_count,
                "has_partial": draft.source.partial_path.is_some(),
                "metadata": provider_capped_json_value(draft.metadata, PROVIDER_MAX_PREVIEW_CHARS),
            }),
        },
        context,
        draft.event,
    )
}

pub(crate) fn mux_event(
    provider_session_id: &str,
    event_index: u64,
    row: &MuxMessageRow,
    occurred_at: DateTime<Utc>,
    model: Option<&str>,
) -> ProviderEventEnvelope {
    let role = row
        .value
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let event_type = mux_event_type(&row.value);
    let model_value = model
        .map(str::to_owned)
        .or_else(|| mux_message_model(&row.value));
    native_event(NativeEventDraft {
        provider: CaptureProvider::Mux,
        source_format: MUX_SOURCE_FORMAT,
        provider_session_id: provider_session_id.to_owned(),
        provider_event_index: event_index,
        provider_event_hash: Some(mux_event_id(
            &row.value,
            row.line_number,
            role,
            row.is_partial,
        )),
        cursor: format!("{}:line:{}", row.source_path.display(), row.line_number),
        event_type,
        role: Some(provider_role(Some(role))),
        occurred_at,
        text: mux_event_text(&row.value, event_type),
        body: row.value.clone(),
        metadata: json!({
            "source": MUX_SOURCE_FORMAT,
            "source_format": MUX_SOURCE_FORMAT,
            "line": row.line_number,
            "is_partial": row.is_partial,
            "role": role,
            "message_id": row.value.get("id").and_then(Value::as_str),
            "workspace_id": row.value.get("workspaceId").and_then(Value::as_str),
            "history_sequence": mux_history_sequence(&row.value),
            "model": model_value,
            "usage": row.value.pointer("/metadata/usage").map(|usage| provider_capped_json_value(usage, PROVIDER_MAX_PREVIEW_CHARS)),
            "provider_metadata": row.value.pointer("/metadata/providerMetadata").map(|metadata| provider_capped_json_value(metadata, PROVIDER_MAX_PREVIEW_CHARS)),
            "mux_metadata": row.value.pointer("/metadata/muxMetadata").map(|metadata| provider_capped_json_value(metadata, PROVIDER_MAX_PREVIEW_CHARS)),
            "partial": row.value.pointer("/metadata/partial").and_then(Value::as_bool),
        }),
    })
}

pub(crate) fn mux_event_type(value: &Value) -> EventType {
    if mux_is_summary_message(value) {
        return EventType::Summary;
    }
    if value.get("role").and_then(Value::as_str) == Some("system") {
        return EventType::Notice;
    }
    let mut saw_tool_call = false;
    if let Some(parts) = value.get("parts").and_then(Value::as_array) {
        for part in parts {
            if part.get("type").and_then(Value::as_str) != Some("dynamic-tool") {
                continue;
            }
            let state = part.get("state").and_then(Value::as_str);
            if matches!(state, Some("output-available" | "output-redacted"))
                || part.get("output").is_some()
            {
                return EventType::ToolOutput;
            }
            saw_tool_call = true;
        }
    }
    if saw_tool_call {
        EventType::ToolCall
    } else {
        EventType::Message
    }
}

pub(crate) fn mux_is_summary_message(value: &Value) -> bool {
    value
        .pointer("/metadata/compacted")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || value
            .pointer("/metadata/compactionBoundary")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || value.pointer("/metadata/contextBoundaryKind").is_some()
        || value
            .pointer("/metadata/muxMetadata/type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.contains("compaction") || kind.contains("summary"))
}

pub(crate) fn mux_event_text(value: &Value, event_type: EventType) -> String {
    let mut rendered = Vec::new();
    if let Some(parts) = value.get("parts").and_then(Value::as_array) {
        for part in parts {
            match part.get("type").and_then(Value::as_str) {
                Some("text" | "reasoning") => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        rendered.push(text.to_owned());
                    }
                }
                Some("dynamic-tool") => rendered.push(mux_tool_part_text(part)),
                Some("file") => {
                    if let Some(text) = mux_file_part_text(part) {
                        rendered.push(text);
                    }
                }
                _ => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        rendered.push(text.to_owned());
                    }
                }
            }
        }
    }
    if !rendered.is_empty() {
        return rendered.join("\n");
    }
    if let Some(text) = value
        .get("content")
        .or_else(|| value.get("message"))
        .and_then(provider_value_text)
    {
        return text;
    }
    match event_type {
        EventType::ToolOutput => "Mux tool output".to_owned(),
        EventType::ToolCall => "Mux tool call".to_owned(),
        EventType::Summary => "Mux summary".to_owned(),
        EventType::Notice => "Mux notice".to_owned(),
        _ => "Mux message".to_owned(),
    }
}

pub(crate) fn mux_tool_part_text(part: &Value) -> String {
    let name = part
        .get("toolName")
        .or_else(|| part.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let state = part.get("state").and_then(Value::as_str);
    let prefix = if matches!(state, Some("output-available" | "output-redacted"))
        || part.get("output").is_some()
    {
        "tool output"
    } else {
        "tool call"
    };
    let mut text = format!("{prefix}: {name}");
    if let Some(input) = part.get("input") {
        text.push('\n');
        text.push_str("input: ");
        text.push_str(&mux_value_preview(input));
    }
    if let Some(output) = part.get("output") {
        text.push('\n');
        text.push_str("output: ");
        text.push_str(&mux_value_preview(output));
    }
    if let Some(nested) = part.get("nestedCalls").and_then(Value::as_array) {
        let names = nested
            .iter()
            .filter_map(|call| {
                call.get("toolName")
                    .or_else(|| call.get("name"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>();
        if !names.is_empty() {
            text.push('\n');
            text.push_str("nested tools: ");
            text.push_str(&names.join(", "));
        }
    }
    text
}

pub(crate) fn mux_file_part_text(part: &Value) -> Option<String> {
    let label = part
        .get("filename")
        .or_else(|| part.get("name"))
        .or_else(|| part.get("mediaType"))
        .or_else(|| part.get("mimeType"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            part.get("url")
                .and_then(Value::as_str)
                .filter(|url| !url.starts_with("data:") && url.len() < 256)
                .map(str::to_owned)
        })?;
    Some(format!("file: {label}"))
}

pub(crate) fn mux_value_preview(value: &Value) -> String {
    let raw = provider_value_text(value)
        .or_else(|| serde_json::to_string(value).ok())
        .unwrap_or_else(|| value.to_string());
    provider_local_preview(&raw, PROVIDER_MAX_PREVIEW_CHARS).0
}

pub(crate) fn mux_event_id(
    value: &Value,
    line_number: usize,
    role: &str,
    is_partial: bool,
) -> String {
    let prefix = if is_partial { "partial:" } else { "" };
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(|id| format!("{prefix}{id}"))
        .or_else(|| {
            mux_history_sequence(value)
                .map(|sequence| format!("{prefix}historySequence:{sequence}"))
        })
        .unwrap_or_else(|| format!("{prefix}{role}:line-{line_number}"))
}

pub(crate) fn mux_history_sequence(value: &Value) -> Option<i64> {
    match value.pointer("/metadata/historySequence") {
        Some(Value::Number(number)) => number
            .as_i64()
            .or_else(|| number.as_u64().and_then(|value| i64::try_from(value).ok())),
        Some(Value::String(raw)) => raw.parse::<i64>().ok(),
        _ => None,
    }
}

pub(crate) fn mux_parts_len(value: &Value) -> usize {
    value
        .get("parts")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

pub(crate) fn mux_session_id_from_rows(rows: &[MuxMessageRow]) -> Option<String> {
    rows.iter().find_map(|row| {
        row.value
            .get("workspaceId")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned)
    })
}

pub(crate) fn mux_message_model(value: &Value) -> Option<String> {
    mux_string_pointer(value, &["/metadata/model", "/model"])
}

pub(crate) fn mux_message_timestamp_opt(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("createdAt")
        .and_then(mux_value_timestamp)
        .or_else(|| {
            value
                .pointer("/metadata/timestamp")
                .and_then(mux_value_timestamp)
        })
        .or_else(|| {
            value
                .get("parts")
                .and_then(Value::as_array)
                .and_then(|parts| {
                    parts
                        .iter()
                        .find_map(|part| part.get("timestamp").and_then(mux_value_timestamp))
                })
        })
}

pub(crate) fn mux_metadata_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    ["/createdAt", "/createdAtMs", "/updatedAt", "/updatedAtMs"]
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(mux_value_timestamp))
}

pub(crate) fn mux_value_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::String(raw) => parse_rfc3339_utc(raw).or_else(|| {
            raw.parse::<f64>()
                .ok()
                .and_then(provider_timestamp_seconds_to_datetime)
        }),
        Value::Number(number) => number
            .as_f64()
            .and_then(provider_timestamp_seconds_to_datetime),
        _ => None,
    }
}

pub(crate) fn read_mux_metadata(path: Option<&Path>, summary: &mut ProviderImportSummary) -> Value {
    let Some(path) = path else {
        return Value::Null;
    };
    match read_text_file_limited(path, MAX_PROVIDER_JSONL_LINE_BYTES, "Mux metadata.json") {
        Ok(raw) => match serde_json::from_str::<Value>(&raw) {
            Ok(value) if value.is_object() => value,
            Ok(_) => {
                push_provider_import_failure(
                    summary,
                    0,
                    "Mux metadata.json must contain a JSON object".to_owned(),
                );
                Value::Null
            }
            Err(err) => {
                push_provider_import_failure(
                    summary,
                    0,
                    format!("invalid Mux metadata.json: {err}"),
                );
                Value::Null
            }
        },
        Err(err) => {
            push_provider_import_failure(
                summary,
                0,
                format!("could not read Mux metadata.json: {err}"),
            );
            Value::Null
        }
    }
}

pub(crate) fn mux_string_pointer(value: &Value, pointers: &[&str]) -> Option<String> {
    pointers.iter().find_map(|pointer| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .filter(|raw| !raw.trim().is_empty())
            .map(str::to_owned)
    })
}

pub(crate) fn mux_source_primary_path(source: &MuxSessionSource) -> &Path {
    source
        .chat_path
        .as_deref()
        .or(source.partial_path.as_deref())
        .unwrap_or(source.session_dir.as_path())
}
