use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use uuid::Uuid;
use work_record_core::{
    inbox_dir as core_inbox_dir, new_id, redact_share_safe_markers, AgentType, CaptureEnvelope,
    CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, Confidence,
    EntityTimestamps, Event, EventRole, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderCursorCheckpoint, ProviderCursorRange, ProviderEventEnvelope, ProviderRawRetention,
    ProviderRedactionBoundary, ProviderSessionEnvelope, ProviderSourceEnvelope,
    ProviderSourceTrust, RedactionState, Run, RunStatus, RunType, Session, SessionEdge,
    SessionEdgeType, SessionStatus, SyncCursor, SyncMetadata, SyncState, Visibility, WorkRecord,
    WorkRecordArchive, PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use work_record_store::{CatalogSession, Store, StoreError};

pub const CAPTURE_SCHEMA_VERSION: u32 = 1;
#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("store error: {0}")]
    Store(#[from] work_record_store::StoreError),
    #[error("time parse error: {0}")]
    Time(#[from] chrono::ParseError),
    #[error("uuid parse error: {0}")]
    Uuid(#[from] uuid::Error),
    #[error("unsupported capture envelope schema version: {0}")]
    UnsupportedSchemaVersion(u32),
    #[error("invalid capture payload: {0}")]
    InvalidPayload(String),
    #[error("invalid spool path: {0:?}")]
    InvalidPath(PathBuf),
    #[error("invalid provider transcript path {path:?}: {reason}")]
    InvalidProviderTranscriptPath { path: PathBuf, reason: &'static str },
    #[error("spool writer is already closed")]
    WriterClosed,
    #[error("line {line} in {path:?} is not a valid capture envelope: {source}")]
    InvalidJsonLine {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
}

pub type Result<T> = std::result::Result<T, CaptureError>;

#[derive(Debug)]
pub struct SpoolWriter {
    tmp_path: PathBuf,
    final_path: PathBuf,
    writer: Option<BufWriter<File>>,
}

impl SpoolWriter {
    pub fn create(inbox: impl AsRef<Path>, machine_id: &str) -> Result<Self> {
        let inbox = inbox.as_ref();
        fs::create_dir_all(inbox)?;

        let machine_id = sanitize_filename_component(machine_id);
        let pid = std::process::id();
        let unix_ms = Utc::now().timestamp_millis();
        let random = new_id().simple().to_string();
        let name = format!("capture-{machine_id}-{pid}-{unix_ms}-{random}.jsonl");
        let final_path = inbox.join(name);
        let tmp_path = append_suffix(&final_path, ".tmp")?;
        let file = File::options()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;

        Ok(Self {
            tmp_path,
            final_path,
            writer: Some(BufWriter::new(file)),
        })
    }

    pub fn tmp_path(&self) -> &Path {
        &self.tmp_path
    }

    pub fn final_path(&self) -> &Path {
        &self.final_path
    }

    pub fn write_envelope(&mut self, envelope: &CaptureEnvelope) -> Result<()> {
        let writer = self.writer.as_mut().ok_or(CaptureError::WriterClosed)?;
        serde_json::to_writer(&mut *writer, envelope)?;
        writer.write_all(b"\n")?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<PathBuf> {
        let mut writer = self.writer.take().ok_or(CaptureError::WriterClosed)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        fs::rename(&self.tmp_path, &self.final_path)?;
        Ok(self.final_path)
    }
}

#[derive(Debug, Clone)]
pub struct FixtureOptions {
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub dedupe_key: Option<String>,
    pub machine_id: Option<String>,
    pub cwd: Option<PathBuf>,
    pub occurred_at: DateTime<Utc>,
}

impl Default for FixtureOptions {
    fn default() -> Self {
        Self {
            title: "Fixture capture".to_owned(),
            body: "fixture body".to_owned(),
            tags: vec!["fixture".to_owned()],
            dedupe_key: None,
            machine_id: None,
            cwd: None,
            occurred_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpoolCounts {
    pub pending: usize,
    pub tmp: usize,
    pub processing: usize,
    pub done: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpoolImportFailure {
    pub path: PathBuf,
    pub error: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpoolImportSummary {
    pub processed_files: usize,
    pub skipped_files: usize,
    pub imported_records: usize,
    pub failed_files: usize,
    pub failures: Vec<SpoolImportFailure>,
}

#[derive(Debug, Clone)]
pub struct ProviderFixtureImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub work_record_id: Option<Uuid>,
    pub expected_provider: Option<CaptureProvider>,
    pub allow_partial_failures: bool,
    pub source_format: String,
    pub fidelity: Fidelity,
}

impl Default for ProviderFixtureImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: Utc::now(),
            work_record_id: None,
            expected_provider: None,
            allow_partial_failures: false,
            source_format: "normalized_provider_fixture_jsonl".to_owned(),
            fidelity: Fidelity::Imported,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderImportSummary {
    pub imported: usize,
    pub skipped: usize,
    pub failed: usize,
    pub redacted: usize,
    pub imported_sessions: usize,
    pub skipped_sessions: usize,
    pub imported_events: usize,
    pub skipped_events: usize,
    pub imported_edges: usize,
    pub skipped_edges: usize,
    pub failures: Vec<ProviderImportFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderImportFailure {
    pub line: usize,
    pub error: String,
}

#[derive(Debug, Clone)]
pub struct CodexHistoryImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub work_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for CodexHistoryImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: Utc::now(),
            work_record_id: None,
            allow_partial_failures: false,
        }
    }
}

#[derive(Clone)]
pub struct CodexSessionImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub work_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
    pub max_session_files: Option<usize>,
    pub max_total_bytes: Option<u64>,
    pub tool_output_mode: CodexToolOutputMode,
    pub include_notices: bool,
    pub fast_event_inserts: bool,
    pub progress: Option<CodexSessionImportProgressCallback>,
}

impl Default for CodexSessionImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: Utc::now(),
            work_record_id: None,
            allow_partial_failures: false,
            max_session_files: None,
            max_total_bytes: None,
            tool_output_mode: CodexToolOutputMode::Full,
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
            .field("work_record_id", &self.work_record_id)
            .field("allow_partial_failures", &self.allow_partial_failures)
            .field("max_session_files", &self.max_session_files)
            .field("max_total_bytes", &self.max_total_bytes)
            .field("tool_output_mode", &self.tool_output_mode)
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
            cataloged_at: Utc::now(),
            allow_partial_failures: true,
            max_session_files: None,
            max_total_bytes: None,
            parallelism: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSummary {
    pub source_files: usize,
    pub source_bytes: u64,
    pub cataloged_sessions: usize,
    pub skipped_sessions: usize,
    pub failed_sessions: usize,
}

#[derive(Debug, Clone)]
pub struct PiSessionImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub work_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for PiSessionImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: Utc::now(),
            work_record_id: None,
            allow_partial_failures: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderFixtureLine {
    pub provider: CaptureProvider,
    pub session: ProviderSessionDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<ProviderEventDto>,
}

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
pub struct ProviderEventDto {
    pub provider_event_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_event_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default)]
    pub event_type: EventType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<EventRole>,
    pub occurred_at: DateTime<Utc>,
    #[serde(default = "default_metadata")]
    pub payload: Value,
    #[serde(default = "default_metadata")]
    pub metadata: Value,
}

#[derive(Debug, Deserialize)]
struct CodexHistoryLine {
    session_id: String,
    ts: i64,
    text: String,
}

#[derive(Debug, Clone)]
struct CodexSessionHeader {
    id: String,
    timestamp: DateTime<Utc>,
    cwd: Option<String>,
    originator: Option<String>,
    cli_version: Option<String>,
    source: Value,
    parent_session: Option<String>,
    agent_nickname: Option<String>,
    agent_role: Option<String>,
    model_provider: Option<String>,
    raw: Value,
}

#[derive(Debug, Clone, Default)]
struct CodexToolCallContext {
    tool_name: String,
    command_preview: Option<String>,
    arguments_preview: Option<String>,
}

#[derive(Debug, Clone)]
struct PiSessionHeader {
    id: String,
    version: Option<u64>,
    timestamp: DateTime<Utc>,
    cwd: Option<String>,
    parent_session: Option<String>,
    raw: Value,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterContext {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub tool_output_mode: CodexToolOutputMode,
    pub include_notices: bool,
}

impl Default for ProviderAdapterContext {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: Utc::now(),
            tool_output_mode: CodexToolOutputMode::Full,
            include_notices: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NormalizedProviderImportOptions {
    pub work_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
    pub persist_cursors: bool,
    pub wrap_transaction: bool,
    pub fast_event_inserts: bool,
}

impl Default for NormalizedProviderImportOptions {
    fn default() -> Self {
        Self {
            work_record_id: None,
            allow_partial_failures: false,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProviderNormalizationResult {
    pub summary: ProviderImportSummary,
    pub captures: Vec<(usize, ProviderCaptureEnvelope)>,
}

pub trait ProviderCaptureAdapter {
    fn provider(&self) -> CaptureProvider;
    fn source_format(&self) -> &str;
    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult>;
}

#[derive(Debug, Clone)]
pub struct ProviderFixtureJsonlAdapter {
    pub expected_provider: Option<CaptureProvider>,
    pub source_format: String,
    pub fidelity: Fidelity,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CodexHistoryJsonlAdapter;

#[derive(Debug, Clone, Copy, Default)]
pub struct CodexSessionJsonlAdapter;

#[derive(Debug, Clone, Copy, Default)]
pub struct PiSessionJsonlAdapter;

impl ProviderCaptureAdapter for ProviderFixtureJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        self.expected_provider.unwrap_or(CaptureProvider::Unknown)
    }

    fn source_format(&self) -> &str {
        &self.source_format
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        ensure_regular_provider_transcript_file(path)?;
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut result = ProviderNormalizationResult::default();

        for (index, line) in reader.lines().enumerate() {
            let line_number = index + 1;
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            let fixture: ProviderFixtureLine = match serde_json::from_str(&line) {
                Ok(fixture) => fixture,
                Err(err) => {
                    result.summary.failed += 1;
                    result.summary.failures.push(ProviderImportFailure {
                        line: line_number,
                        error: err.to_string(),
                    });
                    continue;
                }
            };
            if let Some(expected_provider) = self.expected_provider {
                if fixture.provider != expected_provider {
                    result.summary.failed += 1;
                    result.summary.failures.push(ProviderImportFailure {
                        line: line_number,
                        error: format!(
                            "provider fixture line {line_number} has provider `{}` but expected `{}`",
                            fixture.provider.as_str(),
                            expected_provider.as_str()
                        ),
                    });
                    continue;
                }
            }

            result.captures.push((
                line_number,
                fixture_line_to_capture(&fixture, context, &self.source_format, self.fidelity),
            ));
        }

        Ok(result)
    }
}

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
        let reader = BufReader::new(file);
        let mut result = ProviderNormalizationResult::default();
        let mut parsed = Vec::new();
        let mut first_seen = BTreeMap::new();

        for (index, line) in reader.lines().enumerate() {
            let line_number = index + 1;
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            let history: CodexHistoryLine = match serde_json::from_str(&line) {
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
                            redaction_state: RedactionState::SafePreview,
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

        let mut line_number = 0usize;
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = reader.read_until(b'\n', &mut line)?;
            if read == 0 {
                break;
            }
            line_number += 1;
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            if !should_parse_codex_session_line(&line) {
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
            let occurred_at = value
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_rfc3339_utc)
                .unwrap_or(header.timestamp);
            if let Some(event) = codex_session_event(
                &value,
                line_number,
                occurred_at,
                &mut call_contexts,
                context.tool_output_mode,
            ) {
                if !context.include_notices && event.event_type == EventType::Notice {
                    result.summary.skipped += 1;
                    result.summary.skipped_events += 1;
                    continue;
                }
                result.captures.push((
                    line_number,
                    codex_session_capture(header, Some(event), line_number, occurred_at, context),
                ));
            }
        }

        Ok(result)
    }
}

fn should_parse_codex_session_line(line: &[u8]) -> bool {
    if contains_bytes(line, br#""type":"session_meta""#)
        || contains_bytes(line, br#""type":"compacted""#)
        || contains_bytes(line, br#""type":"event_msg""#)
    {
        return true;
    }

    if !contains_bytes(line, br#""type":"response_item""#) {
        return false;
    }

    (contains_bytes(line, br#""type":"message""#)
        && (contains_bytes(line, br#""role":"user""#)
            || contains_bytes(line, br#""role":"assistant""#)))
        || contains_bytes(line, br#""type":"function_call""#)
        || contains_bytes(line, br#""type":"custom_tool_call""#)
        || contains_bytes(line, br#""type":"web_search_call""#)
        || contains_bytes(line, br#""type":"tool_search_call""#)
        || contains_bytes(line, br#""type":"function_call_output""#)
        || contains_bytes(line, br#""type":"custom_tool_call_output""#)
        || contains_bytes(line, br#""type":"tool_search_output""#)
        || contains_bytes(line, br#""type":"reasoning""#)
}

fn is_codex_tool_output_line(line: &[u8]) -> bool {
    contains_bytes(line, br#""type":"function_call_output""#)
        || contains_bytes(line, br#""type":"custom_tool_call_output""#)
        || contains_bytes(line, br#""type":"tool_search_output""#)
}

fn should_skip_codex_tool_output_line(line: &[u8], mode: CodexToolOutputMode) -> bool {
    if !is_codex_tool_output_line(line) {
        return false;
    }
    match mode {
        CodexToolOutputMode::Full | CodexToolOutputMode::Metadata => false,
        CodexToolOutputMode::Skip => true,
        CodexToolOutputMode::Failures => !codex_tool_output_line_looks_important(line),
    }
}

fn codex_tool_output_line_looks_important(line: &[u8]) -> bool {
    contains_bytes(line, br#""timed_out":true"#)
        || contains_bytes(line, b"timed_out=true")
        || contains_bytes(line, b"timed out")
        || codex_tool_output_line_has_nonzero_exit_code(line)
}

fn codex_tool_output_line_has_nonzero_exit_code(line: &[u8]) -> bool {
    let marker = b"Process exited with code ";
    let mut offset = 0usize;
    while let Some(index) = find_bytes(&line[offset..], marker) {
        let code_start = offset + index + marker.len();
        let mut code_end = code_start;
        if line.get(code_end) == Some(&b'-') {
            code_end += 1;
        }
        while line.get(code_end).is_some_and(|byte| byte.is_ascii_digit()) {
            code_end += 1;
        }
        if let Ok(text) = std::str::from_utf8(&line[code_start..code_end]) {
            if text.parse::<i32>().is_ok_and(|code| code != 0) {
                return true;
            }
        }
        offset = code_end.max(offset + index + marker.len());
        if offset >= line.len() {
            break;
        }
    }
    false
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    find_bytes(haystack, needle).is_some()
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
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
        ensure_regular_provider_transcript_file(path)?;
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut result = ProviderNormalizationResult::default();
        let mut header = None;

        for (index, line) in reader.lines().enumerate() {
            let line_number = index + 1;
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            let value: Value = match serde_json::from_str(&line) {
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
                        let capture = pi_session_capture(&parsed, None, line_number, context);
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
            result.captures.push((
                line_number,
                pi_session_capture(header, Some(value), line_number, context),
            ));
        }

        Ok(result)
    }
}

impl ProviderImportSummary {
    fn merge(&mut self, other: ProviderImportSummary) {
        self.imported += other.imported;
        self.skipped += other.skipped;
        self.failed += other.failed;
        self.redacted += other.redacted;
        self.imported_sessions += other.imported_sessions;
        self.skipped_sessions += other.skipped_sessions;
        self.imported_events += other.imported_events;
        self.skipped_events += other.skipped_events;
        self.imported_edges += other.imported_edges;
        self.skipped_edges += other.skipped_edges;
        self.failures.extend(other.failures);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpoolRepairSummary {
    pub retried_files: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ArchiveCounts {
    records: usize,
}

impl ArchiveCounts {
    fn add(&mut self, other: Self) {
        self.records += other.records;
    }
}

pub fn inbox_dir(data_root: impl AsRef<Path>) -> PathBuf {
    core_inbox_dir(data_root.as_ref().to_path_buf())
}

pub fn write_fixture(inbox: impl AsRef<Path>, options: FixtureOptions) -> Result<PathBuf> {
    let envelope = fixture_envelope(options)?;
    let mut writer = SpoolWriter::create(inbox, &envelope.source.machine_id)?;
    writer.write_envelope(&envelope)?;
    writer.finish()
}

pub fn fixture_envelope(options: FixtureOptions) -> Result<CaptureEnvelope> {
    let machine_id = options.machine_id.unwrap_or_else(default_machine_id);
    let cwd_path = match options.cwd {
        Some(path) => path,
        None => env::current_dir()?,
    };
    let cwd = cwd_path.display().to_string();
    let dedupe_key = options
        .dedupe_key
        .unwrap_or_else(|| format!("fixture:{}", new_id()));
    let tags = if options.tags.is_empty() {
        vec!["fixture".to_owned()]
    } else {
        options.tags
    };
    let payload = json!({
        "kind": "work_record",
        "title": options.title,
        "body": options.body,
        "tags": tags,
        "record_kind": "capture-fixture",
        "workspace": cwd,
    });
    let payload_hash = Some(compute_payload_hash(&payload)?);

    Ok(CaptureEnvelope {
        schema_version: CAPTURE_SCHEMA_VERSION,
        capture_event_id: new_id(),
        dedupe_key,
        source: CaptureSourceDescriptor {
            kind: CaptureSourceKind::DirectCli,
            provider: CaptureProvider::Unknown,
            machine_id,
            process_id: Some(std::process::id()),
            cwd: Some(cwd.clone()),
            raw_source_path: None,
            external_session_id: None,
        },
        occurred_at: options.occurred_at,
        cwd: Some(cwd),
        env_session_hints: json!({}),
        payload,
        payload_hash,
        fidelity: Fidelity::Imported,
    })
}

pub fn read_jsonl(path: impl AsRef<Path>) -> Result<Vec<CaptureEnvelope>> {
    let path = path.as_ref();
    ensure_regular_spool_file(path)?;
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut envelopes = Vec::new();

    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let envelope: CaptureEnvelope =
            serde_json::from_str(&line).map_err(|source| CaptureError::InvalidJsonLine {
                path: path.to_path_buf(),
                line: index + 1,
                source,
            })?;
        validate_envelope(&envelope)?;
        envelopes.push(envelope);
    }

    Ok(envelopes)
}

pub fn import_spool(inbox: impl AsRef<Path>, store: &mut Store) -> Result<SpoolImportSummary> {
    let inbox = inbox.as_ref();
    fs::create_dir_all(inbox)?;
    let mut summary = SpoolImportSummary::default();
    let files = pending_spool_files(inbox)?;

    for pending in files {
        let processing = match claim_pending_file(&pending) {
            Ok(path) => path,
            Err(CaptureError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                summary.skipped_files += 1;
                continue;
            }
            Err(err) => return Err(err),
        };

        match import_processing_file(&processing, store) {
            Ok(counts) => {
                let done = state_path(&processing, ".done")?;
                fs::rename(&processing, done)?;
                summary.processed_files += 1;
                summary.imported_records += counts.records;
            }
            Err(err) => {
                let failed = state_path(&processing, ".failed")?;
                fs::rename(&processing, &failed)?;
                write_failure_metadata(&failed, &err)?;
                summary.processed_files += 1;
                summary.failed_files += 1;
                summary.failures.push(SpoolImportFailure {
                    path: failed,
                    error: err.to_string(),
                });
            }
        }
    }

    Ok(summary)
}

pub fn import_provider_fixture_jsonl(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: ProviderFixtureImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = ProviderFixtureJsonlAdapter {
        expected_provider: options.expected_provider,
        source_format: options.source_format.clone(),
        fidelity: options.fidelity,
    }
    .normalize_path(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            imported_at: options.imported_at,
            tool_output_mode: CodexToolOutputMode::Full,
            include_notices: true,
        },
    )?;

    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            work_record_id: options.work_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: false,
        },
    )
}

pub fn import_codex_history_jsonl(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CodexHistoryImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = CodexHistoryJsonlAdapter.normalize_path(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            imported_at: options.imported_at,
            tool_output_mode: CodexToolOutputMode::Full,
            include_notices: true,
        },
    )?;

    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            work_record_id: options.work_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
        },
    )
}

pub fn import_codex_session_jsonl(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    if options.fast_event_inserts {
        return import_codex_session_paths_fast(vec![path.to_path_buf()], store, options, 0);
    }
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = CodexSessionJsonlAdapter.normalize_path(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            imported_at: options.imported_at,
            tool_output_mode: options.tool_output_mode,
            include_notices: options.include_notices,
        },
    )?;

    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            work_record_id: options.work_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: false,
            wrap_transaction: true,
            fast_event_inserts: options.fast_event_inserts,
        },
    )
}

pub fn import_codex_session_paths(
    paths: Vec<PathBuf>,
    store: &mut Store,
    options: CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    for path in &paths {
        ensure_regular_provider_transcript_file(path)?;
    }
    if options.fast_event_inserts {
        return import_codex_session_paths_fast(paths, store, options, 0);
    }

    import_codex_session_paths_normalized(paths, store, options, 0)
}

pub fn import_codex_session_tree(
    root: impl AsRef<Path>,
    store: &mut Store,
    options: CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    let root = root.as_ref();
    let mut paths = Vec::new();
    collect_jsonl_paths(root, &mut paths)?;
    let skipped_by_bounds = apply_codex_session_import_bounds(
        &mut paths,
        options.max_session_files,
        options.max_total_bytes,
    )?;
    if options.fast_event_inserts {
        return import_codex_session_paths_fast(paths, store, options, skipped_by_bounds);
    }

    import_codex_session_paths_normalized(paths, store, options, skipped_by_bounds)
}

fn import_codex_session_paths_normalized(
    paths: Vec<PathBuf>,
    store: &mut Store,
    options: CodexSessionImportOptions,
    skipped_by_bounds: usize,
) -> Result<ProviderImportSummary> {
    let mut merged = ProviderImportSummary::default();
    merged.skipped_sessions += skipped_by_bounds;
    merged.skipped += skipped_by_bounds;
    let mut captures = Vec::new();
    let mut in_transaction = false;
    if !paths.is_empty() {
        store.begin_immediate_batch()?;
        in_transaction = true;
    }
    for path in paths {
        let normalization = match CodexSessionJsonlAdapter.normalize_path(
            &path,
            &ProviderAdapterContext {
                machine_id: options.machine_id.clone(),
                source_path: Some(path.clone()),
                imported_at: options.imported_at,
                tool_output_mode: options.tool_output_mode,
                include_notices: options.include_notices,
            },
        ) {
            Ok(normalization) => normalization,
            Err(err) => {
                if in_transaction {
                    let _ = store.rollback_batch();
                }
                return Err(err);
            }
        };
        merged.merge(normalization.summary);
        captures.extend(normalization.captures);
    }
    let summary = match import_provider_capture_lines(
        store,
        NormalizedProviderImportOptions {
            work_record_id: options.work_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: false,
            wrap_transaction: false,
            fast_event_inserts: options.fast_event_inserts,
        },
        merged,
        captures,
    ) {
        Ok(summary) => summary,
        Err(err) => {
            if in_transaction {
                let _ = store.rollback_batch();
            }
            return Err(err);
        }
    };
    merged = summary;
    if in_transaction {
        store.commit_batch()?;
    }
    Ok(merged)
}

fn import_codex_session_paths_fast(
    paths: Vec<PathBuf>,
    store: &mut Store,
    options: CodexSessionImportOptions,
    skipped_by_bounds: usize,
) -> Result<ProviderImportSummary> {
    let mut summary = ProviderImportSummary::default();
    summary.skipped_sessions += skipped_by_bounds;
    summary.skipped += skipped_by_bounds;
    let mut caches = ProviderImportCaches::default();
    let mut in_transaction = false;
    let mut files_in_transaction = 0usize;
    let total_files = paths.len();
    let total_bytes = codex_session_paths_total_bytes(&paths);
    let mut completed_files = 0usize;
    let mut completed_bytes = 0u64;
    report_codex_import_progress(
        &options,
        total_files,
        total_bytes,
        completed_files,
        completed_bytes,
        &summary,
        false,
    );

    for path in paths {
        let file_bytes = fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if !in_transaction {
            store.begin_immediate_batch()?;
            in_transaction = true;
            files_in_transaction = 0;
        }
        if let Err(err) =
            import_codex_session_path_fast(&path, store, &options, &mut summary, &mut caches)
        {
            if in_transaction {
                let _ = store.rollback_batch();
            }
            return Err(err);
        }
        files_in_transaction += 1;
        if files_in_transaction >= CODEX_FAST_IMPORT_TRANSACTION_FILES {
            if let Err(err) = store.commit_batch() {
                let _ = store.rollback_batch();
                return Err(err.into());
            }
            in_transaction = false;
            store.checkpoint_wal_passive_if_larger_than(
                CODEX_FAST_IMPORT_PASSIVE_CHECKPOINT_MIN_BYTES,
            )?;
        }
        completed_files += 1;
        completed_bytes = completed_bytes.saturating_add(file_bytes);
        report_codex_import_progress(
            &options,
            total_files,
            total_bytes,
            completed_files,
            completed_bytes,
            &summary,
            false,
        );
    }

    if !in_transaction {
        store.begin_immediate_batch()?;
        in_transaction = true;
    }
    if let Err(err) = resolve_pending_provider_edges(store, &mut summary, &mut caches) {
        if in_transaction {
            let _ = store.rollback_batch();
        }
        return Err(err);
    }

    if let Err(err) = store.commit_batch() {
        let _ = store.rollback_batch();
        return Err(err.into());
    }
    store.checkpoint_wal_passive_if_larger_than(CODEX_FAST_IMPORT_PASSIVE_CHECKPOINT_MIN_BYTES)?;
    report_codex_import_progress(
        &options,
        total_files,
        total_bytes,
        completed_files,
        completed_bytes,
        &summary,
        true,
    );
    Ok(summary)
}

fn codex_session_paths_total_bytes(paths: &[PathBuf]) -> u64 {
    paths
        .iter()
        .filter_map(|path| fs::metadata(path).ok())
        .fold(0u64, |total, metadata| total.saturating_add(metadata.len()))
}

fn report_codex_import_progress(
    options: &CodexSessionImportOptions,
    total_files: usize,
    total_bytes: u64,
    completed_files: usize,
    completed_bytes: u64,
    summary: &ProviderImportSummary,
    done: bool,
) {
    let Some(callback) = &options.progress else {
        return;
    };
    callback(CodexSessionImportProgress {
        source_path: options.source_path.clone(),
        total_files,
        total_bytes,
        completed_files,
        completed_bytes,
        imported_sessions: summary.imported_sessions,
        imported_events: summary.imported_events,
        imported_edges: summary.imported_edges,
        skipped: summary.skipped,
        failed: summary.failed,
        done,
    });
}

fn import_codex_session_path_fast(
    path: &Path,
    store: &mut Store,
    options: &CodexSessionImportOptions,
    summary: &mut ProviderImportSummary,
    caches: &mut ProviderImportCaches,
) -> Result<()> {
    ensure_regular_provider_transcript_file(path)?;
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let context = ProviderAdapterContext {
        machine_id: options.machine_id.clone(),
        source_path: Some(path.to_path_buf()),
        imported_at: options.imported_at,
        tool_output_mode: options.tool_output_mode,
        include_notices: options.include_notices,
    };
    let import_options = NormalizedProviderImportOptions {
        work_record_id: options.work_record_id,
        allow_partial_failures: options.allow_partial_failures,
        persist_cursors: false,
        wrap_transaction: false,
        fast_event_inserts: true,
    };

    let mut header = None;
    let mut call_contexts: BTreeMap<String, CodexToolCallContext> = BTreeMap::new();
    let mut line_number = 0usize;
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        line_number += 1;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if !should_parse_codex_session_line(&line) {
            continue;
        }
        if should_skip_codex_tool_output_line(&line, options.tool_output_mode) {
            summary.skipped += 1;
            summary.skipped_events += 1;
            continue;
        }

        let value: Value = match serde_json::from_slice(&line) {
            Ok(value) => value,
            Err(err) => {
                summary.failed += 1;
                summary.failures.push(ProviderImportFailure {
                    line: line_number,
                    error: err.to_string(),
                });
                if !options.allow_partial_failures {
                    return Ok(());
                }
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
                        &context,
                    );
                    let line_summary = import_provider_capture_line(
                        store,
                        &capture,
                        &import_options,
                        line_number,
                        caches,
                    )?;
                    summary.merge(line_summary);
                    call_contexts.clear();
                    header = Some(parsed);
                }
                Err(err) => {
                    summary.failed += 1;
                    summary.failures.push(ProviderImportFailure {
                        line: line_number,
                        error: err.to_string(),
                    });
                    if !options.allow_partial_failures {
                        return Ok(());
                    }
                }
            }
            continue;
        }

        let Some(header) = header.as_ref() else {
            summary.failed += 1;
            summary.failures.push(ProviderImportFailure {
                line: line_number,
                error: "codex session entry appeared before session_meta".to_owned(),
            });
            if !options.allow_partial_failures {
                return Ok(());
            }
            continue;
        };
        let occurred_at = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_utc)
            .unwrap_or(header.timestamp);
        let Some(event) = codex_session_event(
            &value,
            line_number,
            occurred_at,
            &mut call_contexts,
            options.tool_output_mode,
        ) else {
            continue;
        };
        if !options.include_notices && event.event_type == EventType::Notice {
            summary.skipped += 1;
            summary.skipped_events += 1;
            continue;
        }
        let line_summary = import_codex_provider_event_fast(
            store,
            header,
            &event,
            options.work_record_id,
            line_number,
            context.imported_at,
        )?;
        summary.merge(line_summary);
    }
    Ok(())
}

fn import_codex_provider_event_fast(
    store: &mut Store,
    header: &CodexSessionHeader,
    event: &ProviderEventEnvelope,
    work_record_id: Option<Uuid>,
    line_number: usize,
    imported_at: DateTime<Utc>,
) -> Result<ProviderImportSummary> {
    let mut summary = ProviderImportSummary::default();
    let provider = CaptureProvider::Codex;
    let session_id = provider_session_uuid(provider, &header.id);
    let source_id = provider_source_uuid(provider, &header.id);
    let (payload, redacted_payload) = sanitize_value(event.payload.clone());
    let (event_metadata, redacted_metadata) = sanitize_value(event.metadata.clone());
    let event_hash = event
        .provider_event_hash
        .clone()
        .unwrap_or(compute_payload_hash(&payload)?);
    let dedupe_key = Store::provider_event_dedupe_key(
        provider,
        &header.id,
        event.provider_event_index,
        &event_hash,
    );
    let command_run = provider_command_run_from_event(ProviderCommandRunInput {
        provider,
        provider_session_id: &header.id,
        session_id,
        source_id,
        work_record_id,
        event,
        payload: &payload,
        event_hash: &event_hash,
    });
    let normalized_event = Event {
        id: provider_event_uuid(provider, &header.id, event.provider_event_index),
        seq: provider_event_seq(provider, &header.id, event.provider_event_index),
        work_record_id,
        session_id: Some(session_id),
        run_id: command_run.as_ref().map(|run| run.id),
        event_type: event.event_type,
        role: event.role,
        occurred_at: event.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": provider.as_str(),
            "provider_session_id": header.id,
            "provider_event_index": event.provider_event_index,
            "provider_event_hash": event_hash,
            "cursor": event.cursor,
            "artifacts": event.artifacts,
            "body": payload,
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        redaction_state: effective_event_redaction_state(
            event.redaction_state,
            redacted_payload || redacted_metadata,
        ),
        sync: provider_sync_metadata(
            event.fidelity,
            json!({
                "provider_session_id": header.id,
                "provider_event_index": event.provider_event_index,
                "provider_event_hash": event_hash,
                "cursor": event.cursor,
                "source_format": CODEX_SESSION_SOURCE_FORMAT,
                "source_trust": ProviderSourceTrust::ProviderExport,
                "fixture_line": line_number,
                "imported_at": imported_at,
                "event_idempotency_key": event.idempotency_key,
                "metadata": event_metadata,
            }),
        ),
    };

    if let Some(run) = &command_run {
        store.insert_run_if_absent(run)?;
    }
    let inserted = store.insert_event_if_absent(&normalized_event)?;
    if redacted_payload || redacted_metadata {
        summary.redacted += 1;
    }
    if inserted {
        summary.imported_events += 1;
        summary.imported += 1;
    } else {
        summary.skipped_events += 1;
        summary.skipped += 1;
    }
    Ok(summary)
}

pub fn catalog_codex_session_tree(
    root: impl AsRef<Path>,
    store: &Store,
    options: CodexSessionCatalogOptions,
) -> Result<CatalogSummary> {
    let root = root.as_ref();
    let source_root = options
        .source_root
        .as_deref()
        .unwrap_or(root)
        .display()
        .to_string();
    let cataloged_at_ms = options.cataloged_at.timestamp_millis();
    let mut paths = Vec::new();
    collect_jsonl_paths(root, &mut paths)?;
    let skipped_by_bounds = apply_codex_session_import_bounds(
        &mut paths,
        options.max_session_files,
        options.max_total_bytes,
    )?;

    let mut summary = CatalogSummary {
        skipped_sessions: skipped_by_bounds,
        ..CatalogSummary::default()
    };
    let (scan_summary, sessions) = catalog_codex_session_paths(
        paths,
        &source_root,
        cataloged_at_ms,
        options.allow_partial_failures,
        options.parallelism,
    )?;
    summary.source_files += scan_summary.source_files;
    summary.source_bytes = summary
        .source_bytes
        .saturating_add(scan_summary.source_bytes);
    summary.failed_sessions += scan_summary.failed_sessions;
    summary.cataloged_sessions = sessions.len();

    store.begin_immediate_batch()?;
    let persist = (|| -> Result<()> {
        store.mark_catalog_source_stale(CaptureProvider::Codex, &source_root, cataloged_at_ms)?;
        store.upsert_catalog_sessions(&sessions)?;
        Ok(())
    })();
    match persist {
        Ok(()) => {
            store.commit_batch()?;
        }
        Err(err) => {
            let _ = store.rollback_batch();
            return Err(err);
        }
    }
    Ok(summary)
}

#[derive(Debug, Default)]
struct CatalogWorkerBatch {
    summary: CatalogSummary,
    sessions: Vec<CatalogSession>,
    failures: Vec<String>,
}

fn catalog_codex_session_paths(
    paths: Vec<PathBuf>,
    source_root: &str,
    cataloged_at_ms: i64,
    allow_partial_failures: bool,
    requested_parallelism: Option<usize>,
) -> Result<(CatalogSummary, Vec<CatalogSession>)> {
    let parallelism = catalog_parallelism(paths.len(), requested_parallelism);
    let batches = if parallelism <= 1 {
        vec![catalog_codex_session_chunk(
            paths,
            source_root.to_owned(),
            cataloged_at_ms,
        )]
    } else {
        let chunk_size = paths.len().div_ceil(parallelism).max(1);
        thread::scope(|scope| {
            let mut handles = Vec::new();
            for chunk in paths.chunks(chunk_size) {
                let chunk = chunk.to_vec();
                let source_root = source_root.to_owned();
                handles.push(scope.spawn(move || {
                    catalog_codex_session_chunk(chunk, source_root, cataloged_at_ms)
                }));
            }
            let mut batches = Vec::with_capacity(handles.len());
            for handle in handles {
                batches.push(handle.join().unwrap_or_else(|_| {
                    let mut batch = CatalogWorkerBatch::default();
                    batch
                        .failures
                        .push("catalog worker thread panicked".to_owned());
                    batch.summary.failed_sessions += 1;
                    batch
                }));
            }
            batches
        })
    };

    let mut summary = CatalogSummary::default();
    let mut sessions = Vec::new();
    let mut failures = Vec::new();
    for mut batch in batches {
        summary.source_files += batch.summary.source_files;
        summary.source_bytes = summary
            .source_bytes
            .saturating_add(batch.summary.source_bytes);
        summary.failed_sessions += batch.summary.failed_sessions;
        sessions.append(&mut batch.sessions);
        failures.append(&mut batch.failures);
    }
    if !allow_partial_failures && !failures.is_empty() {
        return Err(CaptureError::InvalidPayload(format!(
            "catalog failed: {}",
            failures.remove(0)
        )));
    }
    Ok((summary, sessions))
}

fn catalog_codex_session_chunk(
    paths: Vec<PathBuf>,
    source_root: String,
    cataloged_at_ms: i64,
) -> CatalogWorkerBatch {
    let mut batch = CatalogWorkerBatch::default();
    batch.sessions = Vec::with_capacity(paths.len());
    for path in paths {
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(err) => {
                batch.summary.failed_sessions += 1;
                batch.failures.push(format!("{}: {err}", path.display()));
                continue;
            }
        };
        batch.summary.source_files += 1;
        batch.summary.source_bytes = batch.summary.source_bytes.saturating_add(metadata.len());
        match catalog_codex_session_file(&path, source_root.as_str(), &metadata, cataloged_at_ms) {
            Ok(session) => batch.sessions.push(session),
            Err(err) => {
                batch.summary.failed_sessions += 1;
                batch.failures.push(format!("{}: {err}", path.display()));
            }
        }
    }
    batch
}

fn catalog_parallelism(path_count: usize, requested_parallelism: Option<usize>) -> usize {
    if path_count <= 1 {
        return 1;
    }
    requested_parallelism
        .or_else(|| thread::available_parallelism().ok().map(usize::from))
        .unwrap_or(1)
        .clamp(1, 32)
        .min(path_count)
}

fn catalog_codex_session_file(
    path: &Path,
    source_root: &str,
    metadata: &fs::Metadata,
    cataloged_at_ms: i64,
) -> Result<CatalogSession> {
    let session_meta = read_codex_session_meta(path)?;
    let payload = session_meta.as_ref().and_then(|value| value.get("payload"));
    let source = payload
        .and_then(|payload| payload.get("source"))
        .cloned()
        .unwrap_or(Value::Null);
    let parent_external_session_id = codex_parent_session_id(&source);
    let external_session_id = payload
        .and_then(|payload| payload.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| codex_session_id_from_path(path));
    let session_started_at_ms = payload
        .and_then(|payload| payload.get("timestamp"))
        .and_then(Value::as_str)
        .or_else(|| {
            session_meta
                .as_ref()
                .and_then(|value| value.get("timestamp"))
                .and_then(Value::as_str)
        })
        .and_then(parse_rfc3339_utc)
        .map(|timestamp| timestamp.timestamp_millis());
    let agent_type = if parent_external_session_id.is_some() {
        AgentType::Subagent
    } else {
        AgentType::Primary
    };
    let role_hint = payload
        .and_then(|payload| payload.get("agent_role"))
        .and_then(Value::as_str)
        .filter(|role| !role.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| Some(agent_type.as_str().to_owned()));

    Ok(CatalogSession {
        provider: CaptureProvider::Codex,
        source_format: CODEX_SESSION_SOURCE_FORMAT.to_owned(),
        source_root: source_root.to_owned(),
        source_path: path.display().to_string(),
        external_session_id,
        parent_external_session_id,
        agent_type,
        role_hint,
        external_agent_id: payload
            .and_then(|payload| payload.get("agent_nickname"))
            .and_then(Value::as_str)
            .filter(|agent| !agent.trim().is_empty())
            .map(str::to_owned),
        cwd: payload
            .and_then(|payload| payload.get("cwd"))
            .and_then(Value::as_str)
            .filter(|cwd| !cwd.trim().is_empty())
            .map(str::to_owned),
        session_started_at_ms,
        file_size_bytes: metadata.len(),
        file_modified_at_ms: system_time_ms(metadata.modified().unwrap_or(UNIX_EPOCH)),
        cataloged_at_ms,
        metadata: json!({
            "originator": payload.and_then(|payload| payload.get("originator")).and_then(Value::as_str),
            "cli_version": payload.and_then(|payload| payload.get("cli_version")).and_then(Value::as_str),
            "model_provider": payload.and_then(|payload| payload.get("model_provider")).and_then(Value::as_str),
            "source_kind": codex_source_kind(&source),
            "source": source,
            "catalog_scope": "session_meta",
            "raw_retention": "path_reference",
        }),
    })
}

fn read_codex_session_meta(path: &Path) -> std::io::Result<Option<Value>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(32) {
        let line = line?;
        if !line.as_bytes().contains(&b'{')
            || !contains_bytes(line.as_bytes(), br#""session_meta""#)
        {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) == Some("session_meta") {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn codex_parent_session_id(source: &Value) -> Option<String> {
    source
        .pointer("/subagent/thread_spawn/parent_thread_id")
        .or_else(|| source.pointer("/thread_spawn/parent_thread_id"))
        .or_else(|| source.get("parent_thread_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
}

fn codex_source_kind(source: &Value) -> Option<String> {
    if let Some(value) = source.as_str().filter(|value| !value.trim().is_empty()) {
        return Some(value.to_owned());
    }
    if source.pointer("/subagent/thread_spawn").is_some() {
        return Some("subagent".to_owned());
    }
    if source.pointer("/thread_spawn").is_some() {
        return Some("thread_spawn".to_owned());
    }
    source
        .as_object()
        .and_then(|object| object.keys().next().cloned())
}

fn codex_session_id_from_path(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    if stem.len() >= 36 {
        let tail = &stem[stem.len() - 36..];
        if tail.chars().all(|ch| ch.is_ascii_hexdigit() || ch == '-') {
            return Some(tail.to_owned());
        }
    }
    (!stem.trim().is_empty()).then(|| stem.to_owned())
}

fn system_time_ms(value: SystemTime) -> i64 {
    value
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn apply_codex_session_import_bounds(
    paths: &mut Vec<PathBuf>,
    max_files: Option<usize>,
    max_total_bytes: Option<u64>,
) -> Result<usize> {
    paths.sort();
    if max_files.is_none() && max_total_bytes.is_none() {
        return Ok(0);
    }

    let original_len = paths.len();
    let mut selected = Vec::new();
    let mut total_bytes = 0u64;
    for path in paths.iter().rev() {
        if max_files.is_some_and(|limit| selected.len() >= limit) {
            continue;
        }
        let len = fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if max_total_bytes.is_some_and(|limit| total_bytes.saturating_add(len) > limit) {
            continue;
        }
        total_bytes = total_bytes.saturating_add(len);
        selected.push(path.clone());
    }
    selected.sort();
    let skipped = original_len.saturating_sub(selected.len());
    *paths = selected;
    Ok(skipped)
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
            include_notices: true,
        },
    )?;

    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            work_record_id: options.work_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: false,
        },
    )
}

pub fn import_normalized_provider_captures(
    store: &mut Store,
    normalization: ProviderNormalizationResult,
    options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    import_provider_capture_lines(
        store,
        options,
        normalization.summary,
        normalization.captures,
    )
}

const CODEX_SESSION_SOURCE_FORMAT: &str = "codex_session_jsonl";
const CODEX_MAX_TEXT_CHARS: usize = 16_000;
const CODEX_MAX_METADATA_TEXT_CHARS: usize = 4_000;
const CODEX_MAX_OUTPUT_PREVIEW_CHARS: usize = 4_000;
const CODEX_FAST_IMPORT_TRANSACTION_FILES: usize = 512;
const CODEX_FAST_IMPORT_PASSIVE_CHECKPOINT_MIN_BYTES: u64 = 2 * 1024 * 1024 * 1024;

fn collect_jsonl_paths(root: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "symlinked provider transcript roots are rejected",
        });
    }
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

fn ensure_regular_provider_transcript_file(path: &Path) -> Result<()> {
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
    Ok(())
}

fn parse_rfc3339_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

fn codex_session_header(value: Value) -> Result<CodexSessionHeader> {
    let payload = value
        .get("payload")
        .ok_or_else(|| CaptureError::InvalidPayload("codex session_meta missing payload".into()))?;
    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| CaptureError::InvalidPayload("codex session_meta missing id".into()))?
        .to_owned();
    let timestamp = payload
        .get("timestamp")
        .and_then(Value::as_str)
        .or_else(|| value.get("timestamp").and_then(Value::as_str))
        .and_then(parse_rfc3339_utc)
        .ok_or_else(|| {
            CaptureError::InvalidPayload("codex session_meta missing timestamp".into())
        })?;
    let source = payload.get("source").cloned().unwrap_or(Value::Null);
    let parent_session = source
        .pointer("/subagent/thread_spawn/parent_thread_id")
        .or_else(|| source.pointer("/thread_spawn/parent_thread_id"))
        .or_else(|| source.get("parent_thread_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned);

    Ok(CodexSessionHeader {
        id,
        timestamp,
        cwd: payload
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::to_owned),
        originator: payload
            .get("originator")
            .and_then(Value::as_str)
            .map(str::to_owned),
        cli_version: payload
            .get("cli_version")
            .and_then(Value::as_str)
            .map(str::to_owned),
        source,
        parent_session,
        agent_nickname: payload
            .get("agent_nickname")
            .and_then(Value::as_str)
            .map(str::to_owned),
        agent_role: payload
            .get("agent_role")
            .and_then(Value::as_str)
            .map(str::to_owned),
        model_provider: payload
            .get("model_provider")
            .and_then(Value::as_str)
            .map(str::to_owned),
        raw: value,
    })
}

fn codex_session_capture(
    header: &CodexSessionHeader,
    event: Option<ProviderEventEnvelope>,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let cursor = Some(ProviderCursorRange {
        before: None,
        after: Some(ProviderCursorCheckpoint {
            stream: provider_cursor_stream(CaptureProvider::Codex, CODEX_SESSION_SOURCE_FORMAT),
            cursor: format!("line:{line_number}"),
            observed_at: occurred_at,
        }),
    });
    let is_subagent = header.parent_session.is_some();
    let role_hint = header
        .agent_role
        .clone()
        .or_else(|| is_subagent.then(|| "subagent".to_owned()))
        .or_else(|| Some("primary".to_owned()));

    ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider: CaptureProvider::Codex,
        source: ProviderSourceEnvelope {
            source_format: CODEX_SESSION_SOURCE_FORMAT.to_owned(),
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
            idempotency_key: Some(format!(
                "provider-source:codex:{CODEX_SESSION_SOURCE_FORMAT}:{}",
                header.id
            )),
            metadata: json!({
                "adapter": CODEX_SESSION_SOURCE_FORMAT,
                "source_fidelity": "codex_rollout_jsonl",
            }),
        },
        session: ProviderSessionEnvelope {
            provider_session_id: header.id.clone(),
            parent_provider_session_id: header.parent_session.clone(),
            root_provider_session_id: header.parent_session.clone(),
            external_agent_id: header.agent_nickname.clone(),
            agent_type: if is_subagent {
                AgentType::Subagent
            } else {
                AgentType::Primary
            },
            role_hint,
            is_primary: !is_subagent,
            status: SessionStatus::Imported,
            started_at: header.timestamp,
            ended_at: None,
            cwd: header.cwd.clone(),
            fidelity: Fidelity::Imported,
            idempotency_key: Some(format!("provider-session:codex:{}", header.id)),
            artifacts: Vec::new(),
            metadata: json!({
                "source_format": CODEX_SESSION_SOURCE_FORMAT,
                "source_fidelity": "codex_rollout_jsonl",
                "originator": header.originator,
                "cli_version": header.cli_version,
                "source": header.source,
                "agent_nickname": header.agent_nickname,
                "agent_role": header.agent_role,
                "model_provider": header.model_provider,
                "parent_session": header.parent_session,
                "raw_session_meta_keys": header.raw.as_object().map(|object| object.keys().cloned().collect::<Vec<_>>()),
                "limitations": [
                    "default backfill indexes user and assistant messages, tool call previews, command output previews, reasoning summaries, lifecycle notices, and parent-child session edges where present",
                    "full raw tool arguments, complete command output, encrypted reasoning content, bootstrap context, and binary artifacts remain in the raw transcript referenced by raw_source_path",
                    "previews are capped and redacted before export"
                ],
            }),
        },
        event,
    }
}

fn codex_session_event(
    value: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    call_contexts: &mut BTreeMap<String, CodexToolCallContext>,
    tool_output_mode: CodexToolOutputMode,
) -> Option<ProviderEventEnvelope> {
    let entry_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    match entry_type {
        "response_item" => {
            let payload = value.get("payload")?;
            codex_response_item_event(
                payload,
                line_number,
                occurred_at,
                call_contexts,
                tool_output_mode,
            )
        }
        "compacted" => {
            let text = value
                .get("payload")
                .and_then(codex_json_text)
                .unwrap_or_else(|| "context compacted".to_owned());
            let (text, truncated) = codex_safe_preview(&text, CODEX_MAX_TEXT_CHARS);
            Some(codex_provider_event(
                line_number,
                occurred_at,
                EventType::Summary,
                Some(EventRole::System),
                json!({
                    "entry_type": entry_type,
                    "text": text,
                    "truncated": truncated,
                }),
                json!({
                    "source": "codex_session",
                    "source_format": CODEX_SESSION_SOURCE_FORMAT,
                    "line": line_number,
                    "entry_type": entry_type,
                }),
            ))
        }
        "event_msg" => {
            let payload = value.get("payload")?;
            let msg_type = payload
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            if matches!(
                msg_type,
                "task_started"
                    | "task_complete"
                    | "turn_aborted"
                    | "context_compacted"
                    | "token_count"
                    | "patch_apply_end"
                    | "web_search_end"
            ) {
                let body = codex_lifecycle_body(payload, msg_type);
                Some(codex_provider_event(
                    line_number,
                    occurred_at,
                    EventType::Notice,
                    Some(EventRole::System),
                    json!({
                        "entry_type": entry_type,
                        "event_msg_type": msg_type,
                        "body": body,
                    }),
                    json!({
                        "source": "codex_session",
                        "source_format": CODEX_SESSION_SOURCE_FORMAT,
                        "line": line_number,
                        "entry_type": entry_type,
                        "event_msg_type": msg_type,
                    }),
                ))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn codex_response_item_event(
    payload: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    call_contexts: &mut BTreeMap<String, CodexToolCallContext>,
    tool_output_mode: CodexToolOutputMode,
) -> Option<ProviderEventEnvelope> {
    let item_type = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    match item_type {
        "message" => codex_message_event(payload, line_number, occurred_at),
        "function_call" | "custom_tool_call" | "web_search_call" | "tool_search_call" => {
            codex_tool_call_event(payload, line_number, occurred_at, call_contexts)
        }
        "function_call_output" | "custom_tool_call_output" | "tool_search_output" => {
            codex_tool_output_event(
                payload,
                line_number,
                occurred_at,
                call_contexts,
                tool_output_mode,
            )
        }
        "reasoning" => codex_reasoning_event(payload, line_number, occurred_at),
        _ => Some(codex_provider_event(
            line_number,
            occurred_at,
            EventType::Notice,
            None,
            json!({
                "item_type": item_type,
                "body": codex_capped_json(payload, CODEX_MAX_METADATA_TEXT_CHARS),
            }),
            json!({
                "source": "codex_session",
                "source_format": CODEX_SESSION_SOURCE_FORMAT,
                "line": line_number,
                "item_type": item_type,
            }),
        )),
    }
}

fn codex_tool_call_event(
    payload: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    call_contexts: &mut BTreeMap<String, CodexToolCallContext>,
) -> Option<ProviderEventEnvelope> {
    let item_type = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("tool_call");
    let tool_name = codex_tool_name(payload, item_type);
    let call_id = payload.get("call_id").and_then(Value::as_str);
    let argument_value = payload
        .get("arguments")
        .or_else(|| payload.get("input"))
        .or_else(|| payload.get("action"))
        .or_else(|| payload.get("execution"));
    let command_preview = codex_command_preview(&tool_name, argument_value);
    let (arguments_preview, arguments_truncated) = argument_value
        .map(|value| codex_value_preview(value, CODEX_MAX_METADATA_TEXT_CHARS))
        .unwrap_or_else(|| (String::new(), false));
    let text = command_preview
        .as_ref()
        .map(|command| format!("{tool_name}: {command}"))
        .unwrap_or_else(|| {
            if arguments_preview.is_empty() {
                format!("{tool_name} tool call")
            } else {
                format!("{tool_name}: {arguments_preview}")
            }
        });
    let (text, text_truncated) = codex_safe_preview(&text, CODEX_MAX_METADATA_TEXT_CHARS);

    if let Some(call_id) = call_id {
        call_contexts.insert(
            call_id.to_owned(),
            CodexToolCallContext {
                tool_name: tool_name.clone(),
                command_preview: command_preview.clone(),
                arguments_preview: (!arguments_preview.is_empty())
                    .then_some(arguments_preview.clone()),
            },
        );
    }

    Some(codex_provider_event(
        line_number,
        occurred_at,
        EventType::ToolCall,
        Some(EventRole::Assistant),
        json!({
            "item_type": item_type,
            "tool": tool_name,
            "name": tool_name,
            "call_id": call_id,
            "command": command_preview,
            "arguments_preview": arguments_preview,
            "arguments_truncated": arguments_truncated,
            "text": text,
            "truncated": text_truncated || arguments_truncated,
        }),
        json!({
            "source": "codex_session",
            "source_format": CODEX_SESSION_SOURCE_FORMAT,
            "line": line_number,
            "item_type": item_type,
            "tool": tool_name,
        }),
    ))
}

fn codex_tool_output_event(
    payload: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    call_contexts: &BTreeMap<String, CodexToolCallContext>,
    tool_output_mode: CodexToolOutputMode,
) -> Option<ProviderEventEnvelope> {
    if tool_output_mode == CodexToolOutputMode::Skip {
        return None;
    }
    let item_type = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("tool_output");
    let call_id = payload.get("call_id").and_then(Value::as_str);
    let context = call_id.and_then(|call_id| call_contexts.get(call_id));
    let tool_name = context
        .map(|context| context.tool_name.clone())
        .unwrap_or_else(|| codex_tool_name(payload, item_type));
    let output_value = payload
        .get("output")
        .or_else(|| payload.get("tools"))
        .or_else(|| payload.get("result"));
    let output_text = output_value.map(codex_output_text);
    let command_preview = context.and_then(|context| context.command_preview.clone());
    let output_text_ref = output_text.as_deref();
    let exit_code = output_text_ref.and_then(codex_exit_code);
    let duration_ms = output_text_ref.and_then(codex_wall_time_ms);
    let output_bytes = output_text_ref.map(str::len).unwrap_or(0);
    let timed_out = codex_timed_out(payload).unwrap_or(false);
    if tool_output_mode == CodexToolOutputMode::Failures
        && !timed_out
        && !exit_code.is_some_and(|code| code != 0)
    {
        return None;
    }
    let event_type = if codex_is_command_tool(&tool_name) {
        EventType::CommandOutput
    } else {
        EventType::ToolOutput
    };
    let keep_preview = tool_output_mode == CodexToolOutputMode::Full
        || timed_out
        || exit_code.is_some_and(|code| code != 0);
    let preview_limit = if tool_output_mode == CodexToolOutputMode::Full {
        CODEX_MAX_OUTPUT_PREVIEW_CHARS
    } else {
        512
    };
    let (output_preview, output_truncated) = if keep_preview {
        output_text_ref
            .map(|text| codex_safe_preview(text, preview_limit))
            .unwrap_or_else(|| (String::new(), false))
    } else {
        (String::new(), output_bytes > 0)
    };
    let text = match tool_output_mode {
        CodexToolOutputMode::Full => {
            if let Some(command) = command_preview.as_deref() {
                format!("{tool_name} output for `{command}`: {output_preview}")
            } else {
                format!("{tool_name} output: {output_preview}")
            }
        }
        CodexToolOutputMode::Metadata
        | CodexToolOutputMode::Failures
        | CodexToolOutputMode::Skip => {
            let command = command_preview
                .as_deref()
                .map(|command| format!(" for `{command}`"))
                .unwrap_or_default();
            let status = exit_code
                .map(|code| format!("exit_code={code}"))
                .unwrap_or_else(|| "exit_code=unknown".to_owned());
            let duration = duration_ms
                .map(|ms| format!(", duration_ms={ms}"))
                .unwrap_or_default();
            let timeout = if timed_out { ", timed_out=true" } else { "" };
            let preview = if output_preview.is_empty() {
                String::new()
            } else {
                format!(": {output_preview}")
            };
            format!("{tool_name} output{command}: {status}{duration}, output_bytes={output_bytes}{timeout}{preview}")
        }
    };
    let (text, text_truncated) = codex_safe_preview(&text, CODEX_MAX_OUTPUT_PREVIEW_CHARS);

    Some(codex_provider_event(
        line_number,
        occurred_at,
        event_type,
        Some(EventRole::Tool),
        json!({
            "item_type": item_type,
            "tool": tool_name,
            "name": tool_name,
            "call_id": call_id,
            "command": command_preview,
            "arguments_preview": context.and_then(|context| context.arguments_preview.clone()),
            "output": if tool_output_mode == CodexToolOutputMode::Full { Some(output_preview.clone()) } else { None },
            "output_preview": output_preview,
            "output_retention": if tool_output_mode == CodexToolOutputMode::Full { "preview" } else { "raw_transcript" },
            "output_bytes": output_bytes,
            "output_truncated": output_truncated,
            "exit_code": exit_code,
            "duration_ms": duration_ms,
            "timed_out": timed_out,
            "text": text,
            "truncated": text_truncated || output_truncated,
        }),
        json!({
            "source": "codex_session",
            "source_format": CODEX_SESSION_SOURCE_FORMAT,
            "line": line_number,
            "item_type": item_type,
            "tool": tool_name,
        }),
    ))
}

fn codex_output_text(value: &Value) -> Cow<'_, str> {
    match value {
        Value::String(text) => Cow::Borrowed(text),
        Value::Null => Cow::Borrowed(""),
        other => Cow::Owned(serde_json::to_string(other).unwrap_or_else(|_| other.to_string())),
    }
}

fn codex_reasoning_event(
    payload: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
) -> Option<ProviderEventEnvelope> {
    let summary = payload
        .get("summary")
        .and_then(codex_content_text)
        .or_else(|| {
            payload
                .get("summary_text")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })?;
    let (summary, truncated) = codex_safe_preview(&summary, CODEX_MAX_TEXT_CHARS);
    Some(codex_provider_event(
        line_number,
        occurred_at,
        EventType::Summary,
        Some(EventRole::Assistant),
        json!({
            "item_type": "reasoning",
            "summary": summary,
            "text": summary,
            "truncated": truncated,
            "encrypted_content_withheld": payload.get("encrypted_content").is_some(),
        }),
        json!({
            "source": "codex_session",
            "source_format": CODEX_SESSION_SOURCE_FORMAT,
            "line": line_number,
            "item_type": "reasoning",
        }),
    ))
}

fn codex_message_event(
    payload: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
) -> Option<ProviderEventEnvelope> {
    let role_text = payload
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if matches!(role_text, "developer" | "system") {
        return None;
    }
    let text = payload.get("content").and_then(codex_content_text)?;
    let (text, truncated) = capped_text(&text, CODEX_MAX_TEXT_CHARS);
    Some(codex_provider_event(
        line_number,
        occurred_at,
        EventType::Message,
        Some(codex_event_role(role_text)),
        json!({
            "item_type": "message",
            "message_role": role_text,
            "phase": payload.get("phase").and_then(Value::as_str),
            "text": text,
            "truncated": truncated,
        }),
        json!({
            "source": "codex_session",
            "source_format": CODEX_SESSION_SOURCE_FORMAT,
            "import_scope": "fast_transcript_index",
            "line": line_number,
            "item_type": "message",
            "message_role": role_text,
        }),
    ))
}

fn codex_provider_event(
    line_number: usize,
    occurred_at: DateTime<Utc>,
    event_type: EventType,
    role: Option<EventRole>,
    payload: Value,
    metadata: Value,
) -> ProviderEventEnvelope {
    ProviderEventEnvelope {
        provider_event_index: (line_number - 1) as u64,
        provider_event_hash: None,
        cursor: Some(format!("line:{line_number}")),
        event_type,
        role,
        occurred_at,
        fidelity: Fidelity::Imported,
        redaction_state: RedactionState::SafePreview,
        idempotency_key: Some(format!("provider-event:codex-session:{line_number}")),
        artifacts: Vec::new(),
        payload,
        metadata,
    }
}

fn codex_lifecycle_body(payload: &Value, msg_type: &str) -> Value {
    let preview = payload
        .get("last_agent_message")
        .or_else(|| payload.get("message"))
        .or_else(|| payload.get("stdout"))
        .or_else(|| payload.get("stderr"))
        .and_then(codex_json_text)
        .unwrap_or_else(|| format!("Codex lifecycle: {msg_type}"));
    let (text, truncated) = codex_safe_preview(&preview, CODEX_MAX_METADATA_TEXT_CHARS);
    json!({
        "text": text,
        "event_msg_type": msg_type,
        "status": payload.get("status").and_then(Value::as_str),
        "success": payload.get("success").and_then(Value::as_bool),
        "duration_ms": payload.get("duration_ms").and_then(Value::as_i64),
        "time_to_first_token_ms": payload.get("time_to_first_token_ms").and_then(Value::as_i64),
        "truncated": truncated,
    })
}

fn codex_tool_name(payload: &Value, item_type: &str) -> String {
    payload
        .get("name")
        .or_else(|| payload.get("tool"))
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(item_type)
        .to_owned()
}

fn codex_is_command_tool(tool_name: &str) -> bool {
    matches!(tool_name, "exec_command" | "shell" | "bash" | "command")
}

fn codex_command_preview(tool_name: &str, argument_value: Option<&Value>) -> Option<String> {
    if !codex_is_command_tool(tool_name) {
        return None;
    }
    let value = argument_value?;
    let parsed = codex_parse_embedded_json(value).unwrap_or_else(|| value.clone());
    let command = parsed
        .get("cmd")
        .or_else(|| parsed.get("command"))
        .or_else(|| parsed.get("shell_command"))
        .and_then(Value::as_str)
        .or_else(|| value.as_str())?;
    Some(codex_safe_preview(command, CODEX_MAX_METADATA_TEXT_CHARS).0)
}

fn codex_value_preview(value: &Value, max_chars: usize) -> (String, bool) {
    let rendered = match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
    };
    codex_safe_preview(&rendered, max_chars)
}

fn codex_safe_preview(value: &str, max_chars: usize) -> (String, bool) {
    let redacted = redact_share_safe_markers(value);
    capped_text(&redacted, max_chars)
}

fn codex_parse_embedded_json(value: &Value) -> Option<Value> {
    match value {
        Value::String(text) => serde_json::from_str::<Value>(text).ok(),
        Value::Object(_) | Value::Array(_) => Some(value.clone()),
        _ => None,
    }
}

fn codex_timed_out(payload: &Value) -> Option<bool> {
    payload
        .get("timed_out")
        .and_then(Value::as_bool)
        .or_else(|| {
            payload
                .get("output")
                .and_then(codex_parse_embedded_json)
                .and_then(|value| {
                    value
                        .get("timed_out")
                        .and_then(Value::as_bool)
                        .or_else(|| value.pointer("/status/timed_out").and_then(Value::as_bool))
                })
        })
}

fn codex_exit_code(text: &str) -> Option<i32> {
    let marker = "Process exited with code ";
    let index = text.find(marker)? + marker.len();
    let tail = &text[index..];
    let digits = tail
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '-')
        .collect::<String>();
    digits.parse().ok()
}

fn codex_wall_time_ms(text: &str) -> Option<i64> {
    let marker = "Wall time: ";
    let index = text.find(marker)? + marker.len();
    let tail = &text[index..];
    let seconds_text = tail
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
        .collect::<String>();
    let seconds = seconds_text.parse::<f64>().ok()?;
    Some((seconds * 1000.0).round() as i64)
}

fn codex_event_role(role: &str) -> EventRole {
    match role {
        "user" => EventRole::User,
        "assistant" => EventRole::Assistant,
        "tool" => EventRole::Tool,
        "system" | "developer" => EventRole::System,
        _ => EventRole::Unknown,
    }
}

fn codex_content_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                if let Some(text) = block
                    .get("text")
                    .or_else(|| block.get("input_text"))
                    .or_else(|| block.get("output_text"))
                    .or_else(|| block.get("summary_text"))
                    .and_then(Value::as_str)
                {
                    parts.push(text.to_owned());
                    continue;
                }
                if let Some(text) = block.get("content").and_then(Value::as_str) {
                    parts.push(text.to_owned());
                    continue;
                }
                if let Some(kind) = block.get("type").and_then(Value::as_str) {
                    if matches!(kind, "tool_call" | "function_call" | "custom_tool_call") {
                        let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                        parts.push(format!("tool call: {name}"));
                    }
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        Value::Object(_) => codex_json_text(value),
        _ => None,
    }
}

fn codex_json_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value).ok(),
        _ => Some(value.to_string()),
    }
}

fn codex_capped_json(value: &Value, max_chars: usize) -> Value {
    match value {
        Value::String(text) => {
            let (text, truncated) = capped_text(text, max_chars);
            json!({ "text": text, "truncated": truncated })
        }
        _ => {
            let rendered = serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned());
            let (text, truncated) = capped_text(&rendered, max_chars);
            json!({ "json": text, "truncated": truncated })
        }
    }
}

fn capped_text(value: &str, max_chars: usize) -> (String, bool) {
    let mut out = String::new();
    let mut truncated = false;
    for (index, ch) in value.chars().enumerate() {
        if index >= max_chars {
            truncated = true;
            break;
        }
        out.push(ch);
    }
    (out, truncated)
}

fn pi_session_header(value: Value) -> Result<PiSessionHeader> {
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

fn pi_session_capture(
    header: &PiSessionHeader,
    entry: Option<Value>,
    line_number: usize,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let event = entry.map(|entry| pi_session_event(&entry, line_number));
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

    ProviderCaptureEnvelope {
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
    }
}

fn pi_session_event(entry: &Value, line_number: usize) -> ProviderEventEnvelope {
    let entry_type = entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let message = entry.get("message");
    let message_role = message
        .and_then(|message| message.get("role"))
        .and_then(Value::as_str);
    let occurred_at = entry
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|timestamp| DateTime::parse_from_rfc3339(timestamp).ok())
        .map(|time| time.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);
    let event_type = pi_event_type(entry_type, message);
    let role = message_role.map(pi_event_role);
    let text = message.and_then(pi_message_text);

    ProviderEventEnvelope {
        provider_event_index: (line_number - 1) as u64,
        provider_event_hash: None,
        cursor: entry.get("id").and_then(Value::as_str).map(str::to_owned),
        event_type,
        role,
        occurred_at,
        fidelity: Fidelity::Imported,
        redaction_state: RedactionState::SafePreview,
        idempotency_key: Some(format!("provider-event:pi:{line_number}")),
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
            "message_role": message_role,
            "model": message
                .and_then(|message| message.get("model"))
                .and_then(Value::as_str),
            "provider": message
                .and_then(|message| message.get("provider"))
                .and_then(Value::as_str),
            "usage": message.and_then(|message| message.get("usage")).cloned(),
        }),
    }
}

fn pi_event_type(entry_type: &str, message: Option<&Value>) -> EventType {
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

fn pi_event_role(role: &str) -> EventRole {
    match role {
        "user" => EventRole::User,
        "assistant" => EventRole::Assistant,
        "toolResult" | "bashExecution" => EventRole::Tool,
        "system" => EventRole::System,
        _ => EventRole::Unknown,
    }
}

fn pi_message_has_tool_call(message: &Value) -> bool {
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

fn pi_message_text(message: &Value) -> Option<String> {
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
    let content = message.get("content")?;
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

fn import_provider_capture_lines(
    store: &mut Store,
    options: NormalizedProviderImportOptions,
    mut summary: ProviderImportSummary,
    captures: Vec<(usize, ProviderCaptureEnvelope)>,
) -> Result<ProviderImportSummary> {
    let mut caches = ProviderImportCaches::default();
    let has_captures = !captures.is_empty();

    if summary.failed > 0 && !options.allow_partial_failures {
        return Ok(summary);
    }

    if has_captures && options.wrap_transaction {
        store.begin_immediate_batch()?;
    }
    for (line_number, capture) in captures {
        match import_provider_capture_line(store, &capture, &options, line_number, &mut caches) {
            Ok(line_summary) => summary.merge(line_summary),
            Err(err) => {
                summary.failed += 1;
                summary.failures.push(ProviderImportFailure {
                    line: line_number,
                    error: err.to_string(),
                });
            }
        }
    }
    resolve_pending_provider_edges(store, &mut summary, &mut caches)?;
    if has_captures && options.wrap_transaction {
        if let Err(err) = store.commit_batch() {
            let _ = store.rollback_batch();
            return Err(err.into());
        }
    }

    Ok(summary)
}

#[derive(Default)]
struct ProviderImportCaches {
    imported_sessions: BTreeSet<Uuid>,
    processed_sources: BTreeSet<Uuid>,
    processed_sessions: BTreeSet<Uuid>,
    imported_edges: BTreeSet<Uuid>,
    processed_edges: BTreeSet<Uuid>,
    session_exists: BTreeMap<Uuid, bool>,
    pending_edges: BTreeMap<Uuid, PendingProviderEdge>,
}

#[derive(Clone)]
struct PendingProviderEdge {
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
    session_id: Uuid,
    parent_session_id: Uuid,
    root_session_id: Option<Uuid>,
    source_id: Uuid,
    source_format: String,
    imported_at: DateTime<Utc>,
    fidelity: Fidelity,
    line_number: usize,
}

fn import_provider_capture_line(
    store: &mut Store,
    capture: &ProviderCaptureEnvelope,
    options: &NormalizedProviderImportOptions,
    line_number: usize,
    caches: &mut ProviderImportCaches,
) -> Result<ProviderImportSummary> {
    if capture.schema_version != PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION {
        return Err(CaptureError::InvalidPayload(format!(
            "unsupported provider capture envelope schema version {} on line {line_number}",
            capture.schema_version
        )));
    }

    let mut summary = ProviderImportSummary::default();
    let provider = capture.provider;
    let session = &capture.session;
    let source = &capture.source;
    let imported_at = source.observed_at;
    let session_id = provider_session_uuid(provider, &session.provider_session_id);
    let source_id = provider_source_uuid(provider, &session.provider_session_id);
    let requested_parent_session_id = session
        .parent_provider_session_id
        .as_ref()
        .map(|id| provider_session_uuid(provider, id));
    let parent_session_id = match requested_parent_session_id {
        Some(parent_id)
            if provider_session_exists_cached(store, parent_id, &mut caches.session_exists)? =>
        {
            Some(parent_id)
        }
        _ => None,
    };
    let requested_root_session_id = session
        .root_provider_session_id
        .as_ref()
        .map(|id| provider_session_uuid(provider, id))
        .or_else(|| requested_parent_session_id.map(|_| session_id));
    let root_session_id = match requested_root_session_id {
        Some(root_id)
            if root_id == session_id
                || provider_session_exists_cached(store, root_id, &mut caches.session_exists)? =>
        {
            Some(root_id)
        }
        _ => None,
    };
    let (source_metadata, redacted_source_metadata) = sanitize_value(source.metadata.clone());
    let (session_metadata, redacted_session_metadata) = sanitize_value(session.metadata.clone());

    let source_record = CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider,
            machine_id: source.machine_id.clone(),
            process_id: None,
            cwd: session.cwd.clone(),
            raw_source_path: source.raw_source_path.clone(),
            external_session_id: Some(session.provider_session_id.clone()),
        },
        started_at: session.started_at,
        ended_at: session.ended_at,
        sync: provider_sync_metadata(
            source.fidelity,
            json!({
                "provider_session_id": session.provider_session_id,
                "source_format": source.source_format,
                "source_trust": source.trust,
                "raw_retention": source.raw_retention,
                "redaction_boundary": source.redaction_boundary,
                "cursor": source.cursor,
                "fixture_line": line_number,
                "imported_at": imported_at,
                "source_idempotency_key": source.idempotency_key,
                "source_metadata": source_metadata,
                "session_metadata": session_metadata,
            }),
        ),
    };
    if caches.processed_sources.insert(source_id) {
        store.upsert_capture_source(&source_record)?;
        if redacted_source_metadata {
            summary.redacted += 1;
        }
    }

    let process_session = caches.processed_sessions.insert(session_id);
    let is_new_session = if process_session {
        !provider_session_exists_cached(store, session_id, &mut caches.session_exists)?
    } else {
        false
    };
    let normalized_session = Session {
        id: session_id,
        work_record_id: options.work_record_id,
        parent_session_id,
        root_session_id,
        capture_source_id: Some(source_id),
        provider,
        external_session_id: Some(session.provider_session_id.clone()),
        external_agent_id: session.external_agent_id.clone(),
        agent_type: session.agent_type,
        role_hint: session.role_hint.clone(),
        is_primary: session.is_primary,
        status: session.status,
        transcript_blob_id: None,
        started_at: session.started_at,
        ended_at: session.ended_at,
        timestamps: timestamps(imported_at),
        sync: provider_sync_metadata(
            session.fidelity,
            json!({
                "provider_session_id": session.provider_session_id,
                "parent_provider_session_id": session.parent_provider_session_id,
                "root_provider_session_id": session.root_provider_session_id,
                "source_format": source.source_format,
                "source_trust": source.trust,
                "fixture_line": line_number,
                "imported_at": imported_at,
                "session_idempotency_key": session.idempotency_key,
                "artifacts": session.artifacts,
                "metadata": session_metadata,
            }),
        ),
    };
    if process_session {
        store.upsert_session(&normalized_session)?;
        caches.session_exists.insert(session_id, true);
        if redacted_session_metadata {
            summary.redacted += 1;
        }
        if is_new_session && caches.imported_sessions.insert(session_id) {
            summary.imported_sessions += 1;
            summary.imported += 1;
        } else {
            summary.skipped_sessions += 1;
            summary.skipped += 1;
        }
    }

    if let Some(parent_id) = parent_session_id {
        let edge_id = provider_edge_uuid(provider, &session.provider_session_id, "parent_child");
        if caches.processed_edges.insert(edge_id) {
            let was_present = store.session_edge_exists(edge_id)?;
            let edge = SessionEdge {
                id: edge_id,
                from_session_id: parent_id,
                to_session_id: session_id,
                edge_type: SessionEdgeType::ParentChild,
                confidence: Confidence::Explicit,
                source_id: Some(source_id),
                timestamps: timestamps(imported_at),
                sync: provider_sync_metadata(
                    session.fidelity,
                    json!({
                        "provider_session_id": session.provider_session_id,
                        "parent_provider_session_id": session.parent_provider_session_id,
                        "source_format": source.source_format,
                        "fixture_line": line_number,
                        "imported_at": imported_at,
                    }),
                ),
            };
            store.upsert_session_edge(&edge)?;
            if !was_present && caches.imported_edges.insert(edge_id) {
                summary.imported_edges += 1;
                summary.imported += 1;
            } else {
                summary.skipped_edges += 1;
                summary.skipped += 1;
            }
        }
    } else if requested_parent_session_id.is_some() {
        let edge_id = provider_edge_uuid(provider, &session.provider_session_id, "parent_child");
        if let Some(parent_session_id) = requested_parent_session_id {
            caches
                .pending_edges
                .entry(edge_id)
                .or_insert_with(|| PendingProviderEdge {
                    provider_session_id: session.provider_session_id.clone(),
                    parent_provider_session_id: session.parent_provider_session_id.clone(),
                    session_id,
                    parent_session_id,
                    root_session_id: requested_root_session_id,
                    source_id,
                    source_format: source.source_format.clone(),
                    imported_at,
                    fidelity: session.fidelity,
                    line_number,
                });
        }
    }

    if let Some(event) = &capture.event {
        let (payload, redacted_payload) = sanitize_value(event.payload.clone());
        let (event_metadata, redacted_metadata) = sanitize_value(event.metadata.clone());
        let event_hash = event
            .provider_event_hash
            .clone()
            .unwrap_or(compute_payload_hash(&payload)?);
        let dedupe_key = Store::provider_event_dedupe_key(
            provider,
            &session.provider_session_id,
            event.provider_event_index,
            &event_hash,
        );
        let command_run = provider_command_run_from_event(ProviderCommandRunInput {
            provider,
            provider_session_id: &session.provider_session_id,
            session_id,
            source_id,
            work_record_id: options.work_record_id,
            event,
            payload: &payload,
            event_hash: &event_hash,
        });
        let normalized_event = Event {
            id: provider_event_uuid(
                provider,
                &session.provider_session_id,
                event.provider_event_index,
            ),
            seq: provider_event_seq(
                provider,
                &session.provider_session_id,
                event.provider_event_index,
            ),
            work_record_id: options.work_record_id,
            session_id: Some(session_id),
            run_id: command_run.as_ref().map(|run| run.id),
            event_type: event.event_type,
            role: event.role,
            occurred_at: event.occurred_at,
            capture_source_id: Some(source_id),
            payload: json!({
                "provider": provider.as_str(),
                "provider_session_id": session.provider_session_id,
                "provider_event_index": event.provider_event_index,
                "provider_event_hash": event_hash,
                "cursor": event.cursor,
                "artifacts": event.artifacts,
                "body": payload,
            }),
            payload_blob_id: None,
            dedupe_key: Some(dedupe_key.clone()),
            redaction_state: effective_event_redaction_state(
                event.redaction_state,
                redacted_payload || redacted_metadata,
            ),
            sync: provider_sync_metadata(
                event.fidelity,
                json!({
                    "provider_session_id": session.provider_session_id,
                    "provider_event_index": event.provider_event_index,
                    "provider_event_hash": event_hash,
                    "cursor": event.cursor,
                    "source_format": source.source_format,
                    "source_trust": source.trust,
                    "fixture_line": line_number,
                    "imported_at": imported_at,
                    "event_idempotency_key": event.idempotency_key,
                    "metadata": event_metadata,
                }),
            ),
        };
        let was_present = if options.fast_event_inserts {
            if let Some(run) = &command_run {
                store.insert_run_if_absent(run)?;
            }
            !store.insert_event_if_absent(&normalized_event)?
        } else {
            let was_present = provider_event_exists(store, &dedupe_key)?;
            if let Some(run) = &command_run {
                store.upsert_run(run)?;
            }
            match store.upsert_event(&normalized_event) {
                Ok(_) => {}
                Err(StoreError::Sql(rusqlite::Error::QueryReturnedNoRows)) => {}
                Err(StoreError::ProviderEventConflict { .. }) => {
                    summary.skipped_events += 1;
                    summary.skipped += 1;
                    if redacted_payload || redacted_metadata {
                        summary.redacted += 1;
                    }
                    if options.persist_cursors {
                        persist_provider_cursor(store, capture)?;
                    }
                    return Ok(summary);
                }
                Err(err) => return Err(CaptureError::Store(err)),
            }
            was_present
        };
        if redacted_payload || redacted_metadata {
            summary.redacted += 1;
        }
        if was_present {
            summary.skipped_events += 1;
            summary.skipped += 1;
        } else {
            summary.imported_events += 1;
            summary.imported += 1;
        }
    }

    if options.persist_cursors {
        persist_provider_cursor(store, capture)?;
    }

    Ok(summary)
}

fn resolve_pending_provider_edges(
    store: &mut Store,
    summary: &mut ProviderImportSummary,
    caches: &mut ProviderImportCaches,
) -> Result<()> {
    let pending = std::mem::take(&mut caches.pending_edges);
    for (edge_id, edge) in pending {
        if caches.processed_edges.contains(&edge_id) {
            update_session_parent_if_needed(store, &edge, caches)?;
            continue;
        }
        if !provider_session_exists_cached(
            store,
            edge.parent_session_id,
            &mut caches.session_exists,
        )? {
            summary.skipped_edges += 1;
            summary.skipped += 1;
            continue;
        }
        let root_session_id = resolve_pending_root_session_id(store, &edge, caches)?;
        update_session_parent(store, &edge, root_session_id)?;
        caches.session_exists.insert(edge.session_id, true);

        let was_present = store.session_edge_exists(edge_id)?;
        let session_edge = SessionEdge {
            id: edge_id,
            from_session_id: edge.parent_session_id,
            to_session_id: edge.session_id,
            edge_type: SessionEdgeType::ParentChild,
            confidence: Confidence::Explicit,
            source_id: Some(edge.source_id),
            timestamps: timestamps(edge.imported_at),
            sync: provider_sync_metadata(
                edge.fidelity,
                json!({
                    "provider_session_id": edge.provider_session_id,
                    "parent_provider_session_id": edge.parent_provider_session_id,
                    "source_format": edge.source_format,
                    "fixture_line": edge.line_number,
                    "imported_at": edge.imported_at,
                    "deferred_edge_resolution": true,
                }),
            ),
        };
        store.upsert_session_edge(&session_edge)?;
        caches.processed_edges.insert(edge_id);
        if !was_present && caches.imported_edges.insert(edge_id) {
            summary.imported_edges += 1;
            summary.imported += 1;
        } else {
            summary.skipped_edges += 1;
            summary.skipped += 1;
        }
    }
    Ok(())
}

fn resolve_pending_root_session_id(
    store: &Store,
    edge: &PendingProviderEdge,
    caches: &mut ProviderImportCaches,
) -> Result<Option<Uuid>> {
    match edge.root_session_id {
        Some(root_id)
            if root_id == edge.session_id
                || provider_session_exists_cached(store, root_id, &mut caches.session_exists)? =>
        {
            Ok(Some(root_id))
        }
        Some(_) | None => Ok(Some(edge.parent_session_id)),
    }
}

fn update_session_parent_if_needed(
    store: &mut Store,
    edge: &PendingProviderEdge,
    caches: &mut ProviderImportCaches,
) -> Result<()> {
    let root_session_id = resolve_pending_root_session_id(store, edge, caches)?;
    update_session_parent(store, edge, root_session_id)
}

fn update_session_parent(
    store: &mut Store,
    edge: &PendingProviderEdge,
    root_session_id: Option<Uuid>,
) -> Result<()> {
    let mut session = store.get_session(edge.session_id)?;
    if session.parent_session_id == Some(edge.parent_session_id)
        && session.root_session_id == root_session_id
    {
        return Ok(());
    }
    session.parent_session_id = Some(edge.parent_session_id);
    session.root_session_id = root_session_id;
    session.timestamps.updated_at = edge.imported_at;
    store.upsert_session(&session)?;
    Ok(())
}

fn fixture_line_to_capture(
    fixture: &ProviderFixtureLine,
    context: &ProviderAdapterContext,
    source_format: &str,
    fidelity: Fidelity,
) -> ProviderCaptureEnvelope {
    let cursor = fixture
        .event
        .as_ref()
        .and_then(|event| event.cursor.as_ref())
        .map(|cursor| ProviderCursorRange {
            before: None,
            after: Some(ProviderCursorCheckpoint {
                stream: provider_cursor_stream(fixture.provider, source_format),
                cursor: cursor.clone(),
                observed_at: fixture
                    .event
                    .as_ref()
                    .map(|event| event.occurred_at)
                    .unwrap_or(context.imported_at),
            }),
        });

    ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider: fixture.provider,
        source: ProviderSourceEnvelope {
            source_format: source_format.to_owned(),
            machine_id: context.machine_id.clone(),
            observed_at: context.imported_at,
            raw_source_path: context
                .source_path
                .as_ref()
                .map(|path| path.display().to_string()),
            raw_retention: ProviderRawRetention::PathReference,
            redaction_boundary: ProviderRedactionBoundary::BeforeExport,
            trust: ProviderSourceTrust::Fixture,
            fidelity,
            cursor,
            idempotency_key: Some(format!(
                "provider-source:{}:{}:{}",
                fixture.provider.as_str(),
                source_format,
                fixture.session.provider_session_id
            )),
            metadata: json!({
                "adapter": "provider_fixture_jsonl",
            }),
        },
        session: ProviderSessionEnvelope {
            provider_session_id: fixture.session.provider_session_id.clone(),
            parent_provider_session_id: fixture.session.parent_provider_session_id.clone(),
            root_provider_session_id: fixture.session.root_provider_session_id.clone(),
            external_agent_id: fixture.session.external_agent_id.clone(),
            agent_type: fixture.session.agent_type,
            role_hint: fixture.session.role_hint.clone(),
            is_primary: fixture.session.is_primary,
            status: fixture.session.status,
            started_at: fixture.session.started_at,
            ended_at: fixture.session.ended_at,
            cwd: fixture.session.cwd.clone(),
            fidelity,
            idempotency_key: Some(format!(
                "provider-session:{}:{}",
                fixture.provider.as_str(),
                fixture.session.provider_session_id
            )),
            artifacts: Vec::new(),
            metadata: fixture.session.metadata.clone(),
        },
        event: fixture.event.as_ref().map(|event| ProviderEventEnvelope {
            provider_event_index: event.provider_event_index,
            provider_event_hash: event.provider_event_hash.clone(),
            cursor: event.cursor.clone(),
            event_type: event.event_type,
            role: event.role,
            occurred_at: event.occurred_at,
            fidelity,
            redaction_state: RedactionState::SafePreview,
            idempotency_key: Some(format!(
                "provider-event:{}:{}:{}",
                fixture.provider.as_str(),
                fixture.session.provider_session_id,
                event.provider_event_index
            )),
            artifacts: Vec::new(),
            payload: event.payload.clone(),
            metadata: event.metadata.clone(),
        }),
    }
}

fn provider_cursor_stream(provider: CaptureProvider, source_format: &str) -> String {
    format!("provider:{}:{}", provider.as_str(), source_format)
}

fn effective_event_redaction_state(
    requested: RedactionState,
    sanitizer_redacted: bool,
) -> RedactionState {
    match requested {
        RedactionState::Withheld => RedactionState::Withheld,
        RedactionState::Redacted => RedactionState::Redacted,
        RedactionState::Raw if !sanitizer_redacted => RedactionState::Raw,
        _ if sanitizer_redacted => RedactionState::Redacted,
        _ => RedactionState::SafePreview,
    }
}

fn persist_provider_cursor(store: &mut Store, capture: &ProviderCaptureEnvelope) -> Result<()> {
    let checkpoint = capture
        .source
        .cursor
        .as_ref()
        .and_then(|cursor| cursor.after.as_ref())
        .cloned()
        .or_else(|| {
            capture.event.as_ref().and_then(|event| {
                event
                    .cursor
                    .as_ref()
                    .map(|cursor| ProviderCursorCheckpoint {
                        stream: provider_cursor_stream(
                            capture.provider,
                            &capture.source.source_format,
                        ),
                        cursor: cursor.clone(),
                        observed_at: event.occurred_at,
                    })
            })
        });
    let Some(checkpoint) = checkpoint else {
        return Ok(());
    };

    store.upsert_sync_cursor(&SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                capture.provider.as_str(),
                capture.source.machine_id,
                checkpoint.stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: capture.source.machine_id.clone(),
        stream: checkpoint.stream,
        cursor: checkpoint.cursor,
        last_synced_at: Some(checkpoint.observed_at),
        timestamps: timestamps(checkpoint.observed_at),
    })?;
    Ok(())
}

pub fn spool_counts(inbox: impl AsRef<Path>) -> Result<SpoolCounts> {
    let inbox = inbox.as_ref();
    let mut counts = SpoolCounts::default();
    if !inbox.exists() {
        return Ok(counts);
    }

    for entry in fs::read_dir(inbox)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if file_name.ends_with(".jsonl") {
            counts.pending += 1;
        } else if file_name.ends_with(".jsonl.tmp") {
            counts.tmp += 1;
        } else if file_name.ends_with(".jsonl.processing") {
            counts.processing += 1;
        } else if file_name.ends_with(".jsonl.done") {
            counts.done += 1;
        } else if file_name.ends_with(".jsonl.failed") {
            counts.failed += 1;
        }
    }

    Ok(counts)
}

pub fn retry_failed_spool_files(inbox: impl AsRef<Path>) -> Result<SpoolRepairSummary> {
    let inbox = inbox.as_ref();
    fs::create_dir_all(inbox)?;
    let mut summary = SpoolRepairSummary::default();

    for entry in fs::read_dir(inbox)? {
        let entry = entry?;
        let failed_path = entry.path();
        let file_name = failed_path
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_default();
        let Some(pending_name) = file_name.strip_suffix(".failed") else {
            continue;
        };
        if !pending_name.ends_with(".jsonl") {
            continue;
        }
        let pending_path = failed_path.with_file_name(pending_name);
        if pending_path.exists() {
            return Err(CaptureError::InvalidPath(pending_path));
        }
        let sidecar = append_suffix(&failed_path, ".error.json")?;
        fs::rename(&failed_path, &pending_path)?;
        if sidecar.exists() {
            fs::remove_file(sidecar)?;
        }
        summary.retried_files += 1;
    }

    Ok(summary)
}

pub fn archive_from_envelopes(envelopes: &[CaptureEnvelope]) -> Result<WorkRecordArchive> {
    let mut archive = WorkRecordArchive {
        schema_version: 1,
        version: 1,
        records: Vec::new(),
        ..WorkRecordArchive::default()
    };

    for envelope in envelopes {
        validate_envelope(envelope)?;
        if let Some(archive_value) = envelope.payload.get("archive") {
            let nested: WorkRecordArchive = serde_json::from_value(archive_value.clone())?;
            archive.records.extend(nested.records);
            archive.capture_sources.extend(nested.capture_sources);
            archive.sessions.extend(nested.sessions);
            archive.runs.extend(nested.runs);
            archive.events.extend(nested.events);
            archive.artifact_records.extend(nested.artifact_records);
            archive.vcs_workspaces.extend(nested.vcs_workspaces);
            archive.vcs_changes.extend(nested.vcs_changes);
            archive.work_record_links.extend(nested.work_record_links);
            archive.summaries.extend(nested.summaries);
            archive.files_touched.extend(nested.files_touched);
            continue;
        }

        let record_value = envelope
            .payload
            .get("record")
            .filter(|value| value.is_object());
        let should_create_record =
            record_value.is_some() || payload_has_record_fields(&envelope.payload);

        if should_create_record {
            let value = record_value.unwrap_or(&envelope.payload);
            let record = record_from_envelope(envelope, value)?;
            archive.records.push(record);
        }
    }

    Ok(archive)
}

pub fn stable_capture_uuid(dedupe_key: &str, role: &str) -> Uuid {
    let mut bytes = [0_u8; 16];
    let name = format!("ctx-work-record-capture:{dedupe_key}:{role}");
    let first = fnv1a64(name.as_bytes()).to_be_bytes();
    let second = fnv1a64(format!("{name}:uuid-v7").as_bytes()).to_be_bytes();

    bytes[..6].copy_from_slice(&first[..6]);
    bytes[6] = 0x70 | (first[6] & 0x0f);
    bytes[7] = first[7];
    bytes[8] = 0x80 | (second[0] & 0x3f);
    bytes[9..].copy_from_slice(&second[1..]);
    Uuid::from_bytes(bytes)
}

pub fn compute_payload_hash(payload: &Value) -> Result<String> {
    let bytes = serde_json::to_vec(payload)?;
    Ok(format!("fnv1a64:{:016x}", fnv1a64(&bytes)))
}

fn import_processing_file(path: &Path, store: &mut Store) -> Result<ArchiveCounts> {
    let envelopes = read_jsonl(path)?;
    let mut counts = ArchiveCounts::default();
    for envelope in envelopes {
        counts.add(import_envelope(store, &envelope)?);
    }
    Ok(counts)
}

fn import_envelope(store: &mut Store, envelope: &CaptureEnvelope) -> Result<ArchiveCounts> {
    let archive = archive_from_envelopes(std::slice::from_ref(envelope))?;
    let source_id = stable_capture_uuid(&envelope.dedupe_key, "source");
    store.import_archive_from_capture_source(
        &archive,
        source_id,
        &envelope.source,
        envelope.occurred_at,
        envelope.fidelity,
        true,
    )?;
    Ok(ArchiveCounts {
        records: archive.records.len(),
    })
}

fn validate_envelope(envelope: &CaptureEnvelope) -> Result<()> {
    if envelope.schema_version == CAPTURE_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(CaptureError::UnsupportedSchemaVersion(
            envelope.schema_version,
        ))
    }
}

fn pending_spool_files(inbox: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(inbox)? {
        let entry = entry?;
        let path = entry.path();
        if path
            .file_name()
            .map(|name| name.to_string_lossy().ends_with(".jsonl"))
            .unwrap_or(false)
        {
            ensure_regular_spool_file(&path)?;
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn claim_pending_file(path: &Path) -> Result<PathBuf> {
    ensure_regular_spool_file(path)?;
    let processing = append_suffix(path, ".processing")?;
    fs::rename(path, &processing)?;
    Ok(processing)
}

fn ensure_regular_spool_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_file() {
        Ok(())
    } else {
        Err(CaptureError::InvalidPath(path.to_path_buf()))
    }
}

fn write_failure_metadata(failed_path: &Path, err: &CaptureError) -> Result<()> {
    let sidecar = append_suffix(failed_path, ".error.json")?;
    let metadata = json!({
        "failed_at": Utc::now(),
        "spool_file": failed_path,
        "error": err.to_string(),
    });
    fs::write(sidecar, serde_json::to_vec_pretty(&metadata)?)?;
    Ok(())
}

fn append_suffix(path: &Path, suffix: &str) -> Result<PathBuf> {
    let file_name = path
        .file_name()
        .ok_or_else(|| CaptureError::InvalidPath(path.to_path_buf()))?
        .to_string_lossy();
    Ok(path.with_file_name(format!("{file_name}{suffix}")))
}

fn state_path(processing_path: &Path, state_suffix: &str) -> Result<PathBuf> {
    let file_name = processing_path
        .file_name()
        .ok_or_else(|| CaptureError::InvalidPath(processing_path.to_path_buf()))?
        .to_string_lossy();
    let base = file_name
        .strip_suffix(".processing")
        .ok_or_else(|| CaptureError::InvalidPath(processing_path.to_path_buf()))?;
    Ok(processing_path.with_file_name(format!("{base}{state_suffix}")))
}

fn record_from_envelope(envelope: &CaptureEnvelope, value: &Value) -> Result<WorkRecord> {
    let id = uuid_field(value, "id")?
        .unwrap_or_else(|| stable_capture_uuid(&envelope.dedupe_key, "record"));
    let title = string_field(value, "title")
        .or_else(|| string_field(value, "summary"))
        .unwrap_or_else(|| format!("Captured {} event", envelope.source.provider));
    let body = match string_field(value, "body").or_else(|| string_field(value, "summary")) {
        Some(body) => body,
        None => serde_json::to_string_pretty(&envelope.payload)?,
    };
    let tags = string_array_field(value, "tags")?.unwrap_or_else(|| {
        vec![
            "capture".to_owned(),
            envelope.source.provider.as_str().to_owned(),
        ]
    });
    let kind = string_field(value, "record_kind")
        .or_else(|| string_field(value, "work_record_kind"))
        .or_else(|| string_field(value, "kind").filter(|kind| kind != "work_record"))
        .unwrap_or_else(|| "capture".to_owned());
    let workspace = string_field(value, "workspace")
        .or_else(|| envelope.cwd.clone())
        .or_else(|| envelope.source.cwd.clone());
    let created_at = datetime_field(value, "created_at")?.unwrap_or(envelope.occurred_at);
    let updated_at = datetime_field(value, "updated_at")?.unwrap_or(created_at);

    Ok(WorkRecord {
        id,
        title,
        body,
        tags,
        kind,
        workspace,
        created_at,
        updated_at,
    })
}

fn uuid_field(value: &Value, field: &str) -> Result<Option<Uuid>> {
    match value.get(field) {
        Some(Value::String(raw)) => Ok(Some(Uuid::parse_str(raw)?)),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(CaptureError::InvalidPayload(format!(
            "{field} must be a UUID string"
        ))),
    }
}

fn datetime_field(value: &Value, field: &str) -> Result<Option<DateTime<Utc>>> {
    match value.get(field) {
        Some(Value::String(raw)) => {
            Ok(Some(DateTime::parse_from_rfc3339(raw)?.with_timezone(&Utc)))
        }
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(CaptureError::InvalidPayload(format!(
            "{field} must be an RFC3339 timestamp string"
        ))),
    }
}

fn string_field(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_owned)
}

fn string_array_field(value: &Value, field: &str) -> Result<Option<Vec<String>>> {
    match value.get(field) {
        Some(Value::Array(items)) => {
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                let item = item.as_str().ok_or_else(|| {
                    CaptureError::InvalidPayload(format!("{field} must contain only strings"))
                })?;
                values.push(item.to_owned());
            }
            Ok(Some(values))
        }
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(CaptureError::InvalidPayload(format!(
            "{field} must be an array of strings"
        ))),
    }
}

fn provider_event_exists(store: &Store, dedupe_key: &str) -> Result<bool> {
    match store.event_id_by_dedupe_key(dedupe_key) {
        Ok(_) => Ok(true),
        Err(StoreError::Sql(rusqlite::Error::QueryReturnedNoRows)) => Ok(false),
        Err(err) => Err(CaptureError::Store(err)),
    }
}

fn provider_session_exists(store: &Store, session_id: Uuid) -> Result<bool> {
    match store.get_session(session_id) {
        Ok(_) => Ok(true),
        Err(StoreError::NotFound(_)) => Ok(false),
        Err(err) => Err(CaptureError::Store(err)),
    }
}

fn provider_session_exists_cached(
    store: &Store,
    session_id: Uuid,
    cache: &mut BTreeMap<Uuid, bool>,
) -> Result<bool> {
    if let Some(exists) = cache.get(&session_id) {
        return Ok(*exists);
    }
    let exists = provider_session_exists(store, session_id)?;
    cache.insert(session_id, exists);
    Ok(exists)
}

struct ProviderCommandRunInput<'a> {
    provider: CaptureProvider,
    provider_session_id: &'a str,
    session_id: Uuid,
    source_id: Uuid,
    work_record_id: Option<Uuid>,
    event: &'a ProviderEventEnvelope,
    payload: &'a Value,
    event_hash: &'a str,
}

fn provider_command_run_from_event(input: ProviderCommandRunInput<'_>) -> Option<Run> {
    let ProviderCommandRunInput {
        provider,
        provider_session_id,
        session_id,
        source_id,
        work_record_id,
        event,
        payload,
        event_hash,
    } = input;
    if event.event_type != EventType::CommandOutput {
        return None;
    }
    let command_preview = payload
        .get("command")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    let call_id = payload.get("call_id").and_then(Value::as_str);
    let key = call_id.unwrap_or(event_hash);
    let duration_ms = payload.get("duration_ms").and_then(Value::as_i64);
    let ended_at = Some(event.occurred_at);
    let started_at = duration_ms
        .and_then(|duration| {
            event
                .occurred_at
                .checked_sub_signed(chrono::Duration::milliseconds(duration.max(0)))
        })
        .unwrap_or(event.occurred_at);
    Some(Run {
        id: provider_run_uuid(provider, provider_session_id, key),
        work_record_id,
        session_id: Some(session_id),
        run_type: RunType::Command,
        status: provider_command_run_status(payload),
        started_at,
        ended_at,
        exit_code: payload
            .get("exit_code")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok()),
        cwd: None,
        command_preview,
        input_blob_id: None,
        output_blob_id: None,
        timestamps: timestamps(event.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            event.fidelity,
            json!({
                "provider_session_id": provider_session_id,
                "provider_event_index": event.provider_event_index,
                "provider_event_hash": event_hash,
                "call_id": call_id,
                "source": "provider_command_output",
            }),
        ),
    })
}

fn provider_command_run_status(payload: &Value) -> RunStatus {
    if payload
        .get("timed_out")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return RunStatus::Cancelled;
    }
    match payload.get("exit_code").and_then(Value::as_i64) {
        Some(0) => RunStatus::Succeeded,
        Some(_) => RunStatus::Failed,
        None => RunStatus::Partial,
    }
}

fn provider_source_uuid(provider: CaptureProvider, provider_session_id: &str) -> Uuid {
    stable_capture_uuid(
        &format!("provider:{}:{provider_session_id}", provider.as_str()),
        "source",
    )
}

fn provider_session_uuid(provider: CaptureProvider, provider_session_id: &str) -> Uuid {
    stable_capture_uuid(
        &format!("provider:{}:{provider_session_id}", provider.as_str()),
        "session",
    )
}

fn provider_run_uuid(provider: CaptureProvider, provider_session_id: &str, run_key: &str) -> Uuid {
    stable_capture_uuid(
        &format!(
            "provider:{}:{provider_session_id}:run:{run_key}",
            provider.as_str()
        ),
        "run",
    )
}

fn provider_event_uuid(
    provider: CaptureProvider,
    provider_session_id: &str,
    provider_event_index: u64,
) -> Uuid {
    stable_capture_uuid(
        &format!(
            "provider:{}:{provider_session_id}:{provider_event_index}",
            provider.as_str()
        ),
        "event",
    )
}

fn provider_event_seq(
    provider: CaptureProvider,
    provider_session_id: &str,
    provider_event_index: u64,
) -> u64 {
    let session_key = format!("provider:{}:{provider_session_id}", provider.as_str());
    ((fnv1a64(session_key.as_bytes()) & 0x0000_07ff_ffff_ffff) << 20)
        | (provider_event_index & 0x000f_ffff)
}

fn provider_edge_uuid(
    provider: CaptureProvider,
    provider_session_id: &str,
    edge_kind: &str,
) -> Uuid {
    stable_capture_uuid(
        &format!(
            "provider:{}:{provider_session_id}:{edge_kind}",
            provider.as_str()
        ),
        "session-edge",
    )
}

fn timestamps(at: DateTime<Utc>) -> EntityTimestamps {
    EntityTimestamps {
        created_at: at,
        updated_at: at,
    }
}

fn provider_sync_metadata(fidelity: Fidelity, metadata: Value) -> SyncMetadata {
    SyncMetadata {
        visibility: Visibility::default(),
        fidelity,
        sync_state: SyncState::default(),
        sync_version: 0,
        deleted_at: None,
        metadata,
    }
}

fn sanitize_value(value: Value) -> (Value, bool) {
    match value {
        Value::Object(map) => {
            let mut redacted = false;
            let mut sanitized = serde_json::Map::new();
            for (key, value) in map {
                if is_sensitive_key(&key) {
                    sanitized.insert(key, Value::String("[REDACTED]".to_owned()));
                    redacted = true;
                    continue;
                }
                let (value, child_redacted) = sanitize_value(value);
                redacted |= child_redacted;
                sanitized.insert(key, value);
            }
            (Value::Object(sanitized), redacted)
        }
        Value::Array(items) => {
            let mut redacted = false;
            let items = items
                .into_iter()
                .map(|item| {
                    let (item, child_redacted) = sanitize_value(item);
                    redacted |= child_redacted;
                    item
                })
                .collect();
            (Value::Array(items), redacted)
        }
        Value::String(text) if looks_sensitive_string(&text) => {
            (Value::String("[REDACTED]".to_owned()), true)
        }
        other => (other, false),
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("api_key")
        || key.contains("apikey")
        || key.contains("token")
        || key.contains("secret")
        || key.contains("password")
        || key == "authorization"
}

fn looks_sensitive_string(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    value.starts_with("sk-")
        || value.starts_with("sess-")
        || value.contains("Bearer ")
        || value.contains("BEGIN PRIVATE KEY")
        || lower.contains("token=")
        || lower.contains("password=")
        || lower.contains("secret=")
}

fn default_metadata() -> Value {
    json!({})
}

fn payload_has_record_fields(value: &Value) -> bool {
    [
        "title",
        "body",
        "summary",
        "tags",
        "record_kind",
        "work_record_kind",
        "workspace",
    ]
    .iter()
    .any(|field| value.get(*field).is_some())
}

fn default_machine_id() -> String {
    env::var("CTX_MACHINE_ID")
        .or_else(|_| env::var("HOSTNAME"))
        .or_else(|_| env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "local".to_owned())
}

fn sanitize_filename_component(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let sanitized = sanitized.trim_matches('-');
    if sanitized.is_empty() {
        "unknown".to_owned()
    } else {
        sanitized.to_owned()
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tempdir() -> TempDir {
        let root = std::env::current_dir().unwrap().join("target/test-data");
        fs::create_dir_all(&root).unwrap();
        tempfile::Builder::new()
            .prefix("work-record-capture-")
            .tempdir_in(root)
            .unwrap()
    }

    fn fixture_options(dedupe_key: &str, title: &str) -> FixtureOptions {
        FixtureOptions {
            title: title.to_owned(),
            body: "captured body".to_owned(),
            tags: vec!["capture-test".to_owned()],
            dedupe_key: Some(dedupe_key.to_owned()),
            machine_id: Some("test-machine".to_owned()),
            cwd: Some(PathBuf::from("/tmp/work")),
            occurred_at: DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        }
    }

    fn provider_fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/provider")
            .join(name)
    }

    fn provider_history_fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/provider-history")
            .join(name)
    }

    fn fixed_import_options(path: PathBuf) -> ProviderFixtureImportOptions {
        ProviderFixtureImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(path),
            imported_at: DateTime::parse_from_rfc3339("2026-06-23T15:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            work_record_id: None,
            expected_provider: None,
            allow_partial_failures: false,
            ..ProviderFixtureImportOptions::default()
        }
    }

    #[test]
    fn spool_writer_closes_tmp_file_atomically_to_jsonl() {
        let temp = tempdir();
        let inbox = temp.path().join("inbox");
        let envelope = fixture_envelope(fixture_options("atomic", "Atomic capture")).unwrap();
        let mut writer = SpoolWriter::create(&inbox, "test-machine").unwrap();
        let tmp_path = writer.tmp_path().to_path_buf();
        let final_path = writer.final_path().to_path_buf();

        writer.write_envelope(&envelope).unwrap();
        assert!(tmp_path.exists());
        assert!(!final_path.exists());

        let closed_path = writer.finish().unwrap();
        assert_eq!(closed_path, final_path);
        assert!(!tmp_path.exists());
        assert!(final_path.exists());
        assert_eq!(read_jsonl(&final_path).unwrap(), vec![envelope]);
    }

    #[test]
    fn failed_import_retains_raw_failed_file_and_error_metadata() {
        let temp = tempdir();
        let inbox = temp.path().join("inbox");
        fs::create_dir_all(&inbox).unwrap();
        let pending = inbox.join("capture-bad.jsonl");
        fs::write(&pending, "not json\n").unwrap();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_spool(&inbox, &mut store).unwrap();

        assert_eq!(summary.failed_files, 1);
        assert_eq!(summary.processed_files, 1);
        let failed = inbox.join("capture-bad.jsonl.failed");
        let sidecar = inbox.join("capture-bad.jsonl.failed.error.json");
        assert!(failed.exists());
        assert!(sidecar.exists());
        assert_eq!(fs::read_to_string(failed).unwrap(), "not json\n");
        assert!(fs::read_to_string(sidecar)
            .unwrap()
            .contains("not a valid capture envelope"));
        assert_eq!(spool_counts(&inbox).unwrap().failed, 1);
    }

    #[test]
    fn import_rejects_non_regular_pending_spool_entry() {
        let temp = tempdir();
        let inbox = temp.path().join("inbox");
        fs::create_dir_all(inbox.join("capture-dir.jsonl")).unwrap();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        assert!(matches!(
            import_spool(&inbox, &mut store),
            Err(CaptureError::InvalidPath(path)) if path.ends_with("capture-dir.jsonl")
        ));
        assert!(inbox.join("capture-dir.jsonl").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn import_rejects_symlink_pending_spool_entry() {
        use std::os::unix::fs::symlink;

        let temp = tempdir();
        let inbox = temp.path().join("inbox");
        fs::create_dir_all(&inbox).unwrap();
        let target = temp.path().join("outside.jsonl");
        fs::write(&target, "not json\n").unwrap();
        let pending = inbox.join("capture-link.jsonl");
        symlink(&target, &pending).unwrap();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        assert!(matches!(
            import_spool(&inbox, &mut store),
            Err(CaptureError::InvalidPath(path)) if path.ends_with("capture-link.jsonl")
        ));
        assert!(pending.exists());
        assert_eq!(fs::read_to_string(target).unwrap(), "not json\n");
    }

    #[test]
    fn import_is_idempotent_by_dedupe_key() {
        let temp = tempdir();
        let inbox = temp.path().join("inbox");
        let envelope = fixture_envelope(fixture_options("same-dedupe", "First title")).unwrap();
        let mut first = SpoolWriter::create(&inbox, "test-machine").unwrap();
        first.write_envelope(&envelope).unwrap();
        first.finish().unwrap();
        let mut second = SpoolWriter::create(&inbox, "test-machine").unwrap();
        second.write_envelope(&envelope).unwrap();
        second.finish().unwrap();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_spool(&inbox, &mut store).unwrap();

        assert_eq!(summary.failed_files, 0);
        assert_eq!(summary.processed_files, 2);
        let records = store.list_records(10).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, stable_capture_uuid("same-dedupe", "record"));
        assert_eq!(records[0].id.get_version_num(), 7);
        assert_eq!(records[0].title, "First title");
        assert_eq!(spool_counts(&inbox).unwrap().done, 2);
    }

    #[test]
    fn provider_fixture_replay_imports_codex_session_tree_and_is_idempotent() {
        let temp = tempdir();
        let db_path = temp.path().join("work.sqlite");
        let fixture = provider_fixture("codex.jsonl");
        let mut store = Store::open(&db_path).unwrap();

        let first = import_provider_fixture_jsonl(
            &fixture,
            &mut store,
            fixed_import_options(fixture.clone()),
        )
        .unwrap();
        assert_eq!(first.failed, 0, "{:?}", first.failures);
        assert_eq!(first.imported_sessions, 2);
        assert_eq!(first.imported_events, 3);
        assert_eq!(first.imported_edges, 1);
        assert_eq!(first.skipped_events, 0);

        let second = import_provider_fixture_jsonl(
            &fixture,
            &mut store,
            fixed_import_options(fixture.clone()),
        )
        .unwrap();
        assert_eq!(second.failed, 0);
        assert_eq!(second.imported_events, 0);
        assert_eq!(second.imported_edges, 0);
        assert_eq!(second.skipped_events, 3);
        assert_eq!(second.skipped_sessions, 2);
        assert_eq!(second.skipped_edges, 1);

        let parent_id = provider_session_uuid(CaptureProvider::Codex, "codex-session-1");
        let child_id = provider_session_uuid(CaptureProvider::Codex, "codex-session-1-subagent-a");
        let parent = store.get_session(parent_id).unwrap();
        let child = store.get_session(child_id).unwrap();
        assert_eq!(
            parent.external_session_id.as_deref(),
            Some("codex-session-1")
        );
        assert_eq!(child.parent_session_id, Some(parent_id));
        assert_eq!(child.root_session_id, Some(parent_id));
        assert_eq!(child.agent_type, AgentType::Subagent);
        assert_eq!(store.events_for_session(parent_id).unwrap().len(), 2);
        assert_eq!(store.events_for_session(child_id).unwrap().len(), 1);
        drop(store);

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let edge_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM session_edges", [], |row| row.get(0))
            .unwrap();
        assert_eq!(edge_count, 1);
        let (from_session_id, to_session_id, edge_type): (String, String, String) = conn
            .query_row(
                "SELECT from_session_id, to_session_id, edge_type FROM session_edges",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(from_session_id, parent_id.to_string());
        assert_eq!(to_session_id, child_id.to_string());
        assert_eq!(edge_type, "parent_child");
    }

    #[test]
    fn provider_fixture_replay_defers_child_edges_until_parent_is_known() {
        let temp = tempdir();
        let fixture = provider_fixture("out-of-order-subagent.jsonl");
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_provider_fixture_jsonl(
            &fixture,
            &mut store,
            fixed_import_options(fixture.clone()),
        )
        .unwrap();

        assert_eq!(summary.failed, 0, "{:?}", summary.failures);
        assert_eq!(summary.imported_sessions, 2);
        assert_eq!(summary.imported_events, 2);
        assert_eq!(summary.imported_edges, 1);
        assert_eq!(summary.skipped_edges, 0);

        let parent_id = provider_session_uuid(CaptureProvider::Codex, "out-of-order-root");
        let child_id = provider_session_uuid(CaptureProvider::Codex, "out-of-order-child");
        let child = store.get_session(child_id).unwrap();
        assert_eq!(child.parent_session_id, Some(parent_id));
        assert_eq!(child.root_session_id, Some(parent_id));
        let conn = rusqlite::Connection::open(temp.path().join("work.sqlite")).unwrap();
        let edge_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM session_edges", [], |row| row.get(0))
            .unwrap();
        assert_eq!(edge_count, 1);
    }

    #[test]
    fn provider_fixture_replay_supports_pi_and_redacts_metadata() {
        let temp = tempdir();
        let fixture = provider_fixture("pi.jsonl");
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_provider_fixture_jsonl(
            &fixture,
            &mut store,
            fixed_import_options(fixture.clone()),
        )
        .unwrap();

        assert_eq!(summary.failed, 0);
        assert_eq!(summary.imported_sessions, 1);
        assert_eq!(summary.imported_events, 2);
        assert_eq!(summary.redacted, 1);
        let session_id = provider_session_uuid(CaptureProvider::Pi, "pi-session-1");
        let events = store.events_for_session(session_id).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].redaction_state, RedactionState::Redacted);
        assert!(events[1].sync.metadata.to_string().contains("[REDACTED]"));
        assert!(!events[1]
            .sync
            .metadata
            .to_string()
            .contains("fixture-token-value"));
    }

    #[test]
    fn pi_session_import_replays_documented_session_jsonl_and_is_idempotent() {
        let temp = tempdir();
        let fixture = provider_history_fixture("pi-session.jsonl");
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let first = import_pi_session_jsonl(
            &fixture,
            &mut store,
            PiSessionImportOptions {
                source_path: Some(fixture.clone()),
                imported_at: "2026-06-23T16:00:00Z".parse().unwrap(),
                ..PiSessionImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(first.failed, 0, "{:?}", first.failures);
        assert_eq!(first.imported_sessions, 1);
        assert_eq!(first.imported_events, 6);
        assert_eq!(first.redacted, 3);

        let second = import_pi_session_jsonl(
            &fixture,
            &mut store,
            PiSessionImportOptions {
                source_path: Some(fixture.clone()),
                imported_at: "2026-06-23T16:00:00Z".parse().unwrap(),
                ..PiSessionImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(second.failed, 0);
        assert_eq!(second.imported_events, 0);
        assert_eq!(second.skipped_events, 6);

        let session_id = provider_session_uuid(CaptureProvider::Pi, "pi-session-docs-1");
        let session = store.get_session(session_id).unwrap();
        assert_eq!(session.sync.fidelity, Fidelity::Imported);
        assert_eq!(
            session.sync.metadata["source_format"].as_str(),
            Some("pi_session_jsonl")
        );
        let events = store.events_for_session(session_id).unwrap();
        assert_eq!(events.len(), 6);
        assert_eq!(events[0].role, Some(EventRole::User));
        assert_eq!(events[1].event_type, EventType::ToolCall);
        assert_eq!(events[2].event_type, EventType::ToolOutput);
        assert_eq!(events[3].event_type, EventType::CommandOutput);
        assert_eq!(events[4].event_type, EventType::Message);
        assert_eq!(events[4].role, Some(EventRole::Assistant));
        assert_eq!(events[5].event_type, EventType::Summary);
        assert!(events[3].payload.to_string().contains("cargo test"));
        assert!(events[3].payload.to_string().contains("[REDACTED]"));
        assert!(!events[3].payload.to_string().contains("fixture-secret"));
    }

    #[test]
    fn codex_session_tree_imports_messages_and_subagent_edges() {
        let temp = tempdir();
        let fixture = provider_history_fixture("codex-sessions");
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let first = import_codex_session_tree(
            &fixture,
            &mut store,
            CodexSessionImportOptions {
                source_path: Some(fixture.clone()),
                imported_at: "2026-06-23T16:30:00Z".parse().unwrap(),
                ..CodexSessionImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(first.failed, 0, "{:?}", first.failures);
        assert_eq!(first.imported_sessions, 2);
        assert_eq!(first.imported_events, 8);
        assert_eq!(first.imported_edges, 1);

        let second = import_codex_session_tree(
            &fixture,
            &mut store,
            CodexSessionImportOptions {
                source_path: Some(fixture.clone()),
                imported_at: "2026-06-23T16:30:00Z".parse().unwrap(),
                ..CodexSessionImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(second.failed, 0);
        assert_eq!(second.imported_events, 0);
        assert_eq!(second.imported_edges, 0);
        assert_eq!(second.skipped_events, 8);
        assert_eq!(second.skipped_edges, 1);

        let parent_id = provider_session_uuid(CaptureProvider::Codex, "codex-session-root");
        let child_id = provider_session_uuid(CaptureProvider::Codex, "codex-session-child");
        let parent = store.get_session(parent_id).unwrap();
        let child = store.get_session(child_id).unwrap();
        assert_eq!(parent.sync.fidelity, Fidelity::Imported);
        assert_eq!(
            parent.sync.metadata["source_format"].as_str(),
            Some("codex_session_jsonl")
        );
        assert_eq!(child.parent_session_id, Some(parent_id));
        assert_eq!(child.root_session_id, Some(parent_id));
        assert_eq!(child.agent_type, AgentType::Subagent);
        assert_eq!(child.role_hint.as_deref(), Some("worker"));

        let parent_events = store.events_for_session(parent_id).unwrap();
        assert_eq!(parent_events.len(), 6);
        assert!(parent_events
            .iter()
            .any(|event| event.event_type == EventType::Message
                && event.payload.to_string().contains("Fix the onboarding bug")));
        assert!(parent_events
            .iter()
            .any(|event| event.event_type == EventType::Message
                && event
                    .payload
                    .to_string()
                    .contains("checking the setup flow")));
        assert!(parent_events
            .iter()
            .any(|event| event.event_type == EventType::Notice
                && event.payload.to_string().contains("task_complete")));
        assert!(parent_events
            .iter()
            .any(|event| event.event_type == EventType::ToolCall
                && event.payload.to_string().contains("exec_command")));
        assert!(parent_events
            .iter()
            .any(|event| event.event_type == EventType::CommandOutput
                && event
                    .payload
                    .to_string()
                    .contains("all onboarding tests passed")));
        assert!(parent_events
            .iter()
            .any(|event| event.event_type == EventType::Summary
                && event
                    .payload
                    .to_string()
                    .contains("provider history discovery")));
        let child_events = store.events_for_session(child_id).unwrap();
        assert_eq!(child_events.len(), 2);
        assert!(child_events
            .iter()
            .any(|event| event.payload.to_string().contains("dashboard search")));
    }

    #[test]
    fn codex_session_tree_defers_cross_file_child_edges_until_parent_is_known() {
        let temp = tempdir();
        let fixture = provider_history_fixture("codex-out-of-order-sessions");
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_codex_session_tree(
            &fixture,
            &mut store,
            CodexSessionImportOptions {
                source_path: Some(fixture.clone()),
                imported_at: "2026-06-24T02:15:00Z".parse().unwrap(),
                max_session_files: Some(usize::MAX),
                ..CodexSessionImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 0, "{:?}", summary.failures);
        assert_eq!(summary.imported_sessions, 2);
        assert_eq!(summary.imported_events, 2);
        assert_eq!(summary.imported_edges, 1);

        let parent_id = provider_session_uuid(CaptureProvider::Codex, "codex-out-of-order-root");
        let child_id = provider_session_uuid(CaptureProvider::Codex, "codex-out-of-order-child");
        let child = store.get_session(child_id).unwrap();
        assert_eq!(child.parent_session_id, Some(parent_id));
        assert_eq!(child.root_session_id, Some(parent_id));
    }

    #[test]
    fn codex_session_paths_imports_only_explicit_subset() {
        let temp = tempdir();
        let fixture = provider_history_fixture("codex-sessions").join("2026/06/23/root.jsonl");
        let total_bytes = fs::metadata(&fixture).unwrap().len();
        let progress = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = Arc::clone(&progress);
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_codex_session_paths(
            vec![fixture.clone()],
            &mut store,
            CodexSessionImportOptions {
                source_path: Some(fixture.clone()),
                imported_at: "2026-06-24T02:30:00Z".parse().unwrap(),
                progress: Some(Arc::new(move |progress| {
                    observed.lock().unwrap().push((
                        progress.total_files,
                        progress.total_bytes,
                        progress.done,
                    ));
                })),
                ..CodexSessionImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 0, "{:?}", summary.failures);
        assert_eq!(summary.imported_sessions, 1);
        assert_eq!(summary.imported_events, 6);
        assert_eq!(summary.imported_edges, 0);
        assert_eq!(store.list_sessions().unwrap().len(), 1);
        let root_id = provider_session_uuid(CaptureProvider::Codex, "codex-session-root");
        let child_id = provider_session_uuid(CaptureProvider::Codex, "codex-session-child");
        assert_eq!(store.events_for_session(root_id).unwrap().len(), 6);
        assert!(store.events_for_session(child_id).unwrap().is_empty());

        let progress = progress.lock().unwrap();
        assert!(progress
            .iter()
            .all(|(files, bytes, _)| { *files == 1 && *bytes == total_bytes }));
        assert_eq!(progress.last().map(|(_, _, done)| *done), Some(true));
    }

    #[test]
    fn codex_session_paths_reimport_skips_existing_events() {
        let temp = tempdir();
        let fixture = provider_history_fixture("codex-sessions").join("2026/06/23");
        let paths = vec![fixture.join("root.jsonl"), fixture.join("subagent.jsonl")];
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let first = import_codex_session_paths(
            paths.clone(),
            &mut store,
            CodexSessionImportOptions {
                imported_at: "2026-06-24T02:45:00Z".parse().unwrap(),
                ..CodexSessionImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(first.failed, 0, "{:?}", first.failures);
        assert_eq!(first.imported_sessions, 2);
        assert_eq!(first.imported_events, 8);
        assert_eq!(first.imported_edges, 1);

        let second = import_codex_session_paths(
            paths,
            &mut store,
            CodexSessionImportOptions {
                imported_at: "2026-06-24T02:45:00Z".parse().unwrap(),
                ..CodexSessionImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(second.failed, 0, "{:?}", second.failures);
        assert_eq!(second.imported_sessions, 0);
        assert_eq!(second.imported_events, 0);
        assert_eq!(second.imported_edges, 0);
        assert_eq!(second.skipped_sessions, 2);
        assert_eq!(second.skipped_events, 8);
        assert_eq!(second.skipped_edges, 1);
    }

    #[cfg(unix)]
    #[test]
    fn codex_session_paths_rejects_symlinked_jsonl_files() {
        use std::os::unix::fs::symlink;

        let temp = tempdir();
        let fixture = provider_history_fixture("codex-sessions").join("2026/06/23/root.jsonl");
        let link = temp.path().join("linked-root.jsonl");
        symlink(&fixture, &link).unwrap();

        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let err = import_codex_session_paths(
            vec![link],
            &mut store,
            CodexSessionImportOptions {
                imported_at: "2026-06-24T03:00:00Z".parse().unwrap(),
                ..CodexSessionImportOptions::default()
            },
        )
        .unwrap_err();

        assert!(matches!(
            err,
            CaptureError::InvalidProviderTranscriptPath { path, reason }
                if path.ends_with("linked-root.jsonl")
                    && reason == "symlinked provider transcript files are rejected"
        ));
        assert!(store.list_sessions().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn codex_session_file_rejects_symlinked_jsonl_files() {
        use std::os::unix::fs::symlink;

        let temp = tempdir();
        let fixture = provider_history_fixture("codex-sessions").join("2026/06/23/root.jsonl");
        let link = temp.path().join("linked-root.jsonl");
        symlink(&fixture, &link).unwrap();

        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let err = import_codex_session_jsonl(
            &link,
            &mut store,
            CodexSessionImportOptions {
                imported_at: "2026-06-23T16:30:00Z".parse().unwrap(),
                ..CodexSessionImportOptions::default()
            },
        )
        .unwrap_err();

        assert!(matches!(
            err,
            CaptureError::InvalidProviderTranscriptPath { path, reason }
                if path.ends_with("linked-root.jsonl")
                    && reason == "symlinked provider transcript files are rejected"
        ));
        assert!(store.list_sessions().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn codex_session_tree_rejects_symlinked_jsonl_files() {
        use std::os::unix::fs::symlink;

        let temp = tempdir();
        let fixture = provider_history_fixture("codex-sessions").join("2026/06/23");
        let sessions = temp.path().join("sessions/2026/06/23");
        fs::create_dir_all(&sessions).unwrap();
        symlink(fixture.join("root.jsonl"), sessions.join("root.jsonl")).unwrap();

        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let err = import_codex_session_tree(
            temp.path().join("sessions"),
            &mut store,
            CodexSessionImportOptions {
                imported_at: "2026-06-23T16:30:00Z".parse().unwrap(),
                ..CodexSessionImportOptions::default()
            },
        )
        .unwrap_err();

        assert!(matches!(
            err,
            CaptureError::InvalidProviderTranscriptPath { path, reason }
                if path.ends_with("root.jsonl")
                    && reason == "symlinked provider transcript files are rejected"
        ));
        assert!(store.list_sessions().unwrap().is_empty());
    }

    #[test]
    fn codex_session_tree_imports_rich_tool_outputs_and_redacts_previews() {
        let temp = tempdir();
        let fixture = provider_history_fixture("codex-rich-sessions");
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_codex_session_tree(
            &fixture,
            &mut store,
            CodexSessionImportOptions {
                source_path: Some(fixture.clone()),
                imported_at: "2026-06-24T01:30:00Z".parse().unwrap(),
                ..CodexSessionImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 0, "{:?}", summary.failures);
        assert_eq!(summary.imported_sessions, 1);
        assert_eq!(summary.imported_events, 12);

        let session_id = provider_session_uuid(CaptureProvider::Codex, "codex-rich-session");
        let events = store.events_for_session(session_id).unwrap();
        assert!(events
            .iter()
            .any(|event| event.event_type == EventType::ToolCall
                && event.payload.to_string().contains("apply_patch")));
        assert!(events
            .iter()
            .any(|event| event.event_type == EventType::CommandOutput
                && event.payload.to_string().contains("unit tests passed")));
        assert!(events
            .iter()
            .any(|event| event.event_type == EventType::Summary
                && event
                    .payload
                    .to_string()
                    .contains("sample command completed")));
        assert!(events
            .iter()
            .any(|event| event.event_type == EventType::Notice
                && event.payload.to_string().contains("patch_apply_end")));

        let rendered = serde_json::to_string(&events).unwrap();
        assert!(rendered.contains("[REDACTED_SECRET]") || rendered.contains("[REDACTED]"));
        assert!(!rendered.contains("ghp_1234567890abcdef"));
        assert!(!rendered.contains("/home/example/private-repo"));
        assert!(!rendered.contains("opaque-private-reasoning-payload"));
    }

    #[test]
    fn codex_failures_output_mode_skips_success_and_keeps_failures() {
        let success = br#"{"timestamp":"2026-06-24T01:00:04.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-success","output":"Chunk ID: ok\nProcess exited with code 0\nOutput:\nunit tests passed\n"}}"#;
        let failure = br#"{"timestamp":"2026-06-24T01:00:04.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-failure","output":"Chunk ID: fail\nProcess exited with code 101\nOutput:\ntest failed\n"}}"#;
        let timeout = br#"{"timestamp":"2026-06-24T01:00:04.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-timeout","timed_out":true,"output":"timed out"}}"#;

        assert!(should_skip_codex_tool_output_line(
            success,
            CodexToolOutputMode::Failures
        ));
        assert!(!should_skip_codex_tool_output_line(
            failure,
            CodexToolOutputMode::Failures
        ));
        assert!(!should_skip_codex_tool_output_line(
            timeout,
            CodexToolOutputMode::Failures
        ));
        assert!(!should_skip_codex_tool_output_line(
            success,
            CodexToolOutputMode::Metadata
        ));
        assert!(should_skip_codex_tool_output_line(
            failure,
            CodexToolOutputMode::Skip
        ));
    }

    #[test]
    fn provider_fixture_replay_supports_claude_cursor_metadata() {
        let temp = tempdir();
        let fixture = provider_fixture("claude.jsonl");
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_provider_fixture_jsonl(
            &fixture,
            &mut store,
            fixed_import_options(fixture.clone()),
        )
        .unwrap();

        assert_eq!(summary.failed, 0);
        assert_eq!(summary.imported_sessions, 1);
        assert_eq!(summary.imported_events, 2);
        let session_id = provider_session_uuid(CaptureProvider::Claude, "claude-session-1");
        let events = store.events_for_session(session_id).unwrap();
        assert_eq!(events[1].event_type, EventType::Summary);
        assert_eq!(
            events[1].sync.metadata["cursor"].as_str(),
            Some("claude-cursor-1")
        );
        assert_eq!(events[1].payload["provider_event_index"].as_u64(), Some(1));
    }

    #[test]
    fn provider_fixture_replay_supports_opencode_fixture() {
        let temp = tempdir();
        let fixture = provider_fixture("opencode.jsonl");
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_provider_fixture_jsonl(
            &fixture,
            &mut store,
            fixed_import_options(fixture.clone()),
        )
        .unwrap();

        assert_eq!(summary.failed, 0);
        assert_eq!(summary.imported_sessions, 2);
        assert_eq!(summary.imported_events, 3);
        assert_eq!(summary.imported_edges, 1);
        let parent_id = provider_session_uuid(CaptureProvider::OpenCode, "opencode-session-1");
        let child_id = provider_session_uuid(CaptureProvider::OpenCode, "opencode-session-1-scout");
        let parent = store.get_session(parent_id).unwrap();
        let child = store.get_session(child_id).unwrap();
        assert_eq!(parent.provider, CaptureProvider::OpenCode);
        assert_eq!(child.parent_session_id, Some(parent_id));
        assert_eq!(child.agent_type, AgentType::Subagent);
        assert_eq!(store.events_for_session(parent_id).unwrap().len(), 2);
        assert_eq!(store.events_for_session(child_id).unwrap().len(), 1);
    }

    #[test]
    fn provider_fixture_replay_supports_antigravity_gemini_and_cursor() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let antigravity = provider_fixture("antigravity.jsonl");
        let antigravity_summary = import_provider_fixture_jsonl(
            &antigravity,
            &mut store,
            fixed_import_options(antigravity.clone()),
        )
        .unwrap();
        assert_eq!(antigravity_summary.failed, 0);
        assert_eq!(antigravity_summary.imported_sessions, 2);
        assert_eq!(antigravity_summary.imported_events, 3);
        assert_eq!(antigravity_summary.imported_edges, 1);
        let antigravity_parent =
            provider_session_uuid(CaptureProvider::Antigravity, "agy-session-1");
        let antigravity_child =
            provider_session_uuid(CaptureProvider::Antigravity, "agy-session-1-worker");
        assert_eq!(
            store
                .get_session(antigravity_child)
                .unwrap()
                .parent_session_id,
            Some(antigravity_parent)
        );

        let gemini = provider_fixture("gemini.jsonl");
        let gemini_summary = import_provider_fixture_jsonl(
            &gemini,
            &mut store,
            fixed_import_options(gemini.clone()),
        )
        .unwrap();
        assert_eq!(gemini_summary.failed, 0);
        assert_eq!(gemini_summary.imported_sessions, 1);
        assert_eq!(gemini_summary.imported_events, 2);
        let gemini_session = provider_session_uuid(CaptureProvider::Gemini, "gemini-session-1");
        let gemini_events = store.events_for_session(gemini_session).unwrap();
        assert_eq!(gemini_events[1].event_type, EventType::ToolOutput);
        assert_eq!(
            gemini_events[1].sync.metadata["metadata"]["telemetry_outfile"].as_str(),
            Some(".gemini/telemetry.log")
        );

        let cursor = provider_fixture("cursor.jsonl");
        let cursor_summary = import_provider_fixture_jsonl(
            &cursor,
            &mut store,
            fixed_import_options(cursor.clone()),
        )
        .unwrap();
        assert_eq!(cursor_summary.failed, 0);
        assert_eq!(cursor_summary.imported_sessions, 1);
        assert_eq!(cursor_summary.imported_events, 2);
        let cursor_session = provider_session_uuid(CaptureProvider::Cursor, "cursor-session-1");
        let cursor_events = store.events_for_session(cursor_session).unwrap();
        assert_eq!(cursor_events[1].event_type, EventType::ToolCall);
        assert_eq!(
            cursor_events[0].sync.metadata["metadata"]["docs_surface"].as_str(),
            Some("Cursor CLI sessions and stream-json output")
        );
    }

    #[test]
    fn fixture_only_provider_replay_is_idempotent_for_major_providers() {
        for (name, provider, external_session_id, sessions, events, edges) in [
            (
                "claude.jsonl",
                CaptureProvider::Claude,
                "claude-session-1",
                1,
                2,
                0,
            ),
            (
                "opencode.jsonl",
                CaptureProvider::OpenCode,
                "opencode-session-1",
                2,
                3,
                1,
            ),
            (
                "antigravity.jsonl",
                CaptureProvider::Antigravity,
                "agy-session-1",
                2,
                3,
                1,
            ),
            (
                "gemini.jsonl",
                CaptureProvider::Gemini,
                "gemini-session-1",
                1,
                2,
                0,
            ),
            (
                "cursor.jsonl",
                CaptureProvider::Cursor,
                "cursor-session-1",
                1,
                2,
                0,
            ),
        ] {
            let temp = tempdir();
            let fixture = provider_fixture(name);
            let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

            let first = import_provider_fixture_jsonl(
                &fixture,
                &mut store,
                fixed_import_options(fixture.clone()),
            )
            .unwrap();
            assert_eq!(first.failed, 0, "{name}: {:?}", first.failures);
            assert_eq!(first.imported_sessions, sessions, "{name}");
            assert_eq!(first.imported_events, events, "{name}");
            assert_eq!(first.imported_edges, edges, "{name}");

            let second = import_provider_fixture_jsonl(
                &fixture,
                &mut store,
                fixed_import_options(fixture.clone()),
            )
            .unwrap();
            assert_eq!(second.failed, 0, "{name}: {:?}", second.failures);
            assert_eq!(second.imported_sessions, 0, "{name}");
            assert_eq!(second.imported_events, 0, "{name}");
            assert_eq!(second.imported_edges, 0, "{name}");
            assert_eq!(second.skipped_sessions, sessions, "{name}");
            assert_eq!(second.skipped_events, events, "{name}");
            assert_eq!(second.skipped_edges, edges, "{name}");

            let session_id = provider_session_uuid(provider, external_session_id);
            assert!(!store.events_for_session(session_id).unwrap().is_empty());
        }
    }

    #[test]
    fn provider_fixture_replay_persists_cursor_checkpoint_and_source_contract_metadata() {
        let temp = tempdir();
        let fixture = provider_fixture("codex.jsonl");
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_provider_fixture_jsonl(
            &fixture,
            &mut store,
            fixed_import_options(fixture.clone()),
        )
        .unwrap();

        assert_eq!(summary.failed, 0);
        let cursor = store
            .get_sync_cursor(
                None,
                "test-machine",
                &provider_cursor_stream(
                    CaptureProvider::Codex,
                    "normalized_provider_fixture_jsonl",
                ),
            )
            .unwrap()
            .unwrap();
        assert_eq!(cursor.cursor, "codex-sub-cursor-0");

        let source = store
            .capture_source_by_external_session(CaptureProvider::Codex, "codex-session-1")
            .unwrap()
            .unwrap();
        assert_eq!(
            source.sync.metadata["source_format"].as_str(),
            Some("normalized_provider_fixture_jsonl")
        );
        assert_eq!(
            source.sync.metadata["source_trust"].as_str(),
            Some("fixture")
        );
        assert_eq!(
            source.sync.metadata["raw_retention"].as_str(),
            Some("path_reference")
        );
        assert_eq!(
            source.sync.metadata["redaction_boundary"].as_str(),
            Some("before_export")
        );
        assert!(source.sync.metadata["source_idempotency_key"]
            .as_str()
            .is_some());
    }

    #[test]
    fn codex_history_import_is_prompt_only_summary_fidelity_and_idempotent() {
        let temp = tempdir();
        let fixture = provider_history_fixture("codex-history.jsonl");
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let first = import_codex_history_jsonl(
            &fixture,
            &mut store,
            CodexHistoryImportOptions {
                source_path: Some(fixture.clone()),
                imported_at: "2026-06-23T15:30:00Z".parse().unwrap(),
                ..CodexHistoryImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(first.failed, 0, "{:?}", first.failures);
        assert_eq!(first.imported_sessions, 2);
        assert_eq!(first.imported_events, 3);
        assert_eq!(first.imported_edges, 0);
        assert!(!store.event_search_projection_needs_backfill().unwrap());

        let second = import_codex_history_jsonl(
            &fixture,
            &mut store,
            CodexHistoryImportOptions {
                source_path: Some(fixture.clone()),
                imported_at: "2026-06-23T15:30:00Z".parse().unwrap(),
                ..CodexHistoryImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(second.failed, 0);
        assert_eq!(second.imported_events, 0);
        assert_eq!(second.skipped_events, 3);

        let session_id = provider_session_uuid(CaptureProvider::Codex, "codex-history-session-1");
        let session = store.get_session(session_id).unwrap();
        assert_eq!(session.sync.fidelity, Fidelity::SummaryOnly);
        assert_eq!(
            session.sync.metadata["source_format"].as_str(),
            Some("codex_history_jsonl")
        );
        assert_eq!(
            session.sync.metadata["metadata"]["source_fidelity"].as_str(),
            Some("prompt_log_only")
        );
        let events = store.events_for_session(session_id).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].sync.fidelity, Fidelity::SummaryOnly);
        assert_eq!(events[0].role, Some(EventRole::User));
        assert_eq!(events[0].event_type, EventType::Message);
        assert_eq!(
            events[0].sync.metadata["source_format"].as_str(),
            Some("codex_history_jsonl")
        );
        let cursor = store
            .get_sync_cursor(
                None,
                &CodexHistoryImportOptions::default().machine_id,
                &provider_cursor_stream(CaptureProvider::Codex, "codex_history_jsonl"),
            )
            .unwrap()
            .unwrap();
        assert_eq!(cursor.cursor, "line:3");
    }

    #[test]
    fn provider_fixture_replay_rejects_malformed_lines_without_partial_import_by_default() {
        let temp = tempdir();
        let fixture = provider_fixture("malformed-partial.jsonl");
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_provider_fixture_jsonl(
            &fixture,
            &mut store,
            fixed_import_options(fixture.clone()),
        )
        .unwrap();

        assert_eq!(summary.imported_sessions, 0);
        assert_eq!(summary.imported_events, 0);
        assert_eq!(summary.failed, 1);
        let session_id = provider_session_uuid(CaptureProvider::Codex, "malformed-partial-session");
        assert!(store.events_for_session(session_id).unwrap().is_empty());
    }

    #[test]
    fn provider_fixture_replay_allows_explicit_partial_import() {
        let temp = tempdir();
        let fixture = provider_fixture("malformed-partial.jsonl");
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let mut options = fixed_import_options(fixture.clone());
        options.allow_partial_failures = true;

        let summary = import_provider_fixture_jsonl(&fixture, &mut store, options).unwrap();

        assert_eq!(summary.imported_sessions, 1);
        assert_eq!(summary.imported_events, 2);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.failures.len(), 1);
        assert_eq!(summary.failures[0].line, 3);
        let session_id = provider_session_uuid(CaptureProvider::Codex, "malformed-partial-session");
        let events = store.events_for_session(session_id).unwrap();
        assert_eq!(events.len(), 2);
        assert!(events[0]
            .payload
            .to_string()
            .contains("Valid event before malformed line."));
        assert!(events[1]
            .payload
            .to_string()
            .contains("Valid event after malformed line."));
    }

    #[test]
    fn provider_fixture_replay_rejects_expected_provider_mismatch() {
        let temp = tempdir();
        let fixture = provider_fixture("claude.jsonl");
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let mut options = fixed_import_options(fixture.clone());
        options.expected_provider = Some(CaptureProvider::Codex);

        let summary = import_provider_fixture_jsonl(fixture, &mut store, options).unwrap();

        assert_eq!(summary.imported, 0);
        assert_eq!(summary.failed, 2);
        assert!(summary.failures.iter().all(|failure| failure
            .error
            .contains("has provider `claude` but expected `codex`")));
    }
}
