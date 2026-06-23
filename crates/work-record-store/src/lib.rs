use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Component, Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;
use work_record_core::{
    new_id, redact_preview, AgentType, Artifact, ArtifactKind, CaptureProvider, CaptureSource,
    CaptureSourceDescriptor, EntityTimestamps, Event, EventRole, EventType, Evidence, Fidelity,
    FileTouched, PullRequest, RedactionState, Run, RunStatus, RunType, Session, SessionEdge,
    SessionStatus, Summary, SyncCursor, SyncMetadata, SyncState, VcsChange, VcsWorkspace,
    Visibility, WorkContext, WorkRecord, WorkRecordArchive, WorkRecordArchiveArtifact,
    WorkRecordLink,
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
    #[error("evidence must be attached to a work record")]
    EvidenceMissingWorkRecord,
    #[error("archive artifact {id} content does not match its blob hash")]
    ArchiveArtifactHashMismatch { id: Uuid },
    #[error("unsafe blob path in local store: {0}")]
    UnsafeBlobPath(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

const SCHEMA_VERSION: i64 = 1;
const BUSY_TIMEOUT: Duration = Duration::from_millis(5_000);

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
    kind TEXT NOT NULL CHECK (kind IN ('provider_import', 'provider_hook', 'shim', 'direct_cli', 'dashboard', 'hosted_sync', 'manual')),
    provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'cursor', 'shell', 'git', 'jj', 'gh', 'unknown')),
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
    pr_url TEXT,
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
    run_type TEXT NOT NULL CHECK (run_type IN ('agent_turn', 'command', 'tool_call', 'review', 'import', 'evidence', 'summary')),
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
    event_type TEXT NOT NULL CHECK (event_type IN ('message', 'tool_call', 'tool_output', 'command_started', 'command_output', 'command_finished', 'file_touched', 'vcs_change', 'pr_link', 'evidence', 'artifact', 'summary', 'notice')),
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

CREATE TABLE IF NOT EXISTS pull_requests (
    id TEXT PRIMARY KEY NOT NULL,
    vcs_workspace_id TEXT REFERENCES vcs_workspaces(id),
    provider TEXT NOT NULL CHECK (provider IN ('github', 'gitlab', 'unknown')),
    url TEXT NOT NULL,
    number INTEGER,
    owner TEXT,
    repo TEXT,
    title TEXT,
    state TEXT,
    head_ref TEXT,
    base_ref TEXT,
    head_sha TEXT,
    confidence TEXT NOT NULL DEFAULT 'unknown' CHECK (confidence IN ('explicit', 'high', 'medium', 'low', 'unknown')),
    link_source TEXT NOT NULL CHECK (link_source IN ('explicit', 'gh_shim', 'captured_url', 'inferred_branch', 'inferred_commit', 'manual')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    source_id TEXT REFERENCES capture_sources(id),
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full', 'withheld')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed', 'withheld')),
    sync_version INTEGER NOT NULL DEFAULT 0,
    deleted_at_ms INTEGER,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    UNIQUE(provider, owner, repo, number)
);

CREATE TABLE IF NOT EXISTS work_record_links (
    id TEXT PRIMARY KEY NOT NULL,
    work_record_id TEXT NOT NULL REFERENCES work_records(id),
    target_type TEXT NOT NULL CHECK (target_type IN ('session', 'run', 'event', 'vcs_workspace', 'vcs_change', 'pull_request', 'artifact', 'evidence')),
    target_id TEXT NOT NULL,
    link_type TEXT NOT NULL CHECK (link_type IN ('produced', 'touched', 'references', 'evidence_for', 'published_to', 'likely_related')),
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

CREATE TABLE IF NOT EXISTS evidence (
    id TEXT PRIMARY KEY NOT NULL,
    work_record_id TEXT NOT NULL REFERENCES work_records(id),
    vcs_change_id TEXT REFERENCES vcs_changes(id),
    kind TEXT NOT NULL DEFAULT 'manual' CHECK (kind IN ('test', 'lint', 'build', 'typecheck', 'screenshot', 'review', 'ci', 'manual')),
    status TEXT NOT NULL DEFAULT 'unknown' CHECK (status IN ('passed', 'failed', 'skipped', 'stale', 'unknown')),
    freshness TEXT NOT NULL DEFAULT 'unbound' CHECK (freshness IN ('fresh', 'probably_fresh', 'stale', 'unbound', 'inferred')),
    command_run_id TEXT REFERENCES runs(id),
    artifact_id TEXT REFERENCES artifacts(id),
    observed_tree_hash TEXT,
    observed_head_sha TEXT,
    started_at_ms INTEGER,
    ended_at_ms INTEGER,
    stale_reason TEXT,
    created_at_ms INTEGER NOT NULL DEFAULT 0,
    updated_at_ms INTEGER NOT NULL DEFAULT 0,
    source_id TEXT REFERENCES capture_sources(id),
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full', 'withheld')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed', 'withheld')),
    sync_version INTEGER NOT NULL DEFAULT 0,
    deleted_at_ms INTEGER,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    record_id TEXT REFERENCES work_records(id) ON DELETE SET NULL,
    command TEXT NOT NULL DEFAULT '',
    exit_code INTEGER NOT NULL DEFAULT 0,
    stdout TEXT NOT NULL DEFAULT '',
    stderr TEXT NOT NULL DEFAULT '',
    started_at TEXT NOT NULL DEFAULT '',
    duration_ms INTEGER NOT NULL DEFAULT 0,
    CHECK (record_id IS NULL OR work_record_id IS NULL OR record_id = work_record_id)
);

CREATE TABLE IF NOT EXISTS evidence_artifacts (
    id TEXT PRIMARY KEY NOT NULL,
    evidence_id TEXT NOT NULL REFERENCES evidence(id) ON DELETE CASCADE,
    artifact_id TEXT NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
    stream TEXT NOT NULL CHECK (stream IN ('stdout', 'stderr')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full', 'withheld')),
    fidelity TEXT NOT NULL DEFAULT 'full' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed', 'withheld')),
    sync_version INTEGER NOT NULL DEFAULT 0,
    deleted_at_ms INTEGER,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    UNIQUE(evidence_id, artifact_id, stream)
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

CREATE TABLE IF NOT EXISTS sync_aliases (
    id TEXT PRIMARY KEY NOT NULL,
    local_table TEXT NOT NULL,
    local_id TEXT NOT NULL,
    hosted_id TEXT NOT NULL,
    team_id TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(local_table, local_id, team_id),
    UNIQUE(hosted_id, team_id)
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

CREATE TABLE IF NOT EXISTS audit_log (
    id TEXT PRIMARY KEY NOT NULL,
    actor_kind TEXT NOT NULL CHECK (actor_kind IN ('human', 'agent', 'system', 'hosted')),
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

CREATE UNIQUE INDEX IF NOT EXISTS idx_pull_requests_provider_owner_repo_number ON pull_requests(provider, owner, repo, number);
CREATE INDEX IF NOT EXISTS idx_pull_requests_vcs_workspace_id ON pull_requests(vcs_workspace_id);
CREATE INDEX IF NOT EXISTS idx_pull_requests_source_id ON pull_requests(source_id);

CREATE INDEX IF NOT EXISTS idx_work_record_links_work_record_id ON work_record_links(work_record_id);
CREATE INDEX IF NOT EXISTS idx_work_record_links_source_id ON work_record_links(source_id);

CREATE INDEX IF NOT EXISTS idx_artifacts_source_id ON artifacts(source_id);

CREATE INDEX IF NOT EXISTS idx_evidence_work_record_id ON evidence(work_record_id);
CREATE INDEX IF NOT EXISTS idx_evidence_record_id ON evidence(record_id);
CREATE INDEX IF NOT EXISTS idx_evidence_vcs_change_id ON evidence(vcs_change_id);
CREATE INDEX IF NOT EXISTS idx_evidence_command_run_id ON evidence(command_run_id);
CREATE INDEX IF NOT EXISTS idx_evidence_artifact_id ON evidence(artifact_id);
CREATE INDEX IF NOT EXISTS idx_evidence_source_id ON evidence(source_id);

CREATE INDEX IF NOT EXISTS idx_evidence_artifacts_evidence_id ON evidence_artifacts(evidence_id);
CREATE INDEX IF NOT EXISTS idx_evidence_artifacts_artifact_id ON evidence_artifacts(artifact_id);

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
CREATE INDEX IF NOT EXISTS idx_audit_log_source_id ON audit_log(source_id);
"#;

const FTS_TABLES_SQL: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS work_record_search USING fts5(
    record_id UNINDEXED,
    title,
    summary,
    primary_user_text,
    decision_text,
    evidence_text,
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

const DROP_FTS_TABLES_SQL: &str = r#"
DROP TABLE IF EXISTS work_record_search;
DROP TABLE IF EXISTS event_search;
DROP TABLE IF EXISTS artifact_search;
"#;

pub struct Store {
    path: PathBuf,
    blob_dir: PathBuf,
    conn: Connection,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let blob_dir = path
            .parent()
            .map(|parent| parent.join("blobs"))
            .unwrap_or_else(|| PathBuf::from("blobs"));
        fs::create_dir_all(&blob_dir)?;
        let conn = Connection::open(&path)?;
        configure_connection(&conn)?;
        let store = Self {
            path,
            blob_dir,
            conn,
        };
        store.migrate()?;
        store.backfill_evidence_artifacts()?;
        store.rebuild_search_projection()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn migrate(&self) -> Result<()> {
        configure_connection(&self.conn)?;
        let user_version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if user_version > SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchemaVersion(user_version));
        }
        if user_version < 1 {
            migrate_to_v1(&self.conn)?;
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
            self.conn
                .query_row(
                    "SELECT id FROM events WHERE dedupe_key = ?1",
                    params![dedupe_key],
                    |row| parse_uuid(row.get::<_, String>(0)?),
                )
                .optional()?
                .unwrap_or(event.id)
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

    pub fn event_id_by_dedupe_key(&self, dedupe_key: &str) -> Result<Uuid> {
        self.conn
            .query_row(
                "SELECT id FROM events WHERE dedupe_key = ?1",
                params![dedupe_key],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .map_err(StoreError::from)
    }

    pub fn events_for_session(&self, session_id: Uuid) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            event_select_sql("WHERE session_id = ?1 ORDER BY seq, occurred_at_ms").as_str(),
        )?;
        let rows = stmt.query_map(params![session_id.to_string()], event_from_row)?;
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

    pub fn upsert_pull_request(&self, pr: &PullRequest) -> Result<Uuid> {
        self.conn.execute(
            r#"
            INSERT INTO pull_requests
            (id, vcs_workspace_id, provider, url, number, owner, repo, title, state, head_ref, base_ref, head_sha, confidence, link_source, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
            ON CONFLICT DO UPDATE SET
                vcs_workspace_id = excluded.vcs_workspace_id,
                url = excluded.url,
                title = excluded.title,
                state = excluded.state,
                head_ref = excluded.head_ref,
                base_ref = excluded.base_ref,
                head_sha = excluded.head_sha,
                confidence = excluded.confidence,
                link_source = excluded.link_source,
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
                pr_url, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
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
                record.pr_url,
                record.created_at.to_rfc3339(),
                record.updated_at.to_rfc3339(),
            ],
        )?;
        self.rebuild_search_projection()?;
        Ok(())
    }

    pub fn upsert_record(&self, record: &WorkRecord) -> Result<()> {
        let created_at_ms = timestamp_ms(record.created_at);
        let updated_at_ms = timestamp_ms(record.updated_at);
        self.conn.execute(
            r#"
            INSERT INTO work_records
            (
                id, title, summary, status, started_at_ms, last_activity_at_ms,
                created_at_ms, updated_at_ms, body, tags_json, kind, workspace,
                pr_url, created_at, updated_at
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
                body = excluded.body,
                tags_json = excluded.tags_json,
                kind = excluded.kind,
                workspace = excluded.workspace,
                pr_url = excluded.pr_url,
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
                record.pr_url,
                record.created_at.to_rfc3339(),
                record.updated_at.to_rfc3339(),
            ],
        )?;
        self.rebuild_search_projection()?;
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
        let mut stmt = self
            .conn
            .prepare(record_select_sql("ORDER BY created_at DESC LIMIT ?1").as_str())?;
        let rows = stmt.query_map(params![limit as i64], record_from_row)?;
        collect_rows(rows)
    }

    pub fn search_records(&self, query: &str, limit: usize) -> Result<Vec<WorkRecord>> {
        if let Some(records) = self.search_records_fts(query, limit)? {
            return Ok(records);
        }
        let like = format!("%{}%", query);
        let mut stmt = self.conn.prepare(
            record_select_sql(
                "WHERE title LIKE ?1 OR body LIKE ?1 OR tags_json LIKE ?1 ORDER BY created_at DESC LIMIT ?2",
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![like, limit as i64], record_from_row)?;
        collect_rows(rows)
    }

    fn search_records_fts(&self, query: &str, limit: usize) -> Result<Option<Vec<WorkRecord>>> {
        if !table_exists(&self.conn, "work_record_search")? {
            return Ok(None);
        }
        let Some(match_query) = fts_match_query(query) else {
            return Ok(Some(self.list_records(limit)?));
        };
        let mut stmt = self.conn.prepare(
            r#"
            SELECT record_id
            FROM work_record_search
            WHERE work_record_search MATCH ?1
            ORDER BY bm25(work_record_search)
            LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(params![match_query, limit as i64], |row| {
            row.get::<_, String>(0)
        })?;
        let mut records = Vec::new();
        for row in rows {
            records.push(self.get_record(parse_uuid(row?)?)?);
        }
        Ok(Some(records))
    }

    pub fn link_pr(&self, id: Uuid, pr_url: &str) -> Result<WorkRecord> {
        let updated_at = Utc::now();
        let updated_at_ms = timestamp_ms(updated_at);
        let changed = self.conn.execute(
            r#"
            UPDATE work_records
            SET pr_url = ?1, updated_at = ?2, updated_at_ms = ?3, last_activity_at_ms = ?3
            WHERE id = ?4
            "#,
            params![
                pr_url,
                updated_at.to_rfc3339(),
                updated_at_ms,
                id.to_string()
            ],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(id));
        }
        self.rebuild_search_projection()?;
        self.get_record(id)
    }

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

    fn store_output_artifact(&self, kind: &str, content: &str) -> Result<Option<String>> {
        if content.is_empty() {
            return Ok(None);
        }

        let hash = sha256_hex(content.as_bytes());
        let shard = &hash[..2];
        let relative_path = format!("blobs/{shard}/{hash}");
        let absolute_dir = self.blob_dir.join(shard);
        fs::create_dir_all(&absolute_dir)?;
        let absolute_path = absolute_dir.join(&hash);
        if !absolute_path.exists() {
            fs::write(&absolute_path, content.as_bytes())?;
        }

        let now = timestamp_ms(Utc::now());
        let id = new_id().to_string();
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

    pub fn get_evidence(&self, id: Uuid) -> Result<Evidence> {
        let mut stmt = self
            .conn
            .prepare(evidence_select_sql("WHERE id = ?1").as_str())?;
        stmt.query_row(params![id.to_string()], evidence_from_row)
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    pub fn recent_evidence(&self, limit: usize) -> Result<Vec<Evidence>> {
        let mut stmt = self
            .conn
            .prepare(evidence_select_sql("ORDER BY started_at DESC LIMIT ?1").as_str())?;
        let rows = stmt.query_map(params![limit as i64], evidence_from_row)?;
        collect_rows(rows)
    }

    pub fn context(&self, query: Option<&str>, limit: usize) -> Result<WorkContext> {
        let records = match query {
            Some(query) => self.search_records(query, limit)?,
            None => self.list_records(limit)?,
        };
        let mut evidence = Vec::new();
        for record in &records {
            evidence.extend(self.evidence_for_record(record.id)?);
        }
        if evidence.is_empty() {
            evidence = self.recent_evidence(limit)?;
        }
        Ok(WorkContext {
            query: query.map(str::to_string),
            records,
            evidence,
        })
    }

    pub fn export_archive(&self) -> Result<WorkRecordArchive> {
        let evidence = self.recent_evidence(usize::MAX)?;
        Ok(WorkRecordArchive {
            schema_version: 1,
            version: 1,
            records: self.list_records(usize::MAX)?,
            artifacts: self.archive_artifacts()?,
            evidence,
        })
    }

    pub fn import_archive(&mut self, archive: &WorkRecordArchive, overwrite: bool) -> Result<()> {
        validate_archive_version(archive)?;
        validate_import_evidence_references(&self.conn, archive)?;
        let archive_artifacts = archive_artifacts_by_evidence(archive);
        let blob_dir = self.blob_dir.clone();
        let tx = self.conn.transaction()?;
        if !overwrite {
            reject_import_conflicts(&tx, archive)?;
        }
        for record in &archive.records {
            upsert_record_tx(&tx, record, None)?;
        }
        for evidence in &archive.evidence {
            if let Some(artifacts) = archive_artifacts.get(&evidence.id) {
                upsert_evidence_with_archive_artifacts_tx(
                    &tx, &blob_dir, evidence, artifacts, None,
                )?;
            } else {
                upsert_evidence_tx(&tx, &blob_dir, evidence, None)?;
            }
        }
        tx.commit()?;
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
        validate_import_evidence_references(&self.conn, archive)?;
        let archive_artifacts = archive_artifacts_by_evidence(archive);
        let blob_dir = self.blob_dir.clone();
        let tx = self.conn.transaction()?;
        if !overwrite {
            reject_import_conflicts(&tx, archive)?;
        }
        upsert_capture_source_tx(&tx, source_id, source, occurred_at, fidelity)?;
        for record in &archive.records {
            upsert_record_tx(&tx, record, Some(source_id))?;
        }
        for evidence in &archive.evidence {
            if let Some(artifacts) = archive_artifacts.get(&evidence.id) {
                upsert_evidence_with_archive_artifacts_tx(
                    &tx,
                    &blob_dir,
                    evidence,
                    artifacts,
                    Some(source_id),
                )?;
            } else {
                upsert_evidence_tx(&tx, &blob_dir, evidence, Some(source_id))?;
            }
        }
        tx.commit()?;
        self.rebuild_search_projection()?;
        Ok(())
    }

    pub fn validate(&self) -> Result<Vec<String>> {
        let integrity: String = self
            .conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        let orphan_count: i64 = self.conn.query_row(
            r#"
            SELECT COUNT(*)
            FROM evidence e
            LEFT JOIN work_records r ON COALESCE(e.record_id, e.work_record_id) = r.id
            WHERE COALESCE(e.record_id, e.work_record_id) IS NOT NULL AND r.id IS NULL
            "#,
            [],
            |row| row.get(0),
        )?;
        let foreign_key_failures = count_foreign_key_failures(&self.conn)?;

        let mut findings = Vec::new();
        if integrity != "ok" {
            findings.push(format!("sqlite integrity_check returned {integrity}"));
        }
        if orphan_count > 0 {
            findings.push(format!(
                "{orphan_count} evidence rows reference missing records"
            ));
        }
        if foreign_key_failures > 0 {
            findings.push(format!(
                "{foreign_key_failures} foreign key violations detected"
            ));
        }
        Ok(findings)
    }

    fn archive_artifacts(&self) -> Result<Vec<WorkRecordArchiveArtifact>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT
                a.id,
                ea.evidence_id,
                ea.stream,
                a.kind,
                a.blob_hash,
                a.blob_path,
                a.byte_size,
                a.media_type,
                a.preview_text,
                a.redaction_state
            FROM evidence_artifacts ea
            JOIN artifacts a ON a.id = ea.artifact_id
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

    fn absolute_blob_path(&self, blob_path: &str) -> Result<PathBuf> {
        let relative_path = blob_path.strip_prefix("blobs/").unwrap_or(blob_path);
        let path = Path::new(relative_path);
        if path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(StoreError::UnsafeBlobPath(blob_path.to_owned()));
        }
        Ok(self.blob_dir.join(path))
    }

    fn rebuild_search_projection(&self) -> Result<()> {
        rebuild_search_projection(&self.conn)
    }
}

fn configure_connection(conn: &Connection) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;
        PRAGMA journal_mode = WAL;
        "#,
    )?;
    Ok(())
}

fn rebuild_search_projection(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "work_record_search")? {
        return Ok(());
    }

    conn.execute_batch(
        r#"
        DELETE FROM work_record_search;
        DELETE FROM event_search;
        DELETE FROM artifact_search;
        "#,
    )?;

    let records = {
        let mut stmt = conn.prepare(record_select_sql("ORDER BY created_at DESC").as_str())?;
        let rows = stmt.query_map([], record_from_row)?;
        collect_rows(rows)?
    };

    for record in records {
        let evidence_text = redacted_evidence_text(conn, record.id)?;
        conn.execute(
            r#"
            INSERT INTO work_record_search
            (record_id, title, summary, primary_user_text, decision_text, evidence_text, tag_text)
            VALUES (?1, ?2, ?3, ?4, '', ?5, ?6)
            "#,
            params![
                record.id.to_string(),
                redact_preview(&record.title, 512),
                redact_preview(&record.body, 2048),
                redact_preview(&record.body, 2048),
                evidence_text,
                redact_preview(&record.tags.join(" "), 1024),
            ],
        )?;
    }

    let mut stmt = conn.prepare(
        r#"
        SELECT a.id, ea.evidence_id, a.preview_text
        FROM artifacts a
        JOIN evidence_artifacts ea ON ea.artifact_id = a.id
        WHERE a.preview_text IS NOT NULL
        "#,
    )?;
    let artifacts = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for artifact in artifacts {
        let (artifact_id, evidence_id, preview) = artifact?;
        let work_record_id = conn
            .query_row(
                "SELECT COALESCE(record_id, work_record_id) FROM evidence WHERE id = ?1",
                params![evidence_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        conn.execute(
            r#"
            INSERT INTO artifact_search (artifact_id, work_record_id, safe_preview_text)
            VALUES (?1, ?2, ?3)
            "#,
            params![artifact_id, work_record_id, redact_preview(&preview, 2048)],
        )?;
    }

    Ok(())
}

fn redacted_evidence_text(conn: &Connection, record_id: Uuid) -> Result<String> {
    let mut stmt = conn.prepare(
        r#"
        SELECT command, stdout, stderr
        FROM evidence
        WHERE record_id = ?1 OR work_record_id = ?1
        ORDER BY started_at DESC
        "#,
    )?;
    let rows = stmt.query_map(params![record_id.to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut text = String::new();
    for row in rows {
        let (command, stdout, stderr) = row?;
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&command);
        text.push('\n');
        text.push_str(&stdout);
        text.push('\n');
        text.push_str(&stderr);
    }
    Ok(redact_preview(&text, 4096))
}

fn migrate_to_v1(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        ensure_columns(conn, "work_records", WORK_RECORD_COLUMNS)?;
        ensure_columns(conn, "evidence", EVIDENCE_COLUMNS)?;
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

fn create_fts_tables_if_supported(conn: &Connection) -> Result<()> {
    match conn.execute_batch(&format!("{DROP_FTS_TABLES_SQL}\n{FTS_TABLES_SQL}")) {
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

        UPDATE evidence
        SET work_record_id = record_id
        WHERE work_record_id IS NULL AND record_id IS NOT NULL;

        UPDATE evidence
        SET status = CASE WHEN exit_code = 0 THEN 'passed' ELSE 'failed' END
        WHERE status = 'unknown';

        UPDATE evidence
        SET started_at_ms = COALESCE(CAST(strftime('%s', started_at) AS INTEGER) * 1000, started_at_ms)
        WHERE started_at_ms IS NULL AND started_at IS NOT NULL;

        UPDATE evidence
        SET ended_at_ms = started_at_ms + duration_ms
        WHERE ended_at_ms IS NULL AND started_at_ms IS NOT NULL;

        UPDATE evidence
        SET created_at_ms = COALESCE(started_at_ms, created_at_ms)
        WHERE created_at_ms = 0;

        UPDATE evidence
        SET updated_at_ms = created_at_ms
        WHERE updated_at_ms = 0;
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

fn evidence_status(exit_code: i32) -> &'static str {
    if exit_code == 0 {
        "passed"
    } else {
        "failed"
    }
}

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

pub fn validate_archive_version(archive: &WorkRecordArchive) -> Result<()> {
    if archive.schema_version == 1 && archive.version == 1 {
        Ok(())
    } else {
        Err(StoreError::UnsupportedArchiveVersion(
            archive.schema_version.max(archive.version),
        ))
    }
}

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
        if !archive_evidence_ids.contains(&artifact.evidence_id) {
            return Err(StoreError::NotFound(artifact.evidence_id));
        }
    }
    Ok(())
}

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
    for evidence in &archive.evidence {
        if row_exists(tx, "evidence", evidence.id)? {
            return Err(StoreError::ImportConflict {
                kind: "evidence",
                id: evidence.id,
            });
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
            workspace, pr_url, created_at, updated_at
        )
        VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
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
            pr_url = excluded.pr_url,
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
            record.pr_url,
            record.created_at.to_rfc3339(),
            record.updated_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn upsert_evidence_tx(
    tx: &Transaction<'_>,
    blob_dir: &Path,
    evidence: &Evidence,
    source_id: Option<Uuid>,
) -> Result<()> {
    let work_record_id = evidence
        .record_id
        .ok_or(StoreError::EvidenceMissingWorkRecord)?;
    let started_at_ms = timestamp_ms(evidence.started_at);
    let ended_at_ms = started_at_ms.saturating_add(evidence.duration_ms);
    let status = evidence_status(evidence.exit_code);
    let stdout_artifact_id = store_output_artifact_tx(tx, blob_dir, "stdout", &evidence.stdout)?;
    let stderr_artifact_id = store_output_artifact_tx(tx, blob_dir, "stderr", &evidence.stderr)?;
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

fn upsert_evidence_with_archive_artifacts_tx(
    tx: &Transaction<'_>,
    blob_dir: &Path,
    evidence: &Evidence,
    artifacts: &[&WorkRecordArchiveArtifact],
    source_id: Option<Uuid>,
) -> Result<()> {
    let mut stdout = None;
    let mut stderr = None;
    let mut stdout_artifact_id = None;
    let mut stderr_artifact_id = None;

    for artifact in artifacts {
        let artifact_id = store_archive_artifact_tx(tx, blob_dir, artifact)?;
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

fn store_archive_artifact_tx(
    tx: &Transaction<'_>,
    blob_dir: &Path,
    artifact: &WorkRecordArchiveArtifact,
) -> Result<String> {
    let hash = sha256_hex(artifact.content.as_bytes());
    if hash != artifact.blob_hash {
        return Err(StoreError::ArchiveArtifactHashMismatch { id: artifact.id });
    }
    if artifact.content.len() as u64 != artifact.byte_size {
        return Err(StoreError::ArchiveArtifactHashMismatch { id: artifact.id });
    }

    let shard = &hash[..2];
    let relative_path = format!("blobs/{shard}/{hash}");
    let absolute_dir = blob_dir.join(shard);
    fs::create_dir_all(&absolute_dir)?;
    let absolute_path = absolute_dir.join(&hash);
    if !absolute_path.exists() {
        fs::write(&absolute_path, artifact.content.as_bytes())?;
    }

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

fn store_output_artifact_tx(
    tx: &Transaction<'_>,
    blob_dir: &Path,
    kind: &str,
    content: &str,
) -> Result<Option<String>> {
    if content.is_empty() {
        return Ok(None);
    }

    let hash = sha256_hex(content.as_bytes());
    let shard = &hash[..2];
    let relative_path = format!("blobs/{shard}/{hash}");
    let absolute_dir = blob_dir.join(shard);
    fs::create_dir_all(&absolute_dir)?;
    let absolute_path = absolute_dir.join(&hash);
    if !absolute_path.exists() {
        fs::write(&absolute_path, content.as_bytes())?;
    }

    let now = timestamp_ms(Utc::now());
    let id = new_id().to_string();
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
        "SELECT id, title, body, tags_json, kind, workspace, pr_url, created_at, updated_at FROM work_records {tail}"
    )
}

fn evidence_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, COALESCE(record_id, work_record_id), command, exit_code, stdout, stderr, started_at, duration_ms FROM evidence {tail}"
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
        pr_url: row.get(6)?,
        created_at: parse_time(row.get::<_, String>(7)?)?,
        updated_at: parse_time(row.get::<_, String>(8)?)?,
    })
}

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
            "ok token=[redacted] password=[redacted] [redacted]"
        );
        assert_eq!(stderr_preview, "secret=[redacted] [redacted] [redacted]");

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

    #[test]
    fn fts_projection_is_populated_with_redacted_evidence_text() {
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
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, record.id);

        if table_exists(&store.conn, "work_record_search").unwrap() {
            let evidence_text: String = store
                .conn
                .query_row(
                    "SELECT evidence_text FROM work_record_search WHERE record_id = ?1",
                    params![record.id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(evidence_text.contains("needle-only-output"));
            assert!(evidence_text.contains("password=[redacted]"));
            assert!(!evidence_text.contains("hunter2"));
        }
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

        let file = FileTouched {
            id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000106").unwrap(),
            work_record_id: Some(record.id),
            run_id: Some(run.id),
            event_id: None,
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
        assert!(stored_payload.contains("updated"));
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
        assert_eq!(
            store.context(Some("import"), 10).unwrap().evidence[0].id,
            evidence.id
        );

        let archive = store.export_archive().unwrap();
        assert_eq!(archive.schema_version, 1);
        assert_eq!(archive.version, 1);
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
        assert_eq!(imported_stdout_preview, "ok token=[redacted]");
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
        assert_eq!(imported_stderr_preview, "warn [redacted]");
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
            schema_version: 2,
            version: 2,
            records: Vec::new(),
            evidence: Vec::new(),
            artifacts: Vec::new(),
        };

        assert!(matches!(
            store.import_archive(&archive, false),
            Err(StoreError::UnsupportedArchiveVersion(2))
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
        };

        assert!(store.import_archive(&archive, false).is_err());
        assert!(matches!(
            store.get_record(record.id),
            Err(StoreError::NotFound(_))
        ));
    }
}
