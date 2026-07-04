use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, NaiveDateTime, Utc};
use ctx_history_core::{
    inbox_dir as core_inbox_dir, new_id, utc_now, AgentType, CaptureEnvelope, CaptureProvider,
    CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, Confidence,
    CtxHistoryJsonlEdgeRecord, CtxHistoryJsonlEventRecord, CtxHistoryJsonlFileTouchRecord,
    CtxHistoryJsonlRecord, CtxHistoryJsonlSessionRecord, CtxHistoryJsonlSourceRecord,
    EntityTimestamps, Event, EventRole, EventType, Fidelity, FileChangeKind, FileTouched,
    HistoryRecord, ProviderCaptureEnvelope, ProviderCursorCheckpoint, ProviderCursorRange,
    ProviderEventEnvelope, ProviderRawRetention, ProviderRedactionBoundary,
    ProviderSessionEnvelope, ProviderSourceEnvelope, ProviderSourceTrust, RedactionState, Run,
    RunStatus, RunType, Session, SessionEdge, SessionEdgeType, SessionHistoryArchive,
    SessionStatus, SyncCursor, SyncMetadata, SyncState, Visibility,
    CTX_HISTORY_JSONL_V1_SCHEMA_VERSION, PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use ctx_history_store::{CatalogSession, Store, StoreError};
use rusqlite::{limits::Limit, Connection, OpenFlags, OptionalExtension};
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
const MAX_PROVIDER_JSONL_LINE_BYTES: usize = 16 * 1024 * 1024;
const MAX_PROVIDER_SQLITE_VALUE_BYTES: usize = MAX_PROVIDER_JSONL_LINE_BYTES;
const MAX_OPENCLAW_SESSION_INDEX_BYTES: usize = 1024 * 1024;
const MAX_OPENCLAW_SESSION_INDEX_PATHS: usize = 256;
const MAX_OPENCLAW_SESSION_INDEX_VISITED_PATHS: usize = 4096;
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
        let unix_ms = utc_now().timestamp_millis();
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
            occurred_at: utc_now(),
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
            imported_at: utc_now(),
            history_record_id: None,
            expected_provider: None,
            allow_partial_failures: false,
            source_format: "normalized_provider_fixture_jsonl".to_owned(),
            fidelity: Fidelity::Imported,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CustomHistoryJsonlV1ImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for CustomHistoryJsonlV1ImportOptions {
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
            imported_at: utc_now(),
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
            imported_at: utc_now(),
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
            imported_at: utc_now(),
            history_record_id: None,
            allow_partial_failures: false,
        }
    }
}

pub type KiloSqliteImportOptions = OpenCodeSqliteImportOptions;

#[derive(Debug, Clone)]
pub struct OpenClawImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for OpenClawImportOptions {
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
pub struct HermesSqliteImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for HermesSqliteImportOptions {
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
pub struct NanoClawImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for NanoClawImportOptions {
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
pub struct AstrBotSqliteImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for AstrBotSqliteImportOptions {
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
pub struct ShelleySqliteImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for ShelleySqliteImportOptions {
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
pub struct ContinueCliImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for ContinueCliImportOptions {
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
pub struct OpenHandsImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for OpenHandsImportOptions {
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
            imported_at: utc_now(),
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
            imported_at: utc_now(),
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
            imported_at: utc_now(),
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
            imported_at: utc_now(),
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
            imported_at: utc_now(),
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

#[derive(Debug, Clone, Default)]
struct CodexSessionLineCapture {
    event: Option<ProviderEventEnvelope>,
    files_touched: Vec<(usize, ProviderFileTouchedEnvelope)>,
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
            imported_at: utc_now(),
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
    pub files_touched: Vec<(usize, ProviderFileTouchedEnvelope)>,
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
pub struct KiloSqliteAdapter;

#[derive(Debug, Clone, Copy, Default)]
pub struct OpenClawJsonlAdapter;

#[derive(Debug, Clone, Copy, Default)]
pub struct HermesSqliteAdapter;

#[derive(Debug, Clone, Copy, Default)]
pub struct NanoClawProjectAdapter;

#[derive(Debug, Clone, Copy, Default)]
pub struct AstrBotSqliteAdapter;

#[derive(Debug, Clone, Copy, Default)]
pub struct ShelleySqliteAdapter;

#[derive(Debug, Clone, Copy, Default)]
pub struct ContinueCliSessionsAdapter;

#[derive(Debug, Clone, Copy, Default)]
pub struct OpenHandsFileEventsAdapter;

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
        let mut reader = BufReader::new(file);
        let mut result = ProviderNormalizationResult::default();
        let mut line = Vec::new();
        let mut line_number = 0usize;

        while read_provider_jsonl_line(&mut reader, &mut line)? {
            line_number += 1;
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }

            let fixture: ProviderFixtureLine = match serde_json::from_slice(&line) {
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

fn codex_session_line_may_touch_file(line: &[u8]) -> bool {
    contains_bytes(line, br#""type":"response_item""#)
        && (contains_bytes(line, b"apply_patch")
            || contains_bytes(line, b"*** Begin Patch")
            || contains_bytes(line, b"write_file")
            || contains_bytes(line, b"edit_file")
            || contains_bytes(line, b"str_replace")
            || contains_bytes(line, b"file_path")
            || contains_bytes(line, b"TargetFile"))
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
        normalize_pi_session_jsonl_path(path, context)
    }
}

fn normalize_pi_session_jsonl_path(
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

fn normalize_pi_session_jsonl_file(
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
            merged.files_touched.append(&mut result.files_touched);
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
        normalize_opencode_sqlite(path, context, &OPENCODE_SQLITE_DIALECT)
    }
}

impl ProviderCaptureAdapter for KiloSqliteAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Kilo
    }

    fn source_format(&self) -> &str {
        KILO_SQLITE_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        ensure_regular_provider_transcript_file(path)?;
        normalize_opencode_sqlite(path, context, &KILO_SQLITE_DIALECT)
    }
}

impl ProviderCaptureAdapter for OpenClawJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::OpenClaw
    }

    fn source_format(&self) -> &str {
        OPENCLAW_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_openclaw_history(path, context)
    }
}

impl ProviderCaptureAdapter for HermesSqliteAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Hermes
    }

    fn source_format(&self) -> &str {
        HERMES_SQLITE_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_hermes_sqlite(path, context)
    }
}

impl ProviderCaptureAdapter for NanoClawProjectAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::NanoClaw
    }

    fn source_format(&self) -> &str {
        NANOCLAW_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_nanoclaw_project(path, context)
    }
}

impl ProviderCaptureAdapter for AstrBotSqliteAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::AstrBot
    }

    fn source_format(&self) -> &str {
        ASTRBOT_SQLITE_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_astrbot_sqlite(path, context)
    }
}

impl ProviderCaptureAdapter for ShelleySqliteAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Shelley
    }

    fn source_format(&self) -> &str {
        SHELLEY_SQLITE_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_shelley_sqlite(path, context)
    }
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

impl ProviderCaptureAdapter for OpenHandsFileEventsAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::OpenHands
    }

    fn source_format(&self) -> &str {
        OPENHANDS_FILE_EVENTS_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_openhands_file_events(path, context)
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
    let mut reader = BufReader::new(file);
    let mut envelopes = Vec::new();
    let mut line = Vec::new();
    let mut line_number = 0usize;

    while read_provider_jsonl_line(&mut reader, &mut line)? {
        line_number += 1;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let envelope: CaptureEnvelope =
            serde_json::from_slice(&line).map_err(|source| CaptureError::InvalidJsonLine {
                path: path.to_path_buf(),
                line: line_number,
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

pub fn import_custom_history_jsonl_v1(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CustomHistoryJsonlV1ImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = normalize_custom_history_jsonl_v1(
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
    if normalization.provider.summary.failed > 0 && !options.allow_partial_failures {
        return Ok(normalization.provider.summary);
    }

    let mut summary = import_normalized_provider_captures(
        store,
        normalization.provider,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
        },
    )?;
    import_custom_history_edges(
        store,
        &normalization.edges,
        options.history_record_id,
        options.allow_partial_failures,
        &mut summary,
    )?;
    import_custom_history_source_cursors(store, &normalization.source_cursors)?;
    Ok(summary)
}

pub fn import_custom_history_jsonl_v1_reader(
    reader: impl BufRead,
    store: &mut Store,
    options: CustomHistoryJsonlV1ImportOptions,
) -> Result<ProviderImportSummary> {
    let normalization = normalize_custom_history_jsonl_v1_reader(
        reader,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            tool_output_mode: CodexToolOutputMode::Full,
            event_mode: CodexEventImportMode::Rich,
            include_notices: true,
        },
    )?;
    if normalization.provider.summary.failed > 0 && !options.allow_partial_failures {
        return Ok(normalization.provider.summary);
    }

    let mut summary = import_normalized_provider_captures(
        store,
        normalization.provider,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
        },
    )?;
    import_custom_history_edges(
        store,
        &normalization.edges,
        options.history_record_id,
        options.allow_partial_failures,
        &mut summary,
    )?;
    import_custom_history_source_cursors(store, &normalization.source_cursors)?;
    Ok(summary)
}

pub fn validate_custom_history_jsonl_v1(path: impl AsRef<Path>) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let normalization = normalize_custom_history_jsonl_v1(
        path,
        &ProviderAdapterContext {
            source_path: Some(path.to_path_buf()),
            ..ProviderAdapterContext::default()
        },
    )?;
    Ok(normalization.provider.summary)
}

pub fn validate_custom_history_jsonl_v1_reader(
    reader: impl BufRead,
) -> Result<ProviderImportSummary> {
    let normalization =
        normalize_custom_history_jsonl_v1_reader(reader, &ProviderAdapterContext::default())?;
    Ok(normalization.provider.summary)
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
    let raw_source_path = context
        .source_path
        .as_ref()
        .map(|path| path.display().to_string());

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

        if !read_provider_jsonl_line(&mut reader, &mut line)? {
            return Ok(summary);
        }
        line_number += 1;
        let read = line.len();
        position = position.saturating_add(read as u64);
        let header_value: Value = serde_json::from_slice(&line)?;
        let header = codex_session_header(header_value)?;

        while position < start_offset {
            if !read_provider_jsonl_line(&mut reader, &mut line)? {
                return Ok(summary);
            }
            line_number += 1;
            let read = line.len();
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
        while read_provider_jsonl_line(&mut reader, &mut line)? {
            line_number += 1;
            let read = line.len();
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
            let occurred_at = match codex_session_line_timestamp(&value, header.timestamp) {
                Ok(occurred_at) => occurred_at,
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
            let mut line_capture = codex_session_line_capture(
                &header,
                &value,
                &mut call_contexts,
                CodexSessionLineContext {
                    line_number,
                    occurred_at,
                    tool_output_mode: options.tool_output_mode,
                    event_mode: options.event_mode,
                    raw_source_path: raw_source_path.as_deref(),
                },
            );
            if let Some(event) = line_capture.event.take() {
                if !options.include_notices && event.event_type == EventType::Notice {
                    summary.skipped += 1;
                    summary.skipped_events += 1;
                } else {
                    summary.merge(import_codex_provider_event_fast(
                        store,
                        &header,
                        &event,
                        options.history_record_id,
                        line_number,
                        context.imported_at,
                        raw_source_path.as_deref(),
                    )?);
                }
            }
            for (_, file) in line_capture.files_touched {
                import_provider_file_touched_line(store, &file, &import_options)?;
            }
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
        let mut chunk_files_touched = Vec::new();
        let mut chunk_bytes = 0u64;
        for (_, path, normalization) in normalized {
            chunk_bytes = chunk_bytes.saturating_add(
                fs::metadata(&path)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0),
            );
            chunk_summary.merge(normalization.summary);
            chunk_captures.extend(normalization.captures);
            chunk_files_touched.extend(normalization.files_touched);
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
            chunk_files_touched,
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
    let raw_source_path = context
        .source_path
        .as_ref()
        .map(|path| path.display().to_string());

    let mut header = None;
    let mut call_contexts: BTreeMap<String, CodexToolCallContext> = BTreeMap::new();
    let mut line_number = 0usize;
    let mut line = Vec::new();
    while read_provider_jsonl_line(&mut reader, &mut line)? {
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
        let occurred_at = match codex_session_line_timestamp(&value, header.timestamp) {
            Ok(occurred_at) => occurred_at,
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
        let mut line_capture = codex_session_line_capture(
            header,
            &value,
            &mut call_contexts,
            CodexSessionLineContext {
                line_number,
                occurred_at,
                tool_output_mode: options.tool_output_mode,
                event_mode: options.event_mode,
                raw_source_path: raw_source_path.as_deref(),
            },
        );
        if let Some(event) = line_capture.event.take() {
            if !options.include_notices && event.event_type == EventType::Notice {
                summary.skipped += 1;
                summary.skipped_events += 1;
            } else {
                let line_summary = import_codex_provider_event_fast(
                    store,
                    header,
                    &event,
                    options.history_record_id,
                    line_number,
                    context.imported_at,
                    raw_source_path.as_deref(),
                )?;
                summary.merge(line_summary);
            }
        }
        for (_, file) in line_capture.files_touched {
            import_provider_file_touched_line(store, &file, &import_options)?;
        }
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
    raw_source_path: Option<&str>,
) -> Result<ProviderImportSummary> {
    let mut summary = ProviderImportSummary::default();
    let provider = CaptureProvider::Codex;
    let session_id = provider_session_uuid(provider, &header.id);
    let source_id = provider_scoped_source_uuid(
        provider,
        &header.id,
        CODEX_SESSION_SOURCE_FORMAT,
        raw_source_path,
    );
    let (payload, redacted_payload) = sanitize_value(event.payload.clone());
    let (event_metadata, redacted_metadata) = sanitize_value(event.metadata.clone());
    let event_hash = event
        .provider_event_hash
        .clone()
        .unwrap_or(compute_payload_hash(&payload)?);
    let event_identity = provider_event_import_identity(
        store,
        provider,
        &header.id,
        source_id,
        event.provider_event_index,
        event.provider_event_index,
        &event_hash,
        None,
    )?;
    let command_run = provider_command_run_from_event(ProviderCommandRunInput {
        provider,
        provider_session_id: &header.id,
        session_id,
        source_id,
        run_source_id: event_identity.run_source_id,
        history_record_id,
        event,
        payload: &payload,
        event_hash: &event_hash,
    })?;
    let normalized_event = Event {
        id: event_identity.id,
        seq: event_identity.seq,
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
        dedupe_key: Some(event_identity.dedupe_key),
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

fn read_codex_session_meta(path: &Path) -> Result<Option<Value>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    for _ in 0..32 {
        if !read_provider_jsonl_line(&mut reader, &mut line)? {
            break;
        }
        if !line.contains(&b'{') || !contains_bytes(&line, br#""session_meta""#) {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
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

pub fn import_kilo_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: KiloSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = KiloSqliteAdapter.normalize_path(
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

pub fn import_openclaw_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: OpenClawImportOptions,
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
        OpenClawJsonlAdapter,
    )
}

pub fn import_hermes_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: HermesSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = HermesSqliteAdapter.normalize_path(
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

pub fn import_nanoclaw_project(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: NanoClawImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = NanoClawProjectAdapter.normalize_path(
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

pub fn import_astrbot_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: AstrBotSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = AstrBotSqliteAdapter.normalize_path(
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

pub fn import_shelley_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: ShelleySqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = ShelleySqliteAdapter.normalize_path(
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

pub fn import_continue_cli_sessions(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: ContinueCliImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = ContinueCliSessionsAdapter.normalize_path(
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

pub fn import_openhands_file_events(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: OpenHandsImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = OpenHandsFileEventsAdapter.normalize_path(
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
    let ProviderNormalizationResult {
        summary,
        captures,
        files_touched,
    } = normalization;
    import_provider_capture_lines(store, options, summary, captures, files_touched)
}

const CODEX_SESSION_SOURCE_FORMAT: &str = "codex_session_jsonl";
const CLAUDE_PROJECTS_SOURCE_FORMAT: &str = "claude_projects_jsonl_tree";
const OPENCODE_SQLITE_SOURCE_FORMAT: &str = "opencode_sqlite";
const KILO_SQLITE_SOURCE_FORMAT: &str = "kilo_sqlite";
const OPENCLAW_SOURCE_FORMAT: &str = "openclaw_session_jsonl_tree";
const HERMES_SQLITE_SOURCE_FORMAT: &str = "hermes_state_sqlite";
const NANOCLAW_SOURCE_FORMAT: &str = "nanoclaw_project";
const ASTRBOT_SQLITE_SOURCE_FORMAT: &str = "astrbot_data_v4_sqlite";
const SHELLEY_SQLITE_SOURCE_FORMAT: &str = "shelley_sqlite";
const CONTINUE_CLI_SOURCE_FORMAT: &str = "continue_cli_sessions_json";
const OPENHANDS_FILE_EVENTS_SOURCE_FORMAT: &str = "openhands_file_events";
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

#[derive(Debug, Clone, Copy)]
struct OpenCodeSqliteDialect {
    provider: CaptureProvider,
    display_name: &'static str,
    source_format: &'static str,
    session_time_created_field: &'static str,
    session_message_seq_field: &'static str,
    session_message_time_created_field: &'static str,
    event_time_created_field: &'static str,
}

const OPENCODE_SQLITE_DIALECT: OpenCodeSqliteDialect = OpenCodeSqliteDialect {
    provider: CaptureProvider::OpenCode,
    display_name: "OpenCode",
    source_format: OPENCODE_SQLITE_SOURCE_FORMAT,
    session_time_created_field: "OpenCode session time_created",
    session_message_seq_field: "OpenCode session_message seq",
    session_message_time_created_field: "OpenCode session_message time_created",
    event_time_created_field: "OpenCode event time.created",
};

const KILO_SQLITE_DIALECT: OpenCodeSqliteDialect = OpenCodeSqliteDialect {
    provider: CaptureProvider::Kilo,
    display_name: "Kilo",
    source_format: KILO_SQLITE_SOURCE_FORMAT,
    session_time_created_field: "Kilo session time_created",
    session_message_seq_field: "Kilo session_message seq",
    session_message_time_created_field: "Kilo session_message time_created",
    event_time_created_field: "Kilo event time.created",
};

#[derive(Debug, Clone, Default)]
struct CustomHistoryJsonlV1NormalizationResult {
    provider: ProviderNormalizationResult,
    edges: Vec<(usize, CustomHistoryJsonlV1EdgeImport)>,
    source_cursors: Vec<CustomHistoryJsonlV1SourceCursorImport>,
}

#[derive(Debug, Clone)]
struct CustomHistoryJsonlV1SourceCursorImport {
    machine_id: String,
    checkpoint: ProviderCursorCheckpoint,
}

#[derive(Debug, Clone)]
struct CustomHistoryJsonlV1EdgeImport {
    provider_key: String,
    source_id: String,
    source_format: String,
    raw_source_path: Option<String>,
    from_provider_session_id: String,
    to_provider_session_id: String,
    edge_id: Option<String>,
    edge_type: SessionEdgeType,
    confidence: Confidence,
    occurred_at: DateTime<Utc>,
    fidelity: Fidelity,
    metadata: Value,
}

fn normalize_custom_history_jsonl_v1(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<CustomHistoryJsonlV1NormalizationResult> {
    ensure_regular_provider_transcript_file(path)?;
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    normalize_custom_history_jsonl_v1_reader(reader, context)
}

fn normalize_custom_history_jsonl_v1_reader(
    reader: impl BufRead,
    context: &ProviderAdapterContext,
) -> Result<CustomHistoryJsonlV1NormalizationResult> {
    let mut reader = reader;
    let mut summary = ProviderImportSummary::default();
    let mut records = Vec::new();
    let mut line = Vec::new();
    let mut line_number = 0usize;

    while read_provider_jsonl_line(&mut reader, &mut line)? {
        line_number += 1;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match serde_json::from_slice::<CtxHistoryJsonlRecord>(&line) {
            Ok(record) => records.push((line_number, record)),
            Err(err) => push_provider_import_failure(&mut summary, line_number, err.to_string()),
        }
    }

    if summary.failed > 0 {
        return Ok(custom_history_failed_normalization(summary));
    }

    let mut manifest_line = None;
    let mut sources = BTreeMap::<String, (usize, CtxHistoryJsonlSourceRecord)>::new();
    let mut sessions = BTreeMap::<(String, String), (usize, CtxHistoryJsonlSessionRecord)>::new();
    let mut events = Vec::<(usize, CtxHistoryJsonlEventRecord)>::new();
    let mut event_keys = BTreeSet::<(String, String, u64)>::new();
    let mut file_touches = Vec::<(usize, CtxHistoryJsonlFileTouchRecord)>::new();
    let mut touch_keys = BTreeSet::<(String, String, u64)>::new();
    let mut edges = Vec::<(usize, CtxHistoryJsonlEdgeRecord)>::new();
    let mut edge_keys = BTreeSet::<(String, String, String, String)>::new();

    for (line_number, record) in records {
        match record {
            CtxHistoryJsonlRecord::Manifest(manifest) => {
                if manifest.schema_version != CTX_HISTORY_JSONL_V1_SCHEMA_VERSION {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        format!(
                            "unsupported custom history schema version `{}`",
                            manifest.schema_version
                        ),
                    );
                }
                if manifest_line.replace(line_number).is_some() {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate manifest record".to_owned(),
                    );
                }
            }
            CtxHistoryJsonlRecord::Source(source) => {
                validate_custom_source_record(&mut summary, line_number, &source);
                if sources
                    .insert(source.source_id.clone(), (line_number, source))
                    .is_some()
                {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate source_id".to_owned(),
                    );
                }
            }
            CtxHistoryJsonlRecord::Session(session) => {
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "source_id",
                    &session.source_id,
                );
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "session_id",
                    &session.session_id,
                );
                let key = (session.source_id.clone(), session.session_id.clone());
                if sessions.insert(key, (line_number, session)).is_some() {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate session record".to_owned(),
                    );
                }
            }
            CtxHistoryJsonlRecord::Event(event) => {
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "source_id",
                    &event.source_id,
                );
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "session_id",
                    &event.session_id,
                );
                let key = (
                    event.source_id.clone(),
                    event.session_id.clone(),
                    event.event_index,
                );
                if !event_keys.insert(key) {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate event_index for session".to_owned(),
                    );
                }
                events.push((line_number, event));
            }
            CtxHistoryJsonlRecord::FileTouch(file_touch) => {
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "source_id",
                    &file_touch.source_id,
                );
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "session_id",
                    &file_touch.session_id,
                );
                if file_touch.path.trim().is_empty() {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "file_touch path must not be empty".to_owned(),
                    );
                }
                let key = (
                    file_touch.source_id.clone(),
                    file_touch.session_id.clone(),
                    file_touch.touch_index,
                );
                if !touch_keys.insert(key) {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate touch_index for session".to_owned(),
                    );
                }
                file_touches.push((line_number, file_touch));
            }
            CtxHistoryJsonlRecord::Edge(edge) => {
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "source_id",
                    &edge.source_id,
                );
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "from_session_id",
                    &edge.from_session_id,
                );
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "to_session_id",
                    &edge.to_session_id,
                );
                let edge_key = edge.edge_id.clone().unwrap_or_else(|| {
                    format!(
                        "{}:{}:{}",
                        edge.from_session_id,
                        edge.to_session_id,
                        edge.edge_type.as_str()
                    )
                });
                let key = (
                    edge.source_id.clone(),
                    edge.from_session_id.clone(),
                    edge.to_session_id.clone(),
                    edge_key,
                );
                if !edge_keys.insert(key) {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate edge record".to_owned(),
                    );
                }
                edges.push((line_number, edge));
            }
        }
    }

    let reference_index = CustomHistoryReferenceIndex {
        manifest_line,
        sources: &sources,
        sessions: &sessions,
        events: &events,
        event_keys: &event_keys,
        file_touches: &file_touches,
        edges: &edges,
    };
    validate_custom_history_references(&mut summary, reference_index);
    if summary.failed > 0 {
        return Ok(custom_history_failed_normalization(summary));
    }

    let mut result = ProviderNormalizationResult {
        summary,
        ..ProviderNormalizationResult::default()
    };
    let mut source_cursors = Vec::new();
    for (_, source) in sources.values() {
        let machine_id = source
            .machine_id
            .clone()
            .unwrap_or_else(|| context.machine_id.clone());
        if let Some(after) = source
            .cursor
            .as_ref()
            .and_then(|cursor| custom_history_normalized_cursor_range(source, cursor).after)
        {
            source_cursors.push(CustomHistoryJsonlV1SourceCursorImport {
                machine_id,
                checkpoint: after,
            });
        }
    }
    for (line_number, session) in sessions.values() {
        let source = &sources
            .get(&session.source_id)
            .expect("session source already validated")
            .1;
        result.captures.push((
            *line_number,
            custom_history_session_capture(source, session, None, context),
        ));
    }
    for (line_number, event) in events {
        let (_, session) = sessions
            .get(&(event.source_id.clone(), event.session_id.clone()))
            .expect("event session already validated");
        let source = &sources
            .get(&event.source_id)
            .expect("event source already validated")
            .1;
        let envelope = custom_history_event_envelope(source, &event);
        result.captures.push((
            line_number,
            custom_history_session_capture(source, session, Some(envelope), context),
        ));
    }
    for (line_number, file_touch) in file_touches {
        let source = &sources
            .get(&file_touch.source_id)
            .expect("file_touch source already validated")
            .1;
        result.files_touched.push((
            line_number,
            custom_history_file_touch_envelope(source, &file_touch, context),
        ));
    }

    let mut custom_edges = Vec::new();
    for (line_number, edge) in edges {
        let source = &sources
            .get(&edge.source_id)
            .expect("edge source already validated")
            .1;
        custom_edges.push((
            line_number,
            custom_history_edge_import(source, &edge, context),
        ));
    }

    Ok(CustomHistoryJsonlV1NormalizationResult {
        provider: result,
        edges: custom_edges,
        source_cursors,
    })
}

fn custom_history_failed_normalization(
    summary: ProviderImportSummary,
) -> CustomHistoryJsonlV1NormalizationResult {
    CustomHistoryJsonlV1NormalizationResult {
        provider: ProviderNormalizationResult {
            summary,
            ..ProviderNormalizationResult::default()
        },
        edges: Vec::new(),
        source_cursors: Vec::new(),
    }
}

fn push_provider_import_failure(summary: &mut ProviderImportSummary, line: usize, error: String) {
    summary.failed += 1;
    summary.failures.push(ProviderImportFailure { line, error });
}

fn validate_custom_source_record(
    summary: &mut ProviderImportSummary,
    line_number: usize,
    source: &CtxHistoryJsonlSourceRecord,
) {
    validate_custom_history_identifier(summary, line_number, "source_id", &source.source_id);
    validate_custom_history_identifier(
        summary,
        line_number,
        "source_format",
        &source.source_format,
    );
    let valid = !source.provider_key.is_empty()
        && source.provider_key.len() <= 128
        && source.provider_key.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
        && source
            .provider_key
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    if !valid {
        push_provider_import_failure(
            summary,
            line_number,
            "provider_key must be 1 to 128 bytes, start with a lowercase ASCII letter or digit, and use only lowercase ASCII letters, digits, '.', '_', or '-'".to_owned(),
        );
    }
}

fn validate_custom_history_identifier(
    summary: &mut ProviderImportSummary,
    line_number: usize,
    field: &str,
    value: &str,
) {
    let error = if value.trim().is_empty() {
        Some(format!("{field} must not be empty"))
    } else if value.len() > 512 {
        Some(format!("{field} must be at most 512 bytes"))
    } else if value.chars().any(char::is_control) {
        Some(format!("{field} must not contain control characters"))
    } else {
        None
    };
    if let Some(error) = error {
        push_provider_import_failure(summary, line_number, error);
    }
}

struct CustomHistoryReferenceIndex<'a> {
    manifest_line: Option<usize>,
    sources: &'a BTreeMap<String, (usize, CtxHistoryJsonlSourceRecord)>,
    sessions: &'a BTreeMap<(String, String), (usize, CtxHistoryJsonlSessionRecord)>,
    events: &'a [(usize, CtxHistoryJsonlEventRecord)],
    event_keys: &'a BTreeSet<(String, String, u64)>,
    file_touches: &'a [(usize, CtxHistoryJsonlFileTouchRecord)],
    edges: &'a [(usize, CtxHistoryJsonlEdgeRecord)],
}

fn validate_custom_history_references(
    summary: &mut ProviderImportSummary,
    references: CustomHistoryReferenceIndex<'_>,
) {
    if references.manifest_line.is_none() {
        push_provider_import_failure(
            summary,
            0,
            "missing manifest record for ctx-history-jsonl-v1".to_owned(),
        );
    }

    for (line_number, session) in references.sessions.values() {
        if !references.sources.contains_key(&session.source_id) {
            push_provider_import_failure(
                summary,
                *line_number,
                format!(
                    "session references unknown source_id `{}`",
                    session.source_id
                ),
            );
        }
        if let Some(parent) = &session.parent_session_id {
            let key = (session.source_id.clone(), parent.clone());
            if !references.sessions.contains_key(&key) {
                push_provider_import_failure(
                    summary,
                    *line_number,
                    format!("session references unknown parent_session_id `{parent}`"),
                );
            }
        }
        if let Some(root) = &session.root_session_id {
            let key = (session.source_id.clone(), root.clone());
            if root != &session.session_id && !references.sessions.contains_key(&key) {
                push_provider_import_failure(
                    summary,
                    *line_number,
                    format!("session references unknown root_session_id `{root}`"),
                );
            }
        }
    }

    for (line_number, event) in references.events {
        if !references
            .sessions
            .contains_key(&(event.source_id.clone(), event.session_id.clone()))
        {
            push_provider_import_failure(
                summary,
                *line_number,
                format!(
                    "event references unknown session `{}` in source `{}`",
                    event.session_id, event.source_id
                ),
            );
        }
    }

    for (line_number, file_touch) in references.file_touches {
        if !references
            .sessions
            .contains_key(&(file_touch.source_id.clone(), file_touch.session_id.clone()))
        {
            push_provider_import_failure(
                summary,
                *line_number,
                format!(
                    "file_touch references unknown session `{}` in source `{}`",
                    file_touch.session_id, file_touch.source_id
                ),
            );
        }
        if let Some(event_index) = file_touch.event_index {
            let key = (
                file_touch.source_id.clone(),
                file_touch.session_id.clone(),
                event_index,
            );
            if !references.event_keys.contains(&key) {
                push_provider_import_failure(
                    summary,
                    *line_number,
                    format!("file_touch references unknown event_index `{event_index}`"),
                );
            }
        }
    }

    for (line_number, edge) in references.edges {
        let from_key = (edge.source_id.clone(), edge.from_session_id.clone());
        let to_key = (edge.source_id.clone(), edge.to_session_id.clone());
        if !references.sessions.contains_key(&from_key) {
            push_provider_import_failure(
                summary,
                *line_number,
                format!(
                    "edge references unknown from_session_id `{}`",
                    edge.from_session_id
                ),
            );
        }
        if !references.sessions.contains_key(&to_key) {
            push_provider_import_failure(
                summary,
                *line_number,
                format!(
                    "edge references unknown to_session_id `{}`",
                    edge.to_session_id
                ),
            );
        }
        if edge.edge_type == SessionEdgeType::ParentChild {
            let Some((_, child)) = references.sessions.get(&to_key) else {
                continue;
            };
            if let Some(parent) = &child.parent_session_id {
                if parent != &edge.from_session_id {
                    push_provider_import_failure(
                        summary,
                        *line_number,
                        format!(
                            "parent_child edge from_session_id `{}` conflicts with session parent_session_id `{parent}`",
                            edge.from_session_id
                        ),
                    );
                }
            }
        }
    }
}

fn custom_history_session_capture(
    source: &CtxHistoryJsonlSourceRecord,
    session: &CtxHistoryJsonlSessionRecord,
    event: Option<ProviderEventEnvelope>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let provider_session_id = custom_history_internal_session_id(
        &source.provider_key,
        &source.source_id,
        &session.session_id,
    );
    let event_cursor = event.as_ref().and_then(|event| {
        event.cursor.as_ref().map(|cursor| ProviderCursorRange {
            before: None,
            after: Some(ProviderCursorCheckpoint {
                stream: custom_history_cursor_stream(source),
                cursor: cursor.clone(),
                observed_at: event.occurred_at,
            }),
        })
    });
    let source_cursor = source
        .cursor
        .as_ref()
        .map(|cursor| custom_history_normalized_cursor_range(source, cursor))
        .or(event_cursor);
    ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider: CaptureProvider::Custom,
        source: ProviderSourceEnvelope {
            source_format: source.source_format.clone(),
            machine_id: source
                .machine_id
                .clone()
                .unwrap_or_else(|| context.machine_id.clone()),
            observed_at: source.observed_at.unwrap_or(context.imported_at),
            raw_source_path: custom_history_effective_raw_source_path(source, context),
            raw_retention: source.raw_retention,
            redaction_boundary: source.redaction_boundary,
            trust: match source.trust {
                ProviderSourceTrust::Unknown => ProviderSourceTrust::ProviderExport,
                other => other,
            },
            fidelity: source.fidelity,
            cursor: source_cursor,
            idempotency_key: Some(format!(
                "ctx-history-jsonl-v1:{}:{}",
                source.provider_key, source.source_id
            )),
            metadata: custom_history_metadata(
                source.metadata.clone(),
                json!({
                    "provider_key": source.provider_key,
                    "source_id": source.source_id,
                    "source_format": source.source_format,
                    "raw_uri": source.raw_uri,
                    "raw_source_path": source.raw_source_path,
                    "fingerprint": source.fingerprint,
                    "importer_version": source.importer_version,
                    "cursor": source.cursor,
                }),
            ),
        },
        session: ProviderSessionEnvelope {
            provider_session_id,
            parent_provider_session_id: session.parent_session_id.as_ref().map(|parent| {
                custom_history_internal_session_id(&source.provider_key, &source.source_id, parent)
            }),
            root_provider_session_id: session.root_session_id.as_ref().map(|root| {
                custom_history_internal_session_id(&source.provider_key, &source.source_id, root)
            }),
            external_agent_id: session.external_agent_id.clone(),
            agent_type: session.agent_type,
            role_hint: session.role_hint.clone(),
            is_primary: session.is_primary,
            status: session.status,
            started_at: session.started_at,
            ended_at: session.ended_at,
            cwd: session.cwd.clone(),
            fidelity: session.fidelity,
            idempotency_key: session.idempotency_key.clone().or_else(|| {
                Some(format!(
                    "ctx-history-jsonl-v1:{}:{}:{}",
                    source.provider_key, source.source_id, session.session_id
                ))
            }),
            artifacts: session.artifacts.clone(),
            metadata: custom_history_metadata(
                session.metadata.clone(),
                json!({
                    "provider_key": source.provider_key,
                    "source_id": source.source_id,
                    "session_id": session.session_id,
                    "native_session_id": session.native_session_id,
                    "parent_session_id": session.parent_session_id,
                    "root_session_id": session.root_session_id,
                }),
            ),
        },
        event,
    }
}

fn custom_history_event_envelope(
    source: &CtxHistoryJsonlSourceRecord,
    event: &CtxHistoryJsonlEventRecord,
) -> ProviderEventEnvelope {
    let payload = if let Some(preview) = &event.preview {
        json!({ "text": preview })
    } else {
        event.payload.clone()
    };
    let raw_payload = event
        .preview
        .as_ref()
        .map(|_| event.payload.clone())
        .filter(|payload| payload != &json!({}));
    ProviderEventEnvelope {
        provider_event_index: event.event_index,
        provider_event_hash: event.event_hash.clone(),
        cursor: event.native_cursor.clone(),
        event_type: event.event_type,
        role: event.role,
        occurred_at: event.occurred_at,
        fidelity: event.fidelity,
        redaction_state: event.redaction_state,
        idempotency_key: event.idempotency_key.clone(),
        artifacts: event.artifacts.clone(),
        payload,
        metadata: custom_history_metadata(
            event.metadata.clone(),
            json!({
                "provider_key": source.provider_key,
                "source_id": event.source_id,
                "session_id": event.session_id,
                "event_id": event.event_id,
                "native_cursor": event.native_cursor,
                "preview": event.preview,
                "raw_payload": raw_payload,
            }),
        ),
    }
}

fn custom_history_file_touch_envelope(
    source: &CtxHistoryJsonlSourceRecord,
    file_touch: &CtxHistoryJsonlFileTouchRecord,
    context: &ProviderAdapterContext,
) -> ProviderFileTouchedEnvelope {
    ProviderFileTouchedEnvelope {
        provider: CaptureProvider::Custom,
        provider_session_id: custom_history_internal_session_id(
            &source.provider_key,
            &source.source_id,
            &file_touch.session_id,
        ),
        provider_touch_index: file_touch.touch_index,
        provider_event_index: file_touch.event_index,
        raw_source_path: custom_history_effective_raw_source_path(source, context),
        path: file_touch.path.clone(),
        change_kind: file_touch.change_kind,
        old_path: file_touch.old_path.clone(),
        line_count_delta: file_touch.line_count_delta,
        confidence: file_touch.confidence,
        occurred_at: file_touch.occurred_at,
        source_format: source.source_format.clone(),
        metadata: custom_history_metadata(
            file_touch.metadata.clone(),
            json!({
                "provider_key": source.provider_key,
                "source_id": file_touch.source_id,
                "session_id": file_touch.session_id,
            }),
        ),
    }
}

fn custom_history_edge_import(
    source: &CtxHistoryJsonlSourceRecord,
    edge: &CtxHistoryJsonlEdgeRecord,
    context: &ProviderAdapterContext,
) -> CustomHistoryJsonlV1EdgeImport {
    CustomHistoryJsonlV1EdgeImport {
        provider_key: source.provider_key.clone(),
        source_id: source.source_id.clone(),
        source_format: source.source_format.clone(),
        raw_source_path: custom_history_effective_raw_source_path(source, context),
        from_provider_session_id: custom_history_internal_session_id(
            &source.provider_key,
            &source.source_id,
            &edge.from_session_id,
        ),
        to_provider_session_id: custom_history_internal_session_id(
            &source.provider_key,
            &source.source_id,
            &edge.to_session_id,
        ),
        edge_id: edge.edge_id.clone(),
        edge_type: edge.edge_type,
        confidence: edge.confidence,
        occurred_at: edge.occurred_at.unwrap_or(context.imported_at),
        fidelity: edge.fidelity,
        metadata: custom_history_metadata(
            edge.metadata.clone(),
            json!({
                "provider_key": source.provider_key,
                "source_id": edge.source_id,
                "from_session_id": edge.from_session_id,
                "to_session_id": edge.to_session_id,
                "edge_id": edge.edge_id,
            }),
        ),
    }
}

fn custom_history_effective_raw_source_path(
    source: &CtxHistoryJsonlSourceRecord,
    context: &ProviderAdapterContext,
) -> Option<String> {
    source.raw_source_path.clone().or_else(|| {
        context
            .source_path
            .as_ref()
            .map(|path| path.display().to_string())
    })
}

fn custom_history_internal_session_id(
    provider_key: &str,
    source_id: &str,
    session_id: &str,
) -> String {
    let key = custom_history_key(json!({
        "schema": CTX_HISTORY_JSONL_V1_SCHEMA_VERSION,
        "kind": "session",
        "provider_key": provider_key,
        "source_id": source_id,
        "session_id": session_id,
    }));
    let id = stable_capture_uuid(&key, "custom-provider-session-id");
    format!("ctx-history-jsonl-v1-{id}")
}

fn custom_history_cursor_stream(source: &CtxHistoryJsonlSourceRecord) -> String {
    custom_history_jsonl_v1_cursor_stream(
        &source.provider_key,
        &source.source_id,
        &source.source_format,
    )
}

pub fn custom_history_jsonl_v1_cursor_stream(
    provider_key: &str,
    source_id: &str,
    source_format: &str,
) -> String {
    let key = custom_history_key(json!({
        "schema": CTX_HISTORY_JSONL_V1_SCHEMA_VERSION,
        "kind": "cursor_stream",
        "provider_key": provider_key,
        "source_id": source_id,
        "source_format": source_format,
    }));
    let stream_id = stable_capture_uuid(&key, "custom-cursor-stream");
    format!("provider:custom:{provider_key}:{stream_id}")
}

fn custom_history_normalized_cursor_range(
    source: &CtxHistoryJsonlSourceRecord,
    cursor: &ProviderCursorRange,
) -> ProviderCursorRange {
    ProviderCursorRange {
        before: cursor
            .before
            .as_ref()
            .map(|checkpoint| custom_history_normalized_cursor_checkpoint(source, checkpoint)),
        after: cursor
            .after
            .as_ref()
            .map(|checkpoint| custom_history_normalized_cursor_checkpoint(source, checkpoint)),
    }
}

fn custom_history_normalized_cursor_checkpoint(
    source: &CtxHistoryJsonlSourceRecord,
    checkpoint: &ProviderCursorCheckpoint,
) -> ProviderCursorCheckpoint {
    ProviderCursorCheckpoint {
        stream: custom_history_cursor_stream(source),
        cursor: checkpoint.cursor.clone(),
        observed_at: checkpoint.observed_at,
    }
}

fn custom_history_key(value: Value) -> String {
    serde_json::to_string(&value).expect("custom history identity key is serializable")
}

fn custom_history_metadata(base: Value, custom: Value) -> Value {
    let mut map = match base {
        Value::Object(map) => map,
        Value::Null => serde_json::Map::new(),
        other => {
            let mut map = serde_json::Map::new();
            map.insert("metadata".to_owned(), other);
            map
        }
    };
    map.insert("ctx_history_jsonl_v1".to_owned(), custom);
    Value::Object(map)
}

fn import_custom_history_edges(
    store: &mut Store,
    edges: &[(usize, CustomHistoryJsonlV1EdgeImport)],
    history_record_id: Option<Uuid>,
    allow_partial_failures: bool,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    if edges.is_empty() {
        return Ok(());
    }

    store.begin_immediate_batch()?;
    for (line_number, edge) in edges {
        let edge_id = if edge.edge_type == SessionEdgeType::ParentChild {
            provider_edge_uuid(
                CaptureProvider::Custom,
                &edge.to_provider_session_id,
                "parent_child",
            )
        } else {
            let key = custom_history_key(json!({
                "schema": CTX_HISTORY_JSONL_V1_SCHEMA_VERSION,
                "kind": "session_edge",
                "provider_key": edge.provider_key,
                "source_id": edge.source_id,
                "from_provider_session_id": edge.from_provider_session_id,
                "to_provider_session_id": edge.to_provider_session_id,
                "edge_type": edge.edge_type.as_str(),
                "edge_id": edge.edge_id,
            }));
            stable_capture_uuid(&key, "session-edge")
        };
        let from_session_id =
            provider_session_uuid(CaptureProvider::Custom, &edge.from_provider_session_id);
        let to_session_id =
            provider_session_uuid(CaptureProvider::Custom, &edge.to_provider_session_id);
        let source_id = provider_scoped_source_uuid(
            CaptureProvider::Custom,
            &edge.to_provider_session_id,
            &edge.source_format,
            edge.raw_source_path.as_deref(),
        );
        let mut exists_cache = BTreeMap::<Uuid, bool>::new();
        if !provider_session_exists_cached(store, from_session_id, &mut exists_cache)?
            || !provider_session_exists_cached(store, to_session_id, &mut exists_cache)?
        {
            push_provider_import_failure(
                summary,
                *line_number,
                "edge endpoint session was not imported".to_owned(),
            );
            if !allow_partial_failures {
                let _ = store.rollback_batch();
                return Ok(());
            }
            continue;
        }
        let was_present = store.session_edge_exists(edge_id)?;
        let session_edge = SessionEdge {
            id: edge_id,
            from_session_id,
            to_session_id,
            edge_type: edge.edge_type,
            confidence: edge.confidence,
            source_id: Some(source_id),
            timestamps: timestamps(edge.occurred_at),
            sync: provider_sync_metadata(
                edge.fidelity,
                json!({
                    "provider_key": edge.provider_key,
                    "source_id": edge.source_id,
                    "history_record_id": history_record_id,
                    "metadata": edge.metadata,
                }),
            ),
        };
        store.upsert_session_edge(&session_edge)?;
        if edge.edge_type == SessionEdgeType::ParentChild {
            let mut child = store.get_session(to_session_id)?;
            child.parent_session_id = Some(from_session_id);
            if child.root_session_id.is_none() {
                child.root_session_id = Some(from_session_id);
            }
            store.upsert_session(&child)?;
        }
        if was_present {
            summary.skipped_edges += 1;
            summary.skipped += 1;
        } else {
            summary.imported_edges += 1;
            summary.imported += 1;
        }
    }
    if let Err(err) = store.commit_batch() {
        let _ = store.rollback_batch();
        return Err(err.into());
    }
    Ok(())
}

fn import_custom_history_source_cursors(
    store: &mut Store,
    cursors: &[CustomHistoryJsonlV1SourceCursorImport],
) -> Result<()> {
    for cursor in cursors {
        store.upsert_sync_cursor(&SyncCursor {
            id: stable_capture_uuid(
                &format!(
                    "provider-cursor:{}:{}:{}",
                    CaptureProvider::Custom.as_str(),
                    cursor.machine_id,
                    cursor.checkpoint.stream
                ),
                "provider-sync-cursor",
            ),
            team_id: None,
            device_id: cursor.machine_id.clone(),
            stream: cursor.checkpoint.stream.clone(),
            cursor: cursor.checkpoint.cursor.clone(),
            last_synced_at: Some(cursor.checkpoint.observed_at),
            timestamps: timestamps(cursor.checkpoint.observed_at),
        })?;
    }
    Ok(())
}

fn collect_jsonl_paths(root: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
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
    ensure_provider_path_parents_are_not_symlinks(path)?;
    Ok(())
}

fn ensure_provider_path_parents_are_not_symlinks(path: &Path) -> Result<()> {
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

fn read_text_file_limited(path: &Path, max_bytes: usize, label: &str) -> Result<String> {
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

fn read_provider_jsonl_line(reader: &mut impl BufRead, buffer: &mut Vec<u8>) -> Result<bool> {
    buffer.clear();
    let mut total = 0usize;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(total > 0);
        }
        if let Some(newline_index) = available.iter().position(|byte| *byte == b'\n') {
            let bytes_to_consume = newline_index + 1;
            if total.saturating_add(bytes_to_consume) > MAX_PROVIDER_JSONL_LINE_BYTES {
                reader.consume(bytes_to_consume);
                return Err(provider_jsonl_line_too_large());
            }
            buffer.extend_from_slice(&available[..bytes_to_consume]);
            reader.consume(bytes_to_consume);
            return Ok(true);
        }

        let bytes_to_consume = available.len();
        if total.saturating_add(bytes_to_consume) > MAX_PROVIDER_JSONL_LINE_BYTES {
            reader.consume(bytes_to_consume);
            discard_provider_jsonl_line(reader)?;
            return Err(provider_jsonl_line_too_large());
        }
        buffer.extend_from_slice(available);
        reader.consume(bytes_to_consume);
        total = total.saturating_add(bytes_to_consume);
    }
}

fn discard_provider_jsonl_line(reader: &mut impl BufRead) -> Result<()> {
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(());
        }
        let bytes_to_consume = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(available.len());
        let found_newline = available
            .get(bytes_to_consume.saturating_sub(1))
            .is_some_and(|byte| *byte == b'\n');
        reader.consume(bytes_to_consume);
        if found_newline {
            return Ok(());
        }
    }
}

fn provider_jsonl_line_too_large() -> CaptureError {
    CaptureError::InvalidPayload(format!(
        "provider JSONL line exceeds max bytes ({MAX_PROVIDER_JSONL_LINE_BYTES})"
    ))
}

fn read_json_file_limited(path: &Path, max_bytes: usize, label: &str) -> Result<Value> {
    let text = read_text_file_limited(path, max_bytes, label)?;
    serde_json::from_str(&text).map_err(CaptureError::from)
}

fn parse_rfc3339_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

fn parse_optional_rfc3339_field(
    value: &Value,
    field: &'static str,
) -> Result<Option<DateTime<Utc>>> {
    let Some(raw_value) = value.get(field) else {
        return Ok(None);
    };
    let raw = raw_value.as_str().ok_or_else(|| {
        CaptureError::InvalidPayload(format!("{field} must be an RFC3339 string"))
    })?;
    parse_rfc3339_utc(raw)
        .ok_or_else(|| {
            CaptureError::InvalidPayload(format!("{field} is not a valid RFC3339 timestamp"))
        })
        .map(Some)
}

fn codex_session_line_timestamp(value: &Value, fallback: DateTime<Utc>) -> Result<DateTime<Utc>> {
    Ok(parse_optional_rfc3339_field(value, "timestamp")?.unwrap_or(fallback))
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
                    "previews are capped before local indexing/export"
                ],
            }),
        },
        event,
    }
}

struct CodexSessionLineContext<'a> {
    line_number: usize,
    occurred_at: DateTime<Utc>,
    tool_output_mode: CodexToolOutputMode,
    event_mode: CodexEventImportMode,
    raw_source_path: Option<&'a str>,
}

fn codex_session_line_capture(
    header: &CodexSessionHeader,
    value: &Value,
    call_contexts: &mut BTreeMap<String, CodexToolCallContext>,
    context: CodexSessionLineContext<'_>,
) -> CodexSessionLineCapture {
    let CodexSessionLineContext {
        line_number,
        occurred_at,
        tool_output_mode,
        event_mode,
        raw_source_path,
    } = context;
    let event = codex_session_event(
        value,
        line_number,
        occurred_at,
        call_contexts,
        tool_output_mode,
        event_mode,
    );
    let mut drafts = Vec::new();
    collect_patch_file_touches(value, &mut drafts);
    if drafts.is_empty()
        && (event
            .as_ref()
            .is_some_and(|event| event_type_supports_structured_file_touches(event.event_type))
            || codex_value_is_tool_call(value))
    {
        collect_structured_file_touches(value, &mut drafts);
    }
    let files_touched = provider_file_touch_envelopes(
        ProviderFileTouchEnvelopeContext {
            provider: CaptureProvider::Codex,
            provider_session_id: &header.id,
            source_format: CODEX_SESSION_SOURCE_FORMAT,
            raw_source_path,
            occurred_at,
            provider_event_index: event.as_ref().map(|event| event.provider_event_index),
            provider_touch_base_index: (line_number as u64) << 16,
            line_number,
        },
        drafts,
    );
    CodexSessionLineCapture {
        event,
        files_touched,
    }
}

fn codex_value_is_tool_call(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("response_item")
        && matches!(
            value
                .get("payload")
                .and_then(|payload| payload.get("type"))
                .and_then(Value::as_str),
            Some("function_call" | "custom_tool_call")
        )
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
            let (text, truncated) = codex_local_preview(&text, CODEX_MAX_TEXT_CHARS);
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
    let (text, text_truncated) = codex_local_preview(&text, CODEX_MAX_METADATA_TEXT_CHARS);

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
            .map(|text| codex_local_preview(text, preview_limit))
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
    let (text, text_truncated) = codex_local_preview(&text, CODEX_MAX_OUTPUT_PREVIEW_CHARS);

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
    let (summary, truncated) = codex_local_preview(&summary, CODEX_MAX_TEXT_CHARS);
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
        redaction_state: RedactionState::LocalPreview,
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
    let (text, truncated) = codex_local_preview(&preview, CODEX_MAX_METADATA_TEXT_CHARS);
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
    Some(codex_local_preview(command, CODEX_MAX_METADATA_TEXT_CHARS).0)
}

fn codex_value_preview(value: &Value, max_chars: usize) -> (String, bool) {
    let rendered = match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
    };
    codex_local_preview(&rendered, max_chars)
}

fn codex_local_preview(value: &str, max_chars: usize) -> (String, bool) {
    capped_text(value, max_chars)
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

fn provider_local_preview(value: &str, max_chars: usize) -> (String, bool) {
    capped_text(value, max_chars)
}

#[derive(Debug, Clone, PartialEq)]
struct FileTouchDraft {
    path: String,
    old_path: Option<String>,
    change_kind: Option<FileChangeKind>,
    confidence: Confidence,
    metadata: Value,
}

fn provider_file_touches_from_event(
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

fn provider_file_touches_from_raw_value(
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
    if drafts.is_empty() && event_type_supports_structured_file_touches(event.event_type) {
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

fn event_type_supports_structured_file_touches(event_type: EventType) -> bool {
    matches!(event_type, EventType::ToolCall | EventType::FileTouched)
}

struct ProviderFileTouchEnvelopeContext<'a> {
    provider: CaptureProvider,
    provider_session_id: &'a str,
    source_format: &'a str,
    raw_source_path: Option<&'a str>,
    occurred_at: DateTime<Utc>,
    provider_event_index: Option<u64>,
    provider_touch_base_index: u64,
    line_number: usize,
}

fn provider_file_touch_envelopes(
    context: ProviderFileTouchEnvelopeContext<'_>,
    drafts: Vec<FileTouchDraft>,
) -> Vec<(usize, ProviderFileTouchedEnvelope)> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for draft in drafts {
        let key = (
            draft.path.clone(),
            draft.old_path.clone(),
            draft.change_kind.map(|kind| kind.as_str().to_owned()),
        );
        if !seen.insert(key) {
            continue;
        }
        let provider_touch_index = context.provider_touch_base_index | (out.len() as u64);
        out.push((
            context.line_number,
            ProviderFileTouchedEnvelope {
                provider: context.provider,
                provider_session_id: context.provider_session_id.to_owned(),
                provider_touch_index,
                provider_event_index: context.provider_event_index,
                raw_source_path: context.raw_source_path.map(str::to_owned),
                path: draft.path,
                change_kind: draft.change_kind,
                old_path: draft.old_path,
                line_count_delta: None,
                confidence: draft.confidence,
                occurred_at: context.occurred_at,
                source_format: context.source_format.to_owned(),
                metadata: draft.metadata,
            },
        ));
    }
    out
}

fn collect_patch_file_touches(value: &Value, out: &mut Vec<FileTouchDraft>) {
    match value {
        Value::String(text) => {
            if text.contains("*** Begin Patch") {
                out.extend(parse_apply_patch_file_touches(text));
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_patch_file_touches(item, out);
            }
        }
        Value::Object(object) => {
            for value in object.values() {
                collect_patch_file_touches(value, out);
            }
        }
        _ => {}
    }
}

fn parse_apply_patch_file_touches(patch: &str) -> Vec<FileTouchDraft> {
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

fn flush_pending_patch_update(out: &mut Vec<FileTouchDraft>, pending_update: &mut Option<String>) {
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

fn collect_structured_file_touches(value: &Value, out: &mut Vec<FileTouchDraft>) {
    collect_structured_file_touches_with_context(value, out, None);
}

fn collect_structured_file_touches_with_context(
    value: &Value,
    out: &mut Vec<FileTouchDraft>,
    inherited_kind: Option<FileChangeKind>,
) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_structured_file_touches_with_context(item, out, inherited_kind);
            }
        }
        Value::Object(object) => {
            let operation_kind = object_operation_hint_kind(object);
            let object_kind = operation_kind.or(inherited_kind);
            collect_structured_file_touch_object(object, out, object_kind);
            for value in object.values() {
                collect_structured_file_touches_with_context(value, out, object_kind);
            }
        }
        _ => {}
    }
}

fn collect_structured_file_touch_object(
    object: &serde_json::Map<String, Value>,
    out: &mut Vec<FileTouchDraft>,
    inherited_kind: Option<FileChangeKind>,
) {
    let inferred_kind = inferred_file_change_kind(object);
    let change_kind = inherited_kind.unwrap_or(inferred_kind);
    let old_path = object.iter().find_map(|(key, value)| {
        is_old_file_path_key(key)
            .then(|| value.as_str())
            .flatten()
            .and_then(normalize_file_path)
    });
    for (key, value) in object {
        if !is_file_path_key(key) {
            continue;
        }
        let Some(raw_path) = value.as_str() else {
            continue;
        };
        if normalized_key(key) == "uri" && !raw_path.trim().starts_with("file://") {
            continue;
        }
        let Some(path) = normalize_file_path(raw_path) else {
            continue;
        };
        out.push(FileTouchDraft {
            path,
            old_path: old_path.clone(),
            change_kind: Some(change_kind),
            confidence: Confidence::High,
            metadata: json!({
                "source": "structured_provider_payload",
                "path_key": key,
            }),
        });
    }
}

fn object_operation_hint_kind(object: &serde_json::Map<String, Value>) -> Option<FileChangeKind> {
    object
        .iter()
        .any(|(key, value)| {
            matches!(
                normalized_key(key).as_str(),
                "tool" | "name" | "action" | "command" | "operation" | "type"
            ) && value.as_str().is_some_and(|text| !text.trim().is_empty())
        })
        .then(|| inferred_file_change_kind(object))
        .filter(|kind| *kind != FileChangeKind::Unknown)
}

fn inferred_file_change_kind(object: &serde_json::Map<String, Value>) -> FileChangeKind {
    let mut haystack = String::new();
    for (key, value) in object {
        haystack.push_str(&key.to_ascii_lowercase());
        haystack.push(' ');
        if matches!(
            key.to_ascii_lowercase().as_str(),
            "tool" | "name" | "action" | "command" | "operation" | "type"
        ) {
            if let Some(text) = value.as_str() {
                haystack.push_str(&text.to_ascii_lowercase());
                haystack.push(' ');
            }
        }
    }
    if haystack.contains("rename") || haystack.contains("move") {
        FileChangeKind::Renamed
    } else if haystack.contains("delete") || haystack.contains("remove") {
        FileChangeKind::Deleted
    } else if haystack.contains("create") || haystack.contains("write") || haystack.contains("add")
    {
        FileChangeKind::Created
    } else if haystack.contains("read") || haystack.contains("view") || haystack.contains("open") {
        FileChangeKind::Read
    } else if object.values().any(value_looks_like_file_content)
        || haystack.contains("edit")
        || haystack.contains("patch")
        || haystack.contains("replace")
        || haystack.contains("update")
    {
        FileChangeKind::Modified
    } else {
        FileChangeKind::Unknown
    }
}

fn value_looks_like_file_content(value: &Value) -> bool {
    value.as_str().is_some_and(|text| {
        text.contains('\n')
            || text.len() > 120
            || text.contains("*** Begin Patch")
            || text.contains("@@")
    })
}

fn is_file_path_key(key: &str) -> bool {
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

fn is_old_file_path_key(key: &str) -> bool {
    matches!(
        normalized_key(key).as_str(),
        "oldpath" | "frompath" | "sourcepath" | "originalpath" | "previouspath"
    )
}

fn normalized_key(key: &str) -> String {
    key.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn normalize_file_path(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_matches('"').trim_matches('\'');
    let trimmed = trimmed.strip_prefix("file://").unwrap_or(trimmed);
    if !looks_like_file_path(trimmed) {
        return None;
    }
    Some(trimmed.to_owned())
}

fn looks_like_file_path(value: &str) -> bool {
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

fn file_touch_draft(
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
    let raw_source_path = path.display().to_string();

    for (line_number, value, occurred_at) in rows {
        let event = claude_event(&value, line_number, occurred_at);
        if let Some(event) = &event {
            result
                .files_touched
                .extend(provider_file_touches_from_raw_value(
                    CaptureProvider::Claude,
                    &provider_session_id,
                    CLAUDE_PROJECTS_SOURCE_FORMAT,
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
                provider: CaptureProvider::Claude,
                source: ProviderSourceEnvelope {
                    source_format: CLAUDE_PROJECTS_SOURCE_FORMAT.to_owned(),
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
                        "source_path": raw_source_path.clone(),
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
                            "previews are capped before local indexing/export"
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
    let (text, truncated) = provider_local_preview(&text, PROVIDER_MAX_TEXT_CHARS);

    Some(ProviderEventEnvelope {
        provider_event_index: (line_number - 1) as u64,
        provider_event_hash: value.get("uuid").and_then(Value::as_str).map(str::to_owned),
        cursor: value.get("uuid").and_then(Value::as_str).map(str::to_owned),
        event_type,
        role,
        occurred_at,
        fidelity: Fidelity::Imported,
        redaction_state: RedactionState::LocalPreview,
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
            let (text, truncated) = provider_local_preview(text, max_chars);
            json!({ "text": text, "truncated": truncated })
        }
        _ => {
            let rendered = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
            let (json_text, truncated) = provider_local_preview(&rendered, max_chars);
            json!({ "json": json_text, "truncated": truncated })
        }
    }
}

fn provider_capped_json_value(value: &Value, max_string_chars: usize) -> Value {
    match value {
        Value::String(text) => {
            let (text, truncated) = provider_local_preview(text, max_string_chars);
            if truncated {
                json!({ "text": text, "truncated": true })
            } else {
                Value::String(text)
            }
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| provider_capped_json_value(item, max_string_chars))
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        provider_capped_json_value(value, max_string_chars),
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

#[derive(Debug, Clone)]
struct ShelleyConversationRow {
    conversation_id: String,
    slug: Option<String>,
    user_initiated: bool,
    created_at: Option<String>,
    updated_at: Option<String>,
    cwd: Option<String>,
    archived: bool,
    parent_conversation_id: Option<String>,
    model: Option<String>,
    conversation_options: Option<String>,
    current_generation: Option<i64>,
    agent_working: bool,
    tags: Option<String>,
    is_draft: bool,
    draft: Option<String>,
    queued_messages: Option<String>,
}

#[derive(Debug, Clone)]
struct ShelleyMessageRow {
    rowid: i64,
    message_id: String,
    conversation_id: String,
    sequence_id: i64,
    entry_type: String,
    llm_data: Option<String>,
    user_data: Option<String>,
    usage_data: Option<String>,
    created_at: Option<String>,
    display_data: Option<String>,
    excluded_from_context: bool,
    generation: Option<i64>,
    llm_api_url: Option<String>,
    model_name: Option<String>,
    forked_from_message_id: Option<String>,
}

#[derive(Debug, Clone)]
struct OpenHandsEventFile {
    path: PathBuf,
    line_number: usize,
    session_id: String,
    user_id: Option<String>,
    event_id: String,
    timestamp: DateTime<Utc>,
    value: Value,
}

struct NativeSessionDraft {
    provider: CaptureProvider,
    source_format: &'static str,
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
    root_provider_session_id: Option<String>,
    external_agent_id: Option<String>,
    agent_type: AgentType,
    role_hint: Option<String>,
    is_primary: bool,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    cwd: Option<String>,
    fidelity: Fidelity,
    raw_source_path: String,
    trust: ProviderSourceTrust,
    source_metadata: Value,
    session_metadata: Value,
}

fn native_provider_capture(
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

fn open_provider_sqlite_readonly(path: &Path) -> Result<Connection> {
    ensure_regular_provider_transcript_file(path)?;
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).map_err(|_| {
        CaptureError::InvalidPayload(format!(
            "provider SQLite value byte limit is unrepresentable: {MAX_PROVIDER_SQLITE_VALUE_BYTES}"
        ))
    })?;
    conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit);
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "query_only", true)?;
    Ok(conn)
}

fn provider_nonnegative_i64_to_u64(value: i64, field: &'static str) -> Result<u64> {
    u64::try_from(value).map_err(|_| {
        CaptureError::InvalidPayload(format!("{field} must be nonnegative, got {value}"))
    })
}

fn provider_line_from_index(index: u64) -> usize {
    index.min(usize::MAX as u64) as usize
}

fn provider_timestamp_seconds_to_datetime(value: f64) -> Option<DateTime<Utc>> {
    if !value.is_finite() {
        return None;
    }
    let millis = if value.abs() > 1_000_000_000_000.0 {
        value.round()
    } else {
        (value * 1000.0).round()
    };
    if millis < i64::MIN as f64 || millis > i64::MAX as f64 {
        return None;
    }
    DateTime::<Utc>::from_timestamp_millis(millis as i64)
}

fn provider_timestamp_seconds(value: Option<f64>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    value
        .and_then(provider_timestamp_seconds_to_datetime)
        .unwrap_or(fallback)
}

fn provider_required_timestamp_seconds(value: f64, field: &'static str) -> Result<DateTime<Utc>> {
    provider_timestamp_seconds_to_datetime(value).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "{field} is outside representable timestamp range: {value}"
        ))
    })
}

fn provider_timestamp_millis(value: Option<i64>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    value
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .unwrap_or(fallback)
}

fn provider_required_timestamp_millis(value: i64, field: &'static str) -> Result<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp_millis(value).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "{field} is outside representable timestamp range: {value}"
        ))
    })
}

fn provider_timestamp_value(value: Option<&Value>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    match value {
        Some(Value::String(raw)) => parse_rfc3339_utc(raw)
            .or_else(|| {
                raw.parse::<f64>()
                    .ok()
                    .map(|ts| provider_timestamp_seconds(Some(ts), fallback))
            })
            .unwrap_or(fallback),
        Some(Value::Number(number)) => number
            .as_f64()
            .map(|ts| provider_timestamp_seconds(Some(ts), fallback))
            .unwrap_or(fallback),
        _ => fallback,
    }
}

fn text_id_index(seed: &str, offset: u64) -> u64 {
    offset.saturating_add(fnv1a64(seed.as_bytes()) & 0x0fff_ffff)
}

fn provider_json_text(raw: &str) -> Value {
    serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}

fn hermes_decode_content(raw: Option<&str>) -> Value {
    let Some(raw) = raw else {
        return Value::Null;
    };
    if let Some(json) = raw.strip_prefix("\0json:") {
        return provider_json_text(json);
    }
    Value::String(raw.to_owned())
}

struct NativeEventDraft {
    provider: CaptureProvider,
    source_format: &'static str,
    provider_session_id: String,
    provider_event_index: u64,
    provider_event_hash: Option<String>,
    cursor: String,
    event_type: EventType,
    role: Option<EventRole>,
    occurred_at: DateTime<Utc>,
    text: String,
    body: Value,
    metadata: Value,
}

fn native_event(draft: NativeEventDraft) -> ProviderEventEnvelope {
    let (text, truncated) = provider_local_preview(&draft.text, PROVIDER_MAX_TEXT_CHARS);
    ProviderEventEnvelope {
        provider_event_index: draft.provider_event_index,
        provider_event_hash: draft.provider_event_hash,
        cursor: Some(draft.cursor),
        event_type: draft.event_type,
        role: draft.role,
        occurred_at: draft.occurred_at,
        fidelity: Fidelity::Imported,
        redaction_state: RedactionState::LocalPreview,
        idempotency_key: Some(format!(
            "provider-event:{}:{}:{}",
            draft.provider.as_str(),
            draft.provider_session_id,
            draft.provider_event_index
        )),
        artifacts: Vec::new(),
        payload: json!({
            "text": text,
            "truncated": truncated,
            "source_format": draft.source_format,
            "body": provider_capped_json(&draft.body, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: draft.metadata,
    }
}

fn openclaw_agent_id(path: &Path) -> Option<String> {
    let components = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    components.windows(2).find_map(|window| {
        (window[0] == "agents" && !window[1].is_empty()).then(|| window[1].clone())
    })
}

fn provider_path_has_component(path: &Path, expected: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str() == expected)
}

fn openclaw_session_indexes(root: &Path) -> BTreeMap<String, Value> {
    let mut indexes = BTreeMap::new();
    let mut paths = Vec::new();
    let mut visited = 0usize;
    collect_named_paths(
        root,
        "sessions.json",
        &mut paths,
        &mut visited,
        MAX_OPENCLAW_SESSION_INDEX_PATHS,
        MAX_OPENCLAW_SESSION_INDEX_VISITED_PATHS,
    );
    for path in paths {
        let Ok(text) = read_text_file_limited(
            &path,
            MAX_OPENCLAW_SESSION_INDEX_BYTES,
            "OpenClaw sessions.json",
        ) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let agent_id = openclaw_agent_id(&path);
        for (key, value) in openclaw_session_index_entries(value) {
            if let Some(session_id) = value
                .get("sessionId")
                .or_else(|| value.get("id"))
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
            {
                if let Some(agent_id) = &agent_id {
                    indexes
                        .entry(format!("{agent_id}/{session_id}"))
                        .or_insert(value.clone());
                }
                indexes
                    .entry(session_id.to_owned())
                    .or_insert(value.clone());
            }
            if let Some(agent_id) = &agent_id {
                indexes
                    .entry(format!("{agent_id}/{key}"))
                    .or_insert(value.clone());
            }
            indexes.entry(key).or_insert(value);
        }
    }
    indexes
}

fn openclaw_session_index_entries(value: Value) -> Vec<(String, Value)> {
    match value {
        Value::Array(items) => items
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                let key = value
                    .get("sessionId")
                    .or_else(|| value.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| index.to_string());
                (key, value)
            })
            .collect(),
        Value::Object(mut map) => {
            if let Some(Value::Array(items)) = map.remove("sessions") {
                return openclaw_session_index_entries(Value::Array(items));
            }
            map.into_iter().collect()
        }
        _ => Vec::new(),
    }
}

fn collect_named_paths(
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

fn normalize_openclaw_history(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let mut paths = Vec::new();
    collect_jsonl_paths(path, &mut paths)?;
    if !path.is_file() {
        paths.retain(|candidate| provider_path_has_component(candidate, "sessions"));
    }
    paths.sort();
    if paths.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no OpenClaw session JSONL transcripts found",
        });
    }
    let indexes = openclaw_session_indexes(path);
    let mut merged = ProviderNormalizationResult::default();
    for transcript_path in paths {
        let mut result = normalize_openclaw_jsonl_file(&transcript_path, context, &indexes)?;
        merged.summary.merge(result.summary);
        merged.captures.append(&mut result.captures);
        merged.files_touched.append(&mut result.files_touched);
    }
    Ok(merged)
}

fn normalize_openclaw_jsonl_file(
    path: &Path,
    context: &ProviderAdapterContext,
    indexes: &BTreeMap<String, Value>,
) -> Result<ProviderNormalizationResult> {
    ensure_regular_provider_transcript_file(path)?;
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut result = ProviderNormalizationResult::default();
    let fallback_id = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("openclaw-session")
        .to_owned();
    let agent_id = openclaw_agent_id(path);
    let mut provider_session_id = agent_id
        .as_ref()
        .map(|agent| format!("{agent}/{fallback_id}"))
        .unwrap_or_else(|| fallback_id.clone());
    let mut started_at = context.imported_at;
    let mut cwd = None;
    let mut header_raw = Value::Null;
    let mut header_seen = false;
    let mut line_number = 0usize;
    let mut line = Vec::new();
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
                    error: err.to_string(),
                });
                continue;
            }
        };
        let row_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("message");
        if row_type == "session" {
            if let Some(id) = value.get("id").and_then(Value::as_str) {
                provider_session_id = agent_id
                    .as_ref()
                    .map(|agent| format!("{agent}/{id}"))
                    .unwrap_or_else(|| id.to_owned());
            }
            started_at = provider_timestamp_value(value.get("timestamp"), context.imported_at);
            cwd = value.get("cwd").and_then(Value::as_str).map(str::to_owned);
            header_raw = value.clone();
            header_seen = true;
            result.captures.push((
                line_number,
                openclaw_capture(
                    OpenClawCaptureDraft {
                        provider_session_id: &provider_session_id,
                        agent_id: agent_id.as_deref(),
                        started_at,
                        ended_at: None,
                        cwd: cwd.clone(),
                        path,
                        indexes,
                        header_raw: header_raw.clone(),
                        event: None,
                    },
                    context,
                ),
            ));
            continue;
        }

        let occurred_at = provider_timestamp_value(value.get("timestamp"), started_at);
        let event_index = (line_number - 1) as u64;
        let event = openclaw_event(
            &provider_session_id,
            event_index,
            line_number,
            &value,
            occurred_at,
        );
        if !header_seen {
            header_seen = true;
            result.captures.push((
                line_number,
                openclaw_capture(
                    OpenClawCaptureDraft {
                        provider_session_id: &provider_session_id,
                        agent_id: agent_id.as_deref(),
                        started_at,
                        ended_at: None,
                        cwd: cwd.clone(),
                        path,
                        indexes,
                        header_raw: header_raw.clone(),
                        event: None,
                    },
                    context,
                ),
            ));
        }
        result.captures.push((
            line_number,
            openclaw_capture(
                OpenClawCaptureDraft {
                    provider_session_id: &provider_session_id,
                    agent_id: agent_id.as_deref(),
                    started_at,
                    ended_at: None,
                    cwd: cwd.clone(),
                    path,
                    indexes,
                    header_raw: header_raw.clone(),
                    event: Some(event),
                },
                context,
            ),
        ));
    }
    Ok(result)
}

struct OpenClawCaptureDraft<'a> {
    provider_session_id: &'a str,
    agent_id: Option<&'a str>,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    cwd: Option<String>,
    path: &'a Path,
    indexes: &'a BTreeMap<String, Value>,
    header_raw: Value,
    event: Option<ProviderEventEnvelope>,
}

fn openclaw_capture(
    draft: OpenClawCaptureDraft<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let OpenClawCaptureDraft {
        provider_session_id,
        agent_id,
        started_at,
        ended_at,
        cwd,
        path,
        indexes,
        header_raw,
        event,
    } = draft;
    let local_id = provider_session_id
        .rsplit_once('/')
        .map(|(_, id)| id)
        .unwrap_or(provider_session_id);
    let index = indexes
        .get(provider_session_id)
        .or_else(|| indexes.get(local_id))
        .cloned()
        .unwrap_or(Value::Null);
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::OpenClaw,
            source_format: OPENCLAW_SOURCE_FORMAT,
            provider_session_id: provider_session_id.to_owned(),
            parent_provider_session_id: index
                .get("parentSessionId")
                .or_else(|| index.get("parent_session_id"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            root_provider_session_id: None,
            external_agent_id: agent_id.map(str::to_owned),
            agent_type: AgentType::Primary,
            role_hint: Some("personal-agent".to_owned()),
            is_primary: true,
            started_at,
            ended_at,
            cwd,
            fidelity: Fidelity::Partial,
            raw_source_path: path.display().to_string(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": OPENCLAW_SOURCE_FORMAT,
                "index": provider_capped_json(&index, PROVIDER_MAX_PREVIEW_CHARS),
                "header": provider_capped_json(&header_raw, PROVIDER_MAX_PREVIEW_CHARS),
                "support_level": "beta",
            }),
            session_metadata: json!({
                "source_format": OPENCLAW_SOURCE_FORMAT,
                "agent_id": agent_id,
                "session_index": provider_capped_json(&index, PROVIDER_MAX_PREVIEW_CHARS),
                "fidelity_gap": "OpenClaw session JSONL is current native storage, but upstream keeps a storage-neutral accessor for future schema changes",
            }),
        },
        context,
        event,
    )
}

fn openclaw_event(
    provider_session_id: &str,
    event_index: u64,
    line_number: usize,
    row: &Value,
    occurred_at: DateTime<Utc>,
) -> ProviderEventEnvelope {
    let row_type = row.get("type").and_then(Value::as_str).unwrap_or("message");
    let message = row.get("message").unwrap_or(row);
    let role = message
        .get("role")
        .or_else(|| row.get("role"))
        .and_then(Value::as_str)
        .map(|role| provider_role(Some(role)));
    let event_type = match row_type {
        "message" => match role {
            Some(EventRole::Tool) => EventType::ToolOutput,
            _ => EventType::Message,
        },
        "leaf" | "compaction" | "custom" => EventType::Notice,
        _ => EventType::Notice,
    };
    let text = message
        .get("content")
        .or_else(|| message.get("text"))
        .or_else(|| message.get("output"))
        .and_then(provider_value_text)
        .unwrap_or_else(|| format!("OpenClaw {row_type}"));
    native_event(NativeEventDraft {
        provider: CaptureProvider::OpenClaw,
        source_format: OPENCLAW_SOURCE_FORMAT,
        provider_session_id: provider_session_id.to_owned(),
        provider_event_index: event_index,
        provider_event_hash: row.get("id").and_then(Value::as_str).map(str::to_owned),
        cursor: format!("line:{line_number}"),
        event_type,
        role,
        occurred_at,
        text,
        body: row.clone(),
        metadata: json!({
            "source": "openclaw_jsonl",
            "source_format": OPENCLAW_SOURCE_FORMAT,
            "row_type": row_type,
            "message_id": row.get("id").and_then(Value::as_str),
            "parent_id": row.get("parentId").or_else(|| row.get("parent_id")).cloned(),
        }),
    })
}

#[derive(Debug, Clone)]
struct HermesSessionRow {
    id: String,
    source: String,
    parent_session_id: Option<String>,
    model: Option<String>,
    model_config: Option<String>,
    started_at: f64,
    ended_at: Option<f64>,
    end_reason: Option<String>,
    message_count: i64,
    tool_call_count: i64,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
    reasoning_tokens: i64,
    cwd: Option<String>,
    git_branch: Option<String>,
    git_repo_root: Option<String>,
    billing_provider: Option<String>,
    billing_base_url: Option<String>,
    billing_mode: Option<String>,
    estimated_cost_usd: Option<f64>,
    actual_cost_usd: Option<f64>,
    title: Option<String>,
    archived: i64,
}

#[derive(Debug, Clone)]
struct HermesMessageRow {
    id: i64,
    session_id: String,
    role: String,
    content: Option<String>,
    tool_call_id: Option<String>,
    tool_calls: Option<String>,
    tool_name: Option<String>,
    timestamp: f64,
    token_count: Option<i64>,
    finish_reason: Option<String>,
    reasoning: Option<String>,
    reasoning_content: Option<String>,
    reasoning_details: Option<String>,
    codex_reasoning_items: Option<String>,
    codex_message_items: Option<String>,
    platform_message_id: Option<String>,
    observed: i64,
    active: i64,
    compacted: i64,
}

fn normalize_hermes_sqlite(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let conn = open_provider_sqlite_readonly(path)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = opencode_schema_fingerprint(&conn)?;
    let sessions = hermes_sessions(&conn)?;
    let messages = hermes_messages(&conn)?;
    let sessions_by_id = sessions
        .into_iter()
        .map(|session| (session.id.clone(), session))
        .collect::<BTreeMap<_, _>>();
    let mut result = ProviderNormalizationResult::default();

    for row in messages {
        let provider_event_index =
            match provider_nonnegative_i64_to_u64(row.id, "Hermes message id") {
                Ok(value) => value,
                Err(err) => {
                    push_provider_import_failure(&mut result.summary, 0, err.to_string());
                    continue;
                }
            };
        let line = provider_line_from_index(provider_event_index);
        let Some(session) = sessions_by_id.get(&row.session_id) else {
            push_provider_import_failure(
                &mut result.summary,
                line,
                format!(
                    "Hermes message {} references missing session {}",
                    row.id, row.session_id
                ),
            );
            continue;
        };
        let provider_session_id = session.id.clone();
        let occurred_at =
            match provider_required_timestamp_seconds(row.timestamp, "Hermes message timestamp") {
                Ok(timestamp) => timestamp,
                Err(err) => {
                    push_provider_import_failure(&mut result.summary, line, err.to_string());
                    continue;
                }
            };
        let started_at = match provider_required_timestamp_seconds(
            session.started_at,
            "Hermes session started_at",
        ) {
            Ok(timestamp) => timestamp,
            Err(err) => {
                push_provider_import_failure(&mut result.summary, line, err.to_string());
                continue;
            }
        };
        let ended_at = match session
            .ended_at
            .map(|timestamp| {
                provider_required_timestamp_seconds(timestamp, "Hermes session ended_at")
            })
            .transpose()
        {
            Ok(timestamp) => timestamp,
            Err(err) => {
                push_provider_import_failure(&mut result.summary, line, err.to_string());
                continue;
            }
        };
        let content = hermes_decode_content(row.content.as_deref());
        let text = provider_value_text(&content).unwrap_or_else(|| {
            row.tool_name
                .as_ref()
                .map(|name| format!("tool: {name}"))
                .unwrap_or_else(|| format!("Hermes {}", row.role))
        });
        let event_type = hermes_event_type(&row);
        let role = Some(provider_role(Some(&row.role)));
        let event = native_event(NativeEventDraft {
            provider: CaptureProvider::Hermes,
            source_format: HERMES_SQLITE_SOURCE_FORMAT,
            provider_session_id: provider_session_id.clone(),
            provider_event_index,
            provider_event_hash: Some(format!("message:{}", row.id)),
            cursor: format!("messages:id:{}", row.id),
            event_type,
            role,
            occurred_at,
            text,
            body: json!({
                "message_id": row.id,
                "role": row.role,
                "content": content,
                "tool_call_id": row.tool_call_id,
                "tool_calls": row.tool_calls.as_deref().map(provider_json_text),
                "tool_name": row.tool_name,
                "reasoning": row.reasoning,
                "reasoning_content": row.reasoning_content,
                "reasoning_details": row.reasoning_details.as_deref().map(provider_json_text),
                "codex_reasoning_items": row.codex_reasoning_items.as_deref().map(provider_json_text),
                "codex_message_items": row.codex_message_items.as_deref().map(provider_json_text),
            }),
            metadata: json!({
                "source": "hermes_state_db",
                "source_format": HERMES_SQLITE_SOURCE_FORMAT,
                "message_id": row.id,
                "platform_message_id": row.platform_message_id,
                "token_count": row.token_count,
                "finish_reason": row.finish_reason,
                "observed": row.observed != 0,
                "active": row.active != 0,
                "compacted": row.compacted != 0,
            }),
        });
        result.captures.push((
            line,
            native_provider_capture(
                NativeSessionDraft {
                    provider: CaptureProvider::Hermes,
                    source_format: HERMES_SQLITE_SOURCE_FORMAT,
                    provider_session_id: provider_session_id.clone(),
                    parent_provider_session_id: session.parent_session_id.clone(),
                    root_provider_session_id: None,
                    external_agent_id: Some(session.source.clone()),
                    agent_type: if session.parent_session_id.is_some() {
                        AgentType::Subagent
                    } else {
                        AgentType::Primary
                    },
                    role_hint: Some(session.source.clone()),
                    is_primary: session.parent_session_id.is_none(),
                    started_at,
                    ended_at,
                    cwd: session.cwd.clone(),
                    fidelity: Fidelity::Imported,
                    raw_source_path: path.display().to_string(),
                    trust: ProviderSourceTrust::ProviderNative,
                    source_metadata: json!({
                        "adapter": HERMES_SQLITE_SOURCE_FORMAT,
                        "sqlite_user_version": user_version,
                        "schema_fingerprint": schema_fingerprint,
                        "upstream_schema_version_at_research": 17,
                    }),
                    session_metadata: json!({
                        "source_format": HERMES_SQLITE_SOURCE_FORMAT,
                        "source": session.source,
                        "title": session.title,
                        "model": session.model,
                        "model_config": session.model_config.as_deref().map(provider_json_text),
                        "end_reason": session.end_reason,
                        "message_count": session.message_count,
                        "tool_call_count": session.tool_call_count,
                        "tokens": {
                            "input": session.input_tokens,
                            "output": session.output_tokens,
                            "cache_read": session.cache_read_tokens,
                            "cache_write": session.cache_write_tokens,
                            "reasoning": session.reasoning_tokens,
                        },
                        "git": {
                            "branch": session.git_branch,
                            "repo_root": session.git_repo_root,
                        },
                        "billing": {
                            "provider": session.billing_provider,
                            "base_url": session.billing_base_url,
                            "mode": session.billing_mode,
                            "estimated_cost_usd": session.estimated_cost_usd,
                            "actual_cost_usd": session.actual_cost_usd,
                        },
                        "archived": session.archived != 0,
                    }),
                },
                context,
                Some(event),
            ),
        ));
    }

    Ok(result)
}

fn hermes_event_type(row: &HermesMessageRow) -> EventType {
    if row.role == "tool" {
        EventType::ToolOutput
    } else if row
        .tool_calls
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || row
            .tool_name
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    {
        EventType::ToolCall
    } else {
        EventType::Message
    }
}

fn hermes_sessions(conn: &Connection) -> Result<Vec<HermesSessionRow>> {
    if !sqlite_table_exists(conn, "sessions")? {
        return Err(CaptureError::InvalidPayload(
            "Hermes state.db is missing required sessions table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "sessions")?;
    ensure_sqlite_table_columns(
        &columns,
        "Hermes sessions table",
        &["id", "source", "started_at"],
    )?;
    let parent_session_id = optional_column_expr(&columns, "parent_session_id", "NULL");
    let model = optional_column_expr(&columns, "model", "NULL");
    let model_config = optional_column_expr(&columns, "model_config", "NULL");
    let ended_at = optional_column_expr(&columns, "ended_at", "NULL");
    let end_reason = optional_column_expr(&columns, "end_reason", "NULL");
    let message_count = optional_column_expr(&columns, "message_count", "0");
    let tool_call_count = optional_column_expr(&columns, "tool_call_count", "0");
    let input_tokens = optional_column_expr(&columns, "input_tokens", "0");
    let output_tokens = optional_column_expr(&columns, "output_tokens", "0");
    let cache_read_tokens = optional_column_expr(&columns, "cache_read_tokens", "0");
    let cache_write_tokens = optional_column_expr(&columns, "cache_write_tokens", "0");
    let reasoning_tokens = optional_column_expr(&columns, "reasoning_tokens", "0");
    let cwd = optional_column_expr(&columns, "cwd", "NULL");
    let git_branch = optional_column_expr(&columns, "git_branch", "NULL");
    let git_repo_root = optional_column_expr(&columns, "git_repo_root", "NULL");
    let billing_provider = optional_column_expr(&columns, "billing_provider", "NULL");
    let billing_base_url = optional_column_expr(&columns, "billing_base_url", "NULL");
    let billing_mode = optional_column_expr(&columns, "billing_mode", "NULL");
    let estimated_cost_usd = optional_column_expr(&columns, "estimated_cost_usd", "NULL");
    let actual_cost_usd = optional_column_expr(&columns, "actual_cost_usd", "NULL");
    let title = optional_column_expr(&columns, "title", "NULL");
    let archived = optional_column_expr(&columns, "archived", "0");
    let sql = format!(
        "select id, source, {parent_session_id}, {model}, {model_config}, started_at, \
         {ended_at}, {end_reason}, {message_count}, {tool_call_count}, {input_tokens}, \
         {output_tokens}, {cache_read_tokens}, {cache_write_tokens}, {reasoning_tokens}, \
         {cwd}, {git_branch}, {git_repo_root}, {billing_provider}, {billing_base_url}, \
         {billing_mode}, {estimated_cost_usd}, {actual_cost_usd}, {title}, {archived} \
         from sessions order by started_at, id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(HermesSessionRow {
            id: row.get(0)?,
            source: row.get(1)?,
            parent_session_id: row.get(2)?,
            model: row.get(3)?,
            model_config: row.get(4)?,
            started_at: row.get(5)?,
            ended_at: row.get(6)?,
            end_reason: row.get(7)?,
            message_count: row.get(8)?,
            tool_call_count: row.get(9)?,
            input_tokens: row.get(10)?,
            output_tokens: row.get(11)?,
            cache_read_tokens: row.get(12)?,
            cache_write_tokens: row.get(13)?,
            reasoning_tokens: row.get(14)?,
            cwd: row.get(15)?,
            git_branch: row.get(16)?,
            git_repo_root: row.get(17)?,
            billing_provider: row.get(18)?,
            billing_base_url: row.get(19)?,
            billing_mode: row.get(20)?,
            estimated_cost_usd: row.get(21)?,
            actual_cost_usd: row.get(22)?,
            title: row.get(23)?,
            archived: row.get(24)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

fn hermes_messages(conn: &Connection) -> Result<Vec<HermesMessageRow>> {
    if !sqlite_table_exists(conn, "messages")? {
        return Err(CaptureError::InvalidPayload(
            "Hermes state.db is missing required messages table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "messages")?;
    ensure_sqlite_table_columns(
        &columns,
        "Hermes messages table",
        &["id", "session_id", "role", "timestamp"],
    )?;
    let content = optional_column_expr(&columns, "content", "NULL");
    let tool_call_id = optional_column_expr(&columns, "tool_call_id", "NULL");
    let tool_calls = optional_column_expr(&columns, "tool_calls", "NULL");
    let tool_name = optional_column_expr(&columns, "tool_name", "NULL");
    let token_count = optional_column_expr(&columns, "token_count", "NULL");
    let finish_reason = optional_column_expr(&columns, "finish_reason", "NULL");
    let reasoning = optional_column_expr(&columns, "reasoning", "NULL");
    let reasoning_content = optional_column_expr(&columns, "reasoning_content", "NULL");
    let reasoning_details = optional_column_expr(&columns, "reasoning_details", "NULL");
    let codex_reasoning_items = optional_column_expr(&columns, "codex_reasoning_items", "NULL");
    let codex_message_items = optional_column_expr(&columns, "codex_message_items", "NULL");
    let platform_message_id = optional_column_expr(&columns, "platform_message_id", "NULL");
    let observed = optional_column_expr(&columns, "observed", "0");
    let active = optional_column_expr(&columns, "active", "1");
    let compacted = optional_column_expr(&columns, "compacted", "0");
    let visibility = if columns.contains("active") || columns.contains("compacted") {
        format!("where ({active} = 1 or {compacted} = 1)")
    } else {
        String::new()
    };
    let sql = format!(
        "select id, session_id, role, {content}, {tool_call_id}, {tool_calls}, {tool_name}, \
         timestamp, {token_count}, {finish_reason}, {reasoning}, {reasoning_content}, \
         {reasoning_details}, {codex_reasoning_items}, {codex_message_items}, \
         {platform_message_id}, {observed}, {active}, {compacted} \
         from messages {visibility} order by session_id, id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(HermesMessageRow {
            id: row.get(0)?,
            session_id: row.get(1)?,
            role: row.get(2)?,
            content: row.get(3)?,
            tool_call_id: row.get(4)?,
            tool_calls: row.get(5)?,
            tool_name: row.get(6)?,
            timestamp: row.get(7)?,
            token_count: row.get(8)?,
            finish_reason: row.get(9)?,
            reasoning: row.get(10)?,
            reasoning_content: row.get(11)?,
            reasoning_details: row.get(12)?,
            codex_reasoning_items: row.get(13)?,
            codex_message_items: row.get(14)?,
            platform_message_id: row.get(15)?,
            observed: row.get(16)?,
            active: row.get(17)?,
            compacted: row.get(18)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

#[derive(Debug, Clone)]
struct NanoClawSessionRow {
    id: String,
    agent_group_id: String,
    messaging_group_id: Option<String>,
    thread_id: Option<String>,
    agent_provider: Option<String>,
    status: Option<String>,
    container_status: Option<String>,
    last_active: Option<i64>,
    created_at: Option<i64>,
    agent_group_name: Option<String>,
    agent_group_folder: Option<String>,
    messaging_channel_type: Option<String>,
    messaging_platform_id: Option<String>,
    messaging_instance: Option<String>,
    messaging_name: Option<String>,
}

#[derive(Debug, Clone)]
struct NanoClawMessageRow {
    source: &'static str,
    id: String,
    seq: Option<i64>,
    kind: Option<String>,
    timestamp: Option<i64>,
    status: Option<String>,
    in_reply_to: Option<String>,
    platform_id: Option<String>,
    channel_type: Option<String>,
    thread_id: Option<String>,
    content: Option<String>,
    trigger: Option<String>,
    source_session_id: Option<String>,
    on_wake: Option<i64>,
}

fn normalize_nanoclaw_project(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let project_root = nanoclaw_project_root(path)?;
    let central_path = project_root.join("data").join("v2.db");
    let conn = open_provider_sqlite_readonly(&central_path)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = opencode_schema_fingerprint(&conn)?;
    let sessions = nanoclaw_sessions(&conn)?;
    let mut result = ProviderNormalizationResult::default();
    for session in sessions {
        let session_dir = project_root
            .join("data")
            .join("v2-sessions")
            .join(&session.agent_group_id)
            .join(&session.id);
        let mut messages = Vec::new();
        let inbound_path = session_dir.join("inbound.db");
        if inbound_path.is_file() {
            messages.extend(nanoclaw_inbound_messages(&inbound_path)?);
        }
        let outbound_path = session_dir.join("outbound.db");
        if outbound_path.is_file() {
            messages.extend(nanoclaw_outbound_messages(&outbound_path)?);
        }
        messages.sort_by_key(|message| {
            (
                message.timestamp.unwrap_or_default(),
                message.seq.unwrap_or_default(),
                message.source,
                message.id.clone(),
            )
        });
        for message in messages {
            let seq = match message
                .seq
                .map(|seq| provider_nonnegative_i64_to_u64(seq, "NanoClaw message seq"))
                .transpose()
            {
                Ok(seq) => seq,
                Err(err) => {
                    push_provider_import_failure(&mut result.summary, 0, err.to_string());
                    continue;
                }
            };
            let provider_session_id = format!("{}/{}", session.agent_group_id, session.id);
            let occurred_at = provider_timestamp_millis(message.timestamp, context.imported_at);
            let started_at = provider_timestamp_millis(session.created_at, occurred_at);
            let content = message
                .content
                .as_deref()
                .map(provider_json_text)
                .unwrap_or(Value::Null);
            let text = provider_value_text(&content).unwrap_or_else(|| {
                format!(
                    "NanoClaw {}",
                    message.kind.as_deref().unwrap_or(message.source)
                )
            });
            let event_index = nanoclaw_event_index(&message, seq);
            let role = if message.source == "inbound" {
                Some(EventRole::User)
            } else {
                Some(EventRole::Assistant)
            };
            let event = native_event(NativeEventDraft {
                provider: CaptureProvider::NanoClaw,
                source_format: NANOCLAW_SOURCE_FORMAT,
                provider_session_id: provider_session_id.clone(),
                provider_event_index: event_index,
                provider_event_hash: Some(format!("{}:{}", message.source, message.id)),
                cursor: format!(
                    "{}:{}:{}",
                    message.source,
                    session.id,
                    message.seq.unwrap_or_default()
                ),
                event_type: EventType::Message,
                role,
                occurred_at,
                text,
                body: json!({
                    "message_id": message.id,
                    "seq": message.seq,
                    "kind": message.kind,
                    "content": content,
                    "status": message.status,
                    "in_reply_to": message.in_reply_to,
                    "platform_id": message.platform_id,
                    "channel_type": message.channel_type,
                    "thread_id": message.thread_id,
                    "trigger": message.trigger,
                    "source_session_id": message.source_session_id,
                    "on_wake": message.on_wake,
                }),
                metadata: json!({
                    "source": format!("nanoclaw_{}", message.source),
                    "source_format": NANOCLAW_SOURCE_FORMAT,
                    "message_id": message.id,
                    "seq": message.seq,
                }),
            });
            result.captures.push((
                event_index.min(usize::MAX as u64) as usize,
                native_provider_capture(
                    NativeSessionDraft {
                        provider: CaptureProvider::NanoClaw,
                        source_format: NANOCLAW_SOURCE_FORMAT,
                        provider_session_id: provider_session_id.clone(),
                        parent_provider_session_id: None,
                        root_provider_session_id: None,
                        external_agent_id: session.agent_provider.clone(),
                        agent_type: AgentType::Primary,
                        role_hint: Some("container-session".to_owned()),
                        is_primary: true,
                        started_at,
                        ended_at: session.last_active.map(|timestamp| {
                            provider_timestamp_millis(Some(timestamp), context.imported_at)
                        }),
                        cwd: session.agent_group_folder.clone(),
                        fidelity: Fidelity::Partial,
                        raw_source_path: project_root.display().to_string(),
                        trust: ProviderSourceTrust::ProviderNative,
                        source_metadata: json!({
                            "adapter": NANOCLAW_SOURCE_FORMAT,
                            "central_db": central_path.display().to_string(),
                            "sqlite_user_version": user_version,
                            "schema_fingerprint": schema_fingerprint,
                            "support_level": "preview",
                        }),
                        session_metadata: json!({
                            "source_format": NANOCLAW_SOURCE_FORMAT,
                            "session_id": session.id,
                            "agent_group_id": session.agent_group_id,
                            "agent_group_name": session.agent_group_name,
                            "agent_provider": session.agent_provider,
                            "status": session.status,
                            "container_status": session.container_status,
                            "messaging_group_id": session.messaging_group_id,
                            "messaging": {
                                "channel_type": session.messaging_channel_type,
                                "platform_id": session.messaging_platform_id,
                                "instance": session.messaging_instance,
                                "name": session.messaging_name,
                                "thread_id": session.thread_id,
                            },
                        }),
                    },
                    context,
                    Some(event),
                ),
            ));
        }
    }
    Ok(result)
}

fn nanoclaw_project_root(path: &Path) -> Result<PathBuf> {
    if path.is_dir() && path.join("data").join("v2.db").is_file() {
        return Ok(path.to_path_buf());
    }
    if path.file_name().and_then(|name| name.to_str()) == Some("v2.db") {
        if let Some(data_dir) = path.parent() {
            if let Some(root) = data_dir.parent() {
                return Ok(root.to_path_buf());
            }
        }
    }
    Err(CaptureError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason: "NanoClaw import path must be a project root or data/v2.db",
    })
}

fn nanoclaw_event_index(message: &NanoClawMessageRow, seq: Option<u64>) -> u64 {
    if let Some(seq) = seq {
        let source_bucket = if message.source == "outbound" {
            500_000
        } else {
            0
        };
        let row_bucket = fnv1a64(format!("{}:{}", message.source, message.id).as_bytes()) % 500_000;
        return seq
            .saturating_mul(1_000_000)
            .saturating_add(source_bucket)
            .saturating_add(row_bucket);
    }
    text_id_index(&format!("{}:{}", message.source, message.id), 2_000_000_000)
}

fn nanoclaw_sessions(conn: &Connection) -> Result<Vec<NanoClawSessionRow>> {
    if !sqlite_table_exists(conn, "sessions")? {
        return Err(CaptureError::InvalidPayload(
            "NanoClaw data/v2.db is missing required sessions table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "sessions")?;
    ensure_sqlite_table_columns(
        &columns,
        "NanoClaw sessions table",
        &["id", "agent_group_id"],
    )?;
    let messaging_group_id = optional_column_expr(&columns, "messaging_group_id", "NULL");
    let thread_id = optional_column_expr(&columns, "thread_id", "NULL");
    let agent_provider = optional_column_expr(&columns, "agent_provider", "NULL");
    let status = optional_column_expr(&columns, "status", "NULL");
    let container_status = optional_column_expr(&columns, "container_status", "NULL");
    let last_active = optional_column_expr(&columns, "last_active", "NULL");
    let created_at = optional_column_expr(&columns, "created_at", "NULL");
    let agent_group_columns = if sqlite_table_exists(conn, "agent_groups")? {
        sqlite_table_columns(conn, "agent_groups")?
    } else {
        BTreeSet::new()
    };
    let agent_group_name =
        if agent_group_columns.contains("id") && agent_group_columns.contains("name") {
            "(select name from agent_groups where agent_groups.id = sessions.agent_group_id)"
        } else {
            "NULL"
        };
    let agent_group_folder =
        if agent_group_columns.contains("id") && agent_group_columns.contains("folder") {
            "(select folder from agent_groups where agent_groups.id = sessions.agent_group_id)"
        } else {
            "NULL"
        };
    let (messaging_channel_type, messaging_platform_id, messaging_instance, messaging_name) =
        if columns.contains("messaging_group_id") && sqlite_table_exists(conn, "messaging_groups")?
        {
            let messaging_columns = sqlite_table_columns(conn, "messaging_groups")?;
            (
                if messaging_columns.contains("id") && messaging_columns.contains("channel_type") {
                    "(select channel_type from messaging_groups where messaging_groups.id = sessions.messaging_group_id)"
                } else {
                    "NULL"
                },
                if messaging_columns.contains("id") && messaging_columns.contains("platform_id") {
                    "(select platform_id from messaging_groups where messaging_groups.id = sessions.messaging_group_id)"
                } else {
                    "NULL"
                },
                if messaging_columns.contains("id") && messaging_columns.contains("instance") {
                    "(select instance from messaging_groups where messaging_groups.id = sessions.messaging_group_id)"
                } else {
                    "NULL"
                },
                if messaging_columns.contains("id") && messaging_columns.contains("name") {
                    "(select name from messaging_groups where messaging_groups.id = sessions.messaging_group_id)"
                } else {
                    "NULL"
                },
            )
        } else {
            ("NULL", "NULL", "NULL", "NULL")
        };
    let sql = format!(
        "select id, agent_group_id, {messaging_group_id}, {thread_id}, {agent_provider}, \
         {status}, {container_status}, {last_active}, {created_at}, {agent_group_name}, \
         {agent_group_folder}, {messaging_channel_type}, {messaging_platform_id}, \
         {messaging_instance}, {messaging_name} from sessions order by created_at, id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(NanoClawSessionRow {
            id: row.get(0)?,
            agent_group_id: row.get(1)?,
            messaging_group_id: row.get(2)?,
            thread_id: row.get(3)?,
            agent_provider: row.get(4)?,
            status: row.get(5)?,
            container_status: row.get(6)?,
            last_active: row.get(7)?,
            created_at: row.get(8)?,
            agent_group_name: row.get(9)?,
            agent_group_folder: row.get(10)?,
            messaging_channel_type: row.get(11)?,
            messaging_platform_id: row.get(12)?,
            messaging_instance: row.get(13)?,
            messaging_name: row.get(14)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

fn nanoclaw_inbound_messages(path: &Path) -> Result<Vec<NanoClawMessageRow>> {
    let conn = open_provider_sqlite_readonly(path)?;
    if !sqlite_table_exists(&conn, "messages_in")? {
        return Ok(Vec::new());
    }
    let columns = sqlite_table_columns(&conn, "messages_in")?;
    ensure_sqlite_table_columns(&columns, "NanoClaw inbound messages table", &["id"])?;
    let seq = optional_column_expr(&columns, "seq", "NULL");
    let kind = optional_column_expr(&columns, "kind", "NULL");
    let timestamp = optional_column_expr(&columns, "timestamp", "NULL");
    let status = optional_column_expr(&columns, "status", "NULL");
    let trigger = optional_column_expr(&columns, "trigger", "NULL");
    let platform_id = optional_column_expr(&columns, "platform_id", "NULL");
    let channel_type = optional_column_expr(&columns, "channel_type", "NULL");
    let thread_id = optional_column_expr(&columns, "thread_id", "NULL");
    let content = optional_column_expr(&columns, "content", "NULL");
    let source_session_id = optional_column_expr(&columns, "source_session_id", "NULL");
    let on_wake = optional_column_expr(&columns, "on_wake", "NULL");
    let sql = format!(
        "select id, {seq}, {kind}, {timestamp}, {status}, {trigger}, {platform_id}, \
         {channel_type}, {thread_id}, {content}, {source_session_id}, {on_wake} \
         from messages_in order by {seq}, id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(NanoClawMessageRow {
            source: "inbound",
            id: row.get(0)?,
            seq: row.get(1)?,
            kind: row.get(2)?,
            timestamp: row.get(3)?,
            status: row.get(4)?,
            trigger: row.get(5)?,
            platform_id: row.get(6)?,
            channel_type: row.get(7)?,
            thread_id: row.get(8)?,
            content: row.get(9)?,
            source_session_id: row.get(10)?,
            on_wake: row.get(11)?,
            in_reply_to: None,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

fn nanoclaw_outbound_messages(path: &Path) -> Result<Vec<NanoClawMessageRow>> {
    let conn = open_provider_sqlite_readonly(path)?;
    if !sqlite_table_exists(&conn, "messages_out")? {
        return Ok(Vec::new());
    }
    let columns = sqlite_table_columns(&conn, "messages_out")?;
    ensure_sqlite_table_columns(&columns, "NanoClaw outbound messages table", &["id"])?;
    let seq = optional_column_expr(&columns, "seq", "NULL");
    let kind = optional_column_expr(&columns, "kind", "NULL");
    let timestamp = optional_column_expr(&columns, "timestamp", "NULL");
    let in_reply_to = optional_column_expr(&columns, "in_reply_to", "NULL");
    let platform_id = optional_column_expr(&columns, "platform_id", "NULL");
    let channel_type = optional_column_expr(&columns, "channel_type", "NULL");
    let thread_id = optional_column_expr(&columns, "thread_id", "NULL");
    let content = optional_column_expr(&columns, "content", "NULL");
    let sql = format!(
        "select id, {seq}, {kind}, {timestamp}, {in_reply_to}, {platform_id}, \
         {channel_type}, {thread_id}, {content} from messages_out order by {seq}, id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(NanoClawMessageRow {
            source: "outbound",
            id: row.get(0)?,
            seq: row.get(1)?,
            kind: row.get(2)?,
            timestamp: row.get(3)?,
            in_reply_to: row.get(4)?,
            platform_id: row.get(5)?,
            channel_type: row.get(6)?,
            thread_id: row.get(7)?,
            content: row.get(8)?,
            status: None,
            trigger: None,
            source_session_id: None,
            on_wake: None,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

fn normalize_shelley_sqlite(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let conn = open_provider_sqlite_readonly(path)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = opencode_schema_fingerprint(&conn)?;
    let conversations = shelley_conversations(&conn)?;
    let messages = shelley_messages(&conn)?;
    let conversations_by_id = conversations
        .iter()
        .map(|conversation| (conversation.conversation_id.clone(), conversation))
        .collect::<BTreeMap<_, _>>();
    let mut seen_message_conversations = BTreeSet::new();
    let raw_source_path = path.display().to_string();
    let mut result = ProviderNormalizationResult::default();

    for message in messages {
        let Some(conversation) = conversations_by_id.get(&message.conversation_id) else {
            result.summary.failed += 1;
            result.summary.failures.push(ProviderImportFailure {
                line: message.sequence_id.max(0) as usize,
                error: format!(
                    "Shelley message {} references missing conversation {}",
                    message.message_id, message.conversation_id
                ),
            });
            continue;
        };
        seen_message_conversations.insert(message.conversation_id.clone());
        let started_at = shelley_timestamp(conversation.created_at.as_deref(), context.imported_at);
        let ended_at = conversation
            .updated_at
            .as_deref()
            .map(|timestamp| shelley_timestamp(Some(timestamp), context.imported_at));
        let occurred_at = shelley_timestamp(message.created_at.as_deref(), started_at);
        let body = shelley_message_body(&message);
        let text = shelley_message_text(&message, &body)
            .unwrap_or_else(|| format!("Shelley {} message", message.entry_type));
        let event_type = shelley_event_type(&message, &body);
        let role = shelley_event_role(&message.entry_type);
        let event = native_event(NativeEventDraft {
            provider: CaptureProvider::Shelley,
            source_format: SHELLEY_SQLITE_SOURCE_FORMAT,
            provider_session_id: conversation.conversation_id.clone(),
            provider_event_index: shelley_event_index(&message),
            provider_event_hash: Some(message.message_id.clone()),
            cursor: format!(
                "conversation:{}:sequence:{}:message:{}",
                message.conversation_id, message.sequence_id, message.message_id
            ),
            event_type,
            role,
            occurred_at,
            text,
            body,
            metadata: json!({
                "source": "shelley_messages",
                "source_format": SHELLEY_SQLITE_SOURCE_FORMAT,
                "message_id": message.message_id,
                "conversation_id": message.conversation_id,
                "sequence_id": message.sequence_id,
                "rowid": message.rowid,
                "message_type": message.entry_type,
                "generation": message.generation,
                "excluded_from_context": message.excluded_from_context,
                "usage": message.usage_data.as_deref().map(provider_json_text),
                "llm_api_url": message.llm_api_url,
                "model_name": message.model_name,
                "forked_from_message_id": message.forked_from_message_id,
            }),
        });
        result.captures.push((
            message.rowid.max(0) as usize,
            shelley_capture(
                ShelleyCaptureDraft {
                    conversation,
                    started_at,
                    ended_at,
                    raw_source_path: &raw_source_path,
                    user_version,
                    schema_fingerprint: &schema_fingerprint,
                    event: Some(event),
                },
                context,
            ),
        ));
    }

    for conversation in conversations {
        if seen_message_conversations.contains(&conversation.conversation_id) {
            continue;
        }
        let started_at = shelley_timestamp(conversation.created_at.as_deref(), context.imported_at);
        let ended_at = conversation
            .updated_at
            .as_deref()
            .map(|timestamp| shelley_timestamp(Some(timestamp), context.imported_at));
        result.captures.push((
            0,
            shelley_capture(
                ShelleyCaptureDraft {
                    conversation: &conversation,
                    started_at,
                    ended_at,
                    raw_source_path: &raw_source_path,
                    user_version,
                    schema_fingerprint: &schema_fingerprint,
                    event: None,
                },
                context,
            ),
        ));
    }

    Ok(result)
}

struct ShelleyCaptureDraft<'a> {
    conversation: &'a ShelleyConversationRow,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    raw_source_path: &'a str,
    user_version: i64,
    schema_fingerprint: &'a str,
    event: Option<ProviderEventEnvelope>,
}

fn shelley_capture(
    draft: ShelleyCaptureDraft<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let ShelleyCaptureDraft {
        conversation,
        started_at,
        ended_at,
        raw_source_path,
        user_version,
        schema_fingerprint,
        event,
    } = draft;
    let is_subagent = conversation.parent_conversation_id.is_some() || !conversation.user_initiated;
    let conversation_options = conversation
        .conversation_options
        .as_deref()
        .map(provider_json_text)
        .unwrap_or(Value::Null);
    let tags = conversation
        .tags
        .as_deref()
        .map(provider_json_text)
        .unwrap_or(Value::Null);
    let queued_messages = conversation
        .queued_messages
        .as_deref()
        .map(provider_json_text)
        .unwrap_or(Value::Null);
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::Shelley,
            source_format: SHELLEY_SQLITE_SOURCE_FORMAT,
            provider_session_id: conversation.conversation_id.clone(),
            parent_provider_session_id: conversation.parent_conversation_id.clone(),
            root_provider_session_id: conversation.parent_conversation_id.clone(),
            external_agent_id: None,
            agent_type: if is_subagent {
                AgentType::Subagent
            } else {
                AgentType::Primary
            },
            role_hint: Some(if is_subagent { "subagent" } else { "primary" }.to_owned()),
            is_primary: !is_subagent,
            started_at,
            ended_at,
            cwd: conversation.cwd.clone(),
            fidelity: Fidelity::Imported,
            raw_source_path: raw_source_path.to_owned(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": SHELLEY_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": user_version,
                "schema_fingerprint": schema_fingerprint,
                "source_path": raw_source_path,
            }),
            session_metadata: json!({
                "source_format": SHELLEY_SQLITE_SOURCE_FORMAT,
                "conversation_id": conversation.conversation_id,
                "slug": conversation.slug,
                "title": conversation.slug,
                "user_initiated": conversation.user_initiated,
                "archived": conversation.archived,
                "parent_conversation_id": conversation.parent_conversation_id,
                "model": conversation.model,
                "conversation_options": conversation_options,
                "current_generation": conversation.current_generation,
                "agent_working": conversation.agent_working,
                "tags": tags,
                "is_draft": conversation.is_draft,
                "draft": conversation.draft,
                "queued_messages": queued_messages,
            }),
        },
        context,
        event,
    )
}

fn shelley_conversations(conn: &Connection) -> Result<Vec<ShelleyConversationRow>> {
    if !sqlite_table_exists(conn, "conversations")? {
        return Err(CaptureError::InvalidPayload(
            "Shelley shelley.db is missing required conversations table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "conversations")?;
    ensure_sqlite_table_columns(
        &columns,
        "Shelley conversations table",
        &["conversation_id"],
    )?;
    let slug = optional_column_expr(&columns, "slug", "NULL");
    let user_initiated = optional_column_expr(&columns, "user_initiated", "1");
    let created_at = optional_column_expr(&columns, "created_at", "NULL");
    let updated_at = optional_column_expr(&columns, "updated_at", "NULL");
    let cwd = optional_column_expr(&columns, "cwd", "NULL");
    let archived = optional_column_expr(&columns, "archived", "0");
    let parent_conversation_id = optional_column_expr(&columns, "parent_conversation_id", "NULL");
    let model = optional_column_expr(&columns, "model", "NULL");
    let conversation_options = optional_column_expr(&columns, "conversation_options", "NULL");
    let current_generation = optional_column_expr(&columns, "current_generation", "NULL");
    let agent_working = optional_column_expr(&columns, "agent_working", "0");
    let tags = optional_column_expr(&columns, "tags", "NULL");
    let is_draft = optional_column_expr(&columns, "is_draft", "0");
    let draft = optional_column_expr(&columns, "draft", "NULL");
    let queued_messages = optional_column_expr(&columns, "queued_messages", "NULL");
    let sql = format!(
        "select conversation_id, {slug}, {user_initiated}, {created_at}, {updated_at}, \
         {cwd}, {archived}, {parent_conversation_id}, {model}, {conversation_options}, \
         {current_generation}, {agent_working}, {tags}, {is_draft}, {draft}, \
         {queued_messages} \
         from conversations order by {created_at}, conversation_id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(ShelleyConversationRow {
            conversation_id: row.get(0)?,
            slug: row.get(1)?,
            user_initiated: sqlite_bool(row.get::<_, Option<i64>>(2)?),
            created_at: row.get(3)?,
            updated_at: row.get(4)?,
            cwd: row.get(5)?,
            archived: sqlite_bool(row.get::<_, Option<i64>>(6)?),
            parent_conversation_id: row.get(7)?,
            model: row.get(8)?,
            conversation_options: row.get(9)?,
            current_generation: row.get(10)?,
            agent_working: sqlite_bool(row.get::<_, Option<i64>>(11)?),
            tags: row.get(12)?,
            is_draft: sqlite_bool(row.get::<_, Option<i64>>(13)?),
            draft: row.get(14)?,
            queued_messages: row.get(15)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

fn shelley_messages(conn: &Connection) -> Result<Vec<ShelleyMessageRow>> {
    if !sqlite_table_exists(conn, "messages")? {
        return Err(CaptureError::InvalidPayload(
            "Shelley shelley.db is missing required messages table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "messages")?;
    ensure_sqlite_table_columns(
        &columns,
        "Shelley messages table",
        &["message_id", "conversation_id", "type"],
    )?;
    let sequence_id = optional_column_expr(&columns, "sequence_id", "rowid");
    let llm_data = optional_column_expr(&columns, "llm_data", "NULL");
    let user_data = optional_column_expr(&columns, "user_data", "NULL");
    let usage_data = optional_column_expr(&columns, "usage_data", "NULL");
    let created_at = optional_column_expr(&columns, "created_at", "NULL");
    let display_data = optional_column_expr(&columns, "display_data", "NULL");
    let excluded_from_context = optional_column_expr(&columns, "excluded_from_context", "0");
    let generation = optional_column_expr(&columns, "generation", "NULL");
    let llm_api_url = optional_column_expr(&columns, "llm_api_url", "NULL");
    let model_name = optional_column_expr(&columns, "model_name", "NULL");
    let forked_from_message_id = optional_column_expr(&columns, "forked_from_message_id", "NULL");
    let sql = format!(
        "select rowid, message_id, conversation_id, {sequence_id}, type, {llm_data}, \
         {user_data}, {usage_data}, {created_at}, {display_data}, \
         {excluded_from_context}, {generation}, {llm_api_url}, {model_name}, \
         {forked_from_message_id} from messages order by conversation_id, {sequence_id}, rowid"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(ShelleyMessageRow {
            rowid: row.get(0)?,
            message_id: row.get(1)?,
            conversation_id: row.get(2)?,
            sequence_id: row.get(3)?,
            entry_type: row.get(4)?,
            llm_data: row.get(5)?,
            user_data: row.get(6)?,
            usage_data: row.get(7)?,
            created_at: row.get(8)?,
            display_data: row.get(9)?,
            excluded_from_context: sqlite_bool(row.get::<_, Option<i64>>(10)?),
            generation: row.get(11)?,
            llm_api_url: row.get(12)?,
            model_name: row.get(13)?,
            forked_from_message_id: row.get(14)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

fn sqlite_bool(value: Option<i64>) -> bool {
    value.unwrap_or(0) != 0
}

fn shelley_timestamp(raw: Option<&str>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return fallback;
    };
    parse_rfc3339_utc(raw)
        .or_else(|| {
            NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f")
                .ok()
                .map(|naive| DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
        })
        .unwrap_or(fallback)
}

fn shelley_message_body(message: &ShelleyMessageRow) -> Value {
    json!({
        "message_id": message.message_id,
        "conversation_id": message.conversation_id,
        "sequence_id": message.sequence_id,
        "type": message.entry_type,
        "llm_data": message.llm_data.as_deref().map(provider_json_text),
        "user_data": message.user_data.as_deref().map(provider_json_text),
        "display_data": message.display_data.as_deref().map(provider_json_text),
        "usage_data": message.usage_data.as_deref().map(provider_json_text),
    })
}

fn shelley_message_text(message: &ShelleyMessageRow, body: &Value) -> Option<String> {
    let mut parts = Vec::new();
    for pointer in ["/user_data", "/llm_data", "/display_data"] {
        if let Some(text) = body.pointer(pointer).and_then(shelley_value_text) {
            parts.push(text);
        }
    }
    if parts.is_empty() && message.entry_type == "system" {
        Some("Shelley system message".to_owned())
    } else if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

fn shelley_event_role(entry_type: &str) -> Option<EventRole> {
    Some(match entry_type {
        "user" => EventRole::User,
        "agent" | "assistant" => EventRole::Assistant,
        "tool" => EventRole::Tool,
        "system" | "error" | "gitinfo" | "warning" | "modelchange" => EventRole::System,
        _ => EventRole::Unknown,
    })
}

fn shelley_event_type(message: &ShelleyMessageRow, body: &Value) -> EventType {
    match message.entry_type.as_str() {
        "tool" => EventType::ToolOutput,
        "gitinfo" => EventType::VcsChange,
        "system" | "error" | "warning" | "modelchange" => EventType::Notice,
        "agent" | "assistant" if shelley_value_has_tool_use(body) => EventType::ToolCall,
        "user" | "agent" | "assistant" if shelley_value_has_tool_result(body) => {
            EventType::ToolOutput
        }
        "user" | "agent" | "assistant" => EventType::Message,
        _ => EventType::Notice,
    }
}

fn shelley_event_index(message: &ShelleyMessageRow) -> u64 {
    let sequence = message.sequence_id.max(0) as u64;
    let bucket = text_id_index(
        &format!("{}:{}", message.conversation_id, message.message_id),
        4_096,
    );
    sequence.saturating_mul(4_096).saturating_add(bucket)
}

fn shelley_value_has_tool_use(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(shelley_value_has_tool_use),
        Value::Object(object) => {
            let content_type = shelley_content_type(value);
            matches!(
                content_type.as_deref(),
                Some("tool_use" | "server_tool_use")
            ) || object.values().any(shelley_value_has_tool_use)
        }
        _ => false,
    }
}

fn shelley_value_has_tool_result(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(shelley_value_has_tool_result),
        Value::Object(object) => {
            let content_type = shelley_content_type(value);
            matches!(
                content_type.as_deref(),
                Some("tool_result" | "web_search_tool_result" | "web_search_result")
            ) || object.values().any(shelley_value_has_tool_result)
        }
        _ => false,
    }
}

fn shelley_value_text(value: &Value) -> Option<String> {
    let mut parts = Vec::new();
    shelley_collect_text(value, &mut parts);
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn shelley_collect_text(value: &Value, parts: &mut Vec<String>) {
    match value {
        Value::String(text) => shelley_push_text(parts, text),
        Value::Array(items) => {
            for item in items {
                if shelley_text_budget_remaining(parts) == 0 {
                    break;
                }
                shelley_collect_text(item, parts);
            }
        }
        Value::Object(object) => {
            if let Some(kind) = shelley_content_type(value) {
                let handled = match kind.as_str() {
                    "text" => {
                        if let Some(text) = object.get("Text").and_then(Value::as_str) {
                            shelley_push_text(parts, text);
                        }
                        true
                    }
                    "thinking" | "redacted_thinking" => {
                        if let Some(text) = object.get("Thinking").and_then(Value::as_str) {
                            shelley_push_text(parts, text);
                        }
                        true
                    }
                    "tool_use" | "server_tool_use" => {
                        let name = object
                            .get("ToolName")
                            .and_then(Value::as_str)
                            .unwrap_or("tool");
                        shelley_push_text(parts, &format!("tool call: {name}"));
                        if let Some(input) = object.get("ToolInput") {
                            if !input.is_null() {
                                let input = provider_capped_json(input, PROVIDER_MAX_PREVIEW_CHARS);
                                shelley_push_text(parts, &format!("tool input: {input}"));
                            }
                        }
                        true
                    }
                    "tool_result" | "web_search_tool_result" => {
                        shelley_push_text(parts, "tool result");
                        if let Some(results) = object.get("ToolResult") {
                            shelley_collect_text(results, parts);
                        }
                        if let Some(display) = object.get("Display") {
                            shelley_collect_text(display, parts);
                        }
                        true
                    }
                    "web_search_result" => {
                        for key in ["Title", "URL", "PageAge"] {
                            if let Some(text) = object.get(key).and_then(Value::as_str) {
                                shelley_push_text(parts, text);
                            }
                        }
                        true
                    }
                    _ => false,
                };
                if handled {
                    return;
                }
            }

            for key in [
                "Text",
                "text",
                "Thinking",
                "thinking",
                "content",
                "Content",
                "output",
                "Output",
                "summary",
                "Summary",
                "message",
                "Message",
                "error",
                "Error",
                "LLMContent",
                "ToolResult",
                "Display",
            ] {
                if shelley_text_budget_remaining(parts) == 0 {
                    break;
                }
                if let Some(child) = object.get(key) {
                    shelley_collect_text(child, parts);
                }
            }
        }
        Value::Number(_) | Value::Bool(_) | Value::Null => {}
    }
}

fn shelley_push_text(parts: &mut Vec<String>, text: &str) {
    let text = text.trim();
    if !text.is_empty() {
        let remaining = shelley_text_budget_remaining(parts);
        if remaining == 0 {
            return;
        }
        let separator_budget = usize::from(!parts.is_empty());
        if remaining <= separator_budget {
            return;
        }
        let (text, _) = capped_text(text, remaining - separator_budget);
        parts.push(text);
    }
}

fn shelley_text_budget_remaining(parts: &[String]) -> usize {
    let used = parts.iter().map(|part| part.chars().count()).sum::<usize>()
        + parts.len().saturating_sub(1);
    (PROVIDER_MAX_TEXT_CHARS + 1).saturating_sub(used)
}

fn shelley_content_type(value: &Value) -> Option<String> {
    let raw = value.get("Type")?;
    if let Some(text) = raw.as_str() {
        let normalized = text.trim().to_ascii_lowercase();
        return match normalized.as_str() {
            "contenttypetext" => Some("text".to_owned()),
            "contenttypethinking" => Some("thinking".to_owned()),
            "contenttyperedactedthinking" => Some("redacted_thinking".to_owned()),
            "contenttypetooluse" => Some("tool_use".to_owned()),
            "contenttypetoolresult" => Some("tool_result".to_owned()),
            "contenttypeservertooluse" => Some("server_tool_use".to_owned()),
            "contenttypewebsearchtoolresult" => Some("web_search_tool_result".to_owned()),
            "contenttypewebsearchresult" => Some("web_search_result".to_owned()),
            _ => Some(normalized),
        };
    }
    raw.as_i64().and_then(|kind| {
        match kind {
            2 => Some("text"),
            3 => Some("thinking"),
            4 => Some("redacted_thinking"),
            5 => Some("tool_use"),
            6 => Some("tool_result"),
            7 => Some("server_tool_use"),
            8 => Some("web_search_tool_result"),
            9 => Some("web_search_result"),
            _ => None,
        }
        .map(str::to_owned)
    })
}

#[derive(Debug, Clone)]
struct AstrBotConversationRow {
    row_id: i64,
    inner_conversation_id: Option<String>,
    conversation_id: String,
    platform_id: Option<String>,
    user_id: Option<String>,
    content: String,
    title: Option<String>,
    persona_id: Option<String>,
    token_usage: Option<String>,
    created_at: Option<i64>,
    updated_at: Option<i64>,
}

#[derive(Debug, Clone)]
struct AstrBotPlatformMessageRow {
    id: i64,
    platform_id: Option<String>,
    user_id: Option<String>,
    sender_id: Option<String>,
    sender_name: Option<String>,
    content: Option<String>,
    llm_checkpoint_id: Option<String>,
    created_at: Option<i64>,
}

fn normalize_astrbot_sqlite(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let conn = open_provider_sqlite_readonly(path)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = opencode_schema_fingerprint(&conn)?;
    let conversations = astrbot_conversations(&conn)?;
    let platform_messages = astrbot_platform_messages(&conn)?;
    let selected_conversation = astrbot_selected_conversation(&conn).ok().flatten();
    let mut result = ProviderNormalizationResult::default();
    let mut checkpoint_sessions = BTreeMap::<String, String>::new();

    for conversation in &conversations {
        let conversation_line = match provider_nonnegative_i64_to_u64(
            conversation.row_id,
            "AstrBot conversation row id",
        ) {
            Ok(value) => provider_line_from_index(value),
            Err(err) => {
                push_provider_import_failure(&mut result.summary, 0, err.to_string());
                continue;
            }
        };
        let provider_session_id = astrbot_provider_session_id(conversation);
        let started_at = provider_timestamp_millis(conversation.created_at, context.imported_at);
        let ended_at = conversation
            .updated_at
            .map(|timestamp| provider_timestamp_millis(Some(timestamp), context.imported_at));
        let content = provider_json_text(&conversation.content);
        if let Value::Array(items) = &content {
            for (index, item) in items.iter().enumerate() {
                if let Some(checkpoint) = astrbot_checkpoint_id(item) {
                    checkpoint_sessions.insert(checkpoint, provider_session_id.clone());
                    continue;
                }
                let role = astrbot_role(item);
                let text = astrbot_item_text(item)
                    .unwrap_or_else(|| "AstrBot conversation item".to_owned());
                let event = native_event(NativeEventDraft {
                    provider: CaptureProvider::AstrBot,
                    source_format: ASTRBOT_SQLITE_SOURCE_FORMAT,
                    provider_session_id: provider_session_id.clone(),
                    provider_event_index: index as u64,
                    provider_event_hash: astrbot_item_id(item)
                        .map(|id| format!("conversation:{id}")),
                    cursor: format!("conversation:{}:item:{index}", conversation.conversation_id),
                    event_type: EventType::Message,
                    role,
                    occurred_at: started_at,
                    text,
                    body: item.clone(),
                    metadata: json!({
                        "source": "astrbot_conversations",
                        "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                        "conversation_id": conversation.conversation_id,
                        "inner_conversation_id": conversation.inner_conversation_id,
                        "item_index": index,
                    }),
                });
                result.captures.push((
                    index + 1,
                    astrbot_capture(
                        AstrBotCaptureDraft {
                            conversation,
                            provider_session_id: &provider_session_id,
                            started_at,
                            ended_at,
                            path,
                            user_version,
                            schema_fingerprint: &schema_fingerprint,
                            selected_conversation: selected_conversation.as_deref(),
                            event: Some(event),
                        },
                        context,
                    ),
                ));
            }
        } else {
            let text =
                provider_value_text(&content).unwrap_or_else(|| "AstrBot conversation".to_owned());
            let event = native_event(NativeEventDraft {
                provider: CaptureProvider::AstrBot,
                source_format: ASTRBOT_SQLITE_SOURCE_FORMAT,
                provider_session_id: provider_session_id.clone(),
                provider_event_index: 0,
                provider_event_hash: Some(format!("conversation-row:{}", conversation.row_id)),
                cursor: format!("conversation:{}:content", conversation.conversation_id),
                event_type: EventType::Message,
                role: None,
                occurred_at: started_at,
                text,
                body: content.clone(),
                metadata: json!({
                    "source": "astrbot_conversations",
                    "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                    "conversation_id": conversation.conversation_id,
                }),
            });
            result.captures.push((
                conversation_line,
                astrbot_capture(
                    AstrBotCaptureDraft {
                        conversation,
                        provider_session_id: &provider_session_id,
                        started_at,
                        ended_at,
                        path,
                        user_version,
                        schema_fingerprint: &schema_fingerprint,
                        selected_conversation: selected_conversation.as_deref(),
                        event: Some(event),
                    },
                    context,
                ),
            ));
        }
    }

    let conversations_by_id = conversations
        .iter()
        .map(|conversation| (astrbot_provider_session_id(conversation), conversation))
        .collect::<BTreeMap<_, _>>();
    for message in platform_messages {
        let message_id =
            match provider_nonnegative_i64_to_u64(message.id, "AstrBot platform message id") {
                Ok(value) => value,
                Err(err) => {
                    push_provider_import_failure(&mut result.summary, 0, err.to_string());
                    continue;
                }
            };
        let provider_session_id = message
            .llm_checkpoint_id
            .as_ref()
            .and_then(|checkpoint| checkpoint_sessions.get(checkpoint))
            .cloned()
            .unwrap_or_else(|| {
                format!(
                    "platform/{}/{}",
                    message.platform_id.as_deref().unwrap_or("unknown"),
                    message.user_id.as_deref().unwrap_or("unknown")
                )
            });
        let conversation = conversations_by_id.get(&provider_session_id).copied();
        let started_at = conversation
            .and_then(|conversation| conversation.created_at)
            .map(|timestamp| provider_timestamp_millis(Some(timestamp), context.imported_at))
            .unwrap_or_else(|| provider_timestamp_millis(message.created_at, context.imported_at));
        let content = message
            .content
            .as_deref()
            .map(provider_json_text)
            .unwrap_or(Value::Null);
        let text =
            provider_value_text(&content).unwrap_or_else(|| "AstrBot platform message".to_owned());
        let role = if message.sender_id.as_deref() == message.user_id.as_deref() {
            Some(EventRole::User)
        } else {
            Some(EventRole::Assistant)
        };
        let event_index = 1_000_000u64.saturating_add(message_id);
        let event = native_event(NativeEventDraft {
            provider: CaptureProvider::AstrBot,
            source_format: ASTRBOT_SQLITE_SOURCE_FORMAT,
            provider_session_id: provider_session_id.clone(),
            provider_event_index: event_index,
            provider_event_hash: Some(format!("platform-message:{}", message.id)),
            cursor: format!("platform_message_history:id:{}", message.id),
            event_type: EventType::Message,
            role,
            occurred_at: provider_timestamp_millis(message.created_at, started_at),
            text,
            body: json!({
                "message_id": message.id,
                "platform_id": message.platform_id,
                "user_id": message.user_id,
                "sender_id": message.sender_id,
                "sender_name": message.sender_name,
                "content": content,
                "llm_checkpoint_id": message.llm_checkpoint_id,
            }),
            metadata: json!({
                "source": "astrbot_platform_message_history",
                "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                "message_id": message.id,
            }),
        });
        if let Some(conversation) = conversation {
            result.captures.push((
                event_index.min(usize::MAX as u64) as usize,
                astrbot_capture(
                    AstrBotCaptureDraft {
                        conversation,
                        provider_session_id: &provider_session_id,
                        started_at,
                        ended_at: conversation.updated_at.map(|timestamp| {
                            provider_timestamp_millis(Some(timestamp), context.imported_at)
                        }),
                        path,
                        user_version,
                        schema_fingerprint: &schema_fingerprint,
                        selected_conversation: selected_conversation.as_deref(),
                        event: Some(event),
                    },
                    context,
                ),
            ));
        } else {
            result.captures.push((
                event_index.min(usize::MAX as u64) as usize,
                native_provider_capture(
                    NativeSessionDraft {
                        provider: CaptureProvider::AstrBot,
                        source_format: ASTRBOT_SQLITE_SOURCE_FORMAT,
                        provider_session_id: provider_session_id.clone(),
                        parent_provider_session_id: None,
                        root_provider_session_id: None,
                        external_agent_id: message.platform_id.clone(),
                        agent_type: AgentType::Primary,
                        role_hint: Some("platform-history".to_owned()),
                        is_primary: true,
                        started_at,
                        ended_at: None,
                        cwd: None,
                        fidelity: Fidelity::Partial,
                        raw_source_path: path.display().to_string(),
                        trust: ProviderSourceTrust::ProviderNative,
                        source_metadata: json!({
                            "adapter": ASTRBOT_SQLITE_SOURCE_FORMAT,
                            "sqlite_user_version": user_version,
                            "schema_fingerprint": schema_fingerprint,
                            "support_level": "preview",
                        }),
                        session_metadata: json!({
                            "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                            "platform_id": message.platform_id,
                            "user_id": message.user_id,
                            "fidelity_gap": "platform history row was not linked to a conversations checkpoint",
                        }),
                    },
                    context,
                    Some(event),
                ),
            ));
        }
    }

    Ok(result)
}

fn astrbot_provider_session_id(conversation: &AstrBotConversationRow) -> String {
    conversation
        .inner_conversation_id
        .as_ref()
        .or(Some(&conversation.conversation_id))
        .cloned()
        .unwrap_or_else(|| format!("conversation-row-{}", conversation.row_id))
}

struct AstrBotCaptureDraft<'a> {
    conversation: &'a AstrBotConversationRow,
    provider_session_id: &'a str,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    path: &'a Path,
    user_version: i64,
    schema_fingerprint: &'a str,
    selected_conversation: Option<&'a str>,
    event: Option<ProviderEventEnvelope>,
}

fn astrbot_capture(
    draft: AstrBotCaptureDraft<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let AstrBotCaptureDraft {
        conversation,
        provider_session_id,
        started_at,
        ended_at,
        path,
        user_version,
        schema_fingerprint,
        selected_conversation,
        event,
    } = draft;
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::AstrBot,
            source_format: ASTRBOT_SQLITE_SOURCE_FORMAT,
            provider_session_id: provider_session_id.to_owned(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: conversation.platform_id.clone(),
            agent_type: AgentType::Primary,
            role_hint: Some("llm-context".to_owned()),
            is_primary: true,
            started_at,
            ended_at,
            cwd: None,
            fidelity: Fidelity::Partial,
            raw_source_path: path.display().to_string(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": ASTRBOT_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": user_version,
                "schema_fingerprint": schema_fingerprint,
                "support_level": "preview",
            }),
            session_metadata: json!({
                "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                "conversation_id": conversation.conversation_id,
                "inner_conversation_id": conversation.inner_conversation_id,
                "platform_id": conversation.platform_id,
                "user_id": conversation.user_id,
                "title": conversation.title,
                "persona_id": conversation.persona_id,
                "token_usage": conversation.token_usage.as_deref().map(provider_json_text),
                "selected_conversation": selected_conversation,
                "fidelity_gap": "AstrBot preview imports local LLM context plus available platform history; it may not be a complete raw IM transcript",
            }),
        },
        context,
        event,
    )
}

fn astrbot_item_id(item: &Value) -> Option<&str> {
    item.get("id")
        .or_else(|| item.get("message_id"))
        .or_else(|| item.get("checkpoint_id"))
        .and_then(Value::as_str)
}

fn astrbot_checkpoint_id(item: &Value) -> Option<String> {
    let item_type = item
        .get("type")
        .or_else(|| item.get("role"))
        .and_then(Value::as_str)?;
    if item_type != "_checkpoint" && item_type != "checkpoint" {
        return None;
    }
    astrbot_item_id(item).map(str::to_owned)
}

fn astrbot_role(item: &Value) -> Option<EventRole> {
    item.get("role")
        .or_else(|| item.get("type"))
        .and_then(Value::as_str)
        .map(|role| provider_role(Some(role)))
}

fn astrbot_item_text(item: &Value) -> Option<String> {
    item.get("content")
        .or_else(|| item.get("text"))
        .or_else(|| item.get("message"))
        .and_then(provider_value_text)
}

fn astrbot_conversations(conn: &Connection) -> Result<Vec<AstrBotConversationRow>> {
    if !sqlite_table_exists(conn, "conversations")? {
        return Err(CaptureError::InvalidPayload(
            "AstrBot data_v4.db is missing required conversations table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "conversations")?;
    ensure_sqlite_table_columns(&columns, "AstrBot conversations table", &["content"])?;
    let row_id = if columns.contains("id") {
        "id"
    } else {
        "rowid"
    };
    let inner_conversation_id = optional_column_expr(&columns, "inner_conversation_id", "NULL");
    let conversation_id = optional_column_expr(
        &columns,
        "conversation_id",
        optional_column_expr(&columns, "inner_conversation_id", "CAST(rowid AS TEXT)"),
    );
    let platform_id = optional_column_expr(&columns, "platform_id", "NULL");
    let user_id = optional_column_expr(&columns, "user_id", "NULL");
    let title = optional_column_expr(&columns, "title", "NULL");
    let persona_id = optional_column_expr(&columns, "persona_id", "NULL");
    let token_usage = optional_column_expr(&columns, "token_usage", "NULL");
    let created_at = optional_column_expr(&columns, "created_at", "NULL");
    let updated_at = optional_column_expr(&columns, "updated_at", "NULL");
    let sql = format!(
        "select {row_id}, {inner_conversation_id}, {conversation_id}, {platform_id}, \
         {user_id}, content, {title}, {persona_id}, {token_usage}, {created_at}, \
         {updated_at} from conversations order by {created_at}, {row_id}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(AstrBotConversationRow {
            row_id: row.get(0)?,
            inner_conversation_id: row.get(1)?,
            conversation_id: row.get::<_, String>(2)?,
            platform_id: row.get(3)?,
            user_id: row.get(4)?,
            content: row.get(5)?,
            title: row.get(6)?,
            persona_id: row.get(7)?,
            token_usage: row.get(8)?,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

fn astrbot_platform_messages(conn: &Connection) -> Result<Vec<AstrBotPlatformMessageRow>> {
    if !sqlite_table_exists(conn, "platform_message_history")? {
        return Ok(Vec::new());
    }
    let columns = sqlite_table_columns(conn, "platform_message_history")?;
    let id = if columns.contains("id") {
        "id"
    } else {
        "rowid"
    };
    let platform_id = optional_column_expr(&columns, "platform_id", "NULL");
    let user_id = optional_column_expr(&columns, "user_id", "NULL");
    let sender_id = optional_column_expr(&columns, "sender_id", "NULL");
    let sender_name = optional_column_expr(&columns, "sender_name", "NULL");
    let content = optional_column_expr(&columns, "content", "NULL");
    let llm_checkpoint_id = optional_column_expr(&columns, "llm_checkpoint_id", "NULL");
    let created_at = optional_column_expr(&columns, "created_at", "NULL");
    let sql = format!(
        "select {id}, {platform_id}, {user_id}, {sender_id}, {sender_name}, \
         {content}, {llm_checkpoint_id}, {created_at} from platform_message_history \
         order by {created_at}, {id}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(AstrBotPlatformMessageRow {
            id: row.get(0)?,
            platform_id: row.get(1)?,
            user_id: row.get(2)?,
            sender_id: row.get(3)?,
            sender_name: row.get(4)?,
            content: row.get(5)?,
            llm_checkpoint_id: row.get(6)?,
            created_at: row.get(7)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

fn astrbot_selected_conversation(conn: &Connection) -> Result<Option<String>> {
    if !sqlite_table_exists(conn, "preferences")? {
        return Ok(None);
    }
    let columns = sqlite_table_columns(conn, "preferences")?;
    if !columns.contains("key") || !columns.contains("value") {
        return Ok(None);
    }
    let scope_filter = if columns.contains("scope") {
        "AND scope = 'umo'"
    } else {
        ""
    };
    let sql =
        format!("select value from preferences where key = 'sel_conv_id' {scope_filter} limit 1");
    let value = conn
        .query_row(&sql, [], |row| row.get::<_, Option<String>>(0))
        .optional()?
        .flatten();
    Ok(value)
}

fn normalize_continue_cli_sessions(
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

fn collect_continue_session_json_paths(root: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
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

fn continue_session_json_path(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("json")
        && path.file_name().and_then(|name| name.to_str()) != Some("sessions.json")
}

fn continue_session_index(paths: &[PathBuf]) -> BTreeMap<String, Value> {
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

fn continue_session_id(session: &Value, path: &Path) -> Option<String> {
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

fn continue_session_started_at(
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

fn continue_history_item_timestamp(item: &Value, fallback: DateTime<Utc>) -> DateTime<Utc> {
    item.get("timestamp")
        .or_else(|| item.get("createdAt"))
        .or_else(|| item.pointer("/message/timestamp"))
        .map(|value| provider_timestamp_value(Some(value), fallback))
        .unwrap_or(fallback)
}

fn continue_capture(
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

fn continue_history_item_event(
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

fn continue_history_item_text(item: &Value) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(text) = item
        .pointer("/message/content")
        .and_then(provider_value_text)
        .or_else(|| item.get("editorState").and_then(provider_value_text))
    {
        parts.push(text);
    }
    if let Some(text) = item
        .get("contextItems")
        .and_then(continue_context_items_text)
    {
        parts.push(text);
    }
    if let Some(text) = item
        .get("toolCallStates")
        .and_then(continue_tool_states_text)
    {
        parts.push(text);
    }
    if let Some(text) = item.get("conversationSummary").and_then(Value::as_str) {
        parts.push(text.to_owned());
    }
    let text = parts
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(text)
}

fn continue_context_items_text(value: &Value) -> Option<String> {
    let items = value.as_array()?;
    let mut parts = Vec::new();
    for item in items {
        if let Some(content) = item.get("content").and_then(provider_value_text) {
            parts.push(content);
        } else if let Some(name) = item.get("name").and_then(Value::as_str) {
            parts.push(name.to_owned());
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn continue_tool_states_text(value: &Value) -> Option<String> {
    let states = value.as_array()?;
    let mut parts = Vec::new();
    for state in states {
        let name = state
            .pointer("/toolCall/function/name")
            .or_else(|| state.pointer("/toolCall/name"))
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let status = state
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        parts.push(format!("tool call: {name} ({status})"));
        if let Some(output) = state.get("output").and_then(provider_value_text) {
            parts.push(output);
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn normalize_opencode_sqlite(
    path: &Path,
    context: &ProviderAdapterContext,
    dialect: &OpenCodeSqliteDialect,
) -> Result<ProviderNormalizationResult> {
    let conn = open_provider_sqlite_readonly(path)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = opencode_schema_fingerprint(&conn)?;
    let legacy_message_rows = opencode_count(&conn, "message").unwrap_or(0);
    let legacy_part_rows = opencode_count(&conn, "part").unwrap_or(0);
    let sessions = opencode_sessions(&conn, dialect)?;
    let messages = opencode_session_messages(&conn, dialect)?;
    let mut result = ProviderNormalizationResult::default();
    let mut session_started = BTreeMap::new();
    for session in &sessions {
        session_started.insert(
            session.id.clone(),
            provider_required_timestamp_millis(
                session.time_created,
                dialect.session_time_created_field,
            )?,
        );
    }
    let sessions_by_id = sessions
        .into_iter()
        .map(|session| (session.id.clone(), session))
        .collect::<BTreeMap<_, _>>();
    let raw_source_path = path.display().to_string();

    for row in messages {
        let provider_event_index =
            match provider_nonnegative_i64_to_u64(row.seq, dialect.session_message_seq_field) {
                Ok(value) => value,
                Err(err) => {
                    push_provider_import_failure(&mut result.summary, 0, err.to_string());
                    continue;
                }
            };
        let line = provider_line_from_index(provider_event_index);
        let Some(session) = sessions_by_id.get(&row.session_id) else {
            push_provider_import_failure(
                &mut result.summary,
                line,
                format!(
                    "{} session_message {} references missing session {}",
                    dialect.display_name, row.id, row.session_id
                ),
            );
            continue;
        };
        let data: Value = match serde_json::from_str(&row.data) {
            Ok(data) => data,
            Err(err) => {
                push_provider_import_failure(
                    &mut result.summary,
                    line,
                    format!("invalid JSON in session_message {}: {err}", row.id),
                );
                continue;
            }
        };
        let occurred_at = match opencode_event_time(&data, dialect) {
            Ok(Some(time)) => time,
            Ok(None) => match provider_required_timestamp_millis(
                row.time_created,
                dialect.session_message_time_created_field,
            ) {
                Ok(time) => time,
                Err(err) => {
                    push_provider_import_failure(&mut result.summary, line, err.to_string());
                    continue;
                }
            },
            Err(err) => {
                push_provider_import_failure(&mut result.summary, line, err.to_string());
                continue;
            }
        };
        let started_at = session_started
            .get(&session.id)
            .copied()
            .unwrap_or(occurred_at);
        let event = opencode_event(&row, &data, occurred_at, provider_event_index, dialect);
        result
            .files_touched
            .extend(provider_file_touches_from_raw_value(
                dialect.provider,
                &session.id,
                dialect.source_format,
                Some(raw_source_path.as_str()),
                &data,
                &event,
                line,
            ));
        let is_subagent = session.parent_id.is_some();
        result.captures.push((
            line,
            ProviderCaptureEnvelope {
                schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
                provider: dialect.provider,
                source: ProviderSourceEnvelope {
                    source_format: dialect.source_format.to_owned(),
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
                            stream: provider_cursor_stream(
                                dialect.provider,
                                dialect.source_format,
                            ),
                            cursor: format!("session_message:{}:seq:{}", row.session_id, row.seq),
                            observed_at: occurred_at,
                        }),
                    }),
                    idempotency_key: Some(format!(
                        "provider-source:{}:{}:{}",
                        dialect.provider.as_str(),
                        dialect.source_format,
                        session.id
                    )),
                    metadata: json!({
                        "adapter": dialect.source_format,
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
                    idempotency_key: Some(format!(
                        "provider-session:{}:{}",
                        dialect.provider.as_str(),
                        session.id
                    )),
                    artifacts: Vec::new(),
                    metadata: json!({
                        "source_format": dialect.source_format,
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

fn opencode_sessions(
    conn: &Connection,
    dialect: &OpenCodeSqliteDialect,
) -> Result<Vec<OpenCodeSessionRow>> {
    if !sqlite_table_exists(conn, "session")? {
        return Err(CaptureError::InvalidPayload(
            format!(
                "{} SQLite database is missing required session table",
                dialect.display_name
            )
            .into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "session")?;
    ensure_sqlite_table_columns(
        &columns,
        &format!("{} SQLite session table", dialect.display_name),
        &["id"],
    )?;
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

fn opencode_session_messages(
    conn: &Connection,
    dialect: &OpenCodeSqliteDialect,
) -> Result<Vec<OpenCodeMessageRow>> {
    if sqlite_table_exists(conn, "session_message")? {
        let rows = opencode_session_message_rows(conn, dialect)?;
        if !rows.is_empty() {
            return Ok(rows);
        }
    }
    if sqlite_table_exists(conn, "session_entry")? {
        let rows = opencode_session_entry_rows(conn, dialect)?;
        if !rows.is_empty() {
            return Ok(rows);
        }
    }
    if sqlite_table_exists(conn, "message")? {
        return opencode_message_rows(conn, dialect);
    }
    Ok(Vec::new())
}

fn opencode_session_message_rows(
    conn: &Connection,
    dialect: &OpenCodeSqliteDialect,
) -> Result<Vec<OpenCodeMessageRow>> {
    let columns = sqlite_table_columns(conn, "session_message")?;
    ensure_sqlite_table_columns(
        &columns,
        &format!("{} SQLite session_message table", dialect.display_name),
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

fn opencode_session_entry_rows(
    conn: &Connection,
    dialect: &OpenCodeSqliteDialect,
) -> Result<Vec<OpenCodeMessageRow>> {
    let columns = sqlite_table_columns(conn, "session_entry")?;
    ensure_sqlite_table_columns(
        &columns,
        &format!("{} SQLite session_entry table", dialect.display_name),
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

fn opencode_message_rows(
    conn: &Connection,
    dialect: &OpenCodeSqliteDialect,
) -> Result<Vec<OpenCodeMessageRow>> {
    let columns = sqlite_table_columns(conn, "message")?;
    ensure_sqlite_table_columns(
        &columns,
        &format!("{} SQLite message table", dialect.display_name),
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
    provider_event_index: u64,
    dialect: &OpenCodeSqliteDialect,
) -> ProviderEventEnvelope {
    let event_type = opencode_event_type(&row.entry_type, data);
    let role = Some(provider_role(Some(&row.entry_type)));
    let text = opencode_event_text(&row.entry_type, data, event_type, dialect);
    let (text, truncated) = provider_local_preview(&text, PROVIDER_MAX_TEXT_CHARS);
    ProviderEventEnvelope {
        provider_event_index,
        provider_event_hash: Some(row.id.clone()),
        cursor: Some(format!(
            "session_message:{}:seq:{}",
            row.session_id, row.seq
        )),
        event_type,
        role,
        occurred_at,
        fidelity: Fidelity::Imported,
        redaction_state: RedactionState::LocalPreview,
        idempotency_key: Some(format!(
            "provider-event:{}:{}:{}",
            dialect.provider.as_str(),
            row.session_id,
            row.id
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
            "source": dialect.source_format,
            "source_format": dialect.source_format,
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

fn opencode_event_text(
    entry_type: &str,
    data: &Value,
    event_type: EventType,
    dialect: &OpenCodeSqliteDialect,
) -> String {
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
        format!("{} event: {entry_type}", dialect.display_name)
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

fn opencode_event_time(
    data: &Value,
    dialect: &OpenCodeSqliteDialect,
) -> Result<Option<DateTime<Utc>>> {
    let Some(value) = data.pointer("/time/created") else {
        return Ok(None);
    };
    let millis = value.as_i64().ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "{} event time.created must be integer millis",
            dialect.display_name
        ))
    })?;
    provider_required_timestamp_millis(millis, dialect.event_time_created_field).map(Some)
}

fn parse_json_object_string(value: Option<&str>) -> Value {
    value
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
        .unwrap_or(Value::Null)
}

fn normalize_openhands_file_events(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let mut event_paths = Vec::new();
    collect_openhands_event_paths(path, &mut event_paths)?;
    event_paths.sort();
    if event_paths.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no OpenHands event JSON files found under v1_conversations",
        });
    }

    let mut result = ProviderNormalizationResult::default();
    let mut events_by_session = BTreeMap::<String, Vec<OpenHandsEventFile>>::new();
    for event_path in event_paths {
        let line_number = openhands_line_number(&event_path);
        let Some(session_id) = openhands_conversation_id_from_path(&event_path) else {
            continue;
        };
        let value = match read_json_file_limited(
            &event_path,
            MAX_PROVIDER_JSONL_LINE_BYTES,
            "OpenHands event JSON",
        ) {
            Ok(value) => value,
            Err(err) => {
                push_provider_import_failure(&mut result.summary, line_number, err.to_string());
                continue;
            }
        };
        let event_id = openhands_event_id(&event_path, &value);
        let timestamp = match openhands_event_timestamp(&value) {
            Some(timestamp) => timestamp,
            None => {
                push_provider_import_failure(
                    &mut result.summary,
                    line_number,
                    format!("OpenHands event {event_id} missing valid timestamp"),
                );
                continue;
            }
        };
        let user_id = openhands_user_id_from_path(&event_path);
        events_by_session
            .entry(session_id.clone())
            .or_default()
            .push(OpenHandsEventFile {
                path: event_path,
                line_number,
                session_id,
                user_id,
                event_id,
                timestamp,
                value,
            });
    }

    for events in events_by_session.values_mut() {
        events.sort_by(|left, right| {
            left.timestamp
                .cmp(&right.timestamp)
                .then_with(|| left.event_id.cmp(&right.event_id))
                .then_with(|| left.path.cmp(&right.path))
        });
        let started_at = events
            .first()
            .map(|event| event.timestamp)
            .unwrap_or(context.imported_at);
        let ended_at = events.last().map(|event| event.timestamp);
        let session_id = events
            .first()
            .map(|event| event.session_id.clone())
            .unwrap_or_else(|| "unknown-conversation".to_owned());
        let user_id = events.iter().find_map(|event| event.user_id.clone());
        let raw_source_path = events
            .first()
            .and_then(|event| event.path.parent())
            .unwrap_or(path)
            .display()
            .to_string();
        let cwd = events.iter().find_map(openhands_event_cwd);

        for (index, event_file) in events.iter().enumerate() {
            let provider_event_index = index as u64;
            let event = openhands_provider_event(&session_id, event_file, provider_event_index);
            result
                .files_touched
                .extend(provider_file_touches_from_raw_value(
                    CaptureProvider::OpenHands,
                    &session_id,
                    OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                    Some(raw_source_path.as_str()),
                    &event_file.value,
                    &event,
                    event_file.line_number,
                ));
            result.captures.push((
                event_file.line_number,
                native_provider_capture(
                    NativeSessionDraft {
                        provider: CaptureProvider::OpenHands,
                        source_format: OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                        provider_session_id: session_id.clone(),
                        parent_provider_session_id: None,
                        root_provider_session_id: None,
                        external_agent_id: user_id.clone(),
                        agent_type: AgentType::Primary,
                        role_hint: Some("primary".to_owned()),
                        is_primary: true,
                        started_at,
                        ended_at,
                        cwd: cwd.clone(),
                        fidelity: Fidelity::Imported,
                        raw_source_path: raw_source_path.clone(),
                        trust: ProviderSourceTrust::ProviderNative,
                        source_metadata: json!({
                            "adapter": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                            "storage": "filesystem_event_service",
                            "conversation_dir": raw_source_path,
                        }),
                        session_metadata: json!({
                            "source_format": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                            "provider": "openhands",
                            "conversation_id": session_id,
                            "user_id": user_id,
                            "event_count": events.len(),
                        }),
                    },
                    context,
                    Some(event),
                ),
            ));
        }
    }

    Ok(result)
}

fn collect_openhands_event_paths(root: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
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
        if openhands_json_path_is_event(root) {
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
            collect_openhands_event_paths(&path, paths)?;
        } else if openhands_json_path_is_event(&path) {
            ensure_regular_provider_transcript_file(&path)?;
            paths.push(path);
        }
    }
    Ok(())
}

fn openhands_json_path_is_event(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("json")
        && provider_path_has_component(path, "v1_conversations")
}

fn openhands_conversation_id_from_path(path: &Path) -> Option<String> {
    let mut components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str());
    while let Some(component) = components.next() {
        if component == "v1_conversations" {
            return components
                .next()
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned);
        }
    }
    None
}

fn openhands_user_id_from_path(path: &Path) -> Option<String> {
    let components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    components.windows(2).find_map(|window| {
        (window[1] == "v1_conversations" && !window[0].trim().is_empty())
            .then(|| window[0].to_owned())
    })
}

fn openhands_event_id(path: &Path, value: &Value) -> String {
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.trim().is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| path.display().to_string())
}

fn openhands_event_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_utc)
}

fn openhands_line_number(path: &Path) -> usize {
    fnv1a64(path.display().to_string().as_bytes()) as usize
}

fn openhands_event_cwd(event: &OpenHandsEventFile) -> Option<String> {
    event
        .value
        .pointer("/observation/metadata/working_dir")
        .or_else(|| event.value.pointer("/observation/metadata/cwd"))
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.trim().is_empty())
        .map(str::to_owned)
}

fn openhands_provider_event(
    session_id: &str,
    event_file: &OpenHandsEventFile,
    provider_event_index: u64,
) -> ProviderEventEnvelope {
    let entry_type = openhands_entry_type(&event_file.value);
    let event_type = openhands_event_type(&event_file.value, &entry_type);
    let role = Some(openhands_role(&event_file.value, &entry_type));
    let text = openhands_event_text(&event_file.value, &entry_type, event_type);
    native_event(NativeEventDraft {
        provider: CaptureProvider::OpenHands,
        source_format: OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
        provider_session_id: session_id.to_owned(),
        provider_event_index,
        provider_event_hash: Some(event_file.event_id.clone()),
        cursor: format!("{}:{}", event_file.path.display(), event_file.event_id),
        event_type,
        role,
        occurred_at: event_file.timestamp,
        text,
        body: event_file.value.clone(),
        metadata: json!({
            "source": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
            "source_format": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
            "event_id": event_file.event_id,
            "entry_type": entry_type,
            "event_path": event_file.path.display().to_string(),
            "conversation_id": session_id,
            "tool_name": event_file.value.get("tool_name").and_then(Value::as_str),
            "tool_call_id": event_file.value.get("tool_call_id").and_then(Value::as_str),
            "action_id": event_file.value.get("action_id").and_then(Value::as_str),
        }),
    })
}

fn openhands_entry_type(value: &Value) -> String {
    if let Some(entry_type) = value
        .get("kind")
        .or_else(|| value.get("type"))
        .and_then(Value::as_str)
    {
        return entry_type.to_owned();
    }
    if value.get("llm_message").is_some() {
        "MessageEvent".to_owned()
    } else if value.get("action").is_some() {
        "ActionEvent".to_owned()
    } else if value.get("observation").is_some() {
        "ObservationEvent".to_owned()
    } else {
        "OpenHandsEvent".to_owned()
    }
}

fn openhands_event_type(value: &Value, entry_type: &str) -> EventType {
    if value.get("llm_message").is_some() || entry_type == "MessageEvent" {
        return EventType::Message;
    }
    if value.get("action").is_some() || entry_type == "ActionEvent" {
        return match value.pointer("/action/kind").and_then(Value::as_str) {
            Some("FinishAction") => EventType::Message,
            Some("ThinkAction") => EventType::Summary,
            Some("FileEditorAction" | "StrReplaceEditorAction" | "PlanningFileEditorAction") => {
                EventType::ToolCall
            }
            _ => EventType::ToolCall,
        };
    }
    if value.get("observation").is_some() || entry_type == "ObservationEvent" {
        return match value.pointer("/observation/kind").and_then(Value::as_str) {
            Some(
                "FileEditorObservation"
                | "StrReplaceEditorObservation"
                | "PlanningFileEditorObservation",
            ) => EventType::FileTouched,
            Some("ExecuteBashObservation" | "TerminalObservation") => EventType::CommandOutput,
            _ => EventType::ToolOutput,
        };
    }
    match entry_type {
        "StreamingDeltaEvent" => EventType::Message,
        "CondensationSummaryEvent" | "CondensationEvent" => EventType::Summary,
        "AgentErrorEvent" | "ConversationErrorEvent" | "ServerErrorEvent" => EventType::ToolOutput,
        _ => EventType::Notice,
    }
}

fn openhands_role(value: &Value, entry_type: &str) -> EventRole {
    if let Some(role) = value.pointer("/llm_message/role").and_then(Value::as_str) {
        return provider_role(Some(role));
    }
    match value.get("source").and_then(Value::as_str) {
        Some("user") => EventRole::User,
        Some("agent") => EventRole::Assistant,
        Some("environment" | "hook") => EventRole::Tool,
        Some(source) => provider_role(Some(source)),
        None if entry_type == "ActionEvent" => EventRole::Assistant,
        None if entry_type == "ObservationEvent" => EventRole::Tool,
        _ => EventRole::Unknown,
    }
}

fn openhands_event_text(value: &Value, entry_type: &str, event_type: EventType) -> String {
    if let Some(text) = value
        .pointer("/llm_message/content")
        .and_then(provider_value_text)
    {
        return text;
    }
    if let Some(text) = value.get("content").and_then(provider_value_text) {
        return text;
    }
    if let Some(text) = value.pointer("/action/message").and_then(Value::as_str) {
        return text.to_owned();
    }
    if let Some(text) = value.pointer("/action/thought").and_then(Value::as_str) {
        return text.to_owned();
    }
    if let Some(command) = value.pointer("/action/command").and_then(Value::as_str) {
        return command.to_owned();
    }
    if let Some(path) = value.pointer("/action/path").and_then(Value::as_str) {
        let command = value
            .pointer("/action/command")
            .and_then(Value::as_str)
            .unwrap_or("file");
        return format!("{command} {path}");
    }
    if let Some(content) = value
        .pointer("/observation/content")
        .and_then(provider_value_text)
    {
        return content;
    }
    if let Some(output) = value.pointer("/observation/output").and_then(Value::as_str) {
        return output.to_owned();
    }
    if let Some(error) = value
        .pointer("/observation/error")
        .and_then(Value::as_str)
        .or_else(|| value.get("error").and_then(Value::as_str))
    {
        return error.to_owned();
    }
    if let Some(prompt) = value.pointer("/action/prompt").and_then(Value::as_str) {
        return prompt.to_owned();
    }
    if event_type == EventType::Notice {
        format!("OpenHands event: {entry_type}")
    } else {
        serde_json::to_string(value).unwrap_or_else(|_| entry_type.to_owned())
    }
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
        merged.files_touched.append(&mut result.files_touched);
    }
    Ok(merged)
}

fn native_jsonl_missing_reason(provider: CaptureProvider) -> &'static str {
    match provider {
        CaptureProvider::Pi => "no Pi session JSONL files found",
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

    let header_index = if provider == CaptureProvider::Antigravity {
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
    let (text, truncated) = provider_local_preview(&text, PROVIDER_MAX_TEXT_CHARS);
    let event_id = native_jsonl_event_id(provider, value, line_number);
    let tool_calls = if provider == CaptureProvider::Antigravity {
        value
            .get("tool_calls")
            .map(|calls| provider_capped_json_value(calls, PROVIDER_MAX_PREVIEW_CHARS))
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

fn pi_session_event(
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

fn pi_provider_event_identity_index(header: &PiSessionHeader, entry: &Value) -> Option<u64> {
    entry
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(|id| fnv1a64(format!("pi:{}:{id}", header.id).as_bytes()))
}

fn pi_event_idempotency_key(header: &PiSessionHeader, entry: &Value, line_number: usize) -> String {
    entry
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(|id| format!("provider-event:pi:{}:{id}", header.id))
        .unwrap_or_else(|| format!("provider-event:pi:{}:{line_number}", header.id))
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

fn pi_entry_text(entry: &Value, message: Option<&Value>) -> Option<String> {
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
    message.get("content").and_then(pi_content_text)
}

fn pi_content_text(content: &Value) -> Option<String> {
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
    mut files_touched: Vec<(usize, ProviderFileTouchedEnvelope)>,
) -> Result<ProviderImportSummary> {
    let mut caches = ProviderImportCaches::default();
    let supplied_file_touch_lines = files_touched
        .iter()
        .map(|(line_number, _)| *line_number)
        .collect::<BTreeSet<_>>();
    for (line_number, capture) in &captures {
        if capture.provider == CaptureProvider::Codex {
            continue;
        }
        if supplied_file_touch_lines.contains(line_number) {
            continue;
        }
        if let Some(event) = &capture.event {
            files_touched.extend(provider_file_touches_from_event(
                capture.provider,
                &capture.session.provider_session_id,
                &capture.source.source_format,
                capture.source.raw_source_path.as_deref(),
                event,
                *line_number,
            ));
        }
    }
    let has_captures = !captures.is_empty() || !files_touched.is_empty();

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
    if let Err(err) = resolve_pending_provider_edges(store, &mut summary, &mut caches) {
        if has_captures && options.wrap_transaction {
            let _ = store.rollback_batch();
        }
        return Err(err);
    }
    for (line_number, file) in files_touched {
        if let Err(err) = import_provider_file_touched_line(store, &file, &options) {
            summary.failed += 1;
            summary.failures.push(ProviderImportFailure {
                line: line_number,
                error: err.to_string(),
            });
        }
    }
    if summary.failed > 0 && !options.allow_partial_failures {
        if has_captures && options.wrap_transaction {
            let _ = store.rollback_batch();
        }
        return Ok(summary);
    }
    if has_captures && options.wrap_transaction {
        if let Err(err) = store.commit_batch() {
            let _ = store.rollback_batch();
            return Err(err.into());
        }
    }

    Ok(summary)
}

fn import_provider_file_touched_line(
    store: &mut Store,
    file: &ProviderFileTouchedEnvelope,
    options: &NormalizedProviderImportOptions,
) -> Result<()> {
    let session_id = provider_session_uuid(file.provider, &file.provider_session_id);
    let source_id = provider_scoped_source_uuid(
        file.provider,
        &file.provider_session_id,
        &file.source_format,
        file.raw_source_path.as_deref(),
    );
    let event_id = match file.provider_event_index {
        Some(index) => provider_file_touch_event_id(
            store,
            file.provider,
            &file.provider_session_id,
            source_id,
            index,
        )?,
        None => None,
    };
    let touch_id = provider_file_touch_import_id(
        store,
        file.provider,
        &file.provider_session_id,
        source_id,
        file.provider_touch_index,
    )?;
    let touched = FileTouched {
        id: touch_id,
        history_record_id: options.history_record_id,
        run_id: None,
        event_id,
        vcs_workspace_id: None,
        path: file.path.clone(),
        change_kind: file.change_kind,
        old_path: file.old_path.clone(),
        line_count_delta: file.line_count_delta,
        confidence: file.confidence,
        timestamps: timestamps(file.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider": file.provider.as_str(),
                "provider_session_id": file.provider_session_id,
                "provider_touch_index": file.provider_touch_index,
                "provider_event_index": file.provider_event_index,
                "raw_source_path": file.raw_source_path,
                "source_id": source_id,
                "source_format": file.source_format,
                "metadata": file.metadata,
                "session_id": session_id,
            }),
        ),
    };
    store.upsert_file_touched(&touched)?;
    Ok(())
}

#[derive(Default)]
struct ProviderImportCaches {
    imported_sessions: BTreeSet<Uuid>,
    processed_sources: BTreeSet<Uuid>,
    processed_sessions: BTreeSet<Uuid>,
    imported_edges: BTreeSet<Uuid>,
    processed_edges: BTreeSet<Uuid>,
    session_exists: BTreeMap<Uuid, bool>,
    pi_event_identities_by_entry_id: BTreeMap<Uuid, BTreeMap<String, ProviderEventImportIdentity>>,
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
    let source_identity_key = provider_scoped_source_identity_key(
        provider,
        &session.provider_session_id,
        &source.source_format,
        source.raw_source_path.as_deref(),
    );
    let source_id = stable_capture_uuid(&source_identity_key, "source");
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
                "source_identity_key": source_identity_key,
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
        let pi_entry_id = event
            .metadata
            .get("entry_id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty());
        let legacy_provider_event_index = event
            .metadata
            .get("legacy_provider_event_index")
            .and_then(Value::as_u64)
            .filter(|_| !(provider == CaptureProvider::Pi && pi_entry_id.is_some()));
        let provider_event_identity_index = event
            .metadata
            .get("provider_event_identity_index")
            .and_then(Value::as_u64)
            .unwrap_or(event.provider_event_index);
        let event_identity = match pi_existing_event_identity_by_entry_id(
            store,
            provider,
            session_id,
            pi_entry_id,
            caches,
        )? {
            Some(identity) => identity,
            None => provider_event_import_identity(
                store,
                provider,
                &session.provider_session_id,
                source_id,
                provider_event_identity_index,
                event.provider_event_index,
                &event_hash,
                legacy_provider_event_index,
            )?,
        };
        let command_run = provider_command_run_from_event(ProviderCommandRunInput {
            provider,
            provider_session_id: &session.provider_session_id,
            session_id,
            source_id,
            run_source_id: event_identity.run_source_id,
            history_record_id: options.history_record_id,
            event,
            payload: &payload,
            event_hash: &event_hash,
        })?;
        let normalized_event = Event {
            id: event_identity.id,
            seq: event_identity.seq,
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
            dedupe_key: Some(event_identity.dedupe_key.clone()),
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
            let was_present = provider_event_exists(store, &event_identity.dedupe_key)?;
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
            redaction_state: RedactionState::LocalPreview,
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
        _ => RedactionState::LocalPreview,
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
    let mut archive = SessionHistoryArchive::default();

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
        "failed_at": utc_now(),
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

#[derive(Clone)]
struct ProviderEventImportIdentity {
    id: Uuid,
    seq: u64,
    dedupe_key: String,
    run_source_id: Option<Uuid>,
}

fn pi_existing_event_identity_by_entry_id(
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
    if !caches
        .pi_event_identities_by_entry_id
        .contains_key(&session_id)
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
        caches
            .pi_event_identities_by_entry_id
            .insert(session_id, identities);
    }
    Ok(caches
        .pi_event_identities_by_entry_id
        .get(&session_id)
        .and_then(|identities| identities.get(entry_id).cloned()))
}

fn pi_stored_event_entry_id(event: &Event) -> Option<&str> {
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

fn provider_event_import_identity(
    store: &Store,
    provider: CaptureProvider,
    provider_session_id: &str,
    source_id: Uuid,
    provider_event_index: u64,
    provider_event_sequence_index: u64,
    event_hash: &str,
    legacy_provider_event_index: Option<u64>,
) -> Result<ProviderEventImportIdentity> {
    let source_identity = provider_source_event_import_identity_with_seq(
        source_id,
        provider_event_index,
        provider_event_sequence_index,
        event_hash,
    );
    let source_identity = avoid_provider_source_event_seq_collision(
        store,
        source_identity,
        source_id,
        provider_event_index,
        provider_event_sequence_index,
    )?;
    if provider_event_exists(store, &source_identity.dedupe_key)?
        || provider_event_id_exists(store, source_identity.id)?
    {
        return Ok(source_identity);
    }

    if let Some(legacy_index) = legacy_provider_event_index {
        let legacy_source_identity =
            provider_source_event_import_identity(source_id, legacy_index, event_hash);
        if provider_event_exists(store, &legacy_source_identity.dedupe_key)?
            || provider_event_id_exists(store, legacy_source_identity.id)?
        {
            return Ok(legacy_source_identity);
        }

        let legacy_provider_identity = provider_legacy_event_import_identity(
            provider,
            provider_session_id,
            legacy_index,
            event_hash,
        );
        if provider_event_exists(store, &legacy_provider_identity.dedupe_key)?
            || provider_event_id_exists(store, legacy_provider_identity.id)?
        {
            return Ok(legacy_provider_identity);
        }
    }

    let legacy_identity = provider_legacy_event_import_identity(
        provider,
        provider_session_id,
        provider_event_index,
        event_hash,
    );
    if provider_event_exists(store, &legacy_identity.dedupe_key)?
        || provider_event_id_exists(store, legacy_identity.id)?
    {
        Ok(legacy_identity)
    } else {
        Ok(source_identity)
    }
}

fn provider_source_event_import_identity(
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

fn provider_source_event_import_identity_with_seq(
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

fn avoid_provider_source_event_seq_collision(
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

fn provider_event_seq_available(store: &Store, seq: u64, event_id: Uuid) -> Result<bool> {
    match store.event_id_by_seq(seq) {
        Ok(existing_id) => Ok(existing_id == event_id),
        Err(StoreError::Sql(rusqlite::Error::QueryReturnedNoRows)) => Ok(true),
        Err(err) => Err(CaptureError::Store(err)),
    }
}

fn provider_legacy_event_import_identity(
    provider: CaptureProvider,
    provider_session_id: &str,
    provider_event_index: u64,
    event_hash: &str,
) -> ProviderEventImportIdentity {
    ProviderEventImportIdentity {
        id: provider_event_uuid(provider, provider_session_id, provider_event_index),
        seq: provider_event_seq(provider, provider_session_id, provider_event_index),
        dedupe_key: Store::provider_event_dedupe_key(
            provider,
            provider_session_id,
            provider_event_index,
            event_hash,
        ),
        run_source_id: None,
    }
}

fn provider_file_touch_event_id(
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

fn provider_file_touch_import_id(
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

fn provider_event_id_exists(store: &Store, id: Uuid) -> Result<bool> {
    match store.get_event(id) {
        Ok(_) => Ok(true),
        Err(StoreError::NotFound(_)) => Ok(false),
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
    run_source_id: Option<Uuid>,
    history_record_id: Option<Uuid>,
    event: &'a ProviderEventEnvelope,
    payload: &'a Value,
    event_hash: &'a str,
}

fn provider_command_run_from_event(input: ProviderCommandRunInput<'_>) -> Result<Option<Run>> {
    let ProviderCommandRunInput {
        provider,
        provider_session_id,
        session_id,
        source_id,
        run_source_id,
        history_record_id,
        event,
        payload,
        event_hash,
    } = input;
    if event.event_type != EventType::CommandOutput {
        return Ok(None);
    }
    let command_preview = payload
        .get("command")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    let call_id = payload.get("call_id").and_then(Value::as_str);
    let key = call_id.unwrap_or(event_hash);
    let duration_ms = provider_command_duration_ms(payload)?;
    let ended_at = Some(event.occurred_at);
    let started_at = match duration_ms {
        Some(duration) => {
            let duration_value = duration;
            let duration = chrono::Duration::try_milliseconds(duration_value).ok_or_else(|| {
                CaptureError::InvalidPayload(format!(
                    "duration_ms is not representable as milliseconds: {duration_value}"
                ))
            })?;
            event
                .occurred_at
                .checked_sub_signed(duration)
                .ok_or_else(|| {
                    CaptureError::InvalidPayload(format!(
                        "duration_ms moves command start before representable time: {}",
                        duration_value
                    ))
                })?
        }
        None => event.occurred_at,
    };
    Ok(Some(Run {
        id: run_source_id
            .map(|source_id| provider_source_run_uuid(source_id, key))
            .unwrap_or_else(|| provider_run_uuid(provider, provider_session_id, key)),
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
    }))
}

fn provider_command_duration_ms(payload: &Value) -> Result<Option<i64>> {
    let Some(value) = payload.get("duration_ms") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let duration = value
        .as_i64()
        .ok_or_else(|| CaptureError::InvalidPayload("duration_ms must be an integer".to_owned()))?;
    if duration < 0 {
        return Err(CaptureError::InvalidPayload(format!(
            "duration_ms must be nonnegative, got {duration}"
        )));
    }
    Ok(Some(duration))
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

#[cfg(test)]
fn provider_source_uuid(provider: CaptureProvider, provider_session_id: &str) -> Uuid {
    stable_capture_uuid(
        &format!("provider:{}:{provider_session_id}", provider.as_str()),
        "source",
    )
}

fn provider_scoped_source_uuid(
    provider: CaptureProvider,
    provider_session_id: &str,
    source_format: &str,
    raw_source_path: Option<&str>,
) -> Uuid {
    stable_capture_uuid(
        &provider_scoped_source_identity_key(
            provider,
            provider_session_id,
            source_format,
            raw_source_path,
        ),
        "source",
    )
}

fn provider_scoped_source_identity_key(
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

fn provider_source_run_uuid(source_id: Uuid, run_key: &str) -> Uuid {
    stable_capture_uuid(&format!("provider-source:{source_id}:run:{run_key}"), "run")
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

fn provider_source_event_uuid(source_id: Uuid, provider_event_index: u64) -> Uuid {
    stable_capture_uuid(
        &format!("provider-source:{source_id}:event:{provider_event_index}"),
        "event",
    )
}

fn provider_file_touch_uuid(
    provider: CaptureProvider,
    provider_session_id: &str,
    provider_touch_index: u64,
) -> Uuid {
    stable_capture_uuid(
        &format!(
            "provider:{}:{provider_session_id}:file-touch:{provider_touch_index}",
            provider.as_str()
        ),
        "file-touch",
    )
}

fn provider_source_file_touch_uuid(source_id: Uuid, provider_touch_index: u64) -> Uuid {
    stable_capture_uuid(
        &format!("provider-source:{source_id}:file-touch:{provider_touch_index}"),
        "file-touch",
    )
}

fn provider_source_event_seq(source_id: Uuid, provider_event_index: u64) -> u64 {
    let source_key = source_id.to_string();
    ((fnv1a64(source_key.as_bytes()) & 0x0000_0000_7fff_ffff) << 32)
        | (provider_event_index & 0xffff_ffff)
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
    (value, false)
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

    fn custom_history_fixture(name: &str) -> PathBuf {
        materialized_fixture("custom-history-jsonl", name)
    }

    fn write_oversized_jsonl_line(path: &Path) {
        fs::write(path, vec![b'x'; MAX_PROVIDER_JSONL_LINE_BYTES + 1]).unwrap();
    }

    fn jsonl_line(value: Value) -> String {
        serde_json::to_string(&value).unwrap() + "\n"
    }

    fn test_provider_event(event_type: EventType) -> ProviderEventEnvelope {
        ProviderEventEnvelope {
            provider_event_index: 0,
            provider_event_hash: Some("event-hash".to_owned()),
            cursor: None,
            event_type,
            role: Some(EventRole::Tool),
            occurred_at: "2026-07-03T12:00:00Z".parse().unwrap(),
            fidelity: Fidelity::Imported,
            redaction_state: RedactionState::LocalPreview,
            idempotency_key: None,
            artifacts: Vec::new(),
            payload: json!({}),
            metadata: json!({}),
        }
    }

    fn materialized_fixture(category: &str, name: &str) -> PathBuf {
        let source = match category {
            "provider" => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/provider")
                .join(name),
            "provider-history" => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/provider-history")
                .join(name),
            "custom-history-jsonl" => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/custom-history-jsonl")
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
                        file_sha256: None,
                        event_count: Some(1),
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
    fn provider_fixture_replay_supports_pi_and_preserves_metadata() {
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
        assert_eq!(summary.redacted, 0);
        let session_id = provider_session_uuid(CaptureProvider::Pi, "pi-session-1");
        let events = store.events_for_session(session_id).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].redaction_state, RedactionState::LocalPreview);
        assert!(events[1]
            .sync
            .metadata
            .to_string()
            .contains("fixture-token-value"));
        assert!(!events[1].sync.metadata.to_string().contains("[REDACTED]"));
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
        assert_eq!(first.redacted, 0);

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
        assert!(events[3].payload.to_string().contains("fixture-secret"));
        assert!(!events[3].payload.to_string().contains("[REDACTED]"));
    }

    #[test]
    fn pi_session_import_rejects_malformed_event_timestamp() {
        let temp = tempdir();
        let path = temp.path().join("bad-timestamp-pi.jsonl");
        fs::write(
            &path,
            [
                jsonl_line(json!({
                    "type": "session",
                    "id": "pi-bad-timestamp",
                    "timestamp": "2026-07-03T12:00:00Z",
                    "version": 1
                })),
                jsonl_line(json!({
                    "type": "message",
                    "id": "pi-bad-event",
                    "timestamp": "not-rfc3339",
                    "message": {
                        "role": "user",
                        "content": "bad timestamp should not import"
                    }
                })),
            ]
            .concat(),
        )
        .unwrap();

        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let summary = import_pi_session_jsonl(
            &path,
            &mut store,
            PiSessionImportOptions {
                imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
                ..PiSessionImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 1, "{:?}", summary.failures);
        assert!(summary.failures[0]
            .error
            .contains("timestamp is not a valid RFC3339 timestamp"));
        assert!(store.list_sessions().unwrap().is_empty());
    }

    #[test]
    fn pi_session_import_uses_entry_ids_when_lines_shift() {
        let temp = tempdir();
        let fixture = temp.path().join("pi-line-shift.jsonl");
        fs::write(
            &fixture,
            concat!(
                "{\"type\":\"session\",\"version\":3,\"id\":\"pi-line-shift\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\"}\n",
                "{\"type\":\"message\",\"id\":\"stable-entry\",\"parentId\":null,\"timestamp\":\"2026-06-24T12:00:01Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"pi line shift stable\"}]}}\n",
            ),
        )
        .unwrap();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let first = import_pi_session_jsonl(
            &fixture,
            &mut store,
            PiSessionImportOptions {
                source_path: Some(fixture.clone()),
                imported_at: "2026-06-24T16:00:00Z".parse().unwrap(),
                ..PiSessionImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(first.imported_events, 1);

        let session_id = provider_session_uuid(CaptureProvider::Pi, "pi-line-shift");
        let first_event_id = store.events_for_session(session_id).unwrap()[0].id;

        fs::write(
            &fixture,
            concat!(
                "{\"type\":\"session\",\"version\":3,\"id\":\"pi-line-shift\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\"}\n",
                "{\"type\":\"model_change\",\"id\":\"inserted-entry\",\"parentId\":null,\"timestamp\":\"2026-06-24T12:00:00Z\",\"provider\":\"google\",\"modelId\":\"gemini-2.5-flash\"}\n",
                "{\"type\":\"message\",\"id\":\"stable-entry\",\"parentId\":\"inserted-entry\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"pi line shift stable\"}]}}\n",
            ),
        )
        .unwrap();

        let second = import_pi_session_jsonl(
            &fixture,
            &mut store,
            PiSessionImportOptions {
                source_path: Some(fixture.clone()),
                imported_at: "2026-06-24T16:01:00Z".parse().unwrap(),
                ..PiSessionImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(second.failed, 0, "{:?}", second.failures);
        assert_eq!(second.imported_events, 1, "{second:?}");
        assert_eq!(second.skipped_events, 1, "{second:?}");

        let events = store.events_for_session(session_id).unwrap();
        assert_eq!(events.len(), 2);
        let shifted = events
            .iter()
            .find(|event| event.payload.to_string().contains("pi line shift stable"))
            .unwrap();
        assert_eq!(shifted.id, first_event_id);
    }

    #[test]
    fn pi_session_identity_resolver_reuses_legacy_line_indexed_events() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let source_id = stable_capture_uuid("legacy-pi-source", "source");
        let legacy_index = 1;
        let event_hash = "0123456789abcdef";
        let legacy_identity =
            provider_source_event_import_identity(source_id, legacy_index, event_hash);
        store
            .upsert_event(&Event {
                id: legacy_identity.id,
                seq: legacy_identity.seq,
                history_record_id: None,
                session_id: None,
                run_id: None,
                event_type: EventType::Message,
                role: Some(EventRole::User),
                occurred_at: "2026-06-24T12:00:01Z".parse().unwrap(),
                capture_source_id: None,
                payload: json!({"text": "legacy line indexed pi event"}),
                payload_blob_id: None,
                dedupe_key: Some(legacy_identity.dedupe_key.clone()),
                redaction_state: RedactionState::LocalPreview,
                sync: provider_sync_metadata(Fidelity::Imported, json!({})),
            })
            .unwrap();

        let header = PiSessionHeader {
            id: "pi-legacy".to_owned(),
            version: Some(3),
            timestamp: "2026-06-24T12:00:00Z".parse().unwrap(),
            cwd: Some("/workspace".to_owned()),
            parent_session: None,
            raw: json!({}),
        };
        let stable_index =
            pi_provider_event_identity_index(&header, &json!({"id": "stable-entry"})).unwrap();

        let resolved = provider_event_import_identity(
            &store,
            CaptureProvider::Pi,
            "pi-legacy",
            source_id,
            stable_index,
            legacy_index + 1,
            event_hash,
            Some(legacy_index as u64),
        )
        .unwrap();

        assert_eq!(resolved.id, legacy_identity.id);
        assert_eq!(resolved.dedupe_key, legacy_identity.dedupe_key);
    }

    #[test]
    fn pi_session_import_reuses_legacy_line_indexed_event_by_entry_id_after_line_shift() {
        let temp = tempdir();
        let fixture = temp.path().join("pi-legacy-line-shift.jsonl");
        let provider_session_id = "pi-legacy-line-shift";
        let raw_path = fixture.display().to_string();
        let source_id = provider_scoped_source_uuid(
            CaptureProvider::Pi,
            provider_session_id,
            "pi_session_jsonl",
            Some(&raw_path),
        );
        let session_id = provider_session_uuid(CaptureProvider::Pi, provider_session_id);
        let legacy_identity = provider_source_event_import_identity(source_id, 1, "legacy-hash");
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let started_at = "2026-06-24T12:00:00Z".parse().unwrap();
        store
            .upsert_capture_source(&CaptureSource {
                id: source_id,
                descriptor: CaptureSourceDescriptor {
                    kind: CaptureSourceKind::ProviderImport,
                    provider: CaptureProvider::Pi,
                    machine_id: "test-machine".to_owned(),
                    process_id: None,
                    cwd: Some("/workspace".to_owned()),
                    raw_source_path: Some(raw_path.clone()),
                    external_session_id: Some(provider_session_id.to_owned()),
                },
                started_at,
                ended_at: None,
                sync: provider_sync_metadata(Fidelity::Imported, json!({})),
            })
            .unwrap();
        store
            .upsert_session(&Session {
                id: session_id,
                history_record_id: None,
                parent_session_id: None,
                root_session_id: None,
                capture_source_id: None,
                provider: CaptureProvider::Pi,
                external_session_id: Some(provider_session_id.to_owned()),
                external_agent_id: None,
                agent_type: AgentType::Primary,
                role_hint: Some("primary".to_owned()),
                is_primary: true,
                status: SessionStatus::Imported,
                transcript_blob_id: None,
                started_at,
                ended_at: None,
                timestamps: timestamps(started_at),
                sync: provider_sync_metadata(Fidelity::Imported, json!({})),
            })
            .unwrap();
        store
            .upsert_event(&Event {
                id: legacy_identity.id,
                seq: legacy_identity.seq,
                history_record_id: None,
                session_id: Some(session_id),
                run_id: None,
                event_type: EventType::Message,
                role: Some(EventRole::User),
                occurred_at: "2026-06-24T12:00:01Z".parse().unwrap(),
                capture_source_id: Some(source_id),
                payload: json!({
                    "provider": "pi",
                    "provider_session_id": provider_session_id,
                    "provider_event_index": 1,
                    "body": {
                        "entry_id": "stable-entry",
                        "text": "legacy stable oracle",
                        "body": {"id": "stable-entry"}
                    }
                }),
                payload_blob_id: None,
                dedupe_key: Some(legacy_identity.dedupe_key.clone()),
                redaction_state: RedactionState::LocalPreview,
                sync: provider_sync_metadata(
                    Fidelity::Imported,
                    json!({"metadata": {"entry_id": "stable-entry"}}),
                ),
            })
            .unwrap();

        fs::write(
            &fixture,
            concat!(
                "{\"type\":\"session\",\"version\":3,\"id\":\"pi-legacy-line-shift\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\"}\n",
                "{\"type\":\"model_change\",\"id\":\"inserted-entry\",\"parentId\":null,\"timestamp\":\"2026-06-24T12:00:00Z\",\"provider\":\"google\",\"modelId\":\"gemini-2.5-flash\"}\n",
                "{\"type\":\"message\",\"id\":\"stable-entry\",\"parentId\":\"inserted-entry\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"new stable oracle\"}]}}\n",
            ),
        )
        .unwrap();

        let summary = import_pi_session_jsonl(
            &fixture,
            &mut store,
            PiSessionImportOptions {
                source_path: Some(fixture.clone()),
                imported_at: "2026-06-24T16:00:00Z".parse().unwrap(),
                ..PiSessionImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 0, "{:?}", summary.failures);
        assert_eq!(summary.imported_events, 1);
        assert_eq!(summary.skipped_events, 1);
        let events = store.events_for_session(session_id).unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|event| event.id == legacy_identity.id));
        assert_eq!(
            events
                .iter()
                .filter(|event| event.payload.to_string().contains("stable-entry"))
                .count(),
            1
        );
    }

    #[test]
    fn pi_session_import_extracts_text_from_non_message_entries() {
        let temp = tempdir();
        let fixture = temp.path().join("pi-non-message-text.jsonl");
        fs::write(
            &fixture,
            concat!(
                "{\"type\":\"session\",\"version\":3,\"id\":\"pi-non-message-text\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\"}\n",
                "{\"type\":\"compaction\",\"id\":\"compact-entry\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"summary\":\"compacted plan oracle\"}\n",
                "{\"type\":\"branch_summary\",\"id\":\"branch-entry\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"summary\":\"branch summary oracle\"}\n",
                "{\"type\":\"custom_message\",\"id\":\"custom-message-entry\",\"timestamp\":\"2026-06-24T12:00:03Z\",\"content\":[{\"type\":\"text\",\"text\":\"custom message oracle\"}]}\n",
                "{\"type\":\"session_info\",\"id\":\"session-info-entry\",\"timestamp\":\"2026-06-24T12:00:04Z\",\"name\":\"session info oracle\"}\n",
                "{\"type\":\"model_change\",\"id\":\"model-entry\",\"timestamp\":\"2026-06-24T12:00:05Z\",\"provider\":\"google\",\"modelId\":\"gemini-2.5-flash\"}\n",
                "{\"type\":\"thinking_level_change\",\"id\":\"thinking-entry\",\"timestamp\":\"2026-06-24T12:00:06Z\",\"thinkingLevel\":\"high\"}\n",
                "{\"type\":\"label\",\"id\":\"label-entry\",\"timestamp\":\"2026-06-24T12:00:07Z\",\"label\":\"label oracle\"}\n",
                "{\"type\":\"custom\",\"id\":\"custom-entry\",\"timestamp\":\"2026-06-24T12:00:08Z\",\"customType\":\"custom type oracle\"}\n",
            ),
        )
        .unwrap();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_pi_session_jsonl(
            &fixture,
            &mut store,
            PiSessionImportOptions {
                source_path: Some(fixture.clone()),
                imported_at: "2026-06-24T16:00:00Z".parse().unwrap(),
                ..PiSessionImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 0, "{:?}", summary.failures);
        assert_eq!(summary.imported_events, 8);
        let session_id = provider_session_uuid(CaptureProvider::Pi, "pi-non-message-text");
        let events = store.events_for_session(session_id).unwrap();
        let texts = events
            .iter()
            .filter_map(|event| event.payload.pointer("/body/text").and_then(Value::as_str))
            .collect::<Vec<_>>();
        for expected in [
            "compacted plan oracle",
            "branch summary oracle",
            "custom message oracle",
            "session info oracle",
            "google/gemini-2.5-flash",
            "high",
            "label oracle",
            "custom type oracle",
        ] {
            assert!(
                texts.contains(&expected),
                "missing {expected:?} in texts {texts:?}"
            );
        }
    }

    #[test]
    fn pi_session_import_replays_default_session_directory_tree() {
        let temp = tempdir();
        let root = temp.path().join(".pi/agent/sessions/--workspace--");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("2026-06-24T12-00-00-000Z_pi-dir-alpha.jsonl"),
            concat!(
                "{\"type\":\"session\",\"version\":3,\"id\":\"pi-dir-alpha\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\"}\n",
                "{\"type\":\"message\",\"id\":\"pi-dir-alpha-user\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"alpha directory import\"}]}}\n",
            ),
        )
        .unwrap();
        fs::write(
            root.join("2026-06-24T12-01-00-000Z_pi-dir-beta.jsonl"),
            concat!(
                "{\"type\":\"session\",\"version\":3,\"id\":\"pi-dir-beta\",\"timestamp\":\"2026-06-24T12:01:00Z\",\"cwd\":\"/workspace\"}\n",
                "{\"type\":\"message\",\"id\":\"pi-dir-beta-user\",\"timestamp\":\"2026-06-24T12:01:01Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"beta directory import\"}]}}\n",
            ),
        )
        .unwrap();
        let sessions_root = temp.path().join(".pi/agent/sessions");
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let first = import_pi_session_jsonl(
            &sessions_root,
            &mut store,
            PiSessionImportOptions {
                source_path: Some(sessions_root.clone()),
                imported_at: "2026-06-24T16:00:00Z".parse().unwrap(),
                ..PiSessionImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(first.failed, 0, "{:?}", first.failures);
        assert_eq!(first.imported_sessions, 2);
        assert_eq!(first.imported_events, 2);

        let second = import_pi_session_jsonl(
            &sessions_root,
            &mut store,
            PiSessionImportOptions {
                source_path: Some(sessions_root.clone()),
                imported_at: "2026-06-24T16:00:00Z".parse().unwrap(),
                ..PiSessionImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(second.failed, 0, "{:?}", second.failures);
        assert_eq!(second.imported_events, 0);
        assert_eq!(second.skipped_events, 2);

        let alpha = provider_session_uuid(CaptureProvider::Pi, "pi-dir-alpha");
        let beta = provider_session_uuid(CaptureProvider::Pi, "pi-dir-beta");
        assert_eq!(store.events_for_session(alpha).unwrap().len(), 1);
        assert_eq!(store.events_for_session(beta).unwrap().len(), 1);
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
    fn codex_session_catalog_rejects_oversized_metadata_line() {
        let temp = tempdir();
        let root = temp.path().join("sessions/2026/07/03");
        fs::create_dir_all(&root).unwrap();
        write_oversized_jsonl_line(&root.join("oversized.jsonl"));
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let err = catalog_codex_session_tree(
            temp.path().join("sessions"),
            &store,
            CodexSessionCatalogOptions {
                source_root: Some(temp.path().join("sessions")),
                cataloged_at: "2026-07-03T12:00:00Z".parse().unwrap(),
                allow_partial_failures: false,
                ..CodexSessionCatalogOptions::default()
            },
        )
        .unwrap_err();

        assert!(
            err.to_string().contains("provider JSONL line exceeds"),
            "{err}"
        );
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
    #[ignore = "manual perf benchmark; private release gates run scripts/public-ctx/perf-smoke.sh from ctx-private"]
    fn synthetic_codex_incremental_import_perf_records_thresholded_evidence() {
        let out_dir = std::env::var_os("CTX_ARTIFACT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .ancestors()
                    .nth(2)
                    .unwrap()
                    .join("target/ctx-artifacts/synthetic_codex_incremental_import_perf")
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
    fn codex_session_file_rejects_symlinked_parent_components() {
        use std::os::unix::fs::symlink;

        let temp = tempdir();
        let real_dir = temp.path().join("real-parent");
        fs::create_dir_all(&real_dir).unwrap();
        let fixture = provider_history_fixture("codex-sessions").join("2026/06/23/root.jsonl");
        fs::copy(&fixture, real_dir.join("root.jsonl")).unwrap();
        let link_dir = temp.path().join("linked-parent");
        symlink(&real_dir, &link_dir).unwrap();
        let linked_file = link_dir.join("root.jsonl");

        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let err = import_codex_session_jsonl(
            &linked_file,
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
                if path.ends_with("linked-parent/root.jsonl")
                    && reason == "symlinked provider transcript path components are rejected"
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
    fn codex_session_jsonl_rejects_oversized_line() {
        let temp = tempdir();
        let path = temp.path().join("oversized-codex.jsonl");
        write_oversized_jsonl_line(&path);

        let err = CodexSessionJsonlAdapter
            .normalize_path(&path, &ProviderAdapterContext::default())
            .unwrap_err();
        assert!(err.to_string().contains("provider JSONL line exceeds"));
    }

    #[test]
    fn codex_session_jsonl_rejects_malformed_event_timestamp() {
        let temp = tempdir();
        let path = temp.path().join("bad-timestamp-codex.jsonl");
        fs::write(
            &path,
            [
                jsonl_line(json!({
                    "timestamp": "2026-07-03T12:00:00Z",
                    "type": "session_meta",
                    "payload": {
                        "id": "codex-bad-timestamp",
                        "timestamp": "2026-07-03T12:00:00Z",
                        "cwd": "/workspace",
                        "originator": "codex-cli"
                    }
                })),
                jsonl_line(json!({
                    "timestamp": "not-rfc3339",
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "user",
                        "content": [
                            {"type": "input_text", "text": "bad timestamp should not import"}
                        ]
                    }
                })),
            ]
            .concat(),
        )
        .unwrap();

        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let summary = import_codex_session_jsonl(
            &path,
            &mut store,
            CodexSessionImportOptions {
                imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
                fast_event_inserts: false,
                ..CodexSessionImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 1, "{:?}", summary.failures);
        assert!(summary.failures[0]
            .error
            .contains("timestamp is not a valid RFC3339 timestamp"));
        assert!(store.list_sessions().unwrap().is_empty());
    }

    #[test]
    fn provider_command_run_rejects_negative_duration() {
        let event = test_provider_event(EventType::CommandOutput);
        let err = provider_command_run_from_event(ProviderCommandRunInput {
            provider: CaptureProvider::Codex,
            provider_session_id: "duration-session",
            session_id: new_id(),
            source_id: new_id(),
            run_source_id: None,
            history_record_id: None,
            event: &event,
            payload: &json!({
                "command": "cargo test",
                "duration_ms": -1
            }),
            event_hash: "event-hash",
        })
        .unwrap_err();

        assert!(err.to_string().contains("duration_ms must be nonnegative"));
    }

    #[test]
    fn codex_session_tree_imports_rich_tool_outputs_and_preserves_previews() {
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
        assert!(rendered.contains("cargo test -p sample -- --token [REDACTED_SECRET]"));
        assert!(rendered.contains("unit tests passed in [REDACTED_PATH]"));
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
        let apply_patch = br#"{"timestamp":"2026-06-24T01:00:07.000Z","type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"*** Begin Patch\n*** Update File: crates/ctx-cli/src/main.rs\n@@\n-old\n+new\n*** End Patch","call_id":"call-patch","status":"completed"}}"#;

        for line in [
            session_meta.as_slice(),
            user_message.as_slice(),
            assistant_message.as_slice(),
            apply_patch.as_slice(),
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
    fn codex_search_event_mode_persists_file_touches_without_tool_events() {
        let temp = tempdir();
        let root = temp.path().join("codex-sessions/2026/06/24");
        fs::create_dir_all(&root).unwrap();
        let fixture = root.join("search-file-touch.jsonl");
        fs::write(
            &fixture,
            concat!(
                "{\"timestamp\":\"2026-06-24T01:00:00.000Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"codex-search-file-touch\",\"cwd\":\"/workspace/ctx\"}}\n",
                "{\"timestamp\":\"2026-06-24T01:00:01.000Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"Please update the CLI.\"}]}}\n",
                "{\"timestamp\":\"2026-06-24T01:00:02.000Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"name\":\"apply_patch\",\"input\":\"*** Begin Patch\\n*** Update File: crates/ctx-cli/src/main.rs\\n@@\\n-old\\n+new\\n*** End Patch\",\"call_id\":\"call-patch\",\"status\":\"completed\"}}\n",
            ),
        )
        .unwrap();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_codex_session_tree(
            temp.path().join("codex-sessions"),
            &mut store,
            CodexSessionImportOptions {
                source_path: Some(temp.path().join("codex-sessions")),
                imported_at: "2026-06-24T02:00:00Z".parse().unwrap(),
                event_mode: CodexEventImportMode::Search,
                tool_output_mode: CodexToolOutputMode::Skip,
                ..CodexSessionImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 0, "{:?}", summary.failures);
        assert_eq!(summary.imported_events, 1);

        let session_id = provider_session_uuid(CaptureProvider::Codex, "codex-search-file-touch");
        let events = store.events_for_session(session_id).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, EventType::Message);

        let archive = store.export_archive().unwrap();
        let touched = archive
            .files_touched
            .iter()
            .find(|file| file.path == "crates/ctx-cli/src/main.rs")
            .expect("apply_patch should create file touch metadata in search mode");
        assert_eq!(touched.change_kind, Some(FileChangeKind::Modified));
        assert_eq!(touched.event_id, None);
        assert_eq!(touched.history_record_id, None);
    }

    #[test]
    fn structured_file_touch_extractor_reads_nested_provider_paths() {
        let event = ProviderEventEnvelope {
            provider_event_index: 7,
            provider_event_hash: None,
            cursor: None,
            event_type: EventType::ToolCall,
            role: Some(EventRole::Assistant),
            occurred_at: "2026-06-24T01:00:00Z".parse().unwrap(),
            fidelity: Fidelity::Imported,
            redaction_state: RedactionState::LocalPreview,
            idempotency_key: None,
            artifacts: Vec::new(),
            payload: serde_json::json!({}),
            metadata: serde_json::json!({}),
        };
        let antigravity = serde_json::json!({
            "type": "CODE_ACTION",
            "tool_calls": [{
                "name": "write_to_file",
                "args": {
                    "TargetFile": "/workspace/demo/README.md",
                    "CodeContent": "# Demo\n"
                }
            }]
        });
        let cursor = serde_json::json!({
            "role": "assistant",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "name": "write_file",
                    "input": {
                        "path": "cursor-native-cli-oracle.txt",
                        "content": "proof"
                    }
                }]
            }
        });

        let antigravity_touches = provider_file_touches_from_raw_value(
            CaptureProvider::Antigravity,
            "agy-session",
            ANTIGRAVITY_CLI_SOURCE_FORMAT,
            None,
            &antigravity,
            &event,
            1,
        );
        let cursor_touches = provider_file_touches_from_raw_value(
            CaptureProvider::Cursor,
            "cursor-session",
            CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
            None,
            &cursor,
            &event,
            1,
        );

        assert_eq!(antigravity_touches[0].1.path, "/workspace/demo/README.md");
        assert_eq!(
            antigravity_touches[0].1.change_kind,
            Some(FileChangeKind::Created)
        );
        assert_eq!(cursor_touches[0].1.path, "cursor-native-cli-oracle.txt");
        assert_eq!(
            cursor_touches[0].1.change_kind,
            Some(FileChangeKind::Created)
        );
    }

    #[test]
    fn structured_file_touch_extractor_covers_provider_tool_shapes() {
        let event = ProviderEventEnvelope {
            provider_event_index: 11,
            provider_event_hash: None,
            cursor: None,
            event_type: EventType::ToolCall,
            role: Some(EventRole::Assistant),
            occurred_at: "2026-06-24T01:00:00Z".parse().unwrap(),
            fidelity: Fidelity::Imported,
            redaction_state: RedactionState::LocalPreview,
            idempotency_key: None,
            artifacts: Vec::new(),
            payload: serde_json::json!({}),
            metadata: serde_json::json!({}),
        };

        for (provider, source_format, raw, expected_path) in [
            (
                CaptureProvider::Claude,
                CLAUDE_PROJECTS_SOURCE_FORMAT,
                serde_json::json!({
                    "type": "assistant",
                    "message": {
                        "content": [{
                            "type": "tool_use",
                            "name": "Edit",
                            "input": {"file_path": "src/claude_file.rs"}
                        }]
                    }
                }),
                "src/claude_file.rs",
            ),
            (
                CaptureProvider::OpenCode,
                OPENCODE_SQLITE_SOURCE_FORMAT,
                serde_json::json!({
                    "content": [{
                        "type": "tool",
                        "name": "write",
                        "input": {"file": "src/opencode_file.rs"}
                    }]
                }),
                "src/opencode_file.rs",
            ),
            (
                CaptureProvider::Gemini,
                GEMINI_CLI_SOURCE_FORMAT,
                serde_json::json!({
                    "type": "gemini",
                    "toolCalls": [{
                        "name": "write_file",
                        "args": {"path": "src/gemini_file.rs", "content": "proof"}
                    }]
                }),
                "src/gemini_file.rs",
            ),
            (
                CaptureProvider::CopilotCli,
                COPILOT_CLI_SOURCE_FORMAT,
                serde_json::json!({
                    "type": "tool.execution_start",
                    "data": {
                        "toolName": "write_file",
                        "args": {"path": "src/copilot_file.rs"}
                    }
                }),
                "src/copilot_file.rs",
            ),
            (
                CaptureProvider::FactoryAiDroid,
                FACTORY_DROID_SOURCE_FORMAT,
                serde_json::json!({
                    "type": "message",
                    "content": [{
                        "type": "tool_use",
                        "name": "write_file",
                        "input": {"path": "src/droid_file.rs"}
                    }]
                }),
                "src/droid_file.rs",
            ),
        ] {
            let touches = provider_file_touches_from_raw_value(
                provider,
                "provider-session",
                source_format,
                None,
                &raw,
                &event,
                1,
            );
            assert_eq!(
                touches.first().map(|(_, file)| file.path.as_str()),
                Some(expected_path),
                "{provider:?} should extract an explicit tool file path"
            );
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
    fn antigravity_native_history_imports_transcripts_and_preserves_previews() {
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
        let archive = store.export_archive().unwrap();
        assert!(archive.files_touched.iter().any(|file| {
            file.path == "/workspace/demo/README.md" && file.confidence == Confidence::High
        }));
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
        assert!(rendered.contains("ghp_1234567890abcdef"));
        assert!(rendered.contains("/home/example/private.txt"));
        assert!(!rendered.contains("[REDACTED"));
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
    fn native_kilo_imports_opencode_derived_sqlite_fixture_idempotently() {
        let temp = tempdir();
        let fixture = provider_history_fixture("kilo/kilo.db");
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let first = import_kilo_sqlite(
            &fixture,
            &mut store,
            KiloSqliteImportOptions {
                machine_id: "test-machine".into(),
                source_path: Some(fixture.clone()),
                imported_at: DateTime::parse_from_rfc3339("2026-07-04T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                allow_partial_failures: true,
                ..KiloSqliteImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(first.failed, 0, "{:?}", first.failures);
        assert_eq!(first.imported_sessions, 1);
        assert_eq!(first.imported_events, 2);

        let session_id = provider_session_uuid(CaptureProvider::Kilo, "kilo-root");
        let session = store.get_session(session_id).unwrap();
        assert_eq!(session.provider, CaptureProvider::Kilo);
        let events = store.events_for_session(session_id).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].sync.metadata["source_format"].as_str(),
            Some(KILO_SQLITE_SOURCE_FORMAT)
        );
        assert_eq!(
            events[0].payload["body"]["session_message_seq"].as_i64(),
            Some(1)
        );
        assert_eq!(
            events[1].payload["body"]["session_message_seq"].as_i64(),
            Some(2)
        );

        let second = import_kilo_sqlite(
            &fixture,
            &mut store,
            KiloSqliteImportOptions {
                source_path: Some(fixture.clone()),
                allow_partial_failures: true,
                ..KiloSqliteImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(second.failed, 0, "{:?}", second.failures);
        assert_eq!(second.imported_sessions, 0);
        assert_eq!(second.imported_events, 0);
        assert_eq!(second.skipped_sessions, 1);
        assert_eq!(second.skipped_events, 2);
    }

    #[test]
    fn native_hermes_rejects_out_of_range_message_timestamp() {
        let temp = tempdir();
        let fixture = write_hermes_smoke_db(&temp);
        let conn = Connection::open(&fixture).unwrap();
        conn.execute(
            "update messages set timestamp = ?1 where content = 'bad timestamp'",
            [1.0e300_f64],
        )
        .unwrap();
        drop(conn);
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_hermes_sqlite(
            &fixture,
            &mut store,
            HermesSqliteImportOptions {
                allow_partial_failures: true,
                ..HermesSqliteImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 1);
        assert!(summary.failures[0]
            .error
            .contains("Hermes message timestamp"));
        assert_eq!(summary.imported_events, 1);
    }

    #[cfg(unix)]
    #[test]
    fn native_opencode_normalizer_rejects_symlinked_sqlite() {
        use std::os::unix::fs::symlink;

        let temp = tempdir();
        let fixture = write_opencode_smoke_db(&temp, false);
        let link = temp.path().join("linked-opencode.db");
        symlink(&fixture, &link).unwrap();

        let err = normalize_opencode_sqlite(
            &link,
            &ProviderAdapterContext::default(),
            &OPENCODE_SQLITE_DIALECT,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            CaptureError::InvalidProviderTranscriptPath { path, reason }
                if path.ends_with("linked-opencode.db")
                    && reason == "symlinked provider transcript files are rejected"
        ));
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
    fn native_opencode_rejects_negative_session_message_seq() {
        let temp = tempdir();
        let fixture = write_opencode_smoke_db(&temp, false);
        let conn = Connection::open(&fixture).unwrap();
        conn.execute(
            "update session_message set seq = -1 where id = 'msg-user'",
            [],
        )
        .unwrap();
        drop(conn);
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

        assert_eq!(summary.failed, 1);
        assert!(summary.failures[0]
            .error
            .contains("OpenCode session_message seq must be nonnegative"));
        assert_eq!(summary.imported_events, 2);
        let session_id = provider_session_uuid(CaptureProvider::OpenCode, "opencode-root");
        let events = store.events_for_session(session_id).unwrap();
        assert!(events.iter().all(|event| {
            event.payload["body"]["session_message_seq"]
                .as_i64()
                .is_some_and(|seq| seq >= 0)
        }));
    }

    #[test]
    fn native_opencode_rejects_out_of_range_message_timestamp() {
        let temp = tempdir();
        let fixture = write_opencode_smoke_db(&temp, false);
        let conn = Connection::open(&fixture).unwrap();
        let data_without_payload_time = json!({"text": "bad timestamp fallback"}).to_string();
        conn.execute(
            "update session_message set time_created = ?1, data = ?2 where id = 'msg-user'",
            rusqlite::params![i64::MAX, data_without_payload_time],
        )
        .unwrap();
        drop(conn);
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

        assert_eq!(summary.failed, 1);
        assert!(summary.failures[0]
            .error
            .contains("OpenCode session_message time_created"));
        assert_eq!(summary.imported_events, 2);
    }

    #[test]
    fn native_opencode_rejects_oversized_sqlite_text_value() {
        let temp = tempdir();
        let fixture = write_opencode_smoke_db(&temp, false);
        let conn = Connection::open(&fixture).unwrap();
        let oversized_data = format!(
            "{{\"time\":{{\"created\":1782259200000}},\"text\":\"{}\"}}",
            "x".repeat(MAX_PROVIDER_SQLITE_VALUE_BYTES + 1)
        );
        conn.execute(
            "update session_message set data = ?1 where id = 'msg-user'",
            [&oversized_data],
        )
        .unwrap();
        drop(conn);

        let err = import_opencode_sqlite(
            &fixture,
            &mut Store::open(temp.path().join("work.sqlite")).unwrap(),
            OpenCodeSqliteImportOptions::default(),
        )
        .unwrap_err();

        assert!(
            err.to_string().contains("too big"),
            "unexpected error: {err}"
        );
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
    fn openclaw_import_ignores_oversized_session_index_sidecar() {
        let temp = tempdir();
        let root = temp.path().join("openclaw");
        let sessions = root.join("agents/personal-agent/sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(
            sessions.join("sessions.json"),
            vec![b'x'; MAX_OPENCLAW_SESSION_INDEX_BYTES + 1],
        )
        .unwrap();
        fs::write(
            sessions.join("openclaw-oversized-index.jsonl"),
            format!(
                "{}\n{}\n",
                json!({
                    "type": "session",
                    "id": "openclaw-oversized-index",
                    "timestamp": "2026-06-24T12:00:00Z",
                    "cwd": "/workspace"
                }),
                json!({
                    "type": "message",
                    "id": "openclaw-oversized-index-user",
                    "timestamp": "2026-06-24T12:00:01Z",
                    "message": {"role": "user", "content": "oversized sidecar should not block import"}
                })
            ),
        )
        .unwrap();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_openclaw_history(
            &root,
            &mut store,
            OpenClawImportOptions {
                allow_partial_failures: true,
                ..OpenClawImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 0);
        assert_eq!(summary.imported_sessions, 1);
        assert_eq!(summary.imported_events, 1);
        let session_id = provider_session_uuid(
            CaptureProvider::OpenClaw,
            "personal-agent/openclaw-oversized-index",
        );
        let session = store.get_session(session_id).unwrap();
        assert_eq!(
            session.external_session_id.as_deref(),
            Some("personal-agent/openclaw-oversized-index")
        );
    }

    #[test]
    fn native_shelley_imports_sessions_messages_metadata_and_citations() {
        let temp = tempdir();
        let fixture = write_shelley_smoke_db(&temp);
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_shelley_sqlite(
            &fixture,
            &mut store,
            ShelleySqliteImportOptions {
                machine_id: "test-machine".into(),
                source_path: Some(fixture.clone()),
                imported_at: DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                allow_partial_failures: true,
                ..ShelleySqliteImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 0, "{:?}", summary.failures);
        assert_eq!(summary.imported_sessions, 3);
        assert_eq!(summary.imported_events, 4);
        assert_eq!(summary.imported_edges, 1);

        let parent_id = provider_session_uuid(CaptureProvider::Shelley, "shelley-root");
        let child_id = provider_session_uuid(CaptureProvider::Shelley, "shelley-child");
        assert_eq!(
            store.get_session(child_id).unwrap().parent_session_id,
            Some(parent_id)
        );
        assert!(store
            .get_session(parent_id)
            .unwrap()
            .sync
            .metadata
            .to_string()
            .contains("queued oracle"));

        let source = store
            .capture_source_by_external_session(CaptureProvider::Shelley, "shelley-root")
            .unwrap()
            .unwrap();
        assert_eq!(
            source.descriptor.raw_source_path.as_deref(),
            fixture.to_str()
        );
        assert_eq!(source.descriptor.provider, CaptureProvider::Shelley);

        let events = store.events_for_session(parent_id).unwrap();
        assert_eq!(events.len(), 3);
        let agent_event = events
            .iter()
            .find(|event| {
                event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-agent")
            })
            .expect("Shelley agent event imported");
        let tool_result_event = events
            .iter()
            .find(|event| {
                event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-tool-result")
            })
            .expect("Shelley tool-result event imported");
        assert_eq!(agent_event.event_type, EventType::ToolCall);
        assert_eq!(tool_result_event.event_type, EventType::ToolOutput);
        let rendered = serde_json::to_string(&events).unwrap();
        assert!(rendered.contains("shelley search oracle"));
        assert!(rendered.contains("thinking through the search"));
        assert!(rendered.contains("tool call: bash"));
        assert!(rendered.contains("tool output oracle"));
        assert!(rendered.contains("claude-opus-4-7"));
        assert!(rendered.contains("https://api.anthropic.com/v1/messages"));
        let user_event = events
            .iter()
            .find(|event| {
                event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-user")
            })
            .expect("Shelley user event imported");
        assert!(user_event
            .sync
            .metadata
            .to_string()
            .contains("conversation:shelley-root:sequence:1:message:msg-user"));

        let cursor = store
            .get_sync_cursor(
                None,
                "test-machine",
                &provider_cursor_stream(CaptureProvider::Shelley, SHELLEY_SQLITE_SOURCE_FORMAT),
            )
            .unwrap()
            .unwrap();
        assert!(cursor
            .cursor
            .contains("conversation:shelley-root:sequence:3:message:msg-tool-result"));
    }

    #[test]
    fn native_shelley_reimport_is_idempotent() {
        let temp = tempdir();
        let fixture = write_shelley_smoke_db(&temp);
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let first = import_shelley_sqlite(
            &fixture,
            &mut store,
            ShelleySqliteImportOptions {
                allow_partial_failures: true,
                ..ShelleySqliteImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(first.imported_events, 4);

        let second = import_shelley_sqlite(
            &fixture,
            &mut store,
            ShelleySqliteImportOptions {
                allow_partial_failures: true,
                ..ShelleySqliteImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(second.failed, 0, "{:?}", second.failures);
        assert_eq!(second.imported_sessions, 0);
        assert_eq!(second.imported_events, 0);
        assert_eq!(second.imported_edges, 0);
        assert_eq!(second.skipped_sessions, 3);
        assert_eq!(second.skipped_events, 4);
        assert_eq!(second.skipped_edges, 1);
    }

    #[test]
    fn native_shelley_handles_duplicate_sequences_and_nonchat_rows() {
        let temp = tempdir();
        let fixture = write_shelley_adversarial_db(&temp);
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_shelley_sqlite(
            &fixture,
            &mut store,
            ShelleySqliteImportOptions {
                allow_partial_failures: true,
                ..ShelleySqliteImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 0, "{:?}", summary.failures);
        assert_eq!(summary.imported_sessions, 1);
        assert_eq!(summary.imported_events, 5);

        let session_id = provider_session_uuid(CaptureProvider::Shelley, "shelley-adversarial");
        let events = store.events_for_session(session_id).unwrap();
        assert_eq!(events.len(), 5);
        assert_eq!(
            events
                .iter()
                .map(|event| event.id)
                .collect::<BTreeSet<_>>()
                .len(),
            5
        );
        let rendered = serde_json::to_string(&events).unwrap();
        assert!(rendered.contains("duplicate sequence first"));
        assert!(rendered.contains("duplicate sequence second"));
        assert!(events
            .iter()
            .any(|event| event.event_type == EventType::VcsChange));
        assert!(events
            .iter()
            .any(
                |event| event.sync.metadata["metadata"]["message_type"].as_str() == Some("warning")
            ));

        let large = events
            .iter()
            .find(|event| {
                event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-large")
            })
            .expect("large Shelley event imported");
        assert_eq!(large.payload["body"]["truncated"].as_bool(), Some(true));
        assert!(
            large.payload["body"]["text"]
                .as_str()
                .unwrap()
                .chars()
                .count()
                <= PROVIDER_MAX_TEXT_CHARS
        );
    }

    #[test]
    fn native_shelley_text_extraction_is_not_duplicate_or_unbounded() {
        let text = shelley_value_text(&json!({
            "Content": [
                {"Type": 2, "Text": "once"}
            ]
        }))
        .unwrap();
        assert_eq!(text, "once");

        let huge = "x".repeat(PROVIDER_MAX_TEXT_CHARS + 200);
        let text = shelley_value_text(&json!({
            "Content": [
                {"Type": 2, "Text": huge},
                {"Type": 2, "Text": "after cap"}
            ]
        }))
        .unwrap();
        assert_eq!(text.chars().count(), PROVIDER_MAX_TEXT_CHARS + 1);
        assert!(!text.contains("after cap"));
    }

    #[test]
    fn native_shelley_event_index_uses_stable_message_identity() {
        let message = ShelleyMessageRow {
            rowid: 1,
            message_id: "msg-stable".to_owned(),
            conversation_id: "conv-stable".to_owned(),
            sequence_id: 42,
            entry_type: "user".to_owned(),
            llm_data: None,
            user_data: None,
            usage_data: None,
            created_at: None,
            display_data: None,
            excluded_from_context: false,
            generation: None,
            llm_api_url: None,
            model_name: None,
            forked_from_message_id: None,
        };
        let mut moved_row = message.clone();
        moved_row.rowid = 999;
        let mut duplicate_sequence = message.clone();
        duplicate_sequence.message_id = "msg-stable-other".to_owned();

        assert_eq!(
            shelley_event_index(&message),
            shelley_event_index(&moved_row)
        );
        assert_ne!(
            shelley_event_index(&message),
            shelley_event_index(&duplicate_sequence)
        );
    }

    #[test]
    fn native_shelley_reports_malformed_and_corrupt_db() {
        let temp = tempdir();
        let malformed = write_shelley_malformed_db(&temp);
        let corrupt = temp.path().join("corrupt-shelley.db");
        fs::write(&corrupt, b"not sqlite").unwrap();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let err = import_shelley_sqlite(
            &malformed,
            &mut store,
            ShelleySqliteImportOptions::default(),
        )
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("Shelley messages table missing required column(s): type"));

        let err =
            import_shelley_sqlite(&corrupt, &mut store, ShelleySqliteImportOptions::default())
                .unwrap_err();
        assert!(err.to_string().contains("not a database"));
    }

    #[test]
    fn provider_sources_discovers_shelley_default_db() {
        let temp = tempdir();
        let db = temp.path().join(".config/shelley/shelley.db");
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        fs::write(&db, b"not inspected by source probe").unwrap();

        let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Shelley);
        let source = sources
            .iter()
            .find(|source| source.source_format == SHELLEY_SQLITE_SOURCE_FORMAT)
            .unwrap_or_else(|| panic!("missing Shelley source in {sources:#?}"));
        assert_eq!(source.provider, CaptureProvider::Shelley);
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.import_support, ProviderImportSupport::Native);
        assert_eq!(source.path, db);
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
    fn native_jsonl_tree_skips_headerless_native_files() {
        let temp = tempdir();
        let root = temp.path().join("gemini/.gemini/tmp/project/chats");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("headerless.jsonl"),
            "{\"type\":\"user\",\"content\":\"missing session header\"}\n",
        )
        .unwrap();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_gemini_cli_history(
            temp.path().join("gemini/.gemini"),
            &mut store,
            GeminiCliImportOptions {
                allow_partial_failures: true,
                ..GeminiCliImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 1);
        assert_eq!(summary.imported_events, 0);
        assert!(summary.failures[0]
            .error
            .contains("no importable native JSONL session header"));
    }

    #[test]
    fn native_jsonl_tree_tolerates_unimportable_siblings_for_shared_providers() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let gemini = write_gemini_smoke_fixture(&temp);
        write_unimportable_jsonl_siblings(
            &temp.path().join("gemini/.gemini/tmp/project/chats"),
            "gemini",
        );
        let gemini_summary = import_gemini_cli_history(
            &gemini,
            &mut store,
            GeminiCliImportOptions {
                allow_partial_failures: true,
                ..GeminiCliImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(gemini_summary.failed, 2, "{:?}", gemini_summary.failures);
        assert_eq!(gemini_summary.imported_sessions, 2);
        assert_eq!(gemini_summary.imported_events, 5);
        assert_provider_failures_include_headerless_and_malformed(&gemini_summary);

        let droid = write_droid_smoke_fixture(&temp);
        write_unimportable_jsonl_siblings(&temp.path().join("droid/sessions/project"), "droid");
        let droid_summary = import_factory_ai_droid_sessions(
            &droid,
            &mut store,
            FactoryAiDroidImportOptions {
                allow_partial_failures: true,
                ..FactoryAiDroidImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(droid_summary.failed, 2, "{:?}", droid_summary.failures);
        assert_eq!(droid_summary.imported_sessions, 2);
        assert_eq!(droid_summary.imported_events, 5);
        assert_provider_failures_include_headerless_and_malformed(&droid_summary);

        let copilot = write_copilot_smoke_fixture(&temp);
        write_unimportable_copilot_siblings(&temp.path().join("copilot/session-state"));
        let copilot_summary = import_copilot_cli_session_events(
            &copilot,
            &mut store,
            CopilotCliImportOptions {
                allow_partial_failures: true,
                ..CopilotCliImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(copilot_summary.failed, 2, "{:?}", copilot_summary.failures);
        assert_eq!(copilot_summary.imported_sessions, 1);
        assert_eq!(copilot_summary.imported_events, 5);
        assert_provider_failures_include_headerless_and_malformed(&copilot_summary);
    }

    fn write_unimportable_jsonl_siblings(root: &Path, prefix: &str) {
        fs::write(root.join(format!("{prefix}-empty.jsonl")), "").unwrap();
        fs::write(
            root.join(format!("{prefix}-malformed.jsonl")),
            "{\"not valid\"\n",
        )
        .unwrap();
        fs::write(
            root.join(format!("{prefix}-headerless.jsonl")),
            "{\"type\":\"message\",\"content\":\"missing session header\"}\n",
        )
        .unwrap();
    }

    fn write_unimportable_copilot_siblings(root: &Path) {
        for (session, content) in [
            ("copilot-empty", ""),
            ("copilot-malformed", "{\"not valid\"\n"),
            (
                "copilot-headerless",
                "{\"type\":\"user.message\",\"data\":{\"content\":\"missing session header\"}}\n",
            ),
        ] {
            let path = root.join(session);
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("events.jsonl"), content).unwrap();
        }
    }

    fn assert_provider_failures_include_headerless_and_malformed(summary: &ProviderImportSummary) {
        assert!(summary.failures.iter().any(|failure| failure
            .error
            .contains("no importable native JSONL session header")));
        assert!(summary
            .failures
            .iter()
            .any(|failure| failure.error.contains("malformed JSONL")));
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

    fn write_hermes_smoke_db(temp: &TempDir) -> PathBuf {
        let path = temp.path().join("hermes-state.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "create table sessions (
                id text primary key,
                source text not null,
                started_at real not null
            );
            create table messages (
                id integer primary key autoincrement,
                session_id text not null,
                role text not null,
                content text,
                timestamp real not null,
                active integer not null default 1,
                compacted integer not null default 0
            );",
        )
        .unwrap();
        conn.execute(
            "insert into sessions values (?1, 'acp', 1782259200.0)",
            ["hermes-root"],
        )
        .unwrap();
        conn.execute(
            "insert into messages (session_id, role, content, timestamp) values (?1, 'user', 'bad timestamp', 1782259201.0)",
            ["hermes-root"],
        )
        .unwrap();
        conn.execute(
            "insert into messages (session_id, role, content, timestamp) values (?1, 'assistant', 'good timestamp', 1782259202.0)",
            ["hermes-root"],
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

    fn write_shelley_smoke_db(temp: &TempDir) -> PathBuf {
        let path = temp.path().join("shelley.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "create table conversations (
                conversation_id text primary key,
                slug text,
                user_initiated boolean not null default true,
                created_at datetime not null default current_timestamp,
                updated_at datetime not null default current_timestamp,
                cwd text,
                archived boolean not null default false,
                parent_conversation_id text,
                model text,
                conversation_options text not null default '{}',
                current_generation integer not null default 1,
                agent_working boolean not null default false,
                tags text not null default '[]',
                is_draft boolean not null default false,
                draft text not null default '',
                queued_messages text not null default '[]'
            );
            create table messages (
                message_id text primary key,
                conversation_id text not null,
                sequence_id integer not null,
                type text not null,
                llm_data text,
                user_data text,
                usage_data text,
                created_at datetime not null default current_timestamp,
                display_data text,
                excluded_from_context boolean not null default false,
                generation integer not null default 1,
                llm_api_url text,
                model_name text,
                forked_from_message_id text
            );",
        )
        .unwrap();
        conn.execute(
            "insert into conversations values (
                'shelley-root', 'root-slug', 1, '2026-06-24 12:00:00',
                '2026-06-24 12:05:00', '/workspace/shelley', 0, null,
                'claude-opus-4-7', ?1, 2, 0, ?2, 0, '', ?3
            )",
            [
                r#"{"thinking_level":"high","subagent_backend":"shelley"}"#,
                r#"["native","ctx"]"#,
                r#"[{"id":"queued-1","llm":{"Content":[{"Type":2,"Text":"queued oracle"}]},"created_at":"2026-06-24T12:00:04Z","model":"claude-opus-4-7"}]"#,
            ],
        )
        .unwrap();
        conn.execute(
            "insert into conversations values (
                'shelley-child', 'child-slug', 0, '2026-06-24 12:01:00',
                '2026-06-24 12:02:00', '/workspace/shelley', 0, 'shelley-root',
                'claude-sonnet-4-5', '{}', 1, 0, '[]', 0, '', '[]'
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "insert into conversations values (
                'shelley-draft', 'old-draft', 1, '2026-06-24 11:00:00',
                '2026-06-24 11:01:00', '/workspace/archive', 1, null,
                null, '{}', 1, 0, '[]', 1, 'draft body', '[]'
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "insert into messages (
                message_id, conversation_id, sequence_id, type, user_data, created_at
            ) values ('msg-user', 'shelley-root', 1, 'user', ?1, '2026-06-24 12:00:01')",
            [json!({
                "Content": [
                    {"Type": 2, "Text": "please run shelley search oracle"}
                ]
            })
            .to_string()],
        )
        .unwrap();
        conn.execute(
            "insert into messages (
                message_id, conversation_id, sequence_id, type, llm_data, usage_data,
                created_at, generation, llm_api_url, model_name
            ) values (
                'msg-agent', 'shelley-root', 2, 'agent', ?1, ?2,
                '2026-06-24 12:00:02', 2, 'https://api.anthropic.com/v1/messages',
                'claude-opus-4-7'
            )",
            [
                json!({
                    "Role": 1,
                    "Content": [
                        {"Type": 3, "Thinking": "thinking through the search"},
                        {"Type": 2, "Text": "I will inspect the source."},
                        {"Type": 5, "ID": "toolu_1", "ToolName": "bash", "ToolInput": {"command": "rg shelley"}}
                    ],
                    "EndOfTurn": false
                })
                .to_string(),
                json!({
                    "input_tokens": 100,
                    "cache_read_input_tokens": 25,
                    "output_tokens": 40,
                    "cost_usd": 0.0123,
                    "model": "claude-opus-4-7",
                    "url": "https://api.anthropic.com/v1/messages"
                })
                .to_string(),
            ],
        )
        .unwrap();
        conn.execute(
            "insert into messages (
                message_id, conversation_id, sequence_id, type, user_data, display_data,
                created_at, forked_from_message_id
            ) values (
                'msg-tool-result', 'shelley-root', 3, 'user', ?1, ?2,
                '2026-06-24 12:00:03', 'source-msg-tool-result'
            )",
            [
                json!({
                    "Role": 0,
                    "Content": [
                        {"Type": 6, "ToolUseID": "toolu_1", "ToolResult": [{"Type": 2, "Text": "tool output oracle"}]}
                    ]
                })
                .to_string(),
                json!({"stdout": "tool output oracle", "exit_code": 0}).to_string(),
            ],
        )
        .unwrap();
        conn.execute(
            "insert into messages (
                message_id, conversation_id, sequence_id, type, llm_data, created_at
            ) values ('msg-child', 'shelley-child', 1, 'agent', ?1, '2026-06-24 12:01:01')",
            [json!({
                "Content": [
                    {"Type": 2, "Text": "subagent result from Shelley"}
                ]
            })
            .to_string()],
        )
        .unwrap();
        path
    }

    fn write_shelley_adversarial_db(temp: &TempDir) -> PathBuf {
        let path = temp.path().join("shelley-adversarial.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "create table conversations (
                conversation_id text primary key,
                slug text,
                user_initiated boolean not null default true,
                created_at datetime not null default current_timestamp,
                updated_at datetime not null default current_timestamp,
                cwd text,
                archived boolean not null default false,
                parent_conversation_id text,
                model text,
                conversation_options text not null default '{}',
                current_generation integer not null default 1,
                agent_working boolean not null default false,
                tags text not null default '[]',
                is_draft boolean not null default false,
                draft text not null default '',
                queued_messages text not null default '[]'
            );
            create table messages (
                message_id text primary key,
                conversation_id text not null,
                sequence_id integer not null,
                type text not null,
                llm_data text,
                user_data text,
                usage_data text,
                created_at datetime not null default current_timestamp,
                display_data text,
                excluded_from_context boolean not null default false,
                generation integer not null default 1,
                llm_api_url text,
                model_name text,
                forked_from_message_id text
            );",
        )
        .unwrap();
        conn.execute(
            "insert into conversations values (
                'shelley-adversarial', 'adversarial', 1, '2026-06-24 12:00:00',
                '2026-06-24 12:05:00', '/workspace/shelley', 0, null,
                'claude-opus-4-7', '{}', 1, 0, '[]', 0, '', '[]'
            )",
            [],
        )
        .unwrap();
        for (message_id, sequence_id, message_type, text) in [
            ("msg-dup-a", 1, "user", "duplicate sequence first"),
            ("msg-dup-b", 1, "user", "duplicate sequence second"),
            ("msg-git", 2, "gitinfo", "commit abc touched shelley.rs"),
            ("msg-warning", 3, "warning", "warning message for Shelley"),
        ] {
            conn.execute(
                "insert into messages (
                    message_id, conversation_id, sequence_id, type, user_data, created_at
                ) values (?1, 'shelley-adversarial', ?2, ?3, ?4, '2026-06-24 12:00:01')",
                rusqlite::params![
                    message_id,
                    sequence_id,
                    message_type,
                    json!({"Content": [{"Type": 2, "Text": text}]}).to_string(),
                ],
            )
            .unwrap();
        }
        conn.execute(
            "insert into messages (
                message_id, conversation_id, sequence_id, type, llm_data, created_at
            ) values ('msg-large', 'shelley-adversarial', 4, 'agent', ?1, '2026-06-24 12:00:04')",
            [json!({
                "Content": [
                    {"Type": 2, "Text": "x".repeat(PROVIDER_MAX_TEXT_CHARS + 200)}
                ]
            })
            .to_string()],
        )
        .unwrap();
        path
    }

    fn write_shelley_malformed_db(temp: &TempDir) -> PathBuf {
        let path = temp.path().join("shelley-malformed.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "create table conversations (conversation_id text primary key);
             create table messages (
                message_id text primary key,
                conversation_id text not null
             );",
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
    fn provider_import_scopes_provenance_by_source_format_and_path() {
        let temp = tempdir();
        let shared_path = temp
            .path()
            .join("shared-source.jsonl")
            .display()
            .to_string();
        assert_provider_source_collision_is_distinct(
            "provider_format_a",
            &shared_path,
            "provider_format_b",
            &shared_path,
        );

        let first_path = temp.path().join("first-source.jsonl").display().to_string();
        let second_path = temp
            .path()
            .join("second-source.jsonl")
            .display()
            .to_string();
        assert_provider_source_collision_is_distinct(
            "provider_format",
            &first_path,
            "provider_format",
            &second_path,
        );
    }

    #[test]
    fn provider_import_reuses_existing_legacy_provider_event_identity() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let provider = CaptureProvider::Claude;
        let provider_session_id = "legacy-provider-session";
        let source_format = "provider_format";
        let raw_source_path = temp
            .path()
            .join("legacy-source.jsonl")
            .display()
            .to_string();
        let occurred_at = DateTime::parse_from_rfc3339("2026-06-23T17:00:01Z")
            .unwrap()
            .with_timezone(&Utc);
        let legacy_source_id = provider_source_uuid(provider, provider_session_id);
        let new_source_id = provider_scoped_source_uuid(
            provider,
            provider_session_id,
            source_format,
            Some(&raw_source_path),
        );
        let session_id = provider_session_uuid(provider, provider_session_id);
        let legacy_event_id = provider_event_uuid(provider, provider_session_id, 0);
        let legacy_touch_id = provider_file_touch_uuid(provider, provider_session_id, 0);
        let event_hash =
            compute_payload_hash(&json!({"text": "same provider event payload"})).unwrap();
        assert_ne!(legacy_source_id, new_source_id);

        store
            .upsert_capture_source(&CaptureSource {
                id: legacy_source_id,
                descriptor: CaptureSourceDescriptor {
                    kind: CaptureSourceKind::ProviderImport,
                    provider,
                    machine_id: "test-machine".to_owned(),
                    process_id: None,
                    cwd: Some("/workspace/example".to_owned()),
                    raw_source_path: None,
                    external_session_id: Some(provider_session_id.to_owned()),
                },
                started_at: occurred_at,
                ended_at: None,
                sync: provider_sync_metadata(Fidelity::Imported, json!({"legacy": true})),
            })
            .unwrap();
        store
            .upsert_session(&Session {
                id: session_id,
                history_record_id: None,
                parent_session_id: None,
                root_session_id: None,
                capture_source_id: Some(legacy_source_id),
                provider,
                external_session_id: Some(provider_session_id.to_owned()),
                external_agent_id: None,
                agent_type: AgentType::Primary,
                role_hint: Some("primary".to_owned()),
                is_primary: true,
                status: SessionStatus::Imported,
                transcript_blob_id: None,
                started_at: occurred_at,
                ended_at: None,
                timestamps: timestamps(occurred_at),
                sync: provider_sync_metadata(Fidelity::Imported, json!({"legacy": true})),
            })
            .unwrap();
        store
            .upsert_event(&Event {
                id: legacy_event_id,
                seq: provider_event_seq(provider, provider_session_id, 0),
                history_record_id: None,
                session_id: Some(session_id),
                run_id: None,
                event_type: EventType::Message,
                role: Some(EventRole::User),
                occurred_at,
                capture_source_id: Some(legacy_source_id),
                payload: json!({"body": {"text": "same provider event payload"}}),
                payload_blob_id: None,
                dedupe_key: Some(Store::provider_event_dedupe_key(
                    provider,
                    provider_session_id,
                    0,
                    &event_hash,
                )),
                redaction_state: RedactionState::LocalPreview,
                sync: provider_sync_metadata(Fidelity::Imported, json!({"legacy": true})),
            })
            .unwrap();
        store
            .upsert_file_touched(&FileTouched {
                id: legacy_touch_id,
                history_record_id: None,
                run_id: None,
                event_id: Some(legacy_event_id),
                vcs_workspace_id: None,
                path: "src/lib.rs".to_owned(),
                change_kind: Some(FileChangeKind::Modified),
                old_path: None,
                line_count_delta: Some(1),
                confidence: Confidence::Explicit,
                timestamps: timestamps(occurred_at),
                source_id: Some(legacy_source_id),
                sync: provider_sync_metadata(Fidelity::Imported, json!({"legacy": true})),
            })
            .unwrap();

        let normalization = ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures: vec![(
                1,
                provider_collision_capture(
                    provider,
                    provider_session_id,
                    source_format,
                    &raw_source_path,
                    occurred_at,
                ),
            )],
            files_touched: vec![(
                1,
                provider_collision_file_touch(
                    provider,
                    provider_session_id,
                    source_format,
                    &raw_source_path,
                    occurred_at,
                ),
            )],
        };

        let summary = import_normalized_provider_captures(
            &mut store,
            normalization,
            NormalizedProviderImportOptions::default(),
        )
        .unwrap();

        assert_eq!(summary.failed, 0, "{:?}", summary.failures);
        assert_eq!(summary.skipped_events, 1);
        let events = store.events_for_session(session_id).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, legacy_event_id);
        assert_eq!(events[0].capture_source_id, Some(legacy_source_id));

        let archive = store.export_archive().unwrap();
        assert_eq!(archive.files_touched.len(), 1);
        assert_eq!(archive.files_touched[0].id, legacy_touch_id);
        assert_eq!(archive.files_touched[0].event_id, Some(legacy_event_id));
        assert_eq!(archive.files_touched[0].source_id, Some(new_source_id));
    }

    #[test]
    fn provider_source_event_seq_keeps_large_provider_indices_distinct() {
        let source_id = Uuid::parse_str("018fe2e4-2266-7000-8000-000000000001").unwrap();

        assert_ne!(
            provider_source_event_seq(source_id, 0),
            provider_source_event_seq(source_id, 1_048_576)
        );
        assert_eq!(
            provider_source_event_seq(source_id, 1_048_576) & 0xffff_ffff,
            1_048_576
        );
    }

    fn assert_provider_source_collision_is_distinct(
        first_source_format: &str,
        first_source_path: &str,
        second_source_format: &str,
        second_source_path: &str,
    ) {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let provider = CaptureProvider::Claude;
        let provider_session_id = "shared-provider-session";
        let occurred_at = DateTime::parse_from_rfc3339("2026-06-23T17:00:01Z")
            .unwrap()
            .with_timezone(&Utc);
        let first_source_id = provider_scoped_source_uuid(
            provider,
            provider_session_id,
            first_source_format,
            Some(first_source_path),
        );
        let second_source_id = provider_scoped_source_uuid(
            provider,
            provider_session_id,
            second_source_format,
            Some(second_source_path),
        );
        assert_ne!(first_source_id, second_source_id);

        let normalization = ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures: vec![
                (
                    1,
                    provider_collision_capture(
                        provider,
                        provider_session_id,
                        first_source_format,
                        first_source_path,
                        occurred_at,
                    ),
                ),
                (
                    2,
                    provider_collision_capture(
                        provider,
                        provider_session_id,
                        second_source_format,
                        second_source_path,
                        occurred_at,
                    ),
                ),
            ],
            files_touched: vec![
                (
                    1,
                    provider_collision_file_touch(
                        provider,
                        provider_session_id,
                        first_source_format,
                        first_source_path,
                        occurred_at,
                    ),
                ),
                (
                    2,
                    provider_collision_file_touch(
                        provider,
                        provider_session_id,
                        second_source_format,
                        second_source_path,
                        occurred_at,
                    ),
                ),
            ],
        };

        let summary = import_normalized_provider_captures(
            &mut store,
            normalization,
            NormalizedProviderImportOptions::default(),
        )
        .unwrap();
        assert_eq!(summary.failed, 0, "{:?}", summary.failures);
        assert_eq!(summary.imported_events, 2);
        assert_eq!(store.capture_source_count().unwrap(), 2);

        let first_source = store.get_capture_source(first_source_id).unwrap();
        let second_source = store.get_capture_source(second_source_id).unwrap();
        assert_eq!(
            first_source.descriptor.raw_source_path.as_deref(),
            Some(first_source_path)
        );
        assert_eq!(
            first_source.sync.metadata["source_format"].as_str(),
            Some(first_source_format)
        );
        assert_eq!(
            second_source.descriptor.raw_source_path.as_deref(),
            Some(second_source_path)
        );
        assert_eq!(
            second_source.sync.metadata["source_format"].as_str(),
            Some(second_source_format)
        );

        let session_id = provider_session_uuid(provider, provider_session_id);
        let event_source_ids = store
            .events_for_session(session_id)
            .unwrap()
            .into_iter()
            .map(|event| event.capture_source_id.unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            event_source_ids,
            BTreeSet::from([first_source_id, second_source_id])
        );

        let archive = store.export_archive().unwrap();
        assert_eq!(archive.files_touched.len(), 2);
        let touched_source_ids = archive
            .files_touched
            .iter()
            .map(|file| file.source_id.unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            touched_source_ids,
            BTreeSet::from([first_source_id, second_source_id])
        );
        for file in archive.files_touched {
            let source_id = file.source_id.unwrap();
            assert_eq!(
                file.event_id,
                Some(provider_source_event_uuid(source_id, 0))
            );
        }
    }

    fn provider_collision_capture(
        provider: CaptureProvider,
        provider_session_id: &str,
        source_format: &str,
        raw_source_path: &str,
        occurred_at: DateTime<Utc>,
    ) -> ProviderCaptureEnvelope {
        ProviderCaptureEnvelope {
            schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
            provider,
            source: ProviderSourceEnvelope {
                source_format: source_format.to_owned(),
                machine_id: "test-machine".to_owned(),
                observed_at: occurred_at,
                raw_source_path: Some(raw_source_path.to_owned()),
                raw_retention: ProviderRawRetention::PathReference,
                redaction_boundary: ProviderRedactionBoundary::BeforeExport,
                trust: ProviderSourceTrust::ProviderExport,
                fidelity: Fidelity::Imported,
                cursor: None,
                idempotency_key: Some(format!(
                    "provider-source:{}:{}:{}",
                    provider.as_str(),
                    source_format,
                    provider_session_id
                )),
                metadata: json!({}),
            },
            session: ProviderSessionEnvelope {
                provider_session_id: provider_session_id.to_owned(),
                parent_provider_session_id: None,
                root_provider_session_id: None,
                external_agent_id: None,
                agent_type: AgentType::Primary,
                role_hint: Some("primary".to_owned()),
                is_primary: true,
                status: SessionStatus::Imported,
                started_at: occurred_at,
                ended_at: None,
                cwd: Some("/workspace/example".to_owned()),
                fidelity: Fidelity::Imported,
                idempotency_key: Some(format!(
                    "provider-session:{}:{}",
                    provider.as_str(),
                    provider_session_id
                )),
                artifacts: Vec::new(),
                metadata: json!({}),
            },
            event: Some(ProviderEventEnvelope {
                provider_event_index: 0,
                provider_event_hash: None,
                cursor: None,
                event_type: EventType::Message,
                role: Some(EventRole::User),
                occurred_at,
                fidelity: Fidelity::Imported,
                redaction_state: RedactionState::LocalPreview,
                idempotency_key: Some(format!(
                    "provider-event:{}:{}:0",
                    provider.as_str(),
                    provider_session_id
                )),
                artifacts: Vec::new(),
                payload: json!({"text": "same provider event payload"}),
                metadata: json!({}),
            }),
        }
    }

    fn provider_collision_file_touch(
        provider: CaptureProvider,
        provider_session_id: &str,
        source_format: &str,
        raw_source_path: &str,
        occurred_at: DateTime<Utc>,
    ) -> ProviderFileTouchedEnvelope {
        ProviderFileTouchedEnvelope {
            provider,
            provider_session_id: provider_session_id.to_owned(),
            provider_touch_index: 0,
            provider_event_index: Some(0),
            raw_source_path: Some(raw_source_path.to_owned()),
            path: "src/lib.rs".to_owned(),
            change_kind: Some(FileChangeKind::Modified),
            old_path: None,
            line_count_delta: Some(1),
            confidence: Confidence::Explicit,
            occurred_at,
            source_format: source_format.to_owned(),
            metadata: json!({}),
        }
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
    fn custom_history_jsonl_imports_full_shape_and_is_idempotent() {
        let temp = tempdir();
        let fixture = custom_history_fixture("basic.jsonl");
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let first = import_custom_history_jsonl_v1(
            &fixture,
            &mut store,
            CustomHistoryJsonlV1ImportOptions {
                source_path: Some(fixture.clone()),
                imported_at: "2026-06-23T12:10:00Z".parse().unwrap(),
                ..CustomHistoryJsonlV1ImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(first.failed, 0, "{:?}", first.failures);
        assert_eq!(first.imported_sessions, 2);
        assert_eq!(first.imported_events, 2);
        assert_eq!(first.imported_edges, 2);

        let root_provider_session_id =
            custom_history_internal_session_id("demo-agent", "demo-source", "demo-session");
        let child_provider_session_id =
            custom_history_internal_session_id("demo-agent", "demo-source", "demo-session-worker");
        let root_id = provider_session_uuid(CaptureProvider::Custom, &root_provider_session_id);
        let child_id = provider_session_uuid(CaptureProvider::Custom, &child_provider_session_id);
        let root = store.get_session(root_id).unwrap();
        let child = store.get_session(child_id).unwrap();
        assert_eq!(root.provider, CaptureProvider::Custom);
        assert_eq!(child.parent_session_id, Some(root_id));
        assert!(root
            .sync
            .metadata
            .to_string()
            .contains("\"provider_key\":\"demo-agent\""));
        let events = store.events_for_session(root_id).unwrap();
        assert_eq!(events.len(), 2);
        assert!(events[0].payload.to_string().contains("Add a parser test."));

        let conn = rusqlite::Connection::open(temp.path().join("work.sqlite")).unwrap();
        let touched: i64 = conn
            .query_row("SELECT COUNT(*) FROM files_touched", [], |row| row.get(0))
            .unwrap();
        assert_eq!(touched, 1);
        let spawned_edges: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_edges WHERE edge_type = 'spawned'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(spawned_edges, 1);
        let cursor_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_cursors WHERE stream LIKE 'provider:custom:demo-agent:%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cursor_count, 1);
        let cursor: String = conn
            .query_row(
                "SELECT cursor FROM sync_cursors WHERE stream LIKE 'provider:custom:demo-agent:%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cursor, "5");
        let raw_cursor_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_cursors WHERE stream = 'demo-agent:demo-source'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(raw_cursor_count, 0);
        drop(conn);

        let second = import_custom_history_jsonl_v1(
            &fixture,
            &mut store,
            CustomHistoryJsonlV1ImportOptions {
                source_path: Some(fixture.clone()),
                imported_at: "2026-06-23T12:10:00Z".parse().unwrap(),
                ..CustomHistoryJsonlV1ImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(second.failed, 0);
        assert_eq!(second.imported_sessions, 0);
        assert_eq!(second.imported_events, 0);
        assert_eq!(second.imported_edges, 0);
        assert_eq!(second.skipped_events, 2);
        assert_eq!(second.skipped_edges, 2);
    }

    #[test]
    fn custom_history_jsonl_reader_import_persists_normalized_cursor() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let input = [
            r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}"#,
            r#"{"record_type":"source","source_id":"src","provider_key":"stream-agent","source_format":"stream-v1","cursor":{"after":{"stream":"native-stream","cursor":"{\"message_id\":7}","observed_at":"2026-07-01T12:00:00Z"}}}"#,
            r#"{"record_type":"session","source_id":"src","session_id":"run","started_at":"2026-07-01T11:59:00Z"}"#,
            r#"{"record_type":"event","source_id":"src","session_id":"run","event_index":0,"event_type":"message","role":"assistant","occurred_at":"2026-07-01T12:00:00Z","preview":"stream import marker"}"#,
        ]
        .join("\n");

        let summary = import_custom_history_jsonl_v1_reader(
            std::io::Cursor::new(input.into_bytes()),
            &mut store,
            CustomHistoryJsonlV1ImportOptions {
                source_path: Some(PathBuf::from("plugin://stream-agent/default")),
                imported_at: "2026-07-01T12:01:00Z".parse().unwrap(),
                ..CustomHistoryJsonlV1ImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 0, "{:?}", summary.failures);
        assert_eq!(summary.imported_sessions, 1);
        assert_eq!(summary.imported_events, 1);
        let cursor = store
            .get_sync_cursor(
                None,
                &CustomHistoryJsonlV1ImportOptions::default().machine_id,
                &custom_history_jsonl_v1_cursor_stream("stream-agent", "src", "stream-v1"),
            )
            .unwrap()
            .unwrap();
        assert_eq!(cursor.cursor, r#"{"message_id":7}"#);
    }

    #[test]
    fn custom_history_jsonl_reader_persists_source_only_cursor() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let input = [
            r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}"#,
            r#"{"record_type":"source","source_id":"src","provider_key":"stream-agent","source_format":"stream-v1","cursor":{"after":{"stream":"native-stream","cursor":"{\"message_id\":9}","observed_at":"2026-07-01T12:02:00Z"}}}"#,
        ]
        .join("\n");

        let summary = import_custom_history_jsonl_v1_reader(
            std::io::Cursor::new(input.into_bytes()),
            &mut store,
            CustomHistoryJsonlV1ImportOptions {
                imported_at: "2026-07-01T12:03:00Z".parse().unwrap(),
                ..CustomHistoryJsonlV1ImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 0, "{:?}", summary.failures);
        assert_eq!(summary.imported_sessions, 0);
        assert_eq!(summary.imported_events, 0);
        let cursor = store
            .get_sync_cursor(
                None,
                &CustomHistoryJsonlV1ImportOptions::default().machine_id,
                &custom_history_jsonl_v1_cursor_stream("stream-agent", "src", "stream-v1"),
            )
            .unwrap()
            .unwrap();
        assert_eq!(cursor.cursor, r#"{"message_id":9}"#);
    }

    #[test]
    fn custom_history_jsonl_malformed_import_is_atomic_by_default() {
        let temp = tempdir();
        let fixture = custom_history_fixture("malformed-partial.jsonl");
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_custom_history_jsonl_v1(
            &fixture,
            &mut store,
            CustomHistoryJsonlV1ImportOptions {
                source_path: Some(fixture.clone()),
                imported_at: "2026-06-23T13:10:00Z".parse().unwrap(),
                ..CustomHistoryJsonlV1ImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.imported_sessions, 0);
        assert_eq!(summary.imported_events, 0);
        assert_eq!(summary.failed, 1);
        assert_eq!(store.capture_source_count().unwrap(), 0);
        let conn = rusqlite::Connection::open(temp.path().join("work.sqlite")).unwrap();
        let sessions: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .unwrap();
        let events: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(sessions, 0);
        assert_eq!(events, 0);
    }

    #[test]
    fn custom_history_jsonl_rejects_oversized_line() {
        let temp = tempdir();
        let path = temp.path().join("oversized-custom.jsonl");
        write_oversized_jsonl_line(&path);
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let err = import_custom_history_jsonl_v1(
            &path,
            &mut store,
            CustomHistoryJsonlV1ImportOptions::default(),
        )
        .unwrap_err();

        assert!(err.to_string().contains("provider JSONL line exceeds"));
        assert_eq!(store.capture_source_count().unwrap(), 0);
    }

    #[test]
    fn custom_history_jsonl_preview_overrides_raw_payload_for_searchable_event_payload() {
        let temp = tempdir();
        let fixture = temp.path().join("preview-overrides-payload.jsonl");
        fs::write(
            &fixture,
            [
                r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}"#,
                r#"{"record_type":"source","source_id":"src","provider_key":"preview-agent","source_format":"demo"}"#,
                r#"{"record_type":"session","source_id":"src","session_id":"run","started_at":"2026-06-23T14:00:00Z"}"#,
                r#"{"record_type":"event","source_id":"src","session_id":"run","event_index":0,"event_type":"message","role":"assistant","occurred_at":"2026-06-23T14:00:01Z","payload":{"raw":"unindexed-raw-payload-token"},"preview":"bounded searchable preview text"}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_custom_history_jsonl_v1(
            &fixture,
            &mut store,
            CustomHistoryJsonlV1ImportOptions {
                source_path: Some(fixture.clone()),
                imported_at: "2026-06-23T14:10:00Z".parse().unwrap(),
                ..CustomHistoryJsonlV1ImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 0, "{:?}", summary.failures);
        let session_id = provider_session_uuid(
            CaptureProvider::Custom,
            &custom_history_internal_session_id("preview-agent", "src", "run"),
        );
        let events = store.events_for_session(session_id).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].payload["body"],
            json!({ "text": "bounded searchable preview text" })
        );
        assert!(!events[0]
            .payload
            .to_string()
            .contains("unindexed-raw-payload-token"));
        assert_eq!(
            events[0].sync.metadata["metadata"]["ctx_history_jsonl_v1"]["raw_payload"]["raw"]
                .as_str(),
            Some("unindexed-raw-payload-token")
        );
    }

    #[test]
    fn custom_history_jsonl_namespaces_provider_keys_to_avoid_collisions() {
        let temp = tempdir();
        let fixture = temp.path().join("same-native-ids.jsonl");
        fs::write(
            &fixture,
            [
                r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}"#,
                r#"{"record_type":"source","source_id":"src","provider_key":"alpha","source_format":"demo"}"#,
                r#"{"record_type":"session","source_id":"src","session_id":"same","started_at":"2026-06-23T14:00:00Z"}"#,
                r#"{"record_type":"event","source_id":"src","session_id":"same","event_index":0,"event_type":"message","role":"user","occurred_at":"2026-06-23T14:00:01Z","payload":{"text":"alpha text"}}"#,
                r#"{"record_type":"source","source_id":"src-2","provider_key":"beta","source_format":"demo"}"#,
                r#"{"record_type":"session","source_id":"src-2","session_id":"same","started_at":"2026-06-23T14:01:00Z"}"#,
                r#"{"record_type":"event","source_id":"src-2","session_id":"same","event_index":0,"event_type":"message","role":"user","occurred_at":"2026-06-23T14:01:01Z","payload":{"text":"beta text"}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_custom_history_jsonl_v1(
            &fixture,
            &mut store,
            CustomHistoryJsonlV1ImportOptions {
                source_path: Some(fixture.clone()),
                imported_at: "2026-06-23T14:10:00Z".parse().unwrap(),
                ..CustomHistoryJsonlV1ImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 0, "{:?}", summary.failures);
        assert_eq!(summary.imported_sessions, 2);
        assert_eq!(summary.imported_events, 2);
        let alpha_session = provider_session_uuid(
            CaptureProvider::Custom,
            &custom_history_internal_session_id("alpha", "src", "same"),
        );
        let beta_session = provider_session_uuid(
            CaptureProvider::Custom,
            &custom_history_internal_session_id("beta", "src-2", "same"),
        );
        assert_ne!(alpha_session, beta_session);
        assert!(store
            .events_for_session(alpha_session)
            .unwrap()
            .iter()
            .any(|event| event.payload.to_string().contains("alpha text")));
        assert!(store
            .events_for_session(beta_session)
            .unwrap()
            .iter()
            .any(|event| event.payload.to_string().contains("beta text")));
    }

    #[test]
    fn custom_history_jsonl_hashes_delimited_identifiers_without_collisions() {
        let temp = tempdir();
        let fixture = temp.path().join("delimited-identifiers.jsonl");
        fs::write(
            &fixture,
            [
                r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}"#,
                r#"{"record_type":"source","source_id":"a:b","provider_key":"delim-agent","source_format":"demo"}"#,
                r#"{"record_type":"session","source_id":"a:b","session_id":"c","started_at":"2026-06-23T14:00:00Z"}"#,
                r#"{"record_type":"event","source_id":"a:b","session_id":"c","event_index":0,"event_type":"message","role":"user","occurred_at":"2026-06-23T14:00:01Z","payload":{"text":"left text"}}"#,
                r#"{"record_type":"source","source_id":"a","provider_key":"delim-agent","source_format":"demo"}"#,
                r#"{"record_type":"session","source_id":"a","session_id":"b:c","started_at":"2026-06-23T14:01:00Z"}"#,
                r#"{"record_type":"event","source_id":"a","session_id":"b:c","event_index":0,"event_type":"message","role":"user","occurred_at":"2026-06-23T14:01:01Z","payload":{"text":"right text"}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_custom_history_jsonl_v1(
            &fixture,
            &mut store,
            CustomHistoryJsonlV1ImportOptions {
                source_path: Some(fixture.clone()),
                imported_at: "2026-06-23T14:10:00Z".parse().unwrap(),
                ..CustomHistoryJsonlV1ImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 0, "{:?}", summary.failures);
        assert_eq!(summary.imported_sessions, 2);
        assert_eq!(summary.imported_events, 2);
        let left_session = provider_session_uuid(
            CaptureProvider::Custom,
            &custom_history_internal_session_id("delim-agent", "a:b", "c"),
        );
        let right_session = provider_session_uuid(
            CaptureProvider::Custom,
            &custom_history_internal_session_id("delim-agent", "a", "b:c"),
        );
        assert_ne!(left_session, right_session);
        assert!(store
            .events_for_session(left_session)
            .unwrap()
            .iter()
            .any(|event| event.payload.to_string().contains("left text")));
        assert!(store
            .events_for_session(right_session)
            .unwrap()
            .iter()
            .any(|event| event.payload.to_string().contains("right text")));
    }

    #[test]
    fn custom_history_jsonl_dedupes_explicit_parent_child_edge_from_session_parent() {
        let temp = tempdir();
        let fixture = temp.path().join("duplicate-parent-child.jsonl");
        fs::write(
            &fixture,
            [
                r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}"#,
                r#"{"record_type":"source","source_id":"src","provider_key":"edge-agent","source_format":"demo"}"#,
                r#"{"record_type":"session","source_id":"src","session_id":"root","started_at":"2026-06-23T15:00:00Z"}"#,
                r#"{"record_type":"session","source_id":"src","session_id":"child","parent_session_id":"root","started_at":"2026-06-23T15:00:01Z"}"#,
                r#"{"record_type":"edge","source_id":"src","from_session_id":"root","to_session_id":"child","edge_type":"parent_child","edge_id":"explicit-parent","occurred_at":"2026-06-23T15:00:02Z"}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let summary = import_custom_history_jsonl_v1(
            &fixture,
            &mut store,
            CustomHistoryJsonlV1ImportOptions {
                source_path: Some(fixture.clone()),
                imported_at: "2026-06-23T15:10:00Z".parse().unwrap(),
                ..CustomHistoryJsonlV1ImportOptions::default()
            },
        )
        .unwrap();

        assert_eq!(summary.failed, 0, "{:?}", summary.failures);
        assert_eq!(summary.imported_edges, 1);
        assert_eq!(summary.skipped_edges, 1);
        let conn = rusqlite::Connection::open(temp.path().join("work.sqlite")).unwrap();
        let parent_child_edges: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_edges WHERE edge_type = 'parent_child'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(parent_child_edges, 1);
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
