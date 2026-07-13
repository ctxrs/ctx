use ctx_history_core::{AgentType, CaptureProvider};
use rusqlite::{params, OptionalExtension};
use serde_json::Value;

use crate::connection::{
    capped_i64, collect_rows, nonnegative_i64_to_u32, nonnegative_i64_to_u64, parse_json,
    parse_text_enum,
};
use crate::{Result, Store, StoreError};

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
    pub import_revision: u32,
    pub cataloged_at_ms: i64,
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogSourceIndexUpdate<'a> {
    pub source_root: &'a str,
    pub source_path: &'a str,
    pub file_size_bytes: u64,
    pub file_modified_at_ms: i64,
    pub import_revision: u32,
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
    pub import_revision: u32,
    pub observed_at_ms: i64,
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceImportFileIndexUpdate<'a> {
    pub source_root: &'a str,
    pub source_path: &'a str,
    pub file_size_bytes: u64,
    pub file_modified_at_ms: i64,
    pub import_revision: u32,
    pub indexed_at_ms: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CatalogCounts {
    pub total: usize,
    pub indexed: usize,
    pub stale: usize,
    pub pending: usize,
    pub failed: usize,
    pub completed_with_rejections: usize,
    pub rejected: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceImportFileCounts {
    pub total: usize,
    pub indexed: usize,
    pub stale: usize,
    pub pending: usize,
    pub failed: usize,
    pub completed_with_rejections: usize,
    pub rejected: usize,
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
    CompletedWithRejections,
    Rejected,
    Failed,
}

impl CatalogIndexedStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Indexed => "indexed",
            Self::CompletedWithRejections => "completed_with_rejections",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }

    fn preserves_native_resume_checkpoint(self) -> bool {
        self == Self::Indexed
    }
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
                    file_modified_at_ms, import_revision, cataloged_at_ms, is_stale, metadata_json
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, 0, ?16)
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
                    import_revision = excluded.import_revision,
                    cataloged_at_ms = excluded.cataloged_at_ms,
                    is_stale = 0,
                    indexed_at_ms = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                        THEN catalog_sessions.indexed_at_ms
                        ELSE NULL
                    END,
                    indexed_file_size_bytes = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                        THEN catalog_sessions.indexed_file_size_bytes
                        ELSE NULL
                    END,
                    indexed_file_modified_at_ms = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                        THEN catalog_sessions.indexed_file_modified_at_ms
                        ELSE NULL
                    END,
                    indexed_status = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                        THEN catalog_sessions.indexed_status
                        ELSE 'pending'
                    END,
                    indexed_error = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                        THEN catalog_sessions.indexed_error
                        ELSE NULL
                    END,
                    indexed_event_count = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                        THEN catalog_sessions.indexed_event_count
                        ELSE NULL
                    END,
                    indexed_import_revision = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                        THEN catalog_sessions.indexed_import_revision
                        ELSE NULL
                    END,
                    last_imported_at_ms = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                        THEN catalog_sessions.last_imported_at_ms
                        WHEN excluded.file_size_bytes > catalog_sessions.file_size_bytes
                         AND catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.indexed_status IN ('indexed', 'completed_with_rejections')
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.indexed_file_size_bytes = catalog_sessions.file_size_bytes
                         AND catalog_sessions.indexed_file_modified_at_ms = catalog_sessions.file_modified_at_ms
                         AND catalog_sessions.indexed_import_revision = catalog_sessions.import_revision
                         AND catalog_sessions.last_imported_file_size_bytes > 0
                         AND catalog_sessions.last_imported_file_size_bytes <= catalog_sessions.file_size_bytes
                        THEN catalog_sessions.last_imported_at_ms
                        ELSE NULL
                    END,
                    last_imported_file_size_bytes = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                        THEN catalog_sessions.last_imported_file_size_bytes
                        WHEN excluded.file_size_bytes > catalog_sessions.file_size_bytes
                         AND catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.indexed_status IN ('indexed', 'completed_with_rejections')
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.indexed_file_size_bytes = catalog_sessions.file_size_bytes
                         AND catalog_sessions.indexed_file_modified_at_ms = catalog_sessions.file_modified_at_ms
                         AND catalog_sessions.indexed_import_revision = catalog_sessions.import_revision
                         AND catalog_sessions.last_imported_file_size_bytes > 0
                         AND catalog_sessions.last_imported_file_size_bytes <= catalog_sessions.file_size_bytes
                        THEN catalog_sessions.last_imported_file_size_bytes
                        ELSE NULL
                    END,
                    last_imported_file_modified_at_ms = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                        THEN catalog_sessions.last_imported_file_modified_at_ms
                        WHEN excluded.file_size_bytes > catalog_sessions.file_size_bytes
                         AND catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.indexed_status IN ('indexed', 'completed_with_rejections')
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.indexed_file_size_bytes = catalog_sessions.file_size_bytes
                         AND catalog_sessions.indexed_file_modified_at_ms = catalog_sessions.file_modified_at_ms
                         AND catalog_sessions.indexed_import_revision = catalog_sessions.import_revision
                         AND catalog_sessions.last_imported_file_size_bytes > 0
                         AND catalog_sessions.last_imported_file_size_bytes <= catalog_sessions.file_size_bytes
                        THEN catalog_sessions.last_imported_file_modified_at_ms
                        ELSE NULL
                    END,
                    last_imported_file_sha256 = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                        THEN catalog_sessions.last_imported_file_sha256
                        WHEN excluded.file_size_bytes > catalog_sessions.file_size_bytes
                         AND catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.indexed_status IN ('indexed', 'completed_with_rejections')
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.indexed_file_size_bytes = catalog_sessions.file_size_bytes
                         AND catalog_sessions.indexed_file_modified_at_ms = catalog_sessions.file_modified_at_ms
                         AND catalog_sessions.indexed_import_revision = catalog_sessions.import_revision
                         AND catalog_sessions.last_imported_file_size_bytes > 0
                         AND catalog_sessions.last_imported_file_size_bytes <= catalog_sessions.file_size_bytes
                        THEN catalog_sessions.last_imported_file_sha256
                        ELSE NULL
                    END,
                    last_imported_event_count = CASE
                        WHEN catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.file_size_bytes = excluded.file_size_bytes
                         AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                        THEN catalog_sessions.last_imported_event_count
                        WHEN excluded.file_size_bytes > catalog_sessions.file_size_bytes
                         AND catalog_sessions.provider IS excluded.provider
                         AND catalog_sessions.source_format IS excluded.source_format
                         AND catalog_sessions.source_root IS excluded.source_root
                         AND catalog_sessions.indexed_status IN ('indexed', 'completed_with_rejections')
                         AND catalog_sessions.import_revision = excluded.import_revision
                         AND catalog_sessions.indexed_file_size_bytes = catalog_sessions.file_size_bytes
                         AND catalog_sessions.indexed_file_modified_at_ms = catalog_sessions.file_modified_at_ms
                         AND catalog_sessions.indexed_import_revision = catalog_sessions.import_revision
                         AND catalog_sessions.last_imported_file_size_bytes > 0
                         AND catalog_sessions.last_imported_file_size_bytes <= catalog_sessions.file_size_bytes
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
                   OR catalog_sessions.import_revision != excluded.import_revision
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
                i64::from(session.import_revision),
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

    pub fn list_active_catalog_sessions_for_source(
        &self,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<Vec<CatalogSession>> {
        let mut stmt = self.conn.prepare(
            format!(
                "{} WHERE provider = ?1
                       AND source_root = ?2
                       AND is_stale = 0
                     ORDER BY session_started_at_ms, source_path",
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

    pub fn mark_catalog_source_indexed(
        &self,
        provider: CaptureProvider,
        update: CatalogSourceIndexUpdate<'_>,
    ) -> Result<usize> {
        self.record_catalog_source_import_result(
            provider,
            update,
            CatalogIndexedStatus::Indexed,
            None,
        )
    }

    pub fn record_catalog_source_import_result(
        &self,
        provider: CaptureProvider,
        update: CatalogSourceIndexUpdate<'_>,
        status: CatalogIndexedStatus,
        error: Option<&str>,
    ) -> Result<usize> {
        let changed = self.conn.execute(
            r#"
                UPDATE catalog_sessions
                SET indexed_at_ms = ?4,
                    indexed_file_size_bytes = ?5,
                    indexed_file_modified_at_ms = ?6,
                    indexed_status = ?8,
                    indexed_error = ?10,
                    indexed_event_count = ?7,
                    indexed_import_revision = ?12,
                    last_imported_at_ms = CASE WHEN ?11 THEN ?4 ELSE last_imported_at_ms END,
                    last_imported_file_size_bytes = CASE WHEN ?11 THEN ?5 ELSE last_imported_file_size_bytes END,
                    last_imported_file_modified_at_ms = CASE WHEN ?11 THEN ?6 ELSE last_imported_file_modified_at_ms END,
                    last_imported_file_sha256 = CASE WHEN ?11 THEN ?9 ELSE last_imported_file_sha256 END,
                    last_imported_event_count = CASE WHEN ?11 THEN ?7 ELSE last_imported_event_count END
                WHERE provider = ?1
                  AND source_root = ?2
                  AND source_path = ?3
                  AND is_stale = 0
                  AND file_size_bytes = ?5
                  AND file_modified_at_ms = ?6
                  AND import_revision = ?12
                "#,
            params![
                provider.as_str(),
                update.source_root,
                update.source_path,
                update.indexed_at_ms,
                capped_i64(update.file_size_bytes),
                update.file_modified_at_ms,
                update.event_count.map(capped_i64),
                status.as_str(),
                update.file_sha256,
                error,
                status.preserves_native_resume_checkpoint(),
                i64::from(update.import_revision),
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
                    file_size_bytes, file_modified_at_ms, import_revision, observed_at_ms, is_stale,
                    metadata_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9)
                ON CONFLICT(provider, source_root, source_path) DO UPDATE SET
                    source_format = excluded.source_format,
                    file_size_bytes = excluded.file_size_bytes,
                    file_modified_at_ms = excluded.file_modified_at_ms,
                    import_revision = excluded.import_revision,
                    observed_at_ms = excluded.observed_at_ms,
                    is_stale = 0,
                    indexed_at_ms = CASE
                        WHEN source_import_files.source_format IS excluded.source_format
                         AND source_import_files.file_size_bytes = excluded.file_size_bytes
                         AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                         AND source_import_files.import_revision = excluded.import_revision
                         AND (json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'source_root'
                              OR source_import_files.metadata_json IS excluded.metadata_json)
                        THEN source_import_files.indexed_at_ms
                        ELSE NULL
                    END,
                    indexed_file_size_bytes = CASE
                        WHEN source_import_files.source_format IS excluded.source_format
                         AND source_import_files.file_size_bytes = excluded.file_size_bytes
                         AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                         AND source_import_files.import_revision = excluded.import_revision
                         AND (json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'source_root'
                              OR source_import_files.metadata_json IS excluded.metadata_json)
                        THEN source_import_files.indexed_file_size_bytes
                        ELSE NULL
                    END,
                    indexed_file_modified_at_ms = CASE
                        WHEN source_import_files.source_format IS excluded.source_format
                         AND source_import_files.file_size_bytes = excluded.file_size_bytes
                         AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                         AND source_import_files.import_revision = excluded.import_revision
                         AND (json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'source_root'
                              OR source_import_files.metadata_json IS excluded.metadata_json)
                        THEN source_import_files.indexed_file_modified_at_ms
                        ELSE NULL
                    END,
                    indexed_status = CASE
                        WHEN source_import_files.source_format IS excluded.source_format
                         AND source_import_files.file_size_bytes = excluded.file_size_bytes
                         AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                         AND source_import_files.import_revision = excluded.import_revision
                         AND (json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'source_root'
                              OR source_import_files.metadata_json IS excluded.metadata_json)
                        THEN source_import_files.indexed_status
                        ELSE 'pending'
                    END,
                    indexed_error = CASE
                        WHEN source_import_files.source_format IS excluded.source_format
                         AND source_import_files.file_size_bytes = excluded.file_size_bytes
                         AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                         AND source_import_files.import_revision = excluded.import_revision
                         AND (json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'source_root'
                              OR source_import_files.metadata_json IS excluded.metadata_json)
                        THEN source_import_files.indexed_error
                        ELSE NULL
                    END,
                    indexed_import_revision = CASE
                        WHEN source_import_files.source_format IS excluded.source_format
                         AND source_import_files.file_size_bytes = excluded.file_size_bytes
                         AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                         AND source_import_files.import_revision = excluded.import_revision
                         AND (json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'source_root'
                              OR source_import_files.metadata_json IS excluded.metadata_json)
                        THEN source_import_files.indexed_import_revision
                        ELSE NULL
                    END,
                    metadata_json = excluded.metadata_json
                WHERE source_import_files.source_format IS NOT excluded.source_format
                   OR source_import_files.file_size_bytes != excluded.file_size_bytes
                   OR source_import_files.file_modified_at_ms != excluded.file_modified_at_ms
                   OR source_import_files.import_revision != excluded.import_revision
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
                i64::from(file.import_revision),
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
                       AND {}
                     ORDER BY source_path",
                source_import_file_select_sql(""),
                source_import_file_pending_condition_sql("source_import_files")
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
        self.record_source_import_file_result(provider, update, CatalogIndexedStatus::Indexed, None)
    }

    pub fn record_source_import_file_result(
        &self,
        provider: CaptureProvider,
        update: SourceImportFileIndexUpdate<'_>,
        status: CatalogIndexedStatus,
        error: Option<&str>,
    ) -> Result<usize> {
        let changed = self.conn.execute(
            r#"
                UPDATE source_import_files
                SET indexed_at_ms = ?4,
                    indexed_file_size_bytes = ?5,
                    indexed_file_modified_at_ms = ?6,
                    indexed_status = ?7,
                    indexed_error = ?8,
                    indexed_import_revision = ?9
                WHERE provider = ?1
                  AND source_root = ?2
                  AND source_path = ?3
                  AND is_stale = 0
                  AND file_size_bytes = ?5
                  AND file_modified_at_ms = ?6
                  AND import_revision = ?9
                "#,
            params![
                provider.as_str(),
                update.source_root,
                update.source_path,
                update.indexed_at_ms,
                capped_i64(update.file_size_bytes),
                update.file_modified_at_ms,
                status.as_str(),
                error,
                i64::from(update.import_revision),
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
        let completed_with_rejections = self.conn.query_row(
            "SELECT COUNT(*) FROM catalog_sessions WHERE is_stale = 0 AND indexed_status = 'completed_with_rejections'",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let rejected = self.conn.query_row(
            "SELECT COUNT(*) FROM catalog_sessions WHERE is_stale = 0 AND indexed_status = 'rejected'",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        Ok(CatalogCounts {
            total,
            indexed,
            stale,
            pending,
            failed,
            completed_with_rejections,
            rejected,
        })
    }

    pub fn source_import_file_counts(&self) -> Result<SourceImportFileCounts> {
        let total = self.conn.query_row(
            "SELECT COUNT(*) FROM source_import_files WHERE is_stale = 0",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let indexed = self.conn.query_row(
            r#"
            SELECT COUNT(*)
            FROM source_import_files
            WHERE is_stale = 0
              AND indexed_status IN ('indexed', 'completed_with_rejections')
              AND indexed_file_size_bytes = file_size_bytes
              AND indexed_file_modified_at_ms = file_modified_at_ms
              AND indexed_import_revision = import_revision
            "#,
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let stale = self.conn.query_row(
            "SELECT COUNT(*) FROM source_import_files WHERE is_stale != 0",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let pending = self.conn.query_row(
            format!(
                "SELECT COUNT(*) FROM source_import_files WHERE is_stale = 0 AND {}",
                source_import_file_pending_condition_sql("source_import_files")
            )
            .as_str(),
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let failed = self.conn.query_row(
            "SELECT COUNT(*) FROM source_import_files WHERE is_stale = 0 AND indexed_status = 'failed'",
            [],
            |row| row.get::<_, i64>(0),
            )? as usize;
        let completed_with_rejections = self.conn.query_row(
            "SELECT COUNT(*) FROM source_import_files WHERE is_stale = 0 AND indexed_status = 'completed_with_rejections'",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let rejected = self.conn.query_row(
            "SELECT COUNT(*) FROM source_import_files WHERE is_stale = 0 AND indexed_status = 'rejected'",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        Ok(SourceImportFileCounts {
            total,
            indexed,
            stale,
            pending,
            failed,
            completed_with_rejections,
            rejected,
        })
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
}

fn catalog_session_select_sql(tail: &str) -> String {
    format!(
        "SELECT source_path, provider, source_format, source_root, external_session_id, parent_external_session_id, agent_type, role_hint, external_agent_id, cwd, session_started_at_ms, file_size_bytes, file_modified_at_ms, import_revision, cataloged_at_ms, metadata_json FROM catalog_sessions {tail}"
    )
}

fn source_import_file_select_sql(tail: &str) -> String {
    format!(
        "SELECT provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, import_revision, observed_at_ms, metadata_json FROM source_import_files {tail}"
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
        import_revision: nonnegative_i64_to_u32(row.get(6)?)?,
        observed_at_ms: row.get(7)?,
        metadata: parse_json(row.get::<_, String>(8)?)?,
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
        import_revision: nonnegative_i64_to_u32(row.get(13)?)?,
        cataloged_at_ms: row.get(14)?,
        metadata: parse_json(row.get::<_, String>(15)?)?,
    })
}

fn catalog_pending_import_condition_sql(alias: &str) -> String {
    format!(
        r#"
        (
            {alias}.indexed_status IN ('pending', 'failed')
            OR {alias}.indexed_file_size_bytes IS NULL
            OR {alias}.indexed_file_modified_at_ms IS NULL
            OR {alias}.indexed_file_size_bytes != {alias}.file_size_bytes
            OR {alias}.indexed_file_modified_at_ms != {alias}.file_modified_at_ms
            OR {alias}.indexed_import_revision IS NULL
            OR {alias}.indexed_import_revision != {alias}.import_revision
            OR (
              {alias}.indexed_status = 'indexed'
              AND NOT EXISTS (
                SELECT 1
                FROM sessions AS session
                LEFT JOIN capture_sources AS source
                  ON source.id = session.capture_source_id
                WHERE session.provider = {alias}.provider
                  AND {alias}.external_session_id IS NOT NULL
                  AND session.external_session_id = {alias}.external_session_id
                  AND (
                      session.capture_source_id IS NULL
                      OR source.source_root = {alias}.source_root
                  )
                LIMIT 1
              )
            )
        )
        "#
    )
}

fn source_import_file_pending_condition_sql(alias: &str) -> String {
    format!(
        r#"
        (
            {alias}.indexed_status IN ('pending', 'failed')
            OR {alias}.indexed_file_size_bytes IS NULL
            OR {alias}.indexed_file_modified_at_ms IS NULL
            OR {alias}.indexed_file_size_bytes != {alias}.file_size_bytes
            OR {alias}.indexed_file_modified_at_ms != {alias}.file_modified_at_ms
            OR {alias}.indexed_import_revision IS NULL
            OR {alias}.indexed_import_revision != {alias}.import_revision
        )
        "#
    )
}

fn catalog_indexed_count_sql() -> String {
    r#"
    SELECT COUNT(*)
    FROM catalog_sessions AS catalog
    WHERE catalog.is_stale = 0
      AND catalog.indexed_status IN ('indexed', 'completed_with_rejections')
      AND catalog.indexed_file_size_bytes = catalog.file_size_bytes
      AND catalog.indexed_file_modified_at_ms = catalog.file_modified_at_ms
      AND catalog.indexed_import_revision = catalog.import_revision
      AND EXISTS (
        SELECT 1
        FROM sessions AS session
        LEFT JOIN capture_sources AS source
          ON source.id = session.capture_source_id
        WHERE session.provider = catalog.provider
          AND catalog.external_session_id IS NOT NULL
          AND session.external_session_id = catalog.external_session_id
          AND (
              session.capture_source_id IS NULL
              OR source.source_root = catalog.source_root
          )
        LIMIT 1
      )
    "#
    .to_owned()
}

#[cfg(test)]
mod tests;
