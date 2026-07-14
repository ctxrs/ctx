use std::path::Path;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderCursorCheckpoint, ProviderCursorRange, ProviderEventEnvelope, ProviderSessionEnvelope,
    ProviderSourceEnvelope, ProviderSourceTrust, SessionStatus,
    PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use serde_json::{json, Value};

use crate::common::time::parse_rfc3339_utc;
use crate::provider::file_touches::provider_file_touches_from_raw_value;
use crate::provider::importer::{
    provider_cursor_stream, provider_event_is_real_conversation_message,
    ProviderNormalizationBatcher,
};
use crate::provider::native::{
    provider_capped_json, provider_policy_body, provider_policy_event_text, provider_role,
    provider_value_text,
};
use crate::{
    ProviderAdapterContext, ProviderImportFailure, ProviderJsonlReader, ProviderJsonlRecordRead,
    ProviderNormalizationResult, Result, CLAUDE_PROJECTS_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

pub(crate) struct ClaudeJsonlScan {
    pub(crate) header: Option<Value>,
    pub(crate) earliest_started_at: Option<DateTime<Utc>>,
    pub(crate) has_real_message: bool,
    pub(crate) failed: usize,
    pub(crate) valid_records: usize,
}

pub(crate) fn scan_claude_projects_jsonl_reader(
    reader: &mut ProviderJsonlReader,
    context: &ProviderAdapterContext,
) -> Result<ClaudeJsonlScan> {
    let mut header = None;
    let mut earliest_started_at: Option<DateTime<Utc>> = None;
    let mut has_real_message = false;
    let mut failed = 0usize;
    let mut valid_records = 0usize;
    let mut line = Vec::new();
    loop {
        let line_number = match reader.read_record(&mut line)? {
            ProviderJsonlRecordRead::Eof | ProviderJsonlRecordRead::DeferredPartial { .. } => break,
            ProviderJsonlRecordRead::Oversized { .. } => continue,
            ProviderJsonlRecordRead::Record { line_number, .. } => {
                usize::try_from(line_number).unwrap_or(usize::MAX)
            }
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let value = match serde_json::from_slice::<Value>(&line) {
            Ok(value) => value,
            Err(_) => {
                failed += 1;
                continue;
            }
        };
        valid_records += 1;
        let parsed_timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_utc);
        let occurred_at = parsed_timestamp.unwrap_or(context.imported_at);
        earliest_started_at =
            Some(earliest_started_at.map_or(occurred_at, |earliest| earliest.min(occurred_at)));
        has_real_message |= claude_event(&value, line_number, occurred_at)
            .as_ref()
            .is_some_and(provider_event_is_real_conversation_message);
        header.get_or_insert(value);
    }
    reader.restart_import_position()?;
    Ok(ClaudeJsonlScan {
        header,
        earliest_started_at,
        has_real_message,
        failed,
        valid_records,
    })
}

pub(crate) fn normalize_claude_projects_jsonl_file(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let mut reader = ProviderJsonlReader::open_replacement(path)?;
    normalize_claude_projects_jsonl_reader(path, &mut reader, context, None, None)
}

pub(crate) fn normalize_claude_projects_jsonl_reader(
    path: &Path,
    reader: &mut ProviderJsonlReader,
    context: &ProviderAdapterContext,
    bootstrap_header: Option<Value>,
    authoritative_started_at: Option<DateTime<Utc>>,
) -> Result<ProviderNormalizationResult> {
    let scan = scan_claude_projects_jsonl_reader(reader, context)?;
    let no_header = bootstrap_header.is_none() && scan.header.is_none();
    let fallback_header = Value::Null;
    let header = bootstrap_header
        .as_ref()
        .or(scan.header.as_ref())
        .unwrap_or(&fallback_header);
    let started_at = authoritative_started_at
        .or(scan.earliest_started_at)
        .unwrap_or(context.imported_at);
    let mut result = ProviderNormalizationResult::default();
    stream_claude_projects_jsonl_reader(path, reader, context, header, started_at, |mut batch| {
        result.summary.merge(batch.summary);
        result.captures.append(&mut batch.captures);
        result.files_touched.append(&mut batch.files_touched);
        Ok(())
    })?;
    if no_header && result.summary.failed == 0 {
        result.summary.skipped += 1;
        result.summary.skipped_sessions += 1;
    }
    Ok(result)
}

pub(crate) fn stream_claude_projects_jsonl_reader<F>(
    path: &Path,
    reader: &mut ProviderJsonlReader,
    context: &ProviderAdapterContext,
    header: &Value,
    started_at: DateTime<Utc>,
    emit: F,
) -> Result<()>
where
    F: FnMut(ProviderNormalizationResult) -> Result<()>,
{
    let metadata = ClaudeNormalizationMetadata::new(path, context, header, started_at);
    let mut batches = ProviderNormalizationBatcher::new(emit);
    let mut line = Vec::new();
    let mut line_number = 0usize;
    while reader.read_record_or_skip_oversized(
        &mut line,
        &mut line_number,
        &mut batches.current_mut().summary,
    )? {
        if line.iter().all(u8::is_ascii_whitespace) {
            batches.record_processed()?;
            continue;
        }
        let value: Value = match serde_json::from_slice(&line) {
            Ok(value) => value,
            Err(err) => {
                let result = batches.current_mut();
                result.summary.failed += 1;
                result.summary.sample_failure(ProviderImportFailure {
                    line: line_number,
                    error: format!("malformed JSONL in {}: {err}", path.display()),
                });
                batches.record_processed()?;
                continue;
            }
        };
        let occurred_at = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_utc)
            .unwrap_or(context.imported_at);
        metadata.push_row(batches.current_mut(), line_number, value, occurred_at);
        batches.record_processed()?;
    }
    batches.finish()
}

struct ClaudeNormalizationMetadata {
    native_session_id: String,
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
    external_agent_id: Option<String>,
    is_subagent: bool,
    started_at: DateTime<Utc>,
    cwd: Option<String>,
    version: Option<String>,
    git_branch: Option<String>,
    machine_id: String,
    imported_at: DateTime<Utc>,
    raw_source_path: String,
    source_root: Option<String>,
}

impl ClaudeNormalizationMetadata {
    fn new(
        path: &Path,
        context: &ProviderAdapterContext,
        header: &Value,
        started_at: DateTime<Utc>,
    ) -> Self {
        let file_stem = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown-session");
        let native_session_id = header
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .unwrap_or(file_stem)
            .to_owned();
        let (provider_session_id, parent_provider_session_id, external_agent_id, is_subagent) =
            claude_path_session_ids(path, &native_session_id);
        let raw_source_path = path.display().to_string();
        Self {
            native_session_id,
            provider_session_id,
            parent_provider_session_id,
            external_agent_id,
            is_subagent,
            started_at,
            cwd: header
                .get("cwd")
                .and_then(Value::as_str)
                .filter(|cwd| !cwd.trim().is_empty())
                .map(str::to_owned),
            version: header
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_owned),
            git_branch: header
                .get("gitBranch")
                .and_then(Value::as_str)
                .map(str::to_owned),
            machine_id: context.machine_id.clone(),
            imported_at: context.imported_at,
            source_root: context
                .source_root_display()
                .or_else(|| Some(raw_source_path.clone())),
            raw_source_path,
        }
    }

    fn push_row(
        &self,
        result: &mut ProviderNormalizationResult,
        line_number: usize,
        value: Value,
        occurred_at: DateTime<Utc>,
    ) {
        let event = claude_event(&value, line_number, occurred_at);
        if let Some(event) = &event {
            result
                .files_touched
                .extend(provider_file_touches_from_raw_value(
                    CaptureProvider::Claude,
                    &self.provider_session_id,
                    CLAUDE_PROJECTS_SOURCE_FORMAT,
                    Some(self.raw_source_path.as_str()),
                    &value,
                    event,
                    line_number,
                ));
        }
        result.captures.push((
            line_number,
            ProviderCaptureEnvelope {
                schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
                provider: CaptureProvider::Claude,
                source: ProviderSourceEnvelope {
                    source_format: CLAUDE_PROJECTS_SOURCE_FORMAT.to_owned(),
                    machine_id: self.machine_id.clone(),
                    observed_at: self.imported_at,
                    raw_source_path: Some(self.raw_source_path.clone()),
                    source_root: self.source_root.clone(),
                    trust: ProviderSourceTrust::ProviderNative,
                    fidelity: Fidelity::Imported,
                    cursor: Some(ProviderCursorRange {
                        before: None,
                        after: Some(ProviderCursorCheckpoint {
                            stream: provider_cursor_stream(
                                CaptureProvider::Claude,
                                CLAUDE_PROJECTS_SOURCE_FORMAT,
                            ),
                            cursor: format!("{}:line:{line_number}", self.raw_source_path),
                            observed_at: occurred_at,
                        }),
                    }),
                    idempotency_key: Some(format!(
                        "provider-source:claude:{CLAUDE_PROJECTS_SOURCE_FORMAT}:{}",
                        self.provider_session_id
                    )),
                    metadata: json!({
                        "adapter": CLAUDE_PROJECTS_SOURCE_FORMAT,
                        "native_session_id": self.native_session_id,
                        "source_path": self.raw_source_path,
                    }),
                },
                session: ProviderSessionEnvelope {
                    provider_session_id: self.provider_session_id.clone(),
                    parent_provider_session_id: self.parent_provider_session_id.clone(),
                    root_provider_session_id: self.parent_provider_session_id.clone(),
                    external_agent_id: self.external_agent_id.clone(),
                    agent_type: if self.is_subagent {
                        AgentType::Subagent
                    } else {
                        AgentType::Primary
                    },
                    role_hint: Some(
                        if self.is_subagent { "subagent" } else { "primary" }.to_owned(),
                    ),
                    is_primary: !self.is_subagent,
                    status: SessionStatus::Imported,
                    started_at: self.started_at,
                    ended_at: None,
                    cwd: self.cwd.clone(),
                    fidelity: Fidelity::Imported,
                    idempotency_key: Some(format!(
                        "provider-session:claude:{}",
                        self.provider_session_id
                    )),
                    artifacts: Vec::new(),
                    metadata: json!({
                        "source_format": CLAUDE_PROJECTS_SOURCE_FORMAT,
                        "native_session_id": self.native_session_id,
                        "version": self.version,
                        "git_branch": self.git_branch,
                        "source_path": self.raw_source_path,
                        "limitations": [
                            "binary attachments are referenced by native payload metadata but not expanded",
                            "previews are capped before local indexing/export"
                        ],
                    }),
                },
                event,
            },
        ));
    }
}

pub(crate) fn claude_path_session_ids(
    path: &Path,
    native_session_id: &str,
) -> (String, Option<String>, Option<String>, bool) {
    let Some(parent) = path.parent() else {
        return (native_session_id.to_owned(), None, None, false);
    };
    if parent.file_name().and_then(|name| name.to_str()) == Some("subagents") {
        let parent_session_id = parent
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(native_session_id)
            .to_owned();
        let agent_id = path
            .file_stem()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("subagent")
            .to_owned();
        return (
            format!("{parent_session_id}/subagents/{agent_id}"),
            Some(parent_session_id),
            Some(agent_id),
            true,
        );
    }
    (native_session_id.to_owned(), None, None, false)
}

pub(crate) fn claude_event(
    value: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
) -> Option<ProviderEventEnvelope> {
    let entry_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let message = value.get("message").unwrap_or(value);
    let message_role = message
        .get("role")
        .and_then(Value::as_str)
        .or_else(|| value.get("role").and_then(Value::as_str));
    let null = Value::Null;
    let content = message.get("content").unwrap_or(&null);
    let event_type = claude_event_type(entry_type, message);
    let role = Some(provider_role(message_role));
    let text = provider_value_text(content).unwrap_or_else(|| {
        if event_type == EventType::Notice {
            format!("Claude event: {entry_type}")
        } else {
            String::new()
        }
    });
    let retained_text = provider_policy_event_text(event_type, &text, content);

    Some(ProviderEventEnvelope {
        provider_event_index: (line_number - 1) as u64,
        provider_event_hash: value.get("uuid").and_then(Value::as_str).map(str::to_owned),
        cursor: value.get("uuid").and_then(Value::as_str).map(str::to_owned),
        event_type,
        role,
        occurred_at,
        fidelity: Fidelity::Imported,
        idempotency_key: value
            .get("uuid")
            .and_then(Value::as_str)
            .map(|uuid| format!("provider-event:claude:{uuid}")),
        artifacts: Vec::new(),
        payload: json!({
            "entry_type": entry_type,
            "uuid": value.get("uuid").and_then(Value::as_str),
            "parent_uuid": value.get("parentUuid").and_then(Value::as_str),
            "message_id": message.get("id").and_then(Value::as_str),
            "request_id": value.get("requestId").and_then(Value::as_str),
            "role": message_role,
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "content_preview": provider_capped_json(&provider_policy_body(event_type, content), PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": "claude_projects_jsonl",
            "source_format": CLAUDE_PROJECTS_SOURCE_FORMAT,
            "line": line_number,
            "entry_type": entry_type,
            "model": message.get("model").and_then(Value::as_str),
            "usage": message.get("usage").cloned(),
            "stop_reason": message.get("stop_reason").and_then(Value::as_str),
            "is_sidechain": value.get("isSidechain").and_then(Value::as_bool),
            "tool_use_result": value.get("toolUseResult").map(|value| provider_policy_body(EventType::ToolOutput, value)),
        }),
    })
}

pub(crate) fn claude_event_type(entry_type: &str, message: &Value) -> EventType {
    if claude_content_has_type(message.get("content"), "tool_result")
        || message.get("toolUseResult").is_some()
    {
        return EventType::ToolOutput;
    }
    if claude_content_has_type(message.get("content"), "tool_use") {
        return EventType::ToolCall;
    }
    match entry_type {
        "user" | "assistant" => EventType::Message,
        "system"
        | "progress"
        | "permission-mode"
        | "last-prompt"
        | "queue-operation"
        | "attachment"
        | "file-history-snapshot"
        | "ai-title" => EventType::Notice,
        _ => EventType::Notice,
    }
}

pub(crate) fn claude_content_has_type(content: Option<&Value>, expected: &str) -> bool {
    content
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some(expected))
        })
        .unwrap_or(false)
}
