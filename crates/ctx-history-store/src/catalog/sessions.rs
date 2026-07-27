use ctx_history_core::{AgentType, CaptureProvider};
use rusqlite::params;
use serde_json::Value;

use crate::connection::{
    capped_i64, collect_rows, nonnegative_i64_to_u64, parse_json, parse_text_enum,
};
use crate::{Result, Store};

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

impl Store {
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
                         AND (json_type(excluded.metadata_json, '$.inventory_file_change_token_v1') IS NULL
                              OR catalog_sessions.metadata_json IS excluded.metadata_json)
                        THEN catalog_sessions.indexed_at_ms
                        ELSE NULL
                    END,
                    indexed_file_size_bytes = CASE
                        WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                         AND (json_type(excluded.metadata_json, '$.inventory_file_change_token_v1') IS NULL
                              OR catalog_sessions.metadata_json IS excluded.metadata_json)
                        THEN catalog_sessions.indexed_file_size_bytes
                        ELSE NULL
                    END,
                    indexed_file_modified_at_ms = CASE
                        WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                         AND (json_type(excluded.metadata_json, '$.inventory_file_change_token_v1') IS NULL
                              OR catalog_sessions.metadata_json IS excluded.metadata_json)
                        THEN catalog_sessions.indexed_file_modified_at_ms
                        ELSE NULL
                    END,
                    indexed_status = CASE
                        WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                         AND (json_type(excluded.metadata_json, '$.inventory_file_change_token_v1') IS NULL
                              OR catalog_sessions.metadata_json IS excluded.metadata_json)
                        THEN catalog_sessions.indexed_status
                        ELSE 'pending'
                    END,
                    indexed_error = CASE
                        WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                         AND (json_type(excluded.metadata_json, '$.inventory_file_change_token_v1') IS NULL
                              OR catalog_sessions.metadata_json IS excluded.metadata_json)
                        THEN catalog_sessions.indexed_error
                        ELSE NULL
                    END,
                    indexed_event_count = CASE
                        WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                         AND (json_type(excluded.metadata_json, '$.inventory_file_change_token_v1') IS NULL
                              OR catalog_sessions.metadata_json IS excluded.metadata_json)
                        THEN catalog_sessions.indexed_event_count
                        ELSE NULL
                    END,
                    last_imported_at_ms = CASE
                        WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                         AND (json_type(excluded.metadata_json, '$.inventory_file_change_token_v1') IS NULL
                              OR catalog_sessions.metadata_json IS excluded.metadata_json)
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
                         AND (json_type(excluded.metadata_json, '$.inventory_file_change_token_v1') IS NULL
                              OR catalog_sessions.metadata_json IS excluded.metadata_json)
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
                         AND (json_type(excluded.metadata_json, '$.inventory_file_change_token_v1') IS NULL
                              OR catalog_sessions.metadata_json IS excluded.metadata_json)
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
                         AND (json_type(excluded.metadata_json, '$.inventory_file_change_token_v1') IS NULL
                              OR catalog_sessions.metadata_json IS excluded.metadata_json)
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
                         AND (json_type(excluded.metadata_json, '$.inventory_file_change_token_v1') IS NULL
                              OR catalog_sessions.metadata_json IS excluded.metadata_json)
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
}

pub(super) fn catalog_session_select_sql(tail: &str) -> String {
    format!(
        "SELECT source_path, provider, source_format, source_root, external_session_id, parent_external_session_id, agent_type, role_hint, external_agent_id, cwd, session_started_at_ms, file_size_bytes, file_modified_at_ms, cataloged_at_ms, metadata_json FROM catalog_sessions {tail}"
    )
}

pub(super) fn catalog_session_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<CatalogSession> {
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
