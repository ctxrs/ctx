use rusqlite::Connection;

use crate::Result;

const SEMANTIC_PROJECTION_EPOCH_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS canonical_semantic_projection_state (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    store_identity TEXT NOT NULL
        CHECK (
            length(store_identity) = 32
            AND store_identity NOT GLOB '*[^0-9a-f]*'
        ),
    mutation_epoch INTEGER NOT NULL DEFAULT 0
        CHECK (mutation_epoch >= 0 AND mutation_epoch <= 9223372036854775807)
);
INSERT OR IGNORE INTO canonical_semantic_projection_state
    (singleton, store_identity, mutation_epoch)
VALUES
    (1, lower(hex(randomblob(16))), 0);

CREATE TRIGGER IF NOT EXISTS canonical_semantic_projection_events_insert
AFTER INSERT ON events
BEGIN
    UPDATE canonical_semantic_projection_state
    SET mutation_epoch = mutation_epoch + 1
    WHERE singleton = 1;
END;
CREATE TRIGGER IF NOT EXISTS canonical_semantic_projection_events_update
AFTER UPDATE OF
    seq, history_record_id, session_id, run_id, event_type, role, occurred_at_ms,
    capture_source_id, payload_json, visibility, sync_state, deleted_at_ms
ON events
WHEN
    OLD.seq IS NOT NEW.seq
    OR OLD.history_record_id IS NOT NEW.history_record_id
    OR OLD.session_id IS NOT NEW.session_id
    OR OLD.run_id IS NOT NEW.run_id
    OR OLD.event_type IS NOT NEW.event_type
    OR OLD.role IS NOT NEW.role
    OR OLD.occurred_at_ms IS NOT NEW.occurred_at_ms
    OR OLD.capture_source_id IS NOT NEW.capture_source_id
    OR OLD.payload_json IS NOT NEW.payload_json
    OR OLD.visibility IS NOT NEW.visibility
    OR OLD.sync_state IS NOT NEW.sync_state
    OR OLD.deleted_at_ms IS NOT NEW.deleted_at_ms
BEGIN
    UPDATE canonical_semantic_projection_state
    SET mutation_epoch = mutation_epoch + 1
    WHERE singleton = 1;
END;
CREATE TRIGGER IF NOT EXISTS canonical_semantic_projection_events_delete
AFTER DELETE ON events
BEGIN
    UPDATE canonical_semantic_projection_state
    SET mutation_epoch = mutation_epoch + 1
    WHERE singleton = 1;
END;

CREATE TRIGGER IF NOT EXISTS canonical_semantic_projection_event_lookup_insert
AFTER INSERT ON event_search_lookup
BEGIN
    UPDATE canonical_semantic_projection_state
    SET mutation_epoch = mutation_epoch + 1
    WHERE singleton = 1;
END;
CREATE TRIGGER IF NOT EXISTS canonical_semantic_projection_event_lookup_update
AFTER UPDATE OF preview_text
ON event_search_lookup
WHEN OLD.preview_text IS NOT NEW.preview_text
BEGIN
    UPDATE canonical_semantic_projection_state
    SET mutation_epoch = mutation_epoch + 1
    WHERE singleton = 1;
END;
CREATE TRIGGER IF NOT EXISTS canonical_semantic_projection_event_lookup_delete
AFTER DELETE ON event_search_lookup
BEGIN
    UPDATE canonical_semantic_projection_state
    SET mutation_epoch = mutation_epoch + 1
    WHERE singleton = 1;
END;

CREATE TRIGGER IF NOT EXISTS canonical_semantic_projection_sessions_insert
AFTER INSERT ON sessions
BEGIN
    UPDATE canonical_semantic_projection_state
    SET mutation_epoch = mutation_epoch + 1
    WHERE singleton = 1;
END;
CREATE TRIGGER IF NOT EXISTS canonical_semantic_projection_sessions_update
AFTER UPDATE OF
    history_record_id, parent_session_id, root_session_id, capture_source_id,
    provider, external_session_id, agent_type, is_primary
ON sessions
WHEN
    OLD.history_record_id IS NOT NEW.history_record_id
    OR OLD.parent_session_id IS NOT NEW.parent_session_id
    OR OLD.root_session_id IS NOT NEW.root_session_id
    OR OLD.capture_source_id IS NOT NEW.capture_source_id
    OR OLD.provider IS NOT NEW.provider
    OR OLD.external_session_id IS NOT NEW.external_session_id
    OR OLD.agent_type IS NOT NEW.agent_type
    OR OLD.is_primary IS NOT NEW.is_primary
BEGIN
    UPDATE canonical_semantic_projection_state
    SET mutation_epoch = mutation_epoch + 1
    WHERE singleton = 1;
END;
CREATE TRIGGER IF NOT EXISTS canonical_semantic_projection_sessions_delete
AFTER DELETE ON sessions
BEGIN
    UPDATE canonical_semantic_projection_state
    SET mutation_epoch = mutation_epoch + 1
    WHERE singleton = 1;
END;

CREATE TRIGGER IF NOT EXISTS canonical_semantic_projection_runs_insert
AFTER INSERT ON runs
BEGIN
    UPDATE canonical_semantic_projection_state
    SET mutation_epoch = mutation_epoch + 1
    WHERE singleton = 1;
END;
CREATE TRIGGER IF NOT EXISTS canonical_semantic_projection_runs_update
AFTER UPDATE OF history_record_id, session_id, source_id
ON runs
WHEN
    OLD.history_record_id IS NOT NEW.history_record_id
    OR OLD.session_id IS NOT NEW.session_id
    OR OLD.source_id IS NOT NEW.source_id
BEGIN
    UPDATE canonical_semantic_projection_state
    SET mutation_epoch = mutation_epoch + 1
    WHERE singleton = 1;
END;
CREATE TRIGGER IF NOT EXISTS canonical_semantic_projection_runs_delete
AFTER DELETE ON runs
BEGIN
    UPDATE canonical_semantic_projection_state
    SET mutation_epoch = mutation_epoch + 1
    WHERE singleton = 1;
END;

CREATE TRIGGER IF NOT EXISTS canonical_semantic_projection_sources_insert
AFTER INSERT ON capture_sources
BEGIN
    UPDATE canonical_semantic_projection_state
    SET mutation_epoch = mutation_epoch + 1
    WHERE singleton = 1;
END;
CREATE TRIGGER IF NOT EXISTS canonical_semantic_projection_sources_update
AFTER UPDATE OF provider, cwd, raw_source_path, metadata_json
ON capture_sources
WHEN
    OLD.provider IS NOT NEW.provider
    OR OLD.cwd IS NOT NEW.cwd
    OR OLD.raw_source_path IS NOT NEW.raw_source_path
    OR OLD.metadata_json IS NOT NEW.metadata_json
BEGIN
    UPDATE canonical_semantic_projection_state
    SET mutation_epoch = mutation_epoch + 1
    WHERE singleton = 1;
END;
CREATE TRIGGER IF NOT EXISTS canonical_semantic_projection_sources_delete
AFTER DELETE ON capture_sources
BEGIN
    UPDATE canonical_semantic_projection_state
    SET mutation_epoch = mutation_epoch + 1
    WHERE singleton = 1;
END;

CREATE TRIGGER IF NOT EXISTS canonical_semantic_projection_records_insert
AFTER INSERT ON history_records
BEGIN
    UPDATE canonical_semantic_projection_state
    SET mutation_epoch = mutation_epoch + 1
    WHERE singleton = 1;
END;
CREATE TRIGGER IF NOT EXISTS canonical_semantic_projection_records_update
AFTER UPDATE OF title, kind, workspace ON history_records
WHEN
    OLD.title IS NOT NEW.title
    OR OLD.kind IS NOT NEW.kind
    OR OLD.workspace IS NOT NEW.workspace
BEGIN
    UPDATE canonical_semantic_projection_state
    SET mutation_epoch = mutation_epoch + 1
    WHERE singleton = 1;
END;
CREATE TRIGGER IF NOT EXISTS canonical_semantic_projection_records_delete
AFTER DELETE ON history_records
BEGIN
    UPDATE canonical_semantic_projection_state
    SET mutation_epoch = mutation_epoch + 1
    WHERE singleton = 1;
END;
"#;

pub(super) fn install(conn: &Connection) -> Result<()> {
    conn.execute_batch(SEMANTIC_PROJECTION_EPOCH_SQL)?;
    Ok(())
}
