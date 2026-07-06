#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderSessionDto {
    pub provider_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: AgentType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_hint: Option<String>,
    #[serde(default)]
    pub is_primary: bool,
    #[serde(default)]
    pub status: SessionStatus,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default = "default_metadata")]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderFileTouchedEnvelope {
    pub provider: CaptureProvider,
    pub provider_session_id: String,
    pub provider_touch_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_event_index: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_source_path: Option<String>,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_kind: Option<FileChangeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_count_delta: Option<i64>,
    #[serde(default)]
    pub confidence: Confidence,
    pub occurred_at: DateTime<Utc>,
    pub source_format: String,
    #[serde(default = "default_metadata")]
    pub metadata: Value,
}

pub(crate) fn provider_file_touches_from_event(
    provider: CaptureProvider,
    provider_session_id: &str,
    source_format: &str,
    raw_source_path: Option<&str>,
    event: &ProviderEventEnvelope,
    line_number: usize,
) -> Vec<(usize, ProviderFileTouchedEnvelope)> {
    if !matches!(
        event.event_type,
        EventType::ToolCall
            | EventType::ToolOutput
            | EventType::CommandOutput
            | EventType::FileTouched
    ) {
        return Vec::new();
    }

    let mut drafts = Vec::new();
    collect_patch_file_touches(&event.payload, &mut drafts);
    if drafts.is_empty() && event_type_supports_structured_file_touches(event.event_type) {
        collect_structured_file_touches(&event.payload, &mut drafts);
    }

    provider_file_touch_envelopes(
        ProviderFileTouchEnvelopeContext {
            provider,
            provider_session_id,
            source_format,
            raw_source_path,
            occurred_at: event.occurred_at,
            provider_event_index: Some(event.provider_event_index),
            provider_touch_base_index: event.provider_event_index << 16,
            line_number,
        },
        drafts,
    )
}

pub(crate) fn provider_file_touches_from_raw_value(
    provider: CaptureProvider,
    provider_session_id: &str,
    source_format: &str,
    raw_source_path: Option<&str>,
    raw_value: &Value,
    event: &ProviderEventEnvelope,
    line_number: usize,
) -> Vec<(usize, ProviderFileTouchedEnvelope)> {
    if !matches!(
        event.event_type,
        EventType::ToolCall
            | EventType::ToolOutput
            | EventType::CommandOutput
            | EventType::FileTouched
    ) {
        return Vec::new();
    }

    let mut drafts = Vec::new();
    collect_patch_file_touches(raw_value, &mut drafts);
    if drafts.is_empty() && (event_type_supports_structured_file_touches(event.event_type)) {
        collect_structured_file_touches(raw_value, &mut drafts);
    }

    provider_file_touch_envelopes(
        ProviderFileTouchEnvelopeContext {
            provider,
            provider_session_id,
            source_format,
            raw_source_path,
            occurred_at: event.occurred_at,
            provider_event_index: Some(event.provider_event_index),
            provider_touch_base_index: event.provider_event_index << 16,
            line_number,
        },
        drafts,
    )
}

pub(crate) struct ProviderFileTouchEnvelopeContext<'a> {
    pub(crate) provider: CaptureProvider,
    pub(crate) provider_session_id: &'a str,
    pub(crate) source_format: &'a str,
    pub(crate) raw_source_path: Option<&'a str>,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) provider_event_index: Option<u64>,
    pub(crate) provider_touch_base_index: u64,
    pub(crate) line_number: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct OpenCodeSessionRow {
    pub(crate) id: String,
    pub(crate) parent_id: Option<String>,
    pub(crate) title: String,
    pub(crate) directory: String,
    pub(crate) model: Option<String>,
    pub(crate) agent: Option<String>,
    pub(crate) time_created: i64,
    pub(crate) time_updated: i64,
    pub(crate) tokens_input: i64,
    pub(crate) tokens_output: i64,
    pub(crate) tokens_reasoning: i64,
    pub(crate) tokens_cache_read: i64,
    pub(crate) tokens_cache_write: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct OpenCodeMessageRow {
    pub(crate) id: String,
    pub(crate) session_id: String,
    pub(crate) entry_type: String,
    pub(crate) seq: i64,
    pub(crate) time_created: i64,
    pub(crate) time_updated: i64,
    pub(crate) data: String,
}

#[derive(Debug, Clone)]
pub(crate) struct OpenHandsEventFile {
    pub(crate) path: PathBuf,
    pub(crate) line_number: usize,
    pub(crate) session_id: String,
    pub(crate) user_id: Option<String>,
    pub(crate) event_id: String,
    pub(crate) timestamp: DateTime<Utc>,
    pub(crate) value: Value,
}

pub(crate) fn native_provider_capture(
    draft: NativeSessionDraft,
    context: &ProviderAdapterContext,
    event: Option<ProviderEventEnvelope>,
) -> ProviderCaptureEnvelope {
    ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider: draft.provider,
        source: ProviderSourceEnvelope {
            source_format: draft.source_format.to_owned(),
            machine_id: context.machine_id.clone(),
            observed_at: context.imported_at,
            raw_source_path: Some(draft.raw_source_path),
            raw_retention: ProviderRawRetention::PathReference,
            redaction_boundary: ProviderRedactionBoundary::BeforeExport,
            trust: draft.trust,
            fidelity: draft.fidelity,
            cursor: event.as_ref().and_then(|event| {
                event.cursor.as_ref().map(|cursor| ProviderCursorRange {
                    before: None,
                    after: Some(ProviderCursorCheckpoint {
                        stream: provider_cursor_stream(draft.provider, draft.source_format),
                        cursor: cursor.clone(),
                        observed_at: event.occurred_at,
                    }),
                })
            }),
            idempotency_key: Some(format!(
                "provider-source:{}:{}:{}",
                draft.provider.as_str(),
                draft.source_format,
                draft.provider_session_id
            )),
            metadata: draft.source_metadata,
        },
        session: ProviderSessionEnvelope {
            provider_session_id: draft.provider_session_id.clone(),
            parent_provider_session_id: draft.parent_provider_session_id,
            root_provider_session_id: draft.root_provider_session_id,
            external_agent_id: draft.external_agent_id,
            agent_type: draft.agent_type,
            role_hint: draft.role_hint,
            is_primary: draft.is_primary,
            status: SessionStatus::Imported,
            started_at: draft.started_at,
            ended_at: draft.ended_at,
            cwd: draft.cwd,
            fidelity: draft.fidelity,
            idempotency_key: Some(format!(
                "provider-session:{}:{}",
                draft.provider.as_str(),
                draft.provider_session_id
            )),
            artifacts: Vec::new(),
            metadata: draft.session_metadata,
        },
        event,
    }
}

pub(crate) struct OpenClawCaptureDraft<'a> {
    pub(crate) provider_session_id: &'a str,
    pub(crate) agent_id: Option<&'a str>,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) cwd: Option<String>,
    pub(crate) path: &'a Path,
    pub(crate) indexes: &'a BTreeMap<String, Value>,
    pub(crate) header_raw: Value,
    pub(crate) event: Option<ProviderEventEnvelope>,
}

#[derive(Debug, Clone)]
pub(crate) struct NanoClawSessionRow {
    pub(crate) id: String,
    pub(crate) agent_group_id: String,
    pub(crate) messaging_group_id: Option<String>,
    pub(crate) thread_id: Option<String>,
    pub(crate) agent_provider: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) container_status: Option<String>,
    pub(crate) last_active: Option<i64>,
    pub(crate) created_at: Option<i64>,
    pub(crate) agent_group_name: Option<String>,
    pub(crate) agent_group_folder: Option<String>,
    pub(crate) messaging_channel_type: Option<String>,
    pub(crate) messaging_platform_id: Option<String>,
    pub(crate) messaging_instance: Option<String>,
    pub(crate) messaging_name: Option<String>,
}

pub(crate) struct AstrBotCaptureDraft<'a> {
    pub(crate) conversation: &'a AstrBotConversationRow,
    pub(crate) provider_session_id: &'a str,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) path: &'a Path,
    pub(crate) user_version: i64,
    pub(crate) schema_fingerprint: &'a str,
    pub(crate) selected_conversation: Option<&'a str>,
    pub(crate) event: Option<ProviderEventEnvelope>,
}

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
