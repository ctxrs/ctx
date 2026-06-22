use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use thiserror::Error;
use uuid::Uuid;
use work_record_core::{Evidence, WorkContext, WorkRecord, WorkRecordArchive};

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
    work_record_id TEXT REFERENCES work_records(id),
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
    tag_text,
    content=''
);

CREATE VIRTUAL TABLE IF NOT EXISTS event_search USING fts5(
    event_id UNINDEXED,
    work_record_id UNINDEXED,
    session_id UNINDEXED,
    role UNINDEXED,
    safe_preview_text,
    rank_bucket UNINDEXED,
    content=''
);

CREATE VIRTUAL TABLE IF NOT EXISTS artifact_search USING fts5(
    artifact_id UNINDEXED,
    work_record_id UNINDEXED,
    safe_preview_text,
    content=''
);
"#;

pub struct Store {
    path: PathBuf,
    conn: Connection,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&path)?;
        configure_connection(&conn)?;
        let store = Self { path, conn };
        store.migrate()?;
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
        self.get_record(id)
    }

    pub fn insert_evidence(&self, evidence: &Evidence) -> Result<()> {
        let started_at_ms = timestamp_ms(evidence.started_at);
        let ended_at_ms = started_at_ms.saturating_add(evidence.duration_ms);
        let status = evidence_status(evidence.exit_code);
        self.conn.execute(
            r#"
            INSERT INTO evidence
            (
                id, work_record_id, record_id, kind, status, freshness,
                started_at_ms, ended_at_ms, created_at_ms, updated_at_ms,
                command, exit_code, stdout, stderr, started_at, duration_ms
            )
            VALUES (?1, ?2, ?2, 'manual', ?3, 'unbound', ?4, ?5, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            "#,
            params![
                evidence.id.to_string(),
                evidence.record_id.map(|id| id.to_string()),
                status,
                started_at_ms,
                ended_at_ms,
                evidence.command,
                evidence.exit_code,
                evidence.stdout,
                evidence.stderr,
                evidence.started_at.to_rfc3339(),
                evidence.duration_ms,
            ],
        )?;
        Ok(())
    }

    pub fn upsert_evidence(&self, evidence: &Evidence) -> Result<()> {
        let started_at_ms = timestamp_ms(evidence.started_at);
        let ended_at_ms = started_at_ms.saturating_add(evidence.duration_ms);
        let status = evidence_status(evidence.exit_code);
        self.conn.execute(
            r#"
            INSERT INTO evidence
            (
                id, work_record_id, record_id, kind, status, freshness,
                started_at_ms, ended_at_ms, created_at_ms, updated_at_ms,
                command, exit_code, stdout, stderr, started_at, duration_ms
            )
            VALUES (?1, ?2, ?2, 'manual', ?3, 'unbound', ?4, ?5, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT(id) DO UPDATE SET
                work_record_id = excluded.work_record_id,
                record_id = excluded.record_id,
                status = excluded.status,
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
                evidence.record_id.map(|id| id.to_string()),
                status,
                started_at_ms,
                ended_at_ms,
                evidence.command,
                evidence.exit_code,
                evidence.stdout,
                evidence.stderr,
                evidence.started_at.to_rfc3339(),
                evidence.duration_ms,
            ],
        )?;
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
        Ok(WorkRecordArchive {
            version: 1,
            records: self.list_records(usize::MAX)?,
            evidence: self.recent_evidence(usize::MAX)?,
        })
    }

    pub fn import_archive(&mut self, archive: &WorkRecordArchive, overwrite: bool) -> Result<()> {
        validate_archive_version(archive)?;
        let tx = self.conn.transaction()?;
        if !overwrite {
            reject_import_conflicts(&tx, archive)?;
        }
        for record in &archive.records {
            upsert_record_tx(&tx, record)?;
        }
        for evidence in &archive.evidence {
            upsert_evidence_tx(&tx, evidence)?;
        }
        tx.commit()?;
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

pub fn validate_archive_version(archive: &WorkRecordArchive) -> Result<()> {
    if archive.version == 1 {
        Ok(())
    } else {
        Err(StoreError::UnsupportedArchiveVersion(archive.version))
    }
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

fn upsert_record_tx(tx: &Transaction<'_>, record: &WorkRecord) -> Result<()> {
    let created_at_ms = timestamp_ms(record.created_at);
    let updated_at_ms = timestamp_ms(record.updated_at);
    tx.execute(
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
    Ok(())
}

fn upsert_evidence_tx(tx: &Transaction<'_>, evidence: &Evidence) -> Result<()> {
    let started_at_ms = timestamp_ms(evidence.started_at);
    let ended_at_ms = started_at_ms.saturating_add(evidence.duration_ms);
    let status = evidence_status(evidence.exit_code);
    tx.execute(
        r#"
        INSERT INTO evidence
        (
            id, work_record_id, record_id, kind, status, freshness,
            started_at_ms, ended_at_ms, created_at_ms, updated_at_ms,
            command, exit_code, stdout, stderr, started_at, duration_ms
        )
        VALUES (?1, ?2, ?2, 'manual', ?3, 'unbound', ?4, ?5, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
        ON CONFLICT(id) DO UPDATE SET
            work_record_id = excluded.work_record_id,
            record_id = excluded.record_id,
            status = excluded.status,
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
            evidence.record_id.map(|id| id.to_string()),
            status,
            started_at_ms,
            ended_at_ms,
            evidence.command,
            evidence.exit_code,
            evidence.stdout,
            evidence.stderr,
            evidence.started_at.to_rfc3339(),
            evidence.duration_ms,
        ],
    )?;
    Ok(())
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
            "ok".into(),
            String::new(),
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
        let mut second = Store::open(temp.path().join("second.sqlite")).unwrap();
        second.import_archive(&archive, false).unwrap();
        assert_eq!(second.get_record(record.id).unwrap().title, "Ship importer");
        assert!(second.validate().unwrap().is_empty());
    }

    #[test]
    fn rejects_unsupported_archive_versions() {
        let temp = tempdir();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let archive = WorkRecordArchive {
            version: 2,
            records: Vec::new(),
            evidence: Vec::new(),
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
            version: 1,
            records: vec![record.clone()],
            evidence: Vec::new(),
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
            version: 1,
            records: vec![record.clone()],
            evidence: vec![evidence],
        };

        assert!(store.import_archive(&archive, false).is_err());
        assert!(matches!(
            store.get_record(record.id),
            Err(StoreError::NotFound(_))
        ));
    }
}
