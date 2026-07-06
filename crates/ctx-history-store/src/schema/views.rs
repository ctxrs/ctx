use rusqlite::{Connection, OptionalExtension};

use crate::Result;

pub(crate) const STABLE_SQL_VIEWS_SQL: &str = r#"
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
    cs.raw_source_path AS source_path,
    cs.source_format AS source_format,
    cs.source_root AS source_root,
    cs.source_identity AS source_identity
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
    cs.raw_source_path AS source_path,
    cs.source_format AS source_format,
    cs.source_root AS source_root,
    cs.source_identity AS source_identity
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
    cs.source_format AS source_format,
    cs.source_root AS source_root,
    cs.source_identity AS source_identity,
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

pub(crate) fn create_stable_sql_views(conn: &Connection) -> Result<()> {
    conn.execute_batch(STABLE_SQL_VIEWS_SQL)?;
    Ok(())
}

pub(crate) fn drop_stable_sql_views(conn: &Connection) -> Result<()> {
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

pub(crate) fn stable_sql_views_exist(conn: &Connection) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'view' AND name = 'ctx_sessions'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}
