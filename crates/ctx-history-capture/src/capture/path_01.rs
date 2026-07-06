#[allow(unused_imports)]
use super::*;

pub trait ProviderCaptureAdapter {
    fn provider(&self) -> CaptureProvider;
    fn source_format(&self) -> &str;
    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult>;
}

impl ProviderCaptureAdapter for ContinueCliSessionsAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Continue
    }

    fn source_format(&self) -> &str {
        CONTINUE_CLI_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_continue_cli_sessions(path, context)
    }
}

pub fn inbox_dir(data_root: impl AsRef<Path>) -> PathBuf {
    core_inbox_dir(data_root.as_ref().to_path_buf())
}

pub(crate) fn collect_jsonl_paths(root: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
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
        if root.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
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
            collect_jsonl_paths(&path, paths)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            ensure_regular_provider_transcript_file(&path)?;
            paths.push(path);
        }
    }
    Ok(())
}

pub(crate) fn ensure_regular_provider_transcript_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "symlinked provider transcript files are rejected",
        });
    }
    if !file_type.is_file() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "provider transcript paths must be regular files",
        });
    }
    ensure_provider_path_parents_are_not_symlinks(path)?;
    Ok(())
}

pub(crate) fn ensure_provider_path_parents_are_not_symlinks(path: &Path) -> Result<()> {
    let parent_count = path.components().count().saturating_sub(1);
    let mut current = PathBuf::new();
    for component in path.components().take(parent_count) {
        current.push(component.as_os_str());
        if current.as_os_str().is_empty() {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&current) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "symlinked provider transcript path components are rejected",
            });
        }
    }
    Ok(())
}

pub(crate) fn read_text_file_limited(path: &Path, max_bytes: usize, label: &str) -> Result<String> {
    let file = File::open(path)?;
    let mut reader = file.take((max_bytes as u64).saturating_add(1));
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(CaptureError::InvalidPayload(format!(
            "{label} exceeds max bytes ({max_bytes})"
        )));
    }
    String::from_utf8(bytes)
        .map_err(|err| CaptureError::InvalidPayload(format!("{label} is not valid UTF-8: {err}")))
}

pub(crate) fn read_json_file_limited(path: &Path, max_bytes: usize, label: &str) -> Result<Value> {
    let text = read_text_file_limited(path, max_bytes, label)?;
    serde_json::from_str(&text).map_err(CaptureError::from)
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FileTouchDraft {
    pub(crate) path: String,
    pub(crate) old_path: Option<String>,
    pub(crate) change_kind: Option<FileChangeKind>,
    pub(crate) confidence: Confidence,
    pub(crate) metadata: Value,
}

pub(crate) fn parse_apply_patch_file_touches(patch: &str) -> Vec<FileTouchDraft> {
    let mut out = Vec::new();
    let mut pending_update: Option<String> = None;
    for line in patch.lines() {
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            flush_pending_patch_update(&mut out, &mut pending_update);
            if let Some(path) = normalize_file_path(path) {
                out.push(file_touch_draft(
                    path,
                    None,
                    FileChangeKind::Created,
                    Confidence::Explicit,
                    "apply_patch_add",
                ));
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            flush_pending_patch_update(&mut out, &mut pending_update);
            pending_update = normalize_file_path(path);
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            flush_pending_patch_update(&mut out, &mut pending_update);
            if let Some(path) = normalize_file_path(path) {
                out.push(file_touch_draft(
                    path,
                    None,
                    FileChangeKind::Deleted,
                    Confidence::Explicit,
                    "apply_patch_delete",
                ));
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Move to: ") {
            let old_path = pending_update.take();
            if let Some(path) = normalize_file_path(path) {
                out.push(file_touch_draft(
                    path,
                    old_path,
                    FileChangeKind::Renamed,
                    Confidence::Explicit,
                    "apply_patch_move",
                ));
            }
        }
    }
    flush_pending_patch_update(&mut out, &mut pending_update);
    out
}

pub(crate) fn flush_pending_patch_update(
    out: &mut Vec<FileTouchDraft>,
    pending_update: &mut Option<String>,
) {
    if let Some(path) = pending_update.take() {
        out.push(file_touch_draft(
            path,
            None,
            FileChangeKind::Modified,
            Confidence::Explicit,
            "apply_patch_update",
        ));
    }
}

pub(crate) fn is_file_path_key(key: &str) -> bool {
    matches!(
        normalized_key(key).as_str(),
        "path"
            | "file"
            | "filepath"
            | "filename"
            | "targetfile"
            | "targetpath"
            | "relativepath"
            | "absolutepath"
            | "uri"
            | "destinationfile"
            | "destinationpath"
    )
}

pub(crate) fn is_old_file_path_key(key: &str) -> bool {
    matches!(
        normalized_key(key).as_str(),
        "oldpath" | "frompath" | "sourcepath" | "originalpath" | "previouspath"
    )
}

pub(crate) fn normalize_file_path(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_matches('"').trim_matches('\'');
    let trimmed = trimmed.strip_prefix("file://").unwrap_or(trimmed);
    if !looks_like_file_path(trimmed) {
        return None;
    }
    Some(trimmed.to_owned())
}

pub(crate) fn looks_like_file_path(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 512
        || value.contains('\n')
        || value.contains('\r')
        || value.contains("://")
        || value.contains("[REDACTED")
        || value.starts_with('{')
        || value.starts_with('[')
    {
        return false;
    }
    value.contains('/')
        || value.contains('\\')
        || value.starts_with('.')
        || value.rsplit(['/', '\\']).next().is_some_and(|name| {
            name.rsplit_once('.').is_some_and(|(stem, ext)| {
                !stem.is_empty()
                    && !ext.is_empty()
                    && ext.len() <= 12
                    && ext.chars().all(|ch| ch.is_ascii_alphanumeric())
            })
        })
}

pub(crate) fn file_touch_draft(
    path: String,
    old_path: Option<String>,
    change_kind: FileChangeKind,
    confidence: Confidence,
    source: &'static str,
) -> FileTouchDraft {
    FileTouchDraft {
        path,
        old_path,
        change_kind: Some(change_kind),
        confidence,
        metadata: json!({ "source": source }),
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
