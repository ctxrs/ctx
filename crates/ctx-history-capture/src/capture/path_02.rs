#[allow(unused_imports)]
use super::*;

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

#[allow(clippy::too_many_arguments)]
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

pub(crate) fn percent_decode_uri_path(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hi = (bytes[index + 1] as char).to_digit(16);
            let lo = (bytes[index + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push(((hi << 4) | lo) as u8);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| value.to_owned())
}

pub(crate) fn provider_path_has_component(path: &Path, expected: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str() == expected)
}

pub(crate) fn collect_named_paths(
    root: &Path,
    name: &str,
    paths: &mut Vec<PathBuf>,
    visited: &mut usize,
    max_paths: usize,
    max_visited: usize,
) {
    if paths.len() >= max_paths || *visited >= max_visited {
        return;
    }
    *visited += 1;
    let Ok(metadata) = fs::symlink_metadata(root) else {
        return;
    };
    if metadata.file_type().is_symlink() {
        return;
    }
    if metadata.file_type().is_file() {
        if root.file_name().and_then(|file_name| file_name.to_str()) == Some(name) {
            paths.push(root.to_path_buf());
        }
        return;
    }
    if !metadata.file_type().is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if paths.len() >= max_paths || *visited >= max_visited {
            break;
        }
        collect_named_paths(&entry.path(), name, paths, visited, max_paths, max_visited);
    }
}

pub(crate) fn provider_optional_regular_file(path: &Path) -> Result<Option<PathBuf>> {
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
            reason: "provider sidecar paths must be regular files",
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

pub(crate) fn read_provider_json_file(path: &Path, label: &str) -> Result<Value> {
    let raw = read_text_file_limited(path, MAX_PROVIDER_JSONL_LINE_BYTES, label)?;
    let value: Value = serde_json::from_str(&raw)?;
    if !value.is_object() {
        return Err(CaptureError::InvalidPayload(format!(
            "{label} must contain a JSON object"
        )));
    }
    Ok(value)
}

pub(crate) fn append_suffix(path: &Path, suffix: &str) -> Result<PathBuf> {
    let file_name = path
        .file_name()
        .ok_or_else(|| CaptureError::InvalidPath(path.to_path_buf()))?
        .to_string_lossy();
    Ok(path.with_file_name(format!("{file_name}{suffix}")))
}

pub(crate) fn state_path(processing_path: &Path, state_suffix: &str) -> Result<PathBuf> {
    let file_name = processing_path
        .file_name()
        .ok_or_else(|| CaptureError::InvalidPath(processing_path.to_path_buf()))?
        .to_string_lossy();
    let base = file_name
        .strip_suffix(".processing")
        .ok_or_else(|| CaptureError::InvalidPath(processing_path.to_path_buf()))?;
    Ok(processing_path.with_file_name(format!("{base}{state_suffix}")))
}
