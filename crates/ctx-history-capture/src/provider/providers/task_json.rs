use std::{
    collections::BTreeMap,
    fs::{self},
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

use crate::common::io::ensure_regular_provider_transcript_file;
use crate::provider::file_touches::provider_file_touches_from_raw_value;
use crate::provider::importer::provider_cursor_stream;
use crate::provider::native::{
    provider_capped_json, provider_local_preview, provider_role, provider_value_text,
    task_json_string_field, task_json_time_field,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportFailure, ProviderImportSummary,
    ProviderNormalizationResult, Result, CLINE_TASK_JSON_SOURCE_FORMAT,
    MAX_PROVIDER_JSONL_LINE_BYTES, PROVIDER_MAX_PREVIEW_CHARS, PROVIDER_MAX_TEXT_CHARS,
    ROO_TASK_JSON_SOURCE_FORMAT,
};

#[derive(Debug, Clone, Copy)]
pub(crate) struct TaskJsonProviderSpec {
    pub(crate) provider: CaptureProvider,
    pub(crate) source_format: &'static str,
    pub(crate) display_name: &'static str,
    pub(crate) api_file: &'static str,
    pub(crate) ui_file: &'static str,
    pub(crate) metadata_file: &'static str,
    pub(crate) history_item_file: Option<&'static str>,
    pub(crate) index_file: Option<&'static str>,
    pub(crate) fallback_api_file: Option<&'static str>,
}

pub(crate) fn task_json_provider(provider: CaptureProvider) -> TaskJsonProviderSpec {
    match provider {
        CaptureProvider::RooCode => TaskJsonProviderSpec {
            provider,
            source_format: ROO_TASK_JSON_SOURCE_FORMAT,
            display_name: "Roo Code",
            api_file: "api_conversation_history.json",
            ui_file: "ui_messages.json",
            metadata_file: "task_metadata.json",
            history_item_file: Some("history_item.json"),
            index_file: Some("_index.json"),
            fallback_api_file: Some("claude_messages.json"),
        },
        _ => TaskJsonProviderSpec {
            provider: CaptureProvider::Cline,
            source_format: CLINE_TASK_JSON_SOURCE_FORMAT,
            display_name: "Cline",
            api_file: "api_conversation_history.json",
            ui_file: "ui_messages.json",
            metadata_file: "task_metadata.json",
            history_item_file: None,
            index_file: None,
            fallback_api_file: None,
        },
    }
}

pub(crate) fn normalize_task_json_history(
    path: &Path,
    context: &ProviderAdapterContext,
    spec: TaskJsonProviderSpec,
) -> Result<ProviderNormalizationResult> {
    let mut task_dirs = collect_task_json_dirs(path, spec)?;
    task_dirs.sort();
    task_dirs.dedup();
    if task_dirs.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: task_json_missing_reason(spec.provider),
        });
    }

    let history_items = task_json_root_history_items(path, spec, context);
    let mut merged = ProviderNormalizationResult::default();
    for (task_ordinal, task_dir) in task_dirs.iter().enumerate() {
        let mut result = normalize_task_json_task_dir(
            task_dir,
            &history_items,
            context,
            spec,
            task_ordinal.saturating_add(1),
        )?;
        merged.summary.merge(result.summary);
        merged.captures.append(&mut result.captures);
        merged.files_touched.append(&mut result.files_touched);
    }

    Ok(merged)
}

pub(crate) fn task_json_missing_reason(provider: CaptureProvider) -> &'static str {
    match provider {
        CaptureProvider::RooCode => {
            "no Roo Code task JSON directories with api_conversation_history.json, ui_messages.json, history_item.json, _index.json, or claude_messages.json were found"
        }
        _ => {
            "no Cline task JSON directories with api_conversation_history.json, ui_messages.json, or task_metadata.json were found"
        }
    }
}

pub(crate) fn collect_task_json_dirs(
    path: &Path,
    spec: TaskJsonProviderSpec,
) -> Result<Vec<PathBuf>> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_file() {
        ensure_regular_provider_transcript_file(path)?;
        if task_json_file_name_is_marker(path, spec) {
            return Ok(path.parent().map(Path::to_path_buf).into_iter().collect());
        }
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: task_json_missing_reason(spec.provider),
        });
    }

    if !metadata.file_type().is_dir() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: task_json_missing_reason(spec.provider),
        });
    }

    if task_json_dir_has_marker(path, spec) {
        return Ok(vec![path.to_path_buf()]);
    }

    let task_roots = [path.join("tasks"), path.to_path_buf()]
        .into_iter()
        .filter(|candidate| candidate.is_dir())
        .collect::<Vec<_>>();
    let mut out = Vec::new();
    for root in task_roots {
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                let candidate = entry.path();
                if task_json_dir_has_marker(&candidate, spec) {
                    out.push(candidate);
                }
            }
        }
    }
    Ok(out)
}

pub(crate) fn task_json_file_name_is_marker(path: &Path, spec: TaskJsonProviderSpec) -> bool {
    let name = path.file_name().and_then(|name| name.to_str());
    name == Some(spec.api_file)
        || name == Some(spec.ui_file)
        || name == Some(spec.metadata_file)
        || spec
            .history_item_file
            .is_some_and(|file| name == Some(file))
        || spec.index_file.is_some_and(|file| name == Some(file))
        || spec
            .fallback_api_file
            .is_some_and(|file| name == Some(file))
}

pub(crate) fn task_json_dir_has_marker(path: &Path, spec: TaskJsonProviderSpec) -> bool {
    path.join(spec.api_file).is_file()
        || path.join(spec.ui_file).is_file()
        || path.join(spec.metadata_file).is_file()
        || spec
            .history_item_file
            .is_some_and(|file| path.join(file).is_file())
        || spec
            .index_file
            .is_some_and(|file| path.join(file).is_file())
        || spec
            .fallback_api_file
            .is_some_and(|file| path.join(file).is_file())
}

pub(crate) fn task_json_root_history_items(
    path: &Path,
    spec: TaskJsonProviderSpec,
    context: &ProviderAdapterContext,
) -> BTreeMap<String, Value> {
    if spec.provider != CaptureProvider::Cline {
        return BTreeMap::new();
    }
    let mut candidates = Vec::new();
    if path.is_dir() {
        candidates.push(path.join("state").join("taskHistory.json"));
        candidates.push(path.join("..").join("state").join("taskHistory.json"));
    }
    if let Some(parent) = path.parent() {
        candidates.push(parent.join("state").join("taskHistory.json"));
        if let Some(grandparent) = parent.parent() {
            candidates.push(grandparent.join("state").join("taskHistory.json"));
        }
    }

    for candidate in candidates {
        let Ok(value) = read_task_json_value(&candidate, context) else {
            continue;
        };
        let Some(items) = value.as_array() else {
            continue;
        };
        let mut out = BTreeMap::new();
        for item in items {
            if let Some(id) = task_json_string_field(item, &["id", "taskId"]) {
                out.insert(id, item.clone());
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    BTreeMap::new()
}

pub(crate) fn normalize_task_json_task_dir(
    task_dir: &Path,
    root_history_items: &BTreeMap<String, Value>,
    context: &ProviderAdapterContext,
    spec: TaskJsonProviderSpec,
    task_ordinal: usize,
) -> Result<ProviderNormalizationResult> {
    let mut result = ProviderNormalizationResult::default();
    let raw_source_path = task_dir.display().to_string();
    let source_path = Some(raw_source_path.as_str());
    let mut file_names = Vec::new();

    let metadata = read_task_json_optional(
        &mut result.summary,
        task_dir,
        spec.metadata_file,
        context,
        task_ordinal,
    );
    if metadata.is_some() {
        file_names.push(spec.metadata_file);
    }
    let history_item = spec.history_item_file.and_then(|file| {
        let value =
            read_task_json_optional(&mut result.summary, task_dir, file, context, task_ordinal);
        if value.is_some() {
            file_names.push(file);
        }
        value
    });
    let index_item = spec.index_file.and_then(|file| {
        let value =
            read_task_json_optional(&mut result.summary, task_dir, file, context, task_ordinal);
        if value.is_some() {
            file_names.push(file);
        }
        value
    });

    let task_id = task_json_task_id(
        task_dir,
        metadata.as_ref(),
        history_item.as_ref(),
        index_item.as_ref(),
    );
    let root_history_item = root_history_items.get(&task_id);
    let started_at = task_json_started_at(
        metadata.as_ref(),
        history_item.as_ref(),
        index_item.as_ref(),
        root_history_item,
        context.imported_at,
    );
    let ended_at = task_json_ended_at(
        metadata.as_ref(),
        history_item.as_ref(),
        index_item.as_ref(),
    );
    let cwd = task_json_cwd(
        metadata.as_ref(),
        history_item.as_ref(),
        index_item.as_ref(),
        root_history_item,
    );

    let mut event_inputs = Vec::new();
    if let Some(value) = read_task_json_optional(
        &mut result.summary,
        task_dir,
        spec.api_file,
        context,
        task_ordinal,
    ) {
        file_names.push(spec.api_file);
        task_json_push_message_events(&mut event_inputs, &value, "api_conversation_history");
    }
    if let Some(value) = read_task_json_optional(
        &mut result.summary,
        task_dir,
        spec.ui_file,
        context,
        task_ordinal,
    ) {
        file_names.push(spec.ui_file);
        task_json_push_message_events(&mut event_inputs, &value, "ui_messages");
    }
    if let Some(file) = spec.fallback_api_file {
        if let Some(value) =
            read_task_json_optional(&mut result.summary, task_dir, file, context, task_ordinal)
        {
            file_names.push(file);
            task_json_push_message_events(&mut event_inputs, &value, "claude_messages");
        }
    }
    if event_inputs.is_empty() {
        if let Some(value) = history_item
            .as_ref()
            .or(root_history_item)
            .and_then(task_json_history_item_event)
        {
            event_inputs.push(TaskJsonEventInput {
                source: "history_item",
                native_index: 0,
                raw: value,
            });
        }
    }

    if event_inputs.is_empty() {
        result.captures.push((
            task_ordinal,
            task_json_capture(
                spec,
                &task_id,
                source_path,
                context,
                started_at,
                ended_at,
                cwd.clone(),
                metadata.as_ref(),
                history_item.as_ref().or(root_history_item),
                index_item.as_ref(),
                &file_names,
                None,
            ),
        ));
        return Ok(result);
    }

    for (event_ordinal, input) in event_inputs.into_iter().enumerate() {
        let line_number = task_ordinal
            .saturating_mul(10_000)
            .saturating_add(event_ordinal)
            .saturating_add(1);
        let occurred_at = task_json_event_time(&input.raw)
            .unwrap_or_else(|| started_at + chrono::Duration::milliseconds(event_ordinal as i64));
        let raw_event = input.raw.clone();
        let event = task_json_event(spec, &task_id, input, event_ordinal, occurred_at);
        result
            .files_touched
            .extend(provider_file_touches_from_raw_value(
                spec.provider,
                &task_id,
                spec.source_format,
                source_path,
                &raw_event,
                &event,
                line_number,
            ));
        result.captures.push((
            line_number,
            task_json_capture(
                spec,
                &task_id,
                source_path,
                context,
                started_at,
                ended_at,
                cwd.clone(),
                metadata.as_ref(),
                history_item.as_ref().or(root_history_item),
                index_item.as_ref(),
                &file_names,
                Some(event),
            ),
        ));
    }

    Ok(result)
}

#[derive(Debug, Clone)]
pub(crate) struct TaskJsonEventInput {
    pub(crate) source: &'static str,
    pub(crate) native_index: usize,
    pub(crate) raw: Value,
}

pub(crate) fn read_task_json_optional(
    summary: &mut ProviderImportSummary,
    task_dir: &Path,
    file_name: &str,
    context: &ProviderAdapterContext,
    line: usize,
) -> Option<Value> {
    let path = task_dir.join(file_name);
    if !path.exists() {
        return None;
    }
    match read_task_json_value(&path, context) {
        Ok(value) => Some(value),
        Err(err) => {
            summary.failed += 1;
            summary.failures.push(ProviderImportFailure {
                line,
                error: format!("{file_name}: {err}"),
            });
            None
        }
    }
}

pub(crate) fn read_task_json_value(
    path: &Path,
    _context: &ProviderAdapterContext,
) -> Result<Value> {
    ensure_regular_provider_transcript_file(path)?;
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_PROVIDER_JSONL_LINE_BYTES as u64 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "provider task JSON file exceeds maximum supported size",
        });
    }
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(CaptureError::from)
}

pub(crate) fn task_json_push_message_events(
    out: &mut Vec<TaskJsonEventInput>,
    value: &Value,
    source: &'static str,
) {
    match value {
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                out.push(TaskJsonEventInput {
                    source,
                    native_index: index,
                    raw: item.clone(),
                });
            }
        }
        Value::Object(object) => {
            if let Some(items) = object
                .get("messages")
                .or_else(|| object.get("history"))
                .and_then(Value::as_array)
            {
                for (index, item) in items.iter().enumerate() {
                    out.push(TaskJsonEventInput {
                        source,
                        native_index: index,
                        raw: item.clone(),
                    });
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn task_json_task_id(
    task_dir: &Path,
    metadata: Option<&Value>,
    history_item: Option<&Value>,
    index_item: Option<&Value>,
) -> String {
    metadata
        .and_then(|value| task_json_string_field(value, &["taskId", "id"]))
        .or_else(|| history_item.and_then(|value| task_json_string_field(value, &["id", "taskId"])))
        .or_else(|| index_item.and_then(|value| task_json_string_field(value, &["id", "taskId"])))
        .or_else(|| {
            task_dir
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.trim().is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown-task".to_owned())
}

pub(crate) fn task_json_started_at(
    metadata: Option<&Value>,
    history_item: Option<&Value>,
    index_item: Option<&Value>,
    root_history_item: Option<&Value>,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    metadata
        .and_then(|value| {
            task_json_time_field(value, &["createdAt", "created_at", "ts", "timestamp"])
        })
        .or_else(|| {
            history_item.and_then(|value| {
                task_json_time_field(value, &["createdAt", "created_at", "ts", "timestamp"])
            })
        })
        .or_else(|| {
            index_item.and_then(|value| {
                task_json_time_field(value, &["createdAt", "created_at", "ts", "timestamp"])
            })
        })
        .or_else(|| {
            root_history_item.and_then(|value| {
                task_json_time_field(value, &["createdAt", "created_at", "ts", "timestamp"])
            })
        })
        .unwrap_or(fallback)
}

pub(crate) fn task_json_ended_at(
    metadata: Option<&Value>,
    history_item: Option<&Value>,
    index_item: Option<&Value>,
) -> Option<DateTime<Utc>> {
    metadata
        .and_then(|value| {
            task_json_time_field(
                value,
                &["lastModified", "updatedAt", "completedAt", "last_modified"],
            )
        })
        .or_else(|| {
            history_item.and_then(|value| {
                task_json_time_field(
                    value,
                    &["lastModified", "updatedAt", "completedAt", "last_modified"],
                )
            })
        })
        .or_else(|| {
            index_item.and_then(|value| {
                task_json_time_field(
                    value,
                    &["lastModified", "updatedAt", "completedAt", "last_modified"],
                )
            })
        })
}

pub(crate) fn task_json_cwd(
    metadata: Option<&Value>,
    history_item: Option<&Value>,
    index_item: Option<&Value>,
    root_history_item: Option<&Value>,
) -> Option<String> {
    metadata
        .and_then(|value| task_json_string_field(value, &["cwd", "workspace", "workspacePath"]))
        .or_else(|| {
            history_item.and_then(|value| {
                task_json_string_field(
                    value,
                    &[
                        "cwd",
                        "workspace",
                        "workspacePath",
                        "cwdOnTaskInitialization",
                    ],
                )
            })
        })
        .or_else(|| {
            index_item.and_then(|value| {
                task_json_string_field(
                    value,
                    &[
                        "cwd",
                        "workspace",
                        "workspacePath",
                        "cwdOnTaskInitialization",
                    ],
                )
            })
        })
        .or_else(|| {
            root_history_item.and_then(|value| {
                task_json_string_field(
                    value,
                    &[
                        "cwd",
                        "workspace",
                        "workspacePath",
                        "cwdOnTaskInitialization",
                    ],
                )
            })
        })
}

pub(crate) fn task_json_history_item_event(value: &Value) -> Option<Value> {
    let text = task_json_string_field(value, &["task", "title", "summary", "name"])?;
    let mut object = serde_json::Map::new();
    object.insert("role".to_owned(), Value::String("user".to_owned()));
    object.insert("content".to_owned(), Value::String(text));
    object.insert("type".to_owned(), Value::String("history_item".to_owned()));
    if let Some(ts) = value
        .get("ts")
        .or_else(|| value.get("timestamp"))
        .or_else(|| value.get("createdAt"))
    {
        object.insert("timestamp".to_owned(), ts.clone());
    }
    Some(Value::Object(object))
}

pub(crate) fn task_json_capture(
    spec: TaskJsonProviderSpec,
    task_id: &str,
    raw_source_path: Option<&str>,
    context: &ProviderAdapterContext,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    cwd: Option<String>,
    metadata: Option<&Value>,
    history_item: Option<&Value>,
    index_item: Option<&Value>,
    file_names: &[&str],
    event: Option<ProviderEventEnvelope>,
) -> ProviderCaptureEnvelope {
    let is_done = history_item
        .and_then(|value| value.get("isCompleted").or_else(|| value.get("completed")))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider: spec.provider,
        source: ProviderSourceEnvelope {
            source_format: spec.source_format.to_owned(),
            machine_id: context.machine_id.clone(),
            observed_at: context.imported_at,
            raw_source_path: raw_source_path.map(str::to_owned),
            raw_retention: ProviderRawRetention::PathReference,
            redaction_boundary: ProviderRedactionBoundary::BeforeExport,
            trust: ProviderSourceTrust::ProviderNative,
            fidelity: Fidelity::Imported,
            cursor: event.as_ref().map(|event| ProviderCursorRange {
                before: None,
                after: Some(ProviderCursorCheckpoint {
                    stream: provider_cursor_stream(spec.provider, spec.source_format),
                    cursor: event.cursor.clone().unwrap_or_else(|| task_id.to_owned()),
                    observed_at: event.occurred_at,
                }),
            }),
            idempotency_key: Some(format!(
                "provider-source:{}:{}:{task_id}",
                spec.provider.as_str(),
                spec.source_format
            )),
            metadata: json!({
                "adapter": spec.source_format,
                "native_task_id": task_id,
                "files": file_names,
            }),
        },
        session: ProviderSessionEnvelope {
            provider_session_id: task_id.to_owned(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: if is_done {
                SessionStatus::Completed
            } else {
                SessionStatus::Imported
            },
            started_at,
            ended_at,
            cwd,
            fidelity: Fidelity::Imported,
            idempotency_key: Some(format!(
                "provider-session:{}:{task_id}",
                spec.provider.as_str()
            )),
            artifacts: Vec::new(),
            metadata: json!({
                "source_format": spec.source_format,
                "provider": spec.provider.as_str(),
                "display_name": spec.display_name,
                "native_task_id": task_id,
                "task_metadata": metadata.map(|value| provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS)),
                "history_item": history_item.map(|value| provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS)),
                "index": index_item.map(|value| provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS)),
                "files": file_names,
                "limitations": [
                    "VS Code extension globalState databases are not parsed; ctx reads file-backed task directories",
                    "binary attachments and checkpoints are preserved only as native JSON metadata when present",
                    "message timestamps are inferred from task metadata when individual messages omit timestamps"
                ],
            }),
        },
        event,
    }
}

pub(crate) fn task_json_event(
    spec: TaskJsonProviderSpec,
    task_id: &str,
    input: TaskJsonEventInput,
    event_ordinal: usize,
    occurred_at: DateTime<Utc>,
) -> ProviderEventEnvelope {
    let event_type = task_json_event_type(&input.raw, input.source);
    let role = Some(task_json_event_role(&input.raw, input.source));
    let text = task_json_event_text(&input.raw, input.source, event_type);
    let (text, truncated) = provider_local_preview(&text, PROVIDER_MAX_TEXT_CHARS);
    let native_id = task_json_string_field(&input.raw, &["id", "uuid", "messageId"])
        .unwrap_or_else(|| format!("{}-{}", input.source, input.native_index));
    let event_id = format!("{task_id}:{}:{native_id}", input.source);

    ProviderEventEnvelope {
        provider_event_index: event_ordinal as u64,
        provider_event_hash: Some(event_id.clone()),
        cursor: Some(event_id.clone()),
        event_type,
        role,
        occurred_at,
        fidelity: Fidelity::Imported,
        redaction_state: RedactionState::LocalPreview,
        idempotency_key: Some(format!(
            "provider-event:{}:{}:{event_id}",
            spec.provider.as_str(),
            spec.source_format
        )),
        artifacts: Vec::new(),
        payload: json!({
            "entry_type": task_json_entry_type(&input.raw, input.source),
            "event_id": event_id,
            "native_index": input.native_index,
            "text": text,
            "truncated": truncated,
            "body": provider_capped_json(&input.raw, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": input.source,
            "source_format": spec.source_format,
            "native_index": input.native_index,
            "role": task_json_string_field(&input.raw, &["role"]),
            "model": task_json_model(&input.raw),
            "usage": task_json_usage(&input.raw),
        }),
    }
}

pub(crate) fn task_json_event_type(value: &Value, source: &str) -> EventType {
    if task_json_content_has(value, "tool_result") {
        return EventType::ToolOutput;
    }
    if task_json_content_has(value, "tool_use") {
        return EventType::ToolCall;
    }
    match source {
        "ui_messages" => match task_json_string_field(value, &["type", "say", "ask"]).as_deref() {
            Some("ask" | "say" | "user" | "assistant" | "text") => EventType::Message,
            Some("command" | "execute_command" | "shell") => EventType::CommandOutput,
            Some("completion_result" | "summary") => EventType::Summary,
            _ => EventType::Notice,
        },
        _ => match task_json_string_field(value, &["type", "role"]).as_deref() {
            Some("user" | "assistant" | "system") => EventType::Message,
            Some("tool_result") => EventType::ToolOutput,
            Some("tool_use") => EventType::ToolCall,
            Some("history_item" | "summary") => EventType::Summary,
            _ => EventType::Message,
        },
    }
}

pub(crate) fn task_json_event_role(value: &Value, source: &str) -> EventRole {
    if let Some(role) = task_json_string_field(value, &["role"]) {
        return provider_role(Some(&role));
    }
    if source == "ui_messages" {
        match task_json_string_field(value, &["type"]).as_deref() {
            Some("ask") => EventRole::User,
            Some("say") => EventRole::Assistant,
            _ => EventRole::Unknown,
        }
    } else {
        EventRole::Unknown
    }
}

pub(crate) fn task_json_event_text(value: &Value, source: &str, event_type: EventType) -> String {
    value
        .get("content")
        .or_else(|| value.pointer("/message/content"))
        .and_then(provider_value_text)
        .or_else(|| value.get("text").and_then(Value::as_str).map(str::to_owned))
        .or_else(|| value.get("message").and_then(provider_value_text))
        .or_else(|| {
            value
                .get("summary")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| {
            if event_type == EventType::Notice {
                format!("Task JSON event: {}", task_json_entry_type(value, source))
            } else {
                serde_json::to_string(value).unwrap_or_else(|_| source.to_owned())
            }
        })
}

pub(crate) fn task_json_entry_type(value: &Value, source: &str) -> String {
    task_json_string_field(value, &["type", "say", "ask", "role"])
        .unwrap_or_else(|| source.to_owned())
}

pub(crate) fn task_json_content_has(value: &Value, expected: &str) -> bool {
    value
        .get("content")
        .or_else(|| value.pointer("/message/content"))
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some(expected))
        })
        .unwrap_or(false)
}

pub(crate) fn task_json_event_time(value: &Value) -> Option<DateTime<Utc>> {
    task_json_time_field(
        value,
        &["timestamp", "ts", "createdAt", "created_at", "time", "date"],
    )
}

pub(crate) fn task_json_model(value: &Value) -> Option<Value> {
    value
        .get("model")
        .or_else(|| value.pointer("/modelInfo/id"))
        .or_else(|| value.pointer("/metadata/model"))
        .cloned()
}

pub(crate) fn task_json_usage(value: &Value) -> Option<Value> {
    value
        .get("usage")
        .or_else(|| value.get("tokensUsed"))
        .or_else(|| value.pointer("/modelInfo/usage"))
        .cloned()
}
