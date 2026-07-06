#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone)]
pub struct PiSessionImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for PiSessionImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: utc_now(),
            history_record_id: None,
            allow_partial_failures: false,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PiSessionHeader {
    pub(crate) id: String,
    pub(crate) version: Option<u64>,
    pub(crate) timestamp: DateTime<Utc>,
    pub(crate) cwd: Option<String>,
    pub(crate) parent_session: Option<String>,
    pub(crate) raw: Value,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PiSessionJsonlAdapter;

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

    let mut paths = Vec::new();
    collect_jsonl_paths(path, &mut paths)?;
    paths.sort();
    if paths.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: native_jsonl_missing_reason(CaptureProvider::Pi),
        });
    }

    let mut merged = ProviderNormalizationResult::default();
    for path in paths {
        let mut file_context = context.clone();
        file_context.source_path = Some(path.clone());
        let mut result = normalize_pi_session_jsonl_file(&path, &file_context)?;
        merged.summary.merge(result.summary);
        merged.captures.append(&mut result.captures);
        merged.files_touched.append(&mut result.files_touched);
    }
    Ok(merged)
}

pub(crate) fn normalize_pi_session_jsonl_file(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    ensure_regular_provider_transcript_file(path)?;
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut result = ProviderNormalizationResult::default();
    let mut header = None;
    let mut line = Vec::new();
    let mut line_number = 0usize;

    while read_provider_jsonl_line(&mut reader, &mut line)? {
        line_number += 1;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }

        let value: Value = match serde_json::from_slice::<Value>(&line) {
            Ok(value) => value,
            Err(err) => {
                result.summary.failed += 1;
                result.summary.failures.push(ProviderImportFailure {
                    line: line_number,
                    error: err.to_string(),
                });
                continue;
            }
        };
        let entry_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if entry_type == "session" {
            match pi_session_header(value) {
                Ok(parsed) => {
                    let capture = pi_session_capture(&parsed, None, line_number, context)?;
                    header = Some(parsed);
                    result.captures.push((line_number, capture));
                }
                Err(err) => {
                    result.summary.failed += 1;
                    result.summary.failures.push(ProviderImportFailure {
                        line: line_number,
                        error: err.to_string(),
                    });
                }
            }
            continue;
        }

        let Some(header) = header.as_ref() else {
            result.summary.failed += 1;
            result.summary.failures.push(ProviderImportFailure {
                line: line_number,
                error: "pi session entry appeared before session header".to_owned(),
            });
            continue;
        };
        match pi_session_capture(header, Some(value), line_number, context) {
            Ok(capture) => result.captures.push((line_number, capture)),
            Err(err) => {
                result.summary.failed += 1;
                result.summary.failures.push(ProviderImportFailure {
                    line: line_number,
                    error: err.to_string(),
                });
            }
        }
    }

    Ok(result)
}

pub fn import_pi_session_jsonl(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: PiSessionImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = PiSessionJsonlAdapter.normalize_path(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            imported_at: options.imported_at,
            tool_output_mode: CodexToolOutputMode::Full,
            event_mode: CodexEventImportMode::Rich,
            include_notices: true,
        },
    )?;

    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
        },
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
            raw_retention: ProviderRawRetention::PathReference,
            redaction_boundary: ProviderRedactionBoundary::BeforeExport,
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
    let text = pi_entry_text(entry, message);
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
        redaction_state: RedactionState::LocalPreview,
        idempotency_key: Some(pi_event_idempotency_key(header, entry, line_number)),
        artifacts: Vec::new(),
        payload: json!({
            "entry_type": entry_type,
            "entry_id": entry.get("id").and_then(Value::as_str),
            "parent_id": entry.get("parentId").and_then(Value::as_str),
            "message_role": message_role,
            "text": text,
            "body": entry,
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

#[derive(Default)]
pub(crate) struct ProviderImportCaches {
    pub(crate) imported_sessions: BTreeSet<Uuid>,
    pub(crate) processed_sources: BTreeSet<Uuid>,
    pub(crate) processed_sessions: BTreeSet<Uuid>,
    pub(crate) imported_edges: BTreeSet<Uuid>,
    pub(crate) processed_edges: BTreeSet<Uuid>,
    pub(crate) session_exists: BTreeMap<Uuid, bool>,
    pub(crate) pi_event_identities_by_entry_id:
        BTreeMap<Uuid, BTreeMap<String, ProviderEventImportIdentity>>,
    pub(crate) pending_edges: BTreeMap<Uuid, PendingProviderEdge>,
}

pub(crate) fn pi_existing_event_identity_by_entry_id(
    store: &Store,
    provider: CaptureProvider,
    session_id: Uuid,
    entry_id: Option<&str>,
    caches: &mut ProviderImportCaches,
) -> Result<Option<ProviderEventImportIdentity>> {
    if provider != CaptureProvider::Pi {
        return Ok(None);
    }
    let Some(entry_id) = entry_id.filter(|id| !id.trim().is_empty()) else {
        return Ok(None);
    };
    if let std::collections::btree_map::Entry::Vacant(entry) =
        caches.pi_event_identities_by_entry_id.entry(session_id)
    {
        let mut identities = BTreeMap::new();
        for event in store.events_for_session(session_id)? {
            let Some(existing_entry_id) = pi_stored_event_entry_id(&event) else {
                continue;
            };
            let Some(dedupe_key) = event.dedupe_key.clone() else {
                continue;
            };
            identities
                .entry(existing_entry_id.to_owned())
                .or_insert(ProviderEventImportIdentity {
                    id: event.id,
                    seq: event.seq,
                    dedupe_key,
                    run_source_id: event.capture_source_id,
                });
        }
        entry.insert(identities);
    }
    Ok(caches
        .pi_event_identities_by_entry_id
        .get(&session_id)
        .and_then(|identities| identities.get(entry_id).cloned()))
}

pub(crate) fn pi_stored_event_entry_id(event: &Event) -> Option<&str> {
    event
        .payload
        .pointer("/body/entry_id")
        .and_then(Value::as_str)
        .or_else(|| {
            event
                .payload
                .pointer("/body/body/id")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            event
                .sync
                .metadata
                .pointer("/metadata/entry_id")
                .and_then(Value::as_str)
        })
}
