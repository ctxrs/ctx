#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone)]
pub struct CodeBuddyImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for CodeBuddyImportOptions {
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

#[derive(Debug, Clone, Copy, Default)]
pub struct CodeBuddyHistoryJsonAdapter;

impl ProviderCaptureAdapter for CodeBuddyHistoryJsonAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::CodeBuddy
    }

    fn source_format(&self) -> &str {
        CODEBUDDY_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_codebuddy_history(path, context)
    }
}

pub fn import_codebuddy_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CodeBuddyImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = CodeBuddyHistoryJsonAdapter.normalize_path(
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

pub(crate) const CODEBUDDY_SOURCE_FORMAT: &str = "codebuddy_history_json";

pub(crate) fn normalize_codebuddy_history(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let mut session_dirs = collect_codebuddy_session_dirs(path)?;
    session_dirs.sort();
    session_dirs.dedup();
    if session_dirs.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no CodeBuddy history sessions with index.json and messages/*.json were found",
        });
    }

    let mut merged = ProviderNormalizationResult::default();
    for (session_ordinal, session_dir) in session_dirs.iter().enumerate() {
        let mut result =
            normalize_codebuddy_session_dir(session_dir, context, session_ordinal + 1)?;
        merged.summary.merge(result.summary);
        merged.captures.append(&mut result.captures);
        merged.files_touched.append(&mut result.files_touched);
    }
    Ok(merged)
}

pub(crate) fn collect_codebuddy_session_dirs(path: &Path) -> Result<Vec<PathBuf>> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_file() {
        ensure_regular_provider_transcript_file(path)?;
        if path.file_name().and_then(|name| name.to_str()) == Some("index.json") {
            if let Some(parent) = path.parent() {
                if codebuddy_is_session_dir(parent) {
                    return Ok(vec![parent.to_path_buf()]);
                }
                let mut sessions = Vec::new();
                codebuddy_collect_project_sessions(parent, &mut sessions);
                return Ok(sessions);
            }
        }
        return Ok(Vec::new());
    }
    if !metadata.file_type().is_dir() {
        return Ok(Vec::new());
    }

    if codebuddy_is_session_dir(path) {
        return Ok(vec![path.to_path_buf()]);
    }

    let mut sessions = Vec::new();
    codebuddy_collect_project_sessions(path, &mut sessions);
    if path.file_name().and_then(|name| name.to_str()) == Some("history") {
        codebuddy_collect_history_root_sessions(path, &mut sessions);
    } else {
        for history in collect_codebuddy_history_roots(path, 20_000, 8) {
            codebuddy_collect_history_root_sessions(&history, &mut sessions);
        }
    }
    Ok(sessions)
}

pub(crate) fn codebuddy_is_session_dir(path: &Path) -> bool {
    path.join("index.json").is_file() && path.join("messages").is_dir()
}

pub(crate) fn codebuddy_collect_project_sessions(project_dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(project_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            let candidate = entry.path();
            if codebuddy_is_session_dir(&candidate) {
                out.push(candidate);
            }
        }
    }
}

pub(crate) fn codebuddy_collect_history_root_sessions(history_dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(history_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            codebuddy_collect_project_sessions(&entry.path(), out);
        }
    }
}

pub(crate) fn collect_codebuddy_history_roots(
    root: &Path,
    max_entries: usize,
    max_depth: usize,
) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut visited = 0usize;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > max_depth {
            continue;
        }
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            visited = visited.saturating_add(1);
            if visited > max_entries {
                return roots;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let path = entry.path();
            if path.file_name().and_then(|name| name.to_str()) == Some("history") {
                roots.push(path);
            } else {
                stack.push((path, depth + 1));
            }
        }
    }
    roots
}

pub(crate) fn normalize_codebuddy_session_dir(
    session_dir: &Path,
    context: &ProviderAdapterContext,
    session_ordinal: usize,
) -> Result<ProviderNormalizationResult> {
    let mut result = ProviderNormalizationResult::default();
    let session_index_path = session_dir.join("index.json");
    let session_index = match read_json_file_limited(
        &session_index_path,
        MAX_PROVIDER_JSONL_LINE_BYTES,
        "CodeBuddy session index.json",
    ) {
        Ok(value) => value,
        Err(err) => {
            result.summary.failed += 1;
            result.summary.failures.push(ProviderImportFailure {
                line: session_ordinal,
                error: format!("index.json: {err}"),
            });
            return Ok(result);
        }
    };

    let project_dir = session_dir.parent().unwrap_or(session_dir);
    let project_hash = project_dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("unknown-project");
    let native_session_id = session_dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("unknown-session");
    let provider_session_id = format!("{project_hash}/{native_session_id}");
    let (project_index, conversation) = codebuddy_project_index_and_conversation(
        project_dir,
        native_session_id,
        &mut result,
        session_ordinal,
    );

    let messages = session_index
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if messages.is_empty() {
        return Ok(result);
    }

    let mut events = Vec::new();
    for (message_index, message_ref) in messages.iter().enumerate() {
        let line_number = session_ordinal
            .saturating_mul(10_000)
            .saturating_add(message_index)
            .saturating_add(1);
        let Some(message_id) = message_ref
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
        else {
            result.summary.failed += 1;
            result.summary.failures.push(ProviderImportFailure {
                line: line_number,
                error: "CodeBuddy message ref has empty id".to_owned(),
            });
            continue;
        };
        let message_path = session_dir
            .join("messages")
            .join(format!("{message_id}.json"));
        let raw_message = match read_json_file_limited(
            &message_path,
            MAX_PROVIDER_JSONL_LINE_BYTES,
            "CodeBuddy message JSON",
        ) {
            Ok(value) => value,
            Err(err) => {
                result.summary.failed += 1;
                result.summary.failures.push(ProviderImportFailure {
                    line: line_number,
                    error: format!("messages/{message_id}.json: {err}"),
                });
                continue;
            }
        };
        let decoded_message = codebuddy_decoded_message(&raw_message);
        let text = codebuddy_message_text(&decoded_message, &raw_message);
        if text.trim().is_empty() {
            continue;
        }
        let occurred_at = codebuddy_message_time(
            &raw_message,
            &decoded_message,
            &message_path,
            context.imported_at,
        );
        events.push(CodeBuddyEventInput {
            line_number,
            provider_event_index: message_index as u64,
            native_message_id: message_id.to_owned(),
            role: message_ref
                .get("role")
                .and_then(Value::as_str)
                .or_else(|| raw_message.get("role").and_then(Value::as_str))
                .map(str::to_owned),
            ref_type: message_ref
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_owned),
            occurred_at,
            text,
            raw_message,
            decoded_message,
        });
    }

    if events.is_empty() {
        return Ok(result);
    }

    let first_event_at = events
        .first()
        .map(|event| event.occurred_at)
        .unwrap_or(context.imported_at);
    let last_event_at = events.last().map(|event| event.occurred_at);
    let started_at = conversation
        .as_ref()
        .and_then(|value| task_json_time_field(value, &["createdAt", "created_at", "timestamp"]))
        .unwrap_or(first_event_at);
    let ended_at = conversation
        .as_ref()
        .and_then(|value| {
            task_json_time_field(
                value,
                &["lastMessageAt", "updatedAt", "completedAt", "last_modified"],
            )
        })
        .or(last_event_at);
    let title = conversation
        .as_ref()
        .and_then(|value| task_json_string_field(value, &["name", "title"]))
        .or_else(|| codebuddy_generated_title(&events));
    let source_path = session_dir.display().to_string();
    let file_names = vec!["index.json", "messages/*.json"];

    for event in events {
        let line_number = event.line_number;
        result.captures.push((
            line_number,
            codebuddy_capture(
                &provider_session_id,
                native_session_id,
                project_hash,
                &source_path,
                context,
                started_at,
                ended_at,
                title.clone(),
                project_index.as_ref(),
                conversation.as_ref(),
                &session_index,
                &file_names,
                event,
            ),
        ));
    }

    Ok(result)
}

#[derive(Debug, Clone)]
pub(crate) struct CodeBuddyEventInput {
    pub(crate) line_number: usize,
    pub(crate) provider_event_index: u64,
    pub(crate) native_message_id: String,
    pub(crate) role: Option<String>,
    pub(crate) ref_type: Option<String>,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) text: String,
    pub(crate) raw_message: Value,
    pub(crate) decoded_message: Value,
}

pub(crate) fn codebuddy_project_index_and_conversation(
    project_dir: &Path,
    native_session_id: &str,
    result: &mut ProviderNormalizationResult,
    line: usize,
) -> (Option<Value>, Option<Value>) {
    let path = project_dir.join("index.json");
    if !path.exists() {
        return (None, None);
    }
    let value = match read_json_file_limited(
        &path,
        MAX_PROVIDER_JSONL_LINE_BYTES,
        "CodeBuddy project index.json",
    ) {
        Ok(value) => value,
        Err(err) => {
            result.summary.failed += 1;
            result.summary.failures.push(ProviderImportFailure {
                line,
                error: format!("project index.json: {err}"),
            });
            return (None, None);
        }
    };
    let conversation = value
        .get("conversations")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("id").and_then(Value::as_str) == Some(native_session_id))
        })
        .cloned();
    (Some(value), conversation)
}

pub(crate) fn codebuddy_decoded_message(raw_message: &Value) -> Value {
    match raw_message.get("message") {
        Some(Value::String(text)) => {
            serde_json::from_str(text).unwrap_or_else(|_| json!({ "content": text }))
        }
        Some(value) => value.clone(),
        None => raw_message.clone(),
    }
}

pub(crate) fn codebuddy_message_text(decoded: &Value, raw_message: &Value) -> String {
    let text = decoded
        .get("content")
        .and_then(codebuddy_content_text)
        .or_else(|| {
            decoded
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| decoded.as_str().map(str::to_owned))
        .or_else(|| raw_message.get("content").and_then(codebuddy_content_text))
        .or_else(|| {
            raw_message
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default();
    codebuddy_clean_content(&text)
}

pub(crate) fn codebuddy_content_text(content: &Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return Some(text.to_owned());
    }
    let blocks = content.as_array()?;
    let parts = blocks
        .iter()
        .filter_map(|block| {
            let block_type = block.get("type").and_then(Value::as_str);
            if block_type.is_some_and(|kind| kind != "text") {
                return None;
            }
            block
                .get("text")
                .or_else(|| block.get("content"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

pub(crate) fn codebuddy_clean_content(content: &str) -> String {
    let mut cleaned = content.to_owned();
    for tag in [
        "user_info",
        "project_context",
        "project_layout",
        "system_reminder",
        "additional_data",
        "currently_opened_file",
    ] {
        cleaned = remove_xml_like_block(&cleaned, tag);
    }
    cleaned = cleaned.replace("<user_query>", "");
    cleaned = cleaned.replace("</user_query>", "");
    cleaned.trim().to_owned()
}

pub(crate) fn codebuddy_message_time(
    raw_message: &Value,
    decoded_message: &Value,
    message_path: &Path,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    task_json_time_field(
        raw_message,
        &["createdAt", "created_at", "timestamp", "time", "date"],
    )
    .or_else(|| {
        task_json_time_field(
            decoded_message,
            &["createdAt", "created_at", "timestamp", "time", "date"],
        )
    })
    .or_else(|| {
        fs::metadata(message_path)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .map(DateTime::<Utc>::from)
    })
    .unwrap_or(fallback)
}

pub(crate) fn codebuddy_generated_title(events: &[CodeBuddyEventInput]) -> Option<String> {
    events
        .iter()
        .find(|event| provider_role(event.role.as_deref()) == EventRole::User)
        .map(|event| event.text.replace('\n', " "))
        .map(|title| title.chars().take(50).collect::<String>())
        .filter(|title| !title.trim().is_empty())
}
