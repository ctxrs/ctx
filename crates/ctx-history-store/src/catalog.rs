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
    pub inventory_generation: u64,
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
    pub inventory_generation: u64,
    pub metadata: &'a Value,
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

include!("catalog/inventory.rs");
include!("catalog/pending_work.rs");
include!("catalog/source_imports.rs");
include!("catalog/counts.rs");

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
              {alias}.indexed_status IN ('indexed', 'completed_with_rejections')
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
                      OR source.raw_source_path = {alias}.source_path
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
              OR source.raw_source_path = catalog.source_path
          )
        LIMIT 1
      )
    "#
    .to_owned()
}

#[cfg(test)]
mod tests;
