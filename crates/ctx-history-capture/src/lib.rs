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
use ctx_history_core::{
    inbox_dir as core_inbox_dir, new_id, redact_share_safe_markers, AgentType, CaptureEnvelope,
    CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, Confidence,
    EntityTimestamps, Event, EventRole, EventType, Fidelity, HistoryRecord,
    ProviderCaptureEnvelope, ProviderCursorCheckpoint, ProviderCursorRange, ProviderEventEnvelope,
    ProviderRawRetention, ProviderRedactionBoundary, ProviderSessionEnvelope,
    ProviderSourceEnvelope, ProviderSourceTrust, RedactionState, Run, RunStatus, RunType, Session,
    SessionEdge, SessionEdgeType, SessionHistoryArchive, SessionStatus, SyncCursor, SyncMetadata,
    SyncState, Visibility, PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use ctx_history_store::{CatalogSession, Store, StoreError};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use uuid::Uuid;

pub mod provider_sources;
pub use provider_sources::{
    discover_provider_sources, discover_provider_sources_for_provider, provider_source_for_path,
    provider_source_spec, provider_source_specs, ProviderCatalogSupport, ProviderDefaultLocation,
    ProviderImportSupport, ProviderSource, ProviderSourceKind, ProviderSourceSpec,
    ProviderSourceStatus,
};

pub const CAPTURE_SCHEMA_VERSION: u32 = 1;
#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("store error: {0}")]
    Store(#[from] ctx_history_store::StoreError),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
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
    pub history_record_id: Option<Uuid>,
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
            history_record_id: None,
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
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for CodexHistoryImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: Utc::now(),
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
            imported_at: Utc::now(),
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
    pub cached_sessions: usize,
    pub parsed_sessions: usize,
    pub skipped_sessions: usize,
    pub failed_sessions: usize,
}

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
            imported_at: Utc::now(),
            history_record_id: None,
            allow_partial_failures: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClaudeProjectsImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for ClaudeProjectsImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: Utc::now(),
            history_record_id: None,
            allow_partial_failures: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OpenCodeSqliteImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for OpenCodeSqliteImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: Utc::now(),
            history_record_id: None,
            allow_partial_failures: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AntigravityCliImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for AntigravityCliImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: Utc::now(),
            history_record_id: None,
            allow_partial_failures: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GeminiCliImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for GeminiCliImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: Utc::now(),
            history_record_id: None,
            allow_partial_failures: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FactoryAiDroidImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for FactoryAiDroidImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: Utc::now(),
            history_record_id: None,
            allow_partial_failures: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CopilotCliImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for CopilotCliImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: Utc::now(),
            history_record_id: None,
            allow_partial_failures: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CursorNativeImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for CursorNativeImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: Utc::now(),
            history_record_id: None,
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
    pub event_mode: CodexEventImportMode,
    pub include_notices: bool,
}

impl Default for ProviderAdapterContext {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: Utc::now(),
            tool_output_mode: CodexToolOutputMode::Full,
            event_mode: CodexEventImportMode::Rich,
            include_notices: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NormalizedProviderImportOptions {
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
    pub persist_cursors: bool,
    pub wrap_transaction: bool,
    pub fast_event_inserts: bool,
}

impl Default for NormalizedProviderImportOptions {
    fn default() -> Self {
        Self {
            history_record_id: None,
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

#[derive(Debug, Clone, Copy, Default)]
pub struct ClaudeProjectsJsonlAdapter;

#[derive(Debug, Clone, Copy, Default)]
pub struct OpenCodeSqliteAdapter;

#[derive(Debug, Clone, Copy, Default)]
pub struct AntigravityCliJsonlAdapter;

#[derive(Debug, Clone, Copy, Default)]
pub struct GeminiCliJsonlAdapter;

#[derive(Debug, Clone, Copy, Default)]
pub struct CursorAgentTranscriptJsonlAdapter;

#[derive(Debug, Clone, Copy, Default)]
pub struct FactoryAiDroidJsonlAdapter;

#[derive(Debug, Clone, Copy, Default)]
pub struct CopilotCliSessionEventsAdapter;

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
                context.event_mode,
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

fn should_parse_codex_session_line(line: &[u8], event_mode: CodexEventImportMode) -> bool {
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

impl ProviderCaptureAdapter for ClaudeProjectsJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Claude
    }

    fn source_format(&self) -> &str {
        CLAUDE_PROJECTS_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        let mut paths = Vec::new();
        collect_jsonl_paths(path, &mut paths)?;
        paths.sort();
        if paths.is_empty() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "no Claude Code project JSONL transcripts found",
            });
        }

        let mut merged = ProviderNormalizationResult::default();
        for path in paths {
            let mut result = normalize_claude_projects_jsonl_file(&path, context)?;
            merged.summary.merge(result.summary);
            merged.captures.append(&mut result.captures);
        }
        Ok(merged)
    }
}

impl ProviderCaptureAdapter for OpenCodeSqliteAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::OpenCode
    }

    fn source_format(&self) -> &str {
        OPENCODE_SQLITE_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        ensure_regular_provider_transcript_file(path)?;
        normalize_opencode_sqlite(path, context)
    }
}

impl ProviderCaptureAdapter for AntigravityCliJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Antigravity
    }

    fn source_format(&self) -> &str {
        ANTIGRAVITY_CLI_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_jsonl_tree(
            path,
            context,
            CaptureProvider::Antigravity,
            ANTIGRAVITY_CLI_SOURCE_FORMAT,
        )
    }
}

impl ProviderCaptureAdapter for GeminiCliJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Gemini
    }

    fn source_format(&self) -> &str {
        GEMINI_CLI_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_jsonl_tree(
            path,
            context,
            CaptureProvider::Gemini,
            GEMINI_CLI_SOURCE_FORMAT,
        )
    }
}

impl ProviderCaptureAdapter for CursorAgentTranscriptJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Cursor
    }

    fn source_format(&self) -> &str {
        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_jsonl_tree(
            path,
            context,
            CaptureProvider::Cursor,
            CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
        )
    }
}

impl ProviderCaptureAdapter for FactoryAiDroidJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::FactoryAiDroid
    }

    fn source_format(&self) -> &str {
        FACTORY_DROID_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_jsonl_tree(
            path,
            context,
            CaptureProvider::FactoryAiDroid,
            FACTORY_DROID_SOURCE_FORMAT,
        )
    }
}

impl ProviderCaptureAdapter for CopilotCliSessionEventsAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::CopilotCli
    }

    fn source_format(&self) -> &str {
        COPILOT_CLI_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_jsonl_tree(
            path,
            context,
            CaptureProvider::CopilotCli,
            COPILOT_CLI_SOURCE_FORMAT,
        )
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
        "kind": "history_record",
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
            event_mode: options.event_mode,
            include_notices: options.include_notices,
        },
    )?;

    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: false,
            wrap_transaction: true,
            fast_event_inserts: options.fast_event_inserts,
        },
    )
}

pub fn import_codex_session_jsonl_tail(
    path: impl AsRef<Path>,
    start_offset: u64,
    store: &mut Store,
    options: CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    if start_offset == 0 {
        return import_codex_session_jsonl(path, store, options);
    }
    ensure_regular_provider_transcript_file(path)?;
    let total_bytes = fs::metadata(path)?.len();
    if start_offset >= total_bytes {
        return Ok(ProviderImportSummary::default());
    }

    let mut summary = ProviderImportSummary::default();
    let mut caches = ProviderImportCaches::default();
    let context = ProviderAdapterContext {
        machine_id: options.machine_id.clone(),
        source_path: Some(path.to_path_buf()),
        imported_at: options.imported_at,
        tool_output_mode: options.tool_output_mode,
        event_mode: options.event_mode,
        include_notices: options.include_notices,
    };
    let import_options = NormalizedProviderImportOptions {
        history_record_id: options.history_record_id,
        allow_partial_failures: options.allow_partial_failures,
        persist_cursors: false,
        wrap_transaction: false,
        fast_event_inserts: true,
    };

    report_codex_import_progress(
        &options,
        1,
        total_bytes - start_offset,
        0,
        0,
        &summary,
        false,
    );

    let mut began_transaction = false;
    let import = (|| -> Result<ProviderImportSummary> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let mut line_number = 0usize;
        let mut position = 0u64;

        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            return Ok(summary);
        }
        line_number += 1;
        position = position.saturating_add(read as u64);
        let header_value: Value = serde_json::from_slice(&line)?;
        let header = codex_session_header(header_value)?;

        while position < start_offset {
            line.clear();
            let read = reader.read_until(b'\n', &mut line)?;
            if read == 0 {
                return Ok(summary);
            }
            line_number += 1;
            position = position.saturating_add(read as u64);
        }

        store.begin_immediate_batch()?;
        began_transaction = true;
        let header_capture =
            codex_session_capture(&header, None, line_number, header.timestamp, &context);
        summary.merge(import_provider_capture_line(
            store,
            &header_capture,
            &import_options,
            line_number,
            &mut caches,
        )?);

        let mut call_contexts: BTreeMap<String, CodexToolCallContext> = BTreeMap::new();
        let mut completed_bytes = 0u64;
        loop {
            line.clear();
            let read = reader.read_until(b'\n', &mut line)?;
            if read == 0 {
                break;
            }
            line_number += 1;
            completed_bytes = completed_bytes.saturating_add(read as u64);
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            if !should_parse_codex_session_line(&line, options.event_mode) {
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
                        return Ok(summary);
                    }
                    continue;
                }
            };
            if value
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|entry_type| entry_type == "session_meta")
            {
                continue;
            }
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
                options.event_mode,
            ) else {
                continue;
            };
            if !options.include_notices && event.event_type == EventType::Notice {
                summary.skipped += 1;
                summary.skipped_events += 1;
                continue;
            }
            summary.merge(import_codex_provider_event_fast(
                store,
                &header,
                &event,
                options.history_record_id,
                line_number,
                context.imported_at,
            )?);
            report_codex_import_progress(
                &options,
                1,
                total_bytes - start_offset,
                0,
                completed_bytes,
                &summary,
                false,
            );
        }

        resolve_pending_provider_edges(store, &mut summary, &mut caches)?;
        Ok(summary)
    })();

    match import {
        Ok(summary) => {
            if began_transaction {
                store.commit_batch()?;
            }
            report_codex_import_progress(
                &options,
                1,
                total_bytes - start_offset,
                1,
                total_bytes - start_offset,
                &summary,
                true,
            );
            Ok(summary)
        }
        Err(err) => {
            if began_transaction {
                let _ = store.rollback_batch();
            }
            Err(err)
        }
    }
}

pub fn import_codex_session_paths(
    paths: Vec<PathBuf>,
    store: &mut Store,
    options: CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    for path in &paths {
        ensure_regular_provider_transcript_file(path)?;
    }
    if options.fast_event_inserts && paths.len() <= 1 {
        return import_codex_session_paths_fast(paths, store, options, 0);
    }

    import_codex_session_paths_parallel_normalized(paths, store, options, 0)
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
    if options.fast_event_inserts && paths.len() <= 1 {
        return import_codex_session_paths_fast(paths, store, options, skipped_by_bounds);
    }

    import_codex_session_paths_parallel_normalized(paths, store, options, skipped_by_bounds)
}

fn import_codex_session_paths_parallel_normalized(
    paths: Vec<PathBuf>,
    store: &mut Store,
    options: CodexSessionImportOptions,
    skipped_by_bounds: usize,
) -> Result<ProviderImportSummary> {
    let mut merged = ProviderImportSummary::default();
    merged.skipped_sessions += skipped_by_bounds;
    merged.skipped += skipped_by_bounds;
    let mut in_transaction = false;
    if !paths.is_empty() {
        store.begin_immediate_batch()?;
        in_transaction = true;
    }
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
        &merged,
        false,
    );

    let parallelism = import_parallelism(paths.len());
    let chunk_size = parallelism.saturating_mul(8).max(16);
    for chunk in paths.chunks(chunk_size) {
        let normalized = match normalize_codex_session_paths_parallel(chunk, &options, parallelism)
        {
            Ok(normalized) => normalized,
            Err(err) => {
                if in_transaction {
                    let _ = store.rollback_batch();
                }
                return Err(err);
            }
        };
        let mut chunk_summary = ProviderImportSummary::default();
        let mut chunk_captures = Vec::new();
        let mut chunk_bytes = 0u64;
        for (_, path, normalization) in normalized {
            chunk_bytes = chunk_bytes.saturating_add(
                fs::metadata(&path)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0),
            );
            chunk_summary.merge(normalization.summary);
            chunk_captures.extend(normalization.captures);
        }
        let summary = match import_provider_capture_lines(
            store,
            NormalizedProviderImportOptions {
                history_record_id: options.history_record_id,
                allow_partial_failures: options.allow_partial_failures,
                persist_cursors: false,
                wrap_transaction: false,
                fast_event_inserts: options.fast_event_inserts,
            },
            chunk_summary,
            chunk_captures,
        ) {
            Ok(summary) => summary,
            Err(err) => {
                if in_transaction {
                    let _ = store.rollback_batch();
                }
                return Err(err);
            }
        };
        merged.merge(summary);
        completed_files += chunk.len();
        completed_bytes = completed_bytes.saturating_add(chunk_bytes);
        report_codex_import_progress(
            &options,
            total_files,
            total_bytes,
            completed_files,
            completed_bytes,
            &merged,
            false,
        );
    }
    if in_transaction {
        store.commit_batch()?;
    }
    store.checkpoint_wal_passive_if_larger_than(CODEX_FAST_IMPORT_PASSIVE_CHECKPOINT_MIN_BYTES)?;
    report_codex_import_progress(
        &options,
        total_files,
        total_bytes,
        completed_files,
        completed_bytes,
        &merged,
        true,
    );
    Ok(merged)
}

fn normalize_codex_session_paths_parallel(
    paths: &[PathBuf],
    options: &CodexSessionImportOptions,
    parallelism: usize,
) -> Result<Vec<(usize, PathBuf, ProviderNormalizationResult)>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    if parallelism <= 1 || paths.len() == 1 {
        let mut normalized = Vec::with_capacity(paths.len());
        for (index, path) in paths.iter().enumerate() {
            normalized.push((
                index,
                path.clone(),
                normalize_codex_session_path(path, options)?,
            ));
        }
        return Ok(normalized);
    }

    let chunk_size = paths.len().div_ceil(parallelism).max(1);
    let mut batches = thread::scope(|scope| {
        let mut handles = Vec::new();
        for (chunk_index, chunk) in paths.chunks(chunk_size).enumerate() {
            let chunk = chunk.to_vec();
            handles.push(scope.spawn(move || {
                let mut normalized = Vec::with_capacity(chunk.len());
                let base_index = chunk_index * chunk_size;
                for (offset, path) in chunk.iter().enumerate() {
                    normalized.push((
                        base_index + offset,
                        path.clone(),
                        normalize_codex_session_path(path, options)?,
                    ));
                }
                Result::<Vec<_>>::Ok(normalized)
            }));
        }
        let mut batches = Vec::with_capacity(handles.len());
        for handle in handles {
            batches.push(handle.join().map_err(|_| {
                CaptureError::InvalidPayload("Codex import worker panicked".into())
            })??);
        }
        Result::<Vec<_>>::Ok(batches)
    })?;
    let total = batches.iter().map(Vec::len).sum();
    let mut normalized = Vec::with_capacity(total);
    for batch in batches.drain(..) {
        normalized.extend(batch);
    }
    normalized.sort_by_key(|(index, _, _)| *index);
    Ok(normalized)
}

fn normalize_codex_session_path(
    path: &Path,
    options: &CodexSessionImportOptions,
) -> Result<ProviderNormalizationResult> {
    CodexSessionJsonlAdapter.normalize_path(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id.clone(),
            source_path: Some(path.to_path_buf()),
            imported_at: options.imported_at,
            tool_output_mode: options.tool_output_mode,
            event_mode: options.event_mode,
            include_notices: options.include_notices,
        },
    )
}

fn import_parallelism(path_count: usize) -> usize {
    if path_count <= 1 {
        return 1;
    }
    thread::available_parallelism()
        .ok()
        .map(usize::from)
        .unwrap_or(1)
        .min(path_count)
        .min(8)
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
        event_mode: options.event_mode,
        include_notices: options.include_notices,
    };
    let import_options = NormalizedProviderImportOptions {
        history_record_id: options.history_record_id,
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
        if !should_parse_codex_session_line(&line, options.event_mode) {
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
            options.event_mode,
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
            options.history_record_id,
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
    history_record_id: Option<Uuid>,
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
        history_record_id,
        event,
        payload: &payload,
        event_hash: &event_hash,
    });
    let normalized_event = Event {
        id: provider_event_uuid(provider, &header.id, event.provider_event_index),
        seq: provider_event_seq(provider, &header.id, event.provider_event_index),
        history_record_id,
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
    let existing = store
        .list_catalog_sessions_for_source(CaptureProvider::Codex, &source_root)?
        .into_iter()
        .map(|session| (session.source_path.clone(), session))
        .collect::<BTreeMap<_, _>>();
    let mut current_paths = Vec::with_capacity(paths.len());
    let mut cached_sessions = Vec::new();
    let mut paths_to_parse = Vec::new();
    let mut metadata_failures = Vec::new();
    for path in paths {
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(err) => {
                summary.failed_sessions += 1;
                metadata_failures.push(format!("{}: {err}", path.display()));
                continue;
            }
        };
        summary.source_files += 1;
        summary.source_bytes = summary.source_bytes.saturating_add(metadata.len());
        let source_path = path.display().to_string();
        current_paths.push(source_path.clone());
        if let Some(session) = cached_catalog_session_if_unchanged(
            existing.get(&source_path),
            &metadata,
            cataloged_at_ms,
        ) {
            summary.cached_sessions += 1;
            cached_sessions.push(session);
        } else {
            paths_to_parse.push(path);
        }
    }
    if !options.allow_partial_failures && !metadata_failures.is_empty() {
        return Err(CaptureError::InvalidPayload(format!(
            "catalog failed: {}",
            metadata_failures.remove(0)
        )));
    }
    let stale_session_count =
        store.catalog_source_stale_session_count(CaptureProvider::Codex, &source_root)?;
    let current_path_set = current_paths.iter().cloned().collect::<BTreeSet<_>>();
    let has_missing_existing_paths = existing
        .keys()
        .any(|source_path| !current_path_set.contains(source_path));
    if paths_to_parse.is_empty()
        && metadata_failures.is_empty()
        && cached_sessions.len() == current_paths.len()
        && existing.len() == current_paths.len()
        && !has_missing_existing_paths
        && stale_session_count == 0
    {
        summary.cataloged_sessions = cached_sessions.len();
        return Ok(summary);
    }
    let (scan_summary, sessions) = catalog_codex_session_paths(
        paths_to_parse,
        &source_root,
        cataloged_at_ms,
        options.allow_partial_failures,
        options.parallelism,
    )?;
    summary.failed_sessions += scan_summary.failed_sessions;
    summary.parsed_sessions += scan_summary.parsed_sessions;
    let parsed_session_count = sessions.len();
    let cached_session_count = cached_sessions.len();
    let mut sessions_to_persist = sessions;
    if stale_session_count > 0 {
        sessions_to_persist.extend(cached_sessions);
    }
    summary.cataloged_sessions = parsed_session_count.saturating_add(cached_session_count);

    store.begin_immediate_batch()?;
    let persist = (|| -> Result<()> {
        if !sessions_to_persist.is_empty() {
            store.upsert_catalog_sessions(&sessions_to_persist)?;
        }
        if stale_session_count > 0 || has_missing_existing_paths {
            store.mark_catalog_source_missing_paths_stale(
                CaptureProvider::Codex,
                &source_root,
                &current_paths,
                cataloged_at_ms,
            )?;
        }
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

fn cached_catalog_session_if_unchanged(
    session: Option<&CatalogSession>,
    metadata: &fs::Metadata,
    cataloged_at_ms: i64,
) -> Option<CatalogSession> {
    let session = session?;
    let modified_at_ms = system_time_ms(metadata.modified().unwrap_or(UNIX_EPOCH));
    if session.provider == CaptureProvider::Codex
        && session.source_format == CODEX_SESSION_SOURCE_FORMAT
        && session.file_size_bytes == metadata.len()
        && session.file_modified_at_ms == modified_at_ms
    {
        let mut session = session.clone();
        session.cataloged_at_ms = cataloged_at_ms;
        Some(session)
    } else {
        None
    }
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
        summary.parsed_sessions += batch.summary.parsed_sessions;
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
    let mut batch = CatalogWorkerBatch {
        sessions: Vec::with_capacity(paths.len()),
        ..CatalogWorkerBatch::default()
    };
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
            Ok(session) => {
                batch.summary.parsed_sessions += 1;
                batch.sessions.push(session);
            }
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

pub fn import_claude_projects_jsonl_tree(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: ClaudeProjectsImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = ClaudeProjectsJsonlAdapter.normalize_path(
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

pub fn import_opencode_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: OpenCodeSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = OpenCodeSqliteAdapter.normalize_path(
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

pub fn import_antigravity_cli_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: AntigravityCliImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
        },
        AntigravityCliJsonlAdapter,
    )
}

pub fn import_gemini_cli_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: GeminiCliImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
        },
        GeminiCliJsonlAdapter,
    )
}

pub fn import_cursor_native_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CursorNativeImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
        },
        CursorAgentTranscriptJsonlAdapter,
    )
}

pub fn import_factory_ai_droid_sessions(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: FactoryAiDroidImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
        },
        FactoryAiDroidJsonlAdapter,
    )
}

pub fn import_copilot_cli_session_events(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CopilotCliImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
        },
        CopilotCliSessionEventsAdapter,
    )
}

struct NativeJsonlTreeImport<'a> {
    path: &'a Path,
    machine_id: String,
    source_path: Option<PathBuf>,
    imported_at: DateTime<Utc>,
    history_record_id: Option<Uuid>,
    allow_partial_failures: bool,
}

fn import_native_jsonl_tree<A: ProviderCaptureAdapter>(
    store: &mut Store,
    request: NativeJsonlTreeImport<'_>,
    adapter: A,
) -> Result<ProviderImportSummary> {
    let source_path = request
        .source_path
        .unwrap_or_else(|| request.path.to_path_buf());
    let normalization = adapter.normalize_path(
        request.path,
        &ProviderAdapterContext {
            machine_id: request.machine_id,
            source_path: Some(source_path),
            imported_at: request.imported_at,
            tool_output_mode: CodexToolOutputMode::Full,
            event_mode: CodexEventImportMode::Rich,
            include_notices: true,
        },
    )?;
    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            history_record_id: request.history_record_id,
            allow_partial_failures: request.allow_partial_failures,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
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
const CLAUDE_PROJECTS_SOURCE_FORMAT: &str = "claude_projects_jsonl_tree";
const OPENCODE_SQLITE_SOURCE_FORMAT: &str = "opencode_sqlite";
const ANTIGRAVITY_CLI_SOURCE_FORMAT: &str = "antigravity_cli_transcript_jsonl_tree";
const GEMINI_CLI_SOURCE_FORMAT: &str = "gemini_cli_chat_recording_jsonl";
const CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT: &str = "cursor_agent_transcript_jsonl";
const FACTORY_DROID_SOURCE_FORMAT: &str = "factory_ai_droid_sessions_jsonl";
const COPILOT_CLI_SOURCE_FORMAT: &str = "copilot_cli_session_events_jsonl";
const CODEX_MAX_TEXT_CHARS: usize = 16_000;
const CODEX_MAX_METADATA_TEXT_CHARS: usize = 4_000;
const CODEX_MAX_OUTPUT_PREVIEW_CHARS: usize = 4_000;
const PROVIDER_MAX_TEXT_CHARS: usize = 16_000;
const PROVIDER_MAX_PREVIEW_CHARS: usize = 4_000;
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
                "import_profile": match context.event_mode {
                    CodexEventImportMode::Search => "search",
                    CodexEventImportMode::Rich => "rich",
                },
                "limitations": [
                    "search profile indexes session metadata, user and assistant messages, compacted context summaries, and parent-child session edges where present",
                    "rich profile can additionally index tool call previews, command output previews, reasoning summaries, and lifecycle notices",
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
    event_mode: CodexEventImportMode,
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
                event_mode,
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
            if event_mode == CodexEventImportMode::Search {
                return None;
            }
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
    event_mode: CodexEventImportMode,
) -> Option<ProviderEventEnvelope> {
    let item_type = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    match item_type {
        "message" => codex_message_event(payload, line_number, occurred_at),
        _ if event_mode == CodexEventImportMode::Search => None,
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

fn provider_safe_preview(value: &str, max_chars: usize) -> (String, bool) {
    let redacted = redact_share_safe_markers(value);
    capped_text(&redacted, max_chars)
}

fn provider_value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                if let Some(text) = block
                    .get("text")
                    .or_else(|| block.get("content"))
                    .or_else(|| block.get("output"))
                    .or_else(|| block.get("summary"))
                    .and_then(Value::as_str)
                {
                    parts.push(text.to_owned());
                    continue;
                }
                if let Some(kind) = block.get("type").and_then(Value::as_str) {
                    if matches!(
                        kind,
                        "tool_use" | "tool" | "toolCall" | "function_call" | "agent"
                    ) {
                        let name = block
                            .get("name")
                            .or_else(|| block.get("tool"))
                            .and_then(Value::as_str)
                            .unwrap_or("tool");
                        parts.push(format!("tool call: {name}"));
                    } else if kind == "tool_result" {
                        parts.push("tool result".to_owned());
                    }
                }
            }
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Value::Object(_) => serde_json::to_string(value).ok(),
        Value::Number(_) | Value::Bool(_) => Some(value.to_string()),
        Value::Null => None,
    }
}

fn provider_role(value: Option<&str>) -> EventRole {
    match value {
        Some("user") => EventRole::User,
        Some("assistant") => EventRole::Assistant,
        Some("system" | "developer") => EventRole::System,
        Some("tool" | "toolResult" | "bashExecution") => EventRole::Tool,
        _ => EventRole::Unknown,
    }
}

fn normalize_claude_projects_jsonl_file(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    ensure_regular_provider_transcript_file(path)?;
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut result = ProviderNormalizationResult::default();
    let mut rows = Vec::new();

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
                    error: format!("malformed JSONL: {err}"),
                });
                continue;
            }
        };
        let timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_utc)
            .unwrap_or(context.imported_at);
        rows.push((line_number, value, timestamp));
    }
    if rows.is_empty() {
        return Ok(result);
    }

    let first = &rows[0].1;
    let file_stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown-session");
    let native_session_id = first
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .unwrap_or(file_stem)
        .to_owned();
    let (provider_session_id, parent_provider_session_id, external_agent_id, is_subagent) =
        claude_path_session_ids(path, &native_session_id);
    let started_at = rows
        .iter()
        .map(|(_, _, timestamp)| *timestamp)
        .min()
        .unwrap_or(context.imported_at);
    let cwd = first
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.trim().is_empty())
        .map(str::to_owned);
    let version = first
        .get("version")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let git_branch = first
        .get("gitBranch")
        .and_then(Value::as_str)
        .map(str::to_owned);

    for (line_number, value, occurred_at) in rows {
        let event = claude_event(&value, line_number, occurred_at);
        result.captures.push((
            line_number,
            ProviderCaptureEnvelope {
                schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
                provider: CaptureProvider::Claude,
                source: ProviderSourceEnvelope {
                    source_format: CLAUDE_PROJECTS_SOURCE_FORMAT.to_owned(),
                    machine_id: context.machine_id.clone(),
                    observed_at: context.imported_at,
                    raw_source_path: Some(path.display().to_string()),
                    raw_retention: ProviderRawRetention::PathReference,
                    redaction_boundary: ProviderRedactionBoundary::BeforeExport,
                    trust: ProviderSourceTrust::ProviderNative,
                    fidelity: Fidelity::Imported,
                    cursor: Some(ProviderCursorRange {
                        before: None,
                        after: Some(ProviderCursorCheckpoint {
                            stream: provider_cursor_stream(
                                CaptureProvider::Claude,
                                CLAUDE_PROJECTS_SOURCE_FORMAT,
                            ),
                            cursor: format!("{}:line:{line_number}", path.display()),
                            observed_at: occurred_at,
                        }),
                    }),
                    idempotency_key: Some(format!(
                        "provider-source:claude:{CLAUDE_PROJECTS_SOURCE_FORMAT}:{provider_session_id}"
                    )),
                    metadata: json!({
                        "adapter": CLAUDE_PROJECTS_SOURCE_FORMAT,
                        "native_session_id": native_session_id,
                        "source_path": path.display().to_string(),
                    }),
                },
                session: ProviderSessionEnvelope {
                    provider_session_id: provider_session_id.clone(),
                    parent_provider_session_id: parent_provider_session_id.clone(),
                    root_provider_session_id: parent_provider_session_id.clone(),
                    external_agent_id: external_agent_id.clone(),
                    agent_type: if is_subagent {
                        AgentType::Subagent
                    } else {
                        AgentType::Primary
                    },
                    role_hint: Some(if is_subagent { "subagent" } else { "primary" }.to_owned()),
                    is_primary: !is_subagent,
                    status: SessionStatus::Imported,
                    started_at,
                    ended_at: None,
                    cwd: cwd.clone(),
                    fidelity: Fidelity::Imported,
                    idempotency_key: Some(format!("provider-session:claude:{provider_session_id}")),
                    artifacts: Vec::new(),
                    metadata: json!({
                        "source_format": CLAUDE_PROJECTS_SOURCE_FORMAT,
                        "native_session_id": native_session_id,
                        "version": version,
                        "git_branch": git_branch,
                        "source_path": path.display().to_string(),
                        "limitations": [
                            "binary attachments are referenced by native payload metadata but not expanded",
                            "previews are capped and redacted before export"
                        ],
                    }),
                },
                event,
            },
        ));
    }

    Ok(result)
}

fn claude_path_session_ids(
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

fn claude_event(
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
    let (text, truncated) = provider_safe_preview(&text, PROVIDER_MAX_TEXT_CHARS);

    Some(ProviderEventEnvelope {
        provider_event_index: (line_number - 1) as u64,
        provider_event_hash: value.get("uuid").and_then(Value::as_str).map(str::to_owned),
        cursor: value.get("uuid").and_then(Value::as_str).map(str::to_owned),
        event_type,
        role,
        occurred_at,
        fidelity: Fidelity::Imported,
        redaction_state: RedactionState::SafePreview,
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
            "text": text,
            "truncated": truncated,
            "content_preview": provider_capped_json(content, PROVIDER_MAX_PREVIEW_CHARS),
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
            "tool_use_result": value.get("toolUseResult").cloned(),
        }),
    })
}

fn claude_event_type(entry_type: &str, message: &Value) -> EventType {
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

fn claude_content_has_type(content: Option<&Value>, expected: &str) -> bool {
    content
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some(expected))
        })
        .unwrap_or(false)
}

fn provider_capped_json(value: &Value, max_chars: usize) -> Value {
    match value {
        Value::Null => Value::Null,
        Value::String(text) => {
            let (text, truncated) = provider_safe_preview(text, max_chars);
            json!({ "text": text, "truncated": truncated })
        }
        _ => {
            let rendered = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
            let (json_text, truncated) = provider_safe_preview(&rendered, max_chars);
            json!({ "json": json_text, "truncated": truncated })
        }
    }
}

fn provider_redacted_json_value(value: &Value, max_string_chars: usize) -> Value {
    match value {
        Value::String(text) => {
            let (text, truncated) = provider_safe_preview(text, max_string_chars);
            if truncated {
                json!({ "text": text, "truncated": true })
            } else {
                Value::String(text)
            }
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| provider_redacted_json_value(item, max_string_chars))
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        provider_redacted_json_value(value, max_string_chars),
                    )
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn antigravity_tool_call_text(value: &Value) -> Option<String> {
    value.as_array().and_then(|calls| {
        let names: Vec<&str> = calls
            .iter()
            .filter_map(|call| call.get("name").and_then(Value::as_str))
            .collect();
        if names.is_empty() {
            None
        } else {
            Some(format!("tool calls: {}", names.join(", ")))
        }
    })
}

#[derive(Debug, Clone)]
struct OpenCodeSessionRow {
    id: String,
    parent_id: Option<String>,
    title: String,
    directory: String,
    model: Option<String>,
    agent: Option<String>,
    time_created: i64,
    time_updated: i64,
    tokens_input: i64,
    tokens_output: i64,
    tokens_reasoning: i64,
    tokens_cache_read: i64,
    tokens_cache_write: i64,
}

#[derive(Debug, Clone)]
struct OpenCodeMessageRow {
    id: String,
    session_id: String,
    entry_type: String,
    seq: i64,
    time_created: i64,
    time_updated: i64,
    data: String,
}

fn normalize_opencode_sqlite(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "query_only", true)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = opencode_schema_fingerprint(&conn)?;
    let legacy_message_rows = opencode_count(&conn, "message").unwrap_or(0);
    let legacy_part_rows = opencode_count(&conn, "part").unwrap_or(0);
    let sessions = opencode_sessions(&conn)?;
    let messages = opencode_session_messages(&conn)?;
    let mut result = ProviderNormalizationResult::default();
    let session_started = sessions
        .iter()
        .map(|session| {
            (
                session.id.clone(),
                timestamp_millis_utc(session.time_created, context.imported_at),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let sessions_by_id = sessions
        .into_iter()
        .map(|session| (session.id.clone(), session))
        .collect::<BTreeMap<_, _>>();

    for row in messages {
        let Some(session) = sessions_by_id.get(&row.session_id) else {
            result.summary.failed += 1;
            result.summary.failures.push(ProviderImportFailure {
                line: row.seq.max(0) as usize,
                error: format!(
                    "OpenCode session_message {} references missing session {}",
                    row.id, row.session_id
                ),
            });
            continue;
        };
        let data: Value = match serde_json::from_str(&row.data) {
            Ok(data) => data,
            Err(err) => {
                result.summary.failed += 1;
                result.summary.failures.push(ProviderImportFailure {
                    line: row.seq.max(0) as usize,
                    error: format!("invalid JSON in session_message {}: {err}", row.id),
                });
                continue;
            }
        };
        let occurred_at = opencode_event_time(&data)
            .or_else(|| Some(timestamp_millis_utc(row.time_created, context.imported_at)))
            .unwrap_or(context.imported_at);
        let started_at = session_started
            .get(&session.id)
            .copied()
            .unwrap_or(occurred_at);
        let event = opencode_event(&row, &data, occurred_at);
        let is_subagent = session.parent_id.is_some();
        result.captures.push((
            row.seq.max(0) as usize,
            ProviderCaptureEnvelope {
                schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
                provider: CaptureProvider::OpenCode,
                source: ProviderSourceEnvelope {
                    source_format: OPENCODE_SQLITE_SOURCE_FORMAT.to_owned(),
                    machine_id: context.machine_id.clone(),
                    observed_at: context.imported_at,
                    raw_source_path: Some(path.display().to_string()),
                    raw_retention: ProviderRawRetention::PathReference,
                    redaction_boundary: ProviderRedactionBoundary::BeforeExport,
                    trust: ProviderSourceTrust::ProviderNative,
                    fidelity: Fidelity::Imported,
                    cursor: Some(ProviderCursorRange {
                        before: None,
                        after: Some(ProviderCursorCheckpoint {
                            stream: provider_cursor_stream(
                                CaptureProvider::OpenCode,
                                OPENCODE_SQLITE_SOURCE_FORMAT,
                            ),
                            cursor: format!("session_message:{}:seq:{}", row.session_id, row.seq),
                            observed_at: occurred_at,
                        }),
                    }),
                    idempotency_key: Some(format!(
                        "provider-source:opencode:{OPENCODE_SQLITE_SOURCE_FORMAT}:{}",
                        session.id
                    )),
                    metadata: json!({
                        "adapter": OPENCODE_SQLITE_SOURCE_FORMAT,
                        "sqlite_user_version": user_version,
                        "schema_fingerprint": schema_fingerprint,
                        "legacy_message_rows": legacy_message_rows,
                        "legacy_part_rows": legacy_part_rows,
                    }),
                },
                session: ProviderSessionEnvelope {
                    provider_session_id: session.id.clone(),
                    parent_provider_session_id: session.parent_id.clone(),
                    root_provider_session_id: session.parent_id.clone(),
                    external_agent_id: session.agent.clone(),
                    agent_type: if is_subagent {
                        AgentType::Subagent
                    } else {
                        AgentType::Primary
                    },
                    role_hint: session
                        .agent
                        .clone()
                        .or_else(|| Some(if is_subagent { "subagent" } else { "primary" }.to_owned())),
                    is_primary: !is_subagent,
                    status: SessionStatus::Imported,
                    started_at,
                    ended_at: None,
                    cwd: Some(session.directory.clone()),
                    fidelity: Fidelity::Imported,
                    idempotency_key: Some(format!("provider-session:opencode:{}", session.id)),
                    artifacts: Vec::new(),
                    metadata: json!({
                        "source_format": OPENCODE_SQLITE_SOURCE_FORMAT,
                        "title": session.title,
                        "model": parse_json_object_string(session.model.as_deref()),
                        "agent": session.agent,
                        "time_updated": session.time_updated,
                        "tokens": {
                            "input": session.tokens_input,
                            "output": session.tokens_output,
                            "reasoning": session.tokens_reasoning,
                            "cache_read": session.tokens_cache_read,
                            "cache_write": session.tokens_cache_write,
                        },
                        "legacy_projection": {
                            "message_rows": legacy_message_rows,
                            "part_rows": legacy_part_rows,
                            "import_policy": "session_message is authoritative; legacy message/part rows are retained as schema reference rows to avoid duplicate turn import"
                        },
                    }),
                },
                event: Some(event),
            },
        ));
    }

    Ok(result)
}

fn opencode_sessions(conn: &Connection) -> Result<Vec<OpenCodeSessionRow>> {
    if !sqlite_table_exists(conn, "session")? {
        return Err(CaptureError::InvalidPayload(
            "OpenCode SQLite database is missing required session table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "session")?;
    ensure_sqlite_table_columns(&columns, "OpenCode SQLite session table", &["id"])?;
    let parent_id = optional_column_expr(&columns, "parent_id", "NULL");
    let title = optional_column_expr(
        &columns,
        "title",
        optional_column_expr(&columns, "slug", "id"),
    );
    let directory = optional_column_expr(&columns, "directory", "''");
    let model = optional_column_expr(&columns, "model", "NULL");
    let agent = optional_column_expr(&columns, "agent", "NULL");
    let time_created = optional_column_expr(&columns, "time_created", "0");
    let time_updated = optional_column_expr(&columns, "time_updated", time_created);
    let tokens_input = optional_column_expr(&columns, "tokens_input", "0");
    let tokens_output = optional_column_expr(&columns, "tokens_output", "0");
    let tokens_reasoning = optional_column_expr(&columns, "tokens_reasoning", "0");
    let tokens_cache_read = optional_column_expr(&columns, "tokens_cache_read", "0");
    let tokens_cache_write = optional_column_expr(&columns, "tokens_cache_write", "0");
    let order_by = if columns.contains("time_created") {
        "time_created, id"
    } else {
        "id"
    };
    let sql = format!(
        "select id, {parent_id}, {title}, {directory}, {model}, {agent}, {time_created}, \
         {time_updated}, {tokens_input}, {tokens_output}, {tokens_reasoning}, \
         {tokens_cache_read}, {tokens_cache_write} from session order by {order_by}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(OpenCodeSessionRow {
            id: row.get(0)?,
            parent_id: row.get(1)?,
            title: row.get(2)?,
            directory: row.get(3)?,
            model: row.get(4)?,
            agent: row.get(5)?,
            time_created: row.get(6)?,
            time_updated: row.get(7)?,
            tokens_input: row.get(8)?,
            tokens_output: row.get(9)?,
            tokens_reasoning: row.get(10)?,
            tokens_cache_read: row.get(11)?,
            tokens_cache_write: row.get(12)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

fn opencode_session_messages(conn: &Connection) -> Result<Vec<OpenCodeMessageRow>> {
    if sqlite_table_exists(conn, "session_message")? {
        let rows = opencode_session_message_rows(conn)?;
        if !rows.is_empty() {
            return Ok(rows);
        }
    }
    if sqlite_table_exists(conn, "session_entry")? {
        let rows = opencode_session_entry_rows(conn)?;
        if !rows.is_empty() {
            return Ok(rows);
        }
    }
    if sqlite_table_exists(conn, "message")? {
        return opencode_message_rows(conn);
    }
    Ok(Vec::new())
}

fn opencode_session_message_rows(conn: &Connection) -> Result<Vec<OpenCodeMessageRow>> {
    let columns = sqlite_table_columns(conn, "session_message")?;
    ensure_sqlite_table_columns(
        &columns,
        "OpenCode SQLite session_message table",
        &["id", "session_id", "data"],
    )?;
    let entry_type = optional_column_expr(&columns, "type", "'message'");
    let time_created = optional_column_expr(&columns, "time_created", "0");
    let time_updated = optional_column_expr(&columns, "time_updated", time_created);
    let (seq_expr, order_expr) = if columns.contains("seq") {
        ("seq", "seq, id")
    } else if columns.contains("time_created") {
        ("NULL", "time_created, id")
    } else {
        ("NULL", "id")
    };
    let sql = format!(
        "select id, session_id, {entry_type}, {seq_expr}, {time_created}, {time_updated}, data \
         from session_message order by session_id, {order_expr}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, String>(6)?,
        ))
    })?;
    let rows = rows
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)?;
    let mut messages = Vec::new();
    let mut next_seq_by_session = BTreeMap::<String, i64>::new();
    for (id, session_id, entry_type, seq, time_created, time_updated, data) in rows {
        let seq = seq.unwrap_or_else(|| next_opencode_seq(&mut next_seq_by_session, &session_id));
        messages.push(OpenCodeMessageRow {
            id,
            session_id,
            entry_type,
            seq,
            time_created,
            time_updated,
            data,
        });
    }
    Ok(messages)
}

fn opencode_session_entry_rows(conn: &Connection) -> Result<Vec<OpenCodeMessageRow>> {
    let columns = sqlite_table_columns(conn, "session_entry")?;
    ensure_sqlite_table_columns(
        &columns,
        "OpenCode SQLite session_entry table",
        &[
            "id",
            "session_id",
            "type",
            "time_created",
            "time_updated",
            "data",
        ],
    )?;
    let mut stmt = conn.prepare(
        "select id, session_id, type, time_created, time_updated, data \
         from session_entry order by session_id, time_created, id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;
    let mut messages = Vec::new();
    let mut next_seq_by_session = BTreeMap::<String, i64>::new();
    for row in rows {
        let (id, session_id, entry_type, time_created, time_updated, data) = row?;
        let seq = next_opencode_seq(&mut next_seq_by_session, &session_id);
        messages.push(OpenCodeMessageRow {
            id,
            session_id,
            entry_type,
            seq,
            time_created,
            time_updated,
            data,
        });
    }
    Ok(messages)
}

fn opencode_message_rows(conn: &Connection) -> Result<Vec<OpenCodeMessageRow>> {
    let columns = sqlite_table_columns(conn, "message")?;
    ensure_sqlite_table_columns(
        &columns,
        "OpenCode SQLite message table",
        &["id", "session_id", "time_created", "time_updated", "data"],
    )?;
    let mut stmt = conn.prepare(
        "select id, session_id, time_created, time_updated, data \
         from message order by session_id, time_created, id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut messages = Vec::new();
    let mut next_seq_by_session = BTreeMap::<String, i64>::new();
    for row in rows {
        let (id, session_id, time_created, time_updated, data) = row?;
        let seq = next_opencode_seq(&mut next_seq_by_session, &session_id);
        let entry_type = serde_json::from_str::<Value>(&data)
            .ok()
            .and_then(|value| opencode_message_type_from_data(&value))
            .unwrap_or_else(|| "message".to_owned());
        messages.push(OpenCodeMessageRow {
            id,
            session_id,
            entry_type,
            seq,
            time_created,
            time_updated,
            data,
        });
    }
    Ok(messages)
}

fn next_opencode_seq(next_seq_by_session: &mut BTreeMap<String, i64>, session_id: &str) -> i64 {
    let entry = next_seq_by_session
        .entry(session_id.to_owned())
        .and_modify(|seq| *seq += 1)
        .or_insert(1);
    *entry
}

fn opencode_message_type_from_data(data: &Value) -> Option<String> {
    data.get("role")
        .or_else(|| data.get("type"))
        .or_else(|| data.pointer("/message/role"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

fn sqlite_table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let exists: i64 = conn.query_row(
        "select count(*) from sqlite_schema where type = 'table' and name = ?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(exists > 0)
}

fn sqlite_table_columns(conn: &Connection, table: &str) -> Result<BTreeSet<String>> {
    let mut stmt = conn.prepare(&format!("pragma table_info({})", sqlite_ident(table)))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    rows.collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(CaptureError::from)
}

fn optional_column_expr<'a>(
    columns: &BTreeSet<String>,
    column: &'a str,
    fallback: &'a str,
) -> &'a str {
    if columns.contains(column) {
        column
    } else {
        fallback
    }
}

fn ensure_sqlite_table_columns(
    columns: &BTreeSet<String>,
    label: &str,
    required: &[&str],
) -> Result<()> {
    let missing = required
        .iter()
        .copied()
        .filter(|column| !columns.contains(*column))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(CaptureError::InvalidPayload(format!(
            "{label} missing required column(s): {}",
            missing.join(", ")
        )))
    }
}

fn sqlite_ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn opencode_schema_fingerprint(conn: &Connection) -> Result<String> {
    let mut stmt = conn.prepare(
        "select name, sql from sqlite_schema where type in ('table','index') order by name",
    )?;
    let rows = stmt.query_map([], |row| {
        let name: String = row.get(0)?;
        let sql: Option<String> = row.get(1)?;
        Ok(format!("{name}:{}", sql.unwrap_or_default()))
    })?;
    let schema = rows.collect::<std::result::Result<Vec<_>, _>>()?.join("\n");
    compute_payload_hash(&json!({ "schema": schema }))
}

fn opencode_count(conn: &Connection, table: &str) -> rusqlite::Result<i64> {
    conn.query_row(&format!("select count(*) from {table}"), [], |row| {
        row.get(0)
    })
}

fn opencode_event(
    row: &OpenCodeMessageRow,
    data: &Value,
    occurred_at: DateTime<Utc>,
) -> ProviderEventEnvelope {
    let event_type = opencode_event_type(&row.entry_type, data);
    let role = Some(provider_role(Some(&row.entry_type)));
    let text = opencode_event_text(&row.entry_type, data, event_type);
    let (text, truncated) = provider_safe_preview(&text, PROVIDER_MAX_TEXT_CHARS);
    ProviderEventEnvelope {
        provider_event_index: row.seq.max(0) as u64,
        provider_event_hash: Some(row.id.clone()),
        cursor: Some(format!(
            "session_message:{}:seq:{}",
            row.session_id, row.seq
        )),
        event_type,
        role,
        occurred_at,
        fidelity: Fidelity::Imported,
        redaction_state: RedactionState::SafePreview,
        idempotency_key: Some(format!(
            "provider-event:opencode:{}:{}",
            row.session_id, row.id
        )),
        artifacts: Vec::new(),
        payload: json!({
            "entry_type": row.entry_type,
            "message_id": row.id,
            "session_message_seq": row.seq,
            "text": text,
            "truncated": truncated,
            "body": provider_capped_json(data, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": "opencode_sqlite",
            "source_format": OPENCODE_SQLITE_SOURCE_FORMAT,
            "session_message_id": row.id,
            "session_message_seq": row.seq,
            "time_created": row.time_created,
            "time_updated": row.time_updated,
            "model": data.get("model").cloned(),
            "tokens": data.get("tokens").cloned(),
            "cost": data.get("cost").cloned(),
            "finish": data.get("finish").cloned(),
            "error": data.get("error").cloned(),
        }),
    }
}

fn opencode_event_type(entry_type: &str, data: &Value) -> EventType {
    match entry_type {
        "assistant" if opencode_content_has_tool(data) => EventType::ToolCall,
        "assistant" | "user" | "system" => EventType::Message,
        "shell" => EventType::CommandOutput,
        _ => EventType::Notice,
    }
}

fn opencode_event_text(entry_type: &str, data: &Value, event_type: EventType) -> String {
    if let Some(text) = data.get("text").and_then(Value::as_str) {
        return text.to_owned();
    }
    if entry_type == "shell" {
        let command = data
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("shell");
        let output = data.get("output").and_then(Value::as_str).unwrap_or("");
        return format!("{command}\n{output}");
    }
    if let Some(content) = data.get("content") {
        if let Some(text) = provider_value_text(content) {
            return text;
        }
    }
    if event_type == EventType::Notice {
        format!("OpenCode event: {entry_type}")
    } else {
        serde_json::to_string(data).unwrap_or_else(|_| entry_type.to_owned())
    }
}

fn opencode_content_has_tool(data: &Value) -> bool {
    data.get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks.iter().any(|block| {
                matches!(
                    block.get("type").and_then(Value::as_str),
                    Some("tool" | "tool_use" | "toolCall")
                )
            })
        })
        .unwrap_or(false)
}

fn opencode_event_time(data: &Value) -> Option<DateTime<Utc>> {
    data.pointer("/time/created")
        .and_then(Value::as_i64)
        .and_then(DateTime::<Utc>::from_timestamp_millis)
}

fn timestamp_millis_utc(millis: i64, fallback: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp_millis(millis).unwrap_or(fallback)
}

fn parse_json_object_string(value: Option<&str>) -> Value {
    value
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
        .unwrap_or(Value::Null)
}

fn normalize_jsonl_tree(
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
        let mut result =
            normalize_native_jsonl_session_file(&path, context, provider, source_format)?;
        merged.summary.merge(result.summary);
        merged.captures.append(&mut result.captures);
    }
    Ok(merged)
}

fn native_jsonl_missing_reason(provider: CaptureProvider) -> &'static str {
    match provider {
        CaptureProvider::Antigravity => {
            "no Antigravity transcript JSONL files found under brain/*/.system_generated/logs"
        }
        CaptureProvider::Gemini => "no Gemini CLI chat JSONL transcripts found under chats",
        CaptureProvider::Cursor => {
            "no Cursor agent transcript JSONL files found under projects/*/agent-transcripts"
        }
        CaptureProvider::CopilotCli => "no Copilot CLI session events.jsonl transcripts found",
        CaptureProvider::FactoryAiDroid => "no Factory AI Droid session JSONL transcripts found",
        _ => "no native provider JSONL transcripts found",
    }
}

fn provider_jsonl_path_is_native(provider: CaptureProvider, path: &Path) -> bool {
    match provider {
        CaptureProvider::Antigravity => {
            matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("transcript_full.jsonl" | "transcript.jsonl")
            )
        }
        CaptureProvider::Gemini => path
            .components()
            .any(|component| component.as_os_str() == "chats"),
        CaptureProvider::Cursor => path
            .components()
            .any(|component| component.as_os_str() == "agent-transcripts"),
        CaptureProvider::CopilotCli => {
            path.file_name().and_then(|name| name.to_str()) == Some("events.jsonl")
        }
        _ => true,
    }
}

fn antigravity_preferred_transcript_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut by_session: BTreeMap<String, PathBuf> = BTreeMap::new();
    for path in paths {
        let session =
            antigravity_session_id_from_path(&path).unwrap_or_else(|| path.display().to_string());
        let prefer_new =
            path.file_name().and_then(|name| name.to_str()) == Some("transcript_full.jsonl");
        let replace = by_session
            .get(&session)
            .map(|current| {
                prefer_new
                    && current.file_name().and_then(|name| name.to_str())
                        != Some("transcript_full.jsonl")
            })
            .unwrap_or(true);
        if replace {
            by_session.insert(session, path);
        }
    }
    by_session.into_values().collect()
}

fn normalize_native_jsonl_session_file(
    path: &Path,
    context: &ProviderAdapterContext,
    provider: CaptureProvider,
    source_format: &'static str,
) -> Result<ProviderNormalizationResult> {
    ensure_regular_provider_transcript_file(path)?;
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut result = ProviderNormalizationResult::default();
    let mut rows = Vec::new();

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
                    error: format!("malformed JSONL: {err}"),
                });
                continue;
            }
        };
        rows.push((line_number, value));
    }

    let header_index = if provider == CaptureProvider::Antigravity {
        if rows.is_empty() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: native_jsonl_missing_reason(provider),
            });
        }
        0
    } else {
        let Some(header_index) = rows
            .iter()
            .position(|(_, value)| native_jsonl_header_session_id(provider, value).is_some())
        else {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: native_jsonl_missing_reason(provider),
            });
        };
        header_index
    };

    let header = rows[header_index].1.clone();
    let native_session_id = if provider == CaptureProvider::Antigravity {
        antigravity_session_id_from_path(path).unwrap_or_else(|| "unknown-session".to_owned())
    } else {
        native_jsonl_header_session_id(provider, &header)
            .unwrap_or_else(|| "unknown-session".to_owned())
    };
    let (provider_session_id, parent_provider_session_id, external_agent_id, agent_type) =
        native_jsonl_path_session(provider, path, &header, &native_session_id);
    let started_at = native_jsonl_timestamp(&header)
        .or_else(|| native_jsonl_header_start_time(provider, &header))
        .unwrap_or(context.imported_at);
    let cwd = native_jsonl_header_cwd(provider, &header);
    let is_subagent = parent_provider_session_id.is_some() || agent_type == AgentType::Subagent;

    for (line_number, value) in rows {
        let occurred_at = native_jsonl_timestamp(&value).unwrap_or(started_at);
        let event = native_jsonl_event(provider, source_format, &value, line_number, occurred_at);
        result.captures.push((
            line_number,
            ProviderCaptureEnvelope {
                schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
                provider,
                source: ProviderSourceEnvelope {
                    source_format: source_format.to_owned(),
                    machine_id: context.machine_id.clone(),
                    observed_at: context.imported_at,
                    raw_source_path: Some(path.display().to_string()),
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
                        "source_path": path.display().to_string(),
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

fn native_jsonl_header_session_id(provider: CaptureProvider, value: &Value) -> Option<String> {
    match provider {
        CaptureProvider::Gemini => value.get("sessionId").and_then(Value::as_str),
        CaptureProvider::FactoryAiDroid => (value.get("type").and_then(Value::as_str)
            == Some("session_start"))
        .then(|| value.get("sessionId").and_then(Value::as_str))
        .flatten(),
        CaptureProvider::CopilotCli => (value.get("type").and_then(Value::as_str)
            == Some("session.start"))
        .then(|| value.pointer("/data/sessionId").and_then(Value::as_str))
        .flatten(),
        CaptureProvider::Cursor => (value.get("role").is_some()
            || value.get("event").is_some()
            || value.get("message").is_some())
        .then_some("cursor-path-session"),
        _ => None,
    }
    .filter(|id| !id.trim().is_empty())
    .map(str::to_owned)
}

fn native_jsonl_header_start_time(
    provider: CaptureProvider,
    value: &Value,
) -> Option<DateTime<Utc>> {
    match provider {
        CaptureProvider::Antigravity => value.get("created_at").and_then(Value::as_str),
        CaptureProvider::Gemini => value.get("startTime").and_then(Value::as_str),
        CaptureProvider::CopilotCli => value.pointer("/data/startTime").and_then(Value::as_str),
        _ => None,
    }
    .and_then(parse_rfc3339_utc)
}

fn native_jsonl_header_cwd(provider: CaptureProvider, value: &Value) -> Option<String> {
    match provider {
        CaptureProvider::Gemini => value
            .get("directories")
            .and_then(Value::as_array)
            .and_then(|dirs| dirs.first())
            .and_then(Value::as_str),
        CaptureProvider::FactoryAiDroid => value.get("cwd").and_then(Value::as_str),
        CaptureProvider::CopilotCli => value.pointer("/data/context/cwd").and_then(Value::as_str),
        _ => None,
    }
    .filter(|cwd| !cwd.trim().is_empty())
    .map(str::to_owned)
}

fn native_jsonl_path_session(
    provider: CaptureProvider,
    path: &Path,
    header: &Value,
    native_session_id: &str,
) -> (String, Option<String>, Option<String>, AgentType) {
    match provider {
        CaptureProvider::Gemini => {
            let parent = path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str());
            if parent.is_some_and(|name| name != "chats") {
                return (
                    native_session_id.to_owned(),
                    parent.map(str::to_owned),
                    None,
                    AgentType::Subagent,
                );
            }
            (native_session_id.to_owned(), None, None, AgentType::Primary)
        }
        CaptureProvider::FactoryAiDroid => {
            let parent = header
                .get("parent")
                .or_else(|| header.get("callingSessionId"))
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .map(str::to_owned);
            let agent_type = if parent.is_some()
                || header.get("decompSessionType").and_then(Value::as_str) == Some("worker")
            {
                AgentType::Subagent
            } else {
                AgentType::Primary
            };
            (
                native_session_id.to_owned(),
                parent,
                header
                    .get("decompMissionId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                agent_type,
            )
        }
        CaptureProvider::Cursor => {
            let session = path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .unwrap_or(native_session_id)
                .to_owned();
            (session, None, None, AgentType::Primary)
        }
        _ => (native_session_id.to_owned(), None, None, AgentType::Primary),
    }
}

fn antigravity_session_id_from_path(path: &Path) -> Option<String> {
    let components: Vec<String> = path
        .components()
        .filter_map(|component| component.as_os_str().to_str().map(str::to_owned))
        .collect();
    components
        .windows(2)
        .find_map(|window| {
            (window[0] == "brain" && !window[1].trim().is_empty()).then(|| window[1].clone())
        })
        .or_else(|| {
            components.windows(2).find_map(|window| {
                (window[1] == ".system_generated" && !window[0].trim().is_empty())
                    .then(|| window[0].clone())
            })
        })
        .or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.trim().is_empty())
                .map(str::to_owned)
        })
}

fn native_jsonl_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_utc)
        .or_else(|| {
            value
                .get("created_at")
                .and_then(Value::as_str)
                .and_then(parse_rfc3339_utc)
        })
        .or_else(|| {
            value
                .pointer("/time/created")
                .and_then(Value::as_i64)
                .and_then(DateTime::<Utc>::from_timestamp_millis)
        })
}

fn native_jsonl_session_status(provider: CaptureProvider, header: &Value) -> SessionStatus {
    if provider == CaptureProvider::CopilotCli
        && header.get("type").and_then(Value::as_str) == Some("abort")
    {
        SessionStatus::Interrupted
    } else {
        SessionStatus::Imported
    }
}

fn native_jsonl_session_metadata(
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

fn native_jsonl_event(
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
    let (text, truncated) = provider_safe_preview(&text, PROVIDER_MAX_TEXT_CHARS);
    let event_id = native_jsonl_event_id(provider, value, line_number);
    let tool_calls = if provider == CaptureProvider::Antigravity {
        value
            .get("tool_calls")
            .map(|calls| provider_redacted_json_value(calls, PROVIDER_MAX_PREVIEW_CHARS))
    } else {
        None
    };

    Some(ProviderEventEnvelope {
        provider_event_index: (line_number - 1) as u64,
        provider_event_hash: Some(event_id.clone()),
        cursor: Some(event_id.clone()),
        event_type,
        role: Some(role),
        occurred_at,
        fidelity: Fidelity::Imported,
        redaction_state: RedactionState::SafePreview,
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
            "body": provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": source_format,
            "source_format": source_format,
            "line": line_number,
            "entry_type": entry_type,
            "status": value.get("status").and_then(Value::as_str),
            "model": native_jsonl_model(provider, value),
            "tokens": value.get("tokens").cloned(),
        }),
    })
}

fn native_jsonl_event_id(provider: CaptureProvider, value: &Value, line_number: usize) -> String {
    if provider == CaptureProvider::Antigravity {
        if let Some(step_index) = value.get("step_index").and_then(Value::as_u64) {
            return format!("step-{step_index}");
        }
    }
    value
        .get("id")
        .or_else(|| value.get("uuid"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("line-{line_number}"))
}

fn native_jsonl_entry_type(provider: CaptureProvider, value: &Value) -> String {
    match provider {
        CaptureProvider::Antigravity => value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        CaptureProvider::Gemini => {
            if value.get("$set").is_some() {
                "$set"
            } else if value.get("$rewindTo").is_some() {
                "$rewindTo"
            } else {
                value
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            }
        }
        _ => value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
    }
    .to_owned()
}

fn native_jsonl_event_type(provider: CaptureProvider, value: &Value) -> EventType {
    match provider {
        CaptureProvider::Antigravity => match value.get("type").and_then(Value::as_str) {
            Some("USER_INPUT" | "CONVERSATION_HISTORY") => EventType::Message,
            Some("PLANNER_RESPONSE") => {
                if value.get("tool_calls").is_some() {
                    EventType::ToolCall
                } else {
                    EventType::Message
                }
            }
            Some("CODE_ACTION") => EventType::ToolCall,
            Some("CHECKPOINT") => EventType::Summary,
            Some("SYSTEM_MESSAGE") => EventType::Notice,
            _ => EventType::Notice,
        },
        CaptureProvider::Gemini => {
            if value.get("$set").is_some() || value.get("$rewindTo").is_some() {
                EventType::Notice
            } else if value.get("toolCalls").is_some() {
                if gemini_tool_calls_have_result(value) {
                    EventType::ToolOutput
                } else {
                    EventType::ToolCall
                }
            } else {
                match value.get("type").and_then(Value::as_str) {
                    Some("user" | "gemini") => EventType::Message,
                    _ => EventType::Notice,
                }
            }
        }
        CaptureProvider::FactoryAiDroid => match value.get("type").and_then(Value::as_str) {
            Some("message") if droid_content_has(value, "tool_use") => EventType::ToolCall,
            Some("message") if droid_content_has(value, "tool_result") => EventType::ToolOutput,
            Some("message") => EventType::Message,
            Some("compaction_state") => EventType::Summary,
            Some("todo_state" | "session_start") => EventType::Notice,
            _ => EventType::Notice,
        },
        CaptureProvider::CopilotCli => match value.get("type").and_then(Value::as_str) {
            Some("user.message" | "assistant.message") => EventType::Message,
            Some("tool.execution_start") => EventType::ToolCall,
            Some("tool.execution_complete") => EventType::ToolOutput,
            Some("session.truncation") => EventType::Summary,
            Some("abort") => EventType::Notice,
            _ => EventType::Notice,
        },
        CaptureProvider::Cursor => {
            if native_jsonl_content_has(value, "tool_result") {
                EventType::ToolOutput
            } else if native_jsonl_content_has(value, "tool_use") {
                EventType::ToolCall
            } else {
                match value
                    .get("event")
                    .or_else(|| value.get("type"))
                    .or_else(|| value.get("role"))
                    .and_then(Value::as_str)
                {
                    Some("turn_ended" | "summary") => EventType::Summary,
                    Some("user" | "assistant") => EventType::Message,
                    _ => EventType::Notice,
                }
            }
        }
        _ => EventType::Notice,
    }
}

fn native_jsonl_role(provider: CaptureProvider, value: &Value) -> EventRole {
    match provider {
        CaptureProvider::Antigravity => match value.get("source").and_then(Value::as_str) {
            Some("user") => EventRole::User,
            Some("planner" | "agent" | "assistant") => EventRole::Assistant,
            Some("tool" | "executor") => EventRole::Tool,
            Some("system") => EventRole::System,
            _ => match value.get("type").and_then(Value::as_str) {
                Some("USER_INPUT") => EventRole::User,
                Some("SYSTEM_MESSAGE" | "CHECKPOINT") => EventRole::System,
                _ => EventRole::Assistant,
            },
        },
        CaptureProvider::Gemini => match value.get("type").and_then(Value::as_str) {
            Some("user") => EventRole::User,
            Some("gemini") => EventRole::Assistant,
            _ => EventRole::System,
        },
        CaptureProvider::FactoryAiDroid => provider_role(value.get("role").and_then(Value::as_str)),
        CaptureProvider::CopilotCli => match value.get("type").and_then(Value::as_str) {
            Some("user.message") => EventRole::User,
            Some("assistant.message") => EventRole::Assistant,
            Some("tool.execution_start" | "tool.execution_complete") => EventRole::Tool,
            _ => EventRole::System,
        },
        CaptureProvider::Cursor => provider_role(
            value
                .get("role")
                .or_else(|| value.pointer("/message/role"))
                .and_then(Value::as_str),
        ),
        _ => EventRole::Unknown,
    }
}

fn native_jsonl_event_text(
    provider: CaptureProvider,
    value: &Value,
    event_type: EventType,
    entry_type: &str,
) -> String {
    match provider {
        CaptureProvider::Antigravity => value
            .get("content")
            .and_then(provider_value_text)
            .map(|content| {
                value
                    .get("tool_calls")
                    .and_then(antigravity_tool_call_text)
                    .map(|tools| format!("{content}\n{tools}"))
                    .unwrap_or(content)
            })
            .or_else(|| value.get("thinking").and_then(provider_value_text))
            .or_else(|| value.get("tool_calls").and_then(antigravity_tool_call_text))
            .unwrap_or_else(|| format!("Antigravity event: {entry_type}")),
        CaptureProvider::Gemini => value
            .get("content")
            .and_then(provider_value_text)
            .or_else(|| value.get("toolCalls").and_then(provider_value_text))
            .or_else(|| value.get("$set").and_then(provider_value_text))
            .or_else(|| {
                value
                    .get("$rewindTo")
                    .and_then(Value::as_str)
                    .map(|id| format!("rewind to {id}"))
            })
            .unwrap_or_else(|| format!("Gemini event: {entry_type}")),
        CaptureProvider::FactoryAiDroid => value
            .get("content")
            .and_then(provider_value_text)
            .or_else(|| {
                value
                    .get("summary")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| value.get("items").and_then(provider_value_text))
            .unwrap_or_else(|| format!("Factory AI Droid event: {entry_type}")),
        CaptureProvider::CopilotCli => value
            .pointer("/data/content")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                value
                    .pointer("/data/result/content")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| {
                value
                    .pointer("/data/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| {
                value
                    .pointer("/data/toolName")
                    .and_then(Value::as_str)
                    .map(|tool| format!("tool {tool}"))
            })
            .unwrap_or_else(|| format!("Copilot CLI event: {entry_type}")),
        CaptureProvider::Cursor => value
            .pointer("/message/content")
            .or_else(|| value.get("content"))
            .and_then(provider_value_text)
            .or_else(|| value.get("text").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| format!("Cursor event: {entry_type}")),
        _ if event_type == EventType::Notice => format!("Provider event: {entry_type}"),
        _ => serde_json::to_string(value).unwrap_or_else(|_| entry_type.to_owned()),
    }
}

fn native_jsonl_model(provider: CaptureProvider, value: &Value) -> Option<Value> {
    match provider {
        CaptureProvider::Antigravity => value.get("model").cloned(),
        CaptureProvider::Gemini => value.get("model").cloned(),
        CaptureProvider::FactoryAiDroid => value
            .get("model")
            .cloned()
            .or_else(|| value.pointer("/metadata/model").cloned()),
        CaptureProvider::CopilotCli => value.pointer("/data/selectedModel").cloned(),
        _ => None,
    }
}

fn gemini_tool_calls_have_result(value: &Value) -> bool {
    value
        .get("toolCalls")
        .and_then(Value::as_array)
        .map(|calls| calls.iter().any(|call| call.get("result").is_some()))
        .unwrap_or(false)
}

fn droid_content_has(value: &Value, expected: &str) -> bool {
    value
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some(expected))
        })
        .unwrap_or(false)
}

fn native_jsonl_content_has(value: &Value, expected: &str) -> bool {
    value
        .pointer("/message/content")
        .or_else(|| value.get("content"))
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some(expected))
        })
        .unwrap_or(false)
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
        history_record_id: options.history_record_id,
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
            history_record_id: options.history_record_id,
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
            history_record_id: options.history_record_id,
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

pub fn archive_from_envelopes(envelopes: &[CaptureEnvelope]) -> Result<SessionHistoryArchive> {
    let mut archive = SessionHistoryArchive {
        schema_version: 1,
        version: 1,
        records: Vec::new(),
        ..SessionHistoryArchive::default()
    };

    for envelope in envelopes {
        validate_envelope(envelope)?;
        if let Some(archive_value) = envelope.payload.get("archive") {
            let nested: SessionHistoryArchive = serde_json::from_value(archive_value.clone())?;
            archive.records.extend(nested.records);
            archive.capture_sources.extend(nested.capture_sources);
            archive.sessions.extend(nested.sessions);
            archive.runs.extend(nested.runs);
            archive.events.extend(nested.events);
            archive.artifact_records.extend(nested.artifact_records);
            archive.vcs_workspaces.extend(nested.vcs_workspaces);
            archive.vcs_changes.extend(nested.vcs_changes);
            archive
                .history_record_links
                .extend(nested.history_record_links);
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
    let name = format!("ctx-ctx-history-capture:{dedupe_key}:{role}");
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

fn record_from_envelope(envelope: &CaptureEnvelope, value: &Value) -> Result<HistoryRecord> {
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
        .or_else(|| string_field(value, "history_record_kind"))
        .or_else(|| string_field(value, "kind").filter(|kind| kind != "history_record"))
        .unwrap_or_else(|| "capture".to_owned());
    let workspace = string_field(value, "workspace")
        .or_else(|| envelope.cwd.clone())
        .or_else(|| envelope.source.cwd.clone());
    let created_at = datetime_field(value, "created_at")?.unwrap_or(envelope.occurred_at);
    let updated_at = datetime_field(value, "updated_at")?.unwrap_or(created_at);

    Ok(HistoryRecord {
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
    history_record_id: Option<Uuid>,
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
        history_record_id,
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
        history_record_id,
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
        "history_record_kind",
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
        tempfile::Builder::new()
            .prefix("ctx-history-capture-")
            .tempdir()
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
        materialized_fixture("provider", name)
    }

    fn provider_history_fixture(name: &str) -> PathBuf {
        materialized_fixture("provider-history", name)
    }

    fn materialized_fixture(category: &str, name: &str) -> PathBuf {
        let source = match category {
            "provider" => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/provider")
                .join(name),
            "provider-history" => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/provider-history")
                .join(name),
            _ => panic!("unknown fixture category {category}"),
        };
        let root = std::env::current_dir()
            .unwrap()
            .join("target/test-data/materialized-fixtures");
        fs::create_dir_all(&root).unwrap();
        let unique = format!(
            "{}-{}-{}-{}",
            category,
            name.replace(['/', '\\', '.'], "_"),
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let target = root.join(unique);
        if source.is_dir() {
            copy_dir_all(&source, &target);
        } else {
            fs::copy(&source, &target).unwrap();
        }
        target
    }

    fn copy_dir_all(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let entry_path = entry.path();
            let target = to.join(entry.file_name());
            if entry_path.is_dir() {
                copy_dir_all(&entry_path, &target);
            } else {
                fs::copy(entry_path, target).unwrap();
            }
        }
    }

    fn synthetic_codex_session_tree(root: &Path, sessions: usize) -> u64 {
        (0..sessions)
            .map(|index| write_synthetic_codex_session(root, index, "baseline"))
            .sum()
    }

    fn write_synthetic_codex_session(root: &Path, index: usize, marker: &str) -> u64 {
        let shard = format!("{:02}", index / 1000);
        let dir = root.join("2026").join("06").join("26").join(shard);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("synthetic-session-{index:06}.jsonl"));
        let seconds = index % 86_400;
        let timestamp = format!(
            "2026-06-26T{:02}:{:02}:{:02}.000Z",
            seconds / 3600,
            (seconds / 60) % 60,
            seconds % 60
        );
        let session_id = format!("synthetic-codex-session-{index:06}");
        let meta = json!({
            "timestamp": timestamp,
            "type": "session_meta",
            "payload": {
                "id": session_id,
                "timestamp": timestamp,
                "cwd": "/repo/ctx",
                "originator": "codex-cli",
                "cli_version": "0.2.0-test",
                "source": "cli",
                "model_provider": "openai"
            }
        });
        let message = json!({
            "timestamp": timestamp,
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": format!("incremental import synthetic corpus {index:06} {marker}")
                }]
            }
        });
        let body = format!("{meta}\n{message}\n");
        fs::write(&path, body.as_bytes()).unwrap();
        body.len() as u64
    }

    #[derive(Debug)]
    struct IncrementalCatchUpSummary {
        catalog: CatalogSummary,
        import: ProviderImportSummary,
        pending_sessions: usize,
    }

    fn incremental_codex_catch_up(
        root: &Path,
        store: &mut Store,
        observed_at: DateTime<Utc>,
    ) -> IncrementalCatchUpSummary {
        let source_root = root.display().to_string();
        let catalog = catalog_codex_session_tree(
            root,
            store,
            CodexSessionCatalogOptions {
                source_root: Some(root.to_path_buf()),
                cataloged_at: observed_at,
                allow_partial_failures: false,
                ..CodexSessionCatalogOptions::default()
            },
        )
        .unwrap();
        let pending = store
            .list_pending_catalog_sessions(CaptureProvider::Codex, &source_root)
            .unwrap();
        let pending_sessions = pending.len();
        if pending.is_empty() {
            return IncrementalCatchUpSummary {
                catalog,
                import: ProviderImportSummary::default(),
                pending_sessions,
            };
        }

        let paths = pending
            .iter()
            .map(|session| PathBuf::from(&session.source_path))
            .collect::<Vec<_>>();
        let import = import_codex_session_paths(
            paths,
            store,
            CodexSessionImportOptions {
                source_path: Some(root.to_path_buf()),
                imported_at: observed_at,
                allow_partial_failures: false,
                ..CodexSessionImportOptions::default()
            },
        )
        .unwrap();
        let indexed_at_ms = observed_at.timestamp_millis();
        for session in pending {
            store
                .mark_catalog_source_indexed(
                    CaptureProvider::Codex,
                    ctx_history_store::CatalogSourceIndexUpdate {
                        source_root: &session.source_root,
                        source_path: &session.source_path,
                        file_size_bytes: session.file_size_bytes,
                        file_modified_at_ms: session.file_modified_at_ms,
                        event_count: 1,
                        indexed_at_ms,
                    },
                )
                .unwrap();
        }

        IncrementalCatchUpSummary {
            catalog,
            import,
            pending_sessions,
        }
    }

    #[derive(Debug)]
    struct TimingStats {
        min_ms: f64,
        p50_ms: f64,
        p95_ms: f64,
        max_ms: f64,
    }

    impl TimingStats {
        fn to_json(&self) -> Value {
            json!({
                "min_ms": rounded(self.min_ms),
                "p50_ms": rounded(self.p50_ms),
                "p95_ms": rounded(self.p95_ms),
                "max_ms": rounded(self.max_ms),
            })
        }
    }

    fn timing_stats(samples: &[f64]) -> TimingStats {
        assert!(!samples.is_empty(), "timing samples must not be empty");
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        TimingStats {
            min_ms: sorted[0],
            p50_ms: percentile(&sorted, 0.50),
            p95_ms: percentile(&sorted, 0.95),
            max_ms: *sorted.last().unwrap(),
        }
    }

    fn percentile(sorted: &[f64], percentile: f64) -> f64 {
        let index = ((sorted.len() - 1) as f64 * percentile).ceil() as usize;
        sorted[index.min(sorted.len() - 1)]
    }

    fn elapsed_ms(duration: std::time::Duration) -> f64 {
        duration.as_secs_f64() * 1000.0
    }

    fn rounded(value: f64) -> f64 {
        (value * 100.0).round() / 100.0
    }

    fn env_flag(name: &str) -> bool {
        std::env::var_os(name).is_some_and(|value| {
            let value = value.to_string_lossy();
            !matches!(value.as_ref(), "" | "0" | "false" | "False" | "FALSE")
        })
    }

    fn env_usize(name: &str) -> Option<usize> {
        std::env::var(name).ok()?.parse().ok()
    }

    fn env_f64(name: &str) -> Option<f64> {
        std::env::var(name).ok()?.parse().ok()
    }

    fn incremental_perf_file_count() -> usize {
        env_usize("CTX_CODEX_INCREMENTAL_PERF_FILES").unwrap_or_else(|| {
            if env_flag("CTX_CODEX_INCREMENTAL_PERF_SLOW") {
                32_000
            } else {
                5_000
            }
        })
    }

    fn incremental_perf_repeats() -> usize {
        env_usize("CTX_CODEX_INCREMENTAL_PERF_REPEATS")
            .unwrap_or(5)
            .max(1)
    }

    fn incremental_perf_noop_p95_threshold_ms(file_count: usize) -> f64 {
        env_f64("CTX_CODEX_INCREMENTAL_PERF_NOOP_P95_MS").unwrap_or({
            if file_count >= 30_000 {
                1_000.0
            } else {
                500.0
            }
        })
    }

    fn incremental_perf_noop_us_per_file_threshold() -> f64 {
        env_f64("CTX_CODEX_INCREMENTAL_PERF_NOOP_US_PER_FILE").unwrap_or(50.0)
    }

    fn fixed_import_options(path: PathBuf) -> ProviderFixtureImportOptions {
        ProviderFixtureImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(path),
            imported_at: DateTime::parse_from_rfc3339("2026-06-23T15:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            history_record_id: None,
            expected_provider: None,
            allow_partial_failures: false,
            ..ProviderFixtureImportOptions::default()
        }
    }

    fn write_minimal_provider_fixture(
        temp: &TempDir,
        provider: CaptureProvider,
        external_session_id: &str,
    ) -> PathBuf {
        let provider_name = provider.as_str();
        let path = temp.path().join(format!("{provider_name}.jsonl"));
        let line = json!({
            "provider": provider_name,
            "session": {
                "provider_session_id": external_session_id,
                "agent_type": "primary",
                "role_hint": "primary",
                "is_primary": true,
                "status": "imported",
                "started_at": "2026-06-23T17:00:00Z",
                "cwd": "/workspace/example",
                "metadata": {"source": "temp-fixture", "provider": provider_name}
            },
            "event": {
                "provider_event_index": 0,
                "cursor": format!("{provider_name}-cursor-0"),
                "event_type": "message",
                "role": "user",
                "occurred_at": "2026-06-23T17:00:01Z",
                "payload": {"text": format!("{provider_name} provider fixture smoke")},
                "metadata": {"source": "temp-fixture"}
            }
        });
        fs::write(&path, format!("{line}\n")).unwrap();
        path
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
            .any(|event| event.payload.to_string().contains("local history search")));
    }

    #[test]
    fn codex_session_catalog_large_noop_uses_metadata_cache() {
        let temp = tempdir();
        let root = temp.path().join("sessions");
        let session_count = 1_024;
        synthetic_codex_session_tree(&root, session_count);
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let first = catalog_codex_session_tree(
            &root,
            &store,
            CodexSessionCatalogOptions {
                source_root: Some(root.clone()),
                cataloged_at: "2026-06-26T12:00:00Z".parse().unwrap(),
                allow_partial_failures: false,
                ..CodexSessionCatalogOptions::default()
            },
        )
        .unwrap();
        assert_eq!(first.source_files, session_count);
        assert_eq!(first.cataloged_sessions, session_count);
        assert_eq!(first.cached_sessions, 0);
        assert_eq!(first.parsed_sessions, session_count);
        assert_eq!(first.failed_sessions, 0);

        let second = catalog_codex_session_tree(
            &root,
            &store,
            CodexSessionCatalogOptions {
                source_root: Some(root.clone()),
                cataloged_at: "2026-06-26T12:01:00Z".parse().unwrap(),
                allow_partial_failures: false,
                ..CodexSessionCatalogOptions::default()
            },
        )
        .unwrap();
        assert_eq!(second.source_files, session_count);
        assert_eq!(second.cataloged_sessions, session_count);
        assert_eq!(second.cached_sessions, session_count);
        assert_eq!(second.parsed_sessions, 0);
        assert_eq!(second.failed_sessions, 0);

        write_synthetic_codex_session(&root, 17, "changed-size-for-incremental-refresh");
        let third = catalog_codex_session_tree(
            &root,
            &store,
            CodexSessionCatalogOptions {
                source_root: Some(root.clone()),
                cataloged_at: "2026-06-26T12:02:00Z".parse().unwrap(),
                allow_partial_failures: false,
                ..CodexSessionCatalogOptions::default()
            },
        )
        .unwrap();
        assert_eq!(third.source_files, session_count);
        assert_eq!(third.cataloged_sessions, session_count);
        assert_eq!(third.cached_sessions, session_count - 1);
        assert_eq!(third.parsed_sessions, 1);
        assert_eq!(third.failed_sessions, 0);
    }

    #[test]
    fn codex_session_catalog_marks_deleted_paths_stale_when_additions_outnumber_deletions() {
        let temp = tempdir();
        let root = temp.path().join("sessions");
        synthetic_codex_session_tree(&root, 2);
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let source_root = root.display().to_string();

        let first = catalog_codex_session_tree(
            &root,
            &store,
            CodexSessionCatalogOptions {
                source_root: Some(root.clone()),
                cataloged_at: "2026-06-26T12:00:00Z".parse().unwrap(),
                allow_partial_failures: false,
                ..CodexSessionCatalogOptions::default()
            },
        )
        .unwrap();
        assert_eq!(first.cataloged_sessions, 2);

        fs::remove_file(
            root.join("2026/06/26/00")
                .join("synthetic-session-000000.jsonl"),
        )
        .unwrap();
        write_synthetic_codex_session(&root, 2, "addition-one");
        write_synthetic_codex_session(&root, 3, "addition-two");

        let second = catalog_codex_session_tree(
            &root,
            &store,
            CodexSessionCatalogOptions {
                source_root: Some(root.clone()),
                cataloged_at: "2026-06-26T12:01:00Z".parse().unwrap(),
                allow_partial_failures: false,
                ..CodexSessionCatalogOptions::default()
            },
        )
        .unwrap();
        assert_eq!(second.source_files, 3);
        assert_eq!(second.cataloged_sessions, 3);
        assert_eq!(
            store
                .catalog_source_stale_session_count(CaptureProvider::Codex, &source_root)
                .unwrap(),
            1
        );
    }

    #[test]
    #[ignore = "manual perf gate; run through //:codex_incremental_import_perf_bench"]
    fn synthetic_codex_incremental_import_perf_records_thresholded_evidence() {
        let out_dir = std::env::var_os("CTX_ARTIFACT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .ancestors()
                    .nth(2)
                    .unwrap()
                    .join("target/ctx-artifacts/codex_incremental_import_perf_bench")
            });
        fs::create_dir_all(&out_dir).unwrap();
        let artifact_path = out_dir.join("synthetic-codex-incremental-import-perf.json");

        let temp = tempdir();
        let root = temp.path().join("sessions");
        let file_count = incremental_perf_file_count();
        let repeats = incremental_perf_repeats();
        let generation_started = std::time::Instant::now();
        let source_bytes = synthetic_codex_session_tree(&root, file_count);
        let generation_ms = elapsed_ms(generation_started.elapsed());

        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let first_started = std::time::Instant::now();
        let first =
            incremental_codex_catch_up(&root, &mut store, "2026-06-26T13:00:00Z".parse().unwrap());
        let first_ms = elapsed_ms(first_started.elapsed());
        assert_eq!(first.catalog.parsed_sessions, file_count);
        assert_eq!(first.catalog.cached_sessions, 0);
        assert_eq!(first.pending_sessions, file_count);
        assert_eq!(first.import.imported_sessions, file_count);

        let warmup =
            incremental_codex_catch_up(&root, &mut store, "2026-06-26T13:01:00Z".parse().unwrap());
        assert_eq!(warmup.catalog.cached_sessions, file_count);
        assert_eq!(warmup.catalog.parsed_sessions, 0);
        assert_eq!(warmup.pending_sessions, 0);
        assert_eq!(warmup.import.imported_sessions, 0);
        assert_eq!(warmup.import.imported_events, 0);

        let mut noop_samples = Vec::with_capacity(repeats);
        let noop_base_time: DateTime<Utc> = "2026-06-26T13:02:00Z".parse().unwrap();
        for index in 0..repeats {
            let observed_at = noop_base_time + chrono::Duration::minutes(index as i64);
            let started = std::time::Instant::now();
            let noop = incremental_codex_catch_up(&root, &mut store, observed_at);
            let elapsed = elapsed_ms(started.elapsed());
            assert_eq!(noop.catalog.cached_sessions, file_count);
            assert_eq!(noop.catalog.parsed_sessions, 0);
            assert_eq!(noop.pending_sessions, 0);
            assert_eq!(noop.import.imported_sessions, 0);
            assert_eq!(noop.import.imported_events, 0);
            noop_samples.push(elapsed);
        }

        let noop_stats = timing_stats(&noop_samples);
        let noop_us_per_file = (noop_stats.p95_ms * 1000.0) / file_count as f64;
        let noop_p95_threshold_ms = incremental_perf_noop_p95_threshold_ms(file_count);
        let noop_us_per_file_threshold = incremental_perf_noop_us_per_file_threshold();
        let checks = vec![
            json!({
                "name": "no_op_catalog_parses_zero_sessions",
                "passed": warmup.catalog.parsed_sessions == 0,
                "actual": warmup.catalog.parsed_sessions,
                "threshold": 0
            }),
            json!({
                "name": "no_op_pending_sessions_zero",
                "passed": warmup.pending_sessions == 0,
                "actual": warmup.pending_sessions,
                "threshold": 0
            }),
            json!({
                "name": "no_op_p95_ms",
                "passed": noop_stats.p95_ms <= noop_p95_threshold_ms,
                "actual": rounded(noop_stats.p95_ms),
                "threshold": noop_p95_threshold_ms
            }),
            json!({
                "name": "no_op_us_per_file",
                "passed": noop_us_per_file <= noop_us_per_file_threshold,
                "actual": rounded(noop_us_per_file),
                "threshold": noop_us_per_file_threshold
            }),
        ];
        let passed = checks
            .iter()
            .all(|check| check["passed"].as_bool().unwrap_or(false));

        let artifact = json!({
            "schema_version": 1,
            "profile": "synthetic-codex-incremental-import-perf",
            "mode": if file_count >= 30_000 { "slow" } else { "standard" },
            "status": if passed { "passed" } else { "failed" },
            "corpus": {
                "source_files": file_count,
                "source_bytes": source_bytes,
                "events_per_session": 1
            },
            "thresholds": {
                "noop_p95_ms": noop_p95_threshold_ms,
                "noop_us_per_file": noop_us_per_file_threshold,
                "env_overrides": [
                    "CTX_CODEX_INCREMENTAL_PERF_FILES",
                    "CTX_CODEX_INCREMENTAL_PERF_REPEATS",
                    "CTX_CODEX_INCREMENTAL_PERF_SLOW",
                    "CTX_CODEX_INCREMENTAL_PERF_NOOP_P95_MS",
                    "CTX_CODEX_INCREMENTAL_PERF_NOOP_US_PER_FILE"
                ]
            },
            "profiles": {
                "generation": {
                    "duration_ms": rounded(generation_ms)
                },
                "first_incremental_catch_up": {
                    "duration_ms": rounded(first_ms),
                    "catalog": {
                        "source_files": first.catalog.source_files,
                        "source_bytes": first.catalog.source_bytes,
                        "cached_sessions": first.catalog.cached_sessions,
                        "parsed_sessions": first.catalog.parsed_sessions,
                        "failed_sessions": first.catalog.failed_sessions
                    },
                    "pending_sessions": first.pending_sessions,
                    "imported_sessions": first.import.imported_sessions,
                    "imported_events": first.import.imported_events
                },
                "noop_incremental_catch_up": {
                    "timings": noop_stats.to_json(),
                    "repeats": repeats,
                    "cached_sessions": warmup.catalog.cached_sessions,
                    "parsed_sessions": warmup.catalog.parsed_sessions,
                    "pending_sessions": warmup.pending_sessions,
                    "p95_us_per_file": rounded(noop_us_per_file)
                }
            },
            "checks": checks
        });
        fs::write(
            &artifact_path,
            serde_json::to_vec_pretty(&artifact).unwrap(),
        )
        .unwrap();
        println!(
            "synthetic Codex incremental import perf artifact: {}",
            artifact_path.display()
        );

        assert!(
            passed,
            "synthetic Codex incremental import perf thresholds failed; see {}",
            artifact_path.display()
        );
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
    fn codex_search_event_mode_only_parses_search_relevant_lines() {
        let session_meta = br#"{"timestamp":"2026-06-24T01:00:00.000Z","type":"session_meta","payload":{"id":"s"}}"#;
        let user_message = br#"{"timestamp":"2026-06-24T01:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"question"}]}}"#;
        let assistant_message = br#"{"timestamp":"2026-06-24T01:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer"}]}}"#;
        let tool_call = br#"{"timestamp":"2026-06-24T01:00:03.000Z","type":"response_item","payload":{"type":"function_call","call_id":"call-1","name":"shell","arguments":"cargo test"}}"#;
        let tool_output = br#"{"timestamp":"2026-06-24T01:00:04.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-1","output":"passed"}}"#;
        let reasoning = br#"{"timestamp":"2026-06-24T01:00:05.000Z","type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"thinking"}]}}"#;
        let notice = br#"{"timestamp":"2026-06-24T01:00:06.000Z","type":"event_msg","payload":{"type":"task_complete"}}"#;

        for line in [
            session_meta.as_slice(),
            user_message.as_slice(),
            assistant_message.as_slice(),
        ] {
            assert!(should_parse_codex_session_line(
                line,
                CodexEventImportMode::Search
            ));
        }
        for line in [
            tool_call.as_slice(),
            tool_output.as_slice(),
            reasoning.as_slice(),
            notice.as_slice(),
        ] {
            assert!(!should_parse_codex_session_line(
                line,
                CodexEventImportMode::Search
            ));
            assert!(should_parse_codex_session_line(
                line,
                CodexEventImportMode::Rich
            ));
        }
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
    fn native_claude_projects_imports_jsonl_tree() {
        let temp = tempdir();
        let fixture = write_claude_smoke_fixture(&temp);
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_claude_projects_jsonl_tree(
            &fixture,
            &mut store,
            ClaudeProjectsImportOptions {
                machine_id: "test-machine".into(),
                source_path: Some(fixture.clone()),
                imported_at: DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                allow_partial_failures: true,
                ..ClaudeProjectsImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 0);
        assert_eq!(summary.imported_sessions, 2);
        assert_eq!(summary.imported_events, 5);
        assert_eq!(summary.imported_edges, 1);
        let parent_id = provider_session_uuid(CaptureProvider::Claude, "claude-native-parent");
        let child_id = provider_session_uuid(
            CaptureProvider::Claude,
            "claude-native-parent/subagents/agent-scout",
        );
        let child = store.get_session(child_id).unwrap();
        assert_eq!(child.parent_session_id, Some(parent_id));
        assert_eq!(child.agent_type, AgentType::Subagent);
        let events = store.events_for_session(parent_id).unwrap();
        assert!(events
            .iter()
            .any(|event| event.event_type == EventType::ToolCall));
        assert!(events
            .iter()
            .any(|event| event.event_type == EventType::ToolOutput));
    }

    #[test]
    fn antigravity_native_history_imports_transcripts_and_redacts_previews() {
        let temp = tempdir();
        let fixture = provider_history_fixture("antigravity/v1/brain");
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_antigravity_cli_history(
            &fixture,
            &mut store,
            AntigravityCliImportOptions {
                source_path: Some(fixture.clone()),
                allow_partial_failures: true,
                imported_at: "2026-06-24T14:00:00Z".parse().unwrap(),
                ..AntigravityCliImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 1, "{:?}", summary.failures);
        assert_eq!(summary.failures[0].line, 3);
        assert!(summary.failures[0].error.contains("malformed JSONL"));
        assert_eq!(summary.imported_sessions, 4);
        assert_eq!(summary.imported_events, 11);

        let success_session = provider_session_uuid(CaptureProvider::Antigravity, "agy-success");
        let success = store.events_for_session(success_session).unwrap();
        assert_eq!(success.len(), 3);
        let tool = success
            .iter()
            .find(|event| event.event_type == EventType::ToolCall)
            .unwrap();
        assert!(tool.payload["body"]["tool_calls"].is_array());
        assert!(tool.payload["body"]["tool_calls"][0]["args"].is_object());
        assert_eq!(
            tool.payload["body"]["tool_calls"][0]["args"]["CodeContent"].as_str(),
            Some("# Demo\n\nThis is a sanitized Antigravity fixture.\n")
        );
        assert_eq!(
            tool.sync.metadata["metadata"]["source_format"].as_str(),
            Some(ANTIGRAVITY_CLI_SOURCE_FORMAT)
        );
        let source_paths: Vec<String> = store
            .list_capture_sources()
            .unwrap()
            .into_iter()
            .filter_map(|source| source.descriptor.raw_source_path)
            .collect();
        assert!(source_paths
            .iter()
            .any(|path| path.contains("transcript_full.jsonl")));

        let future_session = provider_session_uuid(CaptureProvider::Antigravity, "agy-future");
        let future = store.events_for_session(future_session).unwrap();
        assert!(future
            .iter()
            .any(|event| event.event_type == EventType::Notice
                && event.payload["body"]["entry_type"] == "FUTURE_EVENT_KIND"));
        let rendered = serde_json::to_string(&future).unwrap();
        assert!(rendered.contains("[REDACTED]"));
        assert!(rendered.contains("[REDACTED_PATH]"));
        assert!(!rendered.contains("ghp_1234567890abcdef"));
        assert!(!rendered.contains("/home/example/private.txt"));
    }

    #[test]
    fn native_claude_projects_reports_malformed_jsonl() {
        let temp = tempdir();
        let fixture = temp.path().join("claude-malformed/projects/-workspace");
        fs::create_dir_all(&fixture).unwrap();
        fs::write(
            fixture.join("claude-malformed.jsonl"),
            concat!(
                "{\"sessionId\":\"claude-malformed\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"valid\"}}\n",
                "{\"sessionId\":\"claude-malformed\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"partial\"}]\n",
            ),
        )
        .unwrap();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_claude_projects_jsonl_tree(
            &fixture,
            &mut store,
            ClaudeProjectsImportOptions {
                allow_partial_failures: true,
                ..ClaudeProjectsImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 1);
        assert_eq!(summary.imported_sessions, 1);
        assert_eq!(summary.imported_events, 1);
        assert!(summary.failures[0].error.contains("malformed JSONL"));
    }

    #[test]
    fn native_opencode_imports_read_only_sqlite() {
        let temp = tempdir();
        let fixture = write_opencode_smoke_db(&temp, false);
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_opencode_sqlite(
            &fixture,
            &mut store,
            OpenCodeSqliteImportOptions {
                machine_id: "test-machine".into(),
                source_path: Some(fixture.clone()),
                imported_at: DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                allow_partial_failures: true,
                ..OpenCodeSqliteImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 0);
        assert_eq!(summary.imported_sessions, 2);
        assert_eq!(summary.imported_events, 3);
        assert_eq!(summary.imported_edges, 1);
        let parent_id = provider_session_uuid(CaptureProvider::OpenCode, "opencode-root");
        let child_id = provider_session_uuid(CaptureProvider::OpenCode, "opencode-child");
        assert_eq!(
            store.get_session(child_id).unwrap().parent_session_id,
            Some(parent_id)
        );
        let events = store.events_for_session(parent_id).unwrap();
        assert!(events
            .iter()
            .any(|event| event.event_type == EventType::ToolCall));
        assert_eq!(
            events[0].sync.metadata["source_format"].as_str(),
            Some(OPENCODE_SQLITE_SOURCE_FORMAT)
        );
    }

    #[test]
    fn native_opencode_synthesizes_session_message_seq_when_missing() {
        let temp = tempdir();
        let fixture = write_opencode_session_message_without_seq_db(&temp);
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_opencode_sqlite(
            &fixture,
            &mut store,
            OpenCodeSqliteImportOptions {
                allow_partial_failures: true,
                ..OpenCodeSqliteImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 0);
        assert_eq!(summary.imported_sessions, 1);
        assert_eq!(summary.imported_events, 2);

        let session_id = provider_session_uuid(CaptureProvider::OpenCode, "opencode-no-seq");
        let events = store.events_for_session(session_id).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].payload["body"]["session_message_seq"].as_i64(),
            Some(1)
        );
        assert_eq!(
            events[1].payload["body"]["session_message_seq"].as_i64(),
            Some(2)
        );
        assert_ne!(events[0].id, events[1].id);
    }

    #[test]
    fn native_opencode_reports_malformed_and_corrupt_db() {
        let temp = tempdir();
        let malformed = write_opencode_smoke_db(&temp, true);
        let corrupt = temp.path().join("corrupt-opencode.db");
        fs::write(&corrupt, b"not sqlite").unwrap();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_opencode_sqlite(
            &malformed,
            &mut store,
            OpenCodeSqliteImportOptions {
                allow_partial_failures: true,
                ..OpenCodeSqliteImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(summary.failed, 1);
        assert!(summary.failures[0].error.contains("invalid JSON"));

        let err =
            import_opencode_sqlite(&corrupt, &mut store, OpenCodeSqliteImportOptions::default())
                .unwrap_err();
        assert!(err.to_string().contains("not a database"));
    }

    #[test]
    fn native_opencode_accepts_schema_without_model_column() {
        let temp = tempdir();
        let fixture = write_opencode_current_schema_db(&temp, false);
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_opencode_sqlite(
            &fixture,
            &mut store,
            OpenCodeSqliteImportOptions {
                allow_partial_failures: true,
                ..OpenCodeSqliteImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 0);
        assert_eq!(summary.imported_sessions, 0);
        assert_eq!(summary.imported_events, 0);
    }

    #[test]
    fn native_opencode_imports_legacy_message_table_when_session_message_is_absent() {
        let temp = tempdir();
        let fixture = write_opencode_current_schema_db(&temp, true);
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_opencode_sqlite(
            &fixture,
            &mut store,
            OpenCodeSqliteImportOptions {
                allow_partial_failures: true,
                ..OpenCodeSqliteImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 0);
        assert_eq!(summary.imported_sessions, 1);
        assert_eq!(summary.imported_events, 1);

        let session_id = provider_session_uuid(CaptureProvider::OpenCode, "current-root");
        let events = store.events_for_session(session_id).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].sync.metadata["source_format"].as_str(),
            Some(OPENCODE_SQLITE_SOURCE_FORMAT)
        );
    }

    #[test]
    fn native_opencode_rejects_changed_message_schema_before_querying() {
        let temp = tempdir();
        let fixture = write_opencode_future_incomplete_schema_db(&temp);
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let err =
            import_opencode_sqlite(&fixture, &mut store, OpenCodeSqliteImportOptions::default())
                .unwrap_err();

        assert!(err
            .to_string()
            .contains("OpenCode SQLite message table missing required column(s): data"));
    }

    #[test]
    fn native_jsonl_tree_imports_gemini_droid_and_copilot_smokes() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let gemini = write_gemini_smoke_fixture(&temp);
        let gemini_summary = import_gemini_cli_history(
            &gemini,
            &mut store,
            GeminiCliImportOptions {
                allow_partial_failures: true,
                ..GeminiCliImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(gemini_summary.failed, 0);
        assert_eq!(gemini_summary.imported_sessions, 2);
        assert_eq!(gemini_summary.imported_edges, 1);

        let droid = write_droid_smoke_fixture(&temp);
        let droid_summary = import_factory_ai_droid_sessions(
            &droid,
            &mut store,
            FactoryAiDroidImportOptions {
                allow_partial_failures: true,
                ..FactoryAiDroidImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(droid_summary.failed, 0);
        assert_eq!(droid_summary.imported_sessions, 2);
        assert_eq!(droid_summary.imported_edges, 1);

        let copilot = write_copilot_smoke_fixture(&temp);
        let copilot_summary = import_copilot_cli_session_events(
            &copilot,
            &mut store,
            CopilotCliImportOptions {
                allow_partial_failures: true,
                ..CopilotCliImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(copilot_summary.failed, 0);
        assert_eq!(copilot_summary.imported_sessions, 1);
        assert_eq!(copilot_summary.imported_events, 5);
    }

    #[test]
    fn native_jsonl_tree_rejects_headerless_native_files() {
        let temp = tempdir();
        let root = temp.path().join("gemini/.gemini/tmp/project/chats");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("headerless.jsonl"),
            "{\"type\":\"user\",\"content\":\"missing session header\"}\n",
        )
        .unwrap();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let err = import_gemini_cli_history(
            temp.path().join("gemini/.gemini"),
            &mut store,
            GeminiCliImportOptions {
                allow_partial_failures: true,
                ..GeminiCliImportOptions::default()
            },
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("no Gemini CLI chat JSONL transcripts found under chats"));
    }

    fn write_claude_smoke_fixture(temp: &TempDir) -> PathBuf {
        let root = temp.path().join("claude/projects/-workspace");
        let subagents = root.join("claude-native-parent/subagents");
        fs::create_dir_all(&subagents).unwrap();
        fs::write(
            root.join("claude-native-parent.jsonl"),
            concat!(
                "{\"sessionId\":\"claude-native-parent\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\",\"version\":\"test\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"Run a smoke tool.\"}]},\"uuid\":\"claude-parent-1\"}\n",
                "{\"sessionId\":\"claude-native-parent\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"cwd\":\"/workspace\",\"version\":\"test\",\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"Bash\",\"input\":{\"command\":\"true\"}}]},\"uuid\":\"claude-parent-2\"}\n",
                "{\"sessionId\":\"claude-native-parent\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"cwd\":\"/workspace\",\"version\":\"test\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"tool-1\",\"content\":\"ok\"}]},\"uuid\":\"claude-parent-3\"}\n",
            ),
        )
        .unwrap();
        fs::write(
            subagents.join("agent-scout.jsonl"),
            concat!(
                "{\"sessionId\":\"claude-native-parent\",\"timestamp\":\"2026-06-24T12:00:03Z\",\"cwd\":\"/workspace\",\"version\":\"test\",\"isSidechain\":true,\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"inspect\"},\"uuid\":\"claude-child-1\"}\n",
                "{\"sessionId\":\"claude-native-parent\",\"timestamp\":\"2026-06-24T12:00:04Z\",\"cwd\":\"/workspace\",\"version\":\"test\",\"isSidechain\":true,\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":\"done\"},\"uuid\":\"claude-child-2\"}\n",
            ),
        )
        .unwrap();
        temp.path().join("claude/projects")
    }

    fn write_opencode_smoke_db(temp: &TempDir, malformed: bool) -> PathBuf {
        let path = temp.path().join(if malformed {
            "opencode-malformed.db"
        } else {
            "opencode.db"
        });
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "create table session (
                id text primary key, parent_id text, title text not null, directory text not null,
                model text, agent text, time_created integer not null, time_updated integer not null,
                tokens_input integer not null, tokens_output integer not null,
                tokens_reasoning integer not null, tokens_cache_read integer not null,
                tokens_cache_write integer not null
            );
            create table session_message (
                id text primary key, session_id text not null, type text not null, seq integer not null,
                time_created integer not null, time_updated integer not null, data text not null
            );",
        )
        .unwrap();
        conn.execute(
            "insert into session values (?1, null, 'root', '/workspace', '{\"id\":\"test\"}', 'build', 1782259200000, 1782259200000, 1, 1, 0, 0, 0)",
            ["opencode-root"],
        )
        .unwrap();
        conn.execute(
            "insert into session values (?1, ?2, 'child', '/workspace', '{\"id\":\"test\"}', 'scout', 1782259201000, 1782259201000, 1, 1, 0, 0, 0)",
            ["opencode-child", "opencode-root"],
        )
        .unwrap();
        conn.execute(
            "insert into session_message values (?1, ?2, 'user', 1, 1782259200000, 1782259200000, ?3)",
            ["msg-user", "opencode-root", "{\"time\":{\"created\":1782259200000},\"text\":\"inspect\"}"],
        )
        .unwrap();
        conn.execute(
            "insert into session_message values (?1, ?2, 'assistant', 2, 1782259201000, 1782259201000, ?3)",
            ["msg-assistant", "opencode-root", "{\"time\":{\"created\":1782259201000},\"content\":[{\"type\":\"tool\",\"name\":\"bash\"}]}"],
        )
        .unwrap();
        let child_data = if malformed {
            "{\"time\":{\"created\":1782259202000},\"text\":"
        } else {
            "{\"time\":{\"created\":1782259202000},\"text\":\"child done\"}"
        };
        conn.execute(
            "insert into session_message values (?1, ?2, 'assistant', 1, 1782259202000, 1782259202000, ?3)",
            ["msg-child", "opencode-child", child_data],
        )
        .unwrap();
        path
    }

    fn write_opencode_session_message_without_seq_db(temp: &TempDir) -> PathBuf {
        let path = temp.path().join("opencode-no-seq.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "create table session (
                id text primary key, title text not null, directory text not null,
                time_created integer not null, time_updated integer not null
            );
            create table session_message (
                id text primary key, session_id text not null, type text not null,
                time_created integer not null, time_updated integer not null, data text not null
            );",
        )
        .unwrap();
        conn.execute(
            "insert into session values (?1, 'no seq', '/workspace', 1782259200000, 1782259200000)",
            ["opencode-no-seq"],
        )
        .unwrap();
        conn.execute(
            "insert into session_message values (?1, ?2, 'user', 1782259200000, 1782259200000, ?3)",
            [
                "msg-no-seq-user",
                "opencode-no-seq",
                "{\"time\":{\"created\":1782259200000},\"text\":\"first no seq\"}",
            ],
        )
        .unwrap();
        conn.execute(
            "insert into session_message values (?1, ?2, 'assistant', 1782259201000, 1782259201000, ?3)",
            [
                "msg-no-seq-assistant",
                "opencode-no-seq",
                "{\"time\":{\"created\":1782259201000},\"text\":\"second no seq\"}",
            ],
        )
        .unwrap();
        path
    }

    fn write_opencode_current_schema_db(temp: &TempDir, with_message: bool) -> PathBuf {
        let path = temp.path().join(if with_message {
            "opencode-current-message.db"
        } else {
            "opencode-current-empty.db"
        });
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "create table session (
                id text primary key,
                project_id text not null,
                parent_id text,
                slug text not null,
                directory text not null,
                title text not null,
                version text not null,
                share_url text,
                summary_additions integer,
                summary_deletions integer,
                summary_files integer,
                summary_diffs text,
                revert text,
                permission text,
                time_created integer not null,
                time_updated integer not null,
                time_compacting integer,
                time_archived integer,
                workspace_id text
            );
            create table session_entry (
                id text primary key,
                session_id text not null,
                type text not null,
                time_created integer not null,
                time_updated integer not null,
                data text not null
            );
            create table message (
                id text primary key,
                session_id text not null,
                time_created integer not null,
                time_updated integer not null,
                data text not null
            );
            create table part (
                id text primary key,
                message_id text not null,
                session_id text not null,
                type text not null,
                time_created integer not null,
                time_updated integer not null,
                data text not null
            );",
        )
        .unwrap();

        if with_message {
            conn.execute(
                "insert into session (
                    id, project_id, parent_id, slug, directory, title, version, permission,
                    time_created, time_updated
                ) values (?1, 'project-1', null, 'current-root', '/workspace', 'current root',
                    '0.8.0', 'default', 1782259200000, 1782259200000)",
                ["current-root"],
            )
            .unwrap();
            conn.execute(
                "insert into message values (?1, ?2, 1782259200000, 1782259200000, ?3)",
                [
                    "current-message-1",
                    "current-root",
                    "{\"role\":\"user\",\"time\":{\"created\":1782259200000},\"text\":\"legacy hello\"}",
                ],
            )
            .unwrap();
        }

        path
    }

    fn write_opencode_future_incomplete_schema_db(temp: &TempDir) -> PathBuf {
        let path = temp.path().join("opencode-future-incomplete.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "create table session (
                id text primary key,
                project_id text not null,
                slug text not null,
                directory text not null,
                title text not null,
                version text not null,
                time_created integer not null,
                time_updated integer not null
            );
            create table message (
                id text primary key,
                session_id text not null,
                time_created integer not null,
                time_updated integer not null
            );",
        )
        .unwrap();
        conn.execute(
            "insert into session (
                id, project_id, slug, directory, title, version, time_created, time_updated
            ) values ('future-root', 'project-1', 'future-root', '/workspace', 'future root',
                '0.9.0', 1782259200000, 1782259200000)",
            [],
        )
        .unwrap();
        conn.execute(
            "insert into message values ('future-message-1', 'future-root', 1782259200000,
                1782259200000)",
            [],
        )
        .unwrap();
        path
    }

    fn write_gemini_smoke_fixture(temp: &TempDir) -> PathBuf {
        let chats = temp.path().join("gemini/.gemini/tmp/project/chats");
        let child_dir = chats.join("gemini-root");
        fs::create_dir_all(&child_dir).unwrap();
        fs::write(
            chats.join("session-root.jsonl"),
            concat!(
                "{\"sessionId\":\"gemini-root\",\"startTime\":\"2026-06-24T12:00:00Z\",\"kind\":\"main\",\"directories\":[\"/workspace\"]}\n",
                "{\"id\":\"gemini-user\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"type\":\"user\",\"content\":\"hi\"}\n",
                "{\"id\":\"gemini-tool\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"type\":\"gemini\",\"toolCalls\":[{\"id\":\"call-1\",\"name\":\"run_subagent\"}]}\n",
            ),
        )
        .unwrap();
        fs::write(
            child_dir.join("gemini-child.jsonl"),
            concat!(
                "{\"sessionId\":\"gemini-child\",\"startTime\":\"2026-06-24T12:00:03Z\",\"kind\":\"subagent\",\"directories\":[\"/workspace\"]}\n",
                "{\"id\":\"gemini-child-user\",\"timestamp\":\"2026-06-24T12:00:04Z\",\"type\":\"user\",\"content\":\"inspect\"}\n",
            ),
        )
        .unwrap();
        temp.path().join("gemini/.gemini")
    }

    fn write_droid_smoke_fixture(temp: &TempDir) -> PathBuf {
        let root = temp.path().join("droid/sessions/project");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("droid-root.jsonl"),
            concat!(
                "{\"type\":\"session_start\",\"sessionId\":\"droid-root\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\",\"model\":\"factory/droid\"}\n",
                "{\"type\":\"message\",\"id\":\"droid-user\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"delegate\"}]}\n",
                "{\"type\":\"message\",\"id\":\"droid-tool\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"droid_worker\"}]}\n",
            ),
        )
        .unwrap();
        fs::write(
            root.join("droid-child.jsonl"),
            concat!(
                "{\"type\":\"session_start\",\"sessionId\":\"droid-child\",\"timestamp\":\"2026-06-24T12:00:03Z\",\"cwd\":\"/workspace\",\"model\":\"factory/droid\",\"parent\":\"droid-root\",\"decompSessionType\":\"worker\"}\n",
                "{\"type\":\"message\",\"id\":\"droid-child-user\",\"timestamp\":\"2026-06-24T12:00:04Z\",\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"inspect\"}]}\n",
            ),
        )
        .unwrap();
        temp.path().join("droid/sessions")
    }

    fn write_copilot_smoke_fixture(temp: &TempDir) -> PathBuf {
        let root = temp.path().join("copilot/session-state/copilot-root");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("events.jsonl"),
            concat!(
                "{\"id\":\"copilot-1\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"type\":\"session.start\",\"data\":{\"sessionId\":\"copilot-root\",\"startTime\":\"2026-06-24T12:00:00Z\",\"selectedModel\":\"gpt-5-mini\",\"context\":{\"cwd\":\"/workspace\"}}}\n",
                "{\"id\":\"copilot-2\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"type\":\"user.message\",\"data\":{\"content\":\"status\"}}\n",
                "{\"id\":\"copilot-3\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"type\":\"assistant.message\",\"data\":{\"content\":\"running\",\"toolRequests\":[{\"toolCallId\":\"tool-1\",\"name\":\"bash\"}]}}\n",
                "{\"id\":\"copilot-4\",\"timestamp\":\"2026-06-24T12:00:03Z\",\"type\":\"tool.execution_start\",\"data\":{\"toolCallId\":\"tool-1\",\"toolName\":\"bash\"}}\n",
                "{\"id\":\"copilot-5\",\"timestamp\":\"2026-06-24T12:00:04Z\",\"type\":\"tool.execution_complete\",\"data\":{\"toolCallId\":\"tool-1\",\"success\":true,\"result\":{\"content\":\"ok\"}}}\n",
            ),
        )
        .unwrap();
        temp.path().join("copilot/session-state")
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
    fn provider_fixture_replay_is_idempotent_for_native_supported_providers() {
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
    fn provider_fixture_replay_supports_search_only_temp_fixtures() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        for (
            fixture_name,
            provider,
            external_session_id,
            fixture_sessions,
            fixture_events,
            fixture_edges,
        ) in [
            (
                "copilot_cli.jsonl",
                CaptureProvider::CopilotCli,
                "copilot-cli-session-1",
                1,
                2,
                0,
            ),
            (
                "factory_ai_droid.jsonl",
                CaptureProvider::FactoryAiDroid,
                "factory-ai-droid-session-1",
                2,
                3,
                1,
            ),
        ] {
            let fixture = provider_fixture(fixture_name);
            let (fixture, sessions, events, edges) = if fixture.exists() {
                (fixture, fixture_sessions, fixture_events, fixture_edges)
            } else {
                (
                    write_minimal_provider_fixture(&temp, provider, external_session_id),
                    1,
                    1,
                    0,
                )
            };
            let mut options = fixed_import_options(fixture.clone());
            options.expected_provider = Some(provider);

            let first =
                import_provider_fixture_jsonl(&fixture, &mut store, options.clone()).unwrap();
            assert_eq!(first.failed, 0, "{provider}: {:?}", first.failures);
            assert_eq!(first.imported_sessions, sessions, "{provider}");
            assert_eq!(first.imported_events, events, "{provider}");
            assert_eq!(first.imported_edges, edges, "{provider}");

            let second = import_provider_fixture_jsonl(&fixture, &mut store, options).unwrap();
            assert_eq!(second.failed, 0, "{provider}: {:?}", second.failures);
            assert_eq!(second.imported_sessions, 0, "{provider}");
            assert_eq!(second.imported_events, 0, "{provider}");
            assert_eq!(second.imported_edges, 0, "{provider}");
            assert_eq!(second.skipped_sessions, sessions, "{provider}");
            assert_eq!(second.skipped_events, events, "{provider}");
            assert_eq!(second.skipped_edges, edges, "{provider}");

            let session_id = provider_session_uuid(provider, external_session_id);
            let session = store.get_session(session_id).unwrap();
            assert_eq!(session.provider, provider);
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
