use rusqlite::{params, Connection, OptionalExtension};

use crate::Result;

pub(crate) struct ColumnSpec {
    pub(crate) name: &'static str,
    pub(crate) definition: &'static str,
}

pub(crate) const HISTORY_RECORD_COLUMNS: &[ColumnSpec] = &[
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

pub(crate) const CATALOG_SESSION_IMPORT_STATE_COLUMNS: &[ColumnSpec] = &[
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

pub(crate) const CREATE_TABLES_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS capture_sources (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('provider_import', 'provider_hook', 'direct_cli', 'manual')),

    provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'opencode', 'kilo', 'kiro_cli', 'crush', 'goose', 'antigravity', 'gemini', 'tabnine', 'cursor', 'windsurf', 'zed', 'copilot_cli', 'factory_ai_droid', 'qwen_code', 'kimi_code_cli', 'forgecode', 'deepagents', 'mistral_vibe', 'mux', 'rovodev', 'openclaw', 'hermes', 'nanoclaw', 'astrbot', 'shelley', 'continue', 'openhands', 'cline', 'roo_code', 'lingma', 'qoder', 'warp', 'codebuddy', 'auggie', 'firebender', 'junie', 'trae', 'shell', 'git', 'jj', 'gh', 'custom', 'unknown')),

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

    provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'opencode', 'kilo', 'kiro_cli', 'crush', 'goose', 'antigravity', 'gemini', 'tabnine', 'cursor', 'windsurf', 'zed', 'copilot_cli', 'factory_ai_droid', 'qwen_code', 'kimi_code_cli', 'forgecode', 'deepagents', 'mistral_vibe', 'mux', 'rovodev', 'openclaw', 'hermes', 'nanoclaw', 'astrbot', 'shelley', 'continue', 'openhands', 'cline', 'roo_code', 'lingma', 'qoder', 'warp', 'codebuddy', 'auggie', 'firebender', 'junie', 'trae', 'shell', 'git', 'jj', 'gh', 'custom', 'unknown')),

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

    provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'opencode', 'kilo', 'kiro_cli', 'crush', 'goose', 'antigravity', 'gemini', 'tabnine', 'cursor', 'windsurf', 'zed', 'copilot_cli', 'factory_ai_droid', 'qwen_code', 'kimi_code_cli', 'forgecode', 'deepagents', 'mistral_vibe', 'mux', 'rovodev', 'openclaw', 'hermes', 'nanoclaw', 'astrbot', 'shelley', 'continue', 'openhands', 'cline', 'roo_code', 'lingma', 'qoder', 'warp', 'codebuddy', 'auggie', 'firebender', 'junie', 'trae', 'shell', 'git', 'jj', 'gh', 'custom', 'unknown')),

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

pub(crate) fn ensure_columns(conn: &Connection, table: &str, columns: &[ColumnSpec]) -> Result<()> {
    for column in columns {
        if !table_has_column(conn, table, column.name)? {
            let sql = format!("ALTER TABLE {table} ADD COLUMN {}", column.definition);
            conn.execute(&sql, [])?;
        }
    }
    Ok(())
}

pub(crate) fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
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

pub(crate) fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}
