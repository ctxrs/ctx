use std::{fs, path::Path};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderCursorCheckpoint, ProviderCursorRange, ProviderEventEnvelope, ProviderSessionEnvelope,
    ProviderSourceEnvelope, ProviderSourceTrust, SessionStatus,
    PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::provider::providers::native_jsonl::native_jsonl_missing_reason;
use crate::provider::providers::real_content::event_has_real_conversation_content;

use crate::common::path_inventory::SortedJsonlPathInventory;
use crate::common::scratch::CaptureScratchSpace;
use crate::common::time::parse_optional_rfc3339_field;
use crate::provider::adapter::PiSessionJsonlAdapter;
use crate::provider::importer::{
    provider_cursor_stream, ProviderNormalizationBatcher, ProviderSessionContentPolicy,
};
use crate::provider::native::{
    provider_capped_json, provider_policy_body, provider_policy_event_text,
};
use crate::{
    fnv1a64, CaptureError, ProviderAdapterContext, ProviderCaptureAdapter, ProviderImportFailure,
    ProviderJsonlReader, ProviderNormalizationResult, Result, PROVIDER_MAX_PREVIEW_CHARS,
};

#[derive(Clone)]
pub(crate) struct PiSessionHeader {
    pub(crate) id: String,
    pub(crate) version: Option<u64>,
    pub(crate) timestamp: DateTime<Utc>,
    pub(crate) cwd: Option<String>,
    pub(crate) parent_session: Option<String>,
    pub(crate) raw: Value,
}

pub(crate) struct PiJsonlScan {
    pub(crate) has_real_message: bool,
    pub(crate) additional_session_header: bool,
    pub(crate) failed: usize,
    pub(crate) last_line_number: usize,
    pub(crate) admission: PiSessionAdmission,
}

pub(crate) struct PiSessionAdmission {
    connection: Connection,
    _scratch: CaptureScratchSpace,
}

impl PiSessionAdmission {
    fn new() -> Result<Self> {
        let scratch = CaptureScratchSpace::create("pi-admission")?;
        drop(scratch.create_file("pi-session-admission.sqlite")?);
        let connection = Connection::open(scratch.path().join("pi-session-admission.sqlite"))?;
        connection.execute_batch(
            "CREATE TABLE session_admission (
                 provider_session_id TEXT PRIMARY KEY NOT NULL,
                 has_capture INTEGER NOT NULL DEFAULT 0 CHECK (has_capture IN (0, 1)),
                 has_real_message INTEGER NOT NULL DEFAULT 0 CHECK (has_real_message IN (0, 1))
             ) WITHOUT ROWID;",
        )?;
        Ok(Self {
            connection,
            _scratch: scratch,
        })
    }

    fn observe_session(&self, provider_session_id: &str) -> Result<()> {
        self.connection.execute(
            "INSERT OR IGNORE INTO session_admission (provider_session_id) VALUES (?1)",
            params![provider_session_id],
        )?;
        Ok(())
    }

    fn mark_capture(&self, provider_session_id: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE session_admission SET has_capture = 1 WHERE provider_session_id = ?1",
            params![provider_session_id],
        )?;
        Ok(())
    }

    fn mark_real(&self, provider_session_id: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE session_admission SET has_real_message = 1 WHERE provider_session_id = ?1",
            params![provider_session_id],
        )?;
        Ok(())
    }

    fn admits(&self, provider_session_id: &str) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT has_real_message FROM session_admission WHERE provider_session_id = ?1",
            params![provider_session_id],
            |row| row.get(0),
        )?)
    }

    pub(crate) fn filter_batch(
        &self,
        mut normalization: ProviderNormalizationResult,
    ) -> Result<ProviderNormalizationResult> {
        let mut captures = Vec::with_capacity(normalization.captures.len());
        for capture in normalization.captures {
            if self.admits(&capture.1.session.provider_session_id)? {
                captures.push(capture);
            } else {
                normalization.summary.skipped += 1;
                if capture.1.event.is_some() {
                    normalization.summary.skipped_events += 1;
                }
            }
        }
        normalization.captures = captures;

        let mut files_touched = Vec::with_capacity(normalization.files_touched.len());
        for file in normalization.files_touched {
            if self.admits(&file.1.provider_session_id)? {
                files_touched.push(file);
            } else {
                normalization.summary.skipped += 1;
            }
        }
        normalization.files_touched = files_touched;
        Ok(normalization)
    }

    pub(crate) fn rejected_session_count(&self) -> Result<usize> {
        Ok(self.connection.query_row(
            "SELECT COUNT(*) FROM session_admission
             WHERE has_capture = 1 AND has_real_message = 0",
            [],
            |row| row.get(0),
        )?)
    }
}

pub(crate) fn scan_pi_session_jsonl_reader(
    reader: &mut ProviderJsonlReader,
    context: &ProviderAdapterContext,
    bootstrap_header: Option<PiSessionHeader>,
    reject_additional_session_header: bool,
) -> Result<std::result::Result<PiJsonlScan, crate::ProviderJsonlReplacementReason>> {
    let admission = PiSessionAdmission::new()?;
    let mut header = bootstrap_header;
    if let Some(header) = header.as_ref() {
        admission.observe_session(&header.id)?;
    }
    let mut additional_session_header = false;
    let mut has_real_message = false;
    let mut failed = 0usize;
    let mut scratch_summary = crate::ProviderImportSummary::default();
    let mut line = Vec::new();
    let mut line_number = 0usize;
    while reader.read_record_or_skip_oversized(&mut line, &mut line_number, &mut scratch_summary)? {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let value: Value = match serde_json::from_slice(&line) {
            Ok(value) => value,
            Err(_) => {
                failed += 1;
                continue;
            }
        };
        if value.get("type").and_then(Value::as_str) == Some("session") {
            if reject_additional_session_header && header.is_some() {
                reader.restart_import_position()?;
                return Ok(Err(
                    crate::ProviderJsonlReplacementReason::AdditionalSessionHeader,
                ));
            }
            if header.is_some() {
                additional_session_header = true;
            }
            match pi_session_header(value) {
                Ok(parsed) => {
                    admission.observe_session(&parsed.id)?;
                    header = Some(parsed);
                }
                Err(_) => failed += 1,
            }
            continue;
        }
        let Some(header) = header.as_ref() else {
            failed += 1;
            continue;
        };
        match pi_session_capture(header, Some(value), line_number, context) {
            Ok(capture) => {
                admission.mark_capture(&header.id)?;
                let row_has_real = capture
                    .event
                    .as_ref()
                    .is_some_and(pi_event_has_real_message_content);
                if row_has_real {
                    has_real_message = true;
                    admission.mark_real(&header.id)?;
                }
            }
            Err(_) => failed += 1,
        }
    }
    reader.restart_import_position()?;
    Ok(Ok(PiJsonlScan {
        has_real_message,
        additional_session_header,
        failed,
        last_line_number: line_number,
        admission,
    }))
}

impl ProviderCaptureAdapter for PiSessionJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &str {
        "pi_session_jsonl"
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_pi_session_jsonl_path(path, context)
    }
}

pub(crate) fn normalize_pi_session_jsonl_path(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    if fs::symlink_metadata(path)?.file_type().is_file() {
        return normalize_pi_session_jsonl_file(path, context);
    }

    let path_inventory = SortedJsonlPathInventory::build(path, |_| true)?;
    if path_inventory.metrics().paths == 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: native_jsonl_missing_reason(CaptureProvider::Pi),
        });
    }

    let mut merged = ProviderNormalizationResult::default();
    path_inventory.for_each(|file_path| {
        let mut file_context = context.clone();
        file_context.source_path = Some(file_path.clone());
        let mut result = normalize_pi_session_jsonl_file(&file_path, &file_context)?;
        merged.summary.merge(result.summary);
        merged.captures.append(&mut result.captures);
        merged.files_touched.append(&mut result.files_touched);
        Ok(())
    })?;
    Ok(merged)
}

pub(crate) fn normalize_pi_session_jsonl_file(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let mut reader = ProviderJsonlReader::open_replacement(path)?;
    normalize_pi_session_jsonl_reader(
        &mut reader,
        context,
        None,
        ProviderSessionContentPolicy::RequireRealMessage,
        false,
    )
    .map(|decision| {
        decision
            .expect("replacement Pi parsing cannot request replacement")
            .0
    })
}

pub(crate) fn normalize_pi_session_jsonl_reader(
    reader: &mut ProviderJsonlReader,
    context: &ProviderAdapterContext,
    bootstrap_header: Option<PiSessionHeader>,
    content_policy: ProviderSessionContentPolicy,
    reject_additional_session_header: bool,
) -> Result<
    std::result::Result<(ProviderNormalizationResult, bool), crate::ProviderJsonlReplacementReason>,
> {
    let scan = match scan_pi_session_jsonl_reader(
        reader,
        context,
        bootstrap_header.clone(),
        reject_additional_session_header,
    )? {
        Ok(scan) => scan,
        Err(reason) => return Ok(Err(reason)),
    };
    let retain_captures = scan.has_real_message
        || content_policy == ProviderSessionContentPolicy::AllowTailWithoutRealMessage;
    let mut result = ProviderNormalizationResult::default();
    stream_pi_session_jsonl_reader(reader, context, bootstrap_header, |mut batch| {
        if !retain_captures {
            batch.captures.clear();
            batch.files_touched.clear();
        }
        result.summary.merge(batch.summary);
        result.captures.append(&mut batch.captures);
        result.files_touched.append(&mut batch.files_touched);
        Ok(())
    })?;
    debug_assert_eq!(result.summary.failed, scan.failed);
    if !retain_captures && result.summary.failed == 0 {
        result.summary.failed += 1;
        result.summary.sample_failure(ProviderImportFailure {
            line: scan.last_line_number,
            error: "pi session JSONL contained no real message content".to_owned(),
        });
    }

    Ok(Ok((result, scan.additional_session_header)))
}

pub(crate) fn stream_pi_session_jsonl_reader<F>(
    reader: &mut ProviderJsonlReader,
    context: &ProviderAdapterContext,
    bootstrap_header: Option<PiSessionHeader>,
    emit: F,
) -> Result<()>
where
    F: FnMut(ProviderNormalizationResult) -> Result<()>,
{
    let mut batches = ProviderNormalizationBatcher::new(emit);
    let mut header = bootstrap_header;
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
                    error: err.to_string(),
                });
                batches.record_processed()?;
                continue;
            }
        };
        if value.get("type").and_then(Value::as_str) == Some("session") {
            match pi_session_header(value) {
                Ok(parsed) => header = Some(parsed),
                Err(err) => {
                    let result = batches.current_mut();
                    result.summary.failed += 1;
                    result.summary.sample_failure(ProviderImportFailure {
                        line: line_number,
                        error: err.to_string(),
                    });
                }
            }
            batches.record_processed()?;
            continue;
        }
        let Some(header) = header.as_ref() else {
            let result = batches.current_mut();
            result.summary.failed += 1;
            result.summary.sample_failure(ProviderImportFailure {
                line: line_number,
                error: "pi session entry appeared before session header".to_owned(),
            });
            batches.record_processed()?;
            continue;
        };
        match pi_session_capture(header, Some(value), line_number, context) {
            Ok(capture) => batches.current_mut().captures.push((line_number, capture)),
            Err(err) => {
                let result = batches.current_mut();
                result.summary.failed += 1;
                result.summary.sample_failure(ProviderImportFailure {
                    line: line_number,
                    error: err.to_string(),
                });
            }
        }
        batches.record_processed()?;
    }
    batches.finish()
}

pub(crate) fn pi_event_has_real_message_content(event: &ProviderEventEnvelope) -> bool {
    event_has_real_conversation_content(
        event.event_type,
        event.payload.get("text").and_then(Value::as_str),
    )
}

pub(crate) fn pi_session_header(value: Value) -> Result<PiSessionHeader> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| CaptureError::InvalidPayload("pi session header missing id".to_owned()))?
        .to_owned();
    let timestamp = value
        .get("timestamp")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CaptureError::InvalidPayload("pi session header missing timestamp".to_owned())
        })
        .and_then(|timestamp| {
            DateTime::parse_from_rfc3339(timestamp)
                .map(|time| time.with_timezone(&Utc))
                .map_err(CaptureError::from)
        })?;
    Ok(PiSessionHeader {
        id,
        version: value.get("version").and_then(Value::as_u64),
        timestamp,
        cwd: value.get("cwd").and_then(Value::as_str).map(str::to_owned),
        parent_session: value
            .get("parentSession")
            .and_then(Value::as_str)
            .map(str::to_owned),
        raw: value,
    })
}

pub(crate) fn pi_session_capture(
    header: &PiSessionHeader,
    entry: Option<Value>,
    line_number: usize,
    context: &ProviderAdapterContext,
) -> Result<ProviderCaptureEnvelope> {
    let event = entry
        .map(|entry| pi_session_event(header, &entry, line_number))
        .transpose()?;
    let cursor = event.as_ref().and_then(|event| {
        event.cursor.as_ref().map(|cursor| ProviderCursorRange {
            before: None,
            after: Some(ProviderCursorCheckpoint {
                stream: provider_cursor_stream(CaptureProvider::Pi, "pi_session_jsonl"),
                cursor: cursor.clone(),
                observed_at: event.occurred_at,
            }),
        })
    });

    Ok(ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider: CaptureProvider::Pi,
        source: ProviderSourceEnvelope {
            source_format: "pi_session_jsonl".to_owned(),
            machine_id: context.machine_id.clone(),
            observed_at: context.imported_at,
            raw_source_path: context
                .source_path
                .as_ref()
                .map(|path| path.display().to_string()),
            source_root: context.source_root_display(),
            trust: ProviderSourceTrust::ProviderExport,
            fidelity: Fidelity::Imported,
            cursor,
            idempotency_key: Some(format!("provider-source:pi:pi_session_jsonl:{}", header.id)),
            metadata: json!({
                "adapter": "pi_session_jsonl",
                "source_fidelity": "documented_session_jsonl",
            }),
        },
        session: ProviderSessionEnvelope {
            provider_session_id: header.id.clone(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            started_at: header.timestamp,
            ended_at: None,
            cwd: header.cwd.clone(),
            fidelity: Fidelity::Imported,
            idempotency_key: Some(format!("provider-session:pi:{}", header.id)),
            artifacts: Vec::new(),
            metadata: json!({
                "source_format": "pi_session_jsonl",
                "source_fidelity": "documented_session_jsonl",
                "version": header.version,
                "parent_session": header.parent_session,
                "header": header.raw,
                "limitations": [
                    "message branch parentId values are preserved as event metadata, not ctx session edges",
                    "files touched are available only when Pi message payloads include them",
                    "raw image content is not expanded into artifacts by this importer"
                ],
            }),
        },
        event,
    })
}

pub(crate) fn pi_session_event(
    header: &PiSessionHeader,
    entry: &Value,
    line_number: usize,
) -> Result<ProviderEventEnvelope> {
    let entry_type = entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let message = entry.get("message");
    let message_role = message
        .and_then(|message| message.get("role"))
        .and_then(Value::as_str);
    let occurred_at = parse_optional_rfc3339_field(entry, "timestamp")?.ok_or_else(|| {
        CaptureError::InvalidPayload("pi session event missing timestamp".to_owned())
    })?;
    let event_type = pi_event_type(entry_type, message);
    let role = message_role.map(pi_event_role);
    let text = pi_entry_text(entry, message).unwrap_or_default();
    let retained_text = provider_policy_event_text(event_type, &text, entry);
    let provider_event_index = (line_number - 1) as u64;
    let provider_event_identity_index =
        pi_provider_event_identity_index(header, entry).unwrap_or(provider_event_index);
    let legacy_provider_event_index = provider_event_index;

    Ok(ProviderEventEnvelope {
        provider_event_index,
        provider_event_hash: None,
        cursor: entry.get("id").and_then(Value::as_str).map(str::to_owned),
        event_type,
        role,
        occurred_at,
        fidelity: Fidelity::Imported,
        idempotency_key: Some(pi_event_idempotency_key(header, entry, line_number)),
        artifacts: Vec::new(),
        payload: json!({
            "entry_type": entry_type,
            "entry_id": entry.get("id").and_then(Value::as_str),
            "parent_id": entry.get("parentId").and_then(Value::as_str),
            "message_role": message_role,
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "body": provider_capped_json(&provider_policy_body(event_type, entry), PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": "pi_session",
            "source_format": "pi_session_jsonl",
            "line": line_number,
            "entry_type": entry_type,
            "entry_id": entry.get("id").and_then(Value::as_str),
            "parent_id": entry.get("parentId").and_then(Value::as_str),
            "provider_event_identity_index": provider_event_identity_index,
            "legacy_provider_event_index": legacy_provider_event_index,
            "message_role": message_role,
            "model": message
                .and_then(|message| message.get("model"))
                .and_then(Value::as_str),
            "provider": message
                .and_then(|message| message.get("provider"))
                .and_then(Value::as_str),
            "usage": message.and_then(|message| message.get("usage")).cloned(),
        }),
    })
}

pub(crate) fn pi_provider_event_identity_index(
    header: &PiSessionHeader,
    entry: &Value,
) -> Option<u64> {
    entry
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(|id| fnv1a64(format!("pi:{}:{id}", header.id).as_bytes()))
}

pub(crate) fn pi_event_idempotency_key(
    header: &PiSessionHeader,
    entry: &Value,
    line_number: usize,
) -> String {
    entry
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(|id| format!("provider-event:pi:{}:{id}", header.id))
        .unwrap_or_else(|| format!("provider-event:pi:{}:{line_number}", header.id))
}

pub(crate) fn pi_event_type(entry_type: &str, message: Option<&Value>) -> EventType {
    match entry_type {
        "compaction" | "branch_summary" => EventType::Summary,
        "message" => match message
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
        {
            "toolResult" => EventType::ToolOutput,
            "bashExecution" => EventType::CommandOutput,
            "assistant" if message.is_some_and(pi_message_has_tool_call) => EventType::ToolCall,
            _ => EventType::Message,
        },
        "model_change"
        | "thinking_level_change"
        | "custom"
        | "custom_message"
        | "label"
        | "session_info" => EventType::Notice,
        _ => EventType::Notice,
    }
}

pub(crate) fn pi_event_role(role: &str) -> EventRole {
    match role {
        "user" => EventRole::User,
        "assistant" => EventRole::Assistant,
        "toolResult" | "bashExecution" => EventRole::Tool,
        "system" => EventRole::System,
        _ => EventRole::Unknown,
    }
}

pub(crate) fn pi_message_has_tool_call(message: &Value) -> bool {
    message
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some("toolCall"))
        })
        .unwrap_or(false)
}

pub(crate) fn pi_entry_text(entry: &Value, message: Option<&Value>) -> Option<String> {
    if let Some(text) = message.and_then(pi_message_text) {
        return Some(text);
    }
    match entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
    {
        "compaction" | "branch_summary" => entry
            .get("summary")
            .and_then(Value::as_str)
            .map(str::to_owned),
        "custom_message" => entry.get("content").and_then(pi_content_text),
        "session_info" => entry.get("name").and_then(Value::as_str).map(str::to_owned),
        "label" => entry
            .get("label")
            .and_then(Value::as_str)
            .map(str::to_owned),
        "model_change" => {
            let provider = entry.get("provider").and_then(Value::as_str).unwrap_or("");
            let model = entry.get("modelId").and_then(Value::as_str).unwrap_or("");
            let label = [provider, model]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("/");
            (!label.is_empty()).then_some(label)
        }
        "thinking_level_change" => entry
            .get("thinkingLevel")
            .and_then(Value::as_str)
            .map(str::to_owned),
        "custom" => entry
            .get("customType")
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

pub(crate) fn pi_message_text(message: &Value) -> Option<String> {
    if let Some(command) = message.get("command").and_then(Value::as_str) {
        let output = message.get("output").and_then(Value::as_str).unwrap_or("");
        return Some(if output.is_empty() {
            command.to_owned()
        } else {
            format!("{command}\n{output}")
        });
    }
    if let Some(summary) = message
        .get("summary")
        .or_else(|| message.get("content"))
        .and_then(Value::as_str)
    {
        return Some(summary.to_owned());
    }
    message.get("content").and_then(pi_content_text)
}

pub(crate) fn pi_content_text(content: &Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return Some(text.to_owned());
    }
    let blocks = content.as_array()?;
    let mut parts = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    parts.push(text.to_owned());
                }
            }
            Some("thinking") => {
                if let Some(text) = block.get("thinking").and_then(Value::as_str) {
                    parts.push(text.to_owned());
                }
            }
            Some("toolCall") => {
                let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                parts.push(format!("tool call: {name}"));
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}
