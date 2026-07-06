#[allow(unused_imports)]
use super::*;

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

#[derive(Clone)]
pub(crate) struct NativeSessionDraft {
    pub(crate) provider: CaptureProvider,
    pub(crate) source_format: &'static str,
    pub(crate) provider_session_id: String,
    pub(crate) parent_provider_session_id: Option<String>,
    pub(crate) root_provider_session_id: Option<String>,
    pub(crate) external_agent_id: Option<String>,
    pub(crate) agent_type: AgentType,
    pub(crate) role_hint: Option<String>,
    pub(crate) is_primary: bool,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) cwd: Option<String>,
    pub(crate) fidelity: Fidelity,
    pub(crate) raw_source_path: String,
    pub(crate) trust: ProviderSourceTrust,
    pub(crate) source_metadata: Value,
    pub(crate) session_metadata: Value,
}

pub(crate) fn normalize_jsonl_tree(
    path: &Path,
    context: &ProviderAdapterContext,
    provider: CaptureProvider,
    source_format: &'static str,
) -> Result<ProviderNormalizationResult> {
    let mut paths = Vec::new();
    collect_jsonl_paths(path, &mut paths)?;
    paths.retain(|path| provider_jsonl_path_is_native(provider, path));
    if provider == CaptureProvider::Antigravity {
        paths = antigravity_preferred_transcript_paths(paths);
    }
    paths.sort();
    if paths.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: native_jsonl_missing_reason(provider),
        });
    }

    let mut merged = ProviderNormalizationResult::default();
    for path in paths {
        match normalize_native_jsonl_session_file(&path, context, provider, source_format) {
            Ok(mut result) => {
                merged.summary.merge(result.summary);
                merged.captures.append(&mut result.captures);
                merged.files_touched.append(&mut result.files_touched);
            }
            Err(err) => return Err(err),
        }
    }
    Ok(merged)
}

pub(crate) fn normalize_native_jsonl_session_file(
    path: &Path,
    context: &ProviderAdapterContext,
    provider: CaptureProvider,
    source_format: &'static str,
) -> Result<ProviderNormalizationResult> {
    ensure_regular_provider_transcript_file(path)?;
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut result = ProviderNormalizationResult::default();
    let mut rows = Vec::new();
    let mut line = Vec::new();
    let mut line_number = 0usize;

    while read_provider_jsonl_line(&mut reader, &mut line)? {
        line_number += 1;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let value: Value = match serde_json::from_slice(&line) {
            Ok(value) => value,
            Err(err) => {
                result.summary.failed += 1;
                result.summary.failures.push(ProviderImportFailure {
                    line: line_number,
                    error: format!("malformed JSONL: {err}"),
                });
                continue;
            }
        };
        rows.push((line_number, value));
    }

    let header_index = if matches!(
        provider,
        CaptureProvider::Antigravity | CaptureProvider::Windsurf
    ) {
        if rows.is_empty() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: native_jsonl_missing_reason(provider),
            });
        }
        0
    } else {
        if rows.is_empty() {
            return Ok(result);
        }
        let Some(header_index) = rows
            .iter()
            .position(|(_, value)| native_jsonl_header_session_id(provider, value).is_some())
        else {
            if let Some((line_number, _)) = rows.first() {
                result.summary.failed += 1;
                result.summary.failures.push(ProviderImportFailure {
                    line: *line_number,
                    error: "no importable native JSONL session header".to_owned(),
                });
                return Ok(result);
            }
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: native_jsonl_missing_reason(provider),
            });
        };
        header_index
    };

    let header = rows[header_index].1.clone();
    let native_session_id = match provider {
        CaptureProvider::Antigravity => {
            antigravity_session_id_from_path(path).unwrap_or_else(|| "unknown-session".to_owned())
        }
        CaptureProvider::Windsurf => {
            windsurf_session_id_from_path(path).unwrap_or_else(|| "unknown-session".to_owned())
        }
        _ => native_jsonl_header_session_id(provider, &header)
            .unwrap_or_else(|| "unknown-session".to_owned()),
    };
    let (provider_session_id, parent_provider_session_id, external_agent_id, agent_type) =
        native_jsonl_path_session(provider, path, &header, &native_session_id);
    let started_at = native_jsonl_timestamp(&header)
        .or_else(|| native_jsonl_header_start_time(provider, &header))
        .unwrap_or(context.imported_at);
    let cwd = native_jsonl_header_cwd(provider, &header);
    let is_subagent = parent_provider_session_id.is_some() || agent_type == AgentType::Subagent;
    let raw_source_path = path.display().to_string();

    for (line_number, value) in rows {
        let occurred_at = native_jsonl_timestamp(&value).unwrap_or(started_at);
        let event = native_jsonl_event(provider, source_format, &value, line_number, occurred_at);
        if let Some(event) = &event {
            result
                .files_touched
                .extend(provider_file_touches_from_raw_value(
                    provider,
                    &provider_session_id,
                    source_format,
                    Some(raw_source_path.as_str()),
                    &value,
                    event,
                    line_number,
                ));
        }
        result.captures.push((
            line_number,
            ProviderCaptureEnvelope {
                schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
                provider,
                source: ProviderSourceEnvelope {
                    source_format: source_format.to_owned(),
                    machine_id: context.machine_id.clone(),
                    observed_at: context.imported_at,
                    raw_source_path: Some(raw_source_path.clone()),
                    raw_retention: ProviderRawRetention::PathReference,
                    redaction_boundary: ProviderRedactionBoundary::BeforeExport,
                    trust: ProviderSourceTrust::ProviderNative,
                    fidelity: Fidelity::Imported,
                    cursor: Some(ProviderCursorRange {
                        before: None,
                        after: Some(ProviderCursorCheckpoint {
                            stream: provider_cursor_stream(provider, source_format),
                            cursor: format!("{}:line:{line_number}", path.display()),
                            observed_at: occurred_at,
                        }),
                    }),
                    idempotency_key: Some(format!(
                        "provider-source:{}:{source_format}:{provider_session_id}",
                        provider.as_str()
                    )),
                    metadata: json!({
                        "adapter": source_format,
                        "native_session_id": native_session_id,
                        "source_path": raw_source_path.clone(),
                    }),
                },
                session: ProviderSessionEnvelope {
                    provider_session_id: provider_session_id.clone(),
                    parent_provider_session_id: parent_provider_session_id.clone(),
                    root_provider_session_id: parent_provider_session_id.clone(),
                    external_agent_id: external_agent_id.clone(),
                    agent_type,
                    role_hint: Some(if is_subagent { "subagent" } else { "primary" }.to_owned()),
                    is_primary: !is_subagent,
                    status: native_jsonl_session_status(provider, &header),
                    started_at,
                    ended_at: None,
                    cwd: cwd.clone(),
                    fidelity: Fidelity::Imported,
                    idempotency_key: Some(format!(
                        "provider-session:{}:{provider_session_id}",
                        provider.as_str()
                    )),
                    artifacts: Vec::new(),
                    metadata: native_jsonl_session_metadata(provider, source_format, &header, path),
                },
                event,
            },
        ));
    }

    Ok(result)
}

pub(crate) fn native_jsonl_session_metadata(
    provider: CaptureProvider,
    source_format: &str,
    header: &Value,
    path: &Path,
) -> Value {
    json!({
        "source_format": source_format,
        "provider": provider.as_str(),
        "source_path": path.display().to_string(),
        "header": provider_capped_json(header, PROVIDER_MAX_PREVIEW_CHARS),
    })
}

pub(crate) fn native_jsonl_event(
    provider: CaptureProvider,
    source_format: &str,
    value: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
) -> Option<ProviderEventEnvelope> {
    let event_type = native_jsonl_event_type(provider, value);
    let entry_type = native_jsonl_entry_type(provider, value);
    let role = native_jsonl_role(provider, value);
    let text = native_jsonl_event_text(provider, value, event_type, &entry_type);
    let (text, truncated) = provider_local_preview(&text, PROVIDER_MAX_TEXT_CHARS);
    let event_id = native_jsonl_event_id(provider, value, line_number);
    let tool_calls = if provider == CaptureProvider::Antigravity {
        value
            .get("tool_calls")
            .map(|calls| provider_capped_json_value(calls, PROVIDER_MAX_PREVIEW_CHARS))
    } else {
        None
    };
    let body = if provider == CaptureProvider::Windsurf {
        windsurf_redacted_body(value)
    } else {
        provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS)
    };

    Some(ProviderEventEnvelope {
        provider_event_index: (line_number - 1) as u64,
        provider_event_hash: Some(event_id.clone()),
        cursor: Some(event_id.clone()),
        event_type,
        role: Some(role),
        occurred_at,
        fidelity: Fidelity::Imported,
        redaction_state: RedactionState::LocalPreview,
        idempotency_key: Some(format!(
            "provider-event:{}:{source_format}:{event_id}",
            provider.as_str()
        )),
        artifacts: Vec::new(),
        payload: json!({
            "entry_type": entry_type,
            "event_id": event_id,
            "native_step_index": value.get("step_index").and_then(Value::as_u64),
            "text": text,
            "truncated": truncated,
            "tool_calls": tool_calls,
            "body": body,
        }),
        metadata: json!({
            "source": source_format,
            "source_format": source_format,
            "line": line_number,
            "entry_type": entry_type,
            "status": value.get("status").and_then(Value::as_str),
            "model": native_jsonl_model(provider, value),
            "tokens": native_jsonl_tokens(provider, value),
        }),
    })
}

pub(crate) fn provider_source_event_import_identity(
    source_id: Uuid,
    provider_event_index: u64,
    event_hash: &str,
) -> ProviderEventImportIdentity {
    provider_source_event_import_identity_with_seq(
        source_id,
        provider_event_index,
        provider_event_index,
        event_hash,
    )
}

pub(crate) fn provider_source_event_import_identity_with_seq(
    source_id: Uuid,
    provider_event_index: u64,
    provider_event_sequence_index: u64,
    event_hash: &str,
) -> ProviderEventImportIdentity {
    ProviderEventImportIdentity {
        id: provider_source_event_uuid(source_id, provider_event_index),
        seq: provider_source_event_seq(source_id, provider_event_sequence_index),
        dedupe_key: Store::provider_source_event_dedupe_key(
            source_id,
            provider_event_index,
            event_hash,
        ),
        run_source_id: Some(source_id),
    }
}

pub(crate) fn avoid_provider_source_event_seq_collision(
    store: &Store,
    mut identity: ProviderEventImportIdentity,
    source_id: Uuid,
    provider_event_index: u64,
    provider_event_sequence_index: u64,
) -> Result<ProviderEventImportIdentity> {
    if provider_event_seq_available(store, identity.seq, identity.id)? {
        return Ok(identity);
    }

    for candidate in [
        provider_event_sequence_index ^ 0x0008_0000,
        provider_event_index,
        provider_event_index ^ 0x0008_0000,
    ] {
        let seq = provider_source_event_seq(source_id, candidate);
        if provider_event_seq_available(store, seq, identity.id)? {
            identity.seq = seq;
            return Ok(identity);
        }
    }

    for salt in 1..1024 {
        let candidate = provider_event_sequence_index.wrapping_add(salt) & 0x000f_ffff;
        let seq = provider_source_event_seq(source_id, candidate);
        if provider_event_seq_available(store, seq, identity.id)? {
            identity.seq = seq;
            return Ok(identity);
        }
    }

    Ok(identity)
}

pub(crate) fn provider_file_touch_event_id(
    store: &Store,
    provider: CaptureProvider,
    provider_session_id: &str,
    source_id: Uuid,
    provider_event_index: u64,
) -> Result<Option<Uuid>> {
    let source_event_id = provider_source_event_uuid(source_id, provider_event_index);
    if provider_event_id_exists(store, source_event_id)? {
        return Ok(Some(source_event_id));
    }

    let legacy_event_id = provider_event_uuid(provider, provider_session_id, provider_event_index);
    if provider_event_id_exists(store, legacy_event_id)? {
        Ok(Some(legacy_event_id))
    } else {
        Ok(None)
    }
}

pub(crate) fn provider_file_touch_import_id(
    store: &Store,
    provider: CaptureProvider,
    provider_session_id: &str,
    source_id: Uuid,
    provider_touch_index: u64,
) -> Result<Uuid> {
    let source_touch_id = provider_source_file_touch_uuid(source_id, provider_touch_index);
    if store.file_touched_exists(source_touch_id)? {
        return Ok(source_touch_id);
    }

    let legacy_touch_id =
        provider_file_touch_uuid(provider, provider_session_id, provider_touch_index);
    if store.file_touched_exists(legacy_touch_id)? {
        Ok(legacy_touch_id)
    } else {
        Ok(source_touch_id)
    }
}

#[cfg(test)]
pub(crate) fn provider_source_uuid(provider: CaptureProvider, provider_session_id: &str) -> Uuid {
    stable_capture_uuid(
        &format!("provider:{}:{provider_session_id}", provider.as_str()),
        "source",
    )
}

pub(crate) fn provider_scoped_source_identity_key(
    provider: CaptureProvider,
    provider_session_id: &str,
    source_format: &str,
    raw_source_path: Option<&str>,
) -> String {
    serde_json::to_string(&(
        "provider-source-v2",
        provider.as_str(),
        provider_session_id,
        source_format,
        raw_source_path,
    ))
    .expect("provider source identity key should serialize")
}

pub(crate) fn provider_source_run_uuid(source_id: Uuid, run_key: &str) -> Uuid {
    stable_capture_uuid(&format!("provider-source:{source_id}:run:{run_key}"), "run")
}

pub(crate) fn provider_source_event_uuid(source_id: Uuid, provider_event_index: u64) -> Uuid {
    stable_capture_uuid(
        &format!("provider-source:{source_id}:event:{provider_event_index}"),
        "event",
    )
}

pub(crate) fn provider_source_file_touch_uuid(source_id: Uuid, provider_touch_index: u64) -> Uuid {
    stable_capture_uuid(
        &format!("provider-source:{source_id}:file-touch:{provider_touch_index}"),
        "file-touch",
    )
}

pub(crate) fn provider_source_event_seq(source_id: Uuid, provider_event_index: u64) -> u64 {
    let source_key = source_id.to_string();
    ((fnv1a64(source_key.as_bytes()) & 0x0000_0000_7fff_ffff) << 32)
        | (provider_event_index & 0xffff_ffff)
}
