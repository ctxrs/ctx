use std::{
    collections::{BTreeSet, HashMap},
    ffi::CString,
    fs,
    os::raw::c_char,
    path::{Path, PathBuf},
    ptr,
    str::FromStr,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    new_id, utc_now, AgentType, Artifact, ArtifactKind, CaptureProvider, CaptureSource,
    CaptureSourceDescriptor, EntityTimestamps, Event, EventRole, EventType, Fidelity, FileTouched,
    HistoryRecord, HistoryRecordLink, RedactionState, Run, RunStatus, RunType, Session,
    SessionEdge, SessionHistoryArchive, SessionStatus, Summary, SyncCursor, SyncMetadata,
    SyncState, VcsChange, VcsWorkspace, Visibility,
};
use rusqlite::{
    ffi, limits::Limit, params, types::ValueRef, Connection, ErrorCode, OpenFlags,
    OptionalExtension, Transaction,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("time parse error: {0}")]
    Time(#[from] chrono::ParseError),
    #[error("uuid parse error: {0}")]
    Uuid(#[from] uuid::Error),
    #[error("record not found: {0}")]
    NotFound(Uuid),
    #[error("unsupported history store schema version: {0}")]
    UnsupportedSchemaVersion(i64),
    #[error("unsupported session history archive version: {0}")]
    UnsupportedArchiveVersion(u32),
    #[error("archive conflicts with existing {kind}: {id}")]
    ImportConflict { kind: &'static str, id: Uuid },
    #[error("archive artifact {id} content does not match its blob hash")]
    ArchiveArtifactHashMismatch { id: Uuid },
    #[error("unsafe blob path in local store: {0}")]
    UnsafeBlobPath(String),
    #[error("archive artifact {id} content byte size does not match archive metadata")]
    ArchiveArtifactSizeMismatch { id: Uuid },
    #[error("archive artifact {id} blob path is not canonical for its content hash")]
    ArchiveArtifactPathMismatch { id: Uuid },
    #[error("archive artifact {id} blob file is not a regular file: {path:?}")]
    ArchiveArtifactNonRegularFile { id: Uuid, path: PathBuf },
    #[error("archive artifact {id} is missing matching blob content")]
    ArchiveArtifactMissingContent { id: Uuid },
    #[error("provider event conflict for {provider}/{external_session_id} at index {provider_index}: existing hash {existing_hash}, new hash {new_hash}")]
    ProviderEventConflict {
        provider: String,
        external_session_id: String,
        provider_index: u64,
        existing_hash: String,
        new_hash: String,
    },
    #[error("SQL query is empty")]
    RawSqlEmpty,
    #[error("SQL query contains an interior NUL byte")]
    RawSqlInteriorNul,
    #[error("SQL query must be read-only")]
    RawSqlNotReadOnly,
    #[error("SQL query parameters are not supported")]
    RawSqlHasParameters,
    #[error("SQL query must return at least one column")]
    RawSqlNoColumns,
    #[error("SQL query returned {columns} columns; maximum is {max_columns}")]
    RawSqlTooManyColumns { columns: usize, max_columns: usize },
    #[error("{field} must be between {min} and {max}, got {value}")]
    RawSqlLimitOutOfRange {
        field: &'static str,
        value: usize,
        min: usize,
        max: usize,
    },
    #[error("SQL result preview budget {estimated_bytes} bytes exceeds maximum {max_result_bytes}; lower max_rows, max_columns, or max_value_bytes")]
    RawSqlResultBudgetTooLarge {
        estimated_bytes: usize,
        max_result_bytes: usize,
    },
    #[error("SQL query timed out after {timeout_ms}ms")]
    RawSqlTimedOut { timeout_ms: u64 },
}

pub type Result<T> = std::result::Result<T, StoreError>;

const SCHEMA_VERSION: i64 = 31;
const BUSY_TIMEOUT: Duration = Duration::from_millis(30_000);
const OBJECTS_DIR: &str = "objects";
const SPOOL_DIR: &str = "spool";
const LEGACY_HISTORY_DIR_NAME: &str = "work-record";
const LEGACY_BLOBS_DIR: &str = "blobs";
const LEGACY_INBOX_DIR: &str = "inbox";
pub const RAW_SQL_DEFAULT_MAX_ROWS: usize = 100;
pub const RAW_SQL_MAX_ROWS_CAP: usize = 10_000;
pub const RAW_SQL_DEFAULT_MAX_COLUMNS: usize = 64;
pub const RAW_SQL_MAX_COLUMNS_CAP: usize = 256;
pub const RAW_SQL_DEFAULT_MAX_VALUE_BYTES: usize = 512;
pub const RAW_SQL_MAX_VALUE_BYTES_CAP: usize = 1_048_576;
pub const RAW_SQL_MAX_RESULT_PREVIEW_BYTES: usize = 64 * 1024 * 1024;
pub const RAW_SQL_MAX_RESULT_CELLS: usize = 262_144;
const RAW_SQL_MIN_SQLITE_LENGTH_LIMIT_BYTES: usize = 64 * 1024;
const RAW_SQL_VALUE_LENGTH_MARGIN_BYTES: usize = 1024;
pub const RAW_SQL_DEFAULT_MAX_SQL_BYTES: usize = 64 * 1024;
pub const RAW_SQL_MAX_SQL_BYTES_CAP: usize = 1_048_576;
pub const RAW_SQL_DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
pub const RAW_SQL_MAX_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSqlOptions {
    pub max_rows: usize,
    pub max_columns: usize,
    pub max_value_bytes: usize,
    pub max_sql_bytes: usize,
    pub timeout: Duration,
}

impl Default for RawSqlOptions {
    fn default() -> Self {
        Self {
            max_rows: RAW_SQL_DEFAULT_MAX_ROWS,
            max_columns: RAW_SQL_DEFAULT_MAX_COLUMNS,
            max_value_bytes: RAW_SQL_DEFAULT_MAX_VALUE_BYTES,
            max_sql_bytes: RAW_SQL_DEFAULT_MAX_SQL_BYTES,
            timeout: RAW_SQL_DEFAULT_TIMEOUT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSqlColumn {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RawSqlValue {
    Null,
    Integer(i64),
    Real(f64),
    Text {
        value: String,
        bytes: usize,
        truncated: bool,
    },
    Blob {
        bytes: usize,
        preview_hex: String,
        truncated: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSqlTruncation {
    pub rows: bool,
    pub values: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSqlLimits {
    pub max_rows: usize,
    pub max_columns: usize,
    pub max_value_bytes: usize,
    pub max_sql_bytes: usize,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RawSqlResult {
    pub columns: Vec<RawSqlColumn>,
    pub rows: Vec<Vec<RawSqlValue>>,
    pub returned_rows: usize,
    pub truncated: RawSqlTruncation,
    pub elapsed: Duration,
    pub limits: RawSqlLimits,
}

impl RawSqlValue {
    fn is_truncated(&self) -> bool {
        match self {
            Self::Text { truncated, .. } | Self::Blob { truncated, .. } => *truncated,
            Self::Null | Self::Integer(_) | Self::Real(_) => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalDeviceIdentity {
    pub id: Uuid,
    pub stable_device_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalWorkspaceIdentity {
    pub id: Uuid,
    pub device_id: Uuid,
    pub vcs_workspace_id: Option<Uuid>,
    pub repo_fingerprint: String,
    pub root_path_hash: String,
    pub display_root: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogSession {
    pub provider: CaptureProvider,
    pub source_format: String,
    pub source_root: String,
    pub source_path: String,
    pub external_session_id: Option<String>,
    pub parent_external_session_id: Option<String>,
    pub agent_type: AgentType,
    pub role_hint: Option<String>,
    pub external_agent_id: Option<String>,
    pub cwd: Option<String>,
    pub session_started_at_ms: Option<i64>,
    pub file_size_bytes: u64,
    pub file_modified_at_ms: i64,
    pub cataloged_at_ms: i64,
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogSourceIndexUpdate<'a> {
    pub source_root: &'a str,
    pub source_path: &'a str,
    pub file_size_bytes: u64,
    pub file_modified_at_ms: i64,
    pub file_sha256: Option<&'a str>,
    pub event_count: Option<u64>,
    pub indexed_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSourceIndexState {
    pub last_imported_file_size_bytes: Option<u64>,
    pub last_imported_file_modified_at_ms: Option<i64>,
    pub last_imported_event_count: Option<u64>,
    pub last_imported_at_ms: Option<i64>,
    pub last_imported_file_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceImportFile {
    pub provider: CaptureProvider,
    pub source_format: String,
    pub source_root: String,
    pub source_path: String,
    pub file_size_bytes: u64,
    pub file_modified_at_ms: i64,
    pub observed_at_ms: i64,
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceImportFileIndexUpdate<'a> {
    pub source_root: &'a str,
    pub source_path: &'a str,
    pub file_size_bytes: u64,
    pub file_modified_at_ms: i64,
    pub indexed_at_ms: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CatalogCounts {
    pub total: usize,
    pub indexed: usize,
    pub stale: usize,
    pub pending: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndexedHistoryCounts {
    pub sessions: usize,
    pub events: usize,
}

impl IndexedHistoryCounts {
    pub fn items(self) -> usize {
        self.sessions.saturating_add(self.events)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogIndexedStatus {
    Pending,
    Indexed,
    Failed,
}

impl CatalogIndexedStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Indexed => "indexed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventSearchHit {
    pub event_id: Uuid,
    pub history_record_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub session_parent_session_id: Option<Uuid>,
    pub session_root_session_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub seq: u64,
    pub event_type: EventType,
    pub role: Option<EventRole>,
    pub occurred_at: DateTime<Utc>,
    pub preview: String,
    pub score: f64,
    pub provider: Option<CaptureProvider>,
    pub session_external_session_id: Option<String>,
    pub history_source: Option<String>,
    pub history_source_plugin: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
    pub agent_type: Option<AgentType>,
    pub session_is_primary: Option<bool>,
    pub cwd: Option<String>,
    pub raw_source_path: Option<String>,
    pub cursor: Option<String>,
    pub record_title: Option<String>,
    pub record_kind: Option<String>,
    pub record_workspace: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileTouchScope {
    pub history_record_ids: BTreeSet<Uuid>,
    pub session_ids: BTreeSet<Uuid>,
    pub run_ids: BTreeSet<Uuid>,
    pub event_ids: BTreeSet<Uuid>,
    pub source_ids: BTreeSet<Uuid>,
}

impl FileTouchScope {
    pub fn is_empty(&self) -> bool {
        self.history_record_ids.is_empty()
            && self.session_ids.is_empty()
            && self.run_ids.is_empty()
            && self.event_ids.is_empty()
            && self.source_ids.is_empty()
    }
}

const HISTORY_RECORD_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec {
        name: "summary",
        definition: "summary TEXT",
    },
    ColumnSpec {
        name: "status",
        definition: "status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'active', 'completed', 'abandoned', 'archived'))",
    },
    ColumnSpec {
        name: "primary_vcs_workspace_id",
        definition: "primary_vcs_workspace_id TEXT REFERENCES vcs_workspaces(id)",
    },
    ColumnSpec {
        name: "started_at_ms",
        definition: "started_at_ms INTEGER",
    },
    ColumnSpec {
        name: "last_activity_at_ms",
        definition: "last_activity_at_ms INTEGER NOT NULL DEFAULT 0",
    },
    ColumnSpec {
        name: "completed_at_ms",
        definition: "completed_at_ms INTEGER",
    },
    ColumnSpec {
        name: "confidence",
        definition: "confidence TEXT NOT NULL DEFAULT 'unknown' CHECK (confidence IN ('explicit', 'high', 'medium', 'low', 'unknown'))",
    },
    ColumnSpec {
        name: "created_at_ms",
        definition: "created_at_ms INTEGER NOT NULL DEFAULT 0",
    },
    ColumnSpec {
        name: "updated_at_ms",
        definition: "updated_at_ms INTEGER NOT NULL DEFAULT 0",
    },
    ColumnSpec {
        name: "source_id",
        definition: "source_id TEXT REFERENCES capture_sources(id)",
    },
    ColumnSpec {
        name: "visibility",
        definition: "visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full', 'withheld'))",
    },
    ColumnSpec {
        name: "fidelity",
        definition: "fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only'))",
    },
    ColumnSpec {
        name: "sync_state",
        definition: "sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed', 'withheld'))",
    },
    ColumnSpec {
        name: "sync_version",
        definition: "sync_version INTEGER NOT NULL DEFAULT 0",
    },
    ColumnSpec {
        name: "deleted_at_ms",
        definition: "deleted_at_ms INTEGER",
    },
    ColumnSpec {
        name: "metadata_json",
        definition: "metadata_json TEXT NOT NULL DEFAULT '{}'",
    },
];

const CATALOG_SESSION_IMPORT_STATE_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec {
        name: "indexed_at_ms",
        definition: "indexed_at_ms INTEGER",
    },
    ColumnSpec {
        name: "indexed_file_size_bytes",
        definition: "indexed_file_size_bytes INTEGER",
    },
    ColumnSpec {
        name: "indexed_file_modified_at_ms",
        definition: "indexed_file_modified_at_ms INTEGER",
    },
    ColumnSpec {
        name: "indexed_status",
        definition: "indexed_status TEXT NOT NULL DEFAULT 'pending' CHECK (indexed_status IN ('pending', 'indexed', 'failed'))",
    },
    ColumnSpec {
        name: "indexed_error",
        definition: "indexed_error TEXT",
    },
    ColumnSpec {
        name: "indexed_event_count",
        definition: "indexed_event_count INTEGER",
    },
    ColumnSpec {
        name: "last_imported_at_ms",
        definition: "last_imported_at_ms INTEGER",
    },
    ColumnSpec {
        name: "last_imported_file_size_bytes",
        definition: "last_imported_file_size_bytes INTEGER",
    },
    ColumnSpec {
        name: "last_imported_file_modified_at_ms",
        definition: "last_imported_file_modified_at_ms INTEGER",
    },
    ColumnSpec {
        name: "last_imported_file_sha256",
        definition: "last_imported_file_sha256 TEXT",
    },
    ColumnSpec {
        name: "last_imported_event_count",
        definition: "last_imported_event_count INTEGER",
    },
];

const CREATE_TABLES_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS capture_sources (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('provider_import', 'provider_hook', 'direct_cli', 'manual')),
    provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'opencode', 'openloaf', 'kilo', 'kiro_cli', 'crush', 'goose', 'antigravity', 'gemini', 'cursor', 'windsurf', 'zed', 'copilot_cli', 'factory_ai_droid', 'qwen_code', 'kimi_code_cli', 'autohand_code', 'iflow_cli', 'jazz', 'forgecode', 'deepagents', 'mistral_vibe', 'mux', 'reasonix', 'kode', 'neovate', 'command_code', 'terramind', 'rovodev', 'cortex_code', 'openclaw', 'hermes', 'nanoclaw', 'astrbot', 'shelley', 'continue', 'openhands', 'cline', 'roo_code', 'dexto', 'lingma', 'pochi', 'codebuddy', 'aider_desk', 'auggie', 'firebender', 'shell', 'git', 'jj', 'gh', 'custom', 'unknown')),
    machine_id TEXT NOT NULL,
    process_id INTEGER,
    cwd TEXT,
    raw_source_path TEXT,
    external_session_id TEXT,
    started_at_ms INTEGER NOT NULL,
    ended_at_ms INTEGER,
    fidelity TEXT NOT NULL CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full', 'withheld')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed', 'withheld')),
    sync_version INTEGER NOT NULL DEFAULT 0,
    metadata_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS catalog_sessions (
    source_path TEXT PRIMARY KEY NOT NULL,
    provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'opencode', 'openloaf', 'kilo', 'kiro_cli', 'crush', 'goose', 'antigravity', 'gemini', 'cursor', 'windsurf', 'zed', 'copilot_cli', 'factory_ai_droid', 'qwen_code', 'kimi_code_cli', 'autohand_code', 'iflow_cli', 'jazz', 'forgecode', 'deepagents', 'mistral_vibe', 'mux', 'reasonix', 'kode', 'neovate', 'command_code', 'terramind', 'rovodev', 'cortex_code', 'openclaw', 'hermes', 'nanoclaw', 'astrbot', 'shelley', 'continue', 'openhands', 'cline', 'roo_code', 'dexto', 'lingma', 'pochi', 'codebuddy', 'aider_desk', 'auggie', 'firebender', 'shell', 'git', 'jj', 'gh', 'custom', 'unknown')),
    source_format TEXT NOT NULL,
    source_root TEXT NOT NULL,
    external_session_id TEXT,
    parent_external_session_id TEXT,
    agent_type TEXT NOT NULL CHECK (agent_type IN ('primary', 'subagent', 'agent_team_member', 'reviewer', 'implementer', 'unknown')),
    role_hint TEXT,
    external_agent_id TEXT,
    cwd TEXT,
    session_started_at_ms INTEGER,
    file_size_bytes INTEGER NOT NULL,
    file_modified_at_ms INTEGER NOT NULL,
    cataloged_at_ms INTEGER NOT NULL,
    is_stale INTEGER NOT NULL DEFAULT 0,
    indexed_at_ms INTEGER,
    indexed_file_size_bytes INTEGER,
    indexed_file_modified_at_ms INTEGER,
    indexed_status TEXT NOT NULL DEFAULT 'pending' CHECK (indexed_status IN ('pending', 'indexed', 'failed')),
    indexed_error TEXT,
    indexed_event_count INTEGER,
    last_imported_at_ms INTEGER,
    last_imported_file_size_bytes INTEGER,
    last_imported_file_modified_at_ms INTEGER,
    last_imported_file_sha256 TEXT,
    last_imported_event_count INTEGER,
    metadata_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS source_import_files (
    provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'opencode', 'openloaf', 'kilo', 'kiro_cli', 'crush', 'goose', 'antigravity', 'gemini', 'cursor', 'windsurf', 'zed', 'copilot_cli', 'factory_ai_droid', 'qwen_code', 'kimi_code_cli', 'autohand_code', 'iflow_cli', 'jazz', 'forgecode', 'deepagents', 'mistral_vibe', 'mux', 'reasonix', 'kode', 'neovate', 'command_code', 'terramind', 'rovodev', 'cortex_code', 'openclaw', 'hermes', 'nanoclaw', 'astrbot', 'shelley', 'continue', 'openhands', 'cline', 'roo_code', 'dexto', 'lingma', 'pochi', 'codebuddy', 'aider_desk', 'auggie', 'firebender', 'shell', 'git', 'jj', 'gh', 'custom', 'unknown')),
    source_format TEXT NOT NULL,
    source_root TEXT NOT NULL,
    source_path TEXT NOT NULL,
    file_size_bytes INTEGER NOT NULL,
    file_modified_at_ms INTEGER NOT NULL,
    observed_at_ms INTEGER NOT NULL,
    is_stale INTEGER NOT NULL DEFAULT 0,
    indexed_at_ms INTEGER,
    indexed_file_size_bytes INTEGER,
    indexed_file_modified_at_ms INTEGER,
    indexed_status TEXT NOT NULL DEFAULT 'pending' CHECK (indexed_status IN ('pending', 'indexed', 'failed')),
    indexed_error TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY (provider, source_root, source_path)
);

CREATE TABLE IF NOT EXISTS vcs_workspaces (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('git', 'jj')),
    root_path TEXT NOT NULL,
    repo_fingerprint TEXT NOT NULL,
    primary_remote_url_normalized TEXT,
    host TEXT NOT NULL DEFAULT 'unknown' CHECK (host IN ('github', 'gitlab', 'bitbucket', 'local', 'unknown')),
    owner TEXT,
    name TEXT,
    monorepo_subpath TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    source_id TEXT REFERENCES capture_sources(id),
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full', 'withheld')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed', 'withheld')),
    sync_version INTEGER NOT NULL DEFAULT 0,
    deleted_at_ms INTEGER,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    UNIQUE(kind, repo_fingerprint)
);

CREATE TABLE IF NOT EXISTS history_records (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL,
    summary TEXT,
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'active', 'completed', 'abandoned', 'archived')),
    primary_vcs_workspace_id TEXT REFERENCES vcs_workspaces(id),
    started_at_ms INTEGER,
    last_activity_at_ms INTEGER NOT NULL DEFAULT 0,
    completed_at_ms INTEGER,
    confidence TEXT NOT NULL DEFAULT 'unknown' CHECK (confidence IN ('explicit', 'high', 'medium', 'low', 'unknown')),
    created_at_ms INTEGER NOT NULL DEFAULT 0,
    updated_at_ms INTEGER NOT NULL DEFAULT 0,
    source_id TEXT REFERENCES capture_sources(id),
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full', 'withheld')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed', 'withheld')),
    sync_version INTEGER NOT NULL DEFAULT 0,
    deleted_at_ms INTEGER,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    body TEXT NOT NULL DEFAULT '',
    tags_json TEXT NOT NULL DEFAULT '[]',
    kind TEXT NOT NULL DEFAULT 'note',
    workspace TEXT,
    created_at TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS artifacts (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('transcript', 'stdout', 'stderr', 'screenshot', 'report', 'diff', 'file_snapshot', 'json', 'markdown', 'binary')),
    blob_hash TEXT NOT NULL,
    blob_path TEXT NOT NULL,
    byte_size INTEGER NOT NULL,
    media_type TEXT,
    preview_text TEXT,
    redaction_state TEXT NOT NULL DEFAULT 'safe_preview' CHECK (redaction_state IN ('raw', 'redacted', 'safe_preview', 'withheld')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    source_id TEXT REFERENCES capture_sources(id),
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full', 'withheld')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed', 'withheld')),
    sync_version INTEGER NOT NULL DEFAULT 0,
    deleted_at_ms INTEGER,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    UNIQUE(blob_hash, kind)
);

CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY NOT NULL,
    history_record_id TEXT REFERENCES history_records(id),
    parent_session_id TEXT REFERENCES sessions(id),
    root_session_id TEXT REFERENCES sessions(id),
    capture_source_id TEXT REFERENCES capture_sources(id),
    provider TEXT NOT NULL,
    external_session_id TEXT,
    external_agent_id TEXT,
    agent_type TEXT NOT NULL CHECK (agent_type IN ('primary', 'subagent', 'agent_team_member', 'reviewer', 'implementer', 'unknown')),
    role_hint TEXT,
    is_primary INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL CHECK (status IN ('started', 'active', 'idle', 'completed', 'failed', 'interrupted', 'imported')),
    fidelity TEXT NOT NULL CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    transcript_blob_id TEXT REFERENCES artifacts(id),
    started_at_ms INTEGER NOT NULL,
    ended_at_ms INTEGER,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full', 'withheld')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed', 'withheld')),
    sync_version INTEGER NOT NULL DEFAULT 0,
    deleted_at_ms INTEGER,
    metadata_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS session_edges (
    id TEXT PRIMARY KEY NOT NULL,
    from_session_id TEXT NOT NULL REFERENCES sessions(id),
    to_session_id TEXT NOT NULL REFERENCES sessions(id),
    edge_type TEXT NOT NULL CHECK (edge_type IN ('parent_child', 'delegated', 'reviewed', 'spawned', 'resumed_from', 'imported_related')),
    confidence TEXT NOT NULL DEFAULT 'unknown' CHECK (confidence IN ('explicit', 'high', 'medium', 'low', 'unknown')),
    source_id TEXT REFERENCES capture_sources(id),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full', 'withheld')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed', 'withheld')),
    sync_version INTEGER NOT NULL DEFAULT 0,
    deleted_at_ms INTEGER,
    metadata_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS runs (
    id TEXT PRIMARY KEY NOT NULL,
    history_record_id TEXT REFERENCES history_records(id),
    session_id TEXT REFERENCES sessions(id),
    run_type TEXT NOT NULL CHECK (run_type IN ('agent_turn', 'command', 'tool_call', 'review', 'import', 'summary')),
    status TEXT NOT NULL CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'cancelled', 'partial')),
    started_at_ms INTEGER NOT NULL,
    ended_at_ms INTEGER,
    exit_code INTEGER,
    cwd TEXT,
    command_preview TEXT,
    input_blob_id TEXT REFERENCES artifacts(id),
    output_blob_id TEXT REFERENCES artifacts(id),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    source_id TEXT REFERENCES capture_sources(id),
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full', 'withheld')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed', 'withheld')),
    sync_version INTEGER NOT NULL DEFAULT 0,
    deleted_at_ms INTEGER,
    metadata_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS events (
    id TEXT PRIMARY KEY NOT NULL,
    seq INTEGER NOT NULL UNIQUE,
    history_record_id TEXT REFERENCES history_records(id),
    session_id TEXT REFERENCES sessions(id),
    run_id TEXT REFERENCES runs(id),
    event_type TEXT NOT NULL CHECK (event_type IN ('message', 'tool_call', 'tool_output', 'command_started', 'command_output', 'command_finished', 'file_touched', 'vcs_change', 'artifact', 'summary', 'notice')),
    role TEXT CHECK (role IS NULL OR role IN ('user', 'assistant', 'system', 'tool', 'unknown')),
    occurred_at_ms INTEGER NOT NULL,
    capture_source_id TEXT REFERENCES capture_sources(id),
    payload_json TEXT NOT NULL DEFAULT '{}',
    payload_blob_id TEXT REFERENCES artifacts(id),
    dedupe_key TEXT,
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full', 'withheld')),
    redaction_state TEXT NOT NULL DEFAULT 'safe_preview' CHECK (redaction_state IN ('raw', 'redacted', 'safe_preview', 'withheld')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed', 'withheld')),
    sync_version INTEGER NOT NULL DEFAULT 0,
    deleted_at_ms INTEGER,
    metadata_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS vcs_changes (
    id TEXT PRIMARY KEY NOT NULL,
    vcs_workspace_id TEXT NOT NULL REFERENCES vcs_workspaces(id),
    kind TEXT NOT NULL CHECK (kind IN ('git_commit', 'git_branch', 'git_worktree', 'jj_change', 'jj_bookmark', 'patch', 'working_copy')),
    change_id TEXT NOT NULL,
    parent_change_ids_json TEXT NOT NULL DEFAULT '[]',
    branch_or_bookmark TEXT,
    tree_hash TEXT,
    author_time_ms INTEGER,
    confidence TEXT NOT NULL DEFAULT 'unknown' CHECK (confidence IN ('explicit', 'high', 'medium', 'low', 'unknown')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    source_id TEXT REFERENCES capture_sources(id),
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full', 'withheld')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed', 'withheld')),
    sync_version INTEGER NOT NULL DEFAULT 0,
    deleted_at_ms INTEGER,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    UNIQUE(vcs_workspace_id, kind, change_id)
);

CREATE TABLE IF NOT EXISTS history_record_links (
    id TEXT PRIMARY KEY NOT NULL,
    history_record_id TEXT NOT NULL REFERENCES history_records(id),
    target_type TEXT NOT NULL CHECK (target_type IN ('session', 'run', 'event', 'vcs_workspace', 'vcs_change', 'artifact')),
    target_id TEXT NOT NULL,
    link_type TEXT NOT NULL CHECK (link_type IN ('produced', 'touched', 'references', 'likely_related')),
    confidence TEXT NOT NULL DEFAULT 'unknown' CHECK (confidence IN ('explicit', 'high', 'medium', 'low', 'unknown')),
    source_id TEXT REFERENCES capture_sources(id),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full', 'withheld')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed', 'withheld')),
    sync_version INTEGER NOT NULL DEFAULT 0,
    deleted_at_ms INTEGER,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    UNIQUE(history_record_id, target_type, target_id, link_type)
);

CREATE TABLE IF NOT EXISTS summaries (
    id TEXT PRIMARY KEY NOT NULL,
    history_record_id TEXT REFERENCES history_records(id),
    session_id TEXT REFERENCES sessions(id),
    kind TEXT NOT NULL CHECK (kind IN ('imported_provider_summary', 'ctx_generated', 'agent_supplied', 'human_note')),
    model_or_source TEXT,
    text TEXT NOT NULL,
    citations_json TEXT NOT NULL DEFAULT '[]',
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    source_id TEXT REFERENCES capture_sources(id),
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full', 'withheld')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed', 'withheld')),
    sync_version INTEGER NOT NULL DEFAULT 0,
    deleted_at_ms INTEGER,
    metadata_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS files_touched (
    id TEXT PRIMARY KEY NOT NULL,
    history_record_id TEXT REFERENCES history_records(id),
    run_id TEXT REFERENCES runs(id),
    event_id TEXT REFERENCES events(id),
    vcs_workspace_id TEXT REFERENCES vcs_workspaces(id),
    path TEXT NOT NULL,
    change_kind TEXT CHECK (change_kind IS NULL OR change_kind IN ('read', 'created', 'modified', 'deleted', 'renamed', 'unknown')),
    old_path TEXT,
    line_count_delta INTEGER,
    confidence TEXT NOT NULL DEFAULT 'unknown' CHECK (confidence IN ('explicit', 'high', 'medium', 'low', 'unknown')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    source_id TEXT REFERENCES capture_sources(id),
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full', 'withheld')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed', 'withheld')),
    sync_version INTEGER NOT NULL DEFAULT 0,
    deleted_at_ms INTEGER,
    metadata_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS tags (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL DEFAULT 'user' CHECK (kind IN ('user', 'system', 'inferred')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    metadata_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS history_record_tags (
    history_record_id TEXT NOT NULL REFERENCES history_records(id),
    tag_id TEXT NOT NULL REFERENCES tags(id),
    source_id TEXT REFERENCES capture_sources(id),
    confidence TEXT NOT NULL DEFAULT 'unknown' CHECK (confidence IN ('explicit', 'high', 'medium', 'low', 'unknown')),
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (history_record_id, tag_id)
);

CREATE TABLE IF NOT EXISTS record_edges (
    id TEXT PRIMARY KEY NOT NULL,
    from_record_id TEXT NOT NULL REFERENCES history_records(id),
    to_record_id TEXT NOT NULL REFERENCES history_records(id),
    edge_type TEXT NOT NULL CHECK (edge_type IN ('continues', 'duplicates', 'blocks', 'related', 'supersedes', 'split_from')),
    confidence TEXT NOT NULL DEFAULT 'unknown' CHECK (confidence IN ('explicit', 'high', 'medium', 'low', 'unknown')),
    source_id TEXT REFERENCES capture_sources(id),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full', 'withheld')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed', 'withheld')),
    sync_version INTEGER NOT NULL DEFAULT 0,
    deleted_at_ms INTEGER,
    metadata_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS sync_cursors (
    id TEXT PRIMARY KEY NOT NULL,
    team_id TEXT,
    device_id TEXT NOT NULL,
    stream TEXT NOT NULL,
    cursor TEXT NOT NULL,
    last_synced_at_ms INTEGER,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(team_id, device_id, stream)
);

CREATE TABLE IF NOT EXISTS sync_batches (
    id TEXT PRIMARY KEY NOT NULL,
    team_id TEXT,
    device_id TEXT NOT NULL,
    direction TEXT NOT NULL CHECK (direction IN ('upload', 'download')),
    status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'succeeded', 'failed')),
    started_at_ms INTEGER,
    finished_at_ms INTEGER,
    row_count INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    metadata_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS sync_outbox (
    id TEXT PRIMARY KEY NOT NULL,
    local_table TEXT NOT NULL,
    local_id TEXT NOT NULL,
    operation TEXT NOT NULL CHECK (operation IN ('insert', 'update', 'delete', 'blob_upload')),
    team_id TEXT,
    device_id TEXT NOT NULL,
    sync_state TEXT NOT NULL DEFAULT 'pending' CHECK (sync_state IN ('pending', 'synced', 'failed', 'withheld')),
    attempt_count INTEGER NOT NULL DEFAULT 0,
    next_attempt_at_ms INTEGER,
    last_error TEXT,
    payload_json TEXT NOT NULL DEFAULT '{}',
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(local_table, local_id, operation, team_id)
);

CREATE TABLE IF NOT EXISTS local_devices (
    id TEXT PRIMARY KEY NOT NULL,
    stable_device_id TEXT NOT NULL UNIQUE,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    metadata_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS local_workspaces (
    id TEXT PRIMARY KEY NOT NULL,
    device_id TEXT NOT NULL REFERENCES local_devices(id),
    vcs_workspace_id TEXT REFERENCES vcs_workspaces(id),
    repo_fingerprint TEXT NOT NULL,
    root_path_hash TEXT NOT NULL,
    display_root TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    UNIQUE(device_id, repo_fingerprint, root_path_hash)
);

CREATE TABLE IF NOT EXISTS audit_log (
    id TEXT PRIMARY KEY NOT NULL,
    actor_kind TEXT NOT NULL CHECK (actor_kind IN ('human', 'agent', 'system')),
    actor_id TEXT,
    action TEXT NOT NULL,
    target_table TEXT,
    target_id TEXT,
    occurred_at_ms INTEGER NOT NULL,
    source_id TEXT REFERENCES capture_sources(id),
    metadata_json TEXT NOT NULL DEFAULT '{}'
);
"#;

const INDEXES_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS idx_capture_sources_external_session_id ON capture_sources(provider, external_session_id);

CREATE INDEX IF NOT EXISTS idx_catalog_sessions_provider_external_session_id ON catalog_sessions(provider, external_session_id);
CREATE INDEX IF NOT EXISTS idx_catalog_sessions_provider_source_root_stale ON catalog_sessions(provider, source_root, is_stale);
CREATE INDEX IF NOT EXISTS idx_catalog_sessions_provider_source_root_import ON catalog_sessions(provider, source_root, is_stale, indexed_status);
CREATE INDEX IF NOT EXISTS idx_catalog_sessions_started_at ON catalog_sessions(session_started_at_ms);
CREATE INDEX IF NOT EXISTS idx_catalog_sessions_cwd ON catalog_sessions(cwd);
CREATE INDEX IF NOT EXISTS idx_source_import_files_provider_source_root_import ON source_import_files(provider, source_root, is_stale, indexed_status);
CREATE INDEX IF NOT EXISTS idx_source_import_files_provider_source_root_stale ON source_import_files(provider, source_root, is_stale);
CREATE INDEX IF NOT EXISTS idx_sessions_provider_external_session_id ON sessions(provider, external_session_id);

CREATE INDEX IF NOT EXISTS idx_history_records_primary_vcs_workspace_id ON history_records(primary_vcs_workspace_id);
CREATE INDEX IF NOT EXISTS idx_history_records_source_id ON history_records(source_id);
CREATE INDEX IF NOT EXISTS idx_history_records_last_activity_at_ms ON history_records(last_activity_at_ms);
CREATE INDEX IF NOT EXISTS idx_history_records_created_at ON history_records(created_at DESC);

CREATE INDEX IF NOT EXISTS idx_sessions_history_record_id ON sessions(history_record_id);
CREATE INDEX IF NOT EXISTS idx_sessions_parent_session_id ON sessions(parent_session_id);
CREATE INDEX IF NOT EXISTS idx_sessions_root_session_id ON sessions(root_session_id);
CREATE INDEX IF NOT EXISTS idx_sessions_capture_source_id ON sessions(capture_source_id);
CREATE INDEX IF NOT EXISTS idx_sessions_transcript_blob_id ON sessions(transcript_blob_id);

CREATE INDEX IF NOT EXISTS idx_session_edges_from_session_id ON session_edges(from_session_id);
CREATE INDEX IF NOT EXISTS idx_session_edges_to_session_id ON session_edges(to_session_id);
CREATE INDEX IF NOT EXISTS idx_session_edges_source_id ON session_edges(source_id);

CREATE INDEX IF NOT EXISTS idx_runs_history_record_started_at_ms ON runs(history_record_id, started_at_ms);
CREATE INDEX IF NOT EXISTS idx_runs_history_record_id ON runs(history_record_id);
CREATE INDEX IF NOT EXISTS idx_runs_session_id ON runs(session_id);
CREATE INDEX IF NOT EXISTS idx_runs_input_blob_id ON runs(input_blob_id);
CREATE INDEX IF NOT EXISTS idx_runs_output_blob_id ON runs(output_blob_id);
CREATE INDEX IF NOT EXISTS idx_runs_source_id ON runs(source_id);

CREATE INDEX IF NOT EXISTS idx_events_seq ON events(seq);
CREATE INDEX IF NOT EXISTS idx_events_history_record_occurred_at_ms ON events(history_record_id, occurred_at_ms);
CREATE INDEX IF NOT EXISTS idx_events_session_occurred_at_ms ON events(session_id, occurred_at_ms);
CREATE INDEX IF NOT EXISTS idx_events_history_record_id ON events(history_record_id);
CREATE INDEX IF NOT EXISTS idx_events_session_id ON events(session_id);
CREATE INDEX IF NOT EXISTS idx_events_run_id ON events(run_id);
CREATE INDEX IF NOT EXISTS idx_events_capture_source_id ON events(capture_source_id);
CREATE INDEX IF NOT EXISTS idx_events_payload_blob_id ON events(payload_blob_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_events_dedupe_key ON events(dedupe_key) WHERE dedupe_key IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_vcs_workspaces_kind_repo_fingerprint ON vcs_workspaces(kind, repo_fingerprint);
CREATE INDEX IF NOT EXISTS idx_vcs_workspaces_source_id ON vcs_workspaces(source_id);

CREATE INDEX IF NOT EXISTS idx_vcs_changes_vcs_workspace_id ON vcs_changes(vcs_workspace_id);
CREATE INDEX IF NOT EXISTS idx_vcs_changes_source_id ON vcs_changes(source_id);

CREATE INDEX IF NOT EXISTS idx_history_record_links_history_record_id ON history_record_links(history_record_id);
CREATE INDEX IF NOT EXISTS idx_history_record_links_source_id ON history_record_links(source_id);

CREATE INDEX IF NOT EXISTS idx_artifacts_source_id ON artifacts(source_id);

CREATE INDEX IF NOT EXISTS idx_summaries_history_record_id ON summaries(history_record_id);
CREATE INDEX IF NOT EXISTS idx_summaries_session_id ON summaries(session_id);
CREATE INDEX IF NOT EXISTS idx_summaries_source_id ON summaries(source_id);

CREATE INDEX IF NOT EXISTS idx_files_touched_history_record_id ON files_touched(history_record_id);
CREATE INDEX IF NOT EXISTS idx_files_touched_run_id ON files_touched(run_id);
CREATE INDEX IF NOT EXISTS idx_files_touched_event_id ON files_touched(event_id);
CREATE INDEX IF NOT EXISTS idx_files_touched_vcs_workspace_id ON files_touched(vcs_workspace_id);
CREATE INDEX IF NOT EXISTS idx_files_touched_source_id ON files_touched(source_id);
CREATE INDEX IF NOT EXISTS idx_files_touched_path ON files_touched(path);
CREATE INDEX IF NOT EXISTS idx_files_touched_old_path ON files_touched(old_path);

CREATE INDEX IF NOT EXISTS idx_history_record_tags_tag_id ON history_record_tags(tag_id);
CREATE INDEX IF NOT EXISTS idx_history_record_tags_source_id ON history_record_tags(source_id);

CREATE INDEX IF NOT EXISTS idx_record_edges_from_record_id ON record_edges(from_record_id);
CREATE INDEX IF NOT EXISTS idx_record_edges_to_record_id ON record_edges(to_record_id);
CREATE INDEX IF NOT EXISTS idx_record_edges_source_id ON record_edges(source_id);

CREATE INDEX IF NOT EXISTS idx_sync_outbox_sync_state_updated_at_ms ON sync_outbox(sync_state, updated_at_ms);
CREATE INDEX IF NOT EXISTS idx_local_workspaces_device_id ON local_workspaces(device_id);
CREATE INDEX IF NOT EXISTS idx_local_workspaces_vcs_workspace_id ON local_workspaces(vcs_workspace_id);
CREATE INDEX IF NOT EXISTS idx_audit_log_source_id ON audit_log(source_id);
"#;

// `safe_preview_text` is legacy schema naming. It stores local searchable
// preview text and must not be interpreted as share-safe redaction.
const FTS_TABLES_SQL: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS ctx_history_search USING fts5(
    record_id UNINDEXED,
    title,
    summary,
    primary_user_text,
    decision_text,
    context_text,
    tag_text
);

CREATE VIRTUAL TABLE IF NOT EXISTS event_search USING fts5(
    event_id UNINDEXED,
    history_record_id UNINDEXED,
    session_id UNINDEXED,
    role UNINDEXED,
    safe_preview_text,
    rank_bucket UNINDEXED
);

CREATE VIRTUAL TABLE IF NOT EXISTS artifact_search USING fts5(
    artifact_id UNINDEXED,
    history_record_id UNINDEXED,
    safe_preview_text
);
"#;

const STABLE_SQL_VIEWS_SQL: &str = r#"
DROP VIEW IF EXISTS ctx_sessions;
CREATE VIEW ctx_sessions AS
SELECT
    s.id AS ctx_session_id,
    s.history_record_id,
    s.parent_session_id AS parent_ctx_session_id,
    s.root_session_id AS root_ctx_session_id,
    s.provider AS provider,
    s.external_session_id AS provider_session_id,
    s.external_agent_id AS external_agent_id,
    s.agent_type AS agent_type,
    s.role_hint AS role_hint,
    s.is_primary AS is_primary,
    s.status AS status,
    s.fidelity AS fidelity,
    s.started_at_ms AS started_at_ms,
    s.ended_at_ms AS ended_at_ms,
    cs.cwd AS cwd,
    cs.raw_source_path AS source_path
FROM sessions s
LEFT JOIN capture_sources cs ON cs.id = s.capture_source_id
WHERE s.deleted_at_ms IS NULL;

DROP VIEW IF EXISTS ctx_events;
CREATE VIEW ctx_events AS
SELECT
    e.id AS ctx_event_id,
    e.session_id AS ctx_session_id,
    e.history_record_id AS history_record_id,
    s.provider AS provider,
    s.external_session_id AS provider_session_id,
    e.seq AS event_seq,
    e.event_type AS event_type,
    e.role AS role,
    e.occurred_at_ms AS occurred_at_ms,
    e.payload_json AS payload_json,
    e.redaction_state AS redaction_state,
    e.fidelity AS fidelity,
    cs.cwd AS cwd,
    cs.raw_source_path AS source_path
FROM events e
LEFT JOIN sessions s ON s.id = e.session_id
LEFT JOIN capture_sources cs ON cs.id = e.capture_source_id
WHERE e.deleted_at_ms IS NULL;

DROP VIEW IF EXISTS ctx_files_touched;
CREATE VIEW ctx_files_touched AS
SELECT
    ft.id AS ctx_file_touch_id,
    ft.path AS path,
    ft.old_path AS old_path,
    ft.change_kind AS change_kind,
    ft.line_count_delta AS line_count_delta,
    ft.confidence AS confidence,
    ft.event_id AS ctx_event_id,
    COALESCE(e.session_id, r.session_id, source_session.id) AS ctx_session_id,
    COALESCE(
        e.history_record_id,
        r.history_record_id,
        ft.history_record_id,
        event_session.history_record_id,
        run_session.history_record_id,
        source_session.history_record_id
    ) AS history_record_id,
    COALESCE(s.provider, cs.provider) AS provider,
    COALESCE(s.external_session_id, cs.external_session_id) AS provider_session_id,
    ft.created_at_ms AS created_at_ms,
    ft.updated_at_ms AS updated_at_ms
FROM files_touched ft
LEFT JOIN events e ON e.id = ft.event_id
LEFT JOIN runs r ON r.id = ft.run_id
LEFT JOIN capture_sources cs ON cs.id = ft.source_id
LEFT JOIN sessions event_session ON event_session.id = e.session_id
LEFT JOIN sessions run_session ON run_session.id = r.session_id
LEFT JOIN sessions source_session ON source_session.capture_source_id = ft.source_id
LEFT JOIN sessions s ON s.id = COALESCE(e.session_id, r.session_id, source_session.id)
WHERE ft.deleted_at_ms IS NULL;

DROP VIEW IF EXISTS ctx_sources;
CREATE VIEW ctx_sources AS
SELECT
    provider AS provider,
    source_format AS source_format,
    source_root AS source_root,
    source_path AS source_path,
    external_session_id AS provider_session_id,
    parent_external_session_id AS parent_provider_session_id,
    agent_type AS agent_type,
    role_hint AS role_hint,
    external_agent_id AS external_agent_id,
    cwd AS cwd,
    session_started_at_ms AS session_started_at_ms,
    file_size_bytes AS file_size_bytes,
    file_modified_at_ms AS file_modified_at_ms,
    cataloged_at_ms AS cataloged_at_ms,
    indexed_at_ms AS indexed_at_ms,
    indexed_status AS indexed_status,
    indexed_error AS indexed_error,
    indexed_event_count AS indexed_event_count,
    last_imported_at_ms AS last_imported_at_ms,
    last_imported_file_size_bytes AS last_imported_file_size_bytes,
    last_imported_file_modified_at_ms AS last_imported_file_modified_at_ms,
    last_imported_file_sha256 AS last_imported_file_sha256,
    last_imported_event_count AS last_imported_event_count,
    is_stale AS is_stale
FROM catalog_sessions;
"#;

pub struct Store {
    path: PathBuf,
    object_dir: PathBuf,
    conn: Connection,
    busy_timeout: Duration,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_busy_timeout(path, BUSY_TIMEOUT)
    }

    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let object_dir = path
            .parent()
            .map(|parent| parent.join(OBJECTS_DIR))
            .unwrap_or_else(|| PathBuf::from(OBJECTS_DIR));
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        configure_read_only_connection(&conn, BUSY_TIMEOUT)?;
        let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if user_version != SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchemaVersion(user_version));
        }
        Ok(Self {
            path,
            object_dir,
            conn,
            busy_timeout: BUSY_TIMEOUT,
        })
    }

    pub fn open_with_busy_timeout(path: impl AsRef<Path>, busy_timeout: Duration) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut migrated_legacy_layout = false;
        if let Some(parent) = path.parent() {
            migrated_legacy_layout = migrate_legacy_history_layout(parent)?;
            fs::create_dir_all(parent)?;
            restrict_private_dir(parent)?;
        }
        let object_dir = path
            .parent()
            .map(|parent| parent.join(OBJECTS_DIR))
            .unwrap_or_else(|| PathBuf::from(OBJECTS_DIR));
        fs::create_dir_all(&object_dir)?;
        restrict_private_dir(&object_dir)?;
        if let Some(spool_dir) = path.parent().map(|parent| parent.join(SPOOL_DIR)) {
            fs::create_dir_all(&spool_dir)?;
            restrict_private_dir(&spool_dir)?;
        }
        let conn = Connection::open(&path)?;
        restrict_private_file(&path)?;
        configure_connection(&conn, busy_timeout)?;
        let store = Self {
            path,
            object_dir,
            conn,
            busy_timeout,
        };
        store.migrate()?;
        if migrated_legacy_layout {
            store.normalize_legacy_blob_paths()?;
        }
        store.ensure_search_projection_initialized()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn raw_sql_query(&self, sql: &str, options: RawSqlOptions) -> Result<RawSqlResult> {
        let sql = sql.trim();
        if sql.is_empty() {
            return Err(StoreError::RawSqlEmpty);
        }
        validate_raw_sql_options(&options)?;
        validate_raw_sql_statement_bytes(sql, &options)?;
        reject_sql_tail(&self.conn, sql)?;
        let _limits = RawSqlLimitGuard::apply(&self.conn, &options)?;

        let mut stmt = self.conn.prepare(sql)?;
        if stmt.parameter_count() > 0 {
            return Err(StoreError::RawSqlHasParameters);
        }
        if !stmt.readonly() {
            return Err(StoreError::RawSqlNotReadOnly);
        }
        let column_count = stmt.column_count();
        if column_count == 0 {
            return Err(StoreError::RawSqlNoColumns);
        }
        if column_count > options.max_columns {
            return Err(StoreError::RawSqlTooManyColumns {
                columns: column_count,
                max_columns: options.max_columns,
            });
        }
        validate_raw_sql_result_preview_budget(&options, column_count)?;

        let columns = stmt
            .column_names()
            .into_iter()
            .map(|name| RawSqlColumn {
                name: name.to_owned(),
            })
            .collect::<Vec<_>>();
        let started = Instant::now();
        let timeout = options.timeout;
        let progress_started = started;
        self.conn
            .progress_handler(1_000, Some(move || progress_started.elapsed() >= timeout));

        let query_result = (|| -> Result<RawSqlResult> {
            let mut rows = stmt.query([])?;
            let mut output_rows = Vec::new();
            let mut rows_truncated = false;
            let mut values_truncated = false;

            while let Some(row) = rows.next()? {
                if output_rows.len() >= options.max_rows {
                    rows_truncated = true;
                    break;
                }
                let mut output_row = Vec::with_capacity(column_count);
                for index in 0..column_count {
                    let value = raw_sql_value(row.get_ref(index)?, options.max_value_bytes);
                    if value.is_truncated() {
                        values_truncated = true;
                    }
                    output_row.push(value);
                }
                output_rows.push(output_row);
            }

            Ok(RawSqlResult {
                returned_rows: output_rows.len(),
                columns,
                rows: output_rows,
                truncated: RawSqlTruncation {
                    rows: rows_truncated,
                    values: values_truncated,
                },
                elapsed: started.elapsed(),
                limits: RawSqlLimits {
                    max_rows: options.max_rows,
                    max_columns: options.max_columns,
                    max_value_bytes: options.max_value_bytes,
                    max_sql_bytes: options.max_sql_bytes,
                    timeout_ms: duration_ms(options.timeout),
                },
            })
        })();

        self.conn.progress_handler(0, None::<fn() -> bool>);

        match query_result {
            Err(StoreError::Sql(rusqlite::Error::SqliteFailure(error, _)))
                if error.code == ErrorCode::OperationInterrupted
                    && started.elapsed() >= options.timeout =>
            {
                Err(StoreError::RawSqlTimedOut {
                    timeout_ms: duration_ms(options.timeout),
                })
            }
            other => other,
        }
    }

    pub fn begin_immediate_batch(&self) -> Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        Ok(())
    }

    pub fn commit_batch(&self) -> Result<()> {
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    pub fn rollback_batch(&self) -> Result<()> {
        self.conn.execute_batch("ROLLBACK")?;
        Ok(())
    }

    pub fn checkpoint_wal_passive(&self) -> Result<()> {
        self.conn
            .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |_| Ok(()))?;
        Ok(())
    }

    pub fn checkpoint_wal_truncate(&self) -> Result<()> {
        self.conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    }

    pub fn checkpoint_wal_passive_if_larger_than(&self, min_bytes: u64) -> Result<bool> {
        let Some(wal_bytes) = self.wal_bytes()? else {
            return Ok(false);
        };
        if wal_bytes < min_bytes {
            return Ok(false);
        }
        self.checkpoint_wal_passive()?;
        Ok(true)
    }

    pub fn checkpoint_wal_truncate_if_larger_than(&self, min_bytes: u64) -> Result<bool> {
        let Some(wal_bytes) = self.wal_bytes()? else {
            return Ok(false);
        };
        if wal_bytes < min_bytes {
            return Ok(false);
        }
        self.checkpoint_wal_truncate()?;
        Ok(true)
    }

    fn wal_path(&self) -> PathBuf {
        let mut path = self.path.as_os_str().to_os_string();
        path.push("-wal");
        PathBuf::from(path)
    }

    fn wal_bytes(&self) -> Result<Option<u64>> {
        match fs::metadata(self.wal_path()) {
            Ok(metadata) => Ok(Some(metadata.len())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(StoreError::Io(err)),
        }
    }

    pub fn migrate(&self) -> Result<()> {
        configure_connection(&self.conn, self.busy_timeout)?;
        let user_version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if user_version > SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchemaVersion(user_version));
        }
        if user_version < 1 {
            migrate_to_v1(&self.conn)?;
        }
        if user_version < 2 {
            migrate_to_v2(&self.conn)?;
        }
        if user_version < 3 {
            migrate_to_v3(&self.conn)?;
        }
        if user_version < 4 {
            migrate_to_v4(&self.conn)?;
        }
        if user_version < 5 {
            migrate_to_v5(&self.conn)?;
        }
        if user_version < 6 {
            migrate_to_v6(&self.conn)?;
        }
        if user_version < 7 {
            migrate_to_v7(&self.conn)?;
        }
        if user_version < 8 {
            migrate_to_v8(&self.conn)?;
        }
        if user_version < 9 {
            migrate_to_v9(&self.conn)?;
        }
        if user_version < 10 {
            migrate_to_v10(&self.conn)?;
        }
        if user_version < 11 {
            migrate_to_v11(&self.conn)?;
        }
        if user_version < 12 {
            migrate_to_v12(&self.conn)?;
        }
        if user_version < 13 {
            migrate_to_v13(&self.conn)?;
        }
        if user_version < 14 {
            migrate_to_v14(&self.conn)?;
        }
        if user_version < 15 {
            migrate_to_v15(&self.conn)?;
        }
        if user_version < 16 {
            migrate_to_v16(&self.conn)?;
        }
        if user_version < 17 {
            migrate_to_v17(&self.conn)?;
        }
        if user_version < 18 {
            migrate_to_v18(&self.conn)?;
        }
        if user_version < 19 {
            migrate_to_v19(&self.conn)?;
        }
        if user_version < 20 {
            migrate_to_v20(&self.conn)?;
        }
        if user_version < 21 {
            migrate_to_v21(&self.conn)?;
        }
        if user_version < 22 {
            migrate_to_v22(&self.conn)?;
        }
        if user_version < 23 {
            migrate_to_v23(&self.conn)?;
        }
        if user_version < 24 {
            migrate_to_v24(&self.conn)?;
        }
        if user_version < 25 {
            migrate_to_v25(&self.conn)?;
        }
        if user_version < 26 {
            migrate_to_v26(&self.conn)?;
        }
        if user_version < 27 {
            migrate_to_v27(&self.conn)?;
        }
        if user_version < 28 {
            migrate_to_v28(&self.conn)?;
        }
        if user_version < 29 {
            migrate_to_v29(&self.conn)?;
        }
        if user_version < 30 {
            migrate_to_v30(&self.conn)?;
        }
        if user_version < 31 {
            migrate_to_v31(&self.conn)?;
        }
        create_fts_tables_if_supported(&self.conn)?;
        Ok(())
    }

    pub fn schema(&self) -> Result<String> {
        let mut stmt = self.conn.prepare(
            "SELECT sql FROM sqlite_master
             WHERE type IN ('table', 'index', 'view') AND sql IS NOT NULL
             ORDER BY type, name",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut schema = Vec::new();
        for row in rows {
            schema.push(row?);
        }
        Ok(schema.join(";\n"))
    }

    pub fn refresh_search_index(&self) -> Result<()> {
        self.rebuild_search_projection()
    }

    pub fn optimize_search_index(&self) -> Result<()> {
        for table in ["ctx_history_search", "event_search", "artifact_search"] {
            if table_exists(&self.conn, table)? {
                self.conn.execute(
                    format!("INSERT INTO {table}({table}) VALUES ('optimize')").as_str(),
                    [],
                )?;
            }
        }
        Ok(())
    }

    pub fn event_search_projection_needs_backfill(&self) -> Result<bool> {
        if !table_exists(&self.conn, "event_search")? {
            return Ok(false);
        }
        Ok(table_row_count(&self.conn, "events")? > 0
            && table_row_count(&self.conn, "event_search")? == 0)
    }

    pub fn upsert_capture_source(&self, source: &CaptureSource) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO capture_sources
            (
                id, kind, provider, machine_id, process_id, cwd, raw_source_path,
                external_session_id, started_at_ms, ended_at_ms, fidelity,
                visibility, sync_state, sync_version, metadata_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
            ON CONFLICT(id) DO UPDATE SET
                kind = excluded.kind,
                provider = excluded.provider,
                machine_id = excluded.machine_id,
                process_id = excluded.process_id,
                cwd = excluded.cwd,
                raw_source_path = excluded.raw_source_path,
                external_session_id = excluded.external_session_id,
                started_at_ms = excluded.started_at_ms,
                ended_at_ms = excluded.ended_at_ms,
                fidelity = excluded.fidelity,
                visibility = excluded.visibility,
                sync_state = excluded.sync_state,
                sync_version = excluded.sync_version,
                metadata_json = excluded.metadata_json
            "#,
            params![
                source.id.to_string(),
                source.descriptor.kind.as_str(),
                source.descriptor.provider.as_str(),
                source.descriptor.machine_id.as_str(),
                source.descriptor.process_id.map(i64::from),
                source.descriptor.cwd.as_deref(),
                source.descriptor.raw_source_path.as_deref(),
                source.descriptor.external_session_id.as_deref(),
                timestamp_ms(source.started_at),
                optional_timestamp_ms(source.ended_at),
                source.sync.fidelity.as_str(),
                source.sync.visibility.as_str(),
                source.sync.sync_state.as_str(),
                source.sync.sync_version as i64,
                serde_json::to_string(&source.sync.metadata)?,
            ],
        )?;
        Ok(())
    }

    pub fn get_capture_source(&self, id: Uuid) -> Result<CaptureSource> {
        self.conn
            .query_row(
                "SELECT id, kind, provider, machine_id, process_id, cwd, raw_source_path, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json FROM capture_sources WHERE id = ?1",
                params![id.to_string()],
                capture_source_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    pub fn list_capture_sources(&self) -> Result<Vec<CaptureSource>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, provider, machine_id, process_id, cwd, raw_source_path, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json FROM capture_sources ORDER BY started_at_ms, id",
        )?;
        let rows = stmt.query_map([], capture_source_from_row)?;
        collect_rows(rows)
    }

    pub fn capture_source_count(&self) -> Result<usize> {
        let count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM capture_sources", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    pub fn capture_source_by_external_session(
        &self,
        provider: CaptureProvider,
        external_session_id: &str,
    ) -> Result<Option<CaptureSource>> {
        self.conn
            .query_row(
                "SELECT id, kind, provider, machine_id, process_id, cwd, raw_source_path, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json FROM capture_sources WHERE provider = ?1 AND external_session_id = ?2 ORDER BY started_at_ms DESC LIMIT 1",
                params![provider.as_str(), external_session_id],
                capture_source_from_row,
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn mark_catalog_source_stale(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        cataloged_at_ms: i64,
    ) -> Result<usize> {
        let changed = self.conn.execute(
            r#"
            UPDATE catalog_sessions
            SET is_stale = 1, cataloged_at_ms = ?3
            WHERE provider = ?1 AND source_root = ?2
            "#,
            params![provider.as_str(), source_root, cataloged_at_ms],
        )?;
        Ok(changed)
    }

    pub fn upsert_catalog_sessions(&self, sessions: &[CatalogSession]) -> Result<()> {
        let mut stmt = self.conn.prepare(
            r#"
            INSERT INTO catalog_sessions
            (
                source_path, provider, source_format, source_root,
                external_session_id, parent_external_session_id, agent_type, role_hint,
                external_agent_id, cwd, session_started_at_ms, file_size_bytes,
                file_modified_at_ms, cataloged_at_ms, is_stale, metadata_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 0, ?15)
            ON CONFLICT(source_path) DO UPDATE SET
                provider = excluded.provider,
                source_format = excluded.source_format,
                source_root = excluded.source_root,
                external_session_id = excluded.external_session_id,
                parent_external_session_id = excluded.parent_external_session_id,
                agent_type = excluded.agent_type,
                role_hint = excluded.role_hint,
                external_agent_id = excluded.external_agent_id,
                cwd = excluded.cwd,
                session_started_at_ms = excluded.session_started_at_ms,
                file_size_bytes = excluded.file_size_bytes,
                file_modified_at_ms = excluded.file_modified_at_ms,
                cataloged_at_ms = excluded.cataloged_at_ms,
                is_stale = 0,
                indexed_at_ms = CASE
                    WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                     AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN catalog_sessions.indexed_at_ms
                    ELSE NULL
                END,
                indexed_file_size_bytes = CASE
                    WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                     AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN catalog_sessions.indexed_file_size_bytes
                    ELSE NULL
                END,
                indexed_file_modified_at_ms = CASE
                    WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                     AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN catalog_sessions.indexed_file_modified_at_ms
                    ELSE NULL
                END,
                indexed_status = CASE
                    WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                     AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN catalog_sessions.indexed_status
                    ELSE 'pending'
                END,
                indexed_error = CASE
                    WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                     AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN catalog_sessions.indexed_error
                    ELSE NULL
                END,
                indexed_event_count = CASE
                    WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                     AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN catalog_sessions.indexed_event_count
                    ELSE NULL
                END,
                last_imported_at_ms = CASE
                    WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                     AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN catalog_sessions.last_imported_at_ms
                    WHEN excluded.file_size_bytes > catalog_sessions.file_size_bytes
                     AND catalog_sessions.indexed_status = 'indexed'
                     AND catalog_sessions.indexed_file_size_bytes = catalog_sessions.file_size_bytes
                     AND catalog_sessions.indexed_file_modified_at_ms = catalog_sessions.file_modified_at_ms
                     AND catalog_sessions.last_imported_file_size_bytes = catalog_sessions.file_size_bytes
                    THEN catalog_sessions.last_imported_at_ms
                    ELSE NULL
                END,
                last_imported_file_size_bytes = CASE
                    WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                     AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN catalog_sessions.last_imported_file_size_bytes
                    WHEN excluded.file_size_bytes > catalog_sessions.file_size_bytes
                     AND catalog_sessions.indexed_status = 'indexed'
                     AND catalog_sessions.indexed_file_size_bytes = catalog_sessions.file_size_bytes
                     AND catalog_sessions.indexed_file_modified_at_ms = catalog_sessions.file_modified_at_ms
                     AND catalog_sessions.last_imported_file_size_bytes = catalog_sessions.file_size_bytes
                    THEN catalog_sessions.last_imported_file_size_bytes
                    ELSE NULL
                END,
                last_imported_file_modified_at_ms = CASE
                    WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                     AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN catalog_sessions.last_imported_file_modified_at_ms
                    WHEN excluded.file_size_bytes > catalog_sessions.file_size_bytes
                     AND catalog_sessions.indexed_status = 'indexed'
                     AND catalog_sessions.indexed_file_size_bytes = catalog_sessions.file_size_bytes
                     AND catalog_sessions.indexed_file_modified_at_ms = catalog_sessions.file_modified_at_ms
                     AND catalog_sessions.last_imported_file_size_bytes = catalog_sessions.file_size_bytes
                    THEN catalog_sessions.last_imported_file_modified_at_ms
                    ELSE NULL
                END,
                last_imported_file_sha256 = CASE
                    WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                     AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN catalog_sessions.last_imported_file_sha256
                    WHEN excluded.file_size_bytes > catalog_sessions.file_size_bytes
                     AND catalog_sessions.indexed_status = 'indexed'
                     AND catalog_sessions.indexed_file_size_bytes = catalog_sessions.file_size_bytes
                     AND catalog_sessions.indexed_file_modified_at_ms = catalog_sessions.file_modified_at_ms
                     AND catalog_sessions.last_imported_file_size_bytes = catalog_sessions.file_size_bytes
                    THEN catalog_sessions.last_imported_file_sha256
                    ELSE NULL
                END,
                last_imported_event_count = CASE
                    WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                     AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN catalog_sessions.last_imported_event_count
                    WHEN excluded.file_size_bytes > catalog_sessions.file_size_bytes
                     AND catalog_sessions.indexed_status = 'indexed'
                     AND catalog_sessions.indexed_file_size_bytes = catalog_sessions.file_size_bytes
                     AND catalog_sessions.indexed_file_modified_at_ms = catalog_sessions.file_modified_at_ms
                     AND catalog_sessions.last_imported_file_size_bytes = catalog_sessions.file_size_bytes
                    THEN catalog_sessions.last_imported_event_count
                    ELSE NULL
                END,
                metadata_json = excluded.metadata_json
            WHERE catalog_sessions.provider IS NOT excluded.provider
               OR catalog_sessions.source_format IS NOT excluded.source_format
               OR catalog_sessions.source_root IS NOT excluded.source_root
               OR catalog_sessions.external_session_id IS NOT excluded.external_session_id
               OR catalog_sessions.parent_external_session_id IS NOT excluded.parent_external_session_id
               OR catalog_sessions.agent_type IS NOT excluded.agent_type
               OR catalog_sessions.role_hint IS NOT excluded.role_hint
               OR catalog_sessions.external_agent_id IS NOT excluded.external_agent_id
               OR catalog_sessions.cwd IS NOT excluded.cwd
               OR catalog_sessions.session_started_at_ms IS NOT excluded.session_started_at_ms
               OR catalog_sessions.file_size_bytes != excluded.file_size_bytes
               OR catalog_sessions.file_modified_at_ms != excluded.file_modified_at_ms
               OR catalog_sessions.is_stale != 0
               OR catalog_sessions.metadata_json IS NOT excluded.metadata_json
            "#,
        )?;
        for session in sessions {
            stmt.execute(params![
                session.source_path.as_str(),
                session.provider.as_str(),
                session.source_format.as_str(),
                session.source_root.as_str(),
                session.external_session_id.as_deref(),
                session.parent_external_session_id.as_deref(),
                session.agent_type.as_str(),
                session.role_hint.as_deref(),
                session.external_agent_id.as_deref(),
                session.cwd.as_deref(),
                session.session_started_at_ms,
                capped_i64(session.file_size_bytes),
                session.file_modified_at_ms,
                session.cataloged_at_ms,
                serde_json::to_string(&session.metadata)?,
            ])?;
        }
        Ok(())
    }

    pub fn list_catalog_sessions_for_source(
        &self,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<Vec<CatalogSession>> {
        let mut stmt = self.conn.prepare(
            format!(
                "{} WHERE provider = ?1 AND source_root = ?2",
                catalog_session_select_sql("")
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(
            params![provider.as_str(), source_root],
            catalog_session_from_row,
        )?;
        collect_rows(rows)
    }

    pub fn catalog_source_stale_session_count(
        &self,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<usize> {
        self.conn
            .query_row(
                r#"
                SELECT COUNT(*)
                FROM catalog_sessions
                WHERE provider = ?1
                  AND source_root = ?2
                  AND is_stale != 0
                "#,
                params![provider.as_str(), source_root],
                |row| row.get::<_, usize>(0),
            )
            .map_err(Into::into)
    }

    pub fn mark_catalog_source_missing_paths_stale(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        current_paths: &[String],
        cataloged_at_ms: i64,
    ) -> Result<usize> {
        self.conn.execute(
            "CREATE TEMP TABLE IF NOT EXISTS temp_catalog_current_paths(source_path TEXT PRIMARY KEY)",
            [],
        )?;
        self.conn
            .execute("DELETE FROM temp_catalog_current_paths", [])?;
        {
            let mut stmt = self.conn.prepare(
                "INSERT OR IGNORE INTO temp_catalog_current_paths(source_path) VALUES (?1)",
            )?;
            for path in current_paths {
                stmt.execute(params![path.as_str()])?;
            }
        }
        let changed = self.conn.execute(
            r#"
            UPDATE catalog_sessions
            SET is_stale = 1, cataloged_at_ms = ?3
            WHERE provider = ?1
              AND source_root = ?2
              AND NOT EXISTS (
                  SELECT 1
                  FROM temp_catalog_current_paths current
                  WHERE current.source_path = catalog_sessions.source_path
              )
            "#,
            params![provider.as_str(), source_root, cataloged_at_ms],
        )?;
        self.conn
            .execute("DELETE FROM temp_catalog_current_paths", [])?;
        Ok(changed)
    }

    pub fn list_pending_catalog_sessions(
        &self,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<Vec<CatalogSession>> {
        let mut stmt = self.conn.prepare(
            format!(
                "{} WHERE provider = ?1
                   AND source_root = ?2
                   AND is_stale = 0
                   AND {}
                 ORDER BY session_started_at_ms, source_path",
                catalog_session_select_sql(""),
                catalog_pending_import_condition_sql("catalog_sessions")
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(
            params![provider.as_str(), source_root],
            catalog_session_from_row,
        )?;
        collect_rows(rows)
    }

    pub fn mark_catalog_source_indexed(
        &self,
        provider: CaptureProvider,
        update: CatalogSourceIndexUpdate<'_>,
    ) -> Result<usize> {
        let changed = self.conn.execute(
            r#"
            UPDATE catalog_sessions
            SET indexed_at_ms = ?4,
                indexed_file_size_bytes = ?5,
                indexed_file_modified_at_ms = ?6,
                indexed_status = ?8,
                indexed_error = NULL,
                indexed_event_count = ?7,
                last_imported_at_ms = ?4,
                last_imported_file_size_bytes = ?5,
                last_imported_file_modified_at_ms = ?6,
                last_imported_file_sha256 = ?9,
                last_imported_event_count = ?7
            WHERE provider = ?1
              AND source_root = ?2
              AND source_path = ?3
              AND is_stale = 0
            "#,
            params![
                provider.as_str(),
                update.source_root,
                update.source_path,
                update.indexed_at_ms,
                capped_i64(update.file_size_bytes),
                update.file_modified_at_ms,
                update.event_count.map(capped_i64),
                CatalogIndexedStatus::Indexed.as_str(),
                update.file_sha256,
            ],
        )?;
        Ok(changed)
    }

    pub fn mark_catalog_source_failed(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        source_path: &str,
        error: &str,
        indexed_at_ms: i64,
    ) -> Result<usize> {
        let changed = self.conn.execute(
            r#"
            UPDATE catalog_sessions
            SET indexed_at_ms = ?4,
                indexed_file_size_bytes = NULL,
                indexed_file_modified_at_ms = NULL,
                indexed_status = ?6,
                indexed_error = ?5,
                indexed_event_count = NULL
            WHERE provider = ?1
              AND source_root = ?2
              AND source_path = ?3
              AND is_stale = 0
            "#,
            params![
                provider.as_str(),
                source_root,
                source_path,
                indexed_at_ms,
                error,
                CatalogIndexedStatus::Failed.as_str(),
            ],
        )?;
        Ok(changed)
    }

    pub fn catalog_source_index_state(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        source_path: &str,
    ) -> Result<Option<CatalogSourceIndexState>> {
        self.conn
            .query_row(
                r#"
                SELECT last_imported_file_size_bytes,
                       last_imported_file_modified_at_ms,
                       last_imported_event_count,
                       last_imported_at_ms,
                       last_imported_file_sha256
                FROM catalog_sessions
                WHERE provider = ?1
                  AND source_root = ?2
                  AND source_path = ?3
                  AND is_stale = 0
                "#,
                params![provider.as_str(), source_root, source_path],
                |row| {
                    let last_imported_file_size_bytes = row
                        .get::<_, Option<i64>>(0)?
                        .map(nonnegative_i64_to_u64)
                        .transpose()?;
                    let last_imported_event_count = row
                        .get::<_, Option<i64>>(2)?
                        .map(nonnegative_i64_to_u64)
                        .transpose()?;
                    Ok(CatalogSourceIndexState {
                        last_imported_file_size_bytes,
                        last_imported_file_modified_at_ms: row.get(1)?,
                        last_imported_event_count,
                        last_imported_at_ms: row.get(3)?,
                        last_imported_file_sha256: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn upsert_source_import_files(&self, files: &[SourceImportFile]) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        let mut stmt = self.conn.prepare(
            r#"
            INSERT INTO source_import_files (
                provider, source_format, source_root, source_path,
                file_size_bytes, file_modified_at_ms, observed_at_ms, is_stale,
                metadata_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8)
            ON CONFLICT(provider, source_root, source_path) DO UPDATE SET
                source_format = excluded.source_format,
                file_size_bytes = excluded.file_size_bytes,
                file_modified_at_ms = excluded.file_modified_at_ms,
                observed_at_ms = excluded.observed_at_ms,
                is_stale = 0,
                indexed_at_ms = CASE
                    WHEN source_import_files.file_size_bytes = excluded.file_size_bytes
                     AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN source_import_files.indexed_at_ms
                    ELSE NULL
                END,
                indexed_file_size_bytes = CASE
                    WHEN source_import_files.file_size_bytes = excluded.file_size_bytes
                     AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN source_import_files.indexed_file_size_bytes
                    ELSE NULL
                END,
                indexed_file_modified_at_ms = CASE
                    WHEN source_import_files.file_size_bytes = excluded.file_size_bytes
                     AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN source_import_files.indexed_file_modified_at_ms
                    ELSE NULL
                END,
                indexed_status = CASE
                    WHEN source_import_files.file_size_bytes = excluded.file_size_bytes
                     AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN source_import_files.indexed_status
                    ELSE 'pending'
                END,
                indexed_error = CASE
                    WHEN source_import_files.file_size_bytes = excluded.file_size_bytes
                     AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN source_import_files.indexed_error
                    ELSE NULL
                END,
                metadata_json = excluded.metadata_json
            WHERE source_import_files.source_format IS NOT excluded.source_format
               OR source_import_files.file_size_bytes != excluded.file_size_bytes
               OR source_import_files.file_modified_at_ms != excluded.file_modified_at_ms
               OR source_import_files.is_stale != 0
               OR source_import_files.metadata_json IS NOT excluded.metadata_json
            "#,
        )?;
        for file in files {
            stmt.execute(params![
                file.provider.as_str(),
                file.source_format.as_str(),
                file.source_root.as_str(),
                file.source_path.as_str(),
                capped_i64(file.file_size_bytes),
                file.file_modified_at_ms,
                file.observed_at_ms,
                serde_json::to_string(&file.metadata)?,
            ])?;
        }
        Ok(())
    }

    pub fn mark_source_import_missing_paths_stale(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        current_paths: &[String],
        observed_at_ms: i64,
    ) -> Result<usize> {
        self.conn.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS temp_source_import_current_paths (source_path TEXT PRIMARY KEY)",
        )?;
        self.conn
            .execute("DELETE FROM temp_source_import_current_paths", [])?;
        {
            let mut stmt = self.conn.prepare(
                "INSERT OR IGNORE INTO temp_source_import_current_paths (source_path) VALUES (?1)",
            )?;
            for source_path in current_paths {
                stmt.execute(params![source_path])?;
            }
        }
        let changed = self.conn.execute(
            r#"
            UPDATE source_import_files
            SET is_stale = 1, observed_at_ms = ?3
            WHERE provider = ?1
              AND source_root = ?2
              AND is_stale = 0
              AND NOT EXISTS (
                  SELECT 1
                  FROM temp_source_import_current_paths AS current
                  WHERE current.source_path = source_import_files.source_path
              )
            "#,
            params![provider.as_str(), source_root, observed_at_ms],
        )?;
        self.conn
            .execute("DELETE FROM temp_source_import_current_paths", [])?;
        Ok(changed)
    }

    pub fn list_pending_source_import_files(
        &self,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<Vec<SourceImportFile>> {
        let mut stmt = self.conn.prepare(
            format!(
                "{} WHERE provider = ?1
                   AND source_root = ?2
                   AND is_stale = 0
                   AND (
                       indexed_status != 'indexed'
                       OR indexed_file_size_bytes IS NULL
                       OR indexed_file_modified_at_ms IS NULL
                       OR indexed_file_size_bytes != file_size_bytes
                       OR indexed_file_modified_at_ms != file_modified_at_ms
                   )
                 ORDER BY source_path",
                source_import_file_select_sql("")
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(
            params![provider.as_str(), source_root],
            source_import_file_from_row,
        )?;
        collect_rows(rows)
    }

    pub fn mark_source_import_file_indexed(
        &self,
        provider: CaptureProvider,
        update: SourceImportFileIndexUpdate<'_>,
    ) -> Result<usize> {
        let changed = self.conn.execute(
            r#"
            UPDATE source_import_files
            SET indexed_at_ms = ?4,
                indexed_file_size_bytes = ?5,
                indexed_file_modified_at_ms = ?6,
                indexed_status = ?7,
                indexed_error = NULL
            WHERE provider = ?1
              AND source_root = ?2
              AND source_path = ?3
              AND is_stale = 0
            "#,
            params![
                provider.as_str(),
                update.source_root,
                update.source_path,
                update.indexed_at_ms,
                capped_i64(update.file_size_bytes),
                update.file_modified_at_ms,
                CatalogIndexedStatus::Indexed.as_str(),
            ],
        )?;
        Ok(changed)
    }

    pub fn mark_source_import_file_failed(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        source_path: &str,
        error: &str,
        indexed_at_ms: i64,
    ) -> Result<usize> {
        let changed = self.conn.execute(
            r#"
            UPDATE source_import_files
            SET indexed_at_ms = ?4,
                indexed_file_size_bytes = NULL,
                indexed_file_modified_at_ms = NULL,
                indexed_status = ?6,
                indexed_error = ?5
            WHERE provider = ?1
              AND source_root = ?2
              AND source_path = ?3
              AND is_stale = 0
            "#,
            params![
                provider.as_str(),
                source_root,
                source_path,
                indexed_at_ms,
                error,
                CatalogIndexedStatus::Failed.as_str(),
            ],
        )?;
        Ok(changed)
    }

    pub fn catalog_session_count(&self) -> Result<usize> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM catalog_sessions WHERE is_stale = 0",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count as usize)
            .map_err(StoreError::from)
    }

    pub fn catalog_session_counts(&self) -> Result<CatalogCounts> {
        let total = self.conn.query_row(
            "SELECT COUNT(*) FROM catalog_sessions WHERE is_stale = 0",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let indexed = self
            .conn
            .query_row(catalog_indexed_count_sql().as_str(), [], |row| {
                row.get::<_, i64>(0)
            })? as usize;
        let stale = self.conn.query_row(
            "SELECT COUNT(*) FROM catalog_sessions WHERE is_stale != 0",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let pending = self.conn.query_row(
            format!(
                "SELECT COUNT(*) FROM catalog_sessions WHERE is_stale = 0 AND {}",
                catalog_pending_import_condition_sql("catalog_sessions")
            )
            .as_str(),
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let failed = self.conn.query_row(
            "SELECT COUNT(*) FROM catalog_sessions WHERE is_stale = 0 AND indexed_status = 'failed'",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        Ok(CatalogCounts {
            total,
            indexed,
            stale,
            pending,
            failed,
        })
    }

    pub fn upsert_session(&self, session: &Session) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO sessions
            (
                id, history_record_id, parent_session_id, root_session_id, capture_source_id,
                provider, external_session_id, external_agent_id, agent_type, role_hint,
                is_primary, status, fidelity, transcript_blob_id, started_at_ms, ended_at_ms,
                created_at_ms, updated_at_ms, visibility, sync_state, sync_version,
                deleted_at_ms, metadata_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
            ON CONFLICT(id) DO UPDATE SET
                history_record_id = excluded.history_record_id,
                parent_session_id = excluded.parent_session_id,
                root_session_id = excluded.root_session_id,
                capture_source_id = excluded.capture_source_id,
                provider = excluded.provider,
                external_session_id = excluded.external_session_id,
                external_agent_id = excluded.external_agent_id,
                agent_type = excluded.agent_type,
                role_hint = excluded.role_hint,
                is_primary = excluded.is_primary,
                status = excluded.status,
                fidelity = excluded.fidelity,
                transcript_blob_id = excluded.transcript_blob_id,
                started_at_ms = excluded.started_at_ms,
                ended_at_ms = excluded.ended_at_ms,
                updated_at_ms = excluded.updated_at_ms,
                visibility = excluded.visibility,
                sync_state = excluded.sync_state,
                sync_version = excluded.sync_version,
                deleted_at_ms = excluded.deleted_at_ms,
                metadata_json = excluded.metadata_json
            "#,
            params![
                session.id.to_string(),
                optional_uuid_string(session.history_record_id),
                optional_uuid_string(session.parent_session_id),
                optional_uuid_string(session.root_session_id),
                optional_uuid_string(session.capture_source_id),
                session.provider.as_str(),
                session.external_session_id.as_deref(),
                session.external_agent_id.as_deref(),
                session.agent_type.as_str(),
                session.role_hint.as_deref(),
                session.is_primary as i64,
                session.status.as_str(),
                session.sync.fidelity.as_str(),
                optional_uuid_string(session.transcript_blob_id),
                timestamp_ms(session.started_at),
                optional_timestamp_ms(session.ended_at),
                timestamp_ms(session.timestamps.created_at),
                timestamp_ms(session.timestamps.updated_at),
                session.sync.visibility.as_str(),
                session.sync.sync_state.as_str(),
                session.sync.sync_version as i64,
                optional_timestamp_ms(session.sync.deleted_at),
                serde_json::to_string(&session.sync.metadata)?,
            ],
        )?;
        Ok(())
    }

    pub fn get_session(&self, id: Uuid) -> Result<Session> {
        self.conn
            .query_row(
                session_select_sql("WHERE id = ?1").as_str(),
                params![id.to_string()],
                session_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    pub fn sessions_by_id_prefix(&self, prefix: &str) -> Result<Vec<Session>> {
        let mut stmt = self
            .conn
            .prepare(session_select_sql("WHERE id LIKE ?1 ORDER BY id LIMIT 2").as_str())?;
        let rows = stmt.query_map(params![format!("{prefix}%")], session_from_row)?;
        collect_rows(rows)
    }

    pub fn session_by_external_session(
        &self,
        provider: CaptureProvider,
        external_session_id: &str,
    ) -> Result<Option<Session>> {
        self.conn
            .query_row(
                session_select_sql(
                    "WHERE provider = ?1 AND external_session_id = ?2 ORDER BY started_at_ms DESC LIMIT 1",
                )
                .as_str(),
                params![provider.as_str(), external_session_id],
                session_from_row,
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn sessions_by_external_session_limited(
        &self,
        provider: CaptureProvider,
        external_session_id: &str,
        limit: usize,
    ) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            session_select_sql(
                "WHERE provider = ?1 AND external_session_id = ?2 ORDER BY started_at_ms DESC LIMIT ?3",
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(
            params![
                provider.as_str(),
                external_session_id,
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            session_from_row,
        )?;
        collect_rows(rows)
    }

    pub fn sessions_for_record(&self, record_id: Uuid) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            session_select_sql("WHERE history_record_id = ?1 ORDER BY started_at_ms, id").as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], session_from_row)?;
        collect_rows(rows)
    }

    pub fn assign_session_to_record(&self, session_id: Uuid, record_id: Uuid) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET history_record_id = ?1 WHERE id = ?2",
            params![record_id.to_string(), session_id.to_string()],
        )?;
        self.conn.execute(
            "UPDATE events SET history_record_id = ?1 WHERE session_id = ?2",
            params![record_id.to_string(), session_id.to_string()],
        )?;
        self.conn.execute(
            "UPDATE runs SET history_record_id = ?1 WHERE session_id = ?2",
            params![record_id.to_string(), session_id.to_string()],
        )?;
        Ok(())
    }

    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self
            .conn
            .prepare(session_select_sql("ORDER BY started_at_ms, id").as_str())?;
        let rows = stmt.query_map([], session_from_row)?;
        collect_rows(rows)
    }

    pub fn indexed_history_item_count(&self) -> Result<usize> {
        Ok(self.indexed_history_counts()?.items())
    }

    pub fn indexed_history_counts(&self) -> Result<IndexedHistoryCounts> {
        let sessions: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
        let events: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))?;
        Ok(IndexedHistoryCounts {
            sessions: sessions as usize,
            events: events as usize,
        })
    }

    pub fn upsert_session_edge(&self, edge: &SessionEdge) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO session_edges
            (id, from_session_id, to_session_id, edge_type, confidence, source_id, created_at_ms, updated_at_ms, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            ON CONFLICT(id) DO UPDATE SET
                from_session_id = excluded.from_session_id,
                to_session_id = excluded.to_session_id,
                edge_type = excluded.edge_type,
                confidence = excluded.confidence,
                source_id = excluded.source_id,
                updated_at_ms = excluded.updated_at_ms,
                visibility = excluded.visibility,
                fidelity = excluded.fidelity,
                sync_state = excluded.sync_state,
                sync_version = excluded.sync_version,
                deleted_at_ms = excluded.deleted_at_ms,
                metadata_json = excluded.metadata_json
            "#,
            params![
                edge.id.to_string(),
                edge.from_session_id.to_string(),
                edge.to_session_id.to_string(),
                edge.edge_type.as_str(),
                edge.confidence.as_str(),
                optional_uuid_string(edge.source_id),
                timestamp_ms(edge.timestamps.created_at),
                timestamp_ms(edge.timestamps.updated_at),
                edge.sync.visibility.as_str(),
                edge.sync.fidelity.as_str(),
                edge.sync.sync_state.as_str(),
                edge.sync.sync_version as i64,
                optional_timestamp_ms(edge.sync.deleted_at),
                serde_json::to_string(&edge.sync.metadata)?,
            ],
        )?;
        Ok(())
    }

    pub fn session_edge_exists(&self, edge_id: Uuid) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM session_edges WHERE id = ?1",
                params![edge_id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn upsert_run(&self, run: &Run) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO runs
            (id, history_record_id, session_id, run_type, status, started_at_ms, ended_at_ms, exit_code, cwd, command_preview, input_blob_id, output_blob_id, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
            ON CONFLICT(id) DO UPDATE SET
                history_record_id = excluded.history_record_id,
                session_id = excluded.session_id,
                run_type = excluded.run_type,
                status = excluded.status,
                started_at_ms = excluded.started_at_ms,
                ended_at_ms = excluded.ended_at_ms,
                exit_code = excluded.exit_code,
                cwd = excluded.cwd,
                command_preview = excluded.command_preview,
                input_blob_id = excluded.input_blob_id,
                output_blob_id = excluded.output_blob_id,
                updated_at_ms = excluded.updated_at_ms,
                source_id = excluded.source_id,
                visibility = excluded.visibility,
                fidelity = excluded.fidelity,
                sync_state = excluded.sync_state,
                sync_version = excluded.sync_version,
                deleted_at_ms = excluded.deleted_at_ms,
                metadata_json = excluded.metadata_json
            "#,
            params![
                run.id.to_string(),
                optional_uuid_string(run.history_record_id),
                optional_uuid_string(run.session_id),
                run.run_type.as_str(),
                run.status.as_str(),
                timestamp_ms(run.started_at),
                optional_timestamp_ms(run.ended_at),
                run.exit_code,
                run.cwd.as_deref(),
                run.command_preview.as_deref(),
                optional_uuid_string(run.input_blob_id),
                optional_uuid_string(run.output_blob_id),
                timestamp_ms(run.timestamps.created_at),
                timestamp_ms(run.timestamps.updated_at),
                optional_uuid_string(run.source_id),
                run.sync.visibility.as_str(),
                run.sync.fidelity.as_str(),
                run.sync.sync_state.as_str(),
                run.sync.sync_version as i64,
                optional_timestamp_ms(run.sync.deleted_at),
                serde_json::to_string(&run.sync.metadata)?,
            ],
        )?;
        Ok(())
    }

    pub fn insert_run_if_absent(&self, run: &Run) -> Result<bool> {
        let changed = self
            .conn
            .prepare_cached(
                r#"
                INSERT OR IGNORE INTO runs
                (id, history_record_id, session_id, run_type, status, started_at_ms, ended_at_ms, exit_code, cwd, command_preview, input_blob_id, output_blob_id, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
                "#,
            )?
            .execute(params![
                run.id.to_string(),
                optional_uuid_string(run.history_record_id),
                optional_uuid_string(run.session_id),
                run.run_type.as_str(),
                run.status.as_str(),
                timestamp_ms(run.started_at),
                optional_timestamp_ms(run.ended_at),
                run.exit_code,
                run.cwd.as_deref(),
                run.command_preview.as_deref(),
                optional_uuid_string(run.input_blob_id),
                optional_uuid_string(run.output_blob_id),
                timestamp_ms(run.timestamps.created_at),
                timestamp_ms(run.timestamps.updated_at),
                optional_uuid_string(run.source_id),
                run.sync.visibility.as_str(),
                run.sync.fidelity.as_str(),
                run.sync.sync_state.as_str(),
                run.sync.sync_version as i64,
                optional_timestamp_ms(run.sync.deleted_at),
                serde_json::to_string(&run.sync.metadata)?,
            ])?;
        Ok(changed > 0)
    }

    pub fn get_run(&self, id: Uuid) -> Result<Run> {
        self.conn
            .query_row(
                run_select_sql("WHERE id = ?1").as_str(),
                params![id.to_string()],
                run_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    pub fn runs_for_session(&self, session_id: Uuid) -> Result<Vec<Run>> {
        let mut stmt = self
            .conn
            .prepare(run_select_sql("WHERE session_id = ?1 ORDER BY started_at_ms, id").as_str())?;
        let rows = stmt.query_map(params![session_id.to_string()], run_from_row)?;
        collect_rows(rows)
    }

    pub fn runs_for_record(&self, record_id: Uuid) -> Result<Vec<Run>> {
        let mut stmt = self.conn.prepare(
            run_select_sql(
                r#"
                WHERE history_record_id = ?1
                   OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                ORDER BY started_at_ms, id
                "#,
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], run_from_row)?;
        collect_rows(rows)
    }

    fn list_runs(&self) -> Result<Vec<Run>> {
        let mut stmt = self
            .conn
            .prepare(run_select_sql("ORDER BY started_at_ms, id").as_str())?;
        let rows = stmt.query_map([], run_from_row)?;
        collect_rows(rows)
    }

    pub fn provider_event_dedupe_key(
        provider: CaptureProvider,
        external_session_id: &str,
        provider_index: u64,
        payload_hash: &str,
    ) -> String {
        format!(
            "provider:{}:{}:{}:{}",
            provider.as_str(),
            external_session_id,
            provider_index,
            payload_hash
        )
    }

    pub fn provider_source_event_dedupe_key(
        source_id: Uuid,
        provider_index: u64,
        payload_hash: &str,
    ) -> String {
        format!("provider-source:{source_id}:{provider_index}:{payload_hash}")
    }

    pub fn upsert_event(&self, event: &Event) -> Result<Uuid> {
        let event_id = if let Some(dedupe_key) = &event.dedupe_key {
            reject_provider_event_hash_conflict(&self.conn, dedupe_key)?;
            if let Some(existing_id) = self
                .conn
                .query_row(
                    "SELECT id FROM events WHERE dedupe_key = ?1",
                    params![dedupe_key],
                    |row| parse_uuid(row.get::<_, String>(0)?),
                )
                .optional()?
            {
                return Ok(existing_id);
            }
            event.id
        } else {
            event.id
        };

        self.conn.execute(
            r#"
            INSERT INTO events
            (id, seq, history_record_id, session_id, run_id, event_type, role, occurred_at_ms, capture_source_id, payload_json, payload_blob_id, dedupe_key, visibility, redaction_state, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
            ON CONFLICT(id) DO UPDATE SET
                seq = excluded.seq,
                history_record_id = excluded.history_record_id,
                session_id = excluded.session_id,
                run_id = excluded.run_id,
                event_type = excluded.event_type,
                role = excluded.role,
                occurred_at_ms = excluded.occurred_at_ms,
                capture_source_id = excluded.capture_source_id,
                payload_json = excluded.payload_json,
                payload_blob_id = excluded.payload_blob_id,
                dedupe_key = excluded.dedupe_key,
                visibility = excluded.visibility,
                redaction_state = excluded.redaction_state,
                fidelity = excluded.fidelity,
                sync_state = excluded.sync_state,
                sync_version = excluded.sync_version,
                deleted_at_ms = excluded.deleted_at_ms,
                metadata_json = excluded.metadata_json
            "#,
            params![
                event_id.to_string(),
                event.seq as i64,
                optional_uuid_string(event.history_record_id),
                optional_uuid_string(event.session_id),
                optional_uuid_string(event.run_id),
                event.event_type.as_str(),
                event.role.map(|role| role.as_str()),
                timestamp_ms(event.occurred_at),
                optional_uuid_string(event.capture_source_id),
                serde_json::to_string(&event.payload)?,
                optional_uuid_string(event.payload_blob_id),
                event.dedupe_key.as_deref(),
                event.sync.visibility.as_str(),
                event.redaction_state.as_str(),
                event.sync.fidelity.as_str(),
                event.sync.sync_state.as_str(),
                event.sync.sync_version as i64,
                optional_timestamp_ms(event.sync.deleted_at),
                serde_json::to_string(&event.sync.metadata)?,
            ],
        )?;
        upsert_event_search_projection_for_event(&self.conn, event_id, event)?;
        if let Some(dedupe_key) = &event.dedupe_key {
            return self.event_id_by_dedupe_key(dedupe_key);
        }
        Ok(event_id)
    }

    pub fn insert_event_if_absent(&self, event: &Event) -> Result<bool> {
        let changed = self
            .conn
            .prepare_cached(
                r#"
                INSERT OR IGNORE INTO events
                (id, seq, history_record_id, session_id, run_id, event_type, role, occurred_at_ms, capture_source_id, payload_json, payload_blob_id, dedupe_key, visibility, redaction_state, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
                "#,
            )?
            .execute(params![
                event.id.to_string(),
                event.seq as i64,
                optional_uuid_string(event.history_record_id),
                optional_uuid_string(event.session_id),
                optional_uuid_string(event.run_id),
                event.event_type.as_str(),
                event.role.map(|role| role.as_str()),
                timestamp_ms(event.occurred_at),
                optional_uuid_string(event.capture_source_id),
                serde_json::to_string(&event.payload)?,
                optional_uuid_string(event.payload_blob_id),
                event.dedupe_key.as_deref(),
                event.sync.visibility.as_str(),
                event.redaction_state.as_str(),
                event.sync.fidelity.as_str(),
                event.sync.sync_state.as_str(),
                event.sync.sync_version as i64,
                optional_timestamp_ms(event.sync.deleted_at),
                serde_json::to_string(&event.sync.metadata)?,
            ])?;
        if changed == 0 {
            if let Some(dedupe_key) = &event.dedupe_key {
                reject_provider_event_hash_conflict(&self.conn, dedupe_key)?;
            }
        }
        if changed > 0 {
            insert_event_search_projection_for_event(&self.conn, event)?;
        }
        Ok(changed > 0)
    }

    pub fn event_id_by_dedupe_key(&self, dedupe_key: &str) -> Result<Uuid> {
        self.conn
            .query_row(
                "SELECT id FROM events WHERE dedupe_key = ?1",
                params![dedupe_key],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .map_err(StoreError::from)
    }

    pub fn event_id_by_seq(&self, seq: u64) -> Result<Uuid> {
        self.conn
            .query_row(
                "SELECT id FROM events WHERE seq = ?1",
                params![seq as i64],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .map_err(StoreError::from)
    }

    pub fn get_event(&self, id: Uuid) -> Result<Event> {
        self.conn
            .query_row(
                event_select_sql("WHERE id = ?1").as_str(),
                params![id.to_string()],
                event_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    pub fn events_by_id_prefix(&self, prefix: &str) -> Result<Vec<Event>> {
        let mut stmt = self
            .conn
            .prepare(event_select_sql("WHERE id LIKE ?1 ORDER BY id LIMIT 2").as_str())?;
        let rows = stmt.query_map(params![format!("{prefix}%")], event_from_row)?;
        collect_rows(rows)
    }

    pub fn events_for_session(&self, session_id: Uuid) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            event_select_sql("WHERE session_id = ?1 ORDER BY seq, occurred_at_ms").as_str(),
        )?;
        let rows = stmt.query_map(params![session_id.to_string()], event_from_row)?;
        collect_rows(rows)
    }

    pub fn events_for_session_limited(&self, session_id: Uuid, limit: usize) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            event_select_sql("WHERE session_id = ?1 ORDER BY seq, occurred_at_ms LIMIT ?2")
                .as_str(),
        )?;
        let rows = stmt.query_map(
            params![
                session_id.to_string(),
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            event_from_row,
        )?;
        collect_rows(rows)
    }

    pub fn events_for_session_window(
        &self,
        event: &Event,
        before: usize,
        after: usize,
    ) -> Result<Vec<Event>> {
        let Some(session_id) = event.session_id else {
            return Ok(vec![event.clone()]);
        };
        let event_seq = i64::try_from(event.seq).unwrap_or(i64::MAX);
        let mut events = if before == 0 {
            Vec::new()
        } else {
            let mut stmt = self.conn.prepare(
                event_select_sql(
                    "WHERE session_id = ?1 AND seq < ?2 ORDER BY seq DESC, occurred_at_ms DESC LIMIT ?3",
                )
                .as_str(),
            )?;
            let rows = stmt.query_map(
                params![
                    session_id.to_string(),
                    event_seq,
                    i64::try_from(before).unwrap_or(i64::MAX)
                ],
                event_from_row,
            )?;
            let mut rows = collect_rows(rows)?;
            rows.reverse();
            rows
        };
        events.push(event.clone());
        if after > 0 {
            let mut stmt = self.conn.prepare(
                event_select_sql(
                    "WHERE session_id = ?1 AND seq > ?2 ORDER BY seq, occurred_at_ms LIMIT ?3",
                )
                .as_str(),
            )?;
            let rows = stmt.query_map(
                params![
                    session_id.to_string(),
                    event_seq,
                    i64::try_from(after).unwrap_or(i64::MAX)
                ],
                event_from_row,
            )?;
            events.extend(collect_rows(rows)?);
        }
        Ok(events)
    }

    pub fn events_for_record(&self, record_id: Uuid) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            event_select_sql(
                r#"
                WHERE history_record_id = ?1
                   OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                   OR run_id IN (
                        SELECT id FROM runs
                        WHERE history_record_id = ?1
                           OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                   )
                ORDER BY seq, occurred_at_ms
                "#,
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], event_from_row)?;
        collect_rows(rows)
    }

    fn list_events(&self) -> Result<Vec<Event>> {
        let mut stmt = self
            .conn
            .prepare(event_select_sql("ORDER BY seq, occurred_at_ms, id").as_str())?;
        let rows = stmt.query_map([], event_from_row)?;
        collect_rows(rows)
    }

    pub fn upsert_artifact(&self, artifact: &Artifact) -> Result<Uuid> {
        self.conn.execute(
            r#"
            INSERT INTO artifacts
            (id, kind, blob_hash, blob_path, byte_size, media_type, preview_text, redaction_state, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
            ON CONFLICT DO UPDATE SET
                blob_path = excluded.blob_path,
                byte_size = excluded.byte_size,
                media_type = excluded.media_type,
                preview_text = excluded.preview_text,
                redaction_state = excluded.redaction_state,
                updated_at_ms = excluded.updated_at_ms,
                source_id = excluded.source_id,
                visibility = excluded.visibility,
                fidelity = excluded.fidelity,
                sync_state = excluded.sync_state,
                sync_version = excluded.sync_version,
                deleted_at_ms = excluded.deleted_at_ms,
                metadata_json = excluded.metadata_json
            "#,
            params![
                artifact.id.to_string(),
                artifact.kind.as_str(),
                artifact.blob_hash.as_str(),
                artifact.blob_path.as_str(),
                artifact.byte_size as i64,
                artifact.media_type.as_deref(),
                artifact.preview_text.as_deref(),
                artifact.redaction_state.as_str(),
                timestamp_ms(artifact.timestamps.created_at),
                timestamp_ms(artifact.timestamps.updated_at),
                optional_uuid_string(artifact.source_id),
                artifact.sync.visibility.as_str(),
                artifact.sync.fidelity.as_str(),
                artifact.sync.sync_state.as_str(),
                artifact.sync.sync_version as i64,
                optional_timestamp_ms(artifact.sync.deleted_at),
                serde_json::to_string(&artifact.sync.metadata)?,
            ],
        )?;
        self.conn
            .query_row(
                "SELECT id FROM artifacts WHERE blob_hash = ?1 AND kind = ?2",
                params![artifact.blob_hash.as_str(), artifact.kind.as_str()],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .map_err(StoreError::from)
    }

    fn list_artifacts(&self) -> Result<Vec<Artifact>> {
        let mut stmt = self
            .conn
            .prepare(artifact_select_sql("ORDER BY updated_at_ms, id").as_str())?;
        let rows = stmt.query_map([], artifact_from_row)?;
        collect_rows(rows)
    }

    pub fn upsert_vcs_workspace(&self, workspace: &VcsWorkspace) -> Result<Uuid> {
        self.conn.execute(
            r#"
            INSERT INTO vcs_workspaces
            (id, kind, root_path, repo_fingerprint, primary_remote_url_normalized, host, owner, name, monorepo_subpath, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
            ON CONFLICT(kind, repo_fingerprint) DO UPDATE SET
                root_path = excluded.root_path,
                primary_remote_url_normalized = excluded.primary_remote_url_normalized,
                host = excluded.host,
                owner = excluded.owner,
                name = excluded.name,
                monorepo_subpath = excluded.monorepo_subpath,
                updated_at_ms = excluded.updated_at_ms,
                source_id = excluded.source_id,
                visibility = excluded.visibility,
                fidelity = excluded.fidelity,
                sync_state = excluded.sync_state,
                sync_version = excluded.sync_version,
                deleted_at_ms = excluded.deleted_at_ms,
                metadata_json = excluded.metadata_json
            "#,
            params![
                workspace.id.to_string(),
                workspace.kind.as_str(),
                workspace.root_path.as_str(),
                workspace.repo_fingerprint.as_str(),
                workspace.primary_remote_url_normalized.as_deref(),
                workspace.host.as_str(),
                workspace.owner.as_deref(),
                workspace.name.as_deref(),
                workspace.monorepo_subpath.as_deref(),
                timestamp_ms(workspace.timestamps.created_at),
                timestamp_ms(workspace.timestamps.updated_at),
                optional_uuid_string(workspace.source_id),
                workspace.sync.visibility.as_str(),
                workspace.sync.fidelity.as_str(),
                workspace.sync.sync_state.as_str(),
                workspace.sync.sync_version as i64,
                optional_timestamp_ms(workspace.sync.deleted_at),
                serde_json::to_string(&workspace.sync.metadata)?,
            ],
        )?;
        self.conn
            .query_row(
                "SELECT id FROM vcs_workspaces WHERE kind = ?1 AND repo_fingerprint = ?2",
                params![workspace.kind.as_str(), workspace.repo_fingerprint.as_str()],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .map_err(StoreError::from)
    }

    pub fn get_or_create_local_device(&self) -> Result<LocalDeviceIdentity> {
        if let Some(device) = self.local_device()? {
            return Ok(device);
        }
        let now = utc_now();
        let device = LocalDeviceIdentity {
            id: new_id(),
            stable_device_id: format!("ctx-device-{}", new_id().simple()),
            created_at: now,
            updated_at: now,
        };
        self.conn.execute(
            r#"
            INSERT INTO local_devices
            (id, stable_device_id, created_at_ms, updated_at_ms, metadata_json)
            VALUES (?1, ?2, ?3, ?3, '{}')
            "#,
            params![
                device.id.to_string(),
                device.stable_device_id.as_str(),
                timestamp_ms(now),
            ],
        )?;
        Ok(device)
    }

    pub fn register_local_workspace(
        &self,
        root_path: impl AsRef<Path>,
        repo_fingerprint: &str,
        vcs_workspace_id: Option<Uuid>,
    ) -> Result<LocalWorkspaceIdentity> {
        let device = self.get_or_create_local_device()?;
        let root = root_path.as_ref();
        let root_path_hash = sha256_hex(root.display().to_string().as_bytes());
        let display_root = root.display().to_string();
        let now = utc_now();
        let id = new_id();
        self.conn.execute(
            r#"
            INSERT INTO local_workspaces
            (
                id, device_id, vcs_workspace_id, repo_fingerprint, root_path_hash,
                display_root, created_at_ms, updated_at_ms, metadata_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, '{}')
            ON CONFLICT(device_id, repo_fingerprint, root_path_hash) DO UPDATE SET
                vcs_workspace_id = COALESCE(excluded.vcs_workspace_id, local_workspaces.vcs_workspace_id),
                display_root = excluded.display_root,
                updated_at_ms = excluded.updated_at_ms
            "#,
            params![
                id.to_string(),
                device.id.to_string(),
                optional_uuid_string(vcs_workspace_id),
                repo_fingerprint,
                root_path_hash,
                display_root,
                timestamp_ms(now),
            ],
        )?;
        self.conn
            .query_row(
                r#"
                SELECT id, device_id, vcs_workspace_id, repo_fingerprint, root_path_hash,
                       display_root, created_at_ms, updated_at_ms
                FROM local_workspaces
                WHERE device_id = ?1 AND repo_fingerprint = ?2 AND root_path_hash = ?3
                "#,
                params![device.id.to_string(), repo_fingerprint, root_path_hash],
                local_workspace_from_row,
            )
            .map_err(StoreError::from)
    }

    pub fn local_device(&self) -> Result<Option<LocalDeviceIdentity>> {
        self.conn
            .query_row(
                "SELECT id, stable_device_id, created_at_ms, updated_at_ms FROM local_devices ORDER BY created_at_ms, id LIMIT 1",
                [],
                local_device_from_row,
            )
            .optional()
            .map_err(StoreError::from)
    }

    fn list_vcs_workspaces(&self) -> Result<Vec<VcsWorkspace>> {
        let mut stmt = self
            .conn
            .prepare(vcs_workspace_select_sql("ORDER BY updated_at_ms, id").as_str())?;
        let rows = stmt.query_map([], vcs_workspace_from_row)?;
        collect_rows(rows)
    }

    pub fn upsert_vcs_change(&self, change: &VcsChange) -> Result<Uuid> {
        self.conn.execute(
            r#"
            INSERT INTO vcs_changes
            (id, vcs_workspace_id, kind, change_id, parent_change_ids_json, branch_or_bookmark, tree_hash, author_time_ms, confidence, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
            ON CONFLICT(vcs_workspace_id, kind, change_id) DO UPDATE SET
                parent_change_ids_json = excluded.parent_change_ids_json,
                branch_or_bookmark = excluded.branch_or_bookmark,
                tree_hash = excluded.tree_hash,
                author_time_ms = excluded.author_time_ms,
                confidence = excluded.confidence,
                updated_at_ms = excluded.updated_at_ms,
                source_id = excluded.source_id,
                visibility = excluded.visibility,
                fidelity = excluded.fidelity,
                sync_state = excluded.sync_state,
                sync_version = excluded.sync_version,
                deleted_at_ms = excluded.deleted_at_ms,
                metadata_json = excluded.metadata_json
            "#,
            params![
                change.id.to_string(),
                change.vcs_workspace_id.to_string(),
                change.kind.as_str(),
                change.change_id.as_str(),
                serde_json::to_string(&change.parent_change_ids)?,
                change.branch_or_bookmark.as_deref(),
                change.tree_hash.as_deref(),
                optional_timestamp_ms(change.author_time),
                change.confidence.as_str(),
                timestamp_ms(change.timestamps.created_at),
                timestamp_ms(change.timestamps.updated_at),
                optional_uuid_string(change.source_id),
                change.sync.visibility.as_str(),
                change.sync.fidelity.as_str(),
                change.sync.sync_state.as_str(),
                change.sync.sync_version as i64,
                optional_timestamp_ms(change.sync.deleted_at),
                serde_json::to_string(&change.sync.metadata)?,
            ],
        )?;
        self.conn
            .query_row(
                "SELECT id FROM vcs_changes WHERE vcs_workspace_id = ?1 AND kind = ?2 AND change_id = ?3",
                params![change.vcs_workspace_id.to_string(), change.kind.as_str(), change.change_id.as_str()],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .map_err(StoreError::from)
    }

    fn list_vcs_changes(&self) -> Result<Vec<VcsChange>> {
        let mut stmt = self
            .conn
            .prepare(vcs_change_select_sql("ORDER BY updated_at_ms, id").as_str())?;
        let rows = stmt.query_map([], vcs_change_from_row)?;
        collect_rows(rows)
    }

    pub fn upsert_summary(&self, summary: &Summary) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO summaries
            (id, history_record_id, session_id, kind, model_or_source, text, citations_json, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
            ON CONFLICT(id) DO UPDATE SET
                history_record_id = excluded.history_record_id,
                session_id = excluded.session_id,
                kind = excluded.kind,
                model_or_source = excluded.model_or_source,
                text = excluded.text,
                citations_json = excluded.citations_json,
                updated_at_ms = excluded.updated_at_ms,
                source_id = excluded.source_id,
                visibility = excluded.visibility,
                fidelity = excluded.fidelity,
                sync_state = excluded.sync_state,
                sync_version = excluded.sync_version,
                deleted_at_ms = excluded.deleted_at_ms,
                metadata_json = excluded.metadata_json
            "#,
            params![
                summary.id.to_string(),
                optional_uuid_string(summary.history_record_id),
                optional_uuid_string(summary.session_id),
                summary.kind.as_str(),
                summary.model_or_source.as_deref(),
                summary.text.as_str(),
                serde_json::to_string(&summary.citations)?,
                timestamp_ms(summary.timestamps.created_at),
                timestamp_ms(summary.timestamps.updated_at),
                optional_uuid_string(summary.source_id),
                summary.sync.visibility.as_str(),
                summary.sync.fidelity.as_str(),
                summary.sync.sync_state.as_str(),
                summary.sync.sync_version as i64,
                optional_timestamp_ms(summary.sync.deleted_at),
                serde_json::to_string(&summary.sync.metadata)?,
            ],
        )?;
        Ok(())
    }

    fn list_summaries(&self) -> Result<Vec<Summary>> {
        let mut stmt = self
            .conn
            .prepare(summary_select_sql("ORDER BY updated_at_ms, id").as_str())?;
        let rows = stmt.query_map([], summary_from_row)?;
        collect_rows(rows)
    }

    pub fn upsert_file_touched(&self, file: &FileTouched) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO files_touched
            (id, history_record_id, run_id, event_id, vcs_workspace_id, path, change_kind, old_path, line_count_delta, confidence, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
            ON CONFLICT(id) DO UPDATE SET
                history_record_id = excluded.history_record_id,
                run_id = excluded.run_id,
                event_id = excluded.event_id,
                vcs_workspace_id = excluded.vcs_workspace_id,
                path = excluded.path,
                change_kind = excluded.change_kind,
                old_path = excluded.old_path,
                line_count_delta = excluded.line_count_delta,
                confidence = excluded.confidence,
                updated_at_ms = excluded.updated_at_ms,
                source_id = excluded.source_id,
                visibility = excluded.visibility,
                fidelity = excluded.fidelity,
                sync_state = excluded.sync_state,
                sync_version = excluded.sync_version,
                deleted_at_ms = excluded.deleted_at_ms,
                metadata_json = excluded.metadata_json
            "#,
            params![
                file.id.to_string(),
                optional_uuid_string(file.history_record_id),
                optional_uuid_string(file.run_id),
                optional_uuid_string(file.event_id),
                optional_uuid_string(file.vcs_workspace_id),
                file.path.as_str(),
                file.change_kind.map(|kind| kind.as_str()),
                file.old_path.as_deref(),
                file.line_count_delta,
                file.confidence.as_str(),
                timestamp_ms(file.timestamps.created_at),
                timestamp_ms(file.timestamps.updated_at),
                optional_uuid_string(file.source_id),
                file.sync.visibility.as_str(),
                file.sync.fidelity.as_str(),
                file.sync.sync_state.as_str(),
                file.sync.sync_version as i64,
                optional_timestamp_ms(file.sync.deleted_at),
                serde_json::to_string(&file.sync.metadata)?,
            ],
        )?;
        Ok(())
    }

    pub fn file_touched_exists(&self, id: Uuid) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM files_touched WHERE id = ?1",
                params![id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    fn list_files_touched(&self) -> Result<Vec<FileTouched>> {
        let mut stmt = self
            .conn
            .prepare(file_touched_select_sql("ORDER BY updated_at_ms, id").as_str())?;
        let rows = stmt.query_map([], file_touched_from_row)?;
        collect_rows(rows)
    }

    pub fn artifacts_for_record(&self, record_id: Uuid) -> Result<Vec<Artifact>> {
        let mut stmt = self.conn.prepare(
            artifact_select_sql(
                r#"
                WHERE id IN (
                    SELECT transcript_blob_id
                    FROM sessions
                    WHERE history_record_id = ?1 AND transcript_blob_id IS NOT NULL
                    UNION
                    SELECT input_blob_id
                    FROM runs
                    WHERE (history_record_id = ?1
                       OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1))
                       AND input_blob_id IS NOT NULL
                    UNION
                    SELECT output_blob_id
                    FROM runs
                    WHERE (history_record_id = ?1
                       OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1))
                       AND output_blob_id IS NOT NULL
                    UNION
                    SELECT payload_blob_id
                    FROM events
                    WHERE (history_record_id = ?1
                       OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1))
                       AND payload_blob_id IS NOT NULL
                    UNION
                    SELECT target_id
                    FROM history_record_links
                    WHERE history_record_id = ?1 AND target_type = 'artifact'
                )
                ORDER BY updated_at_ms DESC, id
                "#,
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], artifact_from_row)?;
        collect_rows(rows)
    }

    pub fn vcs_changes_for_record(&self, record_id: Uuid) -> Result<Vec<VcsChange>> {
        let mut stmt = self.conn.prepare(
            vcs_change_select_sql(
                r#"
                WHERE id IN (
                    SELECT target_id
                    FROM history_record_links
                    WHERE history_record_id = ?1 AND target_type = 'vcs_change'
                )
                ORDER BY updated_at_ms DESC, id
                "#,
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], vcs_change_from_row)?;
        collect_rows(rows)
    }

    pub fn summaries_for_record(&self, record_id: Uuid) -> Result<Vec<Summary>> {
        let mut stmt = self.conn.prepare(
            summary_select_sql(
                r#"
                WHERE history_record_id = ?1
                   OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                ORDER BY updated_at_ms DESC, id
                "#,
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], summary_from_row)?;
        collect_rows(rows)
    }

    pub fn files_touched_for_record(&self, record_id: Uuid) -> Result<Vec<FileTouched>> {
        let mut stmt = self.conn.prepare(
            file_touched_select_sql(
                r#"
                WHERE history_record_id = ?1
                   OR run_id IN (
                        SELECT id FROM runs
                        WHERE history_record_id = ?1
                           OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                   )
                   OR event_id IN (
                        SELECT id FROM events
                        WHERE history_record_id = ?1
                           OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                   )
                ORDER BY updated_at_ms DESC, id
                "#,
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], file_touched_from_row)?;
        collect_rows(rows)
    }

    pub fn files_touched_for_record_matching(
        &self,
        record_id: Uuid,
        file: &str,
    ) -> Result<Vec<FileTouched>> {
        let Some((exact, suffix)) = file_touch_match_values(file) else {
            return Ok(Vec::new());
        };
        let mut stmt = self.conn.prepare(
            file_touched_select_sql(
                r#"
                WHERE (
                    history_record_id = ?1
                    OR run_id IN (
                         SELECT id FROM runs
                         WHERE history_record_id = ?1
                            OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                    )
                    OR event_id IN (
                         SELECT id FROM events
                         WHERE history_record_id = ?1
                            OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                    )
                )
                AND (
                    path = ?2
                    OR old_path = ?2
                    OR path LIKE ?3 ESCAPE '\'
                    OR old_path LIKE ?3 ESCAPE '\'
                )
                ORDER BY updated_at_ms DESC, id
                "#,
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(
            params![record_id.to_string(), exact, suffix],
            file_touched_from_row,
        )?;
        collect_rows(rows)
    }

    pub fn file_touch_scope(&self, file: &str) -> Result<FileTouchScope> {
        let Some((exact, suffix)) = file_touch_match_values(file) else {
            return Ok(FileTouchScope::default());
        };
        let mut scope = FileTouchScope::default();
        let mut stmt = self.conn.prepare(
            r#"
            SELECT
                COALESCE(
                    ft.history_record_id,
                    e.history_record_id,
                    r.history_record_id,
                    event_session.history_record_id,
                    run_session.history_record_id,
                    source_session.history_record_id
                ),
                COALESCE(e.session_id, r.session_id, source_session.id),
                ft.run_id,
                ft.event_id,
                ft.source_id
            FROM files_touched ft
            LEFT JOIN events e ON e.id = ft.event_id
            LEFT JOIN runs r ON r.id = ft.run_id
            LEFT JOIN sessions event_session ON event_session.id = e.session_id
            LEFT JOIN sessions run_session ON run_session.id = r.session_id
            LEFT JOIN sessions source_session ON source_session.capture_source_id = ft.source_id
            WHERE ft.path = ?1
               OR ft.old_path = ?1
               OR ft.path LIKE ?2 ESCAPE '\'
               OR ft.old_path LIKE ?2 ESCAPE '\'
            "#,
        )?;
        let rows = stmt.query_map(params![exact, suffix], |row| {
            Ok((
                parse_optional_uuid(row.get(0)?)?,
                parse_optional_uuid(row.get(1)?)?,
                parse_optional_uuid(row.get(2)?)?,
                parse_optional_uuid(row.get(3)?)?,
                parse_optional_uuid(row.get(4)?)?,
            ))
        })?;
        for row in rows {
            let (record_id, session_id, run_id, event_id, source_id) = row?;
            if let Some(id) = record_id {
                scope.history_record_ids.insert(id);
            }
            if let Some(id) = session_id {
                scope.session_ids.insert(id);
            }
            if let Some(id) = run_id {
                scope.run_ids.insert(id);
            }
            if let Some(id) = event_id {
                scope.event_ids.insert(id);
            }
            if let Some(id) = source_id {
                scope.source_ids.insert(id);
            }
        }
        Ok(scope)
    }

    pub fn upsert_history_record_link(&self, link: &HistoryRecordLink) -> Result<Uuid> {
        self.conn.execute(
            r#"
            INSERT INTO history_record_links
            (id, history_record_id, target_type, target_id, link_type, confidence, source_id, created_at_ms, updated_at_ms, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
            ON CONFLICT(history_record_id, target_type, target_id, link_type) DO UPDATE SET
                confidence = excluded.confidence,
                source_id = excluded.source_id,
                updated_at_ms = excluded.updated_at_ms,
                visibility = excluded.visibility,
                fidelity = excluded.fidelity,
                sync_state = excluded.sync_state,
                sync_version = excluded.sync_version,
                deleted_at_ms = excluded.deleted_at_ms,
                metadata_json = excluded.metadata_json
            "#,
            params![
                link.id.to_string(),
                link.history_record_id.to_string(),
                link.target_type.as_str(),
                link.target_id.to_string(),
                link.link_type.as_str(),
                link.confidence.as_str(),
                optional_uuid_string(link.source_id),
                timestamp_ms(link.timestamps.created_at),
                timestamp_ms(link.timestamps.updated_at),
                link.sync.visibility.as_str(),
                link.sync.fidelity.as_str(),
                link.sync.sync_state.as_str(),
                link.sync.sync_version as i64,
                optional_timestamp_ms(link.sync.deleted_at),
                serde_json::to_string(&link.sync.metadata)?,
            ],
        )?;
        self.conn
            .query_row(
                "SELECT id FROM history_record_links WHERE history_record_id = ?1 AND target_type = ?2 AND target_id = ?3 AND link_type = ?4",
                params![
                    link.history_record_id.to_string(),
                    link.target_type.as_str(),
                    link.target_id.to_string(),
                    link.link_type.as_str()
                ],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .map_err(StoreError::from)
    }

    fn list_history_record_links(&self) -> Result<Vec<HistoryRecordLink>> {
        let mut stmt = self
            .conn
            .prepare(history_record_link_select_sql("ORDER BY updated_at_ms, id").as_str())?;
        let rows = stmt.query_map([], history_record_link_from_row)?;
        collect_rows(rows)
    }

    pub fn upsert_sync_cursor(&self, cursor: &SyncCursor) -> Result<Uuid> {
        if let Some(existing) =
            self.get_sync_cursor(cursor.team_id.as_deref(), &cursor.device_id, &cursor.stream)?
        {
            self.conn.execute(
                r#"
                UPDATE sync_cursors
                SET cursor = ?1, last_synced_at_ms = ?2, updated_at_ms = ?3
                WHERE id = ?4
                "#,
                params![
                    cursor.cursor.as_str(),
                    optional_timestamp_ms(cursor.last_synced_at),
                    timestamp_ms(cursor.timestamps.updated_at),
                    existing.id.to_string(),
                ],
            )?;
            return Ok(existing.id);
        }

        self.conn.execute(
            r#"
            INSERT INTO sync_cursors
            (id, team_id, device_id, stream, cursor, last_synced_at_ms, created_at_ms, updated_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(team_id, device_id, stream) DO UPDATE SET
                cursor = excluded.cursor,
                last_synced_at_ms = excluded.last_synced_at_ms,
                updated_at_ms = excluded.updated_at_ms
            "#,
            params![
                cursor.id.to_string(),
                cursor.team_id.as_deref(),
                cursor.device_id.as_str(),
                cursor.stream.as_str(),
                cursor.cursor.as_str(),
                optional_timestamp_ms(cursor.last_synced_at),
                timestamp_ms(cursor.timestamps.created_at),
                timestamp_ms(cursor.timestamps.updated_at),
            ],
        )?;
        self.conn
            .query_row(
                "SELECT id FROM sync_cursors WHERE team_id IS ?1 AND device_id = ?2 AND stream = ?3",
                params![cursor.team_id.as_deref(), cursor.device_id.as_str(), cursor.stream.as_str()],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .map_err(StoreError::from)
    }

    pub fn get_sync_cursor(
        &self,
        team_id: Option<&str>,
        device_id: &str,
        stream: &str,
    ) -> Result<Option<SyncCursor>> {
        self.conn
            .query_row(
                "SELECT id, team_id, device_id, stream, cursor, last_synced_at_ms, created_at_ms, updated_at_ms FROM sync_cursors WHERE team_id IS ?1 AND device_id = ?2 AND stream = ?3",
                params![team_id, device_id, stream],
                sync_cursor_from_row,
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn insert_record(&self, record: &HistoryRecord) -> Result<()> {
        let created_at_ms = timestamp_ms(record.created_at);
        let updated_at_ms = timestamp_ms(record.updated_at);
        self.conn.execute(
            r#"
            INSERT INTO history_records
            (
                id, title, summary, status, started_at_ms, last_activity_at_ms,
                created_at_ms, updated_at_ms, body, tags_json, kind, workspace,
                created_at, updated_at
            )
            VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            "#,
            params![
                record.id.to_string(),
                record.title,
                record.body,
                created_at_ms,
                updated_at_ms,
                record.body,
                serde_json::to_string(&record.tags)?,
                record.kind,
                record.workspace,
                record.created_at.to_rfc3339(),
                record.updated_at.to_rfc3339(),
            ],
        )?;
        upsert_record_search_projection(&self.conn, record)?;
        Ok(())
    }

    pub fn upsert_record(&self, record: &HistoryRecord) -> Result<()> {
        self.upsert_record_row(record)?;
        upsert_record_search_projection(&self.conn, record)?;
        Ok(())
    }

    pub fn upsert_records(&self, records: &[HistoryRecord]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        self.begin_immediate_batch()?;
        for record in records {
            if let Err(err) = self.upsert_record_row(record) {
                let _ = self.rollback_batch();
                return Err(err);
            }
        }
        if let Err(err) = self.commit_batch() {
            let _ = self.rollback_batch();
            return Err(err);
        }
        for record in records {
            upsert_record_search_projection(&self.conn, record)?;
        }
        Ok(())
    }

    fn upsert_record_row(&self, record: &HistoryRecord) -> Result<()> {
        let created_at_ms = timestamp_ms(record.created_at);
        let updated_at_ms = timestamp_ms(record.updated_at);
        self.conn.execute(
            r#"
            INSERT INTO history_records
            (
                id, title, summary, status, started_at_ms, last_activity_at_ms,
                created_at_ms, updated_at_ms, body, tags_json, kind, workspace,
                created_at, updated_at
            )
            VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                summary = excluded.summary,
                status = excluded.status,
                started_at_ms = excluded.started_at_ms,
                last_activity_at_ms = excluded.last_activity_at_ms,
                created_at_ms = excluded.created_at_ms,
                updated_at_ms = excluded.updated_at_ms,
                body = excluded.body,
                tags_json = excluded.tags_json,
                kind = excluded.kind,
                workspace = excluded.workspace,
                created_at = excluded.created_at,
                updated_at = excluded.updated_at
            "#,
            params![
                record.id.to_string(),
                record.title,
                record.body,
                created_at_ms,
                updated_at_ms,
                record.body,
                serde_json::to_string(&record.tags)?,
                record.kind,
                record.workspace,
                record.created_at.to_rfc3339(),
                record.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn get_record(&self, id: Uuid) -> Result<HistoryRecord> {
        self.conn
            .query_row(
                record_select_sql("WHERE id = ?1").as_str(),
                params![id.to_string()],
                record_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    pub fn list_records(&self, limit: usize) -> Result<Vec<HistoryRecord>> {
        self.list_records_page(limit, 0)
    }

    pub fn list_records_page(&self, limit: usize, offset: usize) -> Result<Vec<HistoryRecord>> {
        let mut stmt = self.conn.prepare(
            record_select_sql("ORDER BY created_at DESC, id LIMIT ?1 OFFSET ?2").as_str(),
        )?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], record_from_row)?;
        collect_rows(rows)
    }

    pub fn search_records(&self, query: &str, limit: usize) -> Result<Vec<HistoryRecord>> {
        self.search_records_page(query, limit, 0)
    }

    pub fn search_records_page(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<HistoryRecord>> {
        if fts_match_query(query).is_none() {
            return Ok(Vec::new());
        }
        if let Some(records) = self.search_records_fts(query, limit, offset)? {
            return Ok(records);
        }
        let like = format!("%{}%", query);
        let mut stmt = self.conn.prepare(
            record_select_sql(
                "WHERE title LIKE ?1 OR body LIKE ?1 OR tags_json LIKE ?1 ORDER BY created_at DESC, id LIMIT ?2 OFFSET ?3",
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![like, limit as i64, offset as i64], record_from_row)?;
        collect_rows(rows)
    }

    fn search_records_fts(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Option<Vec<HistoryRecord>>> {
        if !table_exists(&self.conn, "ctx_history_search")? {
            return Ok(None);
        }
        let Some(match_query) = fts_match_query(query) else {
            return Ok(Some(Vec::new()));
        };
        let has_event_search = table_exists(&self.conn, "event_search")?;
        let has_artifact_search = table_exists(&self.conn, "artifact_search")?;
        let sql = if has_event_search && has_artifact_search {
            r#"
            WITH matches(record_id, score) AS (
                SELECT record_id, bm25(ctx_history_search)
                FROM ctx_history_search
                WHERE ctx_history_search MATCH ?1
                UNION ALL
                SELECT history_record_id, bm25(event_search)
                FROM event_search
                WHERE event_search MATCH ?1 AND history_record_id IS NOT NULL
                UNION ALL
                SELECT history_record_id, bm25(artifact_search)
                FROM artifact_search
                WHERE artifact_search MATCH ?1 AND history_record_id IS NOT NULL
            )
            SELECT record_id
            FROM matches
            WHERE record_id IS NOT NULL
            GROUP BY record_id
            ORDER BY MIN(score), record_id
            LIMIT ?2 OFFSET ?3
            "#
        } else {
            r#"
            SELECT record_id
            FROM ctx_history_search
            WHERE ctx_history_search MATCH ?1
            ORDER BY bm25(ctx_history_search), record_id
            LIMIT ?2 OFFSET ?3
            "#
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![match_query, limit as i64, offset as i64], |row| {
            row.get::<_, String>(0)
        })?;
        let mut records = Vec::new();
        for row in rows {
            records.push(self.get_record(parse_uuid(row?)?)?);
        }
        Ok(Some(records))
    }

    pub fn max_events_per_history_record(&self) -> Result<i64> {
        let max_events = self.conn.query_row(
            r#"
            SELECT COALESCE(MAX(event_count), 0)
            FROM (
                SELECT COUNT(*) AS event_count
                FROM events
                GROUP BY history_record_id
            )
            "#,
            [],
            |row| row.get(0),
        )?;
        Ok(max_events)
    }

    pub fn has_at_least_events(&self, threshold: i64) -> Result<bool> {
        if threshold <= 0 {
            return Ok(true);
        }
        let exists = self.conn.query_row(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM events
                LIMIT 1 OFFSET ?1
            )
            "#,
            params![threshold - 1],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(exists != 0)
    }

    pub fn has_provider_data(&self, provider: CaptureProvider) -> Result<bool> {
        let exists = self.conn.query_row(
            r#"
            SELECT
                EXISTS(
                    SELECT 1
                    FROM sessions
                    WHERE provider = ?1
                    LIMIT 1
                )
                OR EXISTS(
                    SELECT 1
                    FROM capture_sources
                    WHERE provider = ?1
                    LIMIT 1
                )
            "#,
            params![provider.as_str()],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(exists != 0)
    }

    pub fn search_event_hits(&self, query: &str, limit: usize) -> Result<Vec<EventSearchHit>> {
        self.search_event_hits_page(query, limit, 0)
    }

    pub fn search_event_hits_page(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<EventSearchHit>> {
        if !table_exists(&self.conn, "event_search")? {
            return Ok(Vec::new());
        }
        let Some(match_query) = fts_match_query(query) else {
            return Ok(Vec::new());
        };
        let mut stmt = self.conn.prepare(
            r#"
            SELECT event_search.event_id,
                   COALESCE(e.history_record_id, event_search.history_record_id, s.history_record_id, rs.history_record_id),
                   COALESCE(e.session_id, event_search.session_id, s.id, rs.id),
                   e.run_id,
                   e.seq,
                   e.event_type,
                   e.role,
                   e.occurred_at_ms,
                   event_search.safe_preview_text,
                   bm25(event_search),
                   COALESCE(s.provider, rs.provider, event_source.provider, session_source.provider, run_source.provider),
                   COALESCE(s.external_session_id, rs.external_session_id),
                   COALESCE(s.parent_session_id, rs.parent_session_id),
                   COALESCE(s.root_session_id, rs.root_session_id),
                   COALESCE(s.agent_type, rs.agent_type),
                   COALESCE(s.is_primary, rs.is_primary),
                   COALESCE(event_source.cwd, session_source.cwd, run_source.cwd),
                   COALESCE(event_source.raw_source_path, session_source.raw_source_path, run_source.raw_source_path),
                   e.payload_json,
                   COALESCE(event_source.metadata_json, session_source.metadata_json, run_source.metadata_json),
                   wr.title,
                   wr.kind,
                   wr.workspace
            FROM event_search
            JOIN events e ON e.id = event_search.event_id
            LEFT JOIN runs r ON r.id = e.run_id
            LEFT JOIN sessions s ON s.id = COALESCE(e.session_id, event_search.session_id)
            LEFT JOIN sessions rs ON rs.id = r.session_id
            LEFT JOIN capture_sources event_source ON event_source.id = e.capture_source_id
            LEFT JOIN capture_sources session_source ON session_source.id = COALESCE(s.capture_source_id, rs.capture_source_id)
            LEFT JOIN capture_sources run_source ON run_source.id = r.source_id
            LEFT JOIN history_records wr ON wr.id = COALESCE(e.history_record_id, event_search.history_record_id, s.history_record_id, rs.history_record_id, r.history_record_id)
            WHERE event_search MATCH ?1
            ORDER BY bm25(event_search), e.occurred_at_ms DESC, e.seq DESC, event_search.event_id
            LIMIT ?2 OFFSET ?3
            "#,
        )?;
        let rows = stmt.query_map(
            params![match_query, limit.max(1) as i64, offset as i64],
            |row| {
                let payload_json = row.get::<_, String>(18)?;
                let source_metadata_json = row.get::<_, Option<String>>(19)?;
                let source_identity =
                    event_search_source_identity(source_metadata_json.as_deref())?;
                Ok(EventSearchHit {
                    event_id: parse_uuid(row.get::<_, String>(0)?)?,
                    history_record_id: parse_optional_uuid(row.get(1)?)?,
                    session_id: parse_optional_uuid(row.get(2)?)?,
                    run_id: parse_optional_uuid(row.get(3)?)?,
                    seq: nonnegative_i64_to_u64(row.get(4)?)?,
                    event_type: parse_text_enum::<EventType>(row.get::<_, String>(5)?)?,
                    role: parse_optional_text_enum::<EventRole>(row.get(6)?)?,
                    occurred_at: ms_to_time(row.get(7)?)?,
                    preview: row.get(8)?,
                    score: row.get(9)?,
                    provider: parse_optional_text_enum::<CaptureProvider>(row.get(10)?)?,
                    session_external_session_id: row.get(11)?,
                    history_source: source_identity.history_source,
                    history_source_plugin: source_identity.history_source_plugin,
                    provider_key: source_identity.provider_key,
                    source_id: source_identity.source_id,
                    source_format: source_identity.source_format,
                    session_parent_session_id: parse_optional_uuid(row.get(12)?)?,
                    session_root_session_id: parse_optional_uuid(row.get(13)?)?,
                    agent_type: parse_optional_text_enum::<AgentType>(row.get(14)?)?,
                    session_is_primary: row.get::<_, Option<i64>>(15)?.map(|value| value != 0),
                    cwd: row.get(16)?,
                    raw_source_path: row.get(17)?,
                    cursor: event_search_cursor(&payload_json, source_metadata_json.as_deref())?,
                    record_title: row.get(20)?,
                    record_kind: row.get(21)?,
                    record_workspace: row.get(22)?,
                })
            },
        )?;
        collect_rows(rows)
    }

    pub fn export_archive(&self) -> Result<SessionHistoryArchive> {
        Ok(SessionHistoryArchive {
            schema_version: 2,
            version: 2,
            records: self.list_records(usize::MAX)?,
            capture_sources: self.list_capture_sources()?,
            sessions: self.list_sessions()?,
            runs: self.list_runs()?,
            events: self.list_events()?,
            artifact_records: self.list_artifacts()?,
            vcs_workspaces: self.list_vcs_workspaces()?,
            vcs_changes: self.list_vcs_changes()?,
            history_record_links: self.list_history_record_links()?,
            summaries: self.list_summaries()?,
            files_touched: self.list_files_touched()?,
        })
    }

    pub fn import_archive(
        &mut self,
        archive: &SessionHistoryArchive,
        overwrite: bool,
    ) -> Result<()> {
        validate_archive_version(archive)?;
        reject_archive_event_internal_conflicts(archive)?;
        let blob_dir = self.object_dir.clone();
        let tx = self.conn.transaction()?;
        reject_import_invariant_conflicts(&tx, archive)?;
        if !overwrite {
            reject_import_conflicts(&tx, archive)?;
        }
        let mut blob_guard = BlobWriteGuard::default();
        for record in &archive.records {
            upsert_record_tx(&tx, record, None)?;
        }
        import_rich_archive_entities_tx(&tx, &blob_dir, archive, &mut blob_guard)?;
        tx.commit()?;
        blob_guard.commit();
        self.rebuild_search_projection()?;
        Ok(())
    }

    pub fn import_archive_from_capture_source(
        &mut self,
        archive: &SessionHistoryArchive,
        source_id: Uuid,
        source: &CaptureSourceDescriptor,
        occurred_at: DateTime<Utc>,
        fidelity: Fidelity,
        overwrite: bool,
    ) -> Result<()> {
        validate_archive_version(archive)?;
        reject_archive_event_internal_conflicts(archive)?;
        let blob_dir = self.object_dir.clone();
        let tx = self.conn.transaction()?;
        reject_import_invariant_conflicts(&tx, archive)?;
        if !overwrite {
            reject_capture_source_import_conflict(&tx, source_id)?;
            reject_import_conflicts(&tx, archive)?;
        }
        let mut blob_guard = BlobWriteGuard::default();
        upsert_capture_source_tx(&tx, source_id, source, occurred_at, fidelity)?;
        for record in &archive.records {
            upsert_record_tx(&tx, record, Some(source_id))?;
        }
        import_rich_archive_entities_tx(&tx, &blob_dir, archive, &mut blob_guard)?;
        tx.commit()?;
        blob_guard.commit();
        self.rebuild_search_projection()?;
        Ok(())
    }

    pub fn validate(&self) -> Result<Vec<String>> {
        let integrity: String = self
            .conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        let foreign_key_failures = count_foreign_key_failures(&self.conn)?;

        let mut findings = Vec::new();
        if integrity != "ok" {
            findings.push(format!("sqlite integrity_check returned {integrity}"));
        }
        if foreign_key_failures > 0 {
            findings.push(format!(
                "{foreign_key_failures} foreign key violations detected"
            ));
        }
        Ok(findings)
    }

    fn rebuild_search_projection(&self) -> Result<()> {
        rebuild_search_projection(&self.conn)
    }

    fn ensure_search_projection_initialized(&self) -> Result<()> {
        ensure_search_projection_initialized(&self.conn)
    }

    fn normalize_legacy_blob_paths(&self) -> Result<()> {
        self.conn.execute(
            "UPDATE artifacts SET blob_path = 'objects/' || substr(blob_path, 7) WHERE blob_path LIKE 'blobs/%'",
            [],
        )?;
        Ok(())
    }
}

fn configure_connection(conn: &Connection, busy_timeout: Duration) -> Result<()> {
    conn.busy_timeout(busy_timeout)?;
    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA temp_store = MEMORY;
        PRAGMA cache_size = -32768;
        PRAGMA wal_autocheckpoint = 10000;
        "#,
    )?;
    Ok(())
}

fn configure_read_only_connection(conn: &Connection, busy_timeout: Duration) -> Result<()> {
    conn.busy_timeout(busy_timeout)?;
    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;
        PRAGMA temp_store = MEMORY;
        PRAGMA cache_size = -32768;
        PRAGMA query_only = ON;
        "#,
    )?;
    Ok(())
}

fn validate_raw_sql_options(options: &RawSqlOptions) -> Result<()> {
    validate_raw_sql_usize("max_rows", options.max_rows, 1, RAW_SQL_MAX_ROWS_CAP)?;
    validate_raw_sql_usize(
        "max_columns",
        options.max_columns,
        1,
        RAW_SQL_MAX_COLUMNS_CAP,
    )?;
    validate_raw_sql_usize(
        "max_value_bytes",
        options.max_value_bytes,
        1,
        RAW_SQL_MAX_VALUE_BYTES_CAP,
    )?;
    validate_raw_sql_usize(
        "max_sql_bytes",
        options.max_sql_bytes,
        1,
        RAW_SQL_MAX_SQL_BYTES_CAP,
    )?;
    let timeout_ms = duration_ms(options.timeout);
    if timeout_ms == 0 || options.timeout > RAW_SQL_MAX_TIMEOUT {
        return Err(StoreError::RawSqlLimitOutOfRange {
            field: "timeout_ms",
            value: usize::try_from(timeout_ms).unwrap_or(usize::MAX),
            min: 1,
            max: usize::try_from(duration_ms(RAW_SQL_MAX_TIMEOUT)).unwrap_or(usize::MAX),
        });
    }
    Ok(())
}

fn validate_raw_sql_statement_bytes(sql: &str, options: &RawSqlOptions) -> Result<()> {
    validate_raw_sql_usize("sql_bytes", sql.len(), 1, options.max_sql_bytes)
}

fn validate_raw_sql_result_preview_budget(
    options: &RawSqlOptions,
    column_count: usize,
) -> Result<()> {
    let estimated_cells = options.max_rows.saturating_mul(column_count);
    let per_cell_bytes = options
        .max_value_bytes
        .saturating_mul(4)
        .saturating_add(64)
        .max(128);
    let estimated_bytes = options
        .max_rows
        .saturating_mul(column_count)
        .saturating_mul(per_cell_bytes);
    if estimated_cells > RAW_SQL_MAX_RESULT_CELLS
        || estimated_bytes > RAW_SQL_MAX_RESULT_PREVIEW_BYTES
    {
        return Err(StoreError::RawSqlResultBudgetTooLarge {
            estimated_bytes,
            max_result_bytes: RAW_SQL_MAX_RESULT_PREVIEW_BYTES,
        });
    }
    Ok(())
}

struct RawSqlLimitGuard<'a> {
    conn: &'a Connection,
    length: i32,
    sql_length: i32,
    column: i32,
}

impl<'a> RawSqlLimitGuard<'a> {
    fn apply(conn: &'a Connection, options: &RawSqlOptions) -> Result<Self> {
        let length_limit = raw_sql_length_limit(options)?;
        let sql_length_limit = i32::try_from(options.max_sql_bytes).map_err(|_| {
            StoreError::RawSqlLimitOutOfRange {
                field: "max_sql_bytes",
                value: options.max_sql_bytes,
                min: 1,
                max: RAW_SQL_MAX_SQL_BYTES_CAP,
            }
        })?;
        let column_limit =
            i32::try_from(options.max_columns).map_err(|_| StoreError::RawSqlLimitOutOfRange {
                field: "max_columns",
                value: options.max_columns,
                min: 1,
                max: RAW_SQL_MAX_COLUMNS_CAP,
            })?;
        let guard = Self {
            conn,
            length: conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, length_limit),
            sql_length: conn.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, sql_length_limit),
            column: conn.set_limit(Limit::SQLITE_LIMIT_COLUMN, column_limit),
        };
        Ok(guard)
    }
}

impl Drop for RawSqlLimitGuard<'_> {
    fn drop(&mut self) {
        self.conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, self.length);
        self.conn
            .set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, self.sql_length);
        self.conn.set_limit(Limit::SQLITE_LIMIT_COLUMN, self.column);
    }
}

fn raw_sql_length_limit(options: &RawSqlOptions) -> Result<i32> {
    let bytes = options
        .max_value_bytes
        .saturating_add(RAW_SQL_VALUE_LENGTH_MARGIN_BYTES);
    let bytes = bytes.max(RAW_SQL_MIN_SQLITE_LENGTH_LIMIT_BYTES);
    i32::try_from(bytes).map_err(|_| StoreError::RawSqlLimitOutOfRange {
        field: "max_value_bytes",
        value: options.max_value_bytes,
        min: 1,
        max: RAW_SQL_MAX_VALUE_BYTES_CAP,
    })
}

fn validate_raw_sql_usize(field: &'static str, value: usize, min: usize, max: usize) -> Result<()> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(StoreError::RawSqlLimitOutOfRange {
            field,
            value,
            min,
            max,
        })
    }
}

fn reject_sql_tail(conn: &Connection, sql: &str) -> Result<()> {
    let c_sql = CString::new(sql).map_err(|_| StoreError::RawSqlInteriorNul)?;
    let mut stmt = ptr::null_mut();
    let mut tail: *const c_char = ptr::null();
    let rc =
        unsafe { ffi::sqlite3_prepare_v2(conn.handle(), c_sql.as_ptr(), -1, &mut stmt, &mut tail) };
    if !stmt.is_null() {
        unsafe {
            ffi::sqlite3_finalize(stmt);
        }
    }
    if rc != ffi::SQLITE_OK || tail.is_null() {
        return Ok(());
    }

    let start = c_sql.as_ptr() as usize;
    let tail_offset = (tail as usize).saturating_sub(start);
    let sql_bytes = c_sql.as_bytes();
    if tail_offset < sql_bytes.len() && sql_tail_has_statement(&sql[tail_offset..]) {
        return Err(StoreError::Sql(rusqlite::Error::MultipleStatement));
    }
    Ok(())
}

fn sql_tail_has_statement(mut tail: &str) -> bool {
    loop {
        let trimmed = tail.trim_start();
        if trimmed.is_empty() {
            return false;
        }
        if let Some(rest) = trimmed.strip_prefix("--") {
            if let Some(newline) = rest.find('\n') {
                tail = &rest[newline + 1..];
                continue;
            }
            return false;
        }
        if let Some(rest) = trimmed.strip_prefix("/*") {
            if let Some(end) = rest.find("*/") {
                tail = &rest[end + 2..];
                continue;
            }
            return true;
        }
        return true;
    }
}

fn raw_sql_value(value: ValueRef<'_>, max_value_bytes: usize) -> RawSqlValue {
    match value {
        ValueRef::Null => RawSqlValue::Null,
        ValueRef::Integer(value) => RawSqlValue::Integer(value),
        ValueRef::Real(value) => RawSqlValue::Real(value),
        ValueRef::Text(bytes) => {
            let truncated = bytes.len() > max_value_bytes;
            let preview = if truncated {
                String::from_utf8_lossy(&bytes[..max_value_bytes]).into_owned()
            } else {
                String::from_utf8_lossy(bytes).into_owned()
            };
            RawSqlValue::Text {
                value: preview,
                bytes: bytes.len(),
                truncated,
            }
        }
        ValueRef::Blob(bytes) => {
            let truncated = bytes.len() > max_value_bytes;
            let preview_len = bytes.len().min(max_value_bytes);
            RawSqlValue::Blob {
                bytes: bytes.len(),
                preview_hex: hex_preview(&bytes[..preview_len]),
                truncated,
            }
        }
    }
}

fn hex_preview(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn migrate_legacy_history_layout(data_root: &Path) -> Result<bool> {
    let legacy_dir = data_root.join(LEGACY_HISTORY_DIR_NAME);
    if !legacy_dir.is_dir() {
        return Ok(false);
    }

    let mut moves = Vec::new();
    push_legacy_move(
        &mut moves,
        legacy_dir.join("work.sqlite"),
        data_root.join("work.sqlite"),
    );
    push_legacy_move(
        &mut moves,
        legacy_dir.join("config.toml"),
        data_root.join("config.toml"),
    );
    push_legacy_move(&mut moves, legacy_dir.join("logs"), data_root.join("logs"));
    push_legacy_move(
        &mut moves,
        legacy_dir.join("device.json"),
        data_root.join("device.json"),
    );

    let object_candidates = [
        legacy_dir.join(OBJECTS_DIR),
        legacy_dir.join(LEGACY_BLOBS_DIR),
    ];
    let spool_candidates = [
        legacy_dir.join(SPOOL_DIR),
        legacy_dir.join(LEGACY_INBOX_DIR),
    ];
    if multiple_existing_paths(&object_candidates) || multiple_existing_paths(&spool_candidates) {
        return Ok(false);
    }

    if let Some(object_source) = unique_existing_path(&object_candidates) {
        push_legacy_move(&mut moves, object_source, data_root.join(OBJECTS_DIR));
    }

    if let Some(spool_source) = unique_existing_path(&spool_candidates) {
        push_legacy_move(&mut moves, spool_source, data_root.join(SPOOL_DIR));
    }

    if moves.is_empty() || moves.iter().any(|(_, dest)| dest.exists()) {
        return Ok(false);
    }

    for (source, dest) in moves {
        fs::rename(source, dest)?;
    }
    let _ = fs::remove_dir(&legacy_dir);
    Ok(true)
}

fn push_legacy_move(moves: &mut Vec<(PathBuf, PathBuf)>, source: PathBuf, dest: PathBuf) {
    if source.exists() {
        moves.push((source, dest));
    }
}

fn unique_existing_path(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut existing = paths.iter().filter(|path| path.exists());
    let first = existing.next()?.clone();
    if existing.next().is_some() {
        return None;
    }
    Some(first)
}

fn multiple_existing_paths(paths: &[PathBuf]) -> bool {
    paths.iter().filter(|path| path.exists()).take(2).count() > 1
}

fn object_relative_path(hash: &str) -> String {
    let shard = &hash[..2];
    format!("{OBJECTS_DIR}/{shard}/{hash}")
}

fn rebuild_search_projection(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "ctx_history_search")? {
        return Ok(());
    }

    conn.execute("DELETE FROM ctx_history_search", [])?;
    let has_event_search = table_exists(conn, "event_search")?;
    if has_event_search {
        conn.execute("DELETE FROM event_search", [])?;
        populate_event_search_projection(conn)?;
    }
    if table_exists(conn, "artifact_search")? {
        conn.execute("DELETE FROM artifact_search", [])?;
    }

    let records = {
        let mut stmt = conn.prepare(record_select_sql("ORDER BY created_at DESC").as_str())?;
        let rows = stmt.query_map([], record_from_row)?;
        collect_rows(rows)?
    };

    let mut insert_record_search = conn.prepare(
        r#"
        INSERT INTO ctx_history_search
        (record_id, title, summary, primary_user_text, decision_text, context_text, tag_text)
        VALUES (?1, ?2, ?3, ?4, '', ?5, ?6)
        "#,
    )?;
    for record in records {
        insert_record_search.execute(params![
            record.id.to_string(),
            local_preview(&record.title, 512),
            local_preview(&record.body, 2048),
            local_preview(&record.body, 2048),
            "",
            local_preview(&record.tags.join(" "), 1024),
        ])?;
    }

    Ok(())
}

fn upsert_record_search_projection(conn: &Connection, record: &HistoryRecord) -> Result<()> {
    if !table_exists(conn, "ctx_history_search")? {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM ctx_history_search WHERE record_id = ?1",
        params![record.id.to_string()],
    )?;
    conn.execute(
        r#"
        INSERT INTO ctx_history_search
        (record_id, title, summary, primary_user_text, decision_text, context_text, tag_text)
        VALUES (?1, ?2, ?3, ?4, '', ?5, ?6)
        "#,
        params![
            record.id.to_string(),
            local_preview(&record.title, 512),
            local_preview(&record.body, 2048),
            local_preview(&record.body, 2048),
            "",
            local_preview(&record.tags.join(" "), 1024),
        ],
    )?;
    Ok(())
}

fn ensure_search_projection_initialized(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "ctx_history_search")? {
        return Ok(());
    }

    let mut projection_rows = table_row_count(conn, "ctx_history_search")?;
    if table_exists(conn, "event_search")? {
        projection_rows += table_row_count(conn, "event_search")?;
    }
    if table_exists(conn, "artifact_search")? {
        projection_rows += table_row_count(conn, "artifact_search")?;
    }
    if projection_rows > 0 {
        return Ok(());
    }

    if table_row_count(conn, "history_records")? > 0
        || table_row_count(conn, "events")? > 0
        || linked_artifact_preview_count(conn)? > 0
    {
        rebuild_search_projection(conn)?;
    }

    Ok(())
}

fn table_row_count(conn: &Connection, table: &str) -> Result<i64> {
    match table {
        "artifacts" | "artifact_search" | "events" | "event_search" | "history_records"
        | "ctx_history_search" => {}
        _ => unreachable!("invalid table {table}"),
    }
    let sql = format!("SELECT COUNT(*) FROM {table}");
    Ok(conn.query_row(&sql, [], |row| row.get(0))?)
}

fn linked_artifact_preview_count(conn: &Connection) -> Result<i64> {
    let _ = conn;
    Ok(0)
}

fn populate_event_search_projection(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare(
        r#"
        SELECT e.id,
               COALESCE(e.history_record_id, r.history_record_id, s.history_record_id, rs.history_record_id),
               e.session_id,
               e.role,
               e.event_type,
               e.payload_json,
               e.redaction_state
        FROM events e
        LEFT JOIN runs r ON r.id = e.run_id
        LEFT JOIN sessions s ON s.id = e.session_id
        LEFT JOIN sessions rs ON rs.id = r.session_id
        ORDER BY e.occurred_at_ms, e.seq, e.id
        "#,
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
        ))
    })?;
    let mut insert_event_search = conn.prepare(
        r#"
        INSERT INTO event_search
        (event_id, history_record_id, session_id, role, safe_preview_text, rank_bucket)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        "#,
    )?;
    for row in rows {
        let (
            event_id,
            history_record_id,
            session_id,
            role,
            event_type,
            payload_json,
            redaction_state,
        ) = row?;
        let preview = event_search_preview(&payload_json, &redaction_state)?;
        if preview.trim().is_empty() {
            continue;
        }
        insert_event_search.execute(params![
            event_id,
            history_record_id,
            session_id,
            role,
            preview,
            event_type
        ])?;
    }
    Ok(())
}

fn insert_event_search_projection_for_event(conn: &Connection, event: &Event) -> Result<()> {
    insert_event_search_projection_for_event_id(conn, event.id, event)
}

fn upsert_event_search_projection_for_event(
    conn: &Connection,
    event_id: Uuid,
    event: &Event,
) -> Result<()> {
    if !table_exists(conn, "event_search")? {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM event_search WHERE event_id = ?1",
        params![event_id.to_string()],
    )?;
    insert_event_search_projection_for_event_id(conn, event_id, event)
}

fn insert_event_search_projection_for_event_id(
    conn: &Connection,
    event_id: Uuid,
    event: &Event,
) -> Result<()> {
    if !table_exists(conn, "event_search")? {
        return Ok(());
    }
    let preview = event_search_preview_from_payload(&event.payload, event.redaction_state);
    if preview.trim().is_empty() {
        return Ok(());
    }
    conn.prepare_cached(
        r#"
        INSERT INTO event_search
        (event_id, history_record_id, session_id, role, safe_preview_text, rank_bucket)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        "#,
    )?
    .execute(params![
        event_id.to_string(),
        optional_uuid_string(event.history_record_id),
        optional_uuid_string(event.session_id),
        event.role.map(|role| role.as_str()),
        preview,
        event.event_type.as_str(),
    ])?;
    Ok(())
}

fn event_search_preview(payload_json: &str, redaction_state: &str) -> Result<String> {
    if redaction_state == RedactionState::Raw.as_str() {
        return Ok("raw event payload withheld".to_owned());
    }
    let payload: serde_json::Value = serde_json::from_str(payload_json)?;
    Ok(event_search_preview_from_payload(
        &payload,
        parse_text_enum::<RedactionState>(redaction_state.to_owned())?,
    ))
}

fn event_search_preview_from_payload(
    payload: &serde_json::Value,
    redaction_state: RedactionState,
) -> String {
    if redaction_state == RedactionState::Raw {
        return "raw event payload withheld".to_owned();
    }
    let preview = event_payload_preview(payload)
        .or_else(|| {
            if payload.is_object() || payload.is_array() {
                Some(payload.to_string())
            } else {
                None
            }
        })
        .unwrap_or_default();
    local_preview(&preview, 2048)
}

fn local_preview(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

fn event_payload_preview(payload: &serde_json::Value) -> Option<String> {
    if let Some(body) = payload.get("body") {
        if let Some(preview) = event_value_preview(body) {
            return Some(preview);
        }
    }
    event_value_preview(payload)
}

fn event_value_preview(value: &serde_json::Value) -> Option<String> {
    if let Some(value) = value.as_str() {
        return non_blank(value);
    }
    let object = value.as_object()?;
    for key in [
        "text",
        "preview",
        "summary",
        "command",
        "output_preview",
        "output",
        "message",
    ] {
        if let Some(value) = object.get(key).and_then(event_preview_fragment) {
            return Some(value);
        }
    }
    let structured = ["tool", "name", "arguments_preview", "status"]
        .into_iter()
        .filter_map(|key| {
            object
                .get(key)
                .and_then(event_preview_fragment)
                .map(|value| format!("{key}: {value}"))
        })
        .collect::<Vec<_>>();
    if structured.is_empty() {
        None
    } else {
        Some(structured.join(" | "))
    }
}

fn event_preview_fragment(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => non_blank(value),
        serde_json::Value::Number(_) | serde_json::Value::Bool(_) => Some(value.to_string()),
        _ => None,
    }
}

fn non_blank(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn file_touch_match_values(file: &str) -> Option<(String, String)> {
    let exact = file.trim();
    if exact.is_empty() {
        return None;
    }
    let suffix = exact.trim_start_matches(['/', '\\']);
    Some((
        exact.to_owned(),
        format!("%/{}", escape_like_pattern(suffix)),
    ))
}

fn escape_like_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

fn migrate_to_v1(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(conn, "history_records", HISTORY_RECORD_COLUMNS)?;
        backfill_legacy_tables(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 1;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            Err(err)
        }
    }
}

fn migrate_to_v2(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(conn, "history_records", HISTORY_RECORD_COLUMNS)?;
        backfill_legacy_tables(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 2;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            Err(err)
        }
    }
}

fn migrate_to_v3(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(conn, "history_records", HISTORY_RECORD_COLUMNS)?;
        backfill_legacy_tables(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 3;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            Err(err)
        }
    }
}

fn migrate_to_v4(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        rebuild_capture_sources_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 4;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v5(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(
            conn,
            "catalog_sessions",
            CATALOG_SESSION_IMPORT_STATE_COLUMNS,
        )?;
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 5;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            Err(err)
        }
    }
}

fn migrate_to_v6(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(
            conn,
            "catalog_sessions",
            CATALOG_SESSION_IMPORT_STATE_COLUMNS,
        )?;
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 6;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            Err(err)
        }
    }
}

fn migrate_to_v7(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 7;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v8(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        drop_legacy_history_record_indexes(conn)?;
        rename_table_if_exists(conn, "work_record_links", "history_record_links")?;
        rename_table_if_exists(conn, "work_record_tags", "history_record_tags")?;
        rename_table_if_exists(conn, "work_records", "history_records")?;
        for table in ["sessions", "runs", "events", "summaries", "files_touched"] {
            rename_column_if_exists(conn, table, "work_record_id", "history_record_id")?;
        }
        rename_column_if_exists(
            conn,
            "history_record_links",
            "work_record_id",
            "history_record_id",
        )?;
        rename_column_if_exists(
            conn,
            "history_record_tags",
            "work_record_id",
            "history_record_id",
        )?;
        rewrite_history_table_names(conn, "sync_outbox", "local_table")?;
        rewrite_history_table_names(conn, "audit_log", "target_table")?;
        drop_fts_table_if_column_exists(conn, "event_search", "work_record_id")?;
        drop_fts_table_if_column_exists(conn, "artifact_search", "work_record_id")?;
        conn.execute_batch(CREATE_TABLES_SQL)?;
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 8;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v9(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 9;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            Err(err)
        }
    }
}

fn migrate_to_v10(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 10;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            Err(err)
        }
    }
}

fn migrate_to_v11(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        rebuild_search_projection(conn)?;
        conn.execute_batch("PRAGMA user_version = 11;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            Err(err)
        }
    }
}

fn migrate_to_v12(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        invalidate_provider_import_indexes(conn)?;
        rebuild_search_projection(conn)?;
        conn.execute_batch("PRAGMA user_version = 12;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            Err(err)
        }
    }
}

fn migrate_to_v13(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 13;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            Err(err)
        }
    }
}

fn migrate_to_v14(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(
            conn,
            "catalog_sessions",
            CATALOG_SESSION_IMPORT_STATE_COLUMNS,
        )?;
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        rebuild_source_import_files_provider_check(conn)?;
        backfill_catalog_session_import_checkpoints(conn)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch("PRAGMA user_version = 14;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v15(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        rebuild_source_import_files_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 15;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v16(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        rebuild_source_import_files_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 16;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v17(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        rebuild_source_import_files_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 17;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v18(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        rebuild_source_import_files_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 18;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v19(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        rebuild_source_import_files_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 19;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v20(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        rebuild_source_import_files_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 20;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v21(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        rebuild_source_import_files_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 21;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v22(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        rebuild_source_import_files_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 22;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v23(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        rebuild_source_import_files_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 23;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v24(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        rebuild_source_import_files_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 24;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v25(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        rebuild_source_import_files_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 25;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v26(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        rebuild_source_import_files_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 26;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v27(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        rebuild_source_import_files_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 27;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v28(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        rebuild_source_import_files_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 28;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v29(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        rebuild_source_import_files_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 29;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v30(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        rebuild_source_import_files_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 30;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn migrate_to_v31(conn: &Connection) -> Result<()> {
    let foreign_keys_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        if stable_sql_views_exist(conn)? {
            drop_stable_sql_views(conn)?;
        }
        rebuild_capture_sources_provider_check(conn)?;
        rebuild_catalog_sessions_provider_check(conn)?;
        rebuild_source_import_files_provider_check(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        create_stable_sql_views(conn)?;
        conn.execute_batch("PRAGMA user_version = 31;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            if foreign_keys_enabled != 0 {
                conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            }
            Err(err)
        }
    }
}

fn create_stable_sql_views(conn: &Connection) -> Result<()> {
    conn.execute_batch(STABLE_SQL_VIEWS_SQL)?;
    Ok(())
}

fn drop_stable_sql_views(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        DROP VIEW IF EXISTS ctx_sessions;
        DROP VIEW IF EXISTS ctx_events;
        DROP VIEW IF EXISTS ctx_files_touched;
        DROP VIEW IF EXISTS ctx_sources;
        "#,
    )?;
    Ok(())
}

fn stable_sql_views_exist(conn: &Connection) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'view' AND name = 'ctx_sessions'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn invalidate_provider_import_indexes(conn: &Connection) -> Result<()> {
    if table_exists(conn, "catalog_sessions")? {
        conn.execute(
            r#"
            UPDATE catalog_sessions
            SET indexed_at_ms = NULL,
                indexed_file_size_bytes = NULL,
                indexed_file_modified_at_ms = NULL,
                indexed_status = 'pending',
                indexed_error = NULL,
                indexed_event_count = NULL
            WHERE indexed_status = 'indexed'
            "#,
            [],
        )?;
    }
    if table_exists(conn, "source_import_files")? {
        conn.execute(
            r#"
            UPDATE source_import_files
            SET indexed_at_ms = NULL,
                indexed_file_size_bytes = NULL,
                indexed_file_modified_at_ms = NULL,
                indexed_status = 'pending',
                indexed_error = NULL
            WHERE indexed_status = 'indexed'
            "#,
            [],
        )?;
    }
    Ok(())
}

fn backfill_catalog_session_import_checkpoints(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "catalog_sessions")? {
        return Ok(());
    }
    conn.execute(
        r#"
        UPDATE catalog_sessions
        SET last_imported_at_ms = indexed_at_ms,
            last_imported_file_size_bytes = indexed_file_size_bytes,
            last_imported_file_modified_at_ms = indexed_file_modified_at_ms,
            last_imported_event_count = indexed_event_count
        WHERE last_imported_file_size_bytes IS NULL
          AND indexed_file_size_bytes IS NOT NULL
        "#,
        [],
    )?;
    Ok(())
}

fn drop_legacy_history_record_indexes(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        DROP INDEX IF EXISTS idx_work_records_primary_vcs_workspace_id;
        DROP INDEX IF EXISTS idx_work_records_source_id;
        DROP INDEX IF EXISTS idx_work_records_last_activity_at_ms;
        DROP INDEX IF EXISTS idx_work_records_created_at;
        DROP INDEX IF EXISTS idx_sessions_work_record_id;
        DROP INDEX IF EXISTS idx_runs_work_record_started_at_ms;
        DROP INDEX IF EXISTS idx_runs_work_record_id;
        DROP INDEX IF EXISTS idx_events_work_record_occurred_at_ms;
        DROP INDEX IF EXISTS idx_events_work_record_id;
        DROP INDEX IF EXISTS idx_work_record_links_work_record_id;
        DROP INDEX IF EXISTS idx_work_record_links_source_id;
        DROP INDEX IF EXISTS idx_summaries_work_record_id;
        DROP INDEX IF EXISTS idx_files_touched_work_record_id;
        DROP INDEX IF EXISTS idx_work_record_tags_tag_id;
        DROP INDEX IF EXISTS idx_work_record_tags_source_id;
        "#,
    )?;
    Ok(())
}

fn rename_table_if_exists(conn: &Connection, old: &str, new: &str) -> Result<()> {
    if table_exists(conn, old)? && !table_exists(conn, new)? {
        conn.execute(&format!("ALTER TABLE {old} RENAME TO {new}"), [])?;
    }
    Ok(())
}

fn rename_column_if_exists(conn: &Connection, table: &str, old: &str, new: &str) -> Result<()> {
    if table_exists(conn, table)?
        && table_has_column(conn, table, old)?
        && !table_has_column(conn, table, new)?
    {
        conn.execute(
            &format!("ALTER TABLE {table} RENAME COLUMN {old} TO {new}"),
            [],
        )?;
    }
    Ok(())
}

fn rewrite_history_table_names(conn: &Connection, table: &str, column: &str) -> Result<()> {
    if !table_exists(conn, table)? || !table_has_column(conn, table, column)? {
        return Ok(());
    }
    conn.execute(
        &format!(
            "UPDATE {table}
             SET {column} = CASE {column}
                WHEN 'work_records' THEN 'history_records'
                WHEN 'work_record_links' THEN 'history_record_links'
                WHEN 'work_record_tags' THEN 'history_record_tags'
                ELSE {column}
             END
             WHERE {column} IN ('work_records', 'work_record_links', 'work_record_tags')"
        ),
        [],
    )?;
    Ok(())
}

fn drop_fts_table_if_column_exists(conn: &Connection, table: &str, column: &str) -> Result<()> {
    if table_exists(conn, table)? && table_has_column(conn, table, column)? {
        conn.execute(&format!("DROP TABLE {table}"), [])?;
    }
    Ok(())
}

fn rebuild_capture_sources_provider_check(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "capture_sources")? {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        return Ok(());
    }

    let recreate_views = stable_sql_views_exist(conn)?;
    if recreate_views {
        drop_stable_sql_views(conn)?;
    }
    conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS capture_sources_new;
        CREATE TABLE capture_sources_new (
            id TEXT PRIMARY KEY NOT NULL,
            kind TEXT NOT NULL CHECK (kind IN ('provider_import', 'provider_hook', 'direct_cli', 'manual')),
            provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'opencode', 'openloaf', 'kilo', 'kiro_cli', 'crush', 'goose', 'antigravity', 'gemini', 'cursor', 'windsurf', 'zed', 'copilot_cli', 'factory_ai_droid', 'qwen_code', 'kimi_code_cli', 'autohand_code', 'iflow_cli', 'jazz', 'forgecode', 'deepagents', 'mistral_vibe', 'mux', 'reasonix', 'kode', 'neovate', 'command_code', 'terramind', 'rovodev', 'cortex_code', 'openclaw', 'hermes', 'nanoclaw', 'astrbot', 'shelley', 'continue', 'openhands', 'cline', 'roo_code', 'dexto', 'lingma', 'pochi', 'codebuddy', 'aider_desk', 'auggie', 'firebender', 'shell', 'git', 'jj', 'gh', 'custom', 'unknown')),
            machine_id TEXT NOT NULL,
            process_id INTEGER,
            cwd TEXT,
            raw_source_path TEXT,
            external_session_id TEXT,
            started_at_ms INTEGER NOT NULL,
            ended_at_ms INTEGER,
            fidelity TEXT NOT NULL CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
            visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full', 'withheld')),
            sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed', 'withheld')),
            sync_version INTEGER NOT NULL DEFAULT 0,
            metadata_json TEXT NOT NULL DEFAULT '{}'
        );
        INSERT INTO capture_sources_new
        (id, kind, provider, machine_id, process_id, cwd, raw_source_path, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json)
        SELECT id, kind, provider, machine_id, process_id, cwd, raw_source_path, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json
        FROM capture_sources;
        DROP TABLE capture_sources;
        ALTER TABLE capture_sources_new RENAME TO capture_sources;
        "#,
    )?;
    if recreate_views {
        create_stable_sql_views(conn)?;
    }
    Ok(())
}

fn rebuild_catalog_sessions_provider_check(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "catalog_sessions")? {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        return Ok(());
    }

    let recreate_views = stable_sql_views_exist(conn)?;
    if recreate_views {
        drop_stable_sql_views(conn)?;
    }
    ensure_columns(
        conn,
        "catalog_sessions",
        CATALOG_SESSION_IMPORT_STATE_COLUMNS,
    )?;
    conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS catalog_sessions_new;
        CREATE TABLE catalog_sessions_new (
            source_path TEXT PRIMARY KEY NOT NULL,
            provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'opencode', 'openloaf', 'kilo', 'kiro_cli', 'crush', 'goose', 'antigravity', 'gemini', 'cursor', 'windsurf', 'zed', 'copilot_cli', 'factory_ai_droid', 'qwen_code', 'kimi_code_cli', 'autohand_code', 'iflow_cli', 'jazz', 'forgecode', 'deepagents', 'mistral_vibe', 'mux', 'reasonix', 'kode', 'neovate', 'command_code', 'terramind', 'rovodev', 'cortex_code', 'openclaw', 'hermes', 'nanoclaw', 'astrbot', 'shelley', 'continue', 'openhands', 'cline', 'roo_code', 'dexto', 'lingma', 'pochi', 'codebuddy', 'aider_desk', 'auggie', 'firebender', 'shell', 'git', 'jj', 'gh', 'custom', 'unknown')),
            source_format TEXT NOT NULL,
            source_root TEXT NOT NULL,
            external_session_id TEXT,
            parent_external_session_id TEXT,
            agent_type TEXT NOT NULL CHECK (agent_type IN ('primary', 'subagent', 'agent_team_member', 'reviewer', 'implementer', 'unknown')),
            role_hint TEXT,
            external_agent_id TEXT,
            cwd TEXT,
            session_started_at_ms INTEGER,
            file_size_bytes INTEGER NOT NULL,
            file_modified_at_ms INTEGER NOT NULL,
            cataloged_at_ms INTEGER NOT NULL,
            is_stale INTEGER NOT NULL DEFAULT 0,
            indexed_at_ms INTEGER,
            indexed_file_size_bytes INTEGER,
            indexed_file_modified_at_ms INTEGER,
            indexed_status TEXT NOT NULL DEFAULT 'pending' CHECK (indexed_status IN ('pending', 'indexed', 'failed')),
            indexed_error TEXT,
            indexed_event_count INTEGER,
            last_imported_at_ms INTEGER,
            last_imported_file_size_bytes INTEGER,
            last_imported_file_modified_at_ms INTEGER,
            last_imported_file_sha256 TEXT,
            last_imported_event_count INTEGER,
            metadata_json TEXT NOT NULL DEFAULT '{}'
        );
        INSERT INTO catalog_sessions_new
        (source_path, provider, source_format, source_root, external_session_id, parent_external_session_id, agent_type, role_hint, external_agent_id, cwd, session_started_at_ms, file_size_bytes, file_modified_at_ms, cataloged_at_ms, is_stale, indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_status, indexed_error, indexed_event_count, last_imported_at_ms, last_imported_file_size_bytes, last_imported_file_modified_at_ms, last_imported_file_sha256, last_imported_event_count, metadata_json)
        SELECT source_path, provider, source_format, source_root, external_session_id, parent_external_session_id, agent_type, role_hint, external_agent_id, cwd, session_started_at_ms, file_size_bytes, file_modified_at_ms, cataloged_at_ms, is_stale, indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_status, indexed_error, indexed_event_count, last_imported_at_ms, last_imported_file_size_bytes, last_imported_file_modified_at_ms, last_imported_file_sha256, last_imported_event_count, metadata_json
        FROM catalog_sessions;
        DROP TABLE catalog_sessions;
        ALTER TABLE catalog_sessions_new RENAME TO catalog_sessions;
        "#,
    )?;
    if recreate_views {
        create_stable_sql_views(conn)?;
    }
    Ok(())
}

fn rebuild_source_import_files_provider_check(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "source_import_files")? {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        return Ok(());
    }

    let recreate_views = stable_sql_views_exist(conn)?;
    if recreate_views {
        drop_stable_sql_views(conn)?;
    }
    conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS source_import_files_new;
        CREATE TABLE source_import_files_new (
            provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'opencode', 'openloaf', 'kilo', 'kiro_cli', 'crush', 'goose', 'antigravity', 'gemini', 'cursor', 'windsurf', 'zed', 'copilot_cli', 'factory_ai_droid', 'qwen_code', 'kimi_code_cli', 'autohand_code', 'iflow_cli', 'jazz', 'forgecode', 'deepagents', 'mistral_vibe', 'mux', 'reasonix', 'kode', 'neovate', 'command_code', 'terramind', 'rovodev', 'cortex_code', 'openclaw', 'hermes', 'nanoclaw', 'astrbot', 'shelley', 'continue', 'openhands', 'cline', 'roo_code', 'dexto', 'lingma', 'pochi', 'codebuddy', 'aider_desk', 'auggie', 'firebender', 'shell', 'git', 'jj', 'gh', 'custom', 'unknown')),
            source_format TEXT NOT NULL,
            source_root TEXT NOT NULL,
            source_path TEXT NOT NULL,
            file_size_bytes INTEGER NOT NULL,
            file_modified_at_ms INTEGER NOT NULL,
            observed_at_ms INTEGER NOT NULL,
            is_stale INTEGER NOT NULL DEFAULT 0,
            indexed_at_ms INTEGER,
            indexed_file_size_bytes INTEGER,
            indexed_file_modified_at_ms INTEGER,
            indexed_status TEXT NOT NULL DEFAULT 'pending' CHECK (indexed_status IN ('pending', 'indexed', 'failed')),
            indexed_error TEXT,
            metadata_json TEXT NOT NULL DEFAULT '{}',
            PRIMARY KEY (provider, source_root, source_path)
        );
        INSERT INTO source_import_files_new
        (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms, is_stale, indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_status, indexed_error, metadata_json)
        SELECT provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms, is_stale, indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_status, indexed_error, metadata_json
        FROM source_import_files;
        DROP TABLE source_import_files;
        ALTER TABLE source_import_files_new RENAME TO source_import_files;
        "#,
    )?;
    if recreate_views {
        create_stable_sql_views(conn)?;
    }
    Ok(())
}

fn create_fts_tables_if_supported(conn: &Connection) -> Result<()> {
    match conn.execute_batch(FTS_TABLES_SQL) {
        Ok(()) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(error, message))
            if is_missing_fts_module(error.extended_code, message.as_deref()) =>
        {
            Ok(())
        }
        Err(err) => Err(StoreError::Sql(err)),
    }
}

fn is_missing_fts_module(extended_code: i32, message: Option<&str>) -> bool {
    extended_code == rusqlite::ffi::SQLITE_ERROR
        && message
            .map(|value| value.contains("no such module: fts5"))
            .unwrap_or(false)
}

struct ColumnSpec {
    name: &'static str,
    definition: &'static str,
}

fn ensure_columns(conn: &Connection, table: &str, columns: &[ColumnSpec]) -> Result<()> {
    for column in columns {
        if !table_has_column(conn, table, column.name)? {
            let sql = format!("ALTER TABLE {table} ADD COLUMN {}", column.definition);
            conn.execute(&sql, [])?;
        }
    }
    Ok(())
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let sql = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn reject_provider_event_hash_conflict(conn: &Connection, dedupe_key: &str) -> Result<()> {
    let Some(parsed) = parse_provider_event_dedupe_key(dedupe_key) else {
        return Ok(());
    };
    let prefix = provider_event_dedupe_key_prefix(&parsed);
    let upper_bound = provider_event_dedupe_key_upper_bound(&prefix);
    let mut stmt = conn.prepare(
        "SELECT dedupe_key FROM events
         WHERE dedupe_key >= ?1 AND dedupe_key < ?2
         ORDER BY dedupe_key",
    )?;
    let rows = stmt.query_map(params![prefix, upper_bound], |row| row.get::<_, String>(0))?;
    reject_provider_event_hash_conflict_from_rows(dedupe_key, rows)
}

fn reject_provider_event_hash_conflict_tx(tx: &Transaction<'_>, dedupe_key: &str) -> Result<()> {
    let Some(parsed) = parse_provider_event_dedupe_key(dedupe_key) else {
        return Ok(());
    };
    let prefix = provider_event_dedupe_key_prefix(&parsed);
    let upper_bound = provider_event_dedupe_key_upper_bound(&prefix);
    let mut stmt = tx.prepare(
        "SELECT dedupe_key FROM events
         WHERE dedupe_key >= ?1 AND dedupe_key < ?2
         ORDER BY dedupe_key",
    )?;
    let rows = stmt.query_map(params![prefix, upper_bound], |row| row.get::<_, String>(0))?;
    reject_provider_event_hash_conflict_from_rows(dedupe_key, rows)
}

fn reject_provider_event_hash_conflict_from_rows(
    dedupe_key: &str,
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<String>>,
) -> Result<()> {
    let Some(incoming) = parse_provider_event_dedupe_key(dedupe_key) else {
        return Ok(());
    };
    for row in rows {
        let existing_key = row?;
        let Some(existing) = parse_provider_event_dedupe_key(&existing_key) else {
            continue;
        };
        if existing.has_same_event_identity(&incoming)
            && existing.payload_hash != incoming.payload_hash
        {
            return Err(StoreError::ProviderEventConflict {
                provider: incoming.provider,
                external_session_id: incoming.external_session_id,
                provider_index: incoming.provider_index,
                existing_hash: existing.payload_hash,
                new_hash: incoming.payload_hash,
            });
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct ParsedProviderEventDedupeKey {
    provider: String,
    external_session_id: String,
    source_id: Option<String>,
    provider_index: u64,
    payload_hash: String,
}

impl ParsedProviderEventDedupeKey {
    fn has_same_event_identity(&self, other: &Self) -> bool {
        self.provider == other.provider
            && self.external_session_id == other.external_session_id
            && self.source_id == other.source_id
            && self.provider_index == other.provider_index
    }
}

fn provider_event_dedupe_key_prefix(parsed: &ParsedProviderEventDedupeKey) -> String {
    if let Some(source_id) = &parsed.source_id {
        format!("provider-source:{source_id}:{}:", parsed.provider_index)
    } else {
        format!(
            "provider:{}:{}:{}:",
            parsed.provider, parsed.external_session_id, parsed.provider_index
        )
    }
}

fn provider_event_dedupe_key_upper_bound(prefix: &str) -> String {
    let mut upper_bound = prefix.to_owned();
    upper_bound.push(char::MAX);
    upper_bound
}

fn parse_provider_event_dedupe_key(dedupe_key: &str) -> Option<ParsedProviderEventDedupeKey> {
    if let Some(rest) = dedupe_key.strip_prefix("provider-source:") {
        let mut parts = rest.splitn(3, ':');
        let source_id = parts.next()?.to_owned();
        let provider_index = parts.next()?.parse().ok()?;
        let payload_hash = parts.next()?.to_owned();
        if source_id.is_empty() || payload_hash.is_empty() {
            return None;
        }
        return Some(ParsedProviderEventDedupeKey {
            provider: "provider-source".to_owned(),
            external_session_id: source_id.clone(),
            source_id: Some(source_id),
            provider_index,
            payload_hash,
        });
    }

    let mut parts = dedupe_key.splitn(5, ':');
    let prefix = parts.next()?;
    if prefix != "provider" {
        return None;
    }
    let provider = parts.next()?.to_owned();
    let external_session_id = parts.next()?.to_owned();
    let provider_index = parts.next()?.parse().ok()?;
    let payload_hash = parts.next()?.to_owned();
    if provider.is_empty() || external_session_id.is_empty() || payload_hash.is_empty() {
        None
    } else {
        Some(ParsedProviderEventDedupeKey {
            provider,
            external_session_id,
            source_id: None,
            provider_index,
            payload_hash,
        })
    }
}

fn fts_match_query(query: &str) -> Option<String> {
    let terms = query
        .split_whitespace()
        .map(|term| term.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-'))
        .filter(|term| term.chars().any(char::is_alphanumeric))
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" AND "))
    }
}

fn backfill_legacy_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        UPDATE history_records
        SET summary = body
        WHERE summary IS NULL;

        UPDATE history_records
        SET created_at_ms = COALESCE(CAST(strftime('%s', created_at) AS INTEGER) * 1000, created_at_ms)
        WHERE created_at_ms = 0 AND created_at IS NOT NULL;

        UPDATE history_records
        SET updated_at_ms = COALESCE(CAST(strftime('%s', updated_at) AS INTEGER) * 1000, updated_at_ms)
        WHERE updated_at_ms = 0 AND updated_at IS NOT NULL;

        UPDATE history_records
        SET started_at_ms = created_at_ms
        WHERE started_at_ms IS NULL AND created_at_ms != 0;

        UPDATE history_records
        SET last_activity_at_ms = CASE
            WHEN updated_at_ms != 0 THEN updated_at_ms
            WHEN created_at_ms != 0 THEN created_at_ms
            ELSE last_activity_at_ms
        END
        WHERE last_activity_at_ms = 0;
        "#,
    )?;
    Ok(())
}

fn count_foreign_key_failures(conn: &Connection) -> Result<i64> {
    let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
    let mut rows = stmt.query([])?;
    let mut count = 0;
    while rows.next()?.is_some() {
        count += 1;
    }
    Ok(count)
}

fn timestamp_ms(value: DateTime<Utc>) -> i64 {
    value.timestamp_millis()
}

fn capped_i64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

fn nonnegative_i64_to_u64(value: i64) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))
}

fn nonnegative_i64_to_u32(value: i64) -> rusqlite::Result<u32> {
    u32::try_from(value).map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))
}

fn time_ms(value: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp_millis(value).unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut value = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut value, "{byte:02x}");
    }
    value
}

fn ensure_regular_blob_file(id: Uuid, path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_file() {
        Ok(())
    } else {
        Err(StoreError::ArchiveArtifactNonRegularFile {
            id,
            path: path.to_path_buf(),
        })
    }
}

#[derive(Debug, Default)]
struct BlobWriteGuard {
    created_paths: Vec<PathBuf>,
    committed: bool,
}

impl BlobWriteGuard {
    fn commit(&mut self) {
        self.committed = true;
        self.created_paths.clear();
    }
}

impl Drop for BlobWriteGuard {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for path in self.created_paths.iter().rev() {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(unix)]
fn restrict_private_dir(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_private_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_private_file(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_private_file(_path: &Path) -> Result<()> {
    Ok(())
}

pub fn validate_archive_version(archive: &SessionHistoryArchive) -> Result<()> {
    if matches!((archive.schema_version, archive.version), (1, 1) | (2, 2)) {
        Ok(())
    } else {
        Err(StoreError::UnsupportedArchiveVersion(
            archive.schema_version.max(archive.version),
        ))
    }
}

fn reject_import_conflicts(tx: &Transaction<'_>, archive: &SessionHistoryArchive) -> Result<()> {
    for record in &archive.records {
        if row_exists(tx, "history_records", record.id)? {
            return Err(StoreError::ImportConflict {
                kind: "record",
                id: record.id,
            });
        }
    }
    reject_rich_import_conflicts(tx, archive)?;
    Ok(())
}

fn reject_capture_source_import_conflict(tx: &Transaction<'_>, source_id: Uuid) -> Result<()> {
    if row_exists(tx, "capture_sources", source_id)? {
        return Err(StoreError::ImportConflict {
            kind: "capture_source",
            id: source_id,
        });
    }
    Ok(())
}

fn reject_import_invariant_conflicts(
    tx: &Transaction<'_>,
    archive: &SessionHistoryArchive,
) -> Result<()> {
    if archive.schema_version < 2 && archive.version < 2 {
        return Ok(());
    }

    for event in &archive.events {
        if let Some(dedupe_key) = &event.dedupe_key {
            reject_provider_event_hash_conflict_tx(tx, dedupe_key)?;
        }
    }
    Ok(())
}

fn row_exists(tx: &Transaction<'_>, table: &str, id: Uuid) -> Result<bool> {
    let sql = format!("SELECT 1 FROM {table} WHERE id = ?1");
    Ok(tx
        .query_row(&sql, params![id.to_string()], |_| Ok(()))
        .optional()?
        .is_some())
}

fn reject_rich_import_conflicts(
    tx: &Transaction<'_>,
    archive: &SessionHistoryArchive,
) -> Result<()> {
    if archive.schema_version < 2 && archive.version < 2 {
        return Ok(());
    }

    for source in &archive.capture_sources {
        reject_entity_conflict(
            existing_capture_source_by_id(tx, source.id)?,
            source,
            "capture_source",
            source.id,
        )?;
    }
    for workspace in &archive.vcs_workspaces {
        reject_entity_conflict(
            existing_vcs_workspace_by_id(tx, workspace.id)?,
            workspace,
            "vcs_workspace",
            workspace.id,
        )?;
        reject_entity_conflict(
            existing_vcs_workspace_by_identity(tx, workspace)?,
            workspace,
            "vcs_workspace",
            workspace.id,
        )?;
    }
    for artifact in &archive.artifact_records {
        reject_entity_conflict(
            existing_artifact_by_id(tx, artifact.id)?,
            artifact,
            "artifact",
            artifact.id,
        )?;
        reject_entity_conflict(
            existing_artifact_by_identity(tx, artifact)?,
            artifact,
            "artifact",
            artifact.id,
        )?;
    }
    for session in &archive.sessions {
        reject_entity_conflict(
            existing_session_by_id(tx, session.id)?,
            session,
            "session",
            session.id,
        )?;
        if let Some(external_session_id) = &session.external_session_id {
            reject_entity_conflict(
                existing_session_by_external_session(tx, session.provider, external_session_id)?,
                session,
                "session",
                session.id,
            )?;
        }
    }
    for run in &archive.runs {
        reject_entity_conflict(existing_run_by_id(tx, run.id)?, run, "run", run.id)?;
    }
    for event in &archive.events {
        reject_entity_conflict(
            existing_event_by_id(tx, event.id)?,
            event,
            "event",
            event.id,
        )?;
        reject_entity_conflict(
            existing_event_by_seq(tx, event.seq)?,
            event,
            "event",
            event.id,
        )?;
        if let Some(dedupe_key) = &event.dedupe_key {
            reject_provider_event_hash_conflict_tx(tx, dedupe_key)?;
            reject_entity_conflict(
                existing_event_by_dedupe_key(tx, dedupe_key)?,
                event,
                "event",
                event.id,
            )?;
        }
    }
    for change in &archive.vcs_changes {
        reject_entity_conflict(
            existing_vcs_change_by_id(tx, change.id)?,
            change,
            "vcs_change",
            change.id,
        )?;
        reject_entity_conflict(
            existing_vcs_change_by_identity(tx, change)?,
            change,
            "vcs_change",
            change.id,
        )?;
    }
    for summary in &archive.summaries {
        reject_entity_conflict(
            existing_summary_by_id(tx, summary.id)?,
            summary,
            "summary",
            summary.id,
        )?;
    }
    for file in &archive.files_touched {
        reject_entity_conflict(
            existing_file_touched_by_id(tx, file.id)?,
            file,
            "file_touched",
            file.id,
        )?;
    }
    for link in &archive.history_record_links {
        reject_entity_conflict(
            existing_history_record_link_by_id(tx, link.id)?,
            link,
            "history_record_link",
            link.id,
        )?;
        reject_entity_conflict(
            existing_history_record_link_by_identity(tx, link)?,
            link,
            "history_record_link",
            link.id,
        )?;
    }
    Ok(())
}

fn reject_archive_event_internal_conflicts(archive: &SessionHistoryArchive) -> Result<()> {
    let mut seen_seq: HashMap<u64, &Event> = HashMap::new();
    let mut seen_provider_events: HashMap<(String, String, Option<String>, u64), String> =
        HashMap::new();

    for event in &archive.events {
        if let Some(existing) = seen_seq.insert(event.seq, event) {
            if existing != event {
                return Err(StoreError::ImportConflict {
                    kind: "event",
                    id: event.id,
                });
            }
        }

        let Some(dedupe_key) = &event.dedupe_key else {
            continue;
        };
        let Some(parsed) = parse_provider_event_dedupe_key(dedupe_key) else {
            continue;
        };
        let key = (
            parsed.provider,
            parsed.external_session_id,
            parsed.source_id,
            parsed.provider_index,
        );
        if let Some(existing_hash) = seen_provider_events.get(&key) {
            if existing_hash != &parsed.payload_hash {
                return Err(StoreError::ProviderEventConflict {
                    provider: key.0,
                    external_session_id: key.1,
                    provider_index: key.3,
                    existing_hash: existing_hash.clone(),
                    new_hash: parsed.payload_hash,
                });
            }
        } else {
            seen_provider_events.insert(key, parsed.payload_hash);
        }
    }

    Ok(())
}

fn reject_entity_conflict<T: PartialEq>(
    existing: Option<T>,
    incoming: &T,
    kind: &'static str,
    id: Uuid,
) -> Result<()> {
    if let Some(existing) = existing {
        if existing != *incoming {
            return Err(StoreError::ImportConflict { kind, id });
        }
    }
    Ok(())
}

fn existing_capture_source_by_id(tx: &Transaction<'_>, id: Uuid) -> Result<Option<CaptureSource>> {
    tx.query_row(
        "SELECT id, kind, provider, machine_id, process_id, cwd, raw_source_path, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json FROM capture_sources WHERE id = ?1",
        params![id.to_string()],
        capture_source_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_session_by_id(tx: &Transaction<'_>, id: Uuid) -> Result<Option<Session>> {
    tx.query_row(
        session_select_sql("WHERE id = ?1").as_str(),
        params![id.to_string()],
        session_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_session_by_external_session(
    tx: &Transaction<'_>,
    provider: CaptureProvider,
    external_session_id: &str,
) -> Result<Option<Session>> {
    tx.query_row(
        session_select_sql(
            "WHERE provider = ?1 AND external_session_id = ?2 ORDER BY started_at_ms DESC LIMIT 1",
        )
        .as_str(),
        params![provider.as_str(), external_session_id],
        session_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_run_by_id(tx: &Transaction<'_>, id: Uuid) -> Result<Option<Run>> {
    tx.query_row(
        run_select_sql("WHERE id = ?1").as_str(),
        params![id.to_string()],
        run_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_event_by_id(tx: &Transaction<'_>, id: Uuid) -> Result<Option<Event>> {
    tx.query_row(
        event_select_sql("WHERE id = ?1").as_str(),
        params![id.to_string()],
        event_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_event_by_dedupe_key(tx: &Transaction<'_>, dedupe_key: &str) -> Result<Option<Event>> {
    tx.query_row(
        event_select_sql("WHERE dedupe_key = ?1").as_str(),
        params![dedupe_key],
        event_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_event_by_seq(tx: &Transaction<'_>, seq: u64) -> Result<Option<Event>> {
    tx.query_row(
        event_select_sql("WHERE seq = ?1").as_str(),
        params![seq as i64],
        event_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_artifact_by_id(tx: &Transaction<'_>, id: Uuid) -> Result<Option<Artifact>> {
    tx.query_row(
        artifact_select_sql("WHERE id = ?1").as_str(),
        params![id.to_string()],
        artifact_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_artifact_by_hash_kind(
    tx: &Transaction<'_>,
    blob_hash: &str,
    kind: ArtifactKind,
) -> Result<Option<Artifact>> {
    tx.query_row(
        artifact_select_sql("WHERE blob_hash = ?1 AND kind = ?2").as_str(),
        params![blob_hash, kind.as_str()],
        artifact_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_artifact_by_identity(
    tx: &Transaction<'_>,
    artifact: &Artifact,
) -> Result<Option<Artifact>> {
    existing_artifact_by_hash_kind(tx, &artifact.blob_hash, artifact.kind)
}

fn existing_vcs_workspace_by_id(tx: &Transaction<'_>, id: Uuid) -> Result<Option<VcsWorkspace>> {
    tx.query_row(
        vcs_workspace_select_sql("WHERE id = ?1").as_str(),
        params![id.to_string()],
        vcs_workspace_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_vcs_workspace_by_identity(
    tx: &Transaction<'_>,
    workspace: &VcsWorkspace,
) -> Result<Option<VcsWorkspace>> {
    tx.query_row(
        vcs_workspace_select_sql("WHERE kind = ?1 AND repo_fingerprint = ?2").as_str(),
        params![workspace.kind.as_str(), workspace.repo_fingerprint.as_str()],
        vcs_workspace_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_vcs_change_by_id(tx: &Transaction<'_>, id: Uuid) -> Result<Option<VcsChange>> {
    tx.query_row(
        vcs_change_select_sql("WHERE id = ?1").as_str(),
        params![id.to_string()],
        vcs_change_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_vcs_change_by_identity(
    tx: &Transaction<'_>,
    change: &VcsChange,
) -> Result<Option<VcsChange>> {
    tx.query_row(
        vcs_change_select_sql("WHERE vcs_workspace_id = ?1 AND kind = ?2 AND change_id = ?3")
            .as_str(),
        params![
            change.vcs_workspace_id.to_string(),
            change.kind.as_str(),
            change.change_id.as_str()
        ],
        vcs_change_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_summary_by_id(tx: &Transaction<'_>, id: Uuid) -> Result<Option<Summary>> {
    tx.query_row(
        summary_select_sql("WHERE id = ?1").as_str(),
        params![id.to_string()],
        summary_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_file_touched_by_id(tx: &Transaction<'_>, id: Uuid) -> Result<Option<FileTouched>> {
    tx.query_row(
        file_touched_select_sql("WHERE id = ?1").as_str(),
        params![id.to_string()],
        file_touched_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_history_record_link_by_id(
    tx: &Transaction<'_>,
    id: Uuid,
) -> Result<Option<HistoryRecordLink>> {
    tx.query_row(
        history_record_link_select_sql("WHERE id = ?1").as_str(),
        params![id.to_string()],
        history_record_link_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_history_record_link_by_identity(
    tx: &Transaction<'_>,
    link: &HistoryRecordLink,
) -> Result<Option<HistoryRecordLink>> {
    tx.query_row(
        history_record_link_select_sql(
            "WHERE history_record_id = ?1 AND target_type = ?2 AND target_id = ?3 AND link_type = ?4",
        )
        .as_str(),
        params![
            link.history_record_id.to_string(),
            link.target_type.as_str(),
            link.target_id.to_string(),
            link.link_type.as_str()
        ],
        history_record_link_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

#[cfg(test)]
mod archive_validation_tests {
    use super::*;

    fn tempdir() -> tempfile::TempDir {
        let root = std::env::current_dir().unwrap().join("target/test-data");
        fs::create_dir_all(&root).unwrap();
        tempfile::Builder::new()
            .prefix("ctx-history-store-archive-validation-")
            .tempdir_in(root)
            .unwrap()
    }

    fn fixed_time() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-23T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn artifact(id: Uuid, blob_hash: String, byte_size: u64) -> Artifact {
        Artifact {
            id,
            kind: ArtifactKind::Markdown,
            blob_path: object_relative_path(&blob_hash),
            blob_hash,
            byte_size,
            media_type: Some("text/markdown".into()),
            preview_text: Some("synthetic local preview blob".into()),
            redaction_state: RedactionState::LocalPreview,
            timestamps: EntityTimestamps {
                created_at: fixed_time(),
                updated_at: fixed_time(),
            },
            source_id: None,
            sync: SyncMetadata {
                visibility: Visibility::LocalOnly,
                fidelity: Fidelity::Imported,
                sync_state: SyncState::LocalOnly,
                sync_version: 0,
                deleted_at: None,
                metadata: serde_json::json!({}),
            },
        }
    }

    fn write_blob(blob_dir: &Path, blob_hash: &str, content: &[u8]) {
        let path = blob_dir.join(&blob_hash[..2]).join(blob_hash);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn assert_artifact_error(
        error: StoreError,
        matches_expected: impl FnOnce(&StoreError) -> bool,
    ) {
        assert!(
            matches_expected(&error),
            "unexpected archive artifact validation error: {error:?}"
        );
    }

    #[test]
    fn archive_blob_validation_fails_closed_when_blob_is_missing() {
        let temp = tempdir();
        let content = b"missing synthetic blob";
        let artifact = artifact(new_id(), sha256_hex(content), content.len() as u64);

        let error = validate_archive_artifact_record_blob(temp.path(), &artifact).unwrap_err();
        assert_artifact_error(
            error,
            |error| matches!(error, StoreError::ArchiveArtifactMissingContent { id } if *id == artifact.id),
        );
    }

    #[test]
    fn archive_blob_validation_fails_closed_when_hash_differs() {
        let temp = tempdir();
        let stored_content = b"stored bytes";
        let expected_content = b"expected bytes";
        let artifact = artifact(
            new_id(),
            sha256_hex(expected_content),
            stored_content.len() as u64,
        );
        write_blob(temp.path(), &artifact.blob_hash, stored_content);

        let error = validate_archive_artifact_record_blob(temp.path(), &artifact).unwrap_err();
        assert_artifact_error(
            error,
            |error| matches!(error, StoreError::ArchiveArtifactHashMismatch { id } if *id == artifact.id),
        );
    }

    #[test]
    fn archive_blob_validation_fails_closed_when_byte_size_differs() {
        let temp = tempdir();
        let content = b"size checked bytes";
        let artifact = artifact(new_id(), sha256_hex(content), content.len() as u64 + 1);
        write_blob(temp.path(), &artifact.blob_hash, content);

        let error = validate_archive_artifact_record_blob(temp.path(), &artifact).unwrap_err();
        assert_artifact_error(
            error,
            |error| matches!(error, StoreError::ArchiveArtifactSizeMismatch { id } if *id == artifact.id),
        );
    }

    #[test]
    fn archive_blob_validation_fails_closed_when_blob_path_mismatches_hash() {
        let temp = tempdir();
        let content = b"path checked bytes";
        let mut artifact = artifact(new_id(), sha256_hex(content), content.len() as u64);
        artifact.blob_path = "objects/ff/not-the-recorded-hash".into();
        write_blob(temp.path(), &artifact.blob_hash, content);

        let error = validate_archive_artifact_record_blob(temp.path(), &artifact).unwrap_err();
        assert_artifact_error(
            error,
            |error| matches!(error, StoreError::ArchiveArtifactPathMismatch { id } if *id == artifact.id),
        );
    }

    #[test]
    fn archive_blob_validation_fails_closed_when_blob_is_not_regular_file() {
        let temp = tempdir();
        let content = b"directory at blob path";
        let artifact = artifact(new_id(), sha256_hex(content), content.len() as u64);
        let path = temp
            .path()
            .join(&artifact.blob_hash[..2])
            .join(&artifact.blob_hash);
        fs::create_dir_all(&path).unwrap();

        let error = validate_archive_artifact_record_blob(temp.path(), &artifact).unwrap_err();
        assert_artifact_error(
            error,
            |error| matches!(error, StoreError::ArchiveArtifactNonRegularFile { id, .. } if *id == artifact.id),
        );
    }

    #[test]
    fn archive_version_validation_rejects_future_version() {
        let archive = SessionHistoryArchive {
            schema_version: 3,
            version: 3,
            ..SessionHistoryArchive::default()
        };

        let error = validate_archive_version(&archive).unwrap_err();
        assert!(matches!(
            error,
            StoreError::UnsupportedArchiveVersion(version) if version == 3
        ));
    }
}

fn expected_archive_blob_path(id: Uuid, blob_hash: &str) -> Result<String> {
    if blob_hash.get(..2).is_none() {
        return Err(StoreError::ArchiveArtifactPathMismatch { id });
    }
    Ok(object_relative_path(blob_hash))
}

fn validate_archive_artifact_record_blobs(
    blob_dir: &Path,
    archive: &SessionHistoryArchive,
) -> Result<()> {
    for artifact in &archive.artifact_records {
        validate_archive_artifact_record_blob(blob_dir, artifact)?;
    }
    Ok(())
}

fn validate_archive_artifact_record_blob(blob_dir: &Path, artifact: &Artifact) -> Result<()> {
    let expected_path = expected_archive_blob_path(artifact.id, &artifact.blob_hash)?;
    let legacy_path = {
        let shard = &artifact.blob_hash[..2];
        format!("{LEGACY_BLOBS_DIR}/{shard}/{}", artifact.blob_hash)
    };
    if artifact.blob_path != expected_path && artifact.blob_path != legacy_path {
        return Err(StoreError::ArchiveArtifactPathMismatch { id: artifact.id });
    }

    let absolute_path = blob_dir
        .join(&artifact.blob_hash[..2])
        .join(&artifact.blob_hash);
    if !absolute_path.exists() {
        return Err(StoreError::ArchiveArtifactMissingContent { id: artifact.id });
    }
    ensure_regular_blob_file(artifact.id, &absolute_path)?;
    let content = fs::read(&absolute_path)?;
    let hash = sha256_hex(&content);
    if hash != artifact.blob_hash {
        return Err(StoreError::ArchiveArtifactHashMismatch { id: artifact.id });
    }
    if content.len() as u64 != artifact.byte_size {
        return Err(StoreError::ArchiveArtifactSizeMismatch { id: artifact.id });
    }
    Ok(())
}

fn upsert_capture_source_tx(
    tx: &Transaction<'_>,
    source_id: Uuid,
    source: &CaptureSourceDescriptor,
    occurred_at: DateTime<Utc>,
    fidelity: Fidelity,
) -> Result<()> {
    let occurred_at_ms = timestamp_ms(occurred_at);
    tx.execute(
        r#"
        INSERT INTO capture_sources
        (
            id, kind, provider, machine_id, process_id, cwd, raw_source_path,
            external_session_id, started_at_ms, ended_at_ms, fidelity,
            visibility, sync_state, sync_version, metadata_json
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10, 'local_only', 'local_only', 0, '{}')
        ON CONFLICT(id) DO UPDATE SET
            kind = excluded.kind,
            provider = excluded.provider,
            machine_id = excluded.machine_id,
            process_id = excluded.process_id,
            cwd = excluded.cwd,
            raw_source_path = excluded.raw_source_path,
            external_session_id = excluded.external_session_id,
            started_at_ms = excluded.started_at_ms,
            fidelity = excluded.fidelity
        "#,
        params![
            source_id.to_string(),
            source.kind.as_str(),
            source.provider.as_str(),
            source.machine_id.as_str(),
            source.process_id.map(i64::from),
            source.cwd.as_deref(),
            source.raw_source_path.as_deref(),
            source.external_session_id.as_deref(),
            occurred_at_ms,
            fidelity.as_str(),
        ],
    )?;
    Ok(())
}

fn import_rich_archive_entities_tx(
    tx: &Transaction<'_>,
    blob_dir: &Path,
    archive: &SessionHistoryArchive,
    _blob_guard: &mut BlobWriteGuard,
) -> Result<()> {
    if archive.schema_version < 2 && archive.version < 2 {
        return Ok(());
    }

    validate_archive_artifact_record_blobs(blob_dir, archive)?;

    for source in &archive.capture_sources {
        upsert_imported_capture_source_tx(tx, source)?;
    }
    for workspace in &archive.vcs_workspaces {
        upsert_vcs_workspace_tx(tx, workspace)?;
    }
    for artifact in &archive.artifact_records {
        upsert_artifact_tx(tx, artifact)?;
    }
    for session in &archive.sessions {
        upsert_session_tx(tx, session)?;
    }
    for run in &archive.runs {
        upsert_run_tx(tx, run)?;
    }
    for event in &archive.events {
        upsert_event_tx(tx, event)?;
    }
    for change in &archive.vcs_changes {
        upsert_vcs_change_tx(tx, change)?;
    }
    for summary in &archive.summaries {
        upsert_summary_tx(tx, summary)?;
    }
    for file in &archive.files_touched {
        upsert_file_touched_tx(tx, file)?;
    }
    for link in &archive.history_record_links {
        upsert_history_record_link_tx(tx, link)?;
    }
    Ok(())
}

fn upsert_imported_capture_source_tx(tx: &Transaction<'_>, source: &CaptureSource) -> Result<()> {
    tx.execute(
        r#"
        INSERT INTO capture_sources
        (id, kind, provider, machine_id, process_id, cwd, raw_source_path, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
        ON CONFLICT(id) DO UPDATE SET
            kind = excluded.kind,
            provider = excluded.provider,
            machine_id = excluded.machine_id,
            process_id = excluded.process_id,
            cwd = excluded.cwd,
            raw_source_path = excluded.raw_source_path,
            external_session_id = excluded.external_session_id,
            started_at_ms = excluded.started_at_ms,
            ended_at_ms = excluded.ended_at_ms,
            fidelity = excluded.fidelity,
            visibility = excluded.visibility,
            sync_state = excluded.sync_state,
            sync_version = excluded.sync_version,
            metadata_json = excluded.metadata_json
        "#,
        params![
            source.id.to_string(),
            source.descriptor.kind.as_str(),
            source.descriptor.provider.as_str(),
            source.descriptor.machine_id.as_str(),
            source.descriptor.process_id.map(i64::from),
            source.descriptor.cwd.as_deref(),
            source.descriptor.raw_source_path.as_deref(),
            source.descriptor.external_session_id.as_deref(),
            timestamp_ms(source.started_at),
            optional_timestamp_ms(source.ended_at),
            source.sync.fidelity.as_str(),
            source.sync.visibility.as_str(),
            source.sync.sync_state.as_str(),
            source.sync.sync_version as i64,
            serde_json::to_string(&source.sync.metadata)?,
        ],
    )?;
    Ok(())
}

fn upsert_session_tx(tx: &Transaction<'_>, session: &Session) -> Result<()> {
    tx.execute(
        r#"
        INSERT INTO sessions
        (id, history_record_id, parent_session_id, root_session_id, capture_source_id, provider, external_session_id, external_agent_id, agent_type, role_hint, is_primary, status, fidelity, transcript_blob_id, started_at_ms, ended_at_ms, created_at_ms, updated_at_ms, visibility, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
        ON CONFLICT(id) DO UPDATE SET
            history_record_id = excluded.history_record_id,
            parent_session_id = excluded.parent_session_id,
            root_session_id = excluded.root_session_id,
            capture_source_id = excluded.capture_source_id,
            provider = excluded.provider,
            external_session_id = excluded.external_session_id,
            external_agent_id = excluded.external_agent_id,
            agent_type = excluded.agent_type,
            role_hint = excluded.role_hint,
            is_primary = excluded.is_primary,
            status = excluded.status,
            fidelity = excluded.fidelity,
            transcript_blob_id = excluded.transcript_blob_id,
            started_at_ms = excluded.started_at_ms,
            ended_at_ms = excluded.ended_at_ms,
            updated_at_ms = excluded.updated_at_ms,
            visibility = excluded.visibility,
            sync_state = excluded.sync_state,
            sync_version = excluded.sync_version,
            deleted_at_ms = excluded.deleted_at_ms,
            metadata_json = excluded.metadata_json
        "#,
        params![
            session.id.to_string(),
            optional_uuid_string(session.history_record_id),
            optional_uuid_string(session.parent_session_id),
            optional_uuid_string(session.root_session_id),
            optional_uuid_string(session.capture_source_id),
            session.provider.as_str(),
            session.external_session_id.as_deref(),
            session.external_agent_id.as_deref(),
            session.agent_type.as_str(),
            session.role_hint.as_deref(),
            session.is_primary as i64,
            session.status.as_str(),
            session.sync.fidelity.as_str(),
            optional_uuid_string(session.transcript_blob_id),
            timestamp_ms(session.started_at),
            optional_timestamp_ms(session.ended_at),
            timestamp_ms(session.timestamps.created_at),
            timestamp_ms(session.timestamps.updated_at),
            session.sync.visibility.as_str(),
            session.sync.sync_state.as_str(),
            session.sync.sync_version as i64,
            optional_timestamp_ms(session.sync.deleted_at),
            serde_json::to_string(&session.sync.metadata)?,
        ],
    )?;
    Ok(())
}

fn upsert_run_tx(tx: &Transaction<'_>, run: &Run) -> Result<()> {
    tx.execute(
        r#"
        INSERT INTO runs
        (id, history_record_id, session_id, run_type, status, started_at_ms, ended_at_ms, exit_code, cwd, command_preview, input_blob_id, output_blob_id, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
        ON CONFLICT(id) DO UPDATE SET
            history_record_id = excluded.history_record_id,
            session_id = excluded.session_id,
            run_type = excluded.run_type,
            status = excluded.status,
            started_at_ms = excluded.started_at_ms,
            ended_at_ms = excluded.ended_at_ms,
            exit_code = excluded.exit_code,
            cwd = excluded.cwd,
            command_preview = excluded.command_preview,
            input_blob_id = excluded.input_blob_id,
            output_blob_id = excluded.output_blob_id,
            updated_at_ms = excluded.updated_at_ms,
            source_id = excluded.source_id,
            visibility = excluded.visibility,
            fidelity = excluded.fidelity,
            sync_state = excluded.sync_state,
            sync_version = excluded.sync_version,
            deleted_at_ms = excluded.deleted_at_ms,
            metadata_json = excluded.metadata_json
        "#,
        params![
            run.id.to_string(),
            optional_uuid_string(run.history_record_id),
            optional_uuid_string(run.session_id),
            run.run_type.as_str(),
            run.status.as_str(),
            timestamp_ms(run.started_at),
            optional_timestamp_ms(run.ended_at),
            run.exit_code,
            run.cwd.as_deref(),
            run.command_preview.as_deref(),
            optional_uuid_string(run.input_blob_id),
            optional_uuid_string(run.output_blob_id),
            timestamp_ms(run.timestamps.created_at),
            timestamp_ms(run.timestamps.updated_at),
            optional_uuid_string(run.source_id),
            run.sync.visibility.as_str(),
            run.sync.fidelity.as_str(),
            run.sync.sync_state.as_str(),
            run.sync.sync_version as i64,
            optional_timestamp_ms(run.sync.deleted_at),
            serde_json::to_string(&run.sync.metadata)?,
        ],
    )?;
    Ok(())
}

fn upsert_event_tx(tx: &Transaction<'_>, event: &Event) -> Result<Uuid> {
    let event_id = if let Some(dedupe_key) = &event.dedupe_key {
        if let Some(existing) = tx
            .query_row(
                "SELECT id FROM events WHERE dedupe_key = ?1",
                params![dedupe_key],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .optional()?
        {
            existing
        } else {
            event.id
        }
    } else {
        event.id
    };

    tx.execute(
        r#"
        INSERT INTO events
        (id, seq, history_record_id, session_id, run_id, event_type, role, occurred_at_ms, capture_source_id, payload_json, payload_blob_id, dedupe_key, visibility, redaction_state, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
        ON CONFLICT(id) DO UPDATE SET
            seq = excluded.seq,
            history_record_id = excluded.history_record_id,
            session_id = excluded.session_id,
            run_id = excluded.run_id,
            event_type = excluded.event_type,
            role = excluded.role,
            occurred_at_ms = excluded.occurred_at_ms,
            capture_source_id = excluded.capture_source_id,
            payload_json = excluded.payload_json,
            payload_blob_id = excluded.payload_blob_id,
            dedupe_key = excluded.dedupe_key,
            visibility = excluded.visibility,
            redaction_state = excluded.redaction_state,
            fidelity = excluded.fidelity,
            sync_state = excluded.sync_state,
            sync_version = excluded.sync_version,
            deleted_at_ms = excluded.deleted_at_ms,
            metadata_json = excluded.metadata_json
        "#,
        params![
            event_id.to_string(),
            event.seq as i64,
            optional_uuid_string(event.history_record_id),
            optional_uuid_string(event.session_id),
            optional_uuid_string(event.run_id),
            event.event_type.as_str(),
            event.role.map(|role| role.as_str()),
            timestamp_ms(event.occurred_at),
            optional_uuid_string(event.capture_source_id),
            serde_json::to_string(&event.payload)?,
            optional_uuid_string(event.payload_blob_id),
            event.dedupe_key.as_deref(),
            event.sync.visibility.as_str(),
            event.redaction_state.as_str(),
            event.sync.fidelity.as_str(),
            event.sync.sync_state.as_str(),
            event.sync.sync_version as i64,
            optional_timestamp_ms(event.sync.deleted_at),
            serde_json::to_string(&event.sync.metadata)?,
        ],
    )?;
    Ok(event_id)
}

fn upsert_artifact_tx(tx: &Transaction<'_>, artifact: &Artifact) -> Result<Uuid> {
    tx.execute(
        r#"
        INSERT INTO artifacts
        (id, kind, blob_hash, blob_path, byte_size, media_type, preview_text, redaction_state, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
        ON CONFLICT DO UPDATE SET
            blob_path = excluded.blob_path,
            byte_size = excluded.byte_size,
            media_type = excluded.media_type,
            preview_text = excluded.preview_text,
            redaction_state = excluded.redaction_state,
            updated_at_ms = excluded.updated_at_ms,
            source_id = excluded.source_id,
            visibility = excluded.visibility,
            fidelity = excluded.fidelity,
            sync_state = excluded.sync_state,
            sync_version = excluded.sync_version,
            deleted_at_ms = excluded.deleted_at_ms,
            metadata_json = excluded.metadata_json
        "#,
        params![
            artifact.id.to_string(),
            artifact.kind.as_str(),
            artifact.blob_hash.as_str(),
            artifact.blob_path.as_str(),
            artifact.byte_size as i64,
            artifact.media_type.as_deref(),
            artifact.preview_text.as_deref(),
            artifact.redaction_state.as_str(),
            timestamp_ms(artifact.timestamps.created_at),
            timestamp_ms(artifact.timestamps.updated_at),
            optional_uuid_string(artifact.source_id),
            artifact.sync.visibility.as_str(),
            artifact.sync.fidelity.as_str(),
            artifact.sync.sync_state.as_str(),
            artifact.sync.sync_version as i64,
            optional_timestamp_ms(artifact.sync.deleted_at),
            serde_json::to_string(&artifact.sync.metadata)?,
        ],
    )?;
    tx.query_row(
        "SELECT id FROM artifacts WHERE blob_hash = ?1 AND kind = ?2",
        params![artifact.blob_hash.as_str(), artifact.kind.as_str()],
        |row| parse_uuid(row.get::<_, String>(0)?),
    )
    .map_err(StoreError::from)
}

fn upsert_vcs_workspace_tx(tx: &Transaction<'_>, workspace: &VcsWorkspace) -> Result<Uuid> {
    tx.execute(
        r#"
        INSERT INTO vcs_workspaces
        (id, kind, root_path, repo_fingerprint, primary_remote_url_normalized, host, owner, name, monorepo_subpath, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
        ON CONFLICT(kind, repo_fingerprint) DO UPDATE SET
            root_path = excluded.root_path,
            primary_remote_url_normalized = excluded.primary_remote_url_normalized,
            host = excluded.host,
            owner = excluded.owner,
            name = excluded.name,
            monorepo_subpath = excluded.monorepo_subpath,
            updated_at_ms = excluded.updated_at_ms,
            source_id = excluded.source_id,
            visibility = excluded.visibility,
            fidelity = excluded.fidelity,
            sync_state = excluded.sync_state,
            sync_version = excluded.sync_version,
            deleted_at_ms = excluded.deleted_at_ms,
            metadata_json = excluded.metadata_json
        "#,
        params![
            workspace.id.to_string(),
            workspace.kind.as_str(),
            workspace.root_path.as_str(),
            workspace.repo_fingerprint.as_str(),
            workspace.primary_remote_url_normalized.as_deref(),
            workspace.host.as_str(),
            workspace.owner.as_deref(),
            workspace.name.as_deref(),
            workspace.monorepo_subpath.as_deref(),
            timestamp_ms(workspace.timestamps.created_at),
            timestamp_ms(workspace.timestamps.updated_at),
            optional_uuid_string(workspace.source_id),
            workspace.sync.visibility.as_str(),
            workspace.sync.fidelity.as_str(),
            workspace.sync.sync_state.as_str(),
            workspace.sync.sync_version as i64,
            optional_timestamp_ms(workspace.sync.deleted_at),
            serde_json::to_string(&workspace.sync.metadata)?,
        ],
    )?;
    tx.query_row(
        "SELECT id FROM vcs_workspaces WHERE kind = ?1 AND repo_fingerprint = ?2",
        params![workspace.kind.as_str(), workspace.repo_fingerprint.as_str()],
        |row| parse_uuid(row.get::<_, String>(0)?),
    )
    .map_err(StoreError::from)
}

fn upsert_vcs_change_tx(tx: &Transaction<'_>, change: &VcsChange) -> Result<Uuid> {
    tx.execute(
        r#"
        INSERT INTO vcs_changes
        (id, vcs_workspace_id, kind, change_id, parent_change_ids_json, branch_or_bookmark, tree_hash, author_time_ms, confidence, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
        ON CONFLICT(vcs_workspace_id, kind, change_id) DO UPDATE SET
            parent_change_ids_json = excluded.parent_change_ids_json,
            branch_or_bookmark = excluded.branch_or_bookmark,
            tree_hash = excluded.tree_hash,
            author_time_ms = excluded.author_time_ms,
            confidence = excluded.confidence,
            updated_at_ms = excluded.updated_at_ms,
            source_id = excluded.source_id,
            visibility = excluded.visibility,
            fidelity = excluded.fidelity,
            sync_state = excluded.sync_state,
            sync_version = excluded.sync_version,
            deleted_at_ms = excluded.deleted_at_ms,
            metadata_json = excluded.metadata_json
        "#,
        params![
            change.id.to_string(),
            change.vcs_workspace_id.to_string(),
            change.kind.as_str(),
            change.change_id.as_str(),
            serde_json::to_string(&change.parent_change_ids)?,
            change.branch_or_bookmark.as_deref(),
            change.tree_hash.as_deref(),
            optional_timestamp_ms(change.author_time),
            change.confidence.as_str(),
            timestamp_ms(change.timestamps.created_at),
            timestamp_ms(change.timestamps.updated_at),
            optional_uuid_string(change.source_id),
            change.sync.visibility.as_str(),
            change.sync.fidelity.as_str(),
            change.sync.sync_state.as_str(),
            change.sync.sync_version as i64,
            optional_timestamp_ms(change.sync.deleted_at),
            serde_json::to_string(&change.sync.metadata)?,
        ],
    )?;
    tx.query_row(
        "SELECT id FROM vcs_changes WHERE vcs_workspace_id = ?1 AND kind = ?2 AND change_id = ?3",
        params![
            change.vcs_workspace_id.to_string(),
            change.kind.as_str(),
            change.change_id.as_str()
        ],
        |row| parse_uuid(row.get::<_, String>(0)?),
    )
    .map_err(StoreError::from)
}

fn upsert_record_tx(
    tx: &Transaction<'_>,
    record: &HistoryRecord,
    source_id: Option<Uuid>,
) -> Result<()> {
    let created_at_ms = timestamp_ms(record.created_at);
    let updated_at_ms = timestamp_ms(record.updated_at);
    tx.execute(
        r#"
        INSERT INTO history_records
        (
            id, title, summary, status, started_at_ms, last_activity_at_ms,
            created_at_ms, updated_at_ms, source_id, body, tags_json, kind,
            workspace, created_at, updated_at
        )
        VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
        ON CONFLICT(id) DO UPDATE SET
            title = excluded.title,
            summary = excluded.summary,
            status = excluded.status,
            started_at_ms = excluded.started_at_ms,
            last_activity_at_ms = excluded.last_activity_at_ms,
            created_at_ms = excluded.created_at_ms,
            updated_at_ms = excluded.updated_at_ms,
            source_id = COALESCE(excluded.source_id, history_records.source_id),
            body = excluded.body,
            tags_json = excluded.tags_json,
            kind = excluded.kind,
            workspace = excluded.workspace,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at
        "#,
        params![
            record.id.to_string(),
            record.title,
            record.body,
            created_at_ms,
            updated_at_ms,
            source_id.map(|id| id.to_string()),
            record.body,
            serde_json::to_string(&record.tags)?,
            record.kind,
            record.workspace,
            record.created_at.to_rfc3339(),
            record.updated_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn upsert_summary_tx(tx: &Transaction<'_>, summary: &Summary) -> Result<()> {
    tx.execute(
        r#"
        INSERT INTO summaries
        (id, history_record_id, session_id, kind, model_or_source, text, citations_json, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
        ON CONFLICT(id) DO UPDATE SET
            history_record_id = excluded.history_record_id,
            session_id = excluded.session_id,
            kind = excluded.kind,
            model_or_source = excluded.model_or_source,
            text = excluded.text,
            citations_json = excluded.citations_json,
            updated_at_ms = excluded.updated_at_ms,
            source_id = excluded.source_id,
            visibility = excluded.visibility,
            fidelity = excluded.fidelity,
            sync_state = excluded.sync_state,
            sync_version = excluded.sync_version,
            deleted_at_ms = excluded.deleted_at_ms,
            metadata_json = excluded.metadata_json
        "#,
        params![
            summary.id.to_string(),
            optional_uuid_string(summary.history_record_id),
            optional_uuid_string(summary.session_id),
            summary.kind.as_str(),
            summary.model_or_source.as_deref(),
            summary.text.as_str(),
            serde_json::to_string(&summary.citations)?,
            timestamp_ms(summary.timestamps.created_at),
            timestamp_ms(summary.timestamps.updated_at),
            optional_uuid_string(summary.source_id),
            summary.sync.visibility.as_str(),
            summary.sync.fidelity.as_str(),
            summary.sync.sync_state.as_str(),
            summary.sync.sync_version as i64,
            optional_timestamp_ms(summary.sync.deleted_at),
            serde_json::to_string(&summary.sync.metadata)?,
        ],
    )?;
    Ok(())
}

fn upsert_file_touched_tx(tx: &Transaction<'_>, file: &FileTouched) -> Result<()> {
    tx.execute(
        r#"
        INSERT INTO files_touched
        (id, history_record_id, run_id, event_id, vcs_workspace_id, path, change_kind, old_path, line_count_delta, confidence, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
        ON CONFLICT(id) DO UPDATE SET
            history_record_id = excluded.history_record_id,
            run_id = excluded.run_id,
            event_id = excluded.event_id,
            vcs_workspace_id = excluded.vcs_workspace_id,
            path = excluded.path,
            change_kind = excluded.change_kind,
            old_path = excluded.old_path,
            line_count_delta = excluded.line_count_delta,
            confidence = excluded.confidence,
            updated_at_ms = excluded.updated_at_ms,
            source_id = excluded.source_id,
            visibility = excluded.visibility,
            fidelity = excluded.fidelity,
            sync_state = excluded.sync_state,
            sync_version = excluded.sync_version,
            deleted_at_ms = excluded.deleted_at_ms,
            metadata_json = excluded.metadata_json
        "#,
        params![
            file.id.to_string(),
            optional_uuid_string(file.history_record_id),
            optional_uuid_string(file.run_id),
            optional_uuid_string(file.event_id),
            optional_uuid_string(file.vcs_workspace_id),
            file.path.as_str(),
            file.change_kind.map(|kind| kind.as_str()),
            file.old_path.as_deref(),
            file.line_count_delta,
            file.confidence.as_str(),
            timestamp_ms(file.timestamps.created_at),
            timestamp_ms(file.timestamps.updated_at),
            optional_uuid_string(file.source_id),
            file.sync.visibility.as_str(),
            file.sync.fidelity.as_str(),
            file.sync.sync_state.as_str(),
            file.sync.sync_version as i64,
            optional_timestamp_ms(file.sync.deleted_at),
            serde_json::to_string(&file.sync.metadata)?,
        ],
    )?;
    Ok(())
}

fn upsert_history_record_link_tx(tx: &Transaction<'_>, link: &HistoryRecordLink) -> Result<Uuid> {
    tx.execute(
        r#"
        INSERT INTO history_record_links
        (id, history_record_id, target_type, target_id, link_type, confidence, source_id, created_at_ms, updated_at_ms, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
        ON CONFLICT(history_record_id, target_type, target_id, link_type) DO UPDATE SET
            confidence = excluded.confidence,
            source_id = excluded.source_id,
            updated_at_ms = excluded.updated_at_ms,
            visibility = excluded.visibility,
            fidelity = excluded.fidelity,
            sync_state = excluded.sync_state,
            sync_version = excluded.sync_version,
            deleted_at_ms = excluded.deleted_at_ms,
            metadata_json = excluded.metadata_json
        "#,
        params![
            link.id.to_string(),
            link.history_record_id.to_string(),
            link.target_type.as_str(),
            link.target_id.to_string(),
            link.link_type.as_str(),
            link.confidence.as_str(),
            optional_uuid_string(link.source_id),
            timestamp_ms(link.timestamps.created_at),
            timestamp_ms(link.timestamps.updated_at),
            link.sync.visibility.as_str(),
            link.sync.fidelity.as_str(),
            link.sync.sync_state.as_str(),
            link.sync.sync_version as i64,
            optional_timestamp_ms(link.sync.deleted_at),
            serde_json::to_string(&link.sync.metadata)?,
        ],
    )?;
    tx.query_row(
        "SELECT id FROM history_record_links WHERE history_record_id = ?1 AND target_type = ?2 AND target_id = ?3 AND link_type = ?4",
        params![
            link.history_record_id.to_string(),
            link.target_type.as_str(),
            link.target_id.to_string(),
            link.link_type.as_str()
        ],
        |row| parse_uuid(row.get::<_, String>(0)?),
    )
    .map_err(StoreError::from)
}

fn capture_source_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CaptureSource> {
    Ok(CaptureSource {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        descriptor: CaptureSourceDescriptor {
            kind: parse_text_enum::<ctx_history_core::CaptureSourceKind>(row.get::<_, String>(1)?)?,
            provider: parse_text_enum::<CaptureProvider>(row.get::<_, String>(2)?)?,
            machine_id: row.get(3)?,
            process_id: row
                .get::<_, Option<i64>>(4)?
                .map(nonnegative_i64_to_u32)
                .transpose()?,
            cwd: row.get(5)?,
            raw_source_path: row.get(6)?,
            external_session_id: row.get(7)?,
        },
        started_at: ms_to_time(row.get(8)?)?,
        ended_at: optional_ms_to_time(row.get(9)?)?,
        sync: SyncMetadata {
            fidelity: parse_text_enum::<Fidelity>(row.get::<_, String>(10)?)?,
            visibility: parse_text_enum::<Visibility>(row.get::<_, String>(11)?)?,
            sync_state: parse_text_enum::<SyncState>(row.get::<_, String>(12)?)?,
            sync_version: nonnegative_i64_to_u64(row.get(13)?)?,
            deleted_at: None,
            metadata: parse_json(row.get::<_, String>(14)?)?,
        },
    })
}

fn catalog_session_select_sql(tail: &str) -> String {
    format!(
        "SELECT source_path, provider, source_format, source_root, external_session_id, parent_external_session_id, agent_type, role_hint, external_agent_id, cwd, session_started_at_ms, file_size_bytes, file_modified_at_ms, cataloged_at_ms, metadata_json FROM catalog_sessions {tail}"
    )
}

fn source_import_file_select_sql(tail: &str) -> String {
    format!(
        "SELECT provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms, metadata_json FROM source_import_files {tail}"
    )
}

fn source_import_file_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceImportFile> {
    Ok(SourceImportFile {
        provider: parse_text_enum::<CaptureProvider>(row.get::<_, String>(0)?)?,
        source_format: row.get(1)?,
        source_root: row.get(2)?,
        source_path: row.get(3)?,
        file_size_bytes: nonnegative_i64_to_u64(row.get(4)?)?,
        file_modified_at_ms: row.get(5)?,
        observed_at_ms: row.get(6)?,
        metadata: parse_json(row.get::<_, String>(7)?)?,
    })
}

fn catalog_session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CatalogSession> {
    Ok(CatalogSession {
        source_path: row.get(0)?,
        provider: parse_text_enum::<CaptureProvider>(row.get::<_, String>(1)?)?,
        source_format: row.get(2)?,
        source_root: row.get(3)?,
        external_session_id: row.get(4)?,
        parent_external_session_id: row.get(5)?,
        agent_type: parse_text_enum::<AgentType>(row.get::<_, String>(6)?)?,
        role_hint: row.get(7)?,
        external_agent_id: row.get(8)?,
        cwd: row.get(9)?,
        session_started_at_ms: row.get(10)?,
        file_size_bytes: nonnegative_i64_to_u64(row.get(11)?)?,
        file_modified_at_ms: row.get(12)?,
        cataloged_at_ms: row.get(13)?,
        metadata: parse_json(row.get::<_, String>(14)?)?,
    })
}

fn catalog_pending_import_condition_sql(alias: &str) -> String {
    format!(
        r#"
        (
            {alias}.indexed_status != 'indexed'
            OR {alias}.indexed_file_size_bytes IS NULL
            OR {alias}.indexed_file_modified_at_ms IS NULL
            OR {alias}.indexed_file_size_bytes != {alias}.file_size_bytes
            OR {alias}.indexed_file_modified_at_ms != {alias}.file_modified_at_ms
            OR NOT EXISTS (
                SELECT 1
                FROM sessions AS session
                WHERE session.provider = {alias}.provider
                  AND {alias}.external_session_id IS NOT NULL
                  AND session.external_session_id = {alias}.external_session_id
                LIMIT 1
            )
        )
        "#
    )
}

fn catalog_indexed_count_sql() -> String {
    r#"
    SELECT COUNT(*)
    FROM catalog_sessions AS catalog
    WHERE catalog.is_stale = 0
      AND catalog.indexed_status = 'indexed'
      AND catalog.indexed_file_size_bytes = catalog.file_size_bytes
      AND catalog.indexed_file_modified_at_ms = catalog.file_modified_at_ms
      AND EXISTS (
        SELECT 1
        FROM sessions AS session
        WHERE session.provider = catalog.provider
          AND catalog.external_session_id IS NOT NULL
          AND session.external_session_id = catalog.external_session_id
        LIMIT 1
      )
    "#
    .to_owned()
}

fn session_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, history_record_id, parent_session_id, root_session_id, capture_source_id, provider, external_session_id, external_agent_id, agent_type, role_hint, is_primary, status, fidelity, transcript_blob_id, started_at_ms, ended_at_ms, created_at_ms, updated_at_ms, visibility, sync_state, sync_version, deleted_at_ms, metadata_json FROM sessions {tail}"
    )
}

fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        history_record_id: parse_optional_uuid(row.get(1)?)?,
        parent_session_id: parse_optional_uuid(row.get(2)?)?,
        root_session_id: parse_optional_uuid(row.get(3)?)?,
        capture_source_id: parse_optional_uuid(row.get(4)?)?,
        provider: parse_text_enum::<CaptureProvider>(row.get::<_, String>(5)?)?,
        external_session_id: row.get(6)?,
        external_agent_id: row.get(7)?,
        agent_type: parse_text_enum::<AgentType>(row.get::<_, String>(8)?)?,
        role_hint: row.get(9)?,
        is_primary: row.get::<_, i64>(10)? != 0,
        status: parse_text_enum::<SessionStatus>(row.get::<_, String>(11)?)?,
        transcript_blob_id: parse_optional_uuid(row.get(13)?)?,
        started_at: ms_to_time(row.get(14)?)?,
        ended_at: optional_ms_to_time(row.get(15)?)?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(16)?)?,
            updated_at: ms_to_time(row.get(17)?)?,
        },
        sync: sync_metadata_from_row(row, 18, 12, 19, 20, 21, 22)?,
    })
}

fn run_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, history_record_id, session_id, run_type, status, started_at_ms, ended_at_ms, exit_code, cwd, command_preview, input_blob_id, output_blob_id, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM runs {tail}"
    )
}

fn run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Run> {
    Ok(Run {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        history_record_id: parse_optional_uuid(row.get(1)?)?,
        session_id: parse_optional_uuid(row.get(2)?)?,
        run_type: parse_text_enum::<RunType>(row.get::<_, String>(3)?)?,
        status: parse_text_enum::<RunStatus>(row.get::<_, String>(4)?)?,
        started_at: ms_to_time(row.get(5)?)?,
        ended_at: optional_ms_to_time(row.get(6)?)?,
        exit_code: row.get(7)?,
        cwd: row.get(8)?,
        command_preview: row.get(9)?,
        input_blob_id: parse_optional_uuid(row.get(10)?)?,
        output_blob_id: parse_optional_uuid(row.get(11)?)?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(12)?)?,
            updated_at: ms_to_time(row.get(13)?)?,
        },
        source_id: parse_optional_uuid(row.get(14)?)?,
        sync: sync_metadata_from_row(row, 15, 16, 17, 18, 19, 20)?,
    })
}

fn event_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, seq, history_record_id, session_id, run_id, event_type, role, occurred_at_ms, capture_source_id, payload_json, payload_blob_id, dedupe_key, visibility, redaction_state, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM events {tail}"
    )
}

fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        seq: nonnegative_i64_to_u64(row.get(1)?)?,
        history_record_id: parse_optional_uuid(row.get(2)?)?,
        session_id: parse_optional_uuid(row.get(3)?)?,
        run_id: parse_optional_uuid(row.get(4)?)?,
        event_type: parse_text_enum::<EventType>(row.get::<_, String>(5)?)?,
        role: row
            .get::<_, Option<String>>(6)?
            .map(parse_text_enum::<EventRole>)
            .transpose()?,
        occurred_at: ms_to_time(row.get(7)?)?,
        capture_source_id: parse_optional_uuid(row.get(8)?)?,
        payload: parse_json(row.get::<_, String>(9)?)?,
        payload_blob_id: parse_optional_uuid(row.get(10)?)?,
        dedupe_key: row.get(11)?,
        redaction_state: parse_text_enum::<RedactionState>(row.get::<_, String>(13)?)?,
        sync: sync_metadata_from_row(row, 12, 14, 15, 16, 17, 18)?,
    })
}

fn artifact_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, kind, blob_hash, blob_path, byte_size, media_type, preview_text, redaction_state, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM artifacts {tail}"
    )
}

fn artifact_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Artifact> {
    Ok(Artifact {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        kind: parse_text_enum::<ArtifactKind>(row.get::<_, String>(1)?)?,
        blob_hash: row.get(2)?,
        blob_path: row.get(3)?,
        byte_size: nonnegative_i64_to_u64(row.get(4)?)?,
        media_type: row.get(5)?,
        preview_text: row.get(6)?,
        redaction_state: parse_text_enum::<RedactionState>(row.get::<_, String>(7)?)?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(8)?)?,
            updated_at: ms_to_time(row.get(9)?)?,
        },
        source_id: parse_optional_uuid(row.get(10)?)?,
        sync: sync_metadata_from_row(row, 11, 12, 13, 14, 15, 16)?,
    })
}

fn vcs_workspace_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, kind, root_path, repo_fingerprint, primary_remote_url_normalized, host, owner, name, monorepo_subpath, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM vcs_workspaces {tail}"
    )
}

fn vcs_workspace_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<VcsWorkspace> {
    Ok(VcsWorkspace {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        kind: parse_text_enum::<ctx_history_core::VcsKind>(row.get::<_, String>(1)?)?,
        root_path: row.get(2)?,
        repo_fingerprint: row.get(3)?,
        primary_remote_url_normalized: row.get(4)?,
        host: parse_text_enum::<ctx_history_core::VcsHost>(row.get::<_, String>(5)?)?,
        owner: row.get(6)?,
        name: row.get(7)?,
        monorepo_subpath: row.get(8)?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(9)?)?,
            updated_at: ms_to_time(row.get(10)?)?,
        },
        source_id: parse_optional_uuid(row.get(11)?)?,
        sync: sync_metadata_from_row(row, 12, 13, 14, 15, 16, 17)?,
    })
}

fn vcs_change_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, vcs_workspace_id, kind, change_id, parent_change_ids_json, branch_or_bookmark, tree_hash, author_time_ms, confidence, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM vcs_changes {tail}"
    )
}

fn vcs_change_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<VcsChange> {
    Ok(VcsChange {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        vcs_workspace_id: parse_uuid(row.get::<_, String>(1)?)?,
        kind: parse_text_enum::<ctx_history_core::VcsChangeKind>(row.get::<_, String>(2)?)?,
        change_id: row.get(3)?,
        parent_change_ids: serde_json::from_str(&row.get::<_, String>(4)?)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?,
        branch_or_bookmark: row.get(5)?,
        tree_hash: row.get(6)?,
        author_time: optional_ms_to_time(row.get(7)?)?,
        confidence: parse_text_enum::<ctx_history_core::Confidence>(row.get::<_, String>(8)?)?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(9)?)?,
            updated_at: ms_to_time(row.get(10)?)?,
        },
        source_id: parse_optional_uuid(row.get(11)?)?,
        sync: sync_metadata_from_row(row, 12, 13, 14, 15, 16, 17)?,
    })
}

fn summary_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, history_record_id, session_id, kind, model_or_source, text, citations_json, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM summaries {tail}"
    )
}

fn summary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Summary> {
    Ok(Summary {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        history_record_id: parse_optional_uuid(row.get(1)?)?,
        session_id: parse_optional_uuid(row.get(2)?)?,
        kind: parse_text_enum::<ctx_history_core::SummaryKind>(row.get::<_, String>(3)?)?,
        model_or_source: row.get(4)?,
        text: row.get(5)?,
        citations: serde_json::from_str(&row.get::<_, String>(6)?)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(7)?)?,
            updated_at: ms_to_time(row.get(8)?)?,
        },
        source_id: parse_optional_uuid(row.get(9)?)?,
        sync: sync_metadata_from_row(row, 10, 11, 12, 13, 14, 15)?,
    })
}

fn file_touched_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, history_record_id, run_id, event_id, vcs_workspace_id, path, change_kind, old_path, line_count_delta, confidence, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM files_touched {tail}"
    )
}

fn file_touched_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileTouched> {
    Ok(FileTouched {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        history_record_id: parse_optional_uuid(row.get(1)?)?,
        run_id: parse_optional_uuid(row.get(2)?)?,
        event_id: parse_optional_uuid(row.get(3)?)?,
        vcs_workspace_id: parse_optional_uuid(row.get(4)?)?,
        path: row.get(5)?,
        change_kind: row
            .get::<_, Option<String>>(6)?
            .map(parse_text_enum::<ctx_history_core::FileChangeKind>)
            .transpose()?,
        old_path: row.get(7)?,
        line_count_delta: row.get(8)?,
        confidence: parse_text_enum::<ctx_history_core::Confidence>(row.get::<_, String>(9)?)?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(10)?)?,
            updated_at: ms_to_time(row.get(11)?)?,
        },
        source_id: parse_optional_uuid(row.get(12)?)?,
        sync: sync_metadata_from_row(row, 13, 14, 15, 16, 17, 18)?,
    })
}

fn history_record_link_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, history_record_id, target_type, target_id, link_type, confidence, source_id, created_at_ms, updated_at_ms, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM history_record_links {tail}"
    )
}

fn history_record_link_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryRecordLink> {
    Ok(HistoryRecordLink {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        history_record_id: parse_uuid(row.get::<_, String>(1)?)?,
        target_type: parse_text_enum::<ctx_history_core::HistoryRecordLinkTargetType>(
            row.get::<_, String>(2)?,
        )?,
        target_id: parse_uuid(row.get::<_, String>(3)?)?,
        link_type: parse_text_enum::<ctx_history_core::HistoryRecordLinkType>(
            row.get::<_, String>(4)?,
        )?,
        confidence: parse_text_enum::<ctx_history_core::Confidence>(row.get::<_, String>(5)?)?,
        source_id: parse_optional_uuid(row.get(6)?)?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(7)?)?,
            updated_at: ms_to_time(row.get(8)?)?,
        },
        sync: sync_metadata_from_row(row, 9, 10, 11, 12, 13, 14)?,
    })
}

fn sync_cursor_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SyncCursor> {
    Ok(SyncCursor {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        team_id: row.get(1)?,
        device_id: row.get(2)?,
        stream: row.get(3)?,
        cursor: row.get(4)?,
        last_synced_at: optional_ms_to_time(row.get(5)?)?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(6)?)?,
            updated_at: ms_to_time(row.get(7)?)?,
        },
    })
}

fn sync_metadata_from_row(
    row: &rusqlite::Row<'_>,
    visibility_index: usize,
    fidelity_index: usize,
    sync_state_index: usize,
    sync_version_index: usize,
    deleted_at_index: usize,
    metadata_index: usize,
) -> rusqlite::Result<SyncMetadata> {
    Ok(SyncMetadata {
        visibility: parse_text_enum::<Visibility>(row.get::<_, String>(visibility_index)?)?,
        fidelity: parse_text_enum::<Fidelity>(row.get::<_, String>(fidelity_index)?)?,
        sync_state: parse_text_enum::<SyncState>(row.get::<_, String>(sync_state_index)?)?,
        sync_version: nonnegative_i64_to_u64(row.get(sync_version_index)?)?,
        deleted_at: optional_ms_to_time(row.get(deleted_at_index)?)?,
        metadata: parse_json(row.get::<_, String>(metadata_index)?)?,
    })
}

fn optional_uuid_string(id: Option<Uuid>) -> Option<String> {
    id.map(|id| id.to_string())
}

fn optional_timestamp_ms(value: Option<DateTime<Utc>>) -> Option<i64> {
    value.map(timestamp_ms)
}

fn ms_to_time(value: i64) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp_millis(value).ok_or_else(|| {
        rusqlite::Error::ToSqlConversionFailure(format!("invalid timestamp millis: {value}").into())
    })
}

fn optional_ms_to_time(value: Option<i64>) -> rusqlite::Result<Option<DateTime<Utc>>> {
    value.map(ms_to_time).transpose()
}

fn parse_optional_uuid(value: Option<String>) -> rusqlite::Result<Option<Uuid>> {
    value.map(parse_uuid).transpose()
}

fn parse_json(value: String) -> rusqlite::Result<serde_json::Value> {
    serde_json::from_str(&value)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))
}

fn record_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, title, body, tags_json, kind, workspace, created_at, updated_at FROM history_records {tail}"
    )
}

fn record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryRecord> {
    let tags_json: String = row.get(3)?;
    Ok(HistoryRecord {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        title: row.get(1)?,
        body: row.get(2)?,
        tags: serde_json::from_str(&tags_json)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?,
        kind: row.get(4)?,
        workspace: row.get(5)?,
        created_at: parse_time(row.get::<_, String>(6)?)?,
        updated_at: parse_time(row.get::<_, String>(7)?)?,
    })
}

fn local_device_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LocalDeviceIdentity> {
    Ok(LocalDeviceIdentity {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        stable_device_id: row.get(1)?,
        created_at: time_ms(row.get(2)?),
        updated_at: time_ms(row.get(3)?),
    })
}

fn local_workspace_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LocalWorkspaceIdentity> {
    let vcs_workspace_id: Option<String> = row.get(2)?;
    Ok(LocalWorkspaceIdentity {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        device_id: parse_uuid(row.get::<_, String>(1)?)?,
        vcs_workspace_id: vcs_workspace_id
            .map(parse_uuid)
            .transpose()
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?,
        repo_fingerprint: row.get(3)?,
        root_path_hash: row.get(4)?,
        display_root: row.get(5)?,
        created_at: time_ms(row.get(6)?),
        updated_at: time_ms(row.get(7)?),
    })
}

fn parse_uuid(value: String) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(&value).map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))
}

fn parse_time(value: String) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))
}

fn parse_text_enum<T>(value: String) -> rusqlite::Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    value
        .parse()
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))
}

fn parse_optional_text_enum<T>(value: Option<String>) -> rusqlite::Result<Option<T>>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    value.map(parse_text_enum).transpose()
}

fn event_search_cursor(
    payload_json: &str,
    source_metadata_json: Option<&str>,
) -> rusqlite::Result<Option<String>> {
    let payload: serde_json::Value = serde_json::from_str(payload_json)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
    if let Some(cursor) = payload.get("cursor").and_then(|value| value.as_str()) {
        return Ok(Some(cursor.to_owned()));
    }
    if let Some(cursor) = payload
        .get("body")
        .and_then(|body| body.get("cursor"))
        .and_then(|value| value.as_str())
    {
        return Ok(Some(cursor.to_owned()));
    }

    let Some(source_metadata_json) = source_metadata_json else {
        return Ok(None);
    };
    let metadata: serde_json::Value = serde_json::from_str(source_metadata_json)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
    Ok(metadata
        .get("cursor")
        .and_then(|cursor| cursor.get("after"))
        .and_then(|after| after.get("cursor"))
        .and_then(|value| value.as_str())
        .map(str::to_owned))
}

#[derive(Default)]
struct EventSearchSourceIdentity {
    history_source: Option<String>,
    history_source_plugin: Option<String>,
    provider_key: Option<String>,
    source_id: Option<String>,
    source_format: Option<String>,
}

fn event_search_source_identity(
    source_metadata_json: Option<&str>,
) -> rusqlite::Result<EventSearchSourceIdentity> {
    let Some(source_metadata_json) = source_metadata_json else {
        return Ok(EventSearchSourceIdentity::default());
    };
    let metadata: serde_json::Value = serde_json::from_str(source_metadata_json)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
    let source_metadata = metadata
        .get("source_metadata")
        .and_then(serde_json::Value::as_object);
    let plugin = source_metadata
        .and_then(|metadata| metadata.get("ctx_history_plugin"))
        .or_else(|| metadata.get("ctx_history_plugin"))
        .and_then(serde_json::Value::as_object);
    let custom = source_metadata
        .and_then(|metadata| metadata.get("ctx_history_jsonl_v1"))
        .or_else(|| metadata.get("ctx_history_jsonl_v1"))
        .and_then(serde_json::Value::as_object);
    let plugin_name = plugin
        .and_then(|plugin| plugin.get("plugin_name"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let plugin_source_id = plugin
        .and_then(|plugin| plugin.get("plugin_source_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let history_source = plugin
        .and_then(|plugin| plugin.get("history_source"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            plugin_name
                .as_deref()
                .zip(plugin_source_id.as_deref())
                .map(|(plugin_name, source_id)| format!("{plugin_name}/{source_id}"))
        });
    let provider_key = custom
        .and_then(|custom| custom.get("provider_key"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let source_id = custom
        .and_then(|custom| custom.get("source_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let source_format = custom
        .and_then(|custom| custom.get("source_format"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            source_metadata
                .and_then(|metadata| metadata.get("source_format"))
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            metadata
                .get("source_format")
                .and_then(serde_json::Value::as_str)
        })
        .map(str::to_owned);
    Ok(EventSearchSourceIdentity {
        history_source,
        history_source_plugin: plugin_name,
        provider_key,
        source_id,
        source_format,
    })
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>> {
    let mut values = Vec::new();
    for row in rows {
        values.push(row?);
    }
    Ok(values)
}

#[cfg(test)]
mod search_order_tests {
    use super::*;

    fn tempdir() -> tempfile::TempDir {
        let root = std::env::current_dir().unwrap().join("target/test-data");
        fs::create_dir_all(&root).unwrap();
        tempfile::Builder::new()
            .prefix("ctx-history-store-search-order-")
            .tempdir_in(root)
            .unwrap()
    }

    fn fixed_time() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-23T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn sync_metadata() -> SyncMetadata {
        SyncMetadata {
            visibility: Visibility::LocalOnly,
            fidelity: Fidelity::Imported,
            sync_state: SyncState::LocalOnly,
            sync_version: 0,
            deleted_at: None,
            metadata: serde_json::json!({}),
        }
    }

    fn local_preview_event(seq: u64, text: &str, redaction_state: RedactionState) -> Event {
        Event {
            id: new_id(),
            seq,
            history_record_id: None,
            session_id: None,
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::User),
            occurred_at: fixed_time(),
            capture_source_id: None,
            payload: serde_json::json!({ "text": text }),
            payload_blob_id: None,
            dedupe_key: None,
            redaction_state,
            sync: sync_metadata(),
        }
    }

    #[test]
    fn indexed_history_item_count_uses_sessions_and_events() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();

        for (idx, session_id) in [
            "018f45d0-0000-7000-8000-000000050001",
            "018f45d0-0000-7000-8000-000000050002",
        ]
        .into_iter()
        .enumerate()
        {
            store
                .conn
                .execute(
                    r#"
                    INSERT INTO sessions
                    (id, provider, external_session_id, agent_type, is_primary, status, fidelity,
                     started_at_ms, created_at_ms, updated_at_ms)
                    VALUES (?1, 'codex', ?2, 'primary', 1, 'imported', 'full', 1, 1, 1)
                    "#,
                    params![session_id, format!("external-session-{idx}")],
                )
                .unwrap();
        }

        for (seq, event_id, session_id) in [
            (
                1_i64,
                "018f45d0-0000-7000-8000-000000060001",
                "018f45d0-0000-7000-8000-000000050001",
            ),
            (
                2_i64,
                "018f45d0-0000-7000-8000-000000060002",
                "018f45d0-0000-7000-8000-000000050001",
            ),
            (
                3_i64,
                "018f45d0-0000-7000-8000-000000060003",
                "018f45d0-0000-7000-8000-000000050002",
            ),
        ] {
            store
                .conn
                .execute(
                    r#"
                    INSERT INTO events
                    (id, seq, session_id, event_type, role, occurred_at_ms, payload_json)
                    VALUES (?1, ?2, ?3, 'message', 'user', 1, '{}')
                    "#,
                    params![event_id, seq, session_id],
                )
                .unwrap();
        }

        assert_eq!(store.indexed_history_item_count().unwrap(), 5);
    }

    #[test]
    fn capture_source_count_uses_aggregate_count() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();

        for index in 1..=3 {
            store
                .conn
                .execute(
                    r#"
                    INSERT INTO capture_sources
                    (id, kind, provider, machine_id, started_at_ms, fidelity)
                    VALUES (?1, 'provider_import', 'codex', 'test-machine', ?2, 'full')
                    "#,
                    params![
                        format!("018f45d0-0000-7000-8000-000000070{index:03}"),
                        i64::from(index),
                    ],
                )
                .unwrap();
        }

        assert_eq!(store.capture_source_count().unwrap(), 3);
    }

    fn stable_tie_record(index: u16) -> HistoryRecord {
        let mut record = HistoryRecord::new(
            "Stable tie title",
            "stabletie exact equal body for deterministic fts ranking",
            vec!["stabletie".into()],
            "task",
            None,
        );
        record.id =
            Uuid::parse_str(&format!("018f45d0-0000-7000-8000-000000010{index:03}")).unwrap();
        record.created_at = fixed_time();
        record.updated_at = fixed_time();
        record
    }

    fn assert_search_order(store: &Store, expected: &[Uuid]) {
        let actual = store
            .search_records("stabletie", 10)
            .unwrap()
            .into_iter()
            .map(|record| record.id)
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn search_records_equal_fts_scores_use_record_id_across_refresh_and_reopen() {
        let temp = tempdir();
        let path = temp.path().join("work.sqlite");
        let store = Store::open(&path).unwrap();
        for index in [4, 1, 3, 2] {
            store.insert_record(&stable_tie_record(index)).unwrap();
        }

        let expected = vec![
            stable_tie_record(1).id,
            stable_tie_record(2).id,
            stable_tie_record(3).id,
            stable_tie_record(4).id,
        ];
        assert_search_order(&store, &expected);

        store.upsert_record(&stable_tie_record(3)).unwrap();
        assert_search_order(&store, &expected);

        drop(store);
        let reopened = Store::open(&path).unwrap();
        assert_search_order(&reopened, &expected);
    }

    #[test]
    fn search_records_empty_or_no_token_query_returns_empty() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let record = stable_tie_record(1);
        store.insert_record(&record).unwrap();

        assert!(store.search_records("", 10).unwrap().is_empty());
        assert!(store.search_records("!!!", 10).unwrap().is_empty());
        assert!(store.search_records("---", 10).unwrap().is_empty());
        assert!(store.search_records("___", 10).unwrap().is_empty());
        assert!(store.search_records_page("", 10, 0).unwrap().is_empty());
    }

    #[test]
    fn event_search_local_preview_preserves_private_text_but_raw_is_withheld() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let local_event = local_preview_event(
            1,
            "cwd=/home/example/private token=ghp_1234567890abcdef",
            RedactionState::LocalPreview,
        );
        let raw_event = local_preview_event(
            2,
            "raw cwd=/home/example/private token=ghp_1234567890abcdef",
            RedactionState::Raw,
        );

        store.upsert_event(&local_event).unwrap();
        store.upsert_event(&raw_event).unwrap();

        let local_preview: String = store
            .conn
            .query_row(
                "SELECT safe_preview_text FROM event_search WHERE event_id = ?1",
                [local_event.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(local_preview.contains("/home/example/private"));
        assert!(local_preview.contains("ghp_1234567890abcdef"));

        let raw_preview: String = store
            .conn
            .query_row(
                "SELECT safe_preview_text FROM event_search WHERE event_id = ?1",
                [raw_event.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(raw_preview, "raw event payload withheld");
    }

    #[test]
    fn upsert_record_updates_record_search_without_rebuilding_event_search() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO event_search
                (event_id, history_record_id, session_id, role, safe_preview_text, rank_bucket)
                VALUES ('sentinel-event', NULL, NULL, 'user', 'preserve-event-search-row', 'message')
                "#,
                [],
            )
            .unwrap();

        let record = stable_tie_record(5);
        store.upsert_record(&record).unwrap();

        let sentinel_count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_search WHERE event_id = 'sentinel-event'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(sentinel_count, 1);
        assert_search_order(&store, &[record.id]);
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    type CatalogSessionCheckpointRow = (
        String,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
    );

    fn tempdir() -> tempfile::TempDir {
        let root = std::env::current_dir().unwrap().join("target/test-data");
        fs::create_dir_all(&root).unwrap();
        tempfile::Builder::new()
            .prefix("ctx-history-store-catalog-")
            .tempdir_in(root)
            .unwrap()
    }

    fn fixed_time() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-23T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn timestamps() -> EntityTimestamps {
        EntityTimestamps {
            created_at: fixed_time(),
            updated_at: fixed_time(),
        }
    }

    fn sync_metadata() -> SyncMetadata {
        SyncMetadata {
            visibility: Visibility::LocalOnly,
            fidelity: Fidelity::Imported,
            sync_state: SyncState::LocalOnly,
            sync_version: 0,
            deleted_at: None,
            metadata: serde_json::json!({}),
        }
    }

    fn catalog_session(
        source_path: &str,
        external_session_id: &str,
        mtime_ms: i64,
    ) -> CatalogSession {
        CatalogSession {
            provider: CaptureProvider::Codex,
            source_format: "codex_session_jsonl".into(),
            source_root: "/home/user/.codex/sessions".into(),
            source_path: source_path.into(),
            external_session_id: Some(external_session_id.into()),
            parent_external_session_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".into()),
            external_agent_id: None,
            cwd: Some("/repo".into()),
            session_started_at_ms: Some(mtime_ms),
            file_size_bytes: 42,
            file_modified_at_ms: mtime_ms,
            cataloged_at_ms: mtime_ms,
            metadata: serde_json::json!({"catalog_scope": "session_meta"}),
        }
    }

    fn imported_session(external_session_id: &str) -> Session {
        Session {
            id: new_id(),
            history_record_id: None,
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: None,
            provider: CaptureProvider::Codex,
            external_session_id: Some(external_session_id.into()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".into()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: fixed_time(),
            ended_at: None,
            timestamps: timestamps(),
            sync: sync_metadata(),
        }
    }

    fn session_event(session_id: Uuid, index: u64) -> Event {
        Event {
            id: new_id(),
            seq: index,
            history_record_id: None,
            session_id: Some(session_id),
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::Assistant),
            occurred_at: fixed_time() + chrono::Duration::seconds(index as i64),
            capture_source_id: None,
            payload: serde_json::json!({"index": index}),
            payload_blob_id: None,
            dedupe_key: None,
            redaction_state: RedactionState::LocalPreview,
            sync: sync_metadata(),
        }
    }

    fn artifact_record(id: Uuid, byte_size: u64) -> Artifact {
        Artifact {
            id,
            kind: ArtifactKind::Markdown,
            blob_hash: format!("{:064x}", 1),
            blob_path: format!("{OBJECTS_DIR}/00/test-artifact"),
            byte_size,
            media_type: Some("text/markdown".to_owned()),
            preview_text: Some("artifact preview".to_owned()),
            redaction_state: RedactionState::LocalPreview,
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        }
    }

    fn assert_sql_conversion_error<T: std::fmt::Debug>(result: Result<T>) {
        assert!(
            matches!(result, Err(StoreError::Sql(_))),
            "expected sqlite conversion error, got {result:?}"
        );
    }

    #[test]
    fn catalog_session_upsert_skips_unchanged_rows() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let cataloged_at_ms = timestamp_ms(fixed_time());
        let session = catalog_session(
            "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
            "codex-session-1",
            cataloged_at_ms,
        );
        store
            .upsert_catalog_sessions(std::slice::from_ref(&session))
            .unwrap();
        let after_insert: i64 = store
            .conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();

        let mut recataloged = session.clone();
        recataloged.cataloged_at_ms += 1_000;
        store
            .upsert_catalog_sessions(std::slice::from_ref(&recataloged))
            .unwrap();
        let after_noop: i64 = store
            .conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        assert_eq!(after_noop, after_insert);

        let mut changed = recataloged;
        changed.file_size_bytes += 1;
        changed.cataloged_at_ms += 1_000;
        store
            .upsert_catalog_sessions(std::slice::from_ref(&changed))
            .unwrap();
        let after_changed: i64 = store
            .conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        assert!(after_changed > after_noop);
    }

    #[test]
    fn events_for_session_window_returns_bounded_neighbors() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let session = imported_session("window-session");
        store.upsert_session(&session).unwrap();
        let events = (0..10)
            .map(|index| {
                let event = session_event(session.id, index);
                store.upsert_event(&event).unwrap();
                event
            })
            .collect::<Vec<_>>();

        let middle = store
            .events_for_session_window(&events[5], 2, 3)
            .unwrap()
            .into_iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>();
        assert_eq!(middle, vec![3, 4, 5, 6, 7, 8]);

        let first = store
            .events_for_session_window(&events[0], 50, 1)
            .unwrap()
            .into_iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>();
        assert_eq!(first, vec![0, 1]);

        let last = store
            .events_for_session_window(&events[9], 1, 50)
            .unwrap()
            .into_iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>();
        assert_eq!(last, vec![8, 9]);
    }

    #[test]
    fn sessions_by_external_session_limited_caps_ambiguity_scan() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        for index in 0..5 {
            let mut session = imported_session("shared-provider-session");
            session.started_at = fixed_time() + chrono::Duration::seconds(index);
            store.upsert_session(&session).unwrap();
        }

        let matches = store
            .sessions_by_external_session_limited(
                CaptureProvider::Codex,
                "shared-provider-session",
                2,
            )
            .unwrap();

        assert_eq!(matches.len(), 2);
        assert_eq!(
            matches[0].external_session_id.as_deref(),
            Some("shared-provider-session")
        );
    }

    #[test]
    fn search_index_optimize_is_safe_on_initialized_store() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        store.optimize_search_index().unwrap();
    }

    #[test]
    fn catalog_sessions_count_indexed_and_stale_rows() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let cataloged_at_ms = timestamp_ms(fixed_time());
        store
            .upsert_catalog_sessions(&[catalog_session(
                "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
                "codex-session-1",
                cataloged_at_ms,
            )])
            .unwrap();

        let counts = store.catalog_session_counts().unwrap();
        assert_eq!(counts.total, 1);
        assert_eq!(counts.indexed, 0);
        assert_eq!(counts.stale, 0);
        assert_eq!(counts.pending, 1);
        assert_eq!(counts.failed, 0);
        assert_eq!(
            store
                .catalog_source_stale_session_count(
                    CaptureProvider::Codex,
                    "/home/user/.codex/sessions"
                )
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .list_pending_catalog_sessions(CaptureProvider::Codex, "/home/user/.codex/sessions")
                .unwrap()
                .len(),
            1
        );

        store
            .upsert_session(&imported_session("codex-session-1"))
            .unwrap();
        store
            .mark_catalog_source_indexed(
                CaptureProvider::Codex,
                CatalogSourceIndexUpdate {
                    source_root: "/home/user/.codex/sessions",
                    source_path: "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
                    file_size_bytes: 42,
                    file_modified_at_ms: cataloged_at_ms,
                    file_sha256: None,
                    event_count: Some(3),
                    indexed_at_ms: cataloged_at_ms + 10,
                },
            )
            .unwrap();
        let counts = store.catalog_session_counts().unwrap();
        assert_eq!(counts.indexed, 1);
        assert_eq!(counts.pending, 0);

        store
            .mark_catalog_source_stale(
                CaptureProvider::Codex,
                "/home/user/.codex/sessions",
                cataloged_at_ms + 1,
            )
            .unwrap();
        let counts = store.catalog_session_counts().unwrap();
        assert_eq!(counts.total, 0);
        assert_eq!(counts.indexed, 0);
        assert_eq!(counts.stale, 1);
        assert_eq!(counts.pending, 0);
        assert_eq!(
            store
                .catalog_source_stale_session_count(
                    CaptureProvider::Codex,
                    "/home/user/.codex/sessions"
                )
                .unwrap(),
            1
        );
    }

    #[test]
    fn catalog_import_planning_requires_current_index_state_and_matching_session() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let cataloged_at_ms = timestamp_ms(fixed_time());
        store
            .upsert_catalog_sessions(&[catalog_session(
                "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
                "codex-session-1",
                cataloged_at_ms,
            )])
            .unwrap();
        store
            .mark_catalog_source_indexed(
                CaptureProvider::Codex,
                CatalogSourceIndexUpdate {
                    source_root: "/home/user/.codex/sessions",
                    source_path: "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
                    file_size_bytes: 42,
                    file_modified_at_ms: cataloged_at_ms,
                    file_sha256: None,
                    event_count: Some(3),
                    indexed_at_ms: cataloged_at_ms + 10,
                },
            )
            .unwrap();

        let pending = store
            .list_pending_catalog_sessions(CaptureProvider::Codex, "/home/user/.codex/sessions")
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(store.catalog_session_counts().unwrap().indexed, 0);

        store
            .upsert_session(&imported_session("codex-session-1"))
            .unwrap();
        let pending = store
            .list_pending_catalog_sessions(CaptureProvider::Codex, "/home/user/.codex/sessions")
            .unwrap();
        assert!(pending.is_empty());
        let counts = store.catalog_session_counts().unwrap();
        assert_eq!(counts.indexed, 1);
        assert_eq!(counts.pending, 0);
    }

    #[test]
    fn catalog_import_mark_failed_records_error_and_remains_pending() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let cataloged_at_ms = timestamp_ms(fixed_time());
        store
            .upsert_catalog_sessions(&[catalog_session(
                "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
                "codex-session-1",
                cataloged_at_ms,
            )])
            .unwrap();

        let changed = store
            .mark_catalog_source_failed(
                CaptureProvider::Codex,
                "/home/user/.codex/sessions",
                "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
                "bad json",
                cataloged_at_ms + 10,
            )
            .unwrap();
        assert_eq!(changed, 1);

        let counts = store.catalog_session_counts().unwrap();
        assert_eq!(counts.failed, 1);
        assert_eq!(counts.pending, 1);
        let (status, error, indexed_at_ms): (String, Option<String>, Option<i64>) = store
            .conn
            .query_row(
                "SELECT indexed_status, indexed_error, indexed_at_ms FROM catalog_sessions WHERE source_path = ?1",
                ["/home/user/.codex/sessions/2026/06/24/rollout.jsonl"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, CatalogIndexedStatus::Failed.as_str());
        assert_eq!(error.as_deref(), Some("bad json"));
        assert_eq!(indexed_at_ms, Some(cataloged_at_ms + 10));
    }

    #[test]
    fn catalog_upsert_clears_completion_metadata_but_preserves_append_checkpoint() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let cataloged_at_ms = timestamp_ms(fixed_time());
        let source_path = "/home/user/.codex/sessions/2026/06/24/rollout.jsonl";
        store
            .upsert_catalog_sessions(&[catalog_session(
                source_path,
                "codex-session-1",
                cataloged_at_ms,
            )])
            .unwrap();
        store
            .upsert_session(&imported_session("codex-session-1"))
            .unwrap();
        store
            .mark_catalog_source_indexed(
                CaptureProvider::Codex,
                CatalogSourceIndexUpdate {
                    source_root: "/home/user/.codex/sessions",
                    source_path,
                    file_size_bytes: 42,
                    file_modified_at_ms: cataloged_at_ms,
                    file_sha256: None,
                    event_count: Some(3),
                    indexed_at_ms: cataloged_at_ms + 10,
                },
            )
            .unwrap();

        store
            .upsert_catalog_sessions(&[catalog_session(
                source_path,
                "codex-session-1",
                cataloged_at_ms,
            )])
            .unwrap();
        assert_eq!(store.catalog_session_counts().unwrap().indexed, 1);

        let mut changed = catalog_session(source_path, "codex-session-1", cataloged_at_ms + 1);
        changed.file_size_bytes = 43;
        store.upsert_catalog_sessions(&[changed]).unwrap();

        let counts = store.catalog_session_counts().unwrap();
        assert_eq!(counts.indexed, 0);
        assert_eq!(counts.pending, 1);
        let (
            status,
            indexed_at_ms,
            indexed_size,
            indexed_mtime,
            indexed_event_count,
            checkpoint_at_ms,
            checkpoint_size,
            checkpoint_mtime,
            checkpoint_event_count,
        ): CatalogSessionCheckpointRow = store
            .conn
            .query_row(
                "SELECT indexed_status, indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_event_count, last_imported_at_ms, last_imported_file_size_bytes, last_imported_file_modified_at_ms, last_imported_event_count FROM catalog_sessions WHERE source_path = ?1",
                [source_path],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(status, CatalogIndexedStatus::Pending.as_str());
        assert_eq!(indexed_at_ms, None);
        assert_eq!(indexed_size, None);
        assert_eq!(indexed_mtime, None);
        assert_eq!(indexed_event_count, None);
        assert_eq!(checkpoint_at_ms, Some(cataloged_at_ms + 10));
        assert_eq!(checkpoint_size, Some(42));
        assert_eq!(checkpoint_mtime, Some(cataloged_at_ms));
        assert_eq!(checkpoint_event_count, Some(3));

        let checkpoint = store
            .catalog_source_index_state(
                CaptureProvider::Codex,
                "/home/user/.codex/sessions",
                source_path,
            )
            .unwrap()
            .unwrap();
        assert_eq!(checkpoint.last_imported_file_size_bytes, Some(42));
        assert_eq!(
            checkpoint.last_imported_file_modified_at_ms,
            Some(cataloged_at_ms)
        );
        assert_eq!(checkpoint.last_imported_file_sha256, None);
        assert_eq!(checkpoint.last_imported_event_count, Some(3));
        assert_eq!(checkpoint.last_imported_at_ms, Some(cataloged_at_ms + 10));
    }

    #[test]
    fn catalog_upsert_invalidates_checkpoint_for_shrink_and_same_size_change() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let cataloged_at_ms = timestamp_ms(fixed_time());
        for (source_path, file_size_bytes) in [
            ("/home/user/.codex/sessions/2026/06/24/shrink.jsonl", 41_u64),
            (
                "/home/user/.codex/sessions/2026/06/24/same-size.jsonl",
                42_u64,
            ),
        ] {
            store
                .upsert_catalog_sessions(&[catalog_session(
                    source_path,
                    source_path,
                    cataloged_at_ms,
                )])
                .unwrap();
            store
                .upsert_session(&imported_session(source_path))
                .unwrap();
            store
                .mark_catalog_source_indexed(
                    CaptureProvider::Codex,
                    CatalogSourceIndexUpdate {
                        source_root: "/home/user/.codex/sessions",
                        source_path,
                        file_size_bytes: 42,
                        file_modified_at_ms: cataloged_at_ms,
                        file_sha256: None,
                        event_count: Some(3),
                        indexed_at_ms: cataloged_at_ms + 10,
                    },
                )
                .unwrap();

            let mut changed = catalog_session(source_path, source_path, cataloged_at_ms + 1);
            changed.file_size_bytes = file_size_bytes;
            store.upsert_catalog_sessions(&[changed]).unwrap();

            let (status, indexed_size, checkpoint_size): (String, Option<i64>, Option<i64>) =
                store
                    .conn
                    .query_row(
                        "SELECT indexed_status, indexed_file_size_bytes, last_imported_file_size_bytes FROM catalog_sessions WHERE source_path = ?1",
                        [source_path],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .unwrap();
            assert_eq!(status, CatalogIndexedStatus::Pending.as_str());
            assert_eq!(indexed_size, None);
            assert_eq!(checkpoint_size, None);
        }
    }

    #[test]
    fn catalog_index_checkpoint_event_count_can_be_unknown() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let cataloged_at_ms = timestamp_ms(fixed_time());
        let source_path = "/home/user/.codex/sessions/2026/06/24/unknown-count.jsonl";
        store
            .upsert_catalog_sessions(&[catalog_session(
                source_path,
                "codex-session-unknown-count",
                cataloged_at_ms,
            )])
            .unwrap();
        store
            .mark_catalog_source_indexed(
                CaptureProvider::Codex,
                CatalogSourceIndexUpdate {
                    source_root: "/home/user/.codex/sessions",
                    source_path,
                    file_size_bytes: 42,
                    file_modified_at_ms: cataloged_at_ms,
                    file_sha256: Some("abc123"),
                    event_count: None,
                    indexed_at_ms: cataloged_at_ms + 10,
                },
            )
            .unwrap();

        let checkpoint = store
            .catalog_source_index_state(
                CaptureProvider::Codex,
                "/home/user/.codex/sessions",
                source_path,
            )
            .unwrap()
            .unwrap();
        assert_eq!(checkpoint.last_imported_event_count, None);
        assert_eq!(
            checkpoint.last_imported_file_sha256.as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn source_import_manifest_upsert_ignores_observed_at_for_unchanged_files() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let observed_at_ms = timestamp_ms(fixed_time());
        let mut file = SourceImportFile {
            provider: CaptureProvider::Claude,
            source_format: "claude_projects_jsonl_tree".into(),
            source_root: "/home/user/.claude/projects".into(),
            source_path: "/home/user/.claude/projects/session.jsonl".into(),
            file_size_bytes: 42,
            file_modified_at_ms: observed_at_ms,
            observed_at_ms,
            metadata: serde_json::json!({}),
        };
        store
            .upsert_source_import_files(std::slice::from_ref(&file))
            .unwrap();
        store
            .mark_source_import_file_indexed(
                CaptureProvider::Claude,
                SourceImportFileIndexUpdate {
                    source_root: "/home/user/.claude/projects",
                    source_path: "/home/user/.claude/projects/session.jsonl",
                    file_size_bytes: 42,
                    file_modified_at_ms: observed_at_ms,
                    indexed_at_ms: observed_at_ms + 10,
                },
            )
            .unwrap();
        let after_indexed: i64 = store
            .conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();

        file.observed_at_ms += 1_000;
        store
            .upsert_source_import_files(std::slice::from_ref(&file))
            .unwrap();
        let after_noop: i64 = store
            .conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        assert_eq!(after_noop, after_indexed);
        assert!(store
            .list_pending_source_import_files(
                CaptureProvider::Claude,
                "/home/user/.claude/projects"
            )
            .unwrap()
            .is_empty());
    }

    #[test]
    fn catalog_schema_includes_import_state_columns() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let schema = store.schema().unwrap();
        assert!(schema.contains("indexed_at_ms INTEGER"));
        assert!(schema.contains("indexed_file_size_bytes INTEGER"));
        assert!(schema.contains("indexed_file_modified_at_ms INTEGER"));
        assert!(schema.contains("indexed_status TEXT NOT NULL DEFAULT 'pending'"));
        assert!(schema.contains("indexed_error TEXT"));
        assert!(schema.contains("indexed_event_count INTEGER"));
        assert!(schema.contains("last_imported_at_ms INTEGER"));
        assert!(schema.contains("last_imported_file_size_bytes INTEGER"));
        assert!(schema.contains("last_imported_file_modified_at_ms INTEGER"));
        assert!(schema.contains("last_imported_file_sha256 TEXT"));
        assert!(schema.contains("last_imported_event_count INTEGER"));
    }

    #[test]
    fn raw_sql_query_reads_stable_views() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let schema = store.schema().unwrap();
        for view in [
            "CREATE VIEW ctx_sessions",
            "CREATE VIEW ctx_events",
            "CREATE VIEW ctx_files_touched",
            "CREATE VIEW ctx_sources",
        ] {
            assert!(schema.contains(view), "schema missing {view}");
        }

        let result = store
            .raw_sql_query(
                "SELECT COUNT(*) AS session_count FROM ctx_sessions",
                RawSqlOptions::default(),
            )
            .unwrap();
        assert_eq!(result.columns[0].name, "session_count");
        assert_eq!(result.returned_rows, 1);
        assert_eq!(result.rows[0][0], RawSqlValue::Integer(0));
    }

    #[test]
    fn ctx_files_touched_resolves_session_from_source_id() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let record_id = "018f45d0-0000-7000-8000-000000080001";
        let source_id = "018f45d0-0000-7000-8000-000000080002";
        let session_id = "018f45d0-0000-7000-8000-000000080003";
        let touch_id = "018f45d0-0000-7000-8000-000000080004";
        let detached_source_id = "018f45d0-0000-7000-8000-000000080005";
        let detached_touch_id = "018f45d0-0000-7000-8000-000000080006";

        store
            .conn
            .execute(
                r#"
                INSERT INTO history_records
                (id, title, last_activity_at_ms, created_at_ms, updated_at_ms, body, created_at, updated_at)
                VALUES (?1, 'Touched file view record', 1, 1, 1, '', '', '')
                "#,
                [record_id],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO capture_sources
                (id, kind, provider, machine_id, raw_source_path, external_session_id, started_at_ms, fidelity)
                VALUES (?1, 'provider_import', 'codex', 'test-machine', '/tmp/session.jsonl', 'codex-session-1', 1, 'imported')
                "#,
                [source_id],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO capture_sources
                (id, kind, provider, machine_id, raw_source_path, external_session_id, started_at_ms, fidelity)
                VALUES (?1, 'provider_import', 'opencode', 'test-machine', '/tmp/opencode.db', 'opencode-session-1', 1, 'imported')
                "#,
                [detached_source_id],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO sessions
                (
                    id, history_record_id, capture_source_id, provider, external_session_id,
                    agent_type, is_primary, status, fidelity, started_at_ms, created_at_ms, updated_at_ms
                )
                VALUES (?1, ?2, ?3, 'codex', 'codex-session-1', 'primary', 1, 'imported', 'imported', 1, 1, 1)
                "#,
                params![session_id, record_id, source_id],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO files_touched
                (id, source_id, path, change_kind, confidence, created_at_ms, updated_at_ms, fidelity)
                VALUES (?1, ?2, 'src/main.rs', 'modified', 'explicit', 1, 1, 'imported')
                "#,
                params![touch_id, source_id],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO files_touched
                (id, source_id, path, change_kind, confidence, created_at_ms, updated_at_ms, fidelity)
                VALUES (?1, ?2, 'detached.rs', 'modified', 'explicit', 1, 1, 'imported')
                "#,
                params![detached_touch_id, detached_source_id],
            )
            .unwrap();

        let result = store
            .raw_sql_query(
                "SELECT provider, provider_session_id, ctx_session_id, history_record_id FROM ctx_files_touched WHERE path = 'src/main.rs'",
                RawSqlOptions::default(),
            )
            .unwrap();
        assert_eq!(result.returned_rows, 1);
        assert_eq!(
            result.rows[0][0],
            RawSqlValue::Text {
                value: "codex".to_owned(),
                bytes: 5,
                truncated: false,
            }
        );
        assert_eq!(
            result.rows[0][1],
            RawSqlValue::Text {
                value: "codex-session-1".to_owned(),
                bytes: 15,
                truncated: false,
            }
        );
        assert_eq!(
            result.rows[0][2],
            RawSqlValue::Text {
                value: session_id.to_owned(),
                bytes: session_id.len(),
                truncated: false,
            }
        );
        assert_eq!(
            result.rows[0][3],
            RawSqlValue::Text {
                value: record_id.to_owned(),
                bytes: record_id.len(),
                truncated: false,
            }
        );

        let detached = store
            .raw_sql_query(
                "SELECT provider, provider_session_id, ctx_session_id, history_record_id FROM ctx_files_touched WHERE path = 'detached.rs'",
                RawSqlOptions::default(),
            )
            .unwrap();
        assert_eq!(detached.returned_rows, 1);
        assert_eq!(
            detached.rows[0][0],
            RawSqlValue::Text {
                value: "opencode".to_owned(),
                bytes: 8,
                truncated: false,
            }
        );
        assert_eq!(
            detached.rows[0][1],
            RawSqlValue::Text {
                value: "opencode-session-1".to_owned(),
                bytes: 18,
                truncated: false,
            }
        );
        assert_eq!(detached.rows[0][2], RawSqlValue::Null);
        assert_eq!(detached.rows[0][3], RawSqlValue::Null);
    }

    #[test]
    fn raw_sql_query_rejects_writes_parameters_and_multiple_statements() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();

        assert!(matches!(
            store
                .raw_sql_query("", RawSqlOptions::default())
                .unwrap_err(),
            StoreError::RawSqlEmpty
        ));
        assert!(matches!(
            store
                .raw_sql_query("SELECT ?1", RawSqlOptions::default())
                .unwrap_err(),
            StoreError::RawSqlHasParameters
        ));
        assert!(matches!(
            store
                .raw_sql_query("CREATE TABLE nope(x INTEGER)", RawSqlOptions::default())
                .unwrap_err(),
            StoreError::RawSqlNotReadOnly
        ));
        assert!(matches!(
            store
                .raw_sql_query("SELECT 1; SELECT 2", RawSqlOptions::default())
                .unwrap_err(),
            StoreError::Sql(rusqlite::Error::MultipleStatement)
        ));
    }

    #[test]
    fn raw_sql_query_caps_rows_and_values() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let result = store
            .raw_sql_query(
                "SELECT 'abcdef' AS text_value, X'01020304' AS blob_value UNION ALL SELECT 'ghijkl', X'05060708'",
                RawSqlOptions {
                    max_rows: 1,
                    max_value_bytes: 3,
                    ..RawSqlOptions::default()
                },
            )
            .unwrap();
        assert_eq!(result.returned_rows, 1);
        assert_eq!(result.columns[0].name, "text_value");
        assert_eq!(result.columns[1].name, "blob_value");
        assert_eq!(
            result.rows[0][0],
            RawSqlValue::Text {
                value: "abc".to_owned(),
                bytes: 6,
                truncated: true,
            }
        );
        assert_eq!(
            result.rows[0][1],
            RawSqlValue::Blob {
                bytes: 4,
                preview_hex: "010203".to_owned(),
                truncated: true,
            }
        );
        assert!(result.truncated.rows);
        assert!(result.truncated.values);
    }

    #[test]
    fn row_readers_reject_negative_unsigned_columns() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let bad_process_id = new_id();
        store
            .conn
            .execute(
                r#"
                INSERT INTO capture_sources
                (
                    id, kind, provider, machine_id, process_id, cwd, raw_source_path,
                    external_session_id, started_at_ms, fidelity, sync_version
                )
                VALUES (?1, 'provider_import', 'codex', 'test-machine', -1, '/repo', '/tmp/session.jsonl', 'session', 1, 'imported', 0)
                "#,
                params![bad_process_id.to_string()],
            )
            .unwrap();
        assert_sql_conversion_error(store.get_capture_source(bad_process_id));

        let bad_sync_version = new_id();
        store
            .conn
            .execute(
                r#"
                INSERT INTO capture_sources
                (
                    id, kind, provider, machine_id, cwd, raw_source_path,
                    external_session_id, started_at_ms, fidelity, sync_version
                )
                VALUES (?1, 'provider_import', 'codex', 'test-machine', '/repo', '/tmp/session.jsonl', 'session', 1, 'imported', -1)
                "#,
                params![bad_sync_version.to_string()],
            )
            .unwrap();
        assert_sql_conversion_error(store.get_capture_source(bad_sync_version));

        let event = Event {
            id: new_id(),
            seq: 1,
            history_record_id: None,
            session_id: None,
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::Assistant),
            occurred_at: fixed_time(),
            capture_source_id: None,
            payload: serde_json::json!({"text": "negative seq marker"}),
            payload_blob_id: None,
            dedupe_key: None,
            redaction_state: RedactionState::LocalPreview,
            sync: sync_metadata(),
        };
        store.upsert_event(&event).unwrap();
        store
            .conn
            .execute(
                "UPDATE events SET seq = -1 WHERE id = ?1",
                params![event.id.to_string()],
            )
            .unwrap();
        assert_sql_conversion_error(store.get_event(event.id));
        assert_sql_conversion_error(store.search_event_hits("negative seq marker", 1));

        let artifact = artifact_record(new_id(), 1);
        store.upsert_artifact(&artifact).unwrap();
        store
            .conn
            .execute(
                "UPDATE artifacts SET byte_size = -1 WHERE id = ?1",
                params![artifact.id.to_string()],
            )
            .unwrap();
        assert_sql_conversion_error(store.list_artifacts());
    }

    #[test]
    fn raw_sql_query_rejects_excessive_result_preview_budget() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let many_columns = (0..RAW_SQL_MAX_COLUMNS_CAP)
            .map(|index| format!("1 AS c{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let err = store
            .raw_sql_query(
                &format!("SELECT {many_columns}"),
                RawSqlOptions {
                    max_rows: RAW_SQL_MAX_ROWS_CAP,
                    max_columns: RAW_SQL_MAX_COLUMNS_CAP,
                    max_value_bytes: 32,
                    ..RawSqlOptions::default()
                },
            )
            .unwrap_err();
        assert!(matches!(
            err,
            StoreError::RawSqlResultBudgetTooLarge {
                max_result_bytes: RAW_SQL_MAX_RESULT_PREVIEW_BYTES,
                ..
            }
        ));
    }

    #[test]
    fn raw_sql_query_budgets_against_actual_column_count() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let result = store
            .raw_sql_query(
                "SELECT 1",
                RawSqlOptions {
                    max_rows: RAW_SQL_MAX_ROWS_CAP,
                    max_columns: RAW_SQL_MAX_COLUMNS_CAP,
                    max_value_bytes: 32,
                    ..RawSqlOptions::default()
                },
            )
            .unwrap();
        assert_eq!(result.returned_rows, 1);
        assert_eq!(result.rows[0][0], RawSqlValue::Integer(1));
    }

    #[test]
    fn raw_sql_query_times_out_long_running_queries() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let err = store
            .raw_sql_query(
                r#"
                WITH RECURSIVE numbers(x) AS (
                    SELECT 1
                    UNION ALL
                    SELECT x + 1 FROM numbers WHERE x < 100000000
                )
                SELECT sum(x) FROM numbers
                "#,
                RawSqlOptions {
                    timeout: Duration::from_millis(1),
                    ..RawSqlOptions::default()
                },
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::RawSqlTimedOut { .. }));
    }

    #[test]
    fn raw_sql_query_enforces_sqlite_value_length_limit() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let err = store
            .raw_sql_query(
                "SELECT length(randomblob(200000))",
                RawSqlOptions::default(),
            )
            .unwrap_err();
        assert!(matches!(
            err,
            StoreError::Sql(rusqlite::Error::SqliteFailure(error, _))
                if error.code == ErrorCode::TooBig
        ));
    }

    #[test]
    fn schema_v8_migrates_legacy_history_record_table_names() {
        let temp = tempdir();
        let path = temp.path().join("work.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&legacy_history_record_sql(CREATE_TABLES_SQL))
                .unwrap();
            conn.execute_batch(&legacy_history_record_sql(FTS_TABLES_SQL))
                .unwrap();
            let record_id = new_id();
            conn.execute(
                "INSERT INTO work_records (id, title, last_activity_at_ms, body, created_at, updated_at)
                 VALUES (?1, 'Legacy record', 0, '', '2026-06-23T12:00:00+00:00', '2026-06-23T12:00:00+00:00')",
                [record_id.to_string()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions
                 (id, work_record_id, provider, agent_type, is_primary, status, fidelity, started_at_ms, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, 'codex', 'primary', 1, 'imported', 'partial', 0, 0, 0)",
                params![new_id().to_string(), record_id.to_string()],
            )
            .unwrap();
            conn.execute_batch("PRAGMA user_version = 7;").unwrap();
        }

        let store = Store::open(&path).unwrap();
        assert!(table_exists(&store.conn, "history_records").unwrap());
        assert!(!table_exists(&store.conn, "work_records").unwrap());
        assert!(table_exists(&store.conn, "history_record_links").unwrap());
        assert!(!table_exists(&store.conn, "work_record_links").unwrap());
        for table in ["sessions", "runs", "events", "summaries", "files_touched"] {
            assert!(table_has_column(&store.conn, table, "history_record_id").unwrap());
            assert!(!table_has_column(&store.conn, table, "work_record_id").unwrap());
        }
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn schema_v12_invalidates_provider_import_indexes_for_reimport() {
        let temp = tempdir();
        let path = temp.path().join("work.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(CREATE_TABLES_SQL).unwrap();
            conn.execute(
                r#"
                INSERT INTO catalog_sessions
                (
                    source_path, provider, source_format, source_root, external_session_id,
                    agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms,
                    indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms,
                    indexed_status, indexed_event_count
                )
                VALUES
                (
                    '/tmp/codex/session.jsonl', 'codex', 'codex_rollout_jsonl', '/tmp/codex',
                    'session-1', 'primary', 10, 20, 30, 40, 10, 20, 'indexed', 5
                )
                "#,
                [],
            )
            .unwrap();
            conn.execute(
                r#"
                INSERT INTO source_import_files
                (
                    provider, source_format, source_root, source_path,
                    file_size_bytes, file_modified_at_ms, observed_at_ms,
                    indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms,
                    indexed_status
                )
                VALUES
                (
                    'antigravity', 'antigravity_cli_transcript_jsonl', '/tmp/agy',
                    '/tmp/agy/transcript.jsonl', 10, 20, 30, 40, 10, 20, 'indexed'
                )
                "#,
                [],
            )
            .unwrap();
            conn.execute_batch("PRAGMA user_version = 11;").unwrap();
        }

        let store = Store::open(&path).unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        let catalog_status: (String, Option<i64>, Option<i64>, Option<i64>, Option<i64>) = store
            .conn
            .query_row(
                "SELECT indexed_status, indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_event_count FROM catalog_sessions",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            catalog_status,
            ("pending".to_owned(), None, None, None, None)
        );

        let file_status: (String, Option<i64>, Option<i64>, Option<i64>) = store
            .conn
            .query_row(
                "SELECT indexed_status, indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms FROM source_import_files",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(file_status, ("pending".to_owned(), None, None, None));
    }

    #[test]
    fn schema_v14_backfills_catalog_import_checkpoints() {
        let temp = tempdir();
        let path = temp.path().join("work.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            let legacy_sql = CREATE_TABLES_SQL
                .replace("    last_imported_at_ms INTEGER,\n", "")
                .replace("    last_imported_file_size_bytes INTEGER,\n", "")
                .replace("    last_imported_file_modified_at_ms INTEGER,\n", "")
                .replace("    last_imported_file_sha256 TEXT,\n", "")
                .replace("    last_imported_event_count INTEGER,\n", "");
            conn.execute_batch(&legacy_sql).unwrap();
            conn.execute(
                r#"
                INSERT INTO catalog_sessions
                (
                    source_path, provider, source_format, source_root, external_session_id,
                    agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms,
                    indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms,
                    indexed_status, indexed_event_count
                )
                VALUES
                (
                    '/tmp/codex/session.jsonl', 'codex', 'codex_rollout_jsonl', '/tmp/codex',
                    'session-1', 'primary', 20, 30, 40, 50, 10, 15, 'pending', 7
                )
                "#,
                [],
            )
            .unwrap();
            conn.execute_batch("PRAGMA user_version = 13;").unwrap();
        }

        let store = Store::open(&path).unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        let checkpoint: (Option<i64>, Option<i64>, Option<i64>, Option<i64>) = store
            .conn
            .query_row(
                "SELECT last_imported_at_ms, last_imported_file_size_bytes, last_imported_file_modified_at_ms, last_imported_event_count FROM catalog_sessions",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(checkpoint, (Some(50), Some(10), Some(15), Some(7)));
    }

    fn legacy_history_record_sql(sql: &str) -> String {
        sql.replace("history_record_links", "work_record_links")
            .replace("history_record_tags", "work_record_tags")
            .replace("history_records", "work_records")
            .replace("history_record_id", "work_record_id")
    }

    #[test]
    fn provider_check_constraints_accept_search_only_providers() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        rebuild_capture_sources_provider_check(&store.conn).unwrap();
        rebuild_catalog_sessions_provider_check(&store.conn).unwrap();

        let schema = store.schema().unwrap();
        for (provider, source_format) in [
            ("kilo", "kilo_sqlite"),
            ("crush", "crush_sqlite"),
            ("goose", "goose_sessions_sqlite"),
            ("dexto", "dexto_sqlite"),
            ("lingma", "lingma_sqlite"),
            ("pochi", "pochi_livestore_state_sqlite"),
            ("openloaf", "openloaf_chat_jsonl"),
            ("auggie", "auggie_session_json"),
            ("firebender", "firebender_chat_history_sqlite"),
            ("copilot_cli", "copilot_cli_session_events_jsonl"),
            ("factory_ai_droid", "factory_ai_droid_sessions_jsonl"),
            ("continue", "continue_cli_sessions_json"),
            ("openhands", "openhands_file_events"),
            ("qwen_code", "qwen_code_chat_jsonl"),
            ("kimi_code_cli", "kimi_code_cli_wire_jsonl"),
            ("autohand_code", "autohand_code_sessions_jsonl"),
            ("kiro_cli", "kiro_cli_sqlite"),
            ("iflow_cli", "iflow_cli_session_jsonl"),
            ("jazz", "jazz_history_json"),
            ("forgecode", "forgecode_sqlite"),
            ("deepagents", "deepagents_sessions_sqlite"),
            ("mistral_vibe", "mistral_vibe_session_jsonl"),
            ("mux", "mux_session_jsonl"),
            ("reasonix", "reasonix_session_jsonl"),
            ("kode", "kode_session_jsonl"),
            ("neovate", "neovate_session_jsonl"),
            ("command_code", "command_code_session_jsonl"),
            ("terramind", "terramind_agents_sqlite"),
            ("rovodev", "rovodev_session_json"),
            ("cortex_code", "cortex_code_session_json"),
            ("codebuddy", "codebuddy_history_json"),
            ("aider_desk", "aider_desk_task_context_json"),
            ("windsurf", "windsurf_cascade_hook_transcript_jsonl"),
            ("zed", "zed_threads_sqlite"),
            ("custom", "ctx_history_jsonl_v1"),
        ] {
            assert!(
                schema.contains(provider),
                "schema provider checks should include {provider}"
            );
            store
                .conn
                .execute(
                    r#"
                    INSERT INTO capture_sources
                    (id, kind, provider, machine_id, started_at_ms, fidelity)
                    VALUES (?1, 'provider_import', ?2, 'test-machine', 0, 'partial')
                    "#,
                    params![new_id().to_string(), provider],
                )
                .unwrap();
            store
                .conn
                .execute(
                    r#"
                    INSERT INTO catalog_sessions
                    (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
                    VALUES (?1, ?2, ?3, '/tmp/provider', 'primary', 1, 0, 0)
                    "#,
                    params![format!("/tmp/provider/{provider}.jsonl"), provider, source_format],
                )
                .unwrap();
        }

        let source_count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM capture_sources WHERE provider IN ('kilo', 'crush', 'goose', 'dexto', 'lingma', 'pochi', 'openloaf', 'copilot_cli', 'factory_ai_droid', 'continue', 'openhands', 'qwen_code', 'kimi_code_cli', 'autohand_code', 'kiro_cli', 'iflow_cli', 'jazz', 'auggie', 'firebender', 'forgecode', 'deepagents', 'mistral_vibe', 'mux', 'reasonix', 'kode', 'neovate', 'command_code', 'terramind', 'rovodev', 'cortex_code', 'codebuddy', 'aider_desk', 'windsurf', 'zed', 'custom')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let catalog_count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM catalog_sessions WHERE provider IN ('kilo', 'crush', 'goose', 'dexto', 'lingma', 'pochi', 'openloaf', 'copilot_cli', 'factory_ai_droid', 'continue', 'openhands', 'qwen_code', 'kimi_code_cli', 'autohand_code', 'kiro_cli', 'iflow_cli', 'jazz', 'auggie', 'firebender', 'forgecode', 'deepagents', 'mistral_vibe', 'mux', 'reasonix', 'kode', 'neovate', 'command_code', 'terramind', 'rovodev', 'cortex_code', 'codebuddy', 'aider_desk', 'windsurf', 'zed', 'custom')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source_count, 35);
        assert_eq!(catalog_count, 35);
    }

    #[test]
    fn archive_import_allows_multiple_capture_sources_for_same_provider_session() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let external_session_id = "provider-session-1";
        let first_source = provider_archive_source(
            "018f45d0-0000-7000-8000-000000080001",
            external_session_id,
            "/tmp/provider/first.jsonl",
        );
        let second_source = provider_archive_source(
            "018f45d0-0000-7000-8000-000000080002",
            external_session_id,
            "/tmp/provider/second.jsonl",
        );

        store
            .import_archive(&archive_with_source(first_source.clone()), false)
            .unwrap();
        store
            .import_archive(&archive_with_source(second_source.clone()), false)
            .unwrap();

        let sources = store.list_capture_sources().unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(
            sources
                .iter()
                .map(|source| source.id)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([first_source.id, second_source.id])
        );
        assert!(sources
            .iter()
            .all(|source| source.descriptor.external_session_id.as_deref()
                == Some(external_session_id)));
    }

    fn archive_with_source(source: CaptureSource) -> SessionHistoryArchive {
        SessionHistoryArchive {
            capture_sources: vec![source],
            ..SessionHistoryArchive::default()
        }
    }

    fn provider_archive_source(
        id: &str,
        external_session_id: &str,
        raw_source_path: &str,
    ) -> CaptureSource {
        CaptureSource {
            id: Uuid::parse_str(id).unwrap(),
            descriptor: CaptureSourceDescriptor {
                kind: ctx_history_core::CaptureSourceKind::ProviderImport,
                provider: CaptureProvider::Claude,
                machine_id: "test-machine".to_owned(),
                process_id: None,
                cwd: Some("/repo".to_owned()),
                raw_source_path: Some(raw_source_path.to_owned()),
                external_session_id: Some(external_session_id.to_owned()),
            },
            started_at: fixed_time(),
            ended_at: None,
            sync: sync_metadata(),
        }
    }

    #[test]
    fn schema_v16_rebuilds_provider_checks_with_referenced_sources_and_indexes() {
        let temp = tempdir();
        let path = temp.path().join("work.sqlite");
        let source_id = new_id();
        let session_id;
        let event_id;
        {
            let store = Store::open(&path).unwrap();
            let source = CaptureSource {
                id: source_id,
                descriptor: CaptureSourceDescriptor {
                    kind: ctx_history_core::CaptureSourceKind::ProviderImport,
                    provider: CaptureProvider::Codex,
                    machine_id: "test-machine".to_owned(),
                    process_id: None,
                    cwd: Some("/repo".to_owned()),
                    raw_source_path: Some("/home/user/.codex/sessions/session.jsonl".to_owned()),
                    external_session_id: Some("codex-session-1".to_owned()),
                },
                started_at: fixed_time(),
                ended_at: None,
                sync: sync_metadata(),
            };
            store.upsert_capture_source(&source).unwrap();

            let mut session = imported_session("codex-session-1");
            session.capture_source_id = Some(source_id);
            session_id = session.id;
            store.upsert_session(&session).unwrap();

            let event = Event {
                id: new_id(),
                seq: 0,
                history_record_id: None,
                session_id: Some(session_id),
                run_id: None,
                event_type: EventType::Message,
                role: Some(EventRole::User),
                occurred_at: fixed_time(),
                capture_source_id: Some(source_id),
                payload: serde_json::json!({"text": "migration source reference"}),
                payload_blob_id: None,
                dedupe_key: None,
                redaction_state: RedactionState::LocalPreview,
                sync: sync_metadata(),
            };
            event_id = event.id;
            store.upsert_event(&event).unwrap();
            store
                .conn
                .execute_batch("PRAGMA user_version = 14;")
                .unwrap();
        }

        let store = Store::open(&path).unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let source_refs: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sessions s JOIN events e ON e.session_id = s.id \
                 WHERE s.id = ?1 AND e.id = ?2 AND s.capture_source_id = ?3 AND e.capture_source_id = ?3",
                params![session_id.to_string(), event_id.to_string(), source_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source_refs, 1);
        for index in [
            "idx_capture_sources_external_session_id",
            "idx_catalog_sessions_provider_source_root_import",
            "idx_source_import_files_provider_source_root_import",
        ] {
            let exists: i64 = store
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                    [index],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(exists, 1, "missing rebuilt index {index}");
        }
    }

    #[test]
    fn schema_v17_adds_jsonl_longtail_provider_checks() {
        let temp = tempdir();
        let path = temp.path().join("work.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            let legacy_sql = CREATE_TABLES_SQL.replace(
                ", 'qwen_code', 'kimi_code_cli', 'autohand_code', 'iflow_cli', 'jazz', 'forgecode', 'deepagents', 'mistral_vibe'",
                "",
            );
            conn.execute_batch(&legacy_sql).unwrap();
            conn.execute_batch(INDEXES_SQL).unwrap();
            conn.execute_batch("PRAGMA user_version = 16;").unwrap();
        }

        let store = Store::open(&path).unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        for (provider, source_format) in [
            ("qwen_code", "qwen_code_chat_jsonl"),
            ("kimi_code_cli", "kimi_code_cli_wire_jsonl"),
            ("autohand_code", "autohand_code_sessions_jsonl"),
        ] {
            store
                .conn
                .execute(
                    r#"
                    INSERT INTO capture_sources
                    (id, kind, provider, machine_id, started_at_ms, fidelity)
                    VALUES (?1, 'provider_import', ?2, 'test-machine', 0, 'imported')
                    "#,
                    params![new_id().to_string(), provider],
                )
                .unwrap();
            store
                .conn
                .execute(
                    r#"
                    INSERT INTO catalog_sessions
                    (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
                    VALUES (?1, ?2, ?3, '/tmp/provider', 'primary', 1, 0, 0)
                    "#,
                    params![format!("/tmp/provider/{provider}.jsonl"), provider, source_format],
                )
                .unwrap();
            store
                .conn
                .execute(
                    r#"
                    INSERT INTO source_import_files
                    (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
                    VALUES (?1, ?2, '/tmp/provider', ?3, 1, 0, 0)
                    "#,
                    params![
                        provider,
                        source_format,
                        format!("/tmp/provider/{provider}.jsonl")
                    ],
                )
                .unwrap();
        }
    }

    #[test]
    fn schema_v18_adds_codebuddy_provider_checks() {
        let temp = tempdir();
        let path = temp.path().join("work.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            let legacy_sql = CREATE_TABLES_SQL.replace(", 'codebuddy'", "");
            conn.execute_batch(&legacy_sql).unwrap();
            conn.execute_batch(INDEXES_SQL).unwrap();
            conn.execute_batch("PRAGMA user_version = 17;").unwrap();
        }

        let store = Store::open(&path).unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        let provider = "codebuddy";
        let source_format = "codebuddy_history_json";
        store
            .conn
            .execute(
                r#"
                INSERT INTO capture_sources
                (id, kind, provider, machine_id, started_at_ms, fidelity)
                VALUES (?1, 'provider_import', ?2, 'test-machine', 0, 'imported')
                "#,
                params![new_id().to_string(), provider],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO catalog_sessions
                (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
                VALUES (?1, ?2, ?3, '/tmp/provider', 'primary', 1, 0, 0)
                "#,
                params![
                    format!("/tmp/provider/{provider}/session/index.json"),
                    provider,
                    source_format
                ],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO source_import_files
                (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
                VALUES (?1, ?2, '/tmp/provider', ?3, 1, 0, 0)
                "#,
                params![
                    provider,
                    source_format,
                    format!("/tmp/provider/{provider}/session/index.json")
                ],
            )
            .unwrap();
    }

    #[test]
    fn schema_v19_adds_zed_provider_checks() {
        let temp = tempdir();
        let path = temp.path().join("work.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            let legacy_sql = CREATE_TABLES_SQL.replace(", 'zed'", "");
            conn.execute_batch(&legacy_sql).unwrap();
            conn.execute_batch(INDEXES_SQL).unwrap();
            conn.execute_batch("PRAGMA user_version = 18;").unwrap();
        }

        let store = Store::open(&path).unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        store
            .conn
            .execute(
                r#"
                INSERT INTO capture_sources
                (id, kind, provider, machine_id, started_at_ms, fidelity)
                VALUES (?1, 'provider_import', 'zed', 'test-machine', 0, 'imported')
                "#,
                params![new_id().to_string()],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO catalog_sessions
                (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
                VALUES ('/tmp/zed/threads.db', 'zed', 'zed_threads_sqlite', '/tmp/zed/threads.db', 'primary', 1, 0, 0)
                "#,
                [],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO source_import_files
                (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
                VALUES ('zed', 'zed_threads_sqlite', '/tmp/zed/threads.db', '/tmp/zed/threads.db', 1, 0, 0)
                "#,
                [],
            )
            .unwrap();
    }

    #[test]
    fn schema_v20_adds_kiro_cli_provider_checks() {
        let temp = tempdir();
        let path = temp.path().join("work.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            let legacy_sql = CREATE_TABLES_SQL.replace(", 'kiro_cli'", "");
            conn.execute_batch(&legacy_sql).unwrap();
            conn.execute_batch(INDEXES_SQL).unwrap();
            conn.execute_batch("PRAGMA user_version = 19;").unwrap();
        }

        let store = Store::open(&path).unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        store
            .conn
            .execute(
                r#"
                INSERT INTO capture_sources
                (id, kind, provider, machine_id, started_at_ms, fidelity)
                VALUES (?1, 'provider_import', 'kiro_cli', 'test-machine', 0, 'imported')
                "#,
                params![new_id().to_string()],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO catalog_sessions
                (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
                VALUES ('/tmp/kiro/data.sqlite3', 'kiro_cli', 'kiro_cli_sqlite', '/tmp/kiro', 'primary', 1, 0, 0)
                "#,
                [],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO source_import_files
                (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
                VALUES ('kiro_cli', 'kiro_cli_sqlite', '/tmp/kiro', '/tmp/kiro/data.sqlite3', 1, 0, 0)
                "#,
                [],
            )
            .unwrap();
    }

    #[test]
    fn schema_v21_adds_iflow_provider_checks() {
        let temp = tempdir();
        let path = temp.path().join("work.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            let legacy_sql = CREATE_TABLES_SQL.replace(", 'iflow_cli'", "");
            conn.execute_batch(&legacy_sql).unwrap();
            conn.execute_batch(INDEXES_SQL).unwrap();
            conn.execute_batch("PRAGMA user_version = 20;").unwrap();
        }

        let store = Store::open(&path).unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        store
            .conn
            .execute(
                r#"
                INSERT INTO capture_sources
                (id, kind, provider, machine_id, started_at_ms, fidelity)
                VALUES (?1, 'provider_import', 'iflow_cli', 'test-machine', 0, 'imported')
                "#,
                params![new_id().to_string()],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO catalog_sessions
                (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
                VALUES ('/tmp/provider/iflow.jsonl', 'iflow_cli', 'iflow_cli_session_jsonl', '/tmp/provider', 'primary', 1, 0, 0)
                "#,
                [],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO source_import_files
                (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
                VALUES ('iflow_cli', 'iflow_cli_session_jsonl', '/tmp/provider', '/tmp/provider/iflow.jsonl', 1, 0, 0)
                "#,
                [],
            )
            .unwrap();
    }

    #[test]
    fn schema_v22_adds_forgecode_provider_checks() {
        let temp = tempdir();
        let path = temp.path().join("work.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            let legacy_sql = CREATE_TABLES_SQL.replace(", 'forgecode'", "");
            conn.execute_batch(&legacy_sql).unwrap();
            conn.execute_batch(INDEXES_SQL).unwrap();
            conn.execute_batch("PRAGMA user_version = 21;").unwrap();
        }

        let store = Store::open(&path).unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        store
            .conn
            .execute(
                r#"
                INSERT INTO capture_sources
                (id, kind, provider, machine_id, started_at_ms, fidelity)
                VALUES (?1, 'provider_import', 'forgecode', 'test-machine', 0, 'imported')
                "#,
                params![new_id().to_string()],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO catalog_sessions
                (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
                VALUES ('/tmp/forge/.forge.db', 'forgecode', 'forgecode_sqlite', '/tmp/forge', 'primary', 1, 0, 0)
                "#,
                [],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO source_import_files
                (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
                VALUES ('forgecode', 'forgecode_sqlite', '/tmp/forge', '/tmp/forge/.forge.db', 1, 0, 0)
                "#,
                [],
            )
            .unwrap();
    }

    #[test]
    fn schema_v23_adds_mistral_vibe_provider_checks() {
        let temp = tempdir();
        let path = temp.path().join("work.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            let legacy_sql = CREATE_TABLES_SQL.replace(", 'mistral_vibe'", "");
            conn.execute_batch(&legacy_sql).unwrap();
            conn.execute_batch(INDEXES_SQL).unwrap();
            conn.execute_batch("PRAGMA user_version = 22;").unwrap();
        }

        let store = Store::open(&path).unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        store
            .conn
            .execute(
                r#"
                INSERT INTO capture_sources
                (id, kind, provider, machine_id, started_at_ms, fidelity)
                VALUES (?1, 'provider_import', 'mistral_vibe', 'test-machine', 0, 'imported')
                "#,
                params![new_id().to_string()],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO catalog_sessions
                (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
                VALUES ('/tmp/vibe/messages.jsonl', 'mistral_vibe', 'mistral_vibe_session_jsonl', '/tmp/vibe', 'primary', 1, 0, 0)
                "#,
                [],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO source_import_files
                (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
                VALUES ('mistral_vibe', 'mistral_vibe_session_jsonl', '/tmp/vibe', '/tmp/vibe/messages.jsonl', 1, 0, 0)
                "#,
                [],
            )
            .unwrap();
    }

    #[test]
    fn schema_v24_adds_aider_desk_deepagents_mux_reasonix_kode_neovate_terramind_and_lingma_provider_checks(
    ) {
        let temp = tempdir();
        let path = temp.path().join("work.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            let legacy_sql = CREATE_TABLES_SQL
                .replace(", 'aider_desk'", "")
                .replace(", 'deepagents'", "")
                .replace(", 'mux'", "")
                .replace(", 'reasonix'", "")
                .replace(", 'kode'", "")
                .replace(", 'neovate'", "")
                .replace(", 'terramind'", "")
                .replace(", 'lingma'", "");
            conn.execute_batch(&legacy_sql).unwrap();
            conn.execute_batch(INDEXES_SQL).unwrap();
            conn.execute_batch("PRAGMA user_version = 23;").unwrap();
        }

        let store = Store::open(&path).unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        for provider in [
            "aider_desk",
            "deepagents",
            "mux",
            "reasonix",
            "kode",
            "neovate",
            "terramind",
            "lingma",
        ] {
            store
                .conn
                .execute(
                    r#"
                    INSERT INTO capture_sources
                    (id, kind, provider, machine_id, started_at_ms, fidelity)
                    VALUES (?1, 'provider_import', ?2, 'test-machine', 0, 'imported')
                    "#,
                    params![new_id().to_string(), provider],
                )
                .unwrap();
        }

        for (source_path, provider, source_format, source_root) in [
            (
                "/tmp/aider/.aider-desk/tasks/task-1/context.json",
                "aider_desk",
                "aider_desk_task_context_json",
                "/tmp/aider/.aider-desk/tasks",
            ),
            (
                "/tmp/deepagents/sessions.db",
                "deepagents",
                "deepagents_sessions_sqlite",
                "/tmp/deepagents",
            ),
            (
                "/tmp/mux/chat.jsonl",
                "mux",
                "mux_session_jsonl",
                "/tmp/mux",
            ),
            (
                "/tmp/reasonix/reasonix-session-1.jsonl",
                "reasonix",
                "reasonix_session_jsonl",
                "/tmp/reasonix",
            ),
            (
                "/tmp/kode/kode-session-1.jsonl",
                "kode",
                "kode_session_jsonl",
                "/tmp/kode",
            ),
            (
                "/tmp/neovate/neovate-session-1.jsonl",
                "neovate",
                "neovate_session_jsonl",
                "/tmp/neovate",
            ),
            (
                "/tmp/Nucleus/data/agents.db",
                "terramind",
                "terramind_agents_sqlite",
                "/tmp/Nucleus/data",
            ),
            (
                "/tmp/lingma/local.db",
                "lingma",
                "lingma_sqlite",
                "/tmp/lingma/local.db",
            ),
        ] {
            store
                .conn
                .execute(
                    r#"
                    INSERT INTO catalog_sessions
                    (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
                    VALUES (?1, ?2, ?3, ?4, 'primary', 1, 0, 0)
                    "#,
                    params![source_path, provider, source_format, source_root],
                )
                .unwrap();
        }

        for (provider, source_format, source_root, source_path) in [
            (
                "aider_desk",
                "aider_desk_task_context_json",
                "/tmp/aider/.aider-desk/tasks",
                "/tmp/aider/.aider-desk/tasks/task-1/context.json",
            ),
            (
                "deepagents",
                "deepagents_sessions_sqlite",
                "/tmp/deepagents",
                "/tmp/deepagents/sessions.db",
            ),
            (
                "mux",
                "mux_session_jsonl",
                "/tmp/mux",
                "/tmp/mux/chat.jsonl",
            ),
            (
                "reasonix",
                "reasonix_session_jsonl",
                "/tmp/reasonix",
                "/tmp/reasonix/reasonix-session-1.jsonl",
            ),
            (
                "kode",
                "kode_session_jsonl",
                "/tmp/kode",
                "/tmp/kode/kode-session-1.jsonl",
            ),
            (
                "neovate",
                "neovate_session_jsonl",
                "/tmp/neovate",
                "/tmp/neovate/neovate-session-1.jsonl",
            ),
            (
                "terramind",
                "terramind_agents_sqlite",
                "/tmp/Nucleus/data",
                "/tmp/Nucleus/data/agents.db",
            ),
            (
                "lingma",
                "lingma_sqlite",
                "/tmp/lingma/local.db",
                "/tmp/lingma/local.db",
            ),
        ] {
            store
                .conn
                .execute(
                    r#"
                    INSERT INTO source_import_files
                    (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
                    VALUES (?1, ?2, ?3, ?4, 1, 0, 0)
                    "#,
                    params![provider, source_format, source_root, source_path],
                )
                .unwrap();
        }
    }

    #[test]
    fn schema_v25_adds_command_code_rovodev_and_cortex_code_provider_checks() {
        let temp = tempdir();
        let path = temp.path().join("work.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            let legacy_sql = CREATE_TABLES_SQL
                .replace(", 'command_code'", "")
                .replace(", 'rovodev'", "")
                .replace(", 'cortex_code'", "");
            conn.execute_batch(&legacy_sql).unwrap();
            conn.execute_batch(INDEXES_SQL).unwrap();
            conn.execute_batch("PRAGMA user_version = 24;").unwrap();
        }

        let store = Store::open(&path).unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        for provider in ["command_code", "rovodev", "cortex_code"] {
            store
                .conn
                .execute(
                    r#"
                    INSERT INTO capture_sources
                    (id, kind, provider, machine_id, started_at_ms, fidelity)
                    VALUES (?1, 'provider_import', ?2, 'test-machine', 0, 'imported')
                    "#,
                    params![new_id().to_string(), provider],
                )
                .unwrap();
        }

        for (source_path, provider, source_format, source_root) in [
            (
                "/tmp/commandcode/projects/workspace/session.jsonl",
                "command_code",
                "command_code_session_jsonl",
                "/tmp/commandcode/projects",
            ),
            (
                "/tmp/rovodev/sessions/session/session_context.json",
                "rovodev",
                "rovodev_session_json",
                "/tmp/rovodev/sessions",
            ),
            (
                "/tmp/snowflake/cortex/conversations/session.history.jsonl",
                "cortex_code",
                "cortex_code_session_json",
                "/tmp/snowflake/cortex/conversations",
            ),
        ] {
            store
                .conn
                .execute(
                    r#"
                    INSERT INTO catalog_sessions
                    (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
                    VALUES (?1, ?2, ?3, ?4, 'primary', 1, 0, 0)
                    "#,
                    params![source_path, provider, source_format, source_root],
                )
                .unwrap();
            store
                .conn
                .execute(
                    r#"
                    INSERT INTO source_import_files
                    (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
                    VALUES (?1, ?2, ?3, ?4, 1, 0, 0)
                    "#,
                    params![provider, source_format, source_root, source_path],
                )
                .unwrap();
        }
    }

    #[test]
    fn schema_v26_adds_jazz_provider_checks() {
        let temp = tempdir();
        let path = temp.path().join("work.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            let legacy_sql = CREATE_TABLES_SQL.replace(", 'jazz'", "");
            conn.execute_batch(&legacy_sql).unwrap();
            conn.execute_batch(INDEXES_SQL).unwrap();
            conn.execute_batch("PRAGMA user_version = 25;").unwrap();
        }

        let store = Store::open(&path).unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        store
            .conn
            .execute(
                r#"
                INSERT INTO capture_sources
                (id, kind, provider, machine_id, started_at_ms, fidelity)
                VALUES (?1, 'provider_import', 'jazz', 'test-machine', 0, 'imported')
                "#,
                params![new_id().to_string()],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO catalog_sessions
                (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
                VALUES ('/tmp/jazz/history/jazz-agent.json', 'jazz', 'jazz_history_json', '/tmp/jazz/history', 'primary', 1, 0, 0)
                "#,
                [],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO source_import_files
                (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
                VALUES ('jazz', 'jazz_history_json', '/tmp/jazz/history', '/tmp/jazz/history/jazz-agent.json', 1, 0, 0)
                "#,
                [],
            )
            .unwrap();
    }

    #[test]
    fn schema_v27_adds_windsurf_provider_checks() {
        assert_provider_migration_accepts(
            26,
            "windsurf",
            "windsurf_cascade_hook_transcript_jsonl",
            "/tmp/windsurf/transcripts",
            "/tmp/windsurf/transcripts/trajectory.jsonl",
        );
    }

    #[test]
    fn schema_v28_adds_pochi_provider_checks() {
        assert_provider_migration_accepts(
            27,
            "pochi",
            "pochi_livestore_state_sqlite",
            "/tmp/pochi/storage/store",
            "/tmp/pochi/storage/store/state@6.db",
        );
    }

    #[test]
    fn schema_v29_adds_openloaf_provider_checks() {
        assert_provider_migration_accepts(
            28,
            "openloaf",
            "openloaf_chat_jsonl",
            "/tmp/openloaf/chat-history",
            "/tmp/openloaf/chat-history/session/messages.jsonl",
        );
    }

    #[test]
    fn schema_v30_adds_auggie_provider_checks() {
        assert_provider_migration_accepts(
            29,
            "auggie",
            "auggie_session_json",
            "/tmp/augment/sessions",
            "/tmp/augment/sessions/session.json",
        );
    }

    #[test]
    fn schema_v31_adds_firebender_provider_checks() {
        assert_provider_migration_accepts(
            30,
            "firebender",
            "firebender_chat_history_sqlite",
            "/tmp/project/.idea/firebender/chat_history.db",
            "/tmp/project/.idea/firebender/chat_history.db",
        );
    }

    fn assert_provider_migration_accepts(
        legacy_version: i64,
        provider: &str,
        source_format: &str,
        source_root: &str,
        source_path: &str,
    ) {
        let temp = tempdir();
        let path = temp.path().join("work.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            let legacy_sql = CREATE_TABLES_SQL.replace(&format!(", '{provider}'"), "");
            conn.execute_batch(&legacy_sql).unwrap();
            conn.execute_batch(INDEXES_SQL).unwrap();
            conn.execute_batch(&format!("PRAGMA user_version = {legacy_version};"))
                .unwrap();
        }

        let store = Store::open(&path).unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        store
            .conn
            .execute(
                r#"
                INSERT INTO capture_sources
                (id, kind, provider, machine_id, started_at_ms, fidelity)
                VALUES (?1, 'provider_import', ?2, 'test-machine', 0, 'imported')
                "#,
                params![new_id().to_string(), provider],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO catalog_sessions
                (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
                VALUES (?1, ?2, ?3, ?4, 'primary', 1, 0, 0)
                "#,
                params![source_path, provider, source_format, source_root],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO source_import_files
                (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
                VALUES (?1, ?2, ?3, ?4, 1, 0, 0)
                "#,
                params![provider, source_format, source_root, source_path],
            )
            .unwrap();
    }
}
