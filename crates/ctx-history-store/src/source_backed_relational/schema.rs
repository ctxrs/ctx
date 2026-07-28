use rusqlite::{Connection, OptionalExtension};

use super::{
    RelationalProjectionError, Result, RELATIONAL_PROJECTION_CONTRACT_VERSION,
    RELATIONAL_PROJECTION_SCHEMA_VERSION,
};

pub(super) const SCHEMA_SQL: &str = r#"
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS source_backed_relational_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    schema_version INTEGER NOT NULL,
    contract_version INTEGER NOT NULL,
    build_generation INTEGER NOT NULL,
    active_generation_id TEXT,
    active_manifest_digest BLOB,
    target_generation_id TEXT,
    status TEXT NOT NULL CHECK (status IN ('empty', 'ready', 'behind')),
    source_count INTEGER NOT NULL,
    session_count INTEGER NOT NULL,
    event_count INTEGER NOT NULL,
    file_touch_count INTEGER NOT NULL,
    last_error TEXT
);

INSERT OR IGNORE INTO source_backed_relational_state (
    singleton,
    schema_version,
    contract_version,
    build_generation,
    status,
    source_count,
    session_count,
    event_count,
    file_touch_count
) VALUES (1, 1, 1, 0, 'empty', 0, 0, 0, 0);

CREATE TABLE IF NOT EXISTS source_backed_sources (
    source_id TEXT PRIMARY KEY,
    source_identity BLOB NOT NULL UNIQUE,
    source_descriptor_json BLOB NOT NULL,
    certificate_json BLOB NOT NULL,
    certificate_digest BLOB NOT NULL,
    provider TEXT NOT NULL,
    source_format TEXT NOT NULL,
    source_root TEXT,
    source_path TEXT,
    cwd TEXT,
    revision_kind TEXT NOT NULL,
    parser_revision TEXT NOT NULL,
    certified_bytes INTEGER NOT NULL,
    content_digest_hex TEXT NOT NULL,
    indexed_event_count INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS source_backed_sessions (
    ctx_session_id TEXT PRIMARY KEY,
    session_identity BLOB NOT NULL UNIQUE,
    source_id TEXT NOT NULL REFERENCES source_backed_sources(source_id) ON DELETE CASCADE,
    parent_ctx_session_id TEXT,
    parent_session_identity BLOB,
    root_ctx_session_id TEXT NOT NULL,
    root_session_identity BLOB NOT NULL,
    provider_session_id TEXT,
    external_agent_id TEXT,
    agent_type TEXT NOT NULL,
    role_hint TEXT,
    is_primary INTEGER NOT NULL,
    branch TEXT,
    workspace TEXT,
    cwd TEXT,
    source_path TEXT,
    status TEXT NOT NULL,
    fidelity TEXT NOT NULL,
    started_at_ms INTEGER,
    ended_at_ms INTEGER
);

CREATE INDEX IF NOT EXISTS source_backed_sessions_source
ON source_backed_sessions(source_id);

CREATE TABLE IF NOT EXISTS source_backed_events (
    ctx_event_id TEXT PRIMARY KEY,
    event_identity BLOB NOT NULL UNIQUE,
    source_id TEXT NOT NULL REFERENCES source_backed_sources(source_id) ON DELETE CASCADE,
    ctx_session_id TEXT NOT NULL REFERENCES source_backed_sessions(ctx_session_id) ON DELETE CASCADE,
    session_identity BLOB NOT NULL,
    event_seq INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    role TEXT,
    occurred_at_ms INTEGER,
    payload_json TEXT NOT NULL,
    fidelity TEXT NOT NULL,
    native_locator_json BLOB NOT NULL,
    record_digest BLOB NOT NULL
);

CREATE INDEX IF NOT EXISTS source_backed_events_session_seq
ON source_backed_events(ctx_session_id, event_seq, ctx_event_id);

CREATE INDEX IF NOT EXISTS source_backed_events_source
ON source_backed_events(source_id);

CREATE TABLE IF NOT EXISTS source_backed_files_touched (
    ctx_file_touch_id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL REFERENCES source_backed_sources(source_id) ON DELETE CASCADE,
    ctx_event_id TEXT REFERENCES source_backed_events(ctx_event_id) ON DELETE CASCADE,
    event_identity BLOB,
    ctx_session_id TEXT REFERENCES source_backed_sessions(ctx_session_id) ON DELETE CASCADE,
    session_identity BLOB,
    path TEXT NOT NULL,
    old_path TEXT,
    change_kind TEXT,
    line_count_delta INTEGER,
    confidence TEXT NOT NULL,
    created_at_ms INTEGER,
    updated_at_ms INTEGER
);

CREATE INDEX IF NOT EXISTS source_backed_files_source
ON source_backed_files_touched(source_id);

CREATE INDEX IF NOT EXISTS source_backed_files_path
ON source_backed_files_touched(path);

DROP VIEW IF EXISTS ctx_sessions;
CREATE VIEW ctx_sessions AS
SELECT
    s.ctx_session_id AS ctx_session_id,
    NULL AS history_record_id,
    s.parent_ctx_session_id AS parent_ctx_session_id,
    s.root_ctx_session_id AS root_ctx_session_id,
    src.provider AS provider,
    s.provider_session_id AS provider_session_id,
    s.external_agent_id AS external_agent_id,
    s.agent_type AS agent_type,
    s.role_hint AS role_hint,
    s.is_primary AS is_primary,
    s.status AS status,
    s.fidelity AS fidelity,
    s.started_at_ms AS started_at_ms,
    s.ended_at_ms AS ended_at_ms,
    COALESCE(s.cwd, src.cwd) AS cwd,
    COALESCE(s.source_path, src.source_path) AS source_path,
    src.source_format AS source_format,
    src.source_root AS source_root,
    src.source_id AS source_identity,
    s.branch AS branch,
    s.workspace AS workspace
FROM source_backed_sessions s
JOIN source_backed_sources src ON src.source_id = s.source_id;

DROP VIEW IF EXISTS ctx_events;
CREATE VIEW ctx_events AS
SELECT
    e.ctx_event_id AS ctx_event_id,
    e.ctx_session_id AS ctx_session_id,
    NULL AS history_record_id,
    src.provider AS provider,
    s.provider_session_id AS provider_session_id,
    e.event_seq AS event_seq,
    e.event_type AS event_type,
    e.role AS role,
    e.occurred_at_ms AS occurred_at_ms,
    e.payload_json AS payload_json,
    e.fidelity AS fidelity,
    COALESCE(s.cwd, src.cwd) AS cwd,
    COALESCE(s.source_path, src.source_path) AS source_path,
    src.source_format AS source_format,
    src.source_root AS source_root,
    src.source_id AS source_identity,
    s.branch AS branch,
    s.workspace AS workspace
FROM source_backed_events e
JOIN source_backed_sessions s ON s.ctx_session_id = e.ctx_session_id
JOIN source_backed_sources src ON src.source_id = e.source_id;

DROP VIEW IF EXISTS ctx_files_touched;
CREATE VIEW ctx_files_touched AS
SELECT
    ft.ctx_file_touch_id AS ctx_file_touch_id,
    ft.path AS path,
    ft.old_path AS old_path,
    ft.change_kind AS change_kind,
    ft.line_count_delta AS line_count_delta,
    ft.confidence AS confidence,
    ft.ctx_event_id AS ctx_event_id,
    COALESCE(ft.ctx_session_id, e.ctx_session_id) AS ctx_session_id,
    NULL AS history_record_id,
    src.provider AS provider,
    s.provider_session_id AS provider_session_id,
    src.source_format AS source_format,
    src.source_root AS source_root,
    src.source_id AS source_identity,
    ft.created_at_ms AS created_at_ms,
    ft.updated_at_ms AS updated_at_ms
FROM source_backed_files_touched ft
LEFT JOIN source_backed_events e ON e.ctx_event_id = ft.ctx_event_id
LEFT JOIN source_backed_sessions s
    ON s.ctx_session_id = COALESCE(ft.ctx_session_id, e.ctx_session_id)
JOIN source_backed_sources src ON src.source_id = ft.source_id;

DROP VIEW IF EXISTS ctx_sources;
CREATE VIEW ctx_sources AS
SELECT
    src.provider AS provider,
    src.source_format AS source_format,
    src.source_root AS source_root,
    COALESCE(s.source_path, src.source_path) AS source_path,
    s.provider_session_id AS provider_session_id,
    parent.provider_session_id AS parent_provider_session_id,
    s.agent_type AS agent_type,
    s.role_hint AS role_hint,
    s.external_agent_id AS external_agent_id,
    COALESCE(s.cwd, src.cwd) AS cwd,
    s.started_at_ms AS session_started_at_ms,
    src.certified_bytes AS file_size_bytes,
    NULL AS file_modified_at_ms,
    NULL AS cataloged_at_ms,
    NULL AS indexed_at_ms,
    'indexed' AS indexed_status,
    NULL AS indexed_error,
    src.indexed_event_count AS indexed_event_count,
    NULL AS last_imported_at_ms,
    src.certified_bytes AS last_imported_file_size_bytes,
    NULL AS last_imported_file_modified_at_ms,
    src.content_digest_hex AS last_imported_file_sha256,
    src.indexed_event_count AS last_imported_event_count,
    0 AS is_stale,
    s.branch AS branch,
    s.workspace AS workspace
FROM source_backed_sources src
LEFT JOIN source_backed_sessions s ON s.source_id = src.source_id
LEFT JOIN source_backed_sessions parent
    ON parent.ctx_session_id = s.parent_ctx_session_id;

DROP VIEW IF EXISTS ctx_projection_metadata;
CREATE VIEW ctx_projection_metadata AS
SELECT
    schema_version AS schema_version,
    contract_version AS contract_version,
    build_generation AS build_generation,
    active_generation_id AS core_generation_id,
    target_generation_id AS target_core_generation_id,
    status AS status,
    source_count AS source_count,
    session_count AS session_count,
    event_count AS event_count,
    file_touch_count AS file_touch_count,
    last_error AS last_error
FROM source_backed_relational_state
WHERE singleton = 1;
"#;

pub(super) fn initialize(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA_SQL)?;
    verify(conn)
}

pub(super) fn verify(conn: &Connection) -> Result<()> {
    let state = conn
        .query_row(
            "SELECT schema_version, contract_version
             FROM source_backed_relational_state
             WHERE singleton = 1",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?
        .ok_or(RelationalProjectionError::MissingSchema)?;
    if state.0 != i64::from(RELATIONAL_PROJECTION_SCHEMA_VERSION)
        || state.1 != i64::from(RELATIONAL_PROJECTION_CONTRACT_VERSION)
    {
        return Err(RelationalProjectionError::UnsupportedSchema {
            schema_version: state.0,
            contract_version: state.1,
        });
    }
    for view in [
        "ctx_sessions",
        "ctx_events",
        "ctx_files_touched",
        "ctx_sources",
        "ctx_projection_metadata",
    ] {
        let exists = conn
            .query_row(
                "SELECT 1 FROM sqlite_schema WHERE type = 'view' AND name = ?1",
                [view],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            return Err(RelationalProjectionError::MissingStableView(
                view.to_owned(),
            ));
        }
    }
    Ok(())
}
