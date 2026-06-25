use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

#[cfg(feature = "legacy-pr-evidence")]
use std::collections::HashSet;
#[cfg(feature = "legacy-pr-evidence")]
use std::path::Component;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;
use work_record_core::{
    new_id, redact_preview, redact_share_safe_preview, AgentType, Artifact, ArtifactKind,
    CaptureProvider, CaptureSource, CaptureSourceDescriptor, EntityTimestamps, Event, EventRole,
    EventType, Fidelity, FileTouched, RedactionState, Run, RunStatus, RunType, Session,
    SessionEdge, SessionStatus, Summary, SyncCursor, SyncMetadata, SyncState, VcsChange,
    VcsWorkspace, Visibility, WorkRecord, WorkRecordArchive, WorkRecordLink,
};
#[cfg(feature = "legacy-pr-evidence")]
use work_record_core::{
    Evidence, EvidenceFreshness, EvidenceKind, EvidenceMetadata, EvidenceStatus, PullRequest,
    WorkRecordArchiveArtifact,
};

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
    #[error("unsupported work record store schema version: {0}")]
    UnsupportedSchemaVersion(i64),
    #[error("unsupported work record archive version: {0}")]
    UnsupportedArchiveVersion(u32),
    #[error("archive conflicts with existing {kind}: {id}")]
    ImportConflict { kind: &'static str, id: Uuid },
    #[cfg(feature = "legacy-pr-evidence")]
    #[error("evidence must be attached to a work record")]
    EvidenceMissingWorkRecord,
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
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[cfg(feature = "legacy-pr-evidence")]
pub fn classify_evidence_freshness(
    metadata: &EvidenceMetadata,
    current_head_sha: Option<&str>,
    current_tree_hash: Option<&str>,
    dirty: bool,
) -> EvidenceFreshness {
    if metadata.vcs_change_id.is_none()
        && metadata.observed_head_sha.is_none()
        && metadata.observed_tree_hash.is_none()
    {
        return EvidenceFreshness::Unbound;
    }
    if metadata
        .observed_head_sha
        .as_deref()
        .zip(current_head_sha)
        .is_some_and(|(observed, current)| observed != current)
        || metadata
            .observed_tree_hash
            .as_deref()
            .zip(current_tree_hash)
            .is_some_and(|(observed, current)| observed != current)
    {
        return EvidenceFreshness::Stale;
    }
    if dirty || metadata.observed_tree_hash.is_none() || current_tree_hash.is_none() {
        return EvidenceFreshness::ProbablyFresh;
    }
    EvidenceFreshness::Fresh
}

const SCHEMA_VERSION: i64 = 7;
const BUSY_TIMEOUT: Duration = Duration::from_millis(5_000);
const OBJECTS_DIR: &str = "objects";
const SPOOL_DIR: &str = "spool";
const LEGACY_WORK_RECORD_DIR: &str = "work-record";
const LEGACY_BLOBS_DIR: &str = "blobs";
const LEGACY_INBOX_DIR: &str = "inbox";

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
    pub event_count: u64,
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
    pub work_record_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub seq: u64,
    pub event_type: EventType,
    pub role: Option<EventRole>,
    pub occurred_at: DateTime<Utc>,
    pub preview: String,
    pub score: f64,
    pub provider: Option<CaptureProvider>,
    pub session_external_session_id: Option<String>,
    pub agent_type: Option<AgentType>,
    pub session_is_primary: Option<bool>,
    pub cwd: Option<String>,
    pub raw_source_path: Option<String>,
    pub cursor: Option<String>,
    pub record_title: Option<String>,
    pub record_kind: Option<String>,
    pub record_workspace: Option<String>,
}

const WORK_RECORD_COLUMNS: &[ColumnSpec] = &[
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
];

#[cfg(feature = "legacy-pr-evidence")]
const EVIDENCE_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec {
        name: "work_record_id",
        definition: "work_record_id TEXT REFERENCES work_records(id)",
    },
    ColumnSpec {
        name: "vcs_change_id",
        definition: "vcs_change_id TEXT REFERENCES vcs_changes(id)",
    },
    ColumnSpec {
        name: "kind",
        definition: "kind TEXT NOT NULL DEFAULT 'manual' CHECK (kind IN ('test', 'lint', 'build', 'typecheck', 'screenshot', 'review', 'ci', 'manual'))",
    },
    ColumnSpec {
        name: "status",
        definition: "status TEXT NOT NULL DEFAULT 'unknown' CHECK (status IN ('passed', 'failed', 'skipped', 'stale', 'unknown'))",
    },
    ColumnSpec {
        name: "freshness",
        definition: "freshness TEXT NOT NULL DEFAULT 'unbound' CHECK (freshness IN ('fresh', 'probably_fresh', 'stale', 'unbound', 'inferred'))",
    },
    ColumnSpec {
        name: "command_run_id",
        definition: "command_run_id TEXT REFERENCES runs(id)",
    },
    ColumnSpec {
        name: "artifact_id",
        definition: "artifact_id TEXT REFERENCES artifacts(id)",
    },
    ColumnSpec {
        name: "observed_tree_hash",
        definition: "observed_tree_hash TEXT",
    },
    ColumnSpec {
        name: "observed_head_sha",
        definition: "observed_head_sha TEXT",
    },
    ColumnSpec {
        name: "started_at_ms",
        definition: "started_at_ms INTEGER",
    },
    ColumnSpec {
        name: "ended_at_ms",
        definition: "ended_at_ms INTEGER",
    },
    ColumnSpec {
        name: "stale_reason",
        definition: "stale_reason TEXT",
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

const CREATE_TABLES_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS capture_sources (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('provider_import', 'provider_hook', 'direct_cli', 'manual')),
    provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'opencode', 'antigravity', 'gemini', 'cursor', 'copilot_cli', 'factory_ai_droid', 'amp', 'shell', 'git', 'jj', 'gh', 'unknown')),
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
    provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'opencode', 'antigravity', 'gemini', 'cursor', 'copilot_cli', 'factory_ai_droid', 'amp', 'shell', 'git', 'jj', 'gh', 'unknown')),
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
    metadata_json TEXT NOT NULL DEFAULT '{}'
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

CREATE TABLE IF NOT EXISTS work_records (
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
    work_record_id TEXT REFERENCES work_records(id),
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
    work_record_id TEXT REFERENCES work_records(id),
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
    work_record_id TEXT REFERENCES work_records(id),
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

CREATE TABLE IF NOT EXISTS work_record_links (
    id TEXT PRIMARY KEY NOT NULL,
    work_record_id TEXT NOT NULL REFERENCES work_records(id),
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
    UNIQUE(work_record_id, target_type, target_id, link_type)
);

CREATE TABLE IF NOT EXISTS summaries (
    id TEXT PRIMARY KEY NOT NULL,
    work_record_id TEXT REFERENCES work_records(id),
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
    work_record_id TEXT REFERENCES work_records(id),
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

CREATE TABLE IF NOT EXISTS work_record_tags (
    work_record_id TEXT NOT NULL REFERENCES work_records(id),
    tag_id TEXT NOT NULL REFERENCES tags(id),
    source_id TEXT REFERENCES capture_sources(id),
    confidence TEXT NOT NULL DEFAULT 'unknown' CHECK (confidence IN ('explicit', 'high', 'medium', 'low', 'unknown')),
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (work_record_id, tag_id)
);

CREATE TABLE IF NOT EXISTS record_edges (
    id TEXT PRIMARY KEY NOT NULL,
    from_record_id TEXT NOT NULL REFERENCES work_records(id),
    to_record_id TEXT NOT NULL REFERENCES work_records(id),
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
CREATE INDEX IF NOT EXISTS idx_sessions_provider_external_session_id ON sessions(provider, external_session_id);

CREATE INDEX IF NOT EXISTS idx_work_records_primary_vcs_workspace_id ON work_records(primary_vcs_workspace_id);
CREATE INDEX IF NOT EXISTS idx_work_records_source_id ON work_records(source_id);
CREATE INDEX IF NOT EXISTS idx_work_records_last_activity_at_ms ON work_records(last_activity_at_ms);
CREATE INDEX IF NOT EXISTS idx_work_records_created_at ON work_records(created_at DESC);

CREATE INDEX IF NOT EXISTS idx_sessions_work_record_id ON sessions(work_record_id);
CREATE INDEX IF NOT EXISTS idx_sessions_parent_session_id ON sessions(parent_session_id);
CREATE INDEX IF NOT EXISTS idx_sessions_root_session_id ON sessions(root_session_id);
CREATE INDEX IF NOT EXISTS idx_sessions_capture_source_id ON sessions(capture_source_id);
CREATE INDEX IF NOT EXISTS idx_sessions_transcript_blob_id ON sessions(transcript_blob_id);

CREATE INDEX IF NOT EXISTS idx_session_edges_from_session_id ON session_edges(from_session_id);
CREATE INDEX IF NOT EXISTS idx_session_edges_to_session_id ON session_edges(to_session_id);
CREATE INDEX IF NOT EXISTS idx_session_edges_source_id ON session_edges(source_id);

CREATE INDEX IF NOT EXISTS idx_runs_work_record_started_at_ms ON runs(work_record_id, started_at_ms);
CREATE INDEX IF NOT EXISTS idx_runs_work_record_id ON runs(work_record_id);
CREATE INDEX IF NOT EXISTS idx_runs_session_id ON runs(session_id);
CREATE INDEX IF NOT EXISTS idx_runs_input_blob_id ON runs(input_blob_id);
CREATE INDEX IF NOT EXISTS idx_runs_output_blob_id ON runs(output_blob_id);
CREATE INDEX IF NOT EXISTS idx_runs_source_id ON runs(source_id);

CREATE INDEX IF NOT EXISTS idx_events_seq ON events(seq);
CREATE INDEX IF NOT EXISTS idx_events_work_record_occurred_at_ms ON events(work_record_id, occurred_at_ms);
CREATE INDEX IF NOT EXISTS idx_events_session_occurred_at_ms ON events(session_id, occurred_at_ms);
CREATE INDEX IF NOT EXISTS idx_events_work_record_id ON events(work_record_id);
CREATE INDEX IF NOT EXISTS idx_events_session_id ON events(session_id);
CREATE INDEX IF NOT EXISTS idx_events_run_id ON events(run_id);
CREATE INDEX IF NOT EXISTS idx_events_capture_source_id ON events(capture_source_id);
CREATE INDEX IF NOT EXISTS idx_events_payload_blob_id ON events(payload_blob_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_events_dedupe_key ON events(dedupe_key) WHERE dedupe_key IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_vcs_workspaces_kind_repo_fingerprint ON vcs_workspaces(kind, repo_fingerprint);
CREATE INDEX IF NOT EXISTS idx_vcs_workspaces_source_id ON vcs_workspaces(source_id);

CREATE INDEX IF NOT EXISTS idx_vcs_changes_vcs_workspace_id ON vcs_changes(vcs_workspace_id);
CREATE INDEX IF NOT EXISTS idx_vcs_changes_source_id ON vcs_changes(source_id);

CREATE INDEX IF NOT EXISTS idx_work_record_links_work_record_id ON work_record_links(work_record_id);
CREATE INDEX IF NOT EXISTS idx_work_record_links_source_id ON work_record_links(source_id);

CREATE INDEX IF NOT EXISTS idx_artifacts_source_id ON artifacts(source_id);

CREATE INDEX IF NOT EXISTS idx_summaries_work_record_id ON summaries(work_record_id);
CREATE INDEX IF NOT EXISTS idx_summaries_session_id ON summaries(session_id);
CREATE INDEX IF NOT EXISTS idx_summaries_source_id ON summaries(source_id);

CREATE INDEX IF NOT EXISTS idx_files_touched_work_record_id ON files_touched(work_record_id);
CREATE INDEX IF NOT EXISTS idx_files_touched_run_id ON files_touched(run_id);
CREATE INDEX IF NOT EXISTS idx_files_touched_event_id ON files_touched(event_id);
CREATE INDEX IF NOT EXISTS idx_files_touched_vcs_workspace_id ON files_touched(vcs_workspace_id);
CREATE INDEX IF NOT EXISTS idx_files_touched_source_id ON files_touched(source_id);

CREATE INDEX IF NOT EXISTS idx_work_record_tags_tag_id ON work_record_tags(tag_id);
CREATE INDEX IF NOT EXISTS idx_work_record_tags_source_id ON work_record_tags(source_id);

CREATE INDEX IF NOT EXISTS idx_record_edges_from_record_id ON record_edges(from_record_id);
CREATE INDEX IF NOT EXISTS idx_record_edges_to_record_id ON record_edges(to_record_id);
CREATE INDEX IF NOT EXISTS idx_record_edges_source_id ON record_edges(source_id);

CREATE INDEX IF NOT EXISTS idx_sync_outbox_sync_state_updated_at_ms ON sync_outbox(sync_state, updated_at_ms);
CREATE INDEX IF NOT EXISTS idx_local_workspaces_device_id ON local_workspaces(device_id);
CREATE INDEX IF NOT EXISTS idx_local_workspaces_vcs_workspace_id ON local_workspaces(vcs_workspace_id);
CREATE INDEX IF NOT EXISTS idx_audit_log_source_id ON audit_log(source_id);
"#;

const FTS_TABLES_SQL: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS work_record_search USING fts5(
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
    work_record_id UNINDEXED,
    session_id UNINDEXED,
    role UNINDEXED,
    safe_preview_text,
    rank_bucket UNINDEXED
);

CREATE VIRTUAL TABLE IF NOT EXISTS artifact_search USING fts5(
    artifact_id UNINDEXED,
    work_record_id UNINDEXED,
    safe_preview_text
);
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

    pub fn open_with_busy_timeout(path: impl AsRef<Path>, busy_timeout: Duration) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut migrated_legacy_layout = false;
        if let Some(parent) = path.parent() {
            migrated_legacy_layout = migrate_legacy_work_record_layout(parent)?;
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
        create_fts_tables_if_supported(&self.conn)?;
        Ok(())
    }

    pub fn schema(&self) -> Result<String> {
        let mut stmt = self.conn.prepare(
            "SELECT sql FROM sqlite_master
             WHERE type IN ('table', 'index') AND sql IS NOT NULL
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
        for table in ["work_record_search", "event_search", "artifact_search"] {
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
                indexed_event_count = ?7
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
                capped_i64(update.event_count),
                CatalogIndexedStatus::Indexed.as_str(),
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
                id, work_record_id, parent_session_id, root_session_id, capture_source_id,
                provider, external_session_id, external_agent_id, agent_type, role_hint,
                is_primary, status, fidelity, transcript_blob_id, started_at_ms, ended_at_ms,
                created_at_ms, updated_at_ms, visibility, sync_state, sync_version,
                deleted_at_ms, metadata_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
            ON CONFLICT(id) DO UPDATE SET
                work_record_id = excluded.work_record_id,
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
                optional_uuid_string(session.work_record_id),
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

    pub fn sessions_for_record(&self, record_id: Uuid) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            session_select_sql("WHERE work_record_id = ?1 ORDER BY started_at_ms, id").as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], session_from_row)?;
        collect_rows(rows)
    }

    pub fn assign_session_to_record(&self, session_id: Uuid, record_id: Uuid) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET work_record_id = ?1 WHERE id = ?2",
            params![record_id.to_string(), session_id.to_string()],
        )?;
        self.conn.execute(
            "UPDATE events SET work_record_id = ?1 WHERE session_id = ?2",
            params![record_id.to_string(), session_id.to_string()],
        )?;
        self.conn.execute(
            "UPDATE runs SET work_record_id = ?1 WHERE session_id = ?2",
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
            (id, work_record_id, session_id, run_type, status, started_at_ms, ended_at_ms, exit_code, cwd, command_preview, input_blob_id, output_blob_id, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
            ON CONFLICT(id) DO UPDATE SET
                work_record_id = excluded.work_record_id,
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
                optional_uuid_string(run.work_record_id),
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
                (id, work_record_id, session_id, run_type, status, started_at_ms, ended_at_ms, exit_code, cwd, command_preview, input_blob_id, output_blob_id, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
                "#,
            )?
            .execute(params![
                run.id.to_string(),
                optional_uuid_string(run.work_record_id),
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
                WHERE work_record_id = ?1
                   OR session_id IN (SELECT id FROM sessions WHERE work_record_id = ?1)
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
            (id, seq, work_record_id, session_id, run_id, event_type, role, occurred_at_ms, capture_source_id, payload_json, payload_blob_id, dedupe_key, visibility, redaction_state, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
            ON CONFLICT(id) DO UPDATE SET
                seq = excluded.seq,
                work_record_id = excluded.work_record_id,
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
                optional_uuid_string(event.work_record_id),
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
                (id, seq, work_record_id, session_id, run_id, event_type, role, occurred_at_ms, capture_source_id, payload_json, payload_blob_id, dedupe_key, visibility, redaction_state, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
                "#,
            )?
            .execute(params![
                event.id.to_string(),
                event.seq as i64,
                optional_uuid_string(event.work_record_id),
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

    pub fn events_for_session(&self, session_id: Uuid) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            event_select_sql("WHERE session_id = ?1 ORDER BY seq, occurred_at_ms").as_str(),
        )?;
        let rows = stmt.query_map(params![session_id.to_string()], event_from_row)?;
        collect_rows(rows)
    }

    pub fn events_for_record(&self, record_id: Uuid) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            event_select_sql(
                r#"
                WHERE work_record_id = ?1
                   OR session_id IN (SELECT id FROM sessions WHERE work_record_id = ?1)
                   OR run_id IN (
                        SELECT id FROM runs
                        WHERE work_record_id = ?1
                           OR session_id IN (SELECT id FROM sessions WHERE work_record_id = ?1)
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
        let now = Utc::now();
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
        let display_root = redacted_root_label(root);
        let now = Utc::now();
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

    #[cfg(feature = "legacy-pr-evidence")]
    pub fn upsert_pull_request(&self, pr: &PullRequest) -> Result<Uuid> {
        self.conn.execute(
            r#"
            INSERT INTO pull_requests
            (id, vcs_workspace_id, provider, url, number, owner, repo, title, state, head_ref, base_ref, head_sha, confidence, link_source, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
            ON CONFLICT DO UPDATE SET
                vcs_workspace_id = COALESCE(excluded.vcs_workspace_id, pull_requests.vcs_workspace_id),
                url = excluded.url,
                title = COALESCE(excluded.title, pull_requests.title),
                state = COALESCE(excluded.state, pull_requests.state),
                head_ref = COALESCE(excluded.head_ref, pull_requests.head_ref),
                base_ref = COALESCE(excluded.base_ref, pull_requests.base_ref),
                head_sha = COALESCE(excluded.head_sha, pull_requests.head_sha),
                confidence = excluded.confidence,
                link_source = excluded.link_source,
                updated_at_ms = excluded.updated_at_ms,
                source_id = COALESCE(excluded.source_id, pull_requests.source_id),
                visibility = excluded.visibility,
                fidelity = excluded.fidelity,
                sync_state = excluded.sync_state,
                sync_version = excluded.sync_version,
                deleted_at_ms = excluded.deleted_at_ms,
                metadata_json = excluded.metadata_json
            "#,
            params![
                pr.id.to_string(),
                optional_uuid_string(pr.vcs_workspace_id),
                pr.provider.as_str(),
                pr.url.as_str(),
                pr.number.map(|n| n as i64),
                pr.owner.as_deref(),
                pr.repo.as_deref(),
                pr.title.as_deref(),
                pr.state.as_deref(),
                pr.head_ref.as_deref(),
                pr.base_ref.as_deref(),
                pr.head_sha.as_deref(),
                pr.confidence.as_str(),
                pr.link_source.as_str(),
                timestamp_ms(pr.timestamps.created_at),
                timestamp_ms(pr.timestamps.updated_at),
                optional_uuid_string(pr.source_id),
                pr.sync.visibility.as_str(),
                pr.sync.fidelity.as_str(),
                pr.sync.sync_state.as_str(),
                pr.sync.sync_version as i64,
                optional_timestamp_ms(pr.sync.deleted_at),
                serde_json::to_string(&pr.sync.metadata)?,
            ],
        )?;
        if let (Some(owner), Some(repo), Some(number)) = (&pr.owner, &pr.repo, pr.number) {
            return self
                .conn
                .query_row(
                    "SELECT id FROM pull_requests WHERE provider = ?1 AND owner = ?2 AND repo = ?3 AND number = ?4",
                    params![pr.provider.as_str(), owner, repo, number as i64],
                    |row| parse_uuid(row.get::<_, String>(0)?),
                )
                .map_err(StoreError::from);
        }
        Ok(pr.id)
    }

    #[cfg(feature = "legacy-pr-evidence")]
    fn list_pull_requests(&self) -> Result<Vec<PullRequest>> {
        let mut stmt = self
            .conn
            .prepare(pull_request_select_sql("ORDER BY updated_at_ms, id").as_str())?;
        let rows = stmt.query_map([], pull_request_from_row)?;
        collect_rows(rows)
    }

    pub fn upsert_summary(&self, summary: &Summary) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO summaries
            (id, work_record_id, session_id, kind, model_or_source, text, citations_json, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
            ON CONFLICT(id) DO UPDATE SET
                work_record_id = excluded.work_record_id,
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
                optional_uuid_string(summary.work_record_id),
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
            (id, work_record_id, run_id, event_id, vcs_workspace_id, path, change_kind, old_path, line_count_delta, confidence, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
            ON CONFLICT(id) DO UPDATE SET
                work_record_id = excluded.work_record_id,
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
                optional_uuid_string(file.work_record_id),
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
                    WHERE work_record_id = ?1 AND transcript_blob_id IS NOT NULL
                    UNION
                    SELECT input_blob_id
                    FROM runs
                    WHERE (work_record_id = ?1
                       OR session_id IN (SELECT id FROM sessions WHERE work_record_id = ?1))
                       AND input_blob_id IS NOT NULL
                    UNION
                    SELECT output_blob_id
                    FROM runs
                    WHERE (work_record_id = ?1
                       OR session_id IN (SELECT id FROM sessions WHERE work_record_id = ?1))
                       AND output_blob_id IS NOT NULL
                    UNION
                    SELECT payload_blob_id
                    FROM events
                    WHERE (work_record_id = ?1
                       OR session_id IN (SELECT id FROM sessions WHERE work_record_id = ?1))
                       AND payload_blob_id IS NOT NULL
                    UNION
                    SELECT target_id
                    FROM work_record_links
                    WHERE work_record_id = ?1 AND target_type = 'artifact'
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
                    FROM work_record_links
                    WHERE work_record_id = ?1 AND target_type = 'vcs_change'
                )
                ORDER BY updated_at_ms DESC, id
                "#,
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], vcs_change_from_row)?;
        collect_rows(rows)
    }

    #[cfg(feature = "legacy-pr-evidence")]
    pub fn pull_requests_for_record(&self, record_id: Uuid) -> Result<Vec<PullRequest>> {
        let mut stmt = self.conn.prepare(
            pull_request_select_sql(
                r#"
                WHERE id IN (
                    SELECT target_id
                    FROM work_record_links
                    WHERE work_record_id = ?1 AND target_type = 'pull_request'
                )
                ORDER BY updated_at_ms DESC, id
                "#,
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], pull_request_from_row)?;
        collect_rows(rows)
    }

    pub fn summaries_for_record(&self, record_id: Uuid) -> Result<Vec<Summary>> {
        let mut stmt = self.conn.prepare(
            summary_select_sql(
                r#"
                WHERE work_record_id = ?1
                   OR session_id IN (SELECT id FROM sessions WHERE work_record_id = ?1)
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
                WHERE work_record_id = ?1
                   OR run_id IN (
                        SELECT id FROM runs
                        WHERE work_record_id = ?1
                           OR session_id IN (SELECT id FROM sessions WHERE work_record_id = ?1)
                   )
                   OR event_id IN (
                        SELECT id FROM events
                        WHERE work_record_id = ?1
                           OR session_id IN (SELECT id FROM sessions WHERE work_record_id = ?1)
                   )
                ORDER BY updated_at_ms DESC, id
                "#,
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], file_touched_from_row)?;
        collect_rows(rows)
    }

    pub fn upsert_work_record_link(&self, link: &WorkRecordLink) -> Result<Uuid> {
        self.conn.execute(
            r#"
            INSERT INTO work_record_links
            (id, work_record_id, target_type, target_id, link_type, confidence, source_id, created_at_ms, updated_at_ms, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
            ON CONFLICT(work_record_id, target_type, target_id, link_type) DO UPDATE SET
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
                link.work_record_id.to_string(),
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
                "SELECT id FROM work_record_links WHERE work_record_id = ?1 AND target_type = ?2 AND target_id = ?3 AND link_type = ?4",
                params![
                    link.work_record_id.to_string(),
                    link.target_type.as_str(),
                    link.target_id.to_string(),
                    link.link_type.as_str()
                ],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .map_err(StoreError::from)
    }

    fn list_work_record_links(&self) -> Result<Vec<WorkRecordLink>> {
        let mut stmt = self
            .conn
            .prepare(work_record_link_select_sql("ORDER BY updated_at_ms, id").as_str())?;
        let rows = stmt.query_map([], work_record_link_from_row)?;
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

    pub fn insert_record(&self, record: &WorkRecord) -> Result<()> {
        let created_at_ms = timestamp_ms(record.created_at);
        let updated_at_ms = timestamp_ms(record.updated_at);
        self.conn.execute(
            r#"
            INSERT INTO work_records
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
        self.rebuild_search_projection()?;
        Ok(())
    }

    pub fn upsert_record(&self, record: &WorkRecord) -> Result<()> {
        self.upsert_record_row(record)?;
        self.rebuild_search_projection()?;
        Ok(())
    }

    pub fn upsert_records(&self, records: &[WorkRecord]) -> Result<()> {
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
        self.rebuild_search_projection()?;
        Ok(())
    }

    fn upsert_record_row(&self, record: &WorkRecord) -> Result<()> {
        let created_at_ms = timestamp_ms(record.created_at);
        let updated_at_ms = timestamp_ms(record.updated_at);
        self.conn.execute(
            r#"
            INSERT INTO work_records
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

    pub fn get_record(&self, id: Uuid) -> Result<WorkRecord> {
        self.conn
            .query_row(
                record_select_sql("WHERE id = ?1").as_str(),
                params![id.to_string()],
                record_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    pub fn list_records(&self, limit: usize) -> Result<Vec<WorkRecord>> {
        self.list_records_page(limit, 0)
    }

    pub fn list_records_page(&self, limit: usize, offset: usize) -> Result<Vec<WorkRecord>> {
        let mut stmt = self.conn.prepare(
            record_select_sql("ORDER BY created_at DESC, id LIMIT ?1 OFFSET ?2").as_str(),
        )?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], record_from_row)?;
        collect_rows(rows)
    }

    pub fn search_records(&self, query: &str, limit: usize) -> Result<Vec<WorkRecord>> {
        self.search_records_page(query, limit, 0)
    }

    pub fn search_records_page(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<WorkRecord>> {
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
    ) -> Result<Option<Vec<WorkRecord>>> {
        if !table_exists(&self.conn, "work_record_search")? {
            return Ok(None);
        }
        let Some(match_query) = fts_match_query(query) else {
            return Ok(Some(self.list_records_page(limit, offset)?));
        };
        let has_event_search = table_exists(&self.conn, "event_search")?;
        let has_artifact_search = table_exists(&self.conn, "artifact_search")?;
        let sql = if has_event_search && has_artifact_search {
            r#"
            WITH matches(record_id, score) AS (
                SELECT record_id, bm25(work_record_search)
                FROM work_record_search
                WHERE work_record_search MATCH ?1
                UNION ALL
                SELECT work_record_id, bm25(event_search)
                FROM event_search
                WHERE event_search MATCH ?1 AND work_record_id IS NOT NULL
                UNION ALL
                SELECT work_record_id, bm25(artifact_search)
                FROM artifact_search
                WHERE artifact_search MATCH ?1 AND work_record_id IS NOT NULL
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
            FROM work_record_search
            WHERE work_record_search MATCH ?1
            ORDER BY bm25(work_record_search), record_id
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

    pub fn max_events_per_work_record(&self) -> Result<i64> {
        let max_events = self.conn.query_row(
            r#"
            SELECT COALESCE(MAX(event_count), 0)
            FROM (
                SELECT COUNT(*) AS event_count
                FROM events
                GROUP BY work_record_id
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
                   COALESCE(e.work_record_id, event_search.work_record_id, s.work_record_id, rs.work_record_id),
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
            LEFT JOIN work_records wr ON wr.id = COALESCE(e.work_record_id, event_search.work_record_id, s.work_record_id, rs.work_record_id, r.work_record_id)
            WHERE event_search MATCH ?1
            ORDER BY bm25(event_search), e.occurred_at_ms DESC, e.seq DESC, event_search.event_id
            LIMIT ?2 OFFSET ?3
            "#,
        )?;
        let rows = stmt.query_map(
            params![match_query, limit.max(1) as i64, offset as i64],
            |row| {
                let payload_json = row.get::<_, String>(16)?;
                let source_metadata_json = row.get::<_, Option<String>>(17)?;
                Ok(EventSearchHit {
                    event_id: parse_uuid(row.get::<_, String>(0)?)?,
                    work_record_id: parse_optional_uuid(row.get(1)?)?,
                    session_id: parse_optional_uuid(row.get(2)?)?,
                    run_id: parse_optional_uuid(row.get(3)?)?,
                    seq: row.get::<_, i64>(4)? as u64,
                    event_type: parse_text_enum::<EventType>(row.get::<_, String>(5)?)?,
                    role: parse_optional_text_enum::<EventRole>(row.get(6)?)?,
                    occurred_at: ms_to_time(row.get(7)?)?,
                    preview: row.get(8)?,
                    score: row.get(9)?,
                    provider: parse_optional_text_enum::<CaptureProvider>(row.get(10)?)?,
                    session_external_session_id: row.get(11)?,
                    agent_type: parse_optional_text_enum::<AgentType>(row.get(12)?)?,
                    session_is_primary: row.get::<_, Option<i64>>(13)?.map(|value| value != 0),
                    cwd: row.get(14)?,
                    raw_source_path: row.get(15)?,
                    cursor: event_search_cursor(&payload_json, source_metadata_json.as_deref())?,
                    record_title: row.get(18)?,
                    record_kind: row.get(19)?,
                    record_workspace: row.get(20)?,
                })
            },
        )?;
        collect_rows(rows)
    }

    #[cfg(feature = "legacy-pr-evidence")]
    pub fn insert_evidence(&self, evidence: &Evidence) -> Result<()> {
        let work_record_id = evidence
            .record_id
            .ok_or(StoreError::EvidenceMissingWorkRecord)?;
        let started_at_ms = timestamp_ms(evidence.started_at);
        let ended_at_ms = started_at_ms.saturating_add(evidence.duration_ms);
        let status = evidence_status(evidence.exit_code);
        let stdout_artifact_id = self.store_output_artifact("stdout", &evidence.stdout)?;
        let stderr_artifact_id = self.store_output_artifact("stderr", &evidence.stderr)?;
        let artifact_id = stdout_artifact_id
            .as_deref()
            .or(stderr_artifact_id.as_deref())
            .map(str::to_owned);
        let stdout_preview = output_preview(&evidence.stdout);
        let stderr_preview = output_preview(&evidence.stderr);
        self.conn.execute(
            r#"
            INSERT INTO evidence
            (
                id, work_record_id, record_id, kind, status, freshness,
                command_run_id, artifact_id, started_at_ms, ended_at_ms,
                created_at_ms, updated_at_ms, command, exit_code, stdout,
                stderr, started_at, duration_ms
            )
            VALUES (?1, ?2, ?2, 'manual', ?3, 'unbound', NULL, ?4, ?5, ?6, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            "#,
            params![
                evidence.id.to_string(),
                work_record_id.to_string(),
                status,
                artifact_id,
                started_at_ms,
                ended_at_ms,
                evidence.command,
                evidence.exit_code,
                stdout_preview,
                stderr_preview,
                evidence.started_at.to_rfc3339(),
                evidence.duration_ms,
            ],
        )?;
        self.replace_evidence_artifact_links(
            evidence.id,
            stdout_artifact_id.as_deref(),
            stderr_artifact_id.as_deref(),
        )?;
        self.rebuild_search_projection()?;
        Ok(())
    }

    #[cfg(feature = "legacy-pr-evidence")]
    pub fn upsert_evidence(&self, evidence: &Evidence) -> Result<()> {
        let work_record_id = evidence
            .record_id
            .ok_or(StoreError::EvidenceMissingWorkRecord)?;
        let started_at_ms = timestamp_ms(evidence.started_at);
        let ended_at_ms = started_at_ms.saturating_add(evidence.duration_ms);
        let status = evidence_status(evidence.exit_code);
        let stdout_artifact_id = self.store_output_artifact("stdout", &evidence.stdout)?;
        let stderr_artifact_id = self.store_output_artifact("stderr", &evidence.stderr)?;
        let artifact_id = stdout_artifact_id
            .as_deref()
            .or(stderr_artifact_id.as_deref())
            .map(str::to_owned);
        let stdout_preview = output_preview(&evidence.stdout);
        let stderr_preview = output_preview(&evidence.stderr);
        self.conn.execute(
            r#"
            INSERT INTO evidence
            (
                id, work_record_id, record_id, kind, status, freshness,
                command_run_id, artifact_id, started_at_ms, ended_at_ms,
                created_at_ms, updated_at_ms, command, exit_code, stdout,
                stderr, started_at, duration_ms
            )
            VALUES (?1, ?2, ?2, 'manual', ?3, 'unbound', NULL, ?4, ?5, ?6, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT(id) DO UPDATE SET
                work_record_id = excluded.work_record_id,
                record_id = excluded.record_id,
                status = excluded.status,
                artifact_id = excluded.artifact_id,
                started_at_ms = excluded.started_at_ms,
                ended_at_ms = excluded.ended_at_ms,
                updated_at_ms = excluded.updated_at_ms,
                command = excluded.command,
                exit_code = excluded.exit_code,
                stdout = excluded.stdout,
                stderr = excluded.stderr,
                started_at = excluded.started_at,
                duration_ms = excluded.duration_ms
            "#,
            params![
                evidence.id.to_string(),
                work_record_id.to_string(),
                status,
                artifact_id,
                started_at_ms,
                ended_at_ms,
                evidence.command,
                evidence.exit_code,
                stdout_preview,
                stderr_preview,
                evidence.started_at.to_rfc3339(),
                evidence.duration_ms,
            ],
        )?;
        self.replace_evidence_artifact_links(
            evidence.id,
            stdout_artifact_id.as_deref(),
            stderr_artifact_id.as_deref(),
        )?;
        self.rebuild_search_projection()?;
        Ok(())
    }

    #[cfg(feature = "legacy-pr-evidence")]
    fn store_output_artifact(&self, kind: &str, content: &str) -> Result<Option<String>> {
        if content.is_empty() {
            return Ok(None);
        }

        let hash = sha256_hex(content.as_bytes());
        let shard = &hash[..2];
        let relative_path = object_relative_path(&hash);
        let absolute_dir = self.object_dir.join(shard);
        fs::create_dir_all(&absolute_dir)?;
        restrict_private_dir(&absolute_dir)?;
        let absolute_path = absolute_dir.join(&hash);
        let id = new_id();
        let _created = store_blob_content_if_missing(
            id,
            &absolute_path,
            content.as_bytes(),
            &hash,
            content.len() as u64,
        )?;

        let now = timestamp_ms(Utc::now());
        let id = id.to_string();
        self.conn.execute(
            r#"
            INSERT OR IGNORE INTO artifacts
            (
                id, kind, blob_hash, blob_path, byte_size, media_type,
                preview_text, redaction_state, created_at_ms, updated_at_ms,
                visibility, fidelity, sync_state
            )
            VALUES (?1, ?2, ?3, ?4, ?5, 'text/plain; charset=utf-8', ?6, 'safe_preview', ?7, ?7, 'local_only', 'full', 'local_only')
            "#,
            params![
                id,
                kind,
                hash,
                relative_path,
                content.len() as i64,
                output_preview(content),
                now,
            ],
        )?;

        let artifact_id = self.conn.query_row(
            "SELECT id FROM artifacts WHERE blob_hash = ?1 AND kind = ?2",
            params![hash, kind],
            |row| row.get::<_, String>(0),
        )?;
        Ok(Some(artifact_id))
    }

    #[cfg(feature = "legacy-pr-evidence")]
    fn replace_evidence_artifact_links(
        &self,
        evidence_id: Uuid,
        stdout_artifact_id: Option<&str>,
        stderr_artifact_id: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM evidence_artifacts WHERE evidence_id = ?1 AND stream IN ('stdout', 'stderr')",
            params![evidence_id.to_string()],
        )?;
        if let Some(artifact_id) = stdout_artifact_id {
            self.insert_evidence_artifact_link(evidence_id, artifact_id, "stdout")?;
        }
        if let Some(artifact_id) = stderr_artifact_id {
            self.insert_evidence_artifact_link(evidence_id, artifact_id, "stderr")?;
        }
        Ok(())
    }

    #[cfg(feature = "legacy-pr-evidence")]
    fn insert_evidence_artifact_link(
        &self,
        evidence_id: Uuid,
        artifact_id: &str,
        stream: &str,
    ) -> Result<()> {
        let now = timestamp_ms(Utc::now());
        self.conn.execute(
            r#"
            INSERT OR IGNORE INTO evidence_artifacts
            (
                id, evidence_id, artifact_id, stream, created_at_ms,
                updated_at_ms, visibility, fidelity, sync_state
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?5, 'local_only', 'full', 'local_only')
            "#,
            params![
                new_id().to_string(),
                evidence_id.to_string(),
                artifact_id,
                stream,
                now,
            ],
        )?;
        Ok(())
    }

    #[cfg(feature = "legacy-pr-evidence")]
    fn backfill_evidence_artifacts(&self) -> Result<()> {
        let rows = {
            let mut stmt = self.conn.prepare(
                r#"
                SELECT id, stdout, stderr
                FROM evidence
                WHERE (stdout != '' OR stderr != '')
                  AND id NOT IN (SELECT evidence_id FROM evidence_artifacts)
                "#,
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            collect_rows(rows)?
        };

        for (id, stdout, stderr) in rows {
            let evidence_id = Uuid::parse_str(&id)?;
            let stdout_artifact_id = self.store_output_artifact("stdout", &stdout)?;
            let stderr_artifact_id = self.store_output_artifact("stderr", &stderr)?;
            let artifact_id = stdout_artifact_id
                .as_deref()
                .or(stderr_artifact_id.as_deref())
                .map(str::to_owned);
            self.conn.execute(
                r#"
                UPDATE evidence
                SET artifact_id = COALESCE(artifact_id, ?1),
                    stdout = ?2,
                    stderr = ?3
                WHERE id = ?4
                "#,
                params![
                    artifact_id,
                    output_preview(&stdout),
                    output_preview(&stderr),
                    id
                ],
            )?;
            self.replace_evidence_artifact_links(
                evidence_id,
                stdout_artifact_id.as_deref(),
                stderr_artifact_id.as_deref(),
            )?;
        }
        Ok(())
    }

    #[cfg(feature = "legacy-pr-evidence")]
    pub fn evidence_for_record(&self, record_id: Uuid) -> Result<Vec<Evidence>> {
        let mut stmt = self.conn.prepare(
            evidence_select_sql(
                "WHERE record_id = ?1 OR work_record_id = ?1 ORDER BY started_at DESC",
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], evidence_from_row)?;
        collect_rows(rows)
    }

    #[cfg(feature = "legacy-pr-evidence")]
    pub fn get_evidence(&self, id: Uuid) -> Result<Evidence> {
        let mut stmt = self
            .conn
            .prepare(evidence_select_sql("WHERE id = ?1").as_str())?;
        stmt.query_row(params![id.to_string()], evidence_from_row)
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    #[cfg(feature = "legacy-pr-evidence")]
    pub fn get_evidence_metadata(&self, id: Uuid) -> Result<EvidenceMetadata> {
        self.conn
            .query_row(
                evidence_metadata_select_sql("WHERE id = ?1").as_str(),
                params![id.to_string()],
                evidence_metadata_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    #[cfg(feature = "legacy-pr-evidence")]
    pub fn evidence_metadata_for_record(&self, record_id: Uuid) -> Result<Vec<EvidenceMetadata>> {
        let mut stmt = self.conn.prepare(
            evidence_metadata_select_sql(
                "WHERE record_id = ?1 OR work_record_id = ?1 ORDER BY started_at_ms DESC, id",
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], evidence_metadata_from_row)?;
        collect_rows(rows)
    }

    #[cfg(feature = "legacy-pr-evidence")]
    pub fn recent_evidence_metadata(&self, limit: usize) -> Result<Vec<EvidenceMetadata>> {
        let mut stmt = self.conn.prepare(
            evidence_metadata_select_sql("ORDER BY started_at_ms DESC, id LIMIT ?1").as_str(),
        )?;
        let rows = stmt.query_map(params![limit as i64], evidence_metadata_from_row)?;
        collect_rows(rows)
    }

    #[cfg(feature = "legacy-pr-evidence")]
    pub fn update_evidence_metadata(&self, metadata: &EvidenceMetadata) -> Result<()> {
        let changed = self.conn.execute(
            r#"
            UPDATE evidence
            SET work_record_id = ?2,
                record_id = ?2,
                vcs_change_id = ?3,
                kind = ?4,
                status = ?5,
                freshness = ?6,
                command_run_id = ?7,
                artifact_id = COALESCE(?8, artifact_id),
                observed_tree_hash = ?9,
                observed_head_sha = ?10,
                started_at_ms = COALESCE(?11, started_at_ms),
                ended_at_ms = COALESCE(?12, ended_at_ms),
                stale_reason = ?13,
                updated_at_ms = ?14,
                source_id = ?15,
                visibility = ?16,
                fidelity = ?17,
                sync_state = ?18,
                sync_version = ?19,
                deleted_at_ms = ?20,
                metadata_json = ?21
            WHERE id = ?1
            "#,
            params![
                metadata.id.to_string(),
                metadata.work_record_id.to_string(),
                optional_uuid_string(metadata.vcs_change_id),
                metadata.kind.as_str(),
                metadata.status.as_str(),
                metadata.freshness.as_str(),
                optional_uuid_string(metadata.command_run_id),
                optional_uuid_string(metadata.artifact_id),
                metadata.observed_tree_hash.as_deref(),
                metadata.observed_head_sha.as_deref(),
                optional_timestamp_ms(metadata.started_at),
                optional_timestamp_ms(metadata.ended_at),
                metadata.stale_reason.as_deref(),
                timestamp_ms(metadata.timestamps.updated_at),
                optional_uuid_string(metadata.source_id),
                metadata.sync.visibility.as_str(),
                metadata.sync.fidelity.as_str(),
                metadata.sync.sync_state.as_str(),
                metadata.sync.sync_version as i64,
                optional_timestamp_ms(metadata.sync.deleted_at),
                serde_json::to_string(&metadata.sync.metadata)?,
            ],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(metadata.id));
        }
        Ok(())
    }

    #[cfg(feature = "legacy-pr-evidence")]
    pub fn recent_evidence(&self, limit: usize) -> Result<Vec<Evidence>> {
        let mut stmt = self
            .conn
            .prepare(evidence_select_sql("ORDER BY started_at DESC LIMIT ?1").as_str())?;
        let rows = stmt.query_map(params![limit as i64], evidence_from_row)?;
        collect_rows(rows)
    }

    pub fn export_archive(&self) -> Result<WorkRecordArchive> {
        #[cfg(feature = "legacy-pr-evidence")]
        let evidence = self.recent_evidence(usize::MAX)?;
        Ok(WorkRecordArchive {
            schema_version: 2,
            version: 2,
            records: self.list_records(usize::MAX)?,
            #[cfg(feature = "legacy-pr-evidence")]
            artifacts: self.archive_artifacts()?,
            #[cfg(feature = "legacy-pr-evidence")]
            evidence,
            #[cfg(feature = "legacy-pr-evidence")]
            evidence_metadata: self.recent_evidence_metadata(usize::MAX)?,
            capture_sources: self.list_capture_sources()?,
            sessions: self.list_sessions()?,
            runs: self.list_runs()?,
            events: self.list_events()?,
            artifact_records: self.list_artifacts()?,
            vcs_workspaces: self.list_vcs_workspaces()?,
            vcs_changes: self.list_vcs_changes()?,
            #[cfg(feature = "legacy-pr-evidence")]
            pull_requests: self.list_pull_requests()?,
            work_record_links: self.list_work_record_links()?,
            summaries: self.list_summaries()?,
            files_touched: self.list_files_touched()?,
        })
    }

    pub fn import_archive(&mut self, archive: &WorkRecordArchive, overwrite: bool) -> Result<()> {
        validate_archive_version(archive)?;
        reject_archive_event_internal_conflicts(archive)?;
        #[cfg(feature = "legacy-pr-evidence")]
        validate_import_evidence_references(&self.conn, archive)?;
        #[cfg(feature = "legacy-pr-evidence")]
        let archive_artifacts = archive_artifacts_by_evidence(archive);
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
        #[cfg(feature = "legacy-pr-evidence")]
        for evidence in &archive.evidence {
            if let Some(artifacts) = archive_artifacts.get(&evidence.id) {
                upsert_evidence_with_archive_artifacts_tx(
                    &tx,
                    &blob_dir,
                    evidence,
                    artifacts,
                    None,
                    &mut blob_guard,
                )?;
            } else {
                upsert_evidence_tx(&tx, &blob_dir, evidence, None, &mut blob_guard)?;
            }
        }
        import_rich_archive_entities_tx(&tx, &blob_dir, archive, &mut blob_guard)?;
        #[cfg(feature = "legacy-pr-evidence")]
        for metadata in &archive.evidence_metadata {
            update_evidence_metadata_tx(&tx, metadata)?;
        }
        tx.commit()?;
        blob_guard.commit();
        self.rebuild_search_projection()?;
        Ok(())
    }

    pub fn import_archive_from_capture_source(
        &mut self,
        archive: &WorkRecordArchive,
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

    #[cfg(feature = "legacy-pr-evidence")]
    fn archive_artifacts(&self) -> Result<Vec<WorkRecordArchiveArtifact>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT
                a.id,
                COALESCE(ea.evidence_id, '00000000-0000-0000-0000-000000000000'),
                COALESCE(ea.stream, 'blob'),
                a.kind,
                a.blob_hash,
                a.blob_path,
                a.byte_size,
                a.media_type,
                a.preview_text,
                a.redaction_state
            FROM artifacts a
            LEFT JOIN evidence_artifacts ea ON ea.artifact_id = a.id
            ORDER BY ea.evidence_id, ea.stream, a.id
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            let id = parse_uuid(row.get::<_, String>(0)?)?;
            let evidence_id = parse_uuid(row.get::<_, String>(1)?)?;
            let stream = row.get::<_, String>(2)?;
            let kind = parse_text_enum::<ArtifactKind>(row.get::<_, String>(3)?)?;
            let blob_hash = row.get::<_, String>(4)?;
            let blob_path = row.get::<_, String>(5)?;
            let byte_size = row.get::<_, i64>(6)? as u64;
            let media_type = row.get::<_, Option<String>>(7)?;
            let preview_text = row.get::<_, Option<String>>(8)?;
            let redaction_state = parse_text_enum::<RedactionState>(row.get::<_, String>(9)?)?;
            let absolute_blob_path = self
                .absolute_blob_path(&blob_path)
                .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
            ensure_regular_blob_file(id, &absolute_blob_path)
                .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
            let content = fs::read_to_string(absolute_blob_path)
                .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
            Ok(WorkRecordArchiveArtifact {
                id,
                evidence_id,
                stream,
                kind,
                blob_hash,
                blob_path,
                byte_size,
                media_type,
                preview_text,
                redaction_state,
                content,
            })
        })?;
        collect_rows(rows)
    }

    #[cfg(feature = "legacy-pr-evidence")]
    fn absolute_blob_path(&self, blob_path: &str) -> Result<PathBuf> {
        let relative_path = blob_path
            .strip_prefix("objects/")
            .or_else(|| blob_path.strip_prefix("blobs/"))
            .unwrap_or(blob_path);
        let path = Path::new(relative_path);
        if path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(StoreError::UnsafeBlobPath(blob_path.to_owned()));
        }
        Ok(self.object_dir.join(path))
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

fn migrate_legacy_work_record_layout(data_root: &Path) -> Result<bool> {
    let legacy_dir = data_root.join(LEGACY_WORK_RECORD_DIR);
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

#[cfg(feature = "legacy-pr-evidence")]
fn expected_object_or_legacy_blob_path(hash: &str) -> (String, String) {
    let shard = &hash[..2];
    (
        format!("{OBJECTS_DIR}/{shard}/{hash}"),
        format!("{LEGACY_BLOBS_DIR}/{shard}/{hash}"),
    )
}

fn rebuild_search_projection(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "work_record_search")? {
        return Ok(());
    }

    conn.execute("DELETE FROM work_record_search", [])?;
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
        INSERT INTO work_record_search
        (record_id, title, summary, primary_user_text, decision_text, context_text, tag_text)
        VALUES (?1, ?2, ?3, ?4, '', ?5, ?6)
        "#,
    )?;
    for record in records {
        insert_record_search.execute(params![
            record.id.to_string(),
            redact_preview(&record.title, 512),
            redact_preview(&record.body, 2048),
            redact_preview(&record.body, 2048),
            "",
            redact_preview(&record.tags.join(" "), 1024),
        ])?;
    }

    Ok(())
}

fn ensure_search_projection_initialized(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "work_record_search")? {
        return Ok(());
    }

    let mut projection_rows = table_row_count(conn, "work_record_search")?;
    if table_exists(conn, "event_search")? {
        projection_rows += table_row_count(conn, "event_search")?;
    }
    if table_exists(conn, "artifact_search")? {
        projection_rows += table_row_count(conn, "artifact_search")?;
    }
    if projection_rows > 0 {
        return Ok(());
    }

    if table_row_count(conn, "work_records")? > 0
        || table_row_count(conn, "events")? > 0
        || linked_artifact_preview_count(conn)? > 0
    {
        rebuild_search_projection(conn)?;
    }

    Ok(())
}

fn table_row_count(conn: &Connection, table: &str) -> Result<i64> {
    match table {
        "artifacts" | "artifact_search" | "events" | "event_search" | "work_records"
        | "work_record_search" => {}
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
               COALESCE(e.work_record_id, r.work_record_id, s.work_record_id, rs.work_record_id),
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
        (event_id, work_record_id, session_id, role, safe_preview_text, rank_bucket)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        "#,
    )?;
    for row in rows {
        let (event_id, work_record_id, session_id, role, event_type, payload_json, redaction_state) =
            row?;
        let preview = event_search_preview(&payload_json, &redaction_state)?;
        if preview.trim().is_empty() {
            continue;
        }
        insert_event_search.execute(params![
            event_id,
            work_record_id,
            session_id,
            role,
            preview,
            event_type
        ])?;
    }
    Ok(())
}

fn insert_event_search_projection_for_event(conn: &Connection, event: &Event) -> Result<()> {
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
        (event_id, work_record_id, session_id, role, safe_preview_text, rank_bucket)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        "#,
    )?
    .execute(params![
        event.id.to_string(),
        optional_uuid_string(event.work_record_id),
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
    redact_share_safe_preview(&preview, 2048)
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

fn migrate_to_v1(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(conn, "work_records", WORK_RECORD_COLUMNS)?;
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
        ensure_columns(conn, "work_records", WORK_RECORD_COLUMNS)?;
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
        ensure_columns(conn, "work_records", WORK_RECORD_COLUMNS)?;
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

fn rebuild_capture_sources_provider_check(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "capture_sources")? {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        return Ok(());
    }

    conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS capture_sources_new;
        CREATE TABLE capture_sources_new (
            id TEXT PRIMARY KEY NOT NULL,
            kind TEXT NOT NULL CHECK (kind IN ('provider_import', 'provider_hook', 'direct_cli', 'manual')),
            provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'opencode', 'antigravity', 'gemini', 'cursor', 'copilot_cli', 'factory_ai_droid', 'amp', 'shell', 'git', 'jj', 'gh', 'unknown')),
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
    Ok(())
}

fn rebuild_catalog_sessions_provider_check(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "catalog_sessions")? {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        return Ok(());
    }

    conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS catalog_sessions_new;
        CREATE TABLE catalog_sessions_new (
            source_path TEXT PRIMARY KEY NOT NULL,
            provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'opencode', 'antigravity', 'gemini', 'cursor', 'copilot_cli', 'factory_ai_droid', 'amp', 'shell', 'git', 'jj', 'gh', 'unknown')),
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
            metadata_json TEXT NOT NULL DEFAULT '{}'
        );
        INSERT INTO catalog_sessions_new
        (source_path, provider, source_format, source_root, external_session_id, parent_external_session_id, agent_type, role_hint, external_agent_id, cwd, session_started_at_ms, file_size_bytes, file_modified_at_ms, cataloged_at_ms, is_stale, indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_status, indexed_error, indexed_event_count, metadata_json)
        SELECT source_path, provider, source_format, source_root, external_session_id, parent_external_session_id, agent_type, role_hint, external_agent_id, cwd, session_started_at_ms, file_size_bytes, file_modified_at_ms, cataloged_at_ms, is_stale, indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_status, indexed_error, indexed_event_count, metadata_json
        FROM catalog_sessions;
        DROP TABLE catalog_sessions;
        ALTER TABLE catalog_sessions_new RENAME TO catalog_sessions;
        "#,
    )?;
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
    let mut stmt = conn.prepare("SELECT dedupe_key FROM events WHERE dedupe_key IS NOT NULL")?;
    reject_provider_event_hash_conflict_from_rows(dedupe_key, &mut stmt)
}

fn reject_provider_event_hash_conflict_tx(tx: &Transaction<'_>, dedupe_key: &str) -> Result<()> {
    let mut stmt = tx.prepare("SELECT dedupe_key FROM events WHERE dedupe_key IS NOT NULL")?;
    reject_provider_event_hash_conflict_from_rows(dedupe_key, &mut stmt)
}

fn reject_provider_event_hash_conflict_from_rows(
    dedupe_key: &str,
    stmt: &mut rusqlite::Statement<'_>,
) -> Result<()> {
    let Some((provider, external_session_id, provider_index, new_hash)) =
        parse_provider_event_dedupe_key(dedupe_key)
    else {
        return Ok(());
    };
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    for row in rows {
        let existing_key = row?;
        let Some((existing_provider, existing_session_id, existing_index, existing_hash)) =
            parse_provider_event_dedupe_key(&existing_key)
        else {
            continue;
        };
        if existing_provider == provider
            && existing_session_id == external_session_id
            && existing_index == provider_index
            && existing_hash != new_hash
        {
            return Err(StoreError::ProviderEventConflict {
                provider,
                external_session_id,
                provider_index,
                existing_hash,
                new_hash,
            });
        }
    }
    Ok(())
}

fn parse_provider_event_dedupe_key(dedupe_key: &str) -> Option<(String, String, u64, String)> {
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
        Some((provider, external_session_id, provider_index, payload_hash))
    }
}

fn fts_match_query(query: &str) -> Option<String> {
    let terms = query
        .split_whitespace()
        .map(|term| term.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-'))
        .filter(|term| !term.is_empty())
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
        UPDATE work_records
        SET summary = body
        WHERE summary IS NULL;

        UPDATE work_records
        SET created_at_ms = COALESCE(CAST(strftime('%s', created_at) AS INTEGER) * 1000, created_at_ms)
        WHERE created_at_ms = 0 AND created_at IS NOT NULL;

        UPDATE work_records
        SET updated_at_ms = COALESCE(CAST(strftime('%s', updated_at) AS INTEGER) * 1000, updated_at_ms)
        WHERE updated_at_ms = 0 AND updated_at IS NOT NULL;

        UPDATE work_records
        SET started_at_ms = created_at_ms
        WHERE started_at_ms IS NULL AND created_at_ms != 0;

        UPDATE work_records
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

fn time_ms(value: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp_millis(value).unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

#[cfg(feature = "legacy-pr-evidence")]
fn optional_time_ms(value: Option<i64>) -> Option<DateTime<Utc>> {
    value.map(time_ms)
}

fn redacted_root_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(|name| format!("[REDACTED_ROOT]/{name}"))
        .unwrap_or_else(|| "[REDACTED_ROOT]".to_owned())
}

#[cfg(feature = "legacy-pr-evidence")]
fn evidence_status(exit_code: i32) -> &'static str {
    if exit_code == 0 {
        "passed"
    } else {
        "failed"
    }
}

#[cfg(feature = "legacy-pr-evidence")]
fn output_preview(content: &str) -> String {
    const MAX_CHARS: usize = 4096;
    redact_preview(content, MAX_CHARS)
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

#[cfg(feature = "legacy-pr-evidence")]
fn ensure_existing_blob_content(
    id: Uuid,
    path: &Path,
    expected_hash: &str,
    expected_size: u64,
) -> Result<()> {
    ensure_regular_blob_file(id, path)?;
    let content = fs::read(path)?;
    let hash = sha256_hex(&content);
    if hash != expected_hash {
        return Err(StoreError::ArchiveArtifactHashMismatch { id });
    }
    if content.len() as u64 != expected_size {
        return Err(StoreError::ArchiveArtifactSizeMismatch { id });
    }
    restrict_private_file(path)?;
    Ok(())
}

#[cfg(feature = "legacy-pr-evidence")]
fn store_blob_content_if_missing(
    id: Uuid,
    path: &Path,
    content: &[u8],
    expected_hash: &str,
    expected_size: u64,
) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            ensure_existing_blob_content(id, path, expected_hash, expected_size)?;
            Ok(false)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            use std::io::Write as _;

            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?;
            file.write_all(content)?;
            file.sync_all()?;
            drop(file);
            restrict_private_file(path)?;
            Ok(true)
        }
        Err(err) => Err(StoreError::Io(err)),
    }
}

#[derive(Debug, Default)]
struct BlobWriteGuard {
    created_paths: Vec<PathBuf>,
    committed: bool,
}

impl BlobWriteGuard {
    #[cfg(feature = "legacy-pr-evidence")]
    fn track_created(&mut self, path: &Path, created: bool) {
        if created {
            self.created_paths.push(path.to_path_buf());
        }
    }

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

pub fn validate_archive_version(archive: &WorkRecordArchive) -> Result<()> {
    if matches!((archive.schema_version, archive.version), (1, 1) | (2, 2)) {
        Ok(())
    } else {
        Err(StoreError::UnsupportedArchiveVersion(
            archive.schema_version.max(archive.version),
        ))
    }
}

#[cfg(feature = "legacy-pr-evidence")]
fn validate_import_evidence_references(
    conn: &Connection,
    archive: &WorkRecordArchive,
) -> Result<()> {
    let archive_record_ids = archive
        .records
        .iter()
        .map(|record| record.id)
        .collect::<HashSet<_>>();
    for evidence in &archive.evidence {
        let record_id = evidence
            .record_id
            .ok_or(StoreError::EvidenceMissingWorkRecord)?;
        if archive_record_ids.contains(&record_id) || record_exists(conn, record_id)? {
            continue;
        }
        return Err(StoreError::NotFound(record_id));
    }
    let archive_evidence_ids = archive
        .evidence
        .iter()
        .map(|evidence| evidence.id)
        .collect::<HashSet<_>>();
    for artifact in &archive.artifacts {
        if artifact.evidence_id != Uuid::nil()
            && !archive_evidence_ids.contains(&artifact.evidence_id)
        {
            return Err(StoreError::NotFound(artifact.evidence_id));
        }
    }
    Ok(())
}

#[cfg(feature = "legacy-pr-evidence")]
fn archive_artifacts_by_evidence(
    archive: &WorkRecordArchive,
) -> HashMap<Uuid, Vec<&WorkRecordArchiveArtifact>> {
    let mut artifacts = HashMap::<Uuid, Vec<&WorkRecordArchiveArtifact>>::new();
    for artifact in &archive.artifacts {
        artifacts
            .entry(artifact.evidence_id)
            .or_default()
            .push(artifact);
    }
    artifacts
}

fn reject_import_conflicts(tx: &Transaction<'_>, archive: &WorkRecordArchive) -> Result<()> {
    for record in &archive.records {
        if row_exists(tx, "work_records", record.id)? {
            return Err(StoreError::ImportConflict {
                kind: "record",
                id: record.id,
            });
        }
    }
    #[cfg(feature = "legacy-pr-evidence")]
    for evidence in &archive.evidence {
        if row_exists(tx, "evidence", evidence.id)? {
            return Err(StoreError::ImportConflict {
                kind: "evidence",
                id: evidence.id,
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
    archive: &WorkRecordArchive,
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

fn reject_rich_import_conflicts(tx: &Transaction<'_>, archive: &WorkRecordArchive) -> Result<()> {
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
        if let Some(external_session_id) = &source.descriptor.external_session_id {
            reject_entity_conflict(
                existing_capture_source_by_external_session(
                    tx,
                    source.descriptor.provider,
                    external_session_id,
                )?,
                source,
                "capture_source",
                source.id,
            )?;
        }
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
    #[cfg(feature = "legacy-pr-evidence")]
    for artifact in &archive.artifacts {
        reject_archive_artifact_conflict(tx, artifact)?;
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
    #[cfg(feature = "legacy-pr-evidence")]
    for pr in &archive.pull_requests {
        reject_entity_conflict(
            existing_pull_request_by_id(tx, pr.id)?,
            pr,
            "pull_request",
            pr.id,
        )?;
        if let (Some(owner), Some(repo), Some(number)) = (&pr.owner, &pr.repo, pr.number) {
            reject_entity_conflict(
                existing_pull_request_by_identity(tx, pr.provider, owner, repo, number)?,
                pr,
                "pull_request",
                pr.id,
            )?;
        }
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
    for link in &archive.work_record_links {
        reject_entity_conflict(
            existing_work_record_link_by_id(tx, link.id)?,
            link,
            "work_record_link",
            link.id,
        )?;
        reject_entity_conflict(
            existing_work_record_link_by_identity(tx, link)?,
            link,
            "work_record_link",
            link.id,
        )?;
    }
    Ok(())
}

fn reject_archive_event_internal_conflicts(archive: &WorkRecordArchive) -> Result<()> {
    let mut seen_seq: HashMap<u64, &Event> = HashMap::new();
    let mut seen_provider_events: HashMap<(String, String, u64), String> = HashMap::new();

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
        let Some((provider, external_session_id, provider_index, payload_hash)) =
            parse_provider_event_dedupe_key(dedupe_key)
        else {
            continue;
        };
        let key = (provider, external_session_id, provider_index);
        if let Some(existing_hash) = seen_provider_events.get(&key) {
            if existing_hash != &payload_hash {
                return Err(StoreError::ProviderEventConflict {
                    provider: key.0,
                    external_session_id: key.1,
                    provider_index: key.2,
                    existing_hash: existing_hash.clone(),
                    new_hash: payload_hash,
                });
            }
        } else {
            seen_provider_events.insert(key, payload_hash);
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

fn existing_capture_source_by_external_session(
    tx: &Transaction<'_>,
    provider: CaptureProvider,
    external_session_id: &str,
) -> Result<Option<CaptureSource>> {
    tx.query_row(
        "SELECT id, kind, provider, machine_id, process_id, cwd, raw_source_path, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json FROM capture_sources WHERE provider = ?1 AND external_session_id = ?2 ORDER BY started_at_ms DESC LIMIT 1",
        params![provider.as_str(), external_session_id],
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

#[cfg(feature = "legacy-pr-evidence")]
fn existing_pull_request_by_id(tx: &Transaction<'_>, id: Uuid) -> Result<Option<PullRequest>> {
    tx.query_row(
        pull_request_select_sql("WHERE id = ?1").as_str(),
        params![id.to_string()],
        pull_request_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

#[cfg(feature = "legacy-pr-evidence")]
fn existing_pull_request_by_identity(
    tx: &Transaction<'_>,
    provider: work_record_core::PullRequestProvider,
    owner: &str,
    repo: &str,
    number: u64,
) -> Result<Option<PullRequest>> {
    tx.query_row(
        pull_request_select_sql("WHERE provider = ?1 AND owner = ?2 AND repo = ?3 AND number = ?4")
            .as_str(),
        params![provider.as_str(), owner, repo, number as i64],
        pull_request_from_row,
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

fn existing_work_record_link_by_id(
    tx: &Transaction<'_>,
    id: Uuid,
) -> Result<Option<WorkRecordLink>> {
    tx.query_row(
        work_record_link_select_sql("WHERE id = ?1").as_str(),
        params![id.to_string()],
        work_record_link_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_work_record_link_by_identity(
    tx: &Transaction<'_>,
    link: &WorkRecordLink,
) -> Result<Option<WorkRecordLink>> {
    tx.query_row(
        work_record_link_select_sql(
            "WHERE work_record_id = ?1 AND target_type = ?2 AND target_id = ?3 AND link_type = ?4",
        )
        .as_str(),
        params![
            link.work_record_id.to_string(),
            link.target_type.as_str(),
            link.target_id.to_string(),
            link.link_type.as_str()
        ],
        work_record_link_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

#[cfg(feature = "legacy-pr-evidence")]
fn reject_archive_artifact_conflict(
    tx: &Transaction<'_>,
    artifact: &WorkRecordArchiveArtifact,
) -> Result<()> {
    let hash = sha256_hex(artifact.content.as_bytes());
    if hash != artifact.blob_hash {
        return Err(StoreError::ArchiveArtifactHashMismatch { id: artifact.id });
    }
    if artifact.content.len() as u64 != artifact.byte_size {
        return Err(StoreError::ArchiveArtifactSizeMismatch { id: artifact.id });
    }
    let (relative_path, legacy_relative_path) = expected_object_or_legacy_blob_path(&hash);
    if artifact.blob_path != relative_path && artifact.blob_path != legacy_relative_path {
        return Err(StoreError::ArchiveArtifactPathMismatch { id: artifact.id });
    }

    for existing in [
        existing_artifact_by_id(tx, artifact.id)?,
        existing_artifact_by_hash_kind(tx, &artifact.blob_hash, artifact.kind)?,
    ]
    .into_iter()
    .flatten()
    {
        if existing.kind != artifact.kind
            || existing.blob_hash != artifact.blob_hash
            || existing.blob_path != artifact.blob_path
            || existing.byte_size != artifact.byte_size
            || existing.media_type != artifact.media_type
        {
            return Err(StoreError::ImportConflict {
                kind: "artifact",
                id: artifact.id,
            });
        }
    }
    Ok(())
}

fn expected_archive_blob_path(id: Uuid, blob_hash: &str) -> Result<String> {
    if blob_hash.get(..2).is_none() {
        return Err(StoreError::ArchiveArtifactPathMismatch { id });
    }
    Ok(object_relative_path(blob_hash))
}

fn validate_archive_artifact_record_blobs(
    blob_dir: &Path,
    archive: &WorkRecordArchive,
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

#[cfg(feature = "legacy-pr-evidence")]
fn record_exists(conn: &Connection, id: Uuid) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM work_records WHERE id = ?1",
            params![id.to_string()],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
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
    archive: &WorkRecordArchive,
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
    for link in &archive.work_record_links {
        upsert_work_record_link_tx(tx, link)?;
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
        (id, work_record_id, parent_session_id, root_session_id, capture_source_id, provider, external_session_id, external_agent_id, agent_type, role_hint, is_primary, status, fidelity, transcript_blob_id, started_at_ms, ended_at_ms, created_at_ms, updated_at_ms, visibility, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
        ON CONFLICT(id) DO UPDATE SET
            work_record_id = excluded.work_record_id,
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
            optional_uuid_string(session.work_record_id),
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
        (id, work_record_id, session_id, run_type, status, started_at_ms, ended_at_ms, exit_code, cwd, command_preview, input_blob_id, output_blob_id, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
        ON CONFLICT(id) DO UPDATE SET
            work_record_id = excluded.work_record_id,
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
            optional_uuid_string(run.work_record_id),
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
        (id, seq, work_record_id, session_id, run_id, event_type, role, occurred_at_ms, capture_source_id, payload_json, payload_blob_id, dedupe_key, visibility, redaction_state, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
        ON CONFLICT(id) DO UPDATE SET
            seq = excluded.seq,
            work_record_id = excluded.work_record_id,
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
            optional_uuid_string(event.work_record_id),
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

#[cfg(feature = "legacy-pr-evidence")]
fn upsert_pull_request_tx(tx: &Transaction<'_>, pr: &PullRequest) -> Result<Uuid> {
    tx.execute(
        r#"
        INSERT INTO pull_requests
        (id, vcs_workspace_id, provider, url, number, owner, repo, title, state, head_ref, base_ref, head_sha, confidence, link_source, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
        ON CONFLICT DO UPDATE SET
            vcs_workspace_id = COALESCE(excluded.vcs_workspace_id, pull_requests.vcs_workspace_id),
            url = excluded.url,
            title = COALESCE(excluded.title, pull_requests.title),
            state = COALESCE(excluded.state, pull_requests.state),
            head_ref = COALESCE(excluded.head_ref, pull_requests.head_ref),
            base_ref = COALESCE(excluded.base_ref, pull_requests.base_ref),
            head_sha = COALESCE(excluded.head_sha, pull_requests.head_sha),
            confidence = excluded.confidence,
            link_source = excluded.link_source,
            updated_at_ms = excluded.updated_at_ms,
            source_id = COALESCE(excluded.source_id, pull_requests.source_id),
            visibility = excluded.visibility,
            fidelity = excluded.fidelity,
            sync_state = excluded.sync_state,
            sync_version = excluded.sync_version,
            deleted_at_ms = excluded.deleted_at_ms,
            metadata_json = excluded.metadata_json
        "#,
        params![
            pr.id.to_string(),
            optional_uuid_string(pr.vcs_workspace_id),
            pr.provider.as_str(),
            pr.url.as_str(),
            pr.number.map(|n| n as i64),
            pr.owner.as_deref(),
            pr.repo.as_deref(),
            pr.title.as_deref(),
            pr.state.as_deref(),
            pr.head_ref.as_deref(),
            pr.base_ref.as_deref(),
            pr.head_sha.as_deref(),
            pr.confidence.as_str(),
            pr.link_source.as_str(),
            timestamp_ms(pr.timestamps.created_at),
            timestamp_ms(pr.timestamps.updated_at),
            optional_uuid_string(pr.source_id),
            pr.sync.visibility.as_str(),
            pr.sync.fidelity.as_str(),
            pr.sync.sync_state.as_str(),
            pr.sync.sync_version as i64,
            optional_timestamp_ms(pr.sync.deleted_at),
            serde_json::to_string(&pr.sync.metadata)?,
        ],
    )?;
    if let (Some(owner), Some(repo), Some(number)) = (&pr.owner, &pr.repo, pr.number) {
        return tx
            .query_row(
                "SELECT id FROM pull_requests WHERE provider = ?1 AND owner = ?2 AND repo = ?3 AND number = ?4",
                params![pr.provider.as_str(), owner, repo, number as i64],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .map_err(StoreError::from);
    }
    Ok(pr.id)
}

fn upsert_summary_tx(tx: &Transaction<'_>, summary: &Summary) -> Result<()> {
    tx.execute(
        r#"
        INSERT INTO summaries
        (id, work_record_id, session_id, kind, model_or_source, text, citations_json, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
        ON CONFLICT(id) DO UPDATE SET
            work_record_id = excluded.work_record_id,
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
            optional_uuid_string(summary.work_record_id),
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
        (id, work_record_id, run_id, event_id, vcs_workspace_id, path, change_kind, old_path, line_count_delta, confidence, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
        ON CONFLICT(id) DO UPDATE SET
            work_record_id = excluded.work_record_id,
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
            optional_uuid_string(file.work_record_id),
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

fn upsert_work_record_link_tx(tx: &Transaction<'_>, link: &WorkRecordLink) -> Result<Uuid> {
    tx.execute(
        r#"
        INSERT INTO work_record_links
        (id, work_record_id, target_type, target_id, link_type, confidence, source_id, created_at_ms, updated_at_ms, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
        ON CONFLICT(work_record_id, target_type, target_id, link_type) DO UPDATE SET
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
            link.work_record_id.to_string(),
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
        "SELECT id FROM work_record_links WHERE work_record_id = ?1 AND target_type = ?2 AND target_id = ?3 AND link_type = ?4",
        params![
            link.work_record_id.to_string(),
            link.target_type.as_str(),
            link.target_id.to_string(),
            link.link_type.as_str()
        ],
        |row| parse_uuid(row.get::<_, String>(0)?),
    )
    .map_err(StoreError::from)
}

fn upsert_record_tx(
    tx: &Transaction<'_>,
    record: &WorkRecord,
    source_id: Option<Uuid>,
) -> Result<()> {
    let created_at_ms = timestamp_ms(record.created_at);
    let updated_at_ms = timestamp_ms(record.updated_at);
    tx.execute(
        r#"
        INSERT INTO work_records
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
            source_id = COALESCE(excluded.source_id, work_records.source_id),
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

#[cfg(feature = "legacy-pr-evidence")]
fn upsert_evidence_tx(
    tx: &Transaction<'_>,
    blob_dir: &Path,
    evidence: &Evidence,
    source_id: Option<Uuid>,
    blob_guard: &mut BlobWriteGuard,
) -> Result<()> {
    let work_record_id = evidence
        .record_id
        .ok_or(StoreError::EvidenceMissingWorkRecord)?;
    let started_at_ms = timestamp_ms(evidence.started_at);
    let ended_at_ms = started_at_ms.saturating_add(evidence.duration_ms);
    let status = evidence_status(evidence.exit_code);
    let stdout_artifact_id =
        store_output_artifact_tx(tx, blob_dir, "stdout", &evidence.stdout, blob_guard)?;
    let stderr_artifact_id =
        store_output_artifact_tx(tx, blob_dir, "stderr", &evidence.stderr, blob_guard)?;
    let artifact_id = stdout_artifact_id
        .as_deref()
        .or(stderr_artifact_id.as_deref())
        .map(str::to_owned);
    let stdout_preview = output_preview(&evidence.stdout);
    let stderr_preview = output_preview(&evidence.stderr);
    tx.execute(
        r#"
        INSERT INTO evidence
        (
            id, work_record_id, record_id, kind, status, freshness,
            command_run_id, artifact_id, started_at_ms, ended_at_ms,
            created_at_ms, updated_at_ms, source_id, command, exit_code,
            stdout, stderr, started_at, duration_ms
        )
        VALUES (?1, ?2, ?2, 'manual', ?3, 'unbound', NULL, ?4, ?5, ?6, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
        ON CONFLICT(id) DO UPDATE SET
            work_record_id = excluded.work_record_id,
            record_id = excluded.record_id,
            status = excluded.status,
            artifact_id = excluded.artifact_id,
            started_at_ms = excluded.started_at_ms,
            ended_at_ms = excluded.ended_at_ms,
            updated_at_ms = excluded.updated_at_ms,
            source_id = COALESCE(excluded.source_id, evidence.source_id),
            command = excluded.command,
            exit_code = excluded.exit_code,
            stdout = excluded.stdout,
            stderr = excluded.stderr,
            started_at = excluded.started_at,
            duration_ms = excluded.duration_ms
        "#,
        params![
            evidence.id.to_string(),
            work_record_id.to_string(),
            status,
            artifact_id,
            started_at_ms,
            ended_at_ms,
            source_id.map(|id| id.to_string()),
            evidence.command,
            evidence.exit_code,
            stdout_preview,
            stderr_preview,
            evidence.started_at.to_rfc3339(),
            evidence.duration_ms,
        ],
    )?;
    replace_evidence_artifact_links_tx(
        tx,
        evidence.id,
        stdout_artifact_id.as_deref(),
        stderr_artifact_id.as_deref(),
    )?;
    Ok(())
}

#[cfg(feature = "legacy-pr-evidence")]
fn upsert_evidence_with_archive_artifacts_tx(
    tx: &Transaction<'_>,
    blob_dir: &Path,
    evidence: &Evidence,
    artifacts: &[&WorkRecordArchiveArtifact],
    source_id: Option<Uuid>,
    blob_guard: &mut BlobWriteGuard,
) -> Result<()> {
    let mut stdout = None;
    let mut stderr = None;
    let mut stdout_artifact_id = None;
    let mut stderr_artifact_id = None;

    for artifact in artifacts {
        let artifact_id = store_archive_artifact_tx(tx, blob_dir, artifact, blob_guard)?;
        match artifact.stream.as_str() {
            "stdout" => {
                stdout = Some(artifact.content.clone());
                stdout_artifact_id = Some(artifact_id);
            }
            "stderr" => {
                stderr = Some(artifact.content.clone());
                stderr_artifact_id = Some(artifact_id);
            }
            _ => {}
        }
    }

    let work_record_id = evidence
        .record_id
        .ok_or(StoreError::EvidenceMissingWorkRecord)?;
    let started_at_ms = timestamp_ms(evidence.started_at);
    let ended_at_ms = started_at_ms.saturating_add(evidence.duration_ms);
    let status = evidence_status(evidence.exit_code);
    let stdout = stdout.unwrap_or_else(|| evidence.stdout.clone());
    let stderr = stderr.unwrap_or_else(|| evidence.stderr.clone());
    let artifact_id = stdout_artifact_id
        .as_deref()
        .or(stderr_artifact_id.as_deref())
        .map(str::to_owned);

    tx.execute(
        r#"
        INSERT INTO evidence
        (
            id, work_record_id, record_id, kind, status, freshness,
            command_run_id, artifact_id, started_at_ms, ended_at_ms,
            created_at_ms, updated_at_ms, source_id, command, exit_code,
            stdout, stderr, started_at, duration_ms
        )
        VALUES (?1, ?2, ?2, 'manual', ?3, 'unbound', NULL, ?4, ?5, ?6, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
        ON CONFLICT(id) DO UPDATE SET
            work_record_id = excluded.work_record_id,
            record_id = excluded.record_id,
            status = excluded.status,
            artifact_id = excluded.artifact_id,
            started_at_ms = excluded.started_at_ms,
            ended_at_ms = excluded.ended_at_ms,
            updated_at_ms = excluded.updated_at_ms,
            source_id = COALESCE(excluded.source_id, evidence.source_id),
            command = excluded.command,
            exit_code = excluded.exit_code,
            stdout = excluded.stdout,
            stderr = excluded.stderr,
            started_at = excluded.started_at,
            duration_ms = excluded.duration_ms
        "#,
        params![
            evidence.id.to_string(),
            work_record_id.to_string(),
            status,
            artifact_id,
            started_at_ms,
            ended_at_ms,
            source_id.map(|id| id.to_string()),
            evidence.command,
            evidence.exit_code,
            output_preview(&stdout),
            output_preview(&stderr),
            evidence.started_at.to_rfc3339(),
            evidence.duration_ms,
        ],
    )?;

    replace_evidence_artifact_links_tx(
        tx,
        evidence.id,
        stdout_artifact_id.as_deref(),
        stderr_artifact_id.as_deref(),
    )?;
    Ok(())
}

#[cfg(feature = "legacy-pr-evidence")]
fn update_evidence_metadata_tx(tx: &Transaction<'_>, metadata: &EvidenceMetadata) -> Result<()> {
    let changed = tx.execute(
        r#"
        UPDATE evidence
        SET work_record_id = ?2,
            record_id = ?2,
            vcs_change_id = ?3,
            kind = ?4,
            status = ?5,
            freshness = ?6,
            command_run_id = ?7,
            artifact_id = COALESCE(?8, artifact_id),
            observed_tree_hash = ?9,
            observed_head_sha = ?10,
            started_at_ms = COALESCE(?11, started_at_ms),
            ended_at_ms = COALESCE(?12, ended_at_ms),
            stale_reason = ?13,
            updated_at_ms = ?14,
            source_id = ?15,
            visibility = ?16,
            fidelity = ?17,
            sync_state = ?18,
            sync_version = ?19,
            deleted_at_ms = ?20,
            metadata_json = ?21
        WHERE id = ?1
        "#,
        params![
            metadata.id.to_string(),
            metadata.work_record_id.to_string(),
            optional_uuid_string(metadata.vcs_change_id),
            metadata.kind.as_str(),
            metadata.status.as_str(),
            metadata.freshness.as_str(),
            optional_uuid_string(metadata.command_run_id),
            optional_uuid_string(metadata.artifact_id),
            metadata.observed_tree_hash.as_deref(),
            metadata.observed_head_sha.as_deref(),
            optional_timestamp_ms(metadata.started_at),
            optional_timestamp_ms(metadata.ended_at),
            metadata.stale_reason.as_deref(),
            timestamp_ms(metadata.timestamps.updated_at),
            optional_uuid_string(metadata.source_id),
            metadata.sync.visibility.as_str(),
            metadata.sync.fidelity.as_str(),
            metadata.sync.sync_state.as_str(),
            metadata.sync.sync_version as i64,
            optional_timestamp_ms(metadata.sync.deleted_at),
            serde_json::to_string(&metadata.sync.metadata)?,
        ],
    )?;
    if changed == 0 {
        return Err(StoreError::NotFound(metadata.id));
    }
    Ok(())
}

#[cfg(feature = "legacy-pr-evidence")]
fn store_archive_artifact_tx(
    tx: &Transaction<'_>,
    blob_dir: &Path,
    artifact: &WorkRecordArchiveArtifact,
    blob_guard: &mut BlobWriteGuard,
) -> Result<String> {
    let hash = sha256_hex(artifact.content.as_bytes());
    if hash != artifact.blob_hash {
        return Err(StoreError::ArchiveArtifactHashMismatch { id: artifact.id });
    }
    if artifact.content.len() as u64 != artifact.byte_size {
        return Err(StoreError::ArchiveArtifactSizeMismatch { id: artifact.id });
    }

    let shard = &hash[..2];
    let relative_path = object_relative_path(&hash);
    let legacy_relative_path = format!("{LEGACY_BLOBS_DIR}/{shard}/{hash}");
    if artifact.blob_path != relative_path && artifact.blob_path != legacy_relative_path {
        return Err(StoreError::ArchiveArtifactPathMismatch { id: artifact.id });
    }
    let absolute_dir = blob_dir.join(shard);
    fs::create_dir_all(&absolute_dir)?;
    restrict_private_dir(&absolute_dir)?;
    let absolute_path = absolute_dir.join(&hash);
    let created = store_blob_content_if_missing(
        artifact.id,
        &absolute_path,
        artifact.content.as_bytes(),
        &artifact.blob_hash,
        artifact.byte_size,
    )?;
    blob_guard.track_created(&absolute_path, created);

    let now = timestamp_ms(Utc::now());
    tx.execute(
        r#"
        INSERT OR IGNORE INTO artifacts
        (
            id, kind, blob_hash, blob_path, byte_size, media_type,
            preview_text, redaction_state, created_at_ms, updated_at_ms,
            visibility, fidelity, sync_state
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'safe_preview', ?8, ?8, 'local_only', 'full', 'local_only')
        "#,
        params![
            artifact.id.to_string(),
            artifact.kind.as_str(),
            hash,
            relative_path,
            artifact.content.len() as i64,
            artifact.media_type.as_deref(),
            Some(output_preview(&artifact.content)),
            now,
        ],
    )?;

    let artifact_id = tx.query_row(
        "SELECT id FROM artifacts WHERE blob_hash = ?1 AND kind = ?2",
        params![hash, artifact.kind.as_str()],
        |row| row.get::<_, String>(0),
    )?;
    Ok(artifact_id)
}

#[cfg(feature = "legacy-pr-evidence")]
fn store_output_artifact_tx(
    tx: &Transaction<'_>,
    blob_dir: &Path,
    kind: &str,
    content: &str,
    blob_guard: &mut BlobWriteGuard,
) -> Result<Option<String>> {
    if content.is_empty() {
        return Ok(None);
    }

    let hash = sha256_hex(content.as_bytes());
    let shard = &hash[..2];
    let relative_path = object_relative_path(&hash);
    let absolute_dir = blob_dir.join(shard);
    fs::create_dir_all(&absolute_dir)?;
    restrict_private_dir(&absolute_dir)?;
    let absolute_path = absolute_dir.join(&hash);
    let id = new_id();
    let created = store_blob_content_if_missing(
        id,
        &absolute_path,
        content.as_bytes(),
        &hash,
        content.len() as u64,
    )?;
    blob_guard.track_created(&absolute_path, created);

    let now = timestamp_ms(Utc::now());
    let id = id.to_string();
    tx.execute(
        r#"
        INSERT OR IGNORE INTO artifacts
        (
            id, kind, blob_hash, blob_path, byte_size, media_type,
            preview_text, redaction_state, created_at_ms, updated_at_ms,
            visibility, fidelity, sync_state
        )
        VALUES (?1, ?2, ?3, ?4, ?5, 'text/plain; charset=utf-8', ?6, 'safe_preview', ?7, ?7, 'local_only', 'full', 'local_only')
        "#,
        params![
            id,
            kind,
            hash,
            relative_path,
            content.len() as i64,
            output_preview(content),
            now,
        ],
    )?;

    let artifact_id = tx.query_row(
        "SELECT id FROM artifacts WHERE blob_hash = ?1 AND kind = ?2",
        params![hash, kind],
        |row| row.get::<_, String>(0),
    )?;
    Ok(Some(artifact_id))
}

#[cfg(feature = "legacy-pr-evidence")]
fn replace_evidence_artifact_links_tx(
    tx: &Transaction<'_>,
    evidence_id: Uuid,
    stdout_artifact_id: Option<&str>,
    stderr_artifact_id: Option<&str>,
) -> Result<()> {
    tx.execute(
        "DELETE FROM evidence_artifacts WHERE evidence_id = ?1 AND stream IN ('stdout', 'stderr')",
        params![evidence_id.to_string()],
    )?;
    if let Some(artifact_id) = stdout_artifact_id {
        insert_evidence_artifact_link_tx(tx, evidence_id, artifact_id, "stdout")?;
    }
    if let Some(artifact_id) = stderr_artifact_id {
        insert_evidence_artifact_link_tx(tx, evidence_id, artifact_id, "stderr")?;
    }
    Ok(())
}

#[cfg(feature = "legacy-pr-evidence")]
fn insert_evidence_artifact_link_tx(
    tx: &Transaction<'_>,
    evidence_id: Uuid,
    artifact_id: &str,
    stream: &str,
) -> Result<()> {
    let now = timestamp_ms(Utc::now());
    tx.execute(
        r#"
        INSERT OR IGNORE INTO evidence_artifacts
        (
            id, evidence_id, artifact_id, stream, created_at_ms,
            updated_at_ms, visibility, fidelity, sync_state
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?5, 'local_only', 'full', 'local_only')
        "#,
        params![
            new_id().to_string(),
            evidence_id.to_string(),
            artifact_id,
            stream,
            now,
        ],
    )?;
    Ok(())
}

fn capture_source_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CaptureSource> {
    Ok(CaptureSource {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        descriptor: CaptureSourceDescriptor {
            kind: parse_text_enum::<work_record_core::CaptureSourceKind>(row.get::<_, String>(1)?)?,
            provider: parse_text_enum::<CaptureProvider>(row.get::<_, String>(2)?)?,
            machine_id: row.get(3)?,
            process_id: row.get::<_, Option<i64>>(4)?.map(|value| value as u32),
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
            sync_version: row.get::<_, i64>(13)? as u64,
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
        "SELECT id, work_record_id, parent_session_id, root_session_id, capture_source_id, provider, external_session_id, external_agent_id, agent_type, role_hint, is_primary, status, fidelity, transcript_blob_id, started_at_ms, ended_at_ms, created_at_ms, updated_at_ms, visibility, sync_state, sync_version, deleted_at_ms, metadata_json FROM sessions {tail}"
    )
}

fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        work_record_id: parse_optional_uuid(row.get(1)?)?,
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
        "SELECT id, work_record_id, session_id, run_type, status, started_at_ms, ended_at_ms, exit_code, cwd, command_preview, input_blob_id, output_blob_id, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM runs {tail}"
    )
}

fn run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Run> {
    Ok(Run {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        work_record_id: parse_optional_uuid(row.get(1)?)?,
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
        "SELECT id, seq, work_record_id, session_id, run_id, event_type, role, occurred_at_ms, capture_source_id, payload_json, payload_blob_id, dedupe_key, visibility, redaction_state, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM events {tail}"
    )
}

fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        seq: row.get::<_, i64>(1)? as u64,
        work_record_id: parse_optional_uuid(row.get(2)?)?,
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
        byte_size: row.get::<_, i64>(4)? as u64,
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
        kind: parse_text_enum::<work_record_core::VcsKind>(row.get::<_, String>(1)?)?,
        root_path: row.get(2)?,
        repo_fingerprint: row.get(3)?,
        primary_remote_url_normalized: row.get(4)?,
        host: parse_text_enum::<work_record_core::VcsHost>(row.get::<_, String>(5)?)?,
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
        kind: parse_text_enum::<work_record_core::VcsChangeKind>(row.get::<_, String>(2)?)?,
        change_id: row.get(3)?,
        parent_change_ids: serde_json::from_str(&row.get::<_, String>(4)?)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?,
        branch_or_bookmark: row.get(5)?,
        tree_hash: row.get(6)?,
        author_time: optional_ms_to_time(row.get(7)?)?,
        confidence: parse_text_enum::<work_record_core::Confidence>(row.get::<_, String>(8)?)?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(9)?)?,
            updated_at: ms_to_time(row.get(10)?)?,
        },
        source_id: parse_optional_uuid(row.get(11)?)?,
        sync: sync_metadata_from_row(row, 12, 13, 14, 15, 16, 17)?,
    })
}

#[cfg(feature = "legacy-pr-evidence")]
fn pull_request_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, vcs_workspace_id, provider, url, number, owner, repo, title, state, head_ref, base_ref, head_sha, confidence, link_source, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM pull_requests {tail}"
    )
}

#[cfg(feature = "legacy-pr-evidence")]
fn pull_request_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PullRequest> {
    Ok(PullRequest {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        vcs_workspace_id: parse_optional_uuid(row.get(1)?)?,
        provider: parse_text_enum::<work_record_core::PullRequestProvider>(
            row.get::<_, String>(2)?,
        )?,
        url: row.get(3)?,
        number: row.get::<_, Option<i64>>(4)?.map(|value| value as u64),
        owner: row.get(5)?,
        repo: row.get(6)?,
        title: row.get(7)?,
        state: row.get(8)?,
        head_ref: row.get(9)?,
        base_ref: row.get(10)?,
        head_sha: row.get(11)?,
        confidence: parse_text_enum::<work_record_core::Confidence>(row.get::<_, String>(12)?)?,
        link_source: parse_text_enum::<work_record_core::PullRequestLinkSource>(
            row.get::<_, String>(13)?,
        )?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(14)?)?,
            updated_at: ms_to_time(row.get(15)?)?,
        },
        source_id: parse_optional_uuid(row.get(16)?)?,
        sync: sync_metadata_from_row(row, 17, 18, 19, 20, 21, 22)?,
    })
}

fn summary_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, work_record_id, session_id, kind, model_or_source, text, citations_json, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM summaries {tail}"
    )
}

fn summary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Summary> {
    Ok(Summary {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        work_record_id: parse_optional_uuid(row.get(1)?)?,
        session_id: parse_optional_uuid(row.get(2)?)?,
        kind: parse_text_enum::<work_record_core::SummaryKind>(row.get::<_, String>(3)?)?,
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
        "SELECT id, work_record_id, run_id, event_id, vcs_workspace_id, path, change_kind, old_path, line_count_delta, confidence, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM files_touched {tail}"
    )
}

fn file_touched_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileTouched> {
    Ok(FileTouched {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        work_record_id: parse_optional_uuid(row.get(1)?)?,
        run_id: parse_optional_uuid(row.get(2)?)?,
        event_id: parse_optional_uuid(row.get(3)?)?,
        vcs_workspace_id: parse_optional_uuid(row.get(4)?)?,
        path: row.get(5)?,
        change_kind: row
            .get::<_, Option<String>>(6)?
            .map(parse_text_enum::<work_record_core::FileChangeKind>)
            .transpose()?,
        old_path: row.get(7)?,
        line_count_delta: row.get(8)?,
        confidence: parse_text_enum::<work_record_core::Confidence>(row.get::<_, String>(9)?)?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(10)?)?,
            updated_at: ms_to_time(row.get(11)?)?,
        },
        source_id: parse_optional_uuid(row.get(12)?)?,
        sync: sync_metadata_from_row(row, 13, 14, 15, 16, 17, 18)?,
    })
}

fn work_record_link_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, work_record_id, target_type, target_id, link_type, confidence, source_id, created_at_ms, updated_at_ms, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM work_record_links {tail}"
    )
}

fn work_record_link_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkRecordLink> {
    Ok(WorkRecordLink {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        work_record_id: parse_uuid(row.get::<_, String>(1)?)?,
        target_type: parse_text_enum::<work_record_core::WorkRecordLinkTargetType>(
            row.get::<_, String>(2)?,
        )?,
        target_id: parse_uuid(row.get::<_, String>(3)?)?,
        link_type: parse_text_enum::<work_record_core::WorkRecordLinkType>(
            row.get::<_, String>(4)?,
        )?,
        confidence: parse_text_enum::<work_record_core::Confidence>(row.get::<_, String>(5)?)?,
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
        sync_version: row.get::<_, i64>(sync_version_index)? as u64,
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
        "SELECT id, title, body, tags_json, kind, workspace, created_at, updated_at FROM work_records {tail}"
    )
}

#[cfg(feature = "legacy-pr-evidence")]
fn evidence_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, COALESCE(record_id, work_record_id), command, exit_code, stdout, stderr, started_at, duration_ms FROM evidence {tail}"
    )
}

#[cfg(feature = "legacy-pr-evidence")]
fn evidence_metadata_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, work_record_id, vcs_change_id, kind, status, freshness, command_run_id, artifact_id, observed_tree_hash, observed_head_sha, started_at_ms, ended_at_ms, stale_reason, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM evidence {tail}"
    )
}

fn record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkRecord> {
    let tags_json: String = row.get(3)?;
    Ok(WorkRecord {
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

#[cfg(feature = "legacy-pr-evidence")]
fn evidence_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Evidence> {
    let record_id: Option<String> = row.get(1)?;
    Ok(Evidence {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        record_id: record_id
            .map(parse_uuid)
            .transpose()
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?,
        command: row.get(2)?,
        exit_code: row.get(3)?,
        stdout: row.get(4)?,
        stderr: row.get(5)?,
        started_at: parse_time(row.get::<_, String>(6)?)?,
        duration_ms: row.get(7)?,
    })
}

#[cfg(feature = "legacy-pr-evidence")]
fn evidence_metadata_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EvidenceMetadata> {
    let vcs_change_id: Option<String> = row.get(2)?;
    let command_run_id: Option<String> = row.get(6)?;
    let artifact_id: Option<String> = row.get(7)?;
    let source_id: Option<String> = row.get(15)?;
    let metadata_json: String = row.get(21)?;
    Ok(EvidenceMetadata {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        work_record_id: parse_uuid(row.get::<_, String>(1)?)?,
        vcs_change_id: vcs_change_id
            .map(parse_uuid)
            .transpose()
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?,
        kind: parse_text_enum::<EvidenceKind>(row.get::<_, String>(3)?)?,
        status: parse_text_enum::<EvidenceStatus>(row.get::<_, String>(4)?)?,
        freshness: parse_text_enum::<EvidenceFreshness>(row.get::<_, String>(5)?)?,
        command_run_id: command_run_id
            .map(parse_uuid)
            .transpose()
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?,
        artifact_id: artifact_id
            .map(parse_uuid)
            .transpose()
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?,
        observed_tree_hash: row.get(8)?,
        observed_head_sha: row.get(9)?,
        started_at: optional_time_ms(row.get(10)?),
        ended_at: optional_time_ms(row.get(11)?),
        stale_reason: row.get(12)?,
        timestamps: EntityTimestamps {
            created_at: time_ms(row.get(13)?),
            updated_at: time_ms(row.get(14)?),
        },
        source_id: source_id
            .map(parse_uuid)
            .transpose()
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?,
        sync: SyncMetadata {
            visibility: parse_text_enum::<Visibility>(row.get::<_, String>(16)?)?,
            fidelity: parse_text_enum::<Fidelity>(row.get::<_, String>(17)?)?,
            sync_state: parse_text_enum::<SyncState>(row.get::<_, String>(18)?)?,
            sync_version: row.get::<_, i64>(19)? as u64,
            deleted_at: optional_time_ms(row.get(20)?),
            metadata: serde_json::from_str(&metadata_json)
                .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?,
        },
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
            .prefix("work-record-store-search-order-")
            .tempdir_in(root)
            .unwrap()
    }

    fn fixed_time() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-23T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn stable_tie_record(index: u16) -> WorkRecord {
        let mut record = WorkRecord::new(
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
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    fn tempdir() -> tempfile::TempDir {
        let root = std::env::current_dir().unwrap().join("target/test-data");
        fs::create_dir_all(&root).unwrap();
        tempfile::Builder::new()
            .prefix("work-record-store-catalog-")
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
            work_record_id: None,
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
                    event_count: 3,
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
                    event_count: 3,
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
    fn catalog_upsert_preserves_index_state_until_file_changes() {
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
                    event_count: 3,
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
        let (status, indexed_size, indexed_mtime, indexed_event_count): (
            String,
            Option<i64>,
            Option<i64>,
            Option<i64>,
        ) = store
            .conn
            .query_row(
                "SELECT indexed_status, indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_event_count FROM catalog_sessions WHERE source_path = ?1",
                [source_path],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(status, CatalogIndexedStatus::Pending.as_str());
        assert_eq!(indexed_size, None);
        assert_eq!(indexed_mtime, None);
        assert_eq!(indexed_event_count, None);
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
    }

    #[test]
    fn provider_check_constraints_accept_search_only_providers() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        rebuild_capture_sources_provider_check(&store.conn).unwrap();
        rebuild_catalog_sessions_provider_check(&store.conn).unwrap();

        let schema = store.schema().unwrap();
        for provider in ["copilot_cli", "factory_ai_droid", "amp"] {
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
                    VALUES (?1, ?2, 'normalized_provider_jsonl', '/tmp/provider', 'primary', 1, 0, 0)
                    "#,
                    params![format!("/tmp/provider/{provider}.jsonl"), provider],
                )
                .unwrap();
        }

        let source_count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM capture_sources WHERE provider IN ('copilot_cli', 'factory_ai_droid', 'amp')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let catalog_count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM catalog_sessions WHERE provider IN ('copilot_cli', 'factory_ai_droid', 'amp')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source_count, 3);
        assert_eq!(catalog_count, 3);
    }
}

#[cfg(all(test, feature = "legacy-pr-evidence"))]
mod tests {
    use super::*;
    use work_record_core::{
        Confidence, FileChangeKind, PullRequestLinkSource, PullRequestProvider, SummaryKind,
        VcsChangeKind, VcsHost, VcsKind, WorkRecordLinkTargetType, WorkRecordLinkType,
    };

    fn tempdir() -> tempfile::TempDir {
        let root = std::env::current_dir().unwrap().join("target/test-data");
        fs::create_dir_all(&root).unwrap();
        tempfile::Builder::new()
            .prefix("work-record-store-")
            .tempdir_in(root)
            .unwrap()
    }

    fn sqlite_names(store: &Store, object_type: &str) -> Vec<String> {
        let mut stmt = store
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type = ?1 ORDER BY name")
            .unwrap();
        let rows = stmt
            .query_map(params![object_type], |row| row.get::<_, String>(0))
            .unwrap();
        let mut names = Vec::new();
        for row in rows {
            names.push(row.unwrap());
        }
        names
    }

    fn assert_contains_name(names: &[String], required: &str) {
        assert!(
            names.iter().any(|name| name == required),
            "missing sqlite object {required}"
        );
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
            metadata: serde_json::json!({"import_cursor": "cursor-1"}),
        }
    }

    fn temp_store() -> (tempfile::TempDir, Store) {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        (temp, store)
    }

    #[test]
    fn store_open_creates_flat_objects_and_spool_layout() {
        let temp = tempdir();
        let db_path = temp.path().join("work.sqlite");
        let store = Store::open(&db_path).unwrap();

        assert_eq!(store.path(), db_path.as_path());
        assert!(temp.path().join(OBJECTS_DIR).is_dir());
        assert!(temp.path().join(SPOOL_DIR).is_dir());
        assert!(!temp.path().join(LEGACY_WORK_RECORD_DIR).exists());
    }

    #[test]
    fn store_open_migrates_old_work_record_layout_to_flat_root() {
        let temp = tempdir();
        let legacy = temp.path().join(LEGACY_WORK_RECORD_DIR);
        fs::create_dir_all(&legacy).unwrap();

        let legacy_store = Store::open(legacy.join("work.sqlite")).unwrap();
        let record = WorkRecord::new("Legacy", "body", Vec::new(), "task", None);
        legacy_store.insert_record(&record).unwrap();
        let stdout = "legacy object content";
        let evidence = Evidence::new(
            Some(record.id),
            "cargo test",
            0,
            stdout.into(),
            String::new(),
            fixed_time(),
            1,
        );
        legacy_store.insert_evidence(&evidence).unwrap();
        let old_object_path: String = legacy_store
            .conn
            .query_row("SELECT blob_path FROM artifacts LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let old_blob_path = old_object_path.replacen("objects/", "blobs/", 1);
        legacy_store
            .conn
            .execute(
                "UPDATE artifacts SET blob_path = ?1",
                params![old_blob_path],
            )
            .unwrap();
        drop(legacy_store);

        fs::rename(legacy.join(OBJECTS_DIR), legacy.join(LEGACY_BLOBS_DIR)).unwrap();
        fs::rename(legacy.join(SPOOL_DIR), legacy.join(LEGACY_INBOX_DIR)).unwrap();
        fs::write(
            legacy.join(LEGACY_INBOX_DIR).join("capture-old.jsonl"),
            "{}\n",
        )
        .unwrap();

        let store = Store::open(temp.path().join("work.sqlite")).unwrap();

        assert!(temp.path().join("work.sqlite").is_file());
        assert!(temp.path().join(OBJECTS_DIR).is_dir());
        assert!(temp.path().join(SPOOL_DIR).is_dir());
        assert!(temp
            .path()
            .join(SPOOL_DIR)
            .join("capture-old.jsonl")
            .is_file());
        assert!(!legacy.exists());

        let migrated_blob_path: String = store
            .conn
            .query_row("SELECT blob_path FROM artifacts LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(migrated_blob_path.starts_with("objects/"));
        assert_eq!(
            fs::read_to_string(temp.path().join(&migrated_blob_path)).unwrap(),
            "legacy object content"
        );

        let archive = store.export_archive().unwrap();
        assert!(archive
            .artifacts
            .iter()
            .any(|artifact| artifact.content == "legacy object content"
                && artifact.blob_path.starts_with("objects/")));
    }

    #[test]
    fn store_open_leaves_old_layout_when_flat_destination_exists() {
        let temp = tempdir();
        let legacy = temp.path().join(LEGACY_WORK_RECORD_DIR);
        fs::create_dir_all(legacy.join(LEGACY_BLOBS_DIR)).unwrap();
        Connection::open(legacy.join("work.sqlite")).unwrap();
        Connection::open(temp.path().join("work.sqlite")).unwrap();

        let _store = Store::open(temp.path().join("work.sqlite")).unwrap();

        assert!(legacy.join("work.sqlite").is_file());
        assert!(legacy.join(LEGACY_BLOBS_DIR).is_dir());
        assert!(temp.path().join(OBJECTS_DIR).is_dir());
        assert!(temp.path().join(SPOOL_DIR).is_dir());
    }

    #[test]
    fn old_ade_state_dirs_are_ignored_by_flat_layout_open() {
        let temp = tempdir();
        for name in ["agents", "sessions", "tasks", "runs"] {
            let dir = temp.path().join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("ade-state.txt"), name).unwrap();
        }

        let _store = Store::open(temp.path().join("work.sqlite")).unwrap();

        assert!(temp.path().join("work.sqlite").is_file());
        assert!(temp.path().join(OBJECTS_DIR).is_dir());
        assert!(temp.path().join(SPOOL_DIR).is_dir());
        assert!(!temp.path().join(LEGACY_WORK_RECORD_DIR).exists());
        for name in ["agents", "sessions", "tasks", "runs"] {
            assert_eq!(
                fs::read_to_string(temp.path().join(name).join("ade-state.txt")).unwrap(),
                name
            );
        }
    }

    fn test_record() -> WorkRecord {
        WorkRecord::new(
            "Fresh evidence",
            "body",
            vec!["evidence".to_owned()],
            "task",
            Some("workspace".to_owned()),
        )
    }

    fn archive_with_stdout_artifact(content: &str) -> WorkRecordArchive {
        let record = WorkRecord::new("Archive security", "body", Vec::new(), "task", None);
        let evidence = Evidence::new(
            Some(record.id),
            "cargo test",
            0,
            String::new(),
            String::new(),
            fixed_time(),
            1,
        );
        let hash = sha256_hex(content.as_bytes());
        let blob_path = object_relative_path(&hash);
        let artifact = WorkRecordArchiveArtifact {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-00000000a001").unwrap(),
            evidence_id: evidence.id,
            stream: "stdout".into(),
            kind: ArtifactKind::Stdout,
            blob_hash: hash,
            blob_path,
            byte_size: content.len() as u64,
            media_type: Some("text/plain".into()),
            preview_text: None,
            redaction_state: RedactionState::SafePreview,
            content: content.into(),
        };

        WorkRecordArchive {
            schema_version: 1,
            version: 1,
            records: vec![record],
            evidence: vec![evidence],
            artifacts: vec![artifact],
            ..WorkRecordArchive::default()
        }
    }

    fn event_for_record(record_id: Uuid, seq: u64, text: &str) -> Event {
        provider_event_for_record(record_id, seq, 7, "payload-hash", text)
    }

    fn provider_event_for_record(
        record_id: Uuid,
        seq: u64,
        provider_index: u64,
        payload_hash: &str,
        text: &str,
    ) -> Event {
        Event {
            id: new_id(),
            seq,
            work_record_id: Some(record_id),
            session_id: None,
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::Assistant),
            occurred_at: fixed_time(),
            capture_source_id: None,
            payload: serde_json::json!({ "text": text }),
            payload_blob_id: None,
            dedupe_key: Some(Store::provider_event_dedupe_key(
                CaptureProvider::Codex,
                "shared-session",
                provider_index,
                payload_hash,
            )),
            redaction_state: RedactionState::SafePreview,
            sync: sync_metadata(),
        }
    }

    fn artifact_record_from_archive_artifact(artifact: &WorkRecordArchiveArtifact) -> Artifact {
        Artifact {
            id: artifact.id,
            kind: artifact.kind,
            blob_hash: artifact.blob_hash.clone(),
            blob_path: artifact.blob_path.clone(),
            byte_size: artifact.byte_size,
            media_type: artifact.media_type.clone(),
            preview_text: artifact.preview_text.clone(),
            redaction_state: artifact.redaction_state,
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        }
    }

    #[test]
    fn evidence_metadata_defaults_to_unbound_for_legacy_command_rows() {
        let (_temp, store) = temp_store();
        let record = test_record();
        store.insert_record(&record).unwrap();
        let evidence = Evidence::new(
            Some(record.id),
            "cargo test",
            0,
            "ok".to_owned(),
            String::new(),
            fixed_time(),
            42,
        );
        store.insert_evidence(&evidence).unwrap();

        let metadata = store.get_evidence_metadata(evidence.id).unwrap();
        assert_eq!(metadata.work_record_id, record.id);
        assert_eq!(metadata.kind, EvidenceKind::Manual);
        assert_eq!(metadata.status, EvidenceStatus::Passed);
        assert_eq!(metadata.freshness, EvidenceFreshness::Unbound);
        assert_eq!(
            classify_evidence_freshness(&metadata, Some("head"), Some("tree"), false),
            EvidenceFreshness::Unbound
        );
    }

    #[test]
    fn evidence_metadata_update_projects_fresh_vcs_binding() {
        let (_temp, store) = temp_store();
        let record = test_record();
        store.insert_record(&record).unwrap();
        let evidence = Evidence::new(
            Some(record.id),
            "cargo test",
            0,
            String::new(),
            String::new(),
            fixed_time(),
            7,
        );
        store.insert_evidence(&evidence).unwrap();

        let workspace = git_workspace(new_id());
        let workspace_id = store.upsert_vcs_workspace(&workspace).unwrap();
        let change = VcsChange {
            id: new_id(),
            vcs_workspace_id: workspace_id,
            kind: VcsChangeKind::GitCommit,
            change_id: "abc123".to_owned(),
            parent_change_ids: Vec::new(),
            branch_or_bookmark: Some("main".to_owned()),
            tree_hash: Some("tree123".to_owned()),
            author_time: None,
            confidence: Confidence::Explicit,
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        };
        let change_id = store.upsert_vcs_change(&change).unwrap();
        let mut metadata = store.get_evidence_metadata(evidence.id).unwrap();
        metadata.vcs_change_id = Some(change_id);
        metadata.kind = EvidenceKind::Test;
        metadata.freshness = EvidenceFreshness::Fresh;
        metadata.observed_head_sha = Some("abc123".to_owned());
        metadata.observed_tree_hash = Some("tree123".to_owned());
        metadata.sync.metadata = serde_json::json!({
            "repo_root": "[REDACTED_ROOT]/ctx",
            "branch_or_bookmark": "main",
            "dirty": false,
        });
        store.update_evidence_metadata(&metadata).unwrap();

        let projected = store.evidence_metadata_for_record(record.id).unwrap();
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].vcs_change_id, Some(change_id));
        assert_eq!(projected[0].kind, EvidenceKind::Test);
        assert_eq!(projected[0].freshness, EvidenceFreshness::Fresh);
        assert_eq!(
            projected[0].sync.metadata["dirty"],
            serde_json::json!(false)
        );
        assert_eq!(
            classify_evidence_freshness(&projected[0], Some("abc123"), Some("tree123"), false),
            EvidenceFreshness::Fresh
        );
        assert_eq!(
            classify_evidence_freshness(&projected[0], Some("def456"), Some("tree123"), false),
            EvidenceFreshness::Stale
        );
        assert_eq!(
            classify_evidence_freshness(&projected[0], Some("abc123"), Some("tree123"), true),
            EvidenceFreshness::ProbablyFresh
        );
    }

    #[test]
    fn archive_round_trip_preserves_evidence_metadata_when_present() {
        let (_source_temp, source) = temp_store();
        let record = test_record();
        source.insert_record(&record).unwrap();
        let evidence = Evidence::new(
            Some(record.id),
            "cargo test",
            0,
            String::new(),
            String::new(),
            fixed_time(),
            7,
        );
        source.insert_evidence(&evidence).unwrap();
        let mut metadata = source.get_evidence_metadata(evidence.id).unwrap();
        metadata.kind = EvidenceKind::Test;
        metadata.freshness = EvidenceFreshness::ProbablyFresh;
        metadata.observed_head_sha = Some("abc123".to_owned());
        metadata.sync.metadata = serde_json::json!({
            "repo_root": "[REDACTED_ROOT]/ctx",
            "dirty": true,
        });
        source.update_evidence_metadata(&metadata).unwrap();

        let archive = source.export_archive().unwrap();
        assert_eq!(archive.evidence_metadata.len(), 1);

        let (_dest_temp, mut dest) = temp_store();
        dest.import_archive(&archive, false).unwrap();
        let imported = dest.get_evidence_metadata(evidence.id).unwrap();
        assert_eq!(imported.kind, EvidenceKind::Test);
        assert_eq!(imported.freshness, EvidenceFreshness::ProbablyFresh);
        assert_eq!(imported.observed_head_sha.as_deref(), Some("abc123"));
        assert_eq!(
            imported.sync.metadata["repo_root"],
            serde_json::json!("[REDACTED_ROOT]/ctx")
        );
    }

    #[test]
    fn local_device_and_workspace_identity_are_stable_and_redacted() {
        let temp = tempdir();
        let db_path = temp.path().join("work.sqlite");
        let store = Store::open(&db_path).unwrap();
        let first_device = store.get_or_create_local_device().unwrap();
        let second_device = store.get_or_create_local_device().unwrap();
        assert_eq!(first_device.id, second_device.id);
        assert_eq!(
            first_device.stable_device_id,
            second_device.stable_device_id
        );

        let workspace = store
            .register_local_workspace(temp.path().join("private/repo-name"), "fingerprint-1", None)
            .unwrap();
        let same_workspace = store
            .register_local_workspace(temp.path().join("private/repo-name"), "fingerprint-1", None)
            .unwrap();
        assert_eq!(workspace.id, same_workspace.id);
        assert_ne!(
            workspace.root_path_hash,
            temp.path().join("private/repo-name").display().to_string()
        );
        assert_eq!(workspace.display_root, "[REDACTED_ROOT]/repo-name");
    }

    fn codex_source_descriptor(external_session_id: &str) -> CaptureSourceDescriptor {
        CaptureSourceDescriptor {
            kind: work_record_core::CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Codex,
            machine_id: "machine-1".into(),
            process_id: Some(42),
            cwd: Some("/repo".into()),
            raw_source_path: Some("/sessions/codex.jsonl".into()),
            external_session_id: Some(external_session_id.into()),
        }
    }

    fn imported_source(id: Uuid, external_session_id: &str) -> CaptureSource {
        CaptureSource {
            id,
            descriptor: codex_source_descriptor(external_session_id),
            started_at: fixed_time(),
            ended_at: None,
            sync: sync_metadata(),
        }
    }

    fn git_workspace(id: Uuid) -> VcsWorkspace {
        VcsWorkspace {
            id,
            kind: VcsKind::Git,
            root_path: "/repo".into(),
            repo_fingerprint: "git:repo".into(),
            primary_remote_url_normalized: Some("https://github.com/ctxrs/ctx".into()),
            host: VcsHost::Github,
            owner: Some("ctxrs".into()),
            name: Some("ctx".into()),
            monorepo_subpath: None,
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        }
    }

    fn github_pr(id: Uuid, workspace_id: Option<Uuid>) -> PullRequest {
        PullRequest {
            id,
            vcs_workspace_id: workspace_id,
            provider: PullRequestProvider::Github,
            url: "https://github.com/ctxrs/ctx/pull/123".into(),
            number: Some(123),
            owner: Some("ctxrs".into()),
            repo: Some("ctx".into()),
            title: Some("Rich storage".into()),
            state: Some("open".into()),
            head_ref: Some("ctx/wr-finish-store-search".into()),
            base_ref: Some("main".into()),
            head_sha: Some("abcdef".into()),
            confidence: Confidence::Explicit,
            link_source: PullRequestLinkSource::Explicit,
            timestamps: timestamps(),
            source_id: None,
            sync: sync_metadata(),
        }
    }

    #[test]
    fn migration_creates_foundation_schema_and_indexes() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let user_version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(user_version, SCHEMA_VERSION);

        let tables = sqlite_names(&store, "table");
        for table in [
            "capture_sources",
            "work_records",
            "sessions",
            "session_edges",
            "runs",
            "events",
            "vcs_workspaces",
            "vcs_changes",
            "pull_requests",
            "work_record_links",
            "artifacts",
            "evidence",
            "evidence_artifacts",
            "summaries",
            "files_touched",
            "tags",
            "work_record_tags",
            "record_edges",
            "sync_aliases",
            "sync_cursors",
            "sync_batches",
            "sync_outbox",
            "audit_log",
        ] {
            assert_contains_name(&tables, table);
        }

        let fts5_enabled: i64 = store
            .conn
            .query_row(
                "SELECT sqlite_compileoption_used('ENABLE_FTS5')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        if fts5_enabled == 1 {
            for table in ["work_record_search", "event_search", "artifact_search"] {
                assert_contains_name(&tables, table);
            }
        }

        let indexes = sqlite_names(&store, "index");
        for index in [
            "idx_events_seq",
            "idx_events_work_record_occurred_at_ms",
            "idx_events_session_occurred_at_ms",
            "idx_sessions_work_record_id",
            "idx_sessions_root_session_id",
            "idx_runs_work_record_started_at_ms",
            "idx_work_records_last_activity_at_ms",
            "idx_vcs_workspaces_kind_repo_fingerprint",
            "idx_pull_requests_provider_owner_repo_number",
            "idx_sync_outbox_sync_state_updated_at_ms",
            "idx_evidence_work_record_id",
            "idx_evidence_record_id",
            "idx_evidence_artifacts_evidence_id",
        ] {
            assert_contains_name(&indexes, index);
        }

        let schema = store.schema().unwrap();
        assert!(schema.contains("CHECK (visibility IN"));
        assert!(schema.contains("CHECK (fidelity IN"));
        assert!(schema.contains("CHECK (sync_state IN"));
    }

    #[test]
    fn open_configures_wal_busy_timeout_and_foreign_keys() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let journal_mode: String = store
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        let foreign_keys: i64 = store
            .conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        let busy_timeout: i64 = store
            .conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();

        assert_eq!(journal_mode, "wal");
        assert_eq!(foreign_keys, 1);
        assert_eq!(busy_timeout, 5_000);
    }

    #[test]
    fn evidence_output_is_stored_as_artifact_with_safe_preview() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let record = WorkRecord::new("Output", "blob evidence", Vec::new(), "task", None);
        store.insert_record(&record).unwrap();
        let evidence = Evidence::new(
            Some(record.id),
            "cargo test authorization: Bearer abcdef1234567890",
            0,
            "ok token=secret password=hunter2 AKIA1234567890ABCDEF".into(),
            "secret=shhhhhh bearer abcdef1234567890 ghp_1234567890abcdef".into(),
            Utc::now(),
            1,
        );

        store.insert_evidence(&evidence).unwrap();

        let (artifact_id, stdout_preview, stderr_preview): (String, String, String) = store
            .conn
            .query_row(
                "SELECT artifact_id, stdout, stderr FROM evidence WHERE id = ?1",
                params![evidence.id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert!(!artifact_id.is_empty());
        assert_eq!(
            stdout_preview,
            "ok token=[REDACTED_SECRET] password=[REDACTED_SECRET] [REDACTED_SECRET]"
        );
        assert_eq!(
            stderr_preview,
            "secret=[REDACTED_SECRET] bearer [REDACTED_SECRET] [REDACTED_SECRET]"
        );

        let artifact_count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM artifacts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(artifact_count, 2);

        let evidence_artifact_count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM evidence_artifacts WHERE evidence_id = ?1",
                params![evidence.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(evidence_artifact_count, 2);

        let redaction_states: Vec<String> = store
            .conn
            .prepare("SELECT redaction_state FROM artifacts ORDER BY kind")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(redaction_states, vec!["safe_preview", "safe_preview"]);
    }

    #[cfg(unix)]
    #[test]
    fn evidence_output_rejects_preexisting_symlink_blob_path() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let record = WorkRecord::new("Output", "blob evidence", Vec::new(), "task", None);
        store.insert_record(&record).unwrap();
        let stdout = "preexisting symlink blob content";
        let hash = sha256_hex(stdout.as_bytes());
        let shard = &hash[..2];
        let blob_dir = temp.path().join(OBJECTS_DIR).join(shard);
        fs::create_dir_all(&blob_dir).unwrap();
        let target = temp.path().join("dangling-outside-target");
        std::os::unix::fs::symlink(&target, blob_dir.join(&hash)).unwrap();
        let evidence = Evidence::new(
            Some(record.id),
            "cargo test",
            0,
            stdout.into(),
            String::new(),
            Utc::now(),
            1,
        );

        assert!(matches!(
            store.insert_evidence(&evidence),
            Err(StoreError::ArchiveArtifactNonRegularFile { .. })
        ));
        assert!(store.evidence_for_record(record.id).unwrap().is_empty());
    }

    #[test]
    fn fts_projection_does_not_index_legacy_evidence_text() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let record = WorkRecord::new(
            "Plain title",
            "ordinary body",
            vec!["plain".into()],
            "task",
            None,
        );
        store.insert_record(&record).unwrap();
        let evidence = Evidence::new(
            Some(record.id),
            "cargo test --package recorder",
            1,
            "failed with needle-only-output password=hunter2".into(),
            String::new(),
            Utc::now(),
            1,
        );
        store.insert_evidence(&evidence).unwrap();

        let records = store.search_records("needle-only-output", 10).unwrap();
        assert!(records.is_empty());

        if table_exists(&store.conn, "work_record_search").unwrap() {
            let evidence_text: String = store
                .conn
                .query_row(
                    "SELECT evidence_text FROM work_record_search WHERE record_id = ?1",
                    params![record.id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(evidence_text.is_empty());
        }
    }

    #[test]
    fn fts_projection_indexes_nested_provider_event_previews() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let record = WorkRecord::new(
            "Provider event search",
            "ordinary body",
            Vec::new(),
            "task",
            None,
        );
        store.insert_record(&record).unwrap();
        let mut event = event_for_record(record.id, 1, "unused");
        event.event_type = EventType::ToolCall;
        event.payload = serde_json::json!({
            "provider": "codex",
            "body": {
                "tool": "shell",
                "name": "exec_command",
                "arguments_preview": "nested-store-needle token=secretvalue",
                "arguments": "unsafe-store-secret password=hunter2"
            }
        });
        event.dedupe_key = Some("nested-store-event".into());
        store.upsert_event(&event).unwrap();
        store.upsert_record(&record).unwrap();

        let records = store.search_records("nested-store-needle", 10).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, record.id);

        if table_exists(&store.conn, "event_search").unwrap() {
            let preview: String = store
                .conn
                .query_row(
                    "SELECT safe_preview_text FROM event_search WHERE event_id = ?1",
                    params![event.id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(
                preview.contains("arguments_preview: nested-store-needle token=[REDACTED_SECRET]")
            );
            assert!(!preview.contains("unsafe-store-secret"));
            assert!(!preview.contains("hunter2"));
        }
    }

    #[test]
    fn opening_second_store_does_not_recreate_fts_tables_under_readers() {
        let temp = tempdir();
        let path = temp.path().join("work.sqlite");
        let store = Store::open(&path).unwrap();
        if !table_exists(&store.conn, "work_record_search").unwrap() {
            return;
        }

        let record = WorkRecord::new(
            "Concurrent dashboard",
            "dashboard setup keeps a second connection open",
            vec!["dashboard".into()],
            "task",
            None,
        );
        store.insert_record(&record).unwrap();
        store
            .conn
            .execute(
                "UPDATE work_record_search SET title = 'projection-open-sentinel' WHERE record_id = ?1",
                params![record.id.to_string()],
            )
            .unwrap();

        let mut stmt = store
            .conn
            .prepare("SELECT COUNT(*) FROM work_record_search")
            .unwrap();
        let second = Store::open(&path).unwrap();
        drop(second);

        let count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
        drop(stmt);
        let title: String = store
            .conn
            .query_row(
                "SELECT title FROM work_record_search WHERE record_id = ?1",
                params![record.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(title, "projection-open-sentinel");
    }

    #[test]
    fn opening_store_does_not_rebuild_partial_search_projection() {
        let temp = tempdir();
        let path = temp.path().join("work.sqlite");
        let store = Store::open(&path).unwrap();
        if !table_exists(&store.conn, "event_search").unwrap() {
            return;
        }

        let record = WorkRecord::new(
            "Partial projection",
            "read commands should not repair large search indexes",
            Vec::new(),
            "task",
            None,
        );
        store.insert_record(&record).unwrap();
        let mut event = event_for_record(record.id, 1, "partial-event-needle");
        event.event_type = EventType::ToolCall;
        event.payload = serde_json::json!({
            "provider": "codex",
            "body": {
                "tool": "shell",
                "name": "exec_command",
                "arguments_preview": "partial-event-needle"
            }
        });
        event.dedupe_key = Some("partial-projection-event".into());
        store.upsert_event(&event).unwrap();
        store.upsert_record(&record).unwrap();
        store
            .conn
            .execute("DELETE FROM work_record_search", [])
            .unwrap();
        assert_eq!(
            store
                .conn
                .query_row("SELECT COUNT(*) FROM event_search", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            1
        );

        let reopened = Store::open(&path).unwrap();
        let work_rows: i64 = reopened
            .conn
            .query_row("SELECT COUNT(*) FROM work_record_search", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(work_rows, 0);
        let records = reopened.search_records("partial-event-needle", 1).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, record.id);
    }

    #[test]
    fn migration_preserves_seeded_legacy_database() {
        let temp = tempdir();
        let path = temp.path().join("legacy.sqlite");
        let record_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000001").unwrap();
        let evidence_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000002").unwrap();

        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                r#"
                PRAGMA foreign_keys = ON;
                CREATE TABLE work_records (
                    id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    body TEXT NOT NULL,
                    tags_json TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    workspace TEXT,
                    pr_url TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE TABLE evidence (
                    id TEXT PRIMARY KEY,
                    record_id TEXT REFERENCES work_records(id) ON DELETE SET NULL,
                    command TEXT NOT NULL,
                    exit_code INTEGER NOT NULL,
                    stdout TEXT NOT NULL,
                    stderr TEXT NOT NULL,
                    started_at TEXT NOT NULL,
                    duration_ms INTEGER NOT NULL
                );
                "#,
            )
            .unwrap();
            conn.execute(
                r#"
                INSERT INTO work_records
                (id, title, body, tags_json, kind, workspace, pr_url, created_at, updated_at)
                VALUES (?1, 'Legacy import', 'old body', '["legacy"]', 'task', 'ctx', NULL, '2026-01-01T00:00:00Z', '2026-01-01T00:01:00Z')
                "#,
                params![record_id.to_string()],
            )
            .unwrap();
            conn.execute(
                r#"
                INSERT INTO evidence
                (id, record_id, command, exit_code, stdout, stderr, started_at, duration_ms)
                VALUES (?1, ?2, 'cargo test', 0, 'ok', '', '2026-01-01T00:02:00Z', 1000)
                "#,
                params![evidence_id.to_string(), record_id.to_string()],
            )
            .unwrap();
        }

        let store = Store::open(&path).unwrap();
        assert_eq!(store.get_record(record_id).unwrap().title, "Legacy import");
        assert_eq!(
            store.evidence_for_record(record_id).unwrap()[0].id,
            evidence_id
        );

        let (summary, created_at_ms, last_activity_at_ms): (String, i64, i64) = store
            .conn
            .query_row(
                "SELECT summary, created_at_ms, last_activity_at_ms FROM work_records WHERE id = ?1",
                params![record_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(summary, "old body");
        assert!(created_at_ms > 0);
        assert!(last_activity_at_ms >= created_at_ms);

        let (work_record_id, status): (String, String) = store
            .conn
            .query_row(
                "SELECT work_record_id, status FROM evidence WHERE id = ?1",
                params![evidence_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(work_record_id, record_id.to_string());
        assert_eq!(status, "passed");
        let backfilled_artifact_count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM evidence_artifacts WHERE evidence_id = ?1",
                params![evidence_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(backfilled_artifact_count, 1);
        assert!(store.validate().unwrap().is_empty());
    }

    #[test]
    fn migration_upgrades_existing_v1_mvp_store_with_rich_schema() {
        let temp = tempdir();
        let path = temp.path().join("legacy-v1.sqlite");
        let record_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000011").unwrap();

        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                r#"
                PRAGMA user_version = 1;
                CREATE TABLE work_records (
                    id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    body TEXT NOT NULL,
                    tags_json TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    workspace TEXT,
                    pr_url TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE TABLE evidence (
                    id TEXT PRIMARY KEY,
                    record_id TEXT REFERENCES work_records(id) ON DELETE SET NULL,
                    command TEXT NOT NULL,
                    exit_code INTEGER NOT NULL,
                    stdout TEXT NOT NULL,
                    stderr TEXT NOT NULL,
                    started_at TEXT NOT NULL,
                    duration_ms INTEGER NOT NULL
                );
                "#,
            )
            .unwrap();
            conn.execute(
                r#"
                INSERT INTO work_records
                (id, title, body, tags_json, kind, workspace, pr_url, created_at, updated_at)
                VALUES (?1, 'Legacy v1', 'old body', '[]', 'task', NULL, NULL, '2026-01-01T00:00:00Z', '2026-01-01T00:01:00Z')
                "#,
                params![record_id.to_string()],
            )
            .unwrap();
        }

        let store = Store::open(&path).unwrap();
        let user_version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(user_version, SCHEMA_VERSION);
        assert_eq!(store.get_record(record_id).unwrap().title, "Legacy v1");
        for table in [
            "capture_sources",
            "sessions",
            "runs",
            "events",
            "files_touched",
            "local_devices",
            "local_workspaces",
        ] {
            assert!(table_exists(&store.conn, table).unwrap(), "{table}");
        }
        let indexes = sqlite_names(&store, "index");
        assert_contains_name(&indexes, "idx_events_dedupe_key");
    }

    #[test]
    fn compatibility_writes_populate_normalized_columns() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let record = WorkRecord::new(
            "Normalize me",
            "body for summary",
            vec!["normalized".into()],
            "task",
            Some("ctx".into()),
        );
        store.insert_record(&record).unwrap();
        let evidence = Evidence::new(
            Some(record.id),
            "cargo clippy",
            1,
            String::new(),
            "failed".into(),
            Utc::now(),
            5,
        );
        store.insert_evidence(&evidence).unwrap();

        let (summary, status, visibility, sync_state, last_activity_at_ms): (
            String,
            String,
            String,
            String,
            i64,
        ) = store
            .conn
            .query_row(
                "SELECT summary, status, visibility, sync_state, last_activity_at_ms FROM work_records WHERE id = ?1",
                params![record.id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();
        assert_eq!(summary, record.body);
        assert_eq!(status, "open");
        assert_eq!(visibility, "local_only");
        assert_eq!(sync_state, "local_only");
        assert!(last_activity_at_ms > 0);

        let (work_record_id, evidence_status, freshness): (String, String, String) = store
            .conn
            .query_row(
                "SELECT work_record_id, status, freshness FROM evidence WHERE id = ?1",
                params![evidence.id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(work_record_id, record.id.to_string());
        assert_eq!(evidence_status, "failed");
        assert_eq!(freshness, "unbound");
    }

    #[test]
    fn rich_storage_upserts_are_idempotent_and_queryable() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let record = WorkRecord::new("Rich task", "provider import", Vec::new(), "task", None);
        store.insert_record(&record).unwrap();

        let source = CaptureSource {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000100").unwrap(),
            descriptor: CaptureSourceDescriptor {
                kind: work_record_core::CaptureSourceKind::ProviderImport,
                provider: CaptureProvider::Codex,
                machine_id: "machine-1".into(),
                process_id: Some(42),
                cwd: Some("/repo".into()),
                raw_source_path: Some("/sessions/codex.jsonl".into()),
                external_session_id: Some("codex-session-1".into()),
            },
            started_at: fixed_time(),
            ended_at: None,
            sync: sync_metadata(),
        };
        store.upsert_capture_source(&source).unwrap();
        store.upsert_capture_source(&source).unwrap();
        assert_eq!(
            store
                .capture_source_by_external_session(CaptureProvider::Codex, "codex-session-1")
                .unwrap()
                .unwrap()
                .id,
            source.id
        );

        let session = Session {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000101").unwrap(),
            work_record_id: Some(record.id),
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: Some(source.id),
            provider: CaptureProvider::Codex,
            external_session_id: Some("codex-session-1".into()),
            external_agent_id: Some("agent-a".into()),
            agent_type: AgentType::Primary,
            role_hint: Some("primary".into()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: fixed_time(),
            ended_at: None,
            timestamps: timestamps(),
            sync: sync_metadata(),
        };
        store.upsert_session(&session).unwrap();
        store.upsert_session(&session).unwrap();
        assert_eq!(store.sessions_for_record(record.id).unwrap().len(), 1);

        let run = Run {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000102").unwrap(),
            work_record_id: Some(record.id),
            session_id: Some(session.id),
            run_type: RunType::Command,
            status: RunStatus::Succeeded,
            started_at: fixed_time(),
            ended_at: Some(fixed_time()),
            exit_code: Some(0),
            cwd: Some("/repo".into()),
            command_preview: Some("cargo test -p work-record-store".into()),
            input_blob_id: None,
            output_blob_id: None,
            timestamps: timestamps(),
            source_id: Some(source.id),
            sync: sync_metadata(),
        };
        store.upsert_run(&run).unwrap();
        store.upsert_run(&run).unwrap();
        assert_eq!(store.runs_for_session(session.id).unwrap().len(), 1);

        let workspace = VcsWorkspace {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000103").unwrap(),
            kind: VcsKind::Git,
            root_path: "/repo".into(),
            repo_fingerprint: "git:repo".into(),
            primary_remote_url_normalized: Some("https://github.com/ctxrs/ctx".into()),
            host: VcsHost::Github,
            owner: Some("ctxrs".into()),
            name: Some("ctx".into()),
            monorepo_subpath: None,
            timestamps: timestamps(),
            source_id: Some(source.id),
            sync: sync_metadata(),
        };
        let workspace_id = store.upsert_vcs_workspace(&workspace).unwrap();
        assert_eq!(workspace_id, workspace.id);
        let second_workspace_id = store
            .upsert_vcs_workspace(&VcsWorkspace {
                id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000999").unwrap(),
                ..workspace.clone()
            })
            .unwrap();
        assert_eq!(second_workspace_id, workspace.id);

        let change = VcsChange {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000104").unwrap(),
            vcs_workspace_id: workspace_id,
            kind: VcsChangeKind::GitCommit,
            change_id: "abcdef".into(),
            parent_change_ids: vec!["parent".into()],
            branch_or_bookmark: Some("ctx/wr-finished-storage-rich".into()),
            tree_hash: Some("tree".into()),
            author_time: Some(fixed_time()),
            confidence: Confidence::Explicit,
            timestamps: timestamps(),
            source_id: Some(source.id),
            sync: sync_metadata(),
        };
        assert_eq!(store.upsert_vcs_change(&change).unwrap(), change.id);
        assert_eq!(store.upsert_vcs_change(&change).unwrap(), change.id);

        let pr = PullRequest {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000105").unwrap(),
            vcs_workspace_id: Some(workspace_id),
            provider: PullRequestProvider::Github,
            url: "https://github.com/ctxrs/ctx/pull/123".into(),
            number: Some(123),
            owner: Some("ctxrs".into()),
            repo: Some("ctx".into()),
            title: Some("Rich storage".into()),
            state: Some("open".into()),
            head_ref: Some("ctx/wr-finished-storage-rich".into()),
            base_ref: Some("main".into()),
            head_sha: Some("abcdef".into()),
            confidence: Confidence::Explicit,
            link_source: PullRequestLinkSource::Explicit,
            timestamps: timestamps(),
            source_id: Some(source.id),
            sync: sync_metadata(),
        };
        assert_eq!(store.upsert_pull_request(&pr).unwrap(), pr.id);
        assert_eq!(store.upsert_pull_request(&pr).unwrap(), pr.id);

        let event = Event {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000109").unwrap(),
            seq: 1,
            work_record_id: Some(record.id),
            session_id: Some(session.id),
            run_id: Some(run.id),
            event_type: EventType::Message,
            role: Some(EventRole::Assistant),
            occurred_at: fixed_time(),
            capture_source_id: Some(source.id),
            payload: serde_json::json!({"text": "rich event"}),
            payload_blob_id: None,
            dedupe_key: Some(Store::provider_event_dedupe_key(
                CaptureProvider::Codex,
                "codex-session-1",
                1,
                "rich-hash",
            )),
            redaction_state: RedactionState::SafePreview,
            sync: sync_metadata(),
        };
        assert_eq!(store.upsert_event(&event).unwrap(), event.id);
        assert_eq!(store.upsert_event(&event).unwrap(), event.id);

        let file = FileTouched {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000106").unwrap(),
            work_record_id: Some(record.id),
            run_id: Some(run.id),
            event_id: Some(event.id),
            vcs_workspace_id: Some(workspace_id),
            path: "crates/work-record-store/src/lib.rs".into(),
            change_kind: Some(FileChangeKind::Modified),
            old_path: None,
            line_count_delta: Some(120),
            confidence: Confidence::Explicit,
            timestamps: timestamps(),
            source_id: Some(source.id),
            sync: sync_metadata(),
        };
        store.upsert_file_touched(&file).unwrap();
        store.upsert_file_touched(&file).unwrap();

        let summary = Summary {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000107").unwrap(),
            work_record_id: Some(record.id),
            session_id: Some(session.id),
            kind: SummaryKind::ImportedProviderSummary,
            model_or_source: Some("codex".into()),
            text: "Implemented rich storage APIs".into(),
            citations: Vec::new(),
            timestamps: timestamps(),
            source_id: Some(source.id),
            sync: sync_metadata(),
        };
        store.upsert_summary(&summary).unwrap();
        store.upsert_summary(&summary).unwrap();

        let link = WorkRecordLink {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000108").unwrap(),
            work_record_id: record.id,
            target_type: WorkRecordLinkTargetType::PullRequest,
            target_id: pr.id,
            link_type: WorkRecordLinkType::PublishedTo,
            confidence: Confidence::Explicit,
            source_id: Some(source.id),
            timestamps: timestamps(),
            sync: sync_metadata(),
        };
        assert_eq!(store.upsert_work_record_link(&link).unwrap(), link.id);
        assert_eq!(store.upsert_work_record_link(&link).unwrap(), link.id);

        for table in [
            "capture_sources",
            "sessions",
            "runs",
            "events",
            "vcs_workspaces",
            "vcs_changes",
            "pull_requests",
            "files_touched",
            "summaries",
            "work_record_links",
        ] {
            let count: i64 = store
                .conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 1, "{table} should be idempotent");
        }

        let archive = store.export_archive().unwrap();
        assert_eq!(archive.schema_version, 2);
        assert_eq!(archive.capture_sources.len(), 1);
        assert_eq!(archive.sessions.len(), 1);
        assert_eq!(archive.runs.len(), 1);
        assert_eq!(archive.events.len(), 1);
        assert_eq!(archive.vcs_workspaces.len(), 1);
        assert_eq!(archive.vcs_changes.len(), 1);
        assert_eq!(archive.pull_requests.len(), 1);
        assert_eq!(archive.files_touched.len(), 1);
        assert_eq!(archive.work_record_links.len(), 1);
        assert_eq!(archive.summaries.len(), 1);

        let mut second = Store::open(temp.path().join("rich-import.sqlite")).unwrap();
        second.import_archive(&archive, false).unwrap();
        assert_eq!(
            second
                .capture_source_by_external_session(CaptureProvider::Codex, "codex-session-1")
                .unwrap()
                .unwrap()
                .id,
            source.id
        );
        assert_eq!(
            second.sessions_for_record(record.id).unwrap()[0].id,
            session.id
        );
        assert_eq!(second.runs_for_session(session.id).unwrap()[0].id, run.id);
        assert_eq!(
            second.events_for_session(session.id).unwrap()[0].id,
            event.id
        );
        assert_eq!(second.list_vcs_changes().unwrap()[0].id, change.id);
        assert_eq!(
            second.pull_requests_for_record(record.id).unwrap()[0].id,
            pr.id
        );
        assert_eq!(
            second.files_touched_for_record(record.id).unwrap()[0].id,
            file.id
        );
        assert_eq!(
            second.summaries_for_record(record.id).unwrap()[0].id,
            summary.id
        );
    }

    #[test]
    fn provider_event_upsert_dedupes_by_provider_session_index_and_hash() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let record = WorkRecord::new("Events", "dedupe", Vec::new(), "task", None);
        store.insert_record(&record).unwrap();
        let dedupe_key =
            Store::provider_event_dedupe_key(CaptureProvider::Codex, "codex-session-1", 7, "hash");
        let event = Event {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000202").unwrap(),
            seq: 7,
            work_record_id: Some(record.id),
            session_id: None,
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::Assistant),
            occurred_at: fixed_time(),
            capture_source_id: None,
            payload: serde_json::json!({
                "text": "hello",
                "provider_index": 7,
                "payload_hash": "hash"
            }),
            payload_blob_id: None,
            dedupe_key: Some(dedupe_key.clone()),
            redaction_state: RedactionState::SafePreview,
            sync: sync_metadata(),
        };

        let first_id = store.upsert_event(&event).unwrap();
        let second_id = store
            .upsert_event(&Event {
                id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000299").unwrap(),
                payload: serde_json::json!({"text": "updated"}),
                ..event.clone()
            })
            .unwrap();
        assert_eq!(first_id, second_id);
        assert_eq!(store.event_id_by_dedupe_key(&dedupe_key).unwrap(), first_id);

        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let stored_payload: String = store
            .conn
            .query_row(
                "SELECT payload_json FROM events WHERE id = ?1",
                params![first_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(stored_payload.contains("hello"));
        assert!(!stored_payload.contains("updated"));
    }

    #[test]
    fn provider_event_upsert_rejects_same_provider_tuple_with_different_hash() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let record = WorkRecord::new("Events", "hash conflict", Vec::new(), "task", None);
        store.insert_record(&record).unwrap();
        let event = Event {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000212").unwrap(),
            seq: 17,
            work_record_id: Some(record.id),
            session_id: None,
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::Assistant),
            occurred_at: fixed_time(),
            capture_source_id: None,
            payload: serde_json::json!({"text": "hello"}),
            payload_blob_id: None,
            dedupe_key: Some(Store::provider_event_dedupe_key(
                CaptureProvider::Codex,
                "codex-session-1",
                7,
                "hash-a",
            )),
            redaction_state: RedactionState::SafePreview,
            sync: sync_metadata(),
        };

        store.upsert_event(&event).unwrap();
        let conflict = store.upsert_event(&Event {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000213").unwrap(),
            seq: 18,
            payload: serde_json::json!({"text": "changed"}),
            dedupe_key: Some(Store::provider_event_dedupe_key(
                CaptureProvider::Codex,
                "codex-session-1",
                7,
                "hash-b",
            )),
            ..event
        });

        assert!(matches!(
            conflict,
            Err(StoreError::ProviderEventConflict {
                existing_hash,
                new_hash,
                ..
            }) if existing_hash == "hash-a" && new_hash == "hash-b"
        ));
        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn event_upsert_fails_closed_on_seq_conflict_without_dedupe_match() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let record = WorkRecord::new("Events", "seq conflict", Vec::new(), "task", None);
        store.insert_record(&record).unwrap();
        let event = Event {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000302").unwrap(),
            seq: 42,
            work_record_id: Some(record.id),
            session_id: None,
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::Assistant),
            occurred_at: fixed_time(),
            capture_source_id: None,
            payload: serde_json::json!({"text": "first"}),
            payload_blob_id: None,
            dedupe_key: None,
            redaction_state: RedactionState::SafePreview,
            sync: sync_metadata(),
        };

        store.upsert_event(&event).unwrap();
        let conflict = store.upsert_event(&Event {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000303").unwrap(),
            payload: serde_json::json!({"text": "second"}),
            ..event
        });

        assert!(conflict.is_err());
        let stored_id: String = store
            .conn
            .query_row("SELECT id FROM events WHERE seq = 42", [], |row| row.get(0))
            .unwrap();
        assert_eq!(stored_id, "018f45d0-0000-7000-8000-000000000302");
    }

    #[test]
    fn sync_cursor_roundtrips_source_position_metadata() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let cursor = SyncCursor {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000300").unwrap(),
            team_id: Some("local-import".into()),
            device_id: "device-1".into(),
            stream: "provider:codex".into(),
            cursor: "line:42".into(),
            last_synced_at: Some(fixed_time()),
            timestamps: timestamps(),
        };
        let cursor_id = store.upsert_sync_cursor(&cursor).unwrap();
        let updated_id = store
            .upsert_sync_cursor(&SyncCursor {
                cursor: "line:43".into(),
                ..cursor.clone()
            })
            .unwrap();

        assert_eq!(cursor_id, updated_id);
        let stored = store
            .get_sync_cursor(Some("local-import"), "device-1", "provider:codex")
            .unwrap()
            .unwrap();
        assert_eq!(stored.id, cursor.id);
        assert_eq!(stored.cursor, "line:43");
        assert_eq!(stored.last_synced_at, Some(fixed_time()));

        let local_cursor = SyncCursor {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000301").unwrap(),
            team_id: None,
            device_id: "device-1".into(),
            stream: "provider:claude".into(),
            cursor: "offset:1".into(),
            last_synced_at: None,
            timestamps: timestamps(),
        };
        let local_id = store.upsert_sync_cursor(&local_cursor).unwrap();
        let local_updated_id = store
            .upsert_sync_cursor(&SyncCursor {
                id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000302").unwrap(),
                cursor: "offset:2".into(),
                ..local_cursor
            })
            .unwrap();
        assert_eq!(local_id, local_updated_id);
        let local_count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sync_cursors WHERE team_id IS NULL AND device_id = 'device-1' AND stream = 'provider:claude'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(local_count, 1);
    }

    #[test]
    fn stores_searches_and_exports_records() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let record = WorkRecord::new(
            "Ship importer",
            "import and export json archives",
            vec!["import".into(), "json".into()],
            "task",
            Some("ctx".into()),
        );
        store.insert_record(&record).unwrap();

        let evidence = Evidence::new(
            Some(record.id),
            "cargo test",
            0,
            "ok token=secret".into(),
            "warn ghp_1234567890abcdef".into(),
            Utc::now(),
            12,
        );
        store.insert_evidence(&evidence).unwrap();

        assert_eq!(store.search_records("json", 10).unwrap()[0].id, record.id);

        let archive = store.export_archive().unwrap();
        assert_eq!(archive.schema_version, 2);
        assert_eq!(archive.version, 2);
        assert_eq!(archive.artifacts.len(), 2);
        let archived_stdout = archive
            .artifacts
            .iter()
            .find(|artifact| artifact.stream == "stdout")
            .unwrap();
        let archived_stderr = archive
            .artifacts
            .iter()
            .find(|artifact| artifact.stream == "stderr")
            .unwrap();
        assert_eq!(archived_stdout.evidence_id, evidence.id);
        assert_eq!(archived_stdout.content, "ok token=secret");
        assert_eq!(archived_stderr.evidence_id, evidence.id);
        assert_eq!(archived_stderr.content, "warn ghp_1234567890abcdef");
        let mut second = Store::open(temp.path().join("second.sqlite")).unwrap();
        second.import_archive(&archive, false).unwrap();
        assert_eq!(second.get_record(record.id).unwrap().title, "Ship importer");
        let imported_artifact_count: i64 = second
            .conn
            .query_row("SELECT COUNT(*) FROM evidence_artifacts", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(imported_artifact_count, 2);
        let (imported_stdout_preview, imported_stdout_blob_path): (String, String) = second
            .conn
            .query_row(
                r#"
                SELECT e.stdout, a.blob_path
                FROM evidence e
                JOIN evidence_artifacts ea ON ea.evidence_id = e.id
                JOIN artifacts a ON a.id = ea.artifact_id
                WHERE e.id = ?1 AND ea.stream = 'stdout'
                "#,
                params![evidence.id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(imported_stdout_preview, "ok token=[REDACTED_SECRET]");
        assert_eq!(
            fs::read_to_string(temp.path().join(imported_stdout_blob_path)).unwrap(),
            "ok token=secret"
        );
        let (imported_stderr_preview, imported_stderr_blob_path): (String, String) = second
            .conn
            .query_row(
                r#"
                SELECT e.stderr, a.blob_path
                FROM evidence e
                JOIN evidence_artifacts ea ON ea.evidence_id = e.id
                JOIN artifacts a ON a.id = ea.artifact_id
                WHERE e.id = ?1 AND ea.stream = 'stderr'
                "#,
                params![evidence.id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(imported_stderr_preview, "warn [REDACTED_SECRET]");
        assert_eq!(
            fs::read_to_string(temp.path().join(imported_stderr_blob_path)).unwrap(),
            "warn ghp_1234567890abcdef"
        );
        assert!(second.validate().unwrap().is_empty());
    }

    #[test]
    fn export_rejects_tampered_artifact_blob_paths() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let record = WorkRecord::new("Tamper", "blob path", Vec::new(), "task", None);
        store.insert_record(&record).unwrap();
        let evidence = Evidence::new(
            Some(record.id),
            "cargo test",
            0,
            "ok".into(),
            String::new(),
            Utc::now(),
            12,
        );
        store.insert_evidence(&evidence).unwrap();
        store
            .conn
            .execute("UPDATE artifacts SET blob_path = '../secret.txt'", [])
            .unwrap();

        let result = store.export_archive();

        assert!(matches!(result, Err(StoreError::Sql(_))));
    }

    #[test]
    fn rejects_unsupported_archive_versions() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let archive = WorkRecordArchive {
            schema_version: 3,
            version: 3,
            records: Vec::new(),
            evidence: Vec::new(),
            artifacts: Vec::new(),
            ..WorkRecordArchive::default()
        };

        assert!(matches!(
            store.import_archive(&archive, false),
            Err(StoreError::UnsupportedArchiveVersion(3))
        ));
    }

    #[test]
    fn import_rejects_conflicts_unless_overwrite_is_explicit() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let mut record = WorkRecord::new("Original", "body", Vec::new(), "note", None);
        store.insert_record(&record).unwrap();

        record.title = "Replacement".into();
        let archive = WorkRecordArchive {
            schema_version: 1,
            version: 1,
            records: vec![record.clone()],
            evidence: Vec::new(),
            artifacts: Vec::new(),
            ..WorkRecordArchive::default()
        };

        assert!(matches!(
            store.import_archive(&archive, false),
            Err(StoreError::ImportConflict { kind: "record", .. })
        ));
        assert_eq!(store.get_record(record.id).unwrap().title, "Original");

        store.import_archive(&archive, true).unwrap();
        assert_eq!(store.get_record(record.id).unwrap().title, "Replacement");
    }

    #[test]
    fn capture_source_import_without_overwrite_does_not_mutate_existing_source_id() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let source_id = Uuid::parse_str("018f45d0-0000-7000-8000-00000000c001").unwrap();
        let existing_source = imported_source(source_id, "original-session");
        store.upsert_capture_source(&existing_source).unwrap();

        let archive = archive_with_stdout_artifact("source import output");
        let replacement_descriptor = codex_source_descriptor("replacement-session");

        assert!(matches!(
            store.import_archive_from_capture_source(
                &archive,
                source_id,
                &replacement_descriptor,
                fixed_time(),
                Fidelity::Imported,
                false,
            ),
            Err(StoreError::ImportConflict {
                kind: "capture_source",
                id,
            }) if id == source_id
        ));

        let source = store.get_capture_source(source_id).unwrap();
        assert_eq!(
            source.descriptor.external_session_id.as_deref(),
            Some("original-session")
        );
    }

    #[test]
    fn import_rejects_provider_event_hash_conflicts_even_with_overwrite() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let existing_record = WorkRecord::new("Existing", "body", Vec::new(), "task", None);
        store.insert_record(&existing_record).unwrap();
        let existing_event =
            provider_event_for_record(existing_record.id, 1, 9, "existing-hash", "existing");
        store.upsert_event(&existing_event).unwrap();

        let mut archive = archive_with_stdout_artifact("overwrite rejected output");
        archive.schema_version = 2;
        archive.version = 2;
        archive.events.push(provider_event_for_record(
            archive.records[0].id,
            2,
            9,
            "incoming-hash",
            "incoming",
        ));
        let imported_record_id = archive.records[0].id;

        assert!(matches!(
            store.import_archive(&archive, true),
            Err(StoreError::ProviderEventConflict {
                existing_hash,
                new_hash,
                ..
            }) if existing_hash == "existing-hash" && new_hash == "incoming-hash"
        ));
        assert!(matches!(
            store.get_record(imported_record_id),
            Err(StoreError::NotFound(_))
        ));
    }

    #[test]
    fn failed_import_rolls_back_all_rows() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let record = WorkRecord::new("Atomic", "body", Vec::new(), "note", None);
        let evidence = Evidence::new(
            Some(Uuid::new_v4()),
            "cargo test",
            0,
            String::new(),
            String::new(),
            Utc::now(),
            1,
        );
        let archive = WorkRecordArchive {
            schema_version: 1,
            version: 1,
            records: vec![record.clone()],
            evidence: vec![evidence],
            artifacts: Vec::new(),
            ..WorkRecordArchive::default()
        };

        assert!(store.import_archive(&archive, false).is_err());
        assert!(matches!(
            store.get_record(record.id),
            Err(StoreError::NotFound(_))
        ));
    }

    #[test]
    fn import_rejects_archive_artifact_hash_mismatch_and_rolls_back() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let mut archive = archive_with_stdout_artifact("safe output");
        let record_id = archive.records[0].id;
        let artifact_id = archive.artifacts[0].id;
        archive.artifacts[0].blob_hash = "00bad".into();

        assert!(matches!(
            store.import_archive(&archive, false),
            Err(StoreError::ArchiveArtifactHashMismatch { id }) if id == artifact_id
        ));
        assert!(matches!(
            store.get_record(record_id),
            Err(StoreError::NotFound(_))
        ));
        let artifact_count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM artifacts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(artifact_count, 0);
    }

    #[test]
    fn import_rejects_archive_artifact_byte_size_mismatch_and_rolls_back() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let mut archive = archive_with_stdout_artifact("safe output");
        let record_id = archive.records[0].id;
        let artifact_id = archive.artifacts[0].id;
        archive.artifacts[0].byte_size += 1;

        assert!(matches!(
            store.import_archive(&archive, false),
            Err(StoreError::ArchiveArtifactSizeMismatch { id }) if id == artifact_id
        ));
        assert!(matches!(
            store.get_record(record_id),
            Err(StoreError::NotFound(_))
        ));
    }

    #[test]
    fn import_rejects_hostile_archive_blob_path_and_rolls_back() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let mut archive = archive_with_stdout_artifact("safe output");
        let record_id = archive.records[0].id;
        let artifact_id = archive.artifacts[0].id;
        archive.artifacts[0].blob_path = "../../outside".into();

        assert!(matches!(
            store.import_archive(&archive, false),
            Err(StoreError::ArchiveArtifactPathMismatch { id }) if id == artifact_id
        ));
        assert!(matches!(
            store.get_record(record_id),
            Err(StoreError::NotFound(_))
        ));
        assert!(!temp.path().join("outside").exists());
    }

    #[test]
    fn v2_import_rolls_back_records_evidence_and_rich_rows_on_rich_failure() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let mut archive = archive_with_stdout_artifact("safe output");
        archive.schema_version = 2;
        archive.version = 2;
        let record_id = archive.records[0].id;
        let evidence_id = archive.evidence[0].id;
        archive.sessions.push(Session {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-00000000b001").unwrap(),
            work_record_id: Some(record_id),
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: None,
            provider: CaptureProvider::Codex,
            external_session_id: Some("rich-rollback".into()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: None,
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: Some(
                Uuid::parse_str("018f45d0-0000-7000-8000-00000000ffff").unwrap(),
            ),
            started_at: fixed_time(),
            ended_at: None,
            timestamps: timestamps(),
            sync: sync_metadata(),
        });

        assert!(matches!(
            store.import_archive(&archive, false),
            Err(StoreError::Sql(_))
        ));
        assert!(matches!(
            store.get_record(record_id),
            Err(StoreError::NotFound(_))
        ));
        for (table, id) in [
            ("evidence", evidence_id),
            ("sessions", archive.sessions[0].id),
        ] {
            let count: i64 = store
                .conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE id = ?1"),
                    params![id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 0, "{table} row should roll back");
        }
    }

    #[test]
    fn failed_import_removes_blob_content_created_before_sql_failure() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let mut archive = archive_with_stdout_artifact("created before sql failure");
        archive.schema_version = 2;
        archive.version = 2;
        archive.sessions.push(Session {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-00000000c002").unwrap(),
            work_record_id: Some(archive.records[0].id),
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: None,
            provider: CaptureProvider::Codex,
            external_session_id: Some("blob-rollback".into()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: None,
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: Some(
                Uuid::parse_str("018f45d0-0000-7000-8000-00000000ffff").unwrap(),
            ),
            started_at: fixed_time(),
            ended_at: None,
            timestamps: timestamps(),
            sync: sync_metadata(),
        });
        let blob_path = temp.path().join(&archive.artifacts[0].blob_path);

        assert!(matches!(
            store.import_archive(&archive, false),
            Err(StoreError::Sql(_))
        ));
        assert!(
            !blob_path.exists(),
            "rejected import blob content should not remain durable"
        );
    }

    #[test]
    fn import_rejects_rich_dedupe_conflicts_without_overwrite() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let existing_record = WorkRecord::new("Existing", "body", Vec::new(), "task", None);
        store.insert_record(&existing_record).unwrap();
        let existing_event = event_for_record(existing_record.id, 1, "existing payload");
        store.upsert_event(&existing_event).unwrap();

        let mut archive = archive_with_stdout_artifact("safe output");
        archive.schema_version = 2;
        archive.version = 2;
        archive.events.push(event_for_record(
            archive.records[0].id,
            2,
            "different imported payload",
        ));
        let imported_record_id = archive.records[0].id;

        assert!(matches!(
            store.import_archive(&archive, false),
            Err(StoreError::ImportConflict { kind: "event", id }) if id == archive.events[0].id
        ));
        assert!(matches!(
            store.get_record(imported_record_id),
            Err(StoreError::NotFound(_))
        ));
        assert_eq!(
            store
                .event_id_by_dedupe_key("provider:codex:shared-session:7:payload-hash")
                .unwrap(),
            existing_event.id
        );
    }

    #[test]
    fn v2_import_rejects_vcs_workspace_identity_conflicts_without_overwrite() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let existing =
            git_workspace(Uuid::parse_str("018f45d0-0000-7000-8000-00000000c003").unwrap());
        store.upsert_vcs_workspace(&existing).unwrap();

        let mut incoming =
            git_workspace(Uuid::parse_str("018f45d0-0000-7000-8000-00000000c004").unwrap());
        incoming.owner = Some("different-owner".into());
        let archive = WorkRecordArchive {
            schema_version: 2,
            version: 2,
            vcs_workspaces: vec![incoming.clone()],
            ..WorkRecordArchive::default()
        };

        assert!(matches!(
            store.import_archive(&archive, false),
            Err(StoreError::ImportConflict {
                kind: "vcs_workspace",
                id,
            }) if id == incoming.id
        ));
        assert_eq!(
            store
                .list_vcs_workspaces()
                .unwrap()
                .into_iter()
                .next()
                .unwrap()
                .owner
                .as_deref(),
            Some("ctxrs")
        );
    }

    #[test]
    fn v2_import_rejects_pull_request_identity_conflicts_without_overwrite() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let workspace =
            git_workspace(Uuid::parse_str("018f45d0-0000-7000-8000-00000000c005").unwrap());
        let workspace_id = store.upsert_vcs_workspace(&workspace).unwrap();
        let existing = github_pr(
            Uuid::parse_str("018f45d0-0000-7000-8000-00000000c006").unwrap(),
            Some(workspace_id),
        );
        store.upsert_pull_request(&existing).unwrap();

        let mut incoming = github_pr(
            Uuid::parse_str("018f45d0-0000-7000-8000-00000000c007").unwrap(),
            Some(workspace_id),
        );
        incoming.title = Some("Different PR title".into());
        let archive = WorkRecordArchive {
            schema_version: 2,
            version: 2,
            pull_requests: vec![incoming.clone()],
            ..WorkRecordArchive::default()
        };

        assert!(matches!(
            store.import_archive(&archive, false),
            Err(StoreError::ImportConflict {
                kind: "pull_request",
                id,
            }) if id == incoming.id
        ));
        assert_eq!(
            store.list_pull_requests().unwrap()[0].title.as_deref(),
            Some("Rich storage")
        );
    }

    #[test]
    fn partial_pull_request_upsert_preserves_existing_rich_metadata() {
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let workspace =
            git_workspace(Uuid::parse_str("018f45d0-0000-7000-8000-00000000c005").unwrap());
        let workspace_id = store.upsert_vcs_workspace(&workspace).unwrap();
        let existing = github_pr(
            Uuid::parse_str("018f45d0-0000-7000-8000-00000000c006").unwrap(),
            Some(workspace_id),
        );
        store.upsert_pull_request(&existing).unwrap();

        let mut partial = github_pr(
            Uuid::parse_str("018f45d0-0000-7000-8000-00000000c007").unwrap(),
            None,
        );
        partial.title = None;
        partial.state = None;
        partial.head_ref = None;
        partial.base_ref = None;
        partial.head_sha = None;
        partial.sync.fidelity = Fidelity::Partial;

        let pr_id = store.upsert_pull_request(&partial).unwrap();
        assert_eq!(pr_id, existing.id);
        let stored = store.list_pull_requests().unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].vcs_workspace_id, Some(workspace_id));
        assert_eq!(stored[0].title.as_deref(), Some("Rich storage"));
        assert_eq!(stored[0].state.as_deref(), Some("open"));
        assert_eq!(
            stored[0].head_ref.as_deref(),
            Some("ctx/wr-finish-store-search")
        );
        assert_eq!(stored[0].base_ref.as_deref(), Some("main"));
        assert_eq!(stored[0].head_sha.as_deref(), Some("abcdef"));
    }

    #[test]
    fn v2_import_rejects_provider_event_hash_conflicts_without_overwrite() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let existing_record = WorkRecord::new("Existing", "body", Vec::new(), "task", None);
        store.insert_record(&existing_record).unwrap();
        let existing_event =
            provider_event_for_record(existing_record.id, 1, 9, "existing-hash", "existing");
        store.upsert_event(&existing_event).unwrap();

        let mut archive = archive_with_stdout_artifact("safe output");
        archive.schema_version = 2;
        archive.version = 2;
        archive.events.push(provider_event_for_record(
            archive.records[0].id,
            2,
            9,
            "incoming-hash",
            "incoming",
        ));
        let imported_record_id = archive.records[0].id;

        assert!(matches!(
            store.import_archive(&archive, false),
            Err(StoreError::ProviderEventConflict {
                existing_hash,
                new_hash,
                ..
            }) if existing_hash == "existing-hash" && new_hash == "incoming-hash"
        ));
        assert!(matches!(
            store.get_record(imported_record_id),
            Err(StoreError::NotFound(_))
        ));
    }

    #[test]
    fn v2_capture_source_import_rejects_internal_provider_event_hash_conflicts() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let mut archive = archive_with_stdout_artifact("safe output");
        archive.schema_version = 2;
        archive.version = 2;
        archive.events.push(provider_event_for_record(
            archive.records[0].id,
            31,
            12,
            "first-hash",
            "first",
        ));
        archive.events.push(provider_event_for_record(
            archive.records[0].id,
            32,
            12,
            "second-hash",
            "second",
        ));
        let imported_record_id = archive.records[0].id;
        let source_id = new_id();
        let source = CaptureSourceDescriptor {
            kind: work_record_core::CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Codex,
            machine_id: "machine-1".into(),
            process_id: Some(42),
            cwd: Some("/repo".into()),
            raw_source_path: Some("/sessions/codex.jsonl".into()),
            external_session_id: Some("codex-session-1".into()),
        };

        assert!(matches!(
            store.import_archive_from_capture_source(
                &archive,
                source_id,
                &source,
                fixed_time(),
                Fidelity::Imported,
                false,
            ),
            Err(StoreError::ProviderEventConflict {
                existing_hash,
                new_hash,
                ..
            }) if existing_hash == "first-hash" && new_hash == "second-hash"
        ));
        assert!(matches!(
            store.get_record(imported_record_id),
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            store.get_capture_source(source_id),
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            store.import_archive_from_capture_source(
                &archive,
                source_id,
                &source,
                fixed_time(),
                Fidelity::Imported,
                true,
            ),
            Err(StoreError::ProviderEventConflict {
                existing_hash,
                new_hash,
                ..
            }) if existing_hash == "first-hash" && new_hash == "second-hash"
        ));
        assert!(matches!(
            store.get_record(imported_record_id),
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            store.get_capture_source(source_id),
            Err(StoreError::NotFound(_))
        ));
    }

    #[test]
    fn v2_import_rejects_internal_provider_event_hash_conflicts() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let mut archive = archive_with_stdout_artifact("safe output");
        archive.schema_version = 2;
        archive.version = 2;
        archive.events.push(provider_event_for_record(
            archive.records[0].id,
            31,
            12,
            "first-hash",
            "first",
        ));
        archive.events.push(provider_event_for_record(
            archive.records[0].id,
            32,
            12,
            "second-hash",
            "second",
        ));
        let imported_record_id = archive.records[0].id;

        assert!(matches!(
            store.import_archive(&archive, false),
            Err(StoreError::ProviderEventConflict {
                existing_hash,
                new_hash,
                ..
            }) if existing_hash == "first-hash" && new_hash == "second-hash"
        ));
        assert!(matches!(
            store.get_record(imported_record_id),
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            store.import_archive(&archive, true),
            Err(StoreError::ProviderEventConflict {
                existing_hash,
                new_hash,
                ..
            }) if existing_hash == "first-hash" && new_hash == "second-hash"
        ));
        assert!(matches!(
            store.get_record(imported_record_id),
            Err(StoreError::NotFound(_))
        ));
    }

    #[test]
    fn v2_import_rejects_event_seq_conflicts_without_overwrite() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let existing_record = WorkRecord::new("Existing", "body", Vec::new(), "task", None);
        store.insert_record(&existing_record).unwrap();
        let mut existing_event = event_for_record(existing_record.id, 11, "existing");
        existing_event.dedupe_key = None;
        store.upsert_event(&existing_event).unwrap();

        let mut archive = archive_with_stdout_artifact("safe output");
        archive.schema_version = 2;
        archive.version = 2;
        let mut imported_event = event_for_record(archive.records[0].id, 11, "incoming");
        imported_event.dedupe_key = None;
        archive.events.push(imported_event);
        let imported_record_id = archive.records[0].id;

        assert!(matches!(
            store.import_archive(&archive, false),
            Err(StoreError::ImportConflict { kind: "event", id }) if id == archive.events[0].id
        ));
        assert!(matches!(
            store.get_record(imported_record_id),
            Err(StoreError::NotFound(_))
        ));
    }

    #[test]
    fn v2_import_rejects_invalid_artifact_record_blob_path() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let mut archive = archive_with_stdout_artifact("safe output");
        archive.schema_version = 2;
        archive.version = 2;
        let record_id = archive.records[0].id;
        let artifact_id = archive.artifacts[0].id;
        let mut artifact_record = artifact_record_from_archive_artifact(&archive.artifacts[0]);
        artifact_record.blob_path = "objects/ff/not-the-hash".into();
        archive.artifact_records.push(artifact_record);

        assert!(matches!(
            store.import_archive(&archive, false),
            Err(StoreError::ArchiveArtifactPathMismatch { id }) if id == artifact_id
        ));
        assert!(matches!(
            store.get_record(record_id),
            Err(StoreError::NotFound(_))
        ));
    }

    #[test]
    fn v2_import_rejects_artifact_record_missing_blob_content() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let mut archive = archive_with_stdout_artifact("safe output");
        archive.schema_version = 2;
        archive.version = 2;
        let record_id = archive.records[0].id;
        let artifact_id = archive.artifacts[0].id;
        let artifact_record = artifact_record_from_archive_artifact(&archive.artifacts[0]);
        archive.artifacts.clear();
        archive.artifact_records.push(artifact_record);

        assert!(matches!(
            store.import_archive(&archive, false),
            Err(StoreError::ArchiveArtifactMissingContent { id }) if id == artifact_id
        ));
        assert!(matches!(
            store.get_record(record_id),
            Err(StoreError::NotFound(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn store_open_and_blob_writes_repair_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir();
        let path = temp.path().join("work.sqlite");
        let store = Store::open(&path).unwrap();
        let record = WorkRecord::new("Private", "body", Vec::new(), "task", None);
        store.insert_record(&record).unwrap();
        let evidence = Evidence::new(
            Some(record.id),
            "cargo test",
            0,
            "secret stdout".into(),
            String::new(),
            fixed_time(),
            1,
        );
        store.insert_evidence(&evidence).unwrap();

        let blob_path: String = store
            .conn
            .query_row("SELECT blob_path FROM artifacts LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        for (path, mode) in [
            (temp.path().to_path_buf(), 0o700),
            (temp.path().join(SPOOL_DIR), 0o700),
            (temp.path().join(OBJECTS_DIR), 0o700),
            (temp.path().join(&blob_path[..10]), 0o700),
            (temp.path().join(blob_path), 0o600),
            (path, 0o600),
        ] {
            let actual = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(actual, mode, "{path:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn export_rejects_symlink_archive_blob_file() {
        use std::os::unix::fs::symlink;

        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let record = WorkRecord::new("Symlink", "body", Vec::new(), "task", None);
        store.insert_record(&record).unwrap();
        let evidence = Evidence::new(
            Some(record.id),
            "cargo test",
            0,
            "secret stdout".into(),
            String::new(),
            fixed_time(),
            1,
        );
        store.insert_evidence(&evidence).unwrap();

        let blob_path: String = store
            .conn
            .query_row("SELECT blob_path FROM artifacts LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let absolute_blob_path = temp.path().join(blob_path);
        fs::remove_file(&absolute_blob_path).unwrap();
        let outside = temp.path().join("outside-secret");
        fs::write(&outside, "outside").unwrap();
        symlink(&outside, &absolute_blob_path).unwrap();

        assert!(matches!(
            store.export_archive(),
            Err(StoreError::Sql(rusqlite::Error::ToSqlConversionFailure(_)))
        ));
    }
}
