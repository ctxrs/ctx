use rusqlite::{params, Connection, OptionalExtension};

use crate::Result;

pub(crate) struct ColumnSpec {
    pub(crate) name: &'static str,
    pub(crate) definition: &'static str,
}

// SQLite validates a newly added CHECK constraint against every existing row.
// Upgrade migrations use these equivalent application-validated definitions so
// adding the columns stays a schema-only operation; fresh DDL retains CHECKs.
pub(crate) const LEGACY_CATALOG_IMPORT_REVISION_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec {
        name: "import_revision",
        definition: "import_revision INTEGER NOT NULL DEFAULT 1",
    },
    ColumnSpec {
        name: "indexed_import_revision",
        definition: "indexed_import_revision INTEGER",
    },
];

pub(crate) const LEGACY_SOURCE_IMPORT_REVISION_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec {
        name: "import_revision",
        definition: "import_revision INTEGER NOT NULL DEFAULT 1",
    },
    ColumnSpec {
        name: "indexed_import_revision",
        definition: "indexed_import_revision INTEGER",
    },
];

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
        definition: "visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full'))",
    },
    ColumnSpec {
        name: "fidelity",
        definition: "fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only'))",
    },
    ColumnSpec {
        name: "sync_state",
        definition: "sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed'))",
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
        name: "import_revision",
        definition: "import_revision INTEGER NOT NULL DEFAULT 1 CHECK (import_revision > 0)",
    },
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
        definition: "indexed_status TEXT NOT NULL DEFAULT 'pending' CHECK (indexed_status IN ('pending', 'indexed', 'completed_with_rejections', 'rejected', 'failed'))",
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
        name: "indexed_import_revision",
        definition: "indexed_import_revision INTEGER CHECK (indexed_import_revision > 0)",
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

pub(crate) const SOURCE_IMPORT_FILE_STATE_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec {
        name: "import_revision",
        definition: "import_revision INTEGER NOT NULL DEFAULT 1 CHECK (import_revision > 0)",
    },
    ColumnSpec {
        name: "indexed_import_revision",
        definition: "indexed_import_revision INTEGER CHECK (indexed_import_revision > 0)",
    },
];

pub(crate) const CAPTURE_SOURCE_IDENTITY_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec {
        name: "source_format",
        definition: "source_format TEXT",
    },
    ColumnSpec {
        name: "source_root",
        definition: "source_root TEXT",
    },
    ColumnSpec {
        name: "source_identity",
        definition: "source_identity TEXT",
    },
];

pub(crate) const CREATE_TABLES_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS capture_sources (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('provider_import', 'provider_hook', 'direct_cli', 'manual')),

    provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'opencode', 'kilo', 'kiro_cli', 'crush', 'goose', 'antigravity', 'gemini', 'tabnine', 'cursor', 'windsurf', 'zed', 'copilot_cli', 'factory_ai_droid', 'qwen_code', 'kimi_code_cli', 'forgecode', 'deepagents', 'mistral_vibe', 'mux', 'rovodev', 'openclaw', 'hermes', 'nanoclaw', 'astrbot', 'shelley', 'continue', 'openhands', 'cline', 'roo_code', 'lingma', 'qoder', 'warp', 'codebuddy', 'auggie', 'firebender', 'junie', 'trae', 'shell', 'git', 'jj', 'gh', 'custom', 'unknown', 'mimocode')),

    machine_id TEXT NOT NULL,
    process_id INTEGER,
    cwd TEXT,
    raw_source_path TEXT,
    source_format TEXT,
    source_root TEXT,
    source_identity TEXT,
    external_session_id TEXT,
    started_at_ms INTEGER NOT NULL,
    ended_at_ms INTEGER,
    fidelity TEXT NOT NULL CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed')),
    sync_version INTEGER NOT NULL DEFAULT 0,
    metadata_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS import_inventory_generations (
    provider TEXT NOT NULL,
    source_root TEXT NOT NULL,
    inventory_family TEXT NOT NULL CHECK (inventory_family IN ('catalog_sessions', 'source_import_files')),
    current_generation INTEGER NOT NULL CHECK (current_generation > 0),
    completed_generation INTEGER NOT NULL DEFAULT 0 CHECK (completed_generation >= 0 AND completed_generation <= current_generation),
    PRIMARY KEY (provider, source_root, inventory_family)
);

CREATE TABLE IF NOT EXISTS catalog_sessions (
    source_path TEXT PRIMARY KEY NOT NULL,

    provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'opencode', 'kilo', 'kiro_cli', 'crush', 'goose', 'antigravity', 'gemini', 'tabnine', 'cursor', 'windsurf', 'zed', 'copilot_cli', 'factory_ai_droid', 'qwen_code', 'kimi_code_cli', 'forgecode', 'deepagents', 'mistral_vibe', 'mux', 'rovodev', 'openclaw', 'hermes', 'nanoclaw', 'astrbot', 'shelley', 'continue', 'openhands', 'cline', 'roo_code', 'lingma', 'qoder', 'warp', 'codebuddy', 'auggie', 'firebender', 'junie', 'trae', 'shell', 'git', 'jj', 'gh', 'custom', 'unknown', 'mimocode')),

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
    import_revision INTEGER NOT NULL DEFAULT 1 CHECK (import_revision > 0),
    cataloged_at_ms INTEGER NOT NULL,
    is_stale INTEGER NOT NULL DEFAULT 0,
    indexed_at_ms INTEGER,
    indexed_file_size_bytes INTEGER,
    indexed_file_modified_at_ms INTEGER,
    indexed_status TEXT NOT NULL DEFAULT 'pending' CHECK (indexed_status IN ('pending', 'indexed', 'completed_with_rejections', 'rejected', 'failed')),
    indexed_error TEXT,
    indexed_event_count INTEGER,
    indexed_import_revision INTEGER CHECK (indexed_import_revision > 0),
    last_imported_at_ms INTEGER,
    last_imported_file_size_bytes INTEGER,
    last_imported_file_modified_at_ms INTEGER,
    last_imported_file_sha256 TEXT,
    last_imported_event_count INTEGER,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    pending_reason TEXT CHECK (pending_reason IS NULL OR pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append', 'recovery_retry', 'recovery_replacement', 'parser_revision', 'missing_material', 'abandoned_publication', 'legacy', 'explicit_rescan'))
);

CREATE TABLE IF NOT EXISTS source_import_files (

    provider TEXT NOT NULL CHECK (provider IN ('codex', 'claude', 'pi', 'opencode', 'kilo', 'kiro_cli', 'crush', 'goose', 'antigravity', 'gemini', 'tabnine', 'cursor', 'windsurf', 'zed', 'copilot_cli', 'factory_ai_droid', 'qwen_code', 'kimi_code_cli', 'forgecode', 'deepagents', 'mistral_vibe', 'mux', 'rovodev', 'openclaw', 'hermes', 'nanoclaw', 'astrbot', 'shelley', 'continue', 'openhands', 'cline', 'roo_code', 'lingma', 'qoder', 'warp', 'codebuddy', 'auggie', 'firebender', 'junie', 'trae', 'shell', 'git', 'jj', 'gh', 'custom', 'unknown', 'mimocode')),

    source_format TEXT NOT NULL,
    source_root TEXT NOT NULL,
    source_path TEXT NOT NULL,
    file_size_bytes INTEGER NOT NULL,
    file_modified_at_ms INTEGER NOT NULL,
    import_revision INTEGER NOT NULL DEFAULT 1 CHECK (import_revision > 0),
    observed_at_ms INTEGER NOT NULL,
    is_stale INTEGER NOT NULL DEFAULT 0,
    indexed_at_ms INTEGER,
    indexed_file_size_bytes INTEGER,
    indexed_file_modified_at_ms INTEGER,
    indexed_status TEXT NOT NULL DEFAULT 'pending' CHECK (indexed_status IN ('pending', 'indexed', 'completed_with_rejections', 'rejected', 'failed')),
    indexed_error TEXT,
    indexed_import_revision INTEGER CHECK (indexed_import_revision > 0),
    metadata_json TEXT NOT NULL DEFAULT '{}',
    pending_reason TEXT CHECK (pending_reason IS NULL OR pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append', 'recovery_retry', 'recovery_replacement', 'parser_revision', 'missing_material', 'abandoned_publication', 'legacy', 'explicit_rescan')),
    PRIMARY KEY (provider, source_root, source_path)
);

CREATE TABLE IF NOT EXISTS import_pending_reason_repairs (
    inventory_family TEXT PRIMARY KEY NOT NULL
      CHECK (inventory_family IN ('catalog_sessions', 'source_import_files')),
    cursor_provider TEXT,
    cursor_source_root TEXT,
    cursor_source_path TEXT,
    completed INTEGER NOT NULL DEFAULT 0 CHECK (completed IN (0, 1))
);

CREATE TABLE IF NOT EXISTS import_pending_work (
    inventory_family TEXT NOT NULL
      CHECK (inventory_family IN ('catalog_sessions', 'source_import_files')),
    provider TEXT NOT NULL,
    source_root TEXT NOT NULL,
    source_path TEXT NOT NULL,
    work_class TEXT NOT NULL CHECK (work_class IN ('fresh', 'recovery')),
    indexed_at_ms INTEGER,
    PRIMARY KEY (inventory_family, provider, source_root, source_path)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS import_pending_work_counts (
    inventory_family TEXT NOT NULL
      CHECK (inventory_family IN ('catalog_sessions', 'source_import_files')),
    provider TEXT NOT NULL,
    source_root TEXT NOT NULL,
    work_class TEXT NOT NULL CHECK (work_class IN ('fresh', 'recovery')),
    pending_count INTEGER NOT NULL CHECK (pending_count > 0),
    PRIMARY KEY (inventory_family, provider, source_root, work_class)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS import_pending_work_state (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    selection_mode TEXT NOT NULL CHECK (selection_mode IN ('direct', 'projection'))
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS provider_file_checkpoints (
    provider TEXT NOT NULL,
    source_format TEXT NOT NULL CHECK (length(source_format) > 0),
    source_root TEXT NOT NULL CHECK (length(source_root) > 0),
    source_path TEXT NOT NULL CHECK (length(source_path) > 0),
    import_revision INTEGER NOT NULL CHECK (import_revision > 0),
    checkpoint_version INTEGER NOT NULL CHECK (checkpoint_version > 0),
    stable_file_identity TEXT NOT NULL CHECK (length(stable_file_identity) > 0),
    committed_byte_offset INTEGER NOT NULL CHECK (committed_byte_offset >= 0),
    committed_complete_line_count INTEGER NOT NULL CHECK (committed_complete_line_count >= 0),
    head_sha256 TEXT NOT NULL CHECK (length(head_sha256) = 64),
    boundary_sha256 TEXT NOT NULL CHECK (length(boundary_sha256) = 64),
    resume_state BLOB CHECK (resume_state IS NULL OR length(resume_state) BETWEEN 1 AND 65536),
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (provider, source_format, source_root, source_path)
);

CREATE TABLE IF NOT EXISTS provider_file_publications (
    replacement_id TEXT NOT NULL UNIQUE,
    owner_id TEXT NOT NULL CHECK (length(owner_id) = 64),
    publication_kind TEXT NOT NULL CHECK (publication_kind IN ('incremental', 'replacement')),
    staging_id TEXT NOT NULL CHECK (length(staging_id) = 64),
    provider TEXT NOT NULL,
    inventory_family TEXT NOT NULL CHECK (inventory_family IN ('catalog_sessions', 'source_import_files')),
    inventory_source_format TEXT NOT NULL CHECK (length(inventory_source_format) > 0),
    inventory_source_root TEXT NOT NULL CHECK (length(inventory_source_root) > 0),
    source_path TEXT NOT NULL CHECK (length(source_path) > 0),
    material_source_format TEXT NOT NULL CHECK (length(material_source_format) > 0),
    material_source_root TEXT NOT NULL CHECK (length(material_source_root) > 0),
    inventory_generation INTEGER NOT NULL CHECK (inventory_generation > 0),
    file_size_bytes INTEGER NOT NULL CHECK (file_size_bytes >= 0),
    file_modified_at_ms INTEGER NOT NULL,
    import_revision INTEGER NOT NULL CHECK (import_revision > 0),
    metadata_json TEXT,
    mutation_started INTEGER NOT NULL DEFAULT 0 CHECK (mutation_started IN (0, 1)),
    tracks_prior_material INTEGER NOT NULL DEFAULT 0 CHECK (tracks_prior_material IN (0, 1)),
    staging_initialized INTEGER NOT NULL DEFAULT 0 CHECK (staging_initialized IN (0, 1)),
    preparation_complete INTEGER NOT NULL DEFAULT 0 CHECK (preparation_complete IN (0, 1)),
    preparation_cursor TEXT,
    cleanup_phase INTEGER NOT NULL DEFAULT 0 CHECK (cleanup_phase BETWEEN 0 AND 14),
    cleanup_source_cursor TEXT,
    cleanup_entity_cursor TEXT,
    removed_artifacts INTEGER NOT NULL DEFAULT 0 CHECK (removed_artifacts >= 0),
    removed_summaries INTEGER NOT NULL DEFAULT 0 CHECK (removed_summaries >= 0),
    removed_history_record_links INTEGER NOT NULL DEFAULT 0 CHECK (removed_history_record_links >= 0),
    removed_history_records INTEGER NOT NULL DEFAULT 0 CHECK (removed_history_records >= 0),
    removed_history_record_tags INTEGER NOT NULL DEFAULT 0 CHECK (removed_history_record_tags >= 0),
    removed_record_edges INTEGER NOT NULL DEFAULT 0 CHECK (removed_record_edges >= 0),
    removed_audit_log_entries INTEGER NOT NULL DEFAULT 0 CHECK (removed_audit_log_entries >= 0),
    removed_vcs_workspaces INTEGER NOT NULL DEFAULT 0 CHECK (removed_vcs_workspaces >= 0),
    removed_vcs_changes INTEGER NOT NULL DEFAULT 0 CHECK (removed_vcs_changes >= 0),
    removed_events INTEGER NOT NULL DEFAULT 0 CHECK (removed_events >= 0),
    removed_runs INTEGER NOT NULL DEFAULT 0 CHECK (removed_runs >= 0),
    removed_files_touched INTEGER NOT NULL DEFAULT 0 CHECK (removed_files_touched >= 0),
    removed_session_edges INTEGER NOT NULL DEFAULT 0 CHECK (removed_session_edges >= 0),
    tombstoned_sessions INTEGER NOT NULL DEFAULT 0 CHECK (tombstoned_sessions >= 0),
    started_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    completion_payload_json TEXT CHECK (
        completion_payload_json IS NULL OR
        length(CAST(completion_payload_json AS BLOB)) BETWEEN 1 AND 262144
    ),
    inventory_observation_invalidated INTEGER NOT NULL DEFAULT 0
        CHECK (inventory_observation_invalidated IN (0, 1)),
    retirement_started INTEGER NOT NULL DEFAULT 0 CHECK (retirement_started IN (0, 1)),
    PRIMARY KEY (provider, material_source_format, material_source_root, source_path)
);

CREATE TABLE IF NOT EXISTS provider_file_publication_seen (
    replacement_id TEXT NOT NULL,
    entity_kind TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    PRIMARY KEY (replacement_id, entity_kind, entity_id),
    FOREIGN KEY (replacement_id) REFERENCES provider_file_publications(replacement_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS provider_file_publication_prior_sources (
    replacement_id TEXT NOT NULL,
    source_id TEXT NOT NULL,
    PRIMARY KEY (replacement_id, source_id),
    FOREIGN KEY (replacement_id) REFERENCES provider_file_publications(replacement_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS provider_file_publication_batch (
    replacement_id TEXT NOT NULL,
    source_id TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    PRIMARY KEY (replacement_id, source_id, entity_id),
    FOREIGN KEY (replacement_id) REFERENCES provider_file_publications(replacement_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS semantic_replacement_revision (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    current_revision INTEGER NOT NULL CHECK (current_revision >= 0)
);

CREATE TABLE IF NOT EXISTS search_projection_stats (
    key TEXT PRIMARY KEY NOT NULL,
    value INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
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
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed')),
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
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed')),
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
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    source_id TEXT REFERENCES capture_sources(id),
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed')),
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
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed')),
    sync_version INTEGER NOT NULL DEFAULT 0,
    deleted_at_ms INTEGER,
    metadata_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS session_aliases (
    alias_id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    reason TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
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
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed')),
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
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed')),
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
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed')),
    sync_version INTEGER NOT NULL DEFAULT 0,
    deleted_at_ms INTEGER,
    metadata_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS event_aliases (
    alias_id TEXT PRIMARY KEY NOT NULL,
    event_id TEXT NOT NULL REFERENCES events(id),
    reason TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS event_search_lookup (
    event_id TEXT PRIMARY KEY NOT NULL REFERENCES events(id) ON DELETE CASCADE,
    history_record_id TEXT REFERENCES history_records(id),
    session_id TEXT REFERENCES sessions(id),
    role TEXT CHECK (role IS NULL OR role IN ('user', 'assistant', 'system', 'tool', 'unknown')),
    preview_text TEXT NOT NULL,
    rank_bucket TEXT NOT NULL
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
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed')),
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
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed')),
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
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed')),
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
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed')),
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
    visibility TEXT NOT NULL DEFAULT 'local_only' CHECK (visibility IN ('local_only', 'reportable', 'sync_metadata', 'sync_full')),
    fidelity TEXT NOT NULL DEFAULT 'partial' CHECK (fidelity IN ('full', 'partial', 'imported', 'inferred', 'summary_only')),
    sync_state TEXT NOT NULL DEFAULT 'local_only' CHECK (sync_state IN ('local_only', 'pending', 'synced', 'failed')),
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
    sync_state TEXT NOT NULL DEFAULT 'pending' CHECK (sync_state IN ('pending', 'synced', 'failed')),
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

pub(crate) fn create_event_search_lookup_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS event_search_lookup (
            event_id TEXT PRIMARY KEY NOT NULL REFERENCES events(id) ON DELETE CASCADE,
            history_record_id TEXT REFERENCES history_records(id),
            session_id TEXT REFERENCES sessions(id),
            role TEXT CHECK (role IS NULL OR role IN ('user', 'assistant', 'system', 'tool', 'unknown')),
            preview_text TEXT NOT NULL,
            rank_bucket TEXT NOT NULL
        );
        "#,
    )?;
    Ok(())
}

pub(crate) fn ensure_search_projection_stats_table(conn: &Connection) -> Result<()> {
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS search_projection_stats (
            key TEXT PRIMARY KEY NOT NULL,
            value INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL
        )
        "#,
        [],
    )?;
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
