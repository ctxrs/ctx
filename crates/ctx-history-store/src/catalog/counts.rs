use ctx_history_core::CaptureProvider;
use rusqlite::params;

use super::session_completion::{catalog_indexed_count_sql, catalog_pending_import_condition_sql};
use super::source_files::{
    source_import_file_is_current_sql, source_import_file_is_not_control_sql,
    source_import_file_pending_condition_sql,
};
use crate::connection::nonnegative_i64_to_u64;
use crate::{Result, Store, StoreError};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CatalogCounts {
    pub total: usize,
    pub indexed: usize,
    pub stale: usize,
    pub pending: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceImportFileCounts {
    pub total: usize,
    pub indexed: usize,
    pub stale: usize,
    pub pending: usize,
    pub failed: usize,
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

impl Store {
    pub fn catalog_session_counts(&self) -> Result<CatalogCounts> {
        self.catalog_session_counts_for_projection(true)
    }

    pub fn catalog_session_counts_without_local_projection(&self) -> Result<CatalogCounts> {
        self.catalog_session_counts_for_projection(false)
    }

    fn catalog_session_counts_for_projection(
        &self,
        require_local_projection: bool,
    ) -> Result<CatalogCounts> {
        let total = self.conn.query_row(
            "SELECT COUNT(*) FROM catalog_sessions WHERE is_stale = 0",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let indexed = self.conn.query_row(
            catalog_indexed_count_sql(require_local_projection).as_str(),
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let stale = self.conn.query_row(
            "SELECT COUNT(*) FROM catalog_sessions WHERE is_stale != 0",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let pending = self.conn.query_row(
            format!(
                "SELECT COUNT(*) FROM catalog_sessions WHERE is_stale = 0 AND {}",
                catalog_pending_import_condition_sql("catalog_sessions", require_local_projection)
            )
            .as_str(),
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let failed = self.conn.query_row(
            "SELECT COUNT(*) FROM catalog_sessions
             WHERE is_stale = 0 AND indexed_status = 'failed'",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        Ok(CatalogCounts {
            total,
            indexed,
            stale,
            pending,
            failed,
        })
    }

    pub fn source_import_file_stats_for_source(
        &self,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<(usize, u64)> {
        self.conn
            .query_row(
                format!(
                    r#"
                    SELECT COUNT(*), COALESCE(SUM(file_size_bytes), 0)
                    FROM source_import_files
                    WHERE provider = ?1
                      AND source_root = ?2
                      AND is_stale = 0
                      AND json_type(metadata_json, '$.inventory_missing_generation_v1') IS NULL
                      AND {}
                    "#,
                    source_import_file_is_not_control_sql("source_import_files"),
                )
                .as_str(),
                params![provider.as_str(), source_root],
                |row| {
                    let count = row.get::<_, usize>(0)?;
                    let bytes = nonnegative_i64_to_u64(row.get(1)?)?;
                    Ok((count, bytes))
                },
            )
            .map_err(Into::into)
    }

    pub fn source_import_file_history_exists(
        &self,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<bool> {
        self.conn
            .query_row(
                format!(
                    r#"
                    SELECT EXISTS (
                        SELECT 1
                        FROM source_import_files
                        WHERE provider = ?1
                          AND source_root = ?2
                          AND {}
                        LIMIT 1
                    )
                    "#,
                    source_import_file_is_not_control_sql("source_import_files"),
                )
                .as_str(),
                params![provider.as_str(), source_root],
                |row| row.get::<_, i64>(0).map(|exists| exists != 0),
            )
            .map_err(Into::into)
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

    pub fn source_import_file_counts(&self) -> Result<SourceImportFileCounts> {
        let total = self.conn.query_row(
            format!(
                "SELECT COUNT(*) FROM source_import_files WHERE is_stale = 0 AND {}",
                source_import_file_is_current_sql("source_import_files")
            )
            .as_str(),
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let indexed = self.conn.query_row(
            format!(
                r#"
                SELECT COUNT(*)
                FROM source_import_files
                WHERE is_stale = 0
                  AND indexed_status = 'indexed'
                  AND indexed_file_size_bytes = file_size_bytes
                  AND indexed_file_modified_at_ms = file_modified_at_ms
                  AND {}
                "#,
                source_import_file_is_current_sql("source_import_files")
            )
            .as_str(),
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let stale = self.conn.query_row(
            format!(
                "SELECT COUNT(*) FROM source_import_files WHERE is_stale != 0 AND {}",
                source_import_file_is_not_control_sql("source_import_files")
            )
            .as_str(),
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let pending = self.conn.query_row(
            format!(
                "SELECT COUNT(*) FROM source_import_files WHERE is_stale = 0 AND {} AND {}",
                source_import_file_is_current_sql("source_import_files"),
                source_import_file_pending_condition_sql("source_import_files")
            )
            .as_str(),
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let failed = self.conn.query_row(
            format!(
                "SELECT COUNT(*) FROM source_import_files WHERE is_stale = 0 AND indexed_status = 'failed' AND {}",
                source_import_file_is_current_sql("source_import_files")
            )
            .as_str(),
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        Ok(SourceImportFileCounts {
            total,
            indexed,
            stale,
            pending,
            failed,
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
