#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone)]
pub struct CodexHistoryImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for CodexHistoryImportOptions {
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

#[derive(Clone)]
pub struct CodexSessionImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
    pub max_session_files: Option<usize>,
    pub max_total_bytes: Option<u64>,
    pub tool_output_mode: CodexToolOutputMode,
    pub event_mode: CodexEventImportMode,
    pub include_notices: bool,
    pub fast_event_inserts: bool,
    pub progress: Option<CodexSessionImportProgressCallback>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexEventImportMode {
    Search,
    Rich,
}

impl Default for CodexSessionImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: utc_now(),
            history_record_id: None,
            allow_partial_failures: false,
            max_session_files: None,
            max_total_bytes: None,
            tool_output_mode: CodexToolOutputMode::Full,
            event_mode: CodexEventImportMode::Rich,
            include_notices: true,
            fast_event_inserts: true,
            progress: None,
        }
    }
}

impl std::fmt::Debug for CodexSessionImportOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexSessionImportOptions")
            .field("machine_id", &self.machine_id)
            .field("source_path", &self.source_path)
            .field("imported_at", &self.imported_at)
            .field("history_record_id", &self.history_record_id)
            .field("allow_partial_failures", &self.allow_partial_failures)
            .field("max_session_files", &self.max_session_files)
            .field("max_total_bytes", &self.max_total_bytes)
            .field("tool_output_mode", &self.tool_output_mode)
            .field("event_mode", &self.event_mode)
            .field("include_notices", &self.include_notices)
            .field("fast_event_inserts", &self.fast_event_inserts)
            .field("progress", &self.progress.as_ref().map(|_| "<callback>"))
            .finish()
    }
}

pub type CodexSessionImportProgressCallback =
    Arc<dyn Fn(CodexSessionImportProgress) + Send + Sync + 'static>;

#[derive(Debug, Clone)]
pub struct CodexSessionImportProgress {
    pub source_path: Option<PathBuf>,
    pub total_files: usize,
    pub total_bytes: u64,
    pub completed_files: usize,
    pub completed_bytes: u64,
    pub imported_sessions: usize,
    pub imported_events: usize,
    pub imported_edges: usize,
    pub skipped: usize,
    pub failed: usize,
    pub done: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexToolOutputMode {
    Full,
    Metadata,
    Failures,
    Skip,
}

#[derive(Debug, Clone)]
pub struct CodexSessionCatalogOptions {
    pub source_root: Option<PathBuf>,
    pub cataloged_at: DateTime<Utc>,
    pub allow_partial_failures: bool,
    pub max_session_files: Option<usize>,
    pub max_total_bytes: Option<u64>,
    pub parallelism: Option<usize>,
}

impl Default for CodexSessionCatalogOptions {
    fn default() -> Self {
        Self {
            source_root: None,
            cataloged_at: utc_now(),
            allow_partial_failures: true,
            max_session_files: None,
            max_total_bytes: None,
            parallelism: None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct CodexHistoryLine {
    pub(crate) session_id: String,
    pub(crate) ts: i64,
    pub(crate) text: String,
}

#[derive(Debug, Clone)]
pub(crate) struct CodexSessionHeader {
    pub(crate) id: String,
    pub(crate) timestamp: DateTime<Utc>,
    pub(crate) cwd: Option<String>,
    pub(crate) originator: Option<String>,
    pub(crate) cli_version: Option<String>,
    pub(crate) source: Value,
    pub(crate) parent_session: Option<String>,
    pub(crate) agent_nickname: Option<String>,
    pub(crate) agent_role: Option<String>,
    pub(crate) model_provider: Option<String>,
    pub(crate) raw: Value,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CodexToolCallContext {
    pub(crate) tool_name: String,
    pub(crate) command_preview: Option<String>,
    pub(crate) arguments_preview: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CodexSessionLineCapture {
    pub(crate) event: Option<ProviderEventEnvelope>,
    pub(crate) files_touched: Vec<(usize, ProviderFileTouchedEnvelope)>,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterContext {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub tool_output_mode: CodexToolOutputMode,
    pub event_mode: CodexEventImportMode,
    pub include_notices: bool,
}

impl Default for ProviderAdapterContext {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: utc_now(),
            tool_output_mode: CodexToolOutputMode::Full,
            event_mode: CodexEventImportMode::Rich,
            include_notices: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CodexHistoryJsonlAdapter;

#[derive(Debug, Clone, Copy, Default)]
pub struct CodexSessionJsonlAdapter;

impl ProviderCaptureAdapter for CodexHistoryJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Codex
    }

    fn source_format(&self) -> &str {
        "codex_history_jsonl"
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        ensure_regular_provider_transcript_file(path)?;
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut result = ProviderNormalizationResult::default();
        let mut parsed = Vec::new();
        let mut first_seen = BTreeMap::new();
        let mut line = Vec::new();
        let mut line_number = 0usize;

        while read_provider_jsonl_line(&mut reader, &mut line)? {
            line_number += 1;
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }

            let history: CodexHistoryLine = match serde_json::from_slice(&line) {
                Ok(history) => history,
                Err(err) => {
                    result.summary.failed += 1;
                    result.summary.failures.push(ProviderImportFailure {
                        line: line_number,
                        error: err.to_string(),
                    });
                    continue;
                }
            };
            if history.session_id.trim().is_empty() {
                result.summary.failed += 1;
                result.summary.failures.push(ProviderImportFailure {
                    line: line_number,
                    error: "codex history line has empty session_id".to_owned(),
                });
                continue;
            }
            let Some(occurred_at) = DateTime::from_timestamp(history.ts, 0) else {
                result.summary.failed += 1;
                result.summary.failures.push(ProviderImportFailure {
                    line: line_number,
                    error: format!(
                        "codex history line has invalid unix timestamp {}",
                        history.ts
                    ),
                });
                continue;
            };
            first_seen
                .entry(history.session_id.clone())
                .and_modify(|existing: &mut DateTime<Utc>| {
                    if occurred_at < *existing {
                        *existing = occurred_at;
                    }
                })
                .or_insert(occurred_at);
            parsed.push((line_number, history, occurred_at));
        }

        result.captures = parsed
            .into_iter()
            .map(|(line_number, history, occurred_at)| {
                let started_at = first_seen
                    .get(&history.session_id)
                    .copied()
                    .unwrap_or(occurred_at);
                (
                    line_number,
                    ProviderCaptureEnvelope {
                        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
                        provider: CaptureProvider::Codex,
                        source: ProviderSourceEnvelope {
                            source_format: self.source_format().to_owned(),
                            machine_id: context.machine_id.clone(),
                            observed_at: context.imported_at,
                            raw_source_path: context
                                .source_path
                                .as_ref()
                                .map(|path| path.display().to_string()),
                            raw_retention: ProviderRawRetention::PathReference,
                            redaction_boundary: ProviderRedactionBoundary::BeforeExport,
                            trust: ProviderSourceTrust::ProviderExport,
                            fidelity: Fidelity::SummaryOnly,
                            cursor: Some(ProviderCursorRange {
                                before: None,
                                after: Some(ProviderCursorCheckpoint {
                                    stream: provider_cursor_stream(
                                        CaptureProvider::Codex,
                                        self.source_format(),
                                    ),
                                    cursor: format!("line:{line_number}"),
                                    observed_at: occurred_at,
                                }),
                            }),
                            idempotency_key: Some(format!(
                                "provider-source:{}:{}:{}",
                                CaptureProvider::Codex.as_str(),
                                self.source_format(),
                                history.session_id
                            )),
                            metadata: json!({
                                "adapter": "codex_history_jsonl",
                                "source_fidelity": "prompt_log_only",
                            }),
                        },
                        session: ProviderSessionEnvelope {
                            provider_session_id: history.session_id.clone(),
                            parent_provider_session_id: None,
                            root_provider_session_id: None,
                            external_agent_id: None,
                            agent_type: AgentType::Primary,
                            role_hint: Some("primary".to_owned()),
                            is_primary: true,
                            status: SessionStatus::Imported,
                            started_at,
                            ended_at: None,
                            cwd: None,
                            fidelity: Fidelity::SummaryOnly,
                            idempotency_key: Some(format!(
                                "provider-session:{}:{}",
                                CaptureProvider::Codex.as_str(),
                                history.session_id
                            )),
                            artifacts: Vec::new(),
                            metadata: json!({
                                "source_format": self.source_format(),
                                "source_fidelity": "prompt_log_only",
                                "limitations": [
                                    "user prompts only",
                                    "no assistant responses",
                                    "no tool calls",
                                    "no command output",
                                    "no child session relationships"
                                ],
                            }),
                        },
                        event: Some(ProviderEventEnvelope {
                            provider_event_index: (line_number - 1) as u64,
                            provider_event_hash: None,
                            cursor: Some(format!("line:{line_number}")),
                            event_type: EventType::Message,
                            role: Some(EventRole::User),
                            occurred_at,
                            fidelity: Fidelity::SummaryOnly,
                            redaction_state: RedactionState::LocalPreview,
                            idempotency_key: Some(format!(
                                "provider-event:{}:{}:{}",
                                CaptureProvider::Codex.as_str(),
                                history.session_id,
                                line_number - 1
                            )),
                            artifacts: Vec::new(),
                            payload: json!({
                                "text": history.text,
                                "source_format": self.source_format(),
                            }),
                            metadata: json!({
                                "source": "codex_history",
                                "source_format": self.source_format(),
                                "source_fidelity": "prompt_log_only",
                            }),
                        }),
                    },
                )
            })
            .collect();

        Ok(result)
    }
}

impl ProviderCaptureAdapter for CodexSessionJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Codex
    }

    fn source_format(&self) -> &str {
        "codex_session_jsonl"
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        ensure_regular_provider_transcript_file(path)?;
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut result = ProviderNormalizationResult::default();
        let mut header = None;
        let mut call_contexts: BTreeMap<String, CodexToolCallContext> = BTreeMap::new();
        let raw_source_path = context
            .source_path
            .as_ref()
            .map(|path| path.display().to_string());

        let mut line_number = 0usize;
        let mut line = Vec::new();
        while read_provider_jsonl_line(&mut reader, &mut line)? {
            line_number += 1;
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            if !should_parse_codex_session_line(&line, context.event_mode) {
                continue;
            }
            if should_skip_codex_tool_output_line(&line, context.tool_output_mode) {
                result.summary.skipped += 1;
                result.summary.skipped_events += 1;
                continue;
            }

            let value: Value = match serde_json::from_slice(&line) {
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
            if entry_type == "session_meta" {
                match codex_session_header(value) {
                    Ok(parsed) => {
                        let capture = codex_session_capture(
                            &parsed,
                            None,
                            line_number,
                            parsed.timestamp,
                            context,
                        );
                        call_contexts.clear();
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
                    error: "codex session entry appeared before session_meta".to_owned(),
                });
                continue;
            };
            let occurred_at = match codex_session_line_timestamp(&value, header.timestamp) {
                Ok(occurred_at) => occurred_at,
                Err(err) => {
                    result.summary.failed += 1;
                    result.summary.failures.push(ProviderImportFailure {
                        line: line_number,
                        error: err.to_string(),
                    });
                    continue;
                }
            };
            let mut line_capture = codex_session_line_capture(
                header,
                &value,
                &mut call_contexts,
                CodexSessionLineContext {
                    line_number,
                    occurred_at,
                    tool_output_mode: context.tool_output_mode,
                    event_mode: context.event_mode,
                    raw_source_path: raw_source_path.as_deref(),
                },
            );
            if let Some(event) = line_capture.event.take() {
                if !context.include_notices && event.event_type == EventType::Notice {
                    result.summary.skipped += 1;
                    result.summary.skipped_events += 1;
                } else {
                    result.captures.push((
                        line_number,
                        codex_session_capture(
                            header,
                            Some(event),
                            line_number,
                            occurred_at,
                            context,
                        ),
                    ));
                }
            }
            result.files_touched.append(&mut line_capture.files_touched);
        }

        Ok(result)
    }
}

pub(crate) fn should_parse_codex_session_line(
    line: &[u8],
    event_mode: CodexEventImportMode,
) -> bool {
    if contains_bytes(line, br#""type":"session_meta""#)
        || contains_bytes(line, br#""type":"compacted""#)
    {
        return true;
    }

    if event_mode == CodexEventImportMode::Rich && contains_bytes(line, br#""type":"event_msg""#) {
        return true;
    }

    if !contains_bytes(line, br#""type":"response_item""#) {
        return false;
    }

    if contains_bytes(line, br#""type":"message""#)
        && (contains_bytes(line, br#""role":"user""#)
            || contains_bytes(line, br#""role":"assistant""#))
    {
        return true;
    }

    if codex_session_line_may_touch_file(line) {
        return true;
    }

    event_mode == CodexEventImportMode::Rich
        && (contains_bytes(line, br#""type":"function_call""#)
            || contains_bytes(line, br#""type":"custom_tool_call""#)
            || contains_bytes(line, br#""type":"web_search_call""#)
            || contains_bytes(line, br#""type":"tool_search_call""#)
            || contains_bytes(line, br#""type":"function_call_output""#)
            || contains_bytes(line, br#""type":"custom_tool_call_output""#)
            || contains_bytes(line, br#""type":"tool_search_output""#)
            || contains_bytes(line, br#""type":"reasoning""#))
}

pub(crate) fn codex_session_line_may_touch_file(line: &[u8]) -> bool {
    contains_bytes(line, br#""type":"response_item""#)
        && (contains_bytes(line, b"apply_patch")
            || contains_bytes(line, b"*** Begin Patch")
            || contains_bytes(line, b"write_file")
            || contains_bytes(line, b"edit_file")
            || contains_bytes(line, b"str_replace")
            || contains_bytes(line, b"file_path")
            || contains_bytes(line, b"TargetFile"))
}

pub(crate) fn is_codex_tool_output_line(line: &[u8]) -> bool {
    contains_bytes(line, br#""type":"function_call_output""#)
        || contains_bytes(line, br#""type":"custom_tool_call_output""#)
        || contains_bytes(line, br#""type":"tool_search_output""#)
}

pub(crate) fn should_skip_codex_tool_output_line(line: &[u8], mode: CodexToolOutputMode) -> bool {
    if !is_codex_tool_output_line(line) {
        return false;
    }
    match mode {
        CodexToolOutputMode::Full | CodexToolOutputMode::Metadata => false,
        CodexToolOutputMode::Skip => true,
        CodexToolOutputMode::Failures => !codex_tool_output_line_looks_important(line),
    }
}

pub(crate) fn codex_tool_output_line_looks_important(line: &[u8]) -> bool {
    contains_bytes(line, br#""timed_out":true"#)
        || contains_bytes(line, b"timed_out=true")
        || contains_bytes(line, b"timed out")
        || codex_tool_output_line_has_nonzero_exit_code(line)
}
