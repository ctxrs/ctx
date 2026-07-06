use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::BufReader,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, Confidence, EventRole, EventType, Fidelity, FileChangeKind,
    ProviderSourceTrust,
};
use serde_json::{json, Value};

use crate::JUNIE_SESSION_EVENTS_SOURCE_FORMAT;

use crate::common::io::{ensure_regular_provider_transcript_file, read_provider_jsonl_line};
use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::native::{
    native_event, native_provider_capture, provider_capped_json_value, provider_timestamp_millis,
    NativeEventDraft, NativeSessionDraft,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderFileTouchedEnvelope, ProviderImportFailure,
    ProviderNormalizationResult, Result, PROVIDER_MAX_PREVIEW_CHARS,
};

#[derive(Debug, Clone, Default)]
pub(crate) struct JunieIndexMeta {
    pub(crate) session_id: String,
    pub(crate) created_at: Option<i64>,
    pub(crate) updated_at: Option<i64>,
    pub(crate) task_name: Option<String>,
    pub(crate) project_dir: Option<String>,
    pub(crate) raw: Value,
}

#[derive(Debug, Clone)]
pub(crate) struct JunieSessionPath {
    pub(crate) events_path: PathBuf,
    pub(crate) index_meta: JunieIndexMeta,
}

#[derive(Debug, Clone)]
pub(crate) struct JunieStepAgg {
    pub(crate) order: usize,
    pub(crate) label: Option<String>,
    pub(crate) command: Option<String>,
    pub(crate) files: Option<Value>,
    pub(crate) changes: Vec<Value>,
    pub(crate) details: Option<String>,
    pub(crate) status: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct JunieUsage {
    pub(crate) input_tokens: i64,
    pub(crate) output_tokens: i64,
    pub(crate) cache_read_tokens: i64,
    pub(crate) cache_write_tokens: i64,
    pub(crate) model: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct JunieAssistantBuffer {
    pub(crate) open: bool,
    pub(crate) turn_ts: Option<DateTime<Utc>>,
    pub(crate) steps: BTreeMap<String, JunieStepAgg>,
    pub(crate) results: BTreeMap<String, String>,
    pub(crate) usage: JunieUsage,
}

pub(crate) fn normalize_junie_session_events(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let session_paths = junie_session_event_paths(path)?;
    if session_paths.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no Junie index.jsonl entries with session events.jsonl files were found",
        });
    }

    let mut merged = ProviderNormalizationResult::default();
    for (session_ordinal, session_path) in session_paths.iter().enumerate() {
        match normalize_junie_session_events_file(session_path, context, session_ordinal) {
            Ok(mut result) => {
                merged.summary.merge(result.summary);
                merged.captures.append(&mut result.captures);
                merged.files_touched.append(&mut result.files_touched);
            }
            Err(err) => {
                merged.summary.failed += 1;
                merged.summary.failures.push(ProviderImportFailure {
                    line: session_ordinal.saturating_add(1),
                    error: err.to_string(),
                });
            }
        }
    }
    if merged.captures.is_empty() && merged.summary.failed == 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Junie session events were empty or unsupported",
        });
    }
    Ok(merged)
}

pub(crate) fn junie_session_event_paths(path: &Path) -> Result<Vec<JunieSessionPath>> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_file() {
        ensure_regular_provider_transcript_file(path)?;
        if path.file_name().and_then(|name| name.to_str()) != Some("events.jsonl") {
            return Ok(Vec::new());
        }
        let session_id = junie_session_id_from_events_path(path)?;
        let index_meta =
            junie_index_meta_for_events_path(path, &session_id).unwrap_or_else(|| JunieIndexMeta {
                session_id,
                ..JunieIndexMeta::default()
            });
        return Ok(vec![JunieSessionPath {
            events_path: path.to_path_buf(),
            index_meta,
        }]);
    }
    if !metadata.file_type().is_dir() {
        return Ok(Vec::new());
    }

    let direct_events = path.join("events.jsonl");
    if direct_events.is_file() {
        let session_id = junie_session_id_from_events_path(&direct_events)?;
        let index_meta = junie_index_meta_for_events_path(&direct_events, &session_id)
            .unwrap_or_else(|| JunieIndexMeta {
                session_id,
                ..JunieIndexMeta::default()
            });
        return Ok(vec![JunieSessionPath {
            events_path: direct_events,
            index_meta,
        }]);
    }

    let index_path = path.join("index.jsonl");
    if !index_path.is_file() {
        return Ok(Vec::new());
    }
    let metas = junie_read_index(&index_path)?;
    let mut out = Vec::new();
    for meta in metas {
        if !junie_session_id_is_safe(&meta.session_id) {
            continue;
        }
        let events_path = path.join(&meta.session_id).join("events.jsonl");
        if events_path.is_file() {
            out.push(JunieSessionPath {
                events_path,
                index_meta: meta,
            });
        }
    }
    Ok(out)
}

pub(crate) fn junie_index_meta_for_events_path(
    path: &Path,
    session_id: &str,
) -> Option<JunieIndexMeta> {
    let index_path = path.parent()?.parent()?.join("index.jsonl");
    junie_read_index(&index_path)
        .ok()?
        .into_iter()
        .find(|meta| meta.session_id == session_id)
}

pub(crate) fn junie_read_index(path: &Path) -> Result<Vec<JunieIndexMeta>> {
    ensure_regular_provider_transcript_file(path)?;
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut metas = Vec::new();
    let mut line = Vec::new();
    while read_provider_jsonl_line(&mut reader, &mut line)? {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let Some(session_id) = value
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|session_id| junie_session_id_is_safe(session_id))
            .map(str::to_owned)
        else {
            continue;
        };
        metas.push(JunieIndexMeta {
            session_id,
            created_at: junie_timestamp_millis_field(&value, "createdAt"),
            updated_at: junie_timestamp_millis_field(&value, "updatedAt"),
            task_name: value
                .get("taskName")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned),
            project_dir: value
                .get("projectDir")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned),
            raw: value,
        });
    }
    Ok(metas)
}

pub(crate) fn junie_timestamp_millis_field(value: &Value, field: &str) -> Option<i64> {
    let value = value.get(field)?;
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_f64().map(|value| value.round() as i64))
}

pub(crate) fn junie_session_id_from_events_path(path: &Path) -> Result<String> {
    let Some(session_id) = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
    else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Junie events.jsonl path is not inside a session directory",
        });
    };
    if !junie_session_id_is_safe(session_id) {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Junie session id is not a safe path segment",
        });
    }
    Ok(session_id.to_owned())
}

pub(crate) fn junie_session_id_is_safe(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id != "."
        && session_id != ".."
        && !session_id.contains('/')
        && !session_id.contains('\\')
}

pub(crate) fn normalize_junie_session_events_file(
    session_path: &JunieSessionPath,
    context: &ProviderAdapterContext,
    session_ordinal: usize,
) -> Result<ProviderNormalizationResult> {
    ensure_regular_provider_transcript_file(&session_path.events_path)?;
    let provider_session_id = if session_path.index_meta.session_id.is_empty() {
        junie_session_id_from_events_path(&session_path.events_path)?
    } else {
        session_path.index_meta.session_id.clone()
    };
    if !junie_session_id_is_safe(&provider_session_id) {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: session_path.events_path.clone(),
            reason: "Junie session id is not a safe path segment",
        });
    }

    let started_at =
        provider_timestamp_millis(session_path.index_meta.created_at, context.imported_at);
    let mut ended_at = session_path
        .index_meta
        .updated_at
        .map(|timestamp| provider_timestamp_millis(Some(timestamp), started_at));
    let raw_source_path = session_path.events_path.display().to_string();
    let mut cwd = session_path.index_meta.project_dir.clone();
    let mut title = session_path.index_meta.task_name.clone();
    let base_line = session_ordinal.saturating_mul(100_000);
    let mut result = ProviderNormalizationResult::default();
    let mut buffer = JunieAssistantBuffer::default();
    let mut provider_event_index = 0u64;
    let mut last_ts = started_at;
    let mut saw_supported_event = false;
    let base_draft = NativeSessionDraft {
        provider: CaptureProvider::Junie,
        source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        provider_session_id: provider_session_id.clone(),
        parent_provider_session_id: None,
        root_provider_session_id: None,
        external_agent_id: None,
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
            "adapter": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            "source_path": raw_source_path.clone(),
            "storage": "~/.junie/sessions/index.jsonl + session-*/events.jsonl",
            "upstream_schema_anchor": {
                "source": "vladar107/claudescope",
                "connector": "packages/server/src/connectors/junie",
                "notes": "event-sourced UI render stream with UserPromptEvent and SessionA2uxEvent agentEvent blocks"
            },
        }),
        session_metadata: json!({
            "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            "session_id": provider_session_id.clone(),
            "title": title.clone(),
            "project_dir": cwd.clone(),
            "index": provider_capped_json_value(&session_path.index_meta.raw, PROVIDER_MAX_PREVIEW_CHARS),
            "limitations": [
                "ctx imports Junie events.jsonl UI stream blocks, not a provider conversational message log",
                "custom attachment image files are not read by the native importer",
                "unknown SessionA2uxEvent agentEvent kinds are skipped"
            ],
        }),
    };

    let file = File::open(&session_path.events_path)?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut line_number = 0usize;
    while read_provider_jsonl_line(&mut reader, &mut line)? {
        line_number += 1;
        let import_line = base_line.saturating_add(line_number);
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let value: Value = match serde_json::from_slice(&line) {
            Ok(value) => value,
            Err(err) => {
                push_provider_import_failure(
                    &mut result.summary,
                    import_line,
                    format!("malformed Junie events JSONL: {err}"),
                );
                continue;
            }
        };
        let kind = value.get("kind").and_then(Value::as_str).unwrap_or("");
        if kind == "UserPromptEvent" {
            junie_flush_assistant(
                &mut buffer,
                &base_draft,
                context,
                &mut result,
                import_line,
                &mut provider_event_index,
            );
            let prompt = value.get("prompt").and_then(Value::as_str).unwrap_or("");
            if !prompt.trim().is_empty() {
                let event = native_event(NativeEventDraft {
                    provider: CaptureProvider::Junie,
                    source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                    provider_session_id: provider_session_id.clone(),
                    provider_event_index,
                    provider_event_hash: Some(format!("line:{line_number}:user")),
                    cursor: format!(
                        "{}:line:{line_number}:event:{provider_event_index}",
                        session_path.events_path.display()
                    ),
                    event_type: EventType::Message,
                    role: Some(EventRole::User),
                    occurred_at: last_ts,
                    text: prompt.to_owned(),
                    body: json!({
                        "kind": kind,
                        "prompt": prompt,
                    }),
                    metadata: json!({
                        "source": "junie_user_prompt",
                        "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                    }),
                });
                provider_event_index = provider_event_index.saturating_add(1);
                result.captures.push((
                    import_line,
                    native_provider_capture(base_draft.clone(), context, Some(event)),
                ));
                saw_supported_event = true;
            }
            continue;
        }
        if kind != "SessionA2uxEvent" {
            continue;
        }
        if let Some(timestamp) = junie_timestamp_millis_field(&value, "timestampMs")
            .and_then(DateTime::<Utc>::from_timestamp_millis)
        {
            last_ts = timestamp;
            ended_at = Some(timestamp);
        }
        let agent_event = value
            .get("event")
            .and_then(|event| event.get("agentEvent"))
            .unwrap_or(&Value::Null);
        let agent_kind = agent_event
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("");
        match agent_kind {
            "LlmResponseMetadataEvent" => {
                junie_ensure_assistant(&mut buffer, last_ts);
                junie_merge_usage(&mut buffer.usage, agent_event);
                saw_supported_event = true;
            }
            "AgentTaskNameUpdatedEvent" => {
                if let Some(name) = agent_event.get("name").and_then(Value::as_str) {
                    if !name.trim().is_empty() {
                        title = Some(name.to_owned());
                    }
                }
            }
            "CurrentDirectoryUpdatedEvent" => {
                if cwd.is_none() {
                    cwd = agent_event
                        .get("currentDirectory")
                        .and_then(Value::as_str)
                        .filter(|value| !value.trim().is_empty())
                        .map(str::to_owned);
                }
            }
            "ResultBlockUpdatedEvent" => {
                junie_ensure_assistant(&mut buffer, last_ts);
                if let Some(text) = agent_event.get("result").and_then(Value::as_str) {
                    if !text.trim().is_empty() {
                        let step_id = agent_event
                            .get("stepId")
                            .and_then(Value::as_str)
                            .filter(|value| !value.is_empty())
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("result-{line_number}"));
                        buffer.results.insert(step_id, text.to_owned());
                        saw_supported_event = true;
                    }
                }
            }
            "ToolBlockUpdatedEvent"
            | "TerminalBlockUpdatedEvent"
            | "ViewFilesBlockUpdatedEvent"
            | "FileChangesBlockUpdatedEvent" => {
                junie_merge_step(&mut buffer, agent_event, last_ts);
                saw_supported_event = true;
            }
            _ => {}
        }
    }
    junie_flush_assistant(
        &mut buffer,
        &base_draft,
        context,
        &mut result,
        base_line.saturating_add(line_number.saturating_add(1)),
        &mut provider_event_index,
    );

    if result.captures.is_empty() && !saw_supported_event {
        push_provider_import_failure(
            &mut result.summary,
            base_line,
            "Junie events.jsonl contained no supported UserPromptEvent or SessionA2uxEvent blocks"
                .to_owned(),
        );
    }

    if let Some(ended_at) = ended_at {
        for (_, capture) in &mut result.captures {
            capture.session.ended_at = Some(ended_at);
            capture.session.cwd = cwd.clone();
            capture.session.metadata["title"] = json!(title.clone());
            capture.session.metadata["project_dir"] = json!(cwd.clone());
        }
    }
    Ok(result)
}

pub(crate) fn junie_ensure_assistant(
    buffer: &mut JunieAssistantBuffer,
    occurred_at: DateTime<Utc>,
) {
    if !buffer.open {
        buffer.open = true;
        buffer.turn_ts = Some(occurred_at);
    }
}

pub(crate) fn junie_merge_usage(usage: &mut JunieUsage, agent_event: &Value) {
    let Some(items) = agent_event.get("modelUsage").and_then(Value::as_array) else {
        return;
    };
    for item in items {
        usage.input_tokens = usage
            .input_tokens
            .saturating_add(junie_i64_field(item, "inputTokens"));
        usage.output_tokens = usage
            .output_tokens
            .saturating_add(junie_i64_field(item, "outputTokens"));
        usage.cache_read_tokens = usage
            .cache_read_tokens
            .saturating_add(junie_i64_field(item, "cacheInputTokens"));
        usage.cache_write_tokens = usage
            .cache_write_tokens
            .saturating_add(junie_i64_field(item, "cacheCreateTokens"));
        if let Some(model) = item.get("model").and_then(Value::as_str) {
            if !model.trim().is_empty() {
                usage.model = Some(model.to_owned());
            }
        }
    }
}

pub(crate) fn junie_i64_field(value: &Value, field: &str) -> i64 {
    value
        .get(field)
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        })
        .unwrap_or(0)
}

pub(crate) fn junie_merge_step(
    buffer: &mut JunieAssistantBuffer,
    agent_event: &Value,
    occurred_at: DateTime<Utc>,
) {
    let Some(step_id) = agent_event
        .get("stepId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    junie_ensure_assistant(buffer, occurred_at);
    let next_order = buffer.steps.len();
    let step = buffer
        .steps
        .entry(step_id.to_owned())
        .or_insert_with(|| JunieStepAgg {
            order: next_order,
            label: None,
            command: None,
            files: None,
            changes: Vec::new(),
            details: None,
            status: None,
        });
    if let Some(text) = agent_event.get("text").and_then(Value::as_str) {
        if !text.trim().is_empty() {
            step.label = Some(text.to_owned());
        }
    }
    if let Some(command) = agent_event.get("command").and_then(Value::as_str) {
        if !command.trim().is_empty() {
            step.command = Some(command.to_owned());
        }
    }
    if let Some(files) = agent_event.get("files").filter(|value| value.is_array()) {
        step.files = Some(files.clone());
    }
    if let Some(changes) = agent_event.get("changes").and_then(Value::as_array) {
        step.changes = changes.clone();
    }
    if let Some(details) = agent_event.get("details").and_then(Value::as_str) {
        if !details.trim().is_empty() {
            step.details = Some(details.to_owned());
        }
    }
    if let Some(status) = agent_event.get("status").and_then(Value::as_str) {
        if !status.trim().is_empty() {
            step.status = Some(status.to_owned());
        }
    }
}

pub(crate) fn junie_flush_assistant(
    buffer: &mut JunieAssistantBuffer,
    base_draft: &NativeSessionDraft,
    context: &ProviderAdapterContext,
    result: &mut ProviderNormalizationResult,
    line_number: usize,
    provider_event_index: &mut u64,
) {
    if !buffer.open {
        return;
    }
    let occurred_at = buffer.turn_ts.unwrap_or(base_draft.started_at);
    let mut steps = buffer.steps.values().cloned().collect::<Vec<_>>();
    steps.sort_by_key(|step| step.order);
    for step in &steps {
        if !step.changes.is_empty() {
            junie_emit_file_changes(
                base_draft,
                context,
                result,
                line_number,
                provider_event_index,
                occurred_at,
                step,
            );
        } else {
            junie_emit_step_events(
                base_draft,
                context,
                result,
                line_number,
                provider_event_index,
                occurred_at,
                step,
            );
        }
    }
    let final_text = buffer
        .results
        .values()
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n");
    if !final_text.trim().is_empty() {
        let index = *provider_event_index;
        let event = native_event(NativeEventDraft {
            provider: CaptureProvider::Junie,
            source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            provider_session_id: base_draft.provider_session_id.clone(),
            provider_event_index: index,
            provider_event_hash: Some(format!("assistant-result:{index}")),
            cursor: format!(
                "{}:line:{line_number}:event:{index}",
                base_draft.raw_source_path
            ),
            event_type: EventType::Message,
            role: Some(EventRole::Assistant),
            occurred_at,
            text: final_text,
            body: json!({
                "result_blocks": buffer.results.clone(),
                "model": buffer.usage.model.clone(),
                "usage": {
                    "input_tokens": buffer.usage.input_tokens,
                    "output_tokens": buffer.usage.output_tokens,
                    "cache_read_tokens": buffer.usage.cache_read_tokens,
                    "cache_write_tokens": buffer.usage.cache_write_tokens,
                },
            }),
            metadata: json!({
                "source": "junie_result_blocks",
                "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                "model": buffer.usage.model.clone(),
                "usage": {
                    "input_tokens": buffer.usage.input_tokens,
                    "output_tokens": buffer.usage.output_tokens,
                    "cache_read_tokens": buffer.usage.cache_read_tokens,
                    "cache_write_tokens": buffer.usage.cache_write_tokens,
                },
            }),
        });
        *provider_event_index = (*provider_event_index).saturating_add(1);
        result.captures.push((
            line_number,
            native_provider_capture(base_draft.clone(), context, Some(event)),
        ));
    }
    *buffer = JunieAssistantBuffer::default();
}

pub(crate) fn junie_emit_step_events(
    base_draft: &NativeSessionDraft,
    context: &ProviderAdapterContext,
    result: &mut ProviderNormalizationResult,
    line_number: usize,
    provider_event_index: &mut u64,
    occurred_at: DateTime<Utc>,
    step: &JunieStepAgg,
) {
    let (tool_name, text, body) = if let Some(command) = &step.command {
        (
            "Bash",
            format!("Bash: {command}"),
            json!({
                "tool_name": "Bash",
                "command": command,
                "label": step.label,
                "status": step.status,
            }),
        )
    } else if let Some(files) = &step.files {
        (
            "view",
            step.label
                .clone()
                .unwrap_or_else(|| "View files".to_owned()),
            json!({
                "tool_name": "view",
                "label": step.label,
                "files": files,
                "status": step.status,
            }),
        )
    } else {
        (
            "tool",
            step.label
                .clone()
                .unwrap_or_else(|| "Junie tool step".to_owned()),
            json!({
                "tool_name": "tool",
                "label": step.label,
                "status": step.status,
            }),
        )
    };
    let tool_index = *provider_event_index;
    let tool_event = native_event(NativeEventDraft {
        provider: CaptureProvider::Junie,
        source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        provider_session_id: base_draft.provider_session_id.clone(),
        provider_event_index: tool_index,
        provider_event_hash: Some(format!("step:{}:tool", step.order)),
        cursor: format!(
            "{}:line:{line_number}:event:{tool_index}",
            base_draft.raw_source_path
        ),
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at,
        text,
        body: body.clone(),
        metadata: json!({
            "source": "junie_step",
            "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            "tool_name": tool_name,
        }),
    });
    *provider_event_index = (*provider_event_index).saturating_add(1);
    result.captures.push((
        line_number,
        native_provider_capture(base_draft.clone(), context, Some(tool_event)),
    ));

    if let Some(details) = &step.details {
        if !details.trim().is_empty() {
            let output_index = *provider_event_index;
            let output_event = native_event(NativeEventDraft {
                provider: CaptureProvider::Junie,
                source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                provider_session_id: base_draft.provider_session_id.clone(),
                provider_event_index: output_index,
                provider_event_hash: Some(format!("step:{}:output", step.order)),
                cursor: format!(
                    "{}:line:{line_number}:event:{output_index}",
                    base_draft.raw_source_path
                ),
                event_type: if step.command.is_some() {
                    EventType::CommandOutput
                } else {
                    EventType::ToolOutput
                },
                role: Some(EventRole::Tool),
                occurred_at,
                text: details.clone(),
                body: json!({
                    "tool_name": tool_name,
                    "details": details,
                    "status": step.status,
                }),
                metadata: json!({
                    "source": "junie_step_details",
                    "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                    "tool_name": tool_name,
                }),
            });
            *provider_event_index = (*provider_event_index).saturating_add(1);
            result.captures.push((
                line_number,
                native_provider_capture(base_draft.clone(), context, Some(output_event)),
            ));
        }
    }
}

pub(crate) fn junie_emit_file_changes(
    base_draft: &NativeSessionDraft,
    context: &ProviderAdapterContext,
    result: &mut ProviderNormalizationResult,
    line_number: usize,
    provider_event_index: &mut u64,
    occurred_at: DateTime<Utc>,
    step: &JunieStepAgg,
) {
    for (change_index, change) in step.changes.iter().enumerate() {
        let before_path = change.get("beforeRelativePath").and_then(Value::as_str);
        let after_path = change.get("afterRelativePath").and_then(Value::as_str);
        let Some(path) = after_path.or(before_path) else {
            continue;
        };
        if path.trim().is_empty() {
            continue;
        }
        let change_kind = match (before_path, after_path) {
            (None, Some(_)) => FileChangeKind::Created,
            (Some(_), None) => FileChangeKind::Deleted,
            (Some(before), Some(after)) if before != after => FileChangeKind::Renamed,
            _ => FileChangeKind::Modified,
        };
        let event_index = *provider_event_index;
        let event = native_event(NativeEventDraft {
            provider: CaptureProvider::Junie,
            source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            provider_session_id: base_draft.provider_session_id.clone(),
            provider_event_index: event_index,
            provider_event_hash: Some(format!("step:{}:change:{change_index}", step.order)),
            cursor: format!(
                "{}:line:{line_number}:event:{event_index}",
                base_draft.raw_source_path
            ),
            event_type: EventType::ToolCall,
            role: Some(EventRole::Assistant),
            occurred_at,
            text: format!("Edit: {path}"),
            body: json!({
                "tool_name": "Edit",
                "file_path": path,
                "old_string": junie_file_content_text(change.get("beforeContent")),
                "new_string": junie_file_content_text(change.get("afterContent")),
                "before_relative_path": before_path,
                "after_relative_path": after_path,
                "change_kind": change_kind.as_str(),
                "status": step.status,
            }),
            metadata: json!({
                "source": "junie_file_change",
                "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                "tool_name": "Edit",
                "change_kind": change_kind.as_str(),
            }),
        });
        *provider_event_index = (*provider_event_index).saturating_add(1);
        result.captures.push((
            line_number,
            native_provider_capture(base_draft.clone(), context, Some(event)),
        ));
        result.files_touched.push((
            line_number,
            ProviderFileTouchedEnvelope {
                provider: CaptureProvider::Junie,
                provider_session_id: base_draft.provider_session_id.clone(),
                provider_touch_index: event_index
                    .saturating_mul(1_000)
                    .saturating_add(change_index as u64),
                provider_event_index: Some(event_index),
                raw_source_path: Some(base_draft.raw_source_path.clone()),
                source_root: context
                    .source_root_display()
                    .or_else(|| Some(base_draft.raw_source_path.clone())),
                path: path.to_owned(),
                change_kind: Some(change_kind),
                old_path: before_path
                    .filter(|before| after_path.is_some_and(|after| after != *before))
                    .map(str::to_owned),
                line_count_delta: None,
                confidence: Confidence::Explicit,
                occurred_at,
                source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT.to_owned(),
                metadata: json!({
                    "source": "junie_file_change",
                    "step_order": step.order,
                    "change_index": change_index,
                }),
            },
        ));
    }
}

pub(crate) fn junie_file_content_text(value: Option<&Value>) -> Option<String> {
    let value = value?;
    value
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| value.as_str())
        .map(str::to_owned)
}
