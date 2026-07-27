use ctx_history_core::CaptureProvider;
use rusqlite::{params, OptionalExtension};

use super::source_files::{
    source_import_file_from_row, source_import_file_is_not_control_sql,
    source_import_file_select_sql,
};
use crate::connection::collect_rows;
use crate::{Result, Store};

const SOURCE_IMPORT_FILE_PAGE_SIZE: usize = 64;

impl Store {
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

    pub fn reconcile_source_import_missing_paths_page(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        observed_at_ms: i64,
        after_source_path: Option<&str>,
    ) -> Result<Option<String>> {
        if !self.source_import_reconciliation_is_authorized(
            provider,
            source_root,
            observed_at_ms,
        )? {
            return Ok(None);
        }
        self.reconcile_source_import_missing_paths_page_inner(
            provider,
            source_root,
            observed_at_ms,
            after_source_path,
        )
    }

    pub fn reconcile_source_import_single_file_missing_paths_page(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        observed_at_ms: i64,
        after_source_path: Option<&str>,
    ) -> Result<Option<String>> {
        self.reconcile_source_import_missing_paths_page_inner(
            provider,
            source_root,
            observed_at_ms,
            after_source_path,
        )
    }

    fn reconcile_source_import_missing_paths_page_inner(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        observed_at_ms: i64,
        after_source_path: Option<&str>,
    ) -> Result<Option<String>> {
        let sql = source_import_missing_page_sql(after_source_path.is_some());
        let mut select = self.conn.prepare(&sql)?;
        let paths = match after_source_path {
            Some(after_source_path) => collect_rows(select.query_map(
                params![
                    provider.as_str(),
                    source_root,
                    observed_at_ms,
                    after_source_path
                ],
                |row| row.get::<_, String>(0),
            )?)?,
            None => collect_rows(select.query_map(
                params![provider.as_str(), source_root, observed_at_ms],
                |row| row.get::<_, String>(0),
            )?)?,
        };
        let Some(last_source_path) = paths.last().cloned() else {
            return Ok(None);
        };
        let mut update = self.conn.prepare(
            r#"
                UPDATE source_import_files
                SET is_stale = CASE
                        WHEN json_type(metadata_json, '$.inventory_missing_generation_v1') IS NULL
                        THEN 0
                        WHEN json_extract(metadata_json, '$.inventory_missing_generation_v1') < ?4
                        THEN 1
                        ELSE 0
                    END,
                    metadata_json = CASE
                        WHEN json_type(metadata_json, '$.inventory_missing_generation_v1') IS NULL
                        THEN json_set(
                            metadata_json,
                            '$.inventory_missing_generation_v1',
                            ?4
                        )
                        ELSE metadata_json
                    END
                WHERE provider = ?1
                  AND source_root = ?2
                  AND source_path = ?3
                  AND observed_at_ms < ?4
                  AND is_stale = 0
                "#,
        )?;
        for source_path in paths {
            update.execute(params![
                provider.as_str(),
                source_root,
                source_path,
                observed_at_ms,
            ])?;
        }
        Ok(Some(last_source_path))
    }

    pub fn mark_source_import_shadowed_paths_stale_page(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        observed_at_ms: i64,
        after_source_path: Option<&str>,
    ) -> Result<Option<String>> {
        if !self.source_import_reconciliation_is_authorized(
            provider,
            source_root,
            observed_at_ms,
        )? {
            return Ok(None);
        }
        let sql = source_import_shadowed_page_sql(after_source_path.is_some());
        let mut select = self.conn.prepare(&sql)?;
        let paths = match after_source_path {
            Some(after_source_path) => collect_rows(select.query_map(
                params![
                    provider.as_str(),
                    source_root,
                    observed_at_ms,
                    after_source_path
                ],
                |row| row.get::<_, String>(0),
            )?)?,
            None => collect_rows(select.query_map(
                params![provider.as_str(), source_root, observed_at_ms],
                |row| row.get::<_, String>(0),
            )?)?,
        };
        let Some(last_source_path) = paths.last().cloned() else {
            return Ok(None);
        };
        let mut update = self.conn.prepare(
            r#"
                UPDATE source_import_files
                SET is_stale = 1
                WHERE provider = ?1
                  AND source_root = ?2
                  AND source_path = ?3
                  AND observed_at_ms = ?4
                  AND is_stale = 0
                "#,
        )?;
        for source_path in paths {
            update.execute(params![
                provider.as_str(),
                source_root,
                source_path,
                observed_at_ms,
            ])?;
        }
        Ok(Some(last_source_path))
    }

    fn source_import_reconciliation_is_authorized(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        observed_at_ms: i64,
    ) -> Result<bool> {
        let control = self
            .conn
            .query_row(
                source_import_file_select_sql(
                    "WHERE provider = ?1 AND source_root = ?2 AND source_path = ?2 AND observed_at_ms = ?3",
                )
                .as_str(),
                params![provider.as_str(), source_root, observed_at_ms],
                source_import_file_from_row,
            )
            .optional()?;
        control.map_or(Ok(false), |file| {
            file.inventory_control_allows_reconciliation()
        })
    }
}

pub(super) fn source_import_missing_page_sql(has_cursor: bool) -> String {
    let cursor_clause = if has_cursor {
        "AND source_path > ?4"
    } else {
        ""
    };
    format!(
        r#"
        SELECT source_path
        FROM source_import_files
        WHERE provider = ?1
          AND source_root = ?2
          AND observed_at_ms < ?3
          AND is_stale = 0
          AND (
              json_type(metadata_json, '$.inventory_missing_generation_v1') IS NULL
              OR json_extract(metadata_json, '$.inventory_missing_generation_v1') < ?3
          )
          {}
          AND {}
        ORDER BY source_path
        LIMIT {}
        "#,
        cursor_clause,
        source_import_file_is_not_control_sql("source_import_files"),
        SOURCE_IMPORT_FILE_PAGE_SIZE,
    )
}

pub(super) fn source_import_shadowed_page_sql(has_cursor: bool) -> String {
    let cursor_clause = if has_cursor {
        "AND candidate.source_path > ?4"
    } else {
        ""
    };
    format!(
        r#"
        SELECT candidate.source_path
        FROM source_import_files AS candidate
        WHERE candidate.provider = ?1
          AND candidate.source_root = ?2
          AND candidate.observed_at_ms = ?3
          AND candidate.is_stale = 0
          {}
          AND json_type(candidate.metadata_json, '$.inventory_preferred_path_v1') = 'text'
          AND EXISTS (
              SELECT 1
              FROM source_import_files AS preferred
              WHERE preferred.provider = candidate.provider
                AND preferred.source_root = candidate.source_root
                AND preferred.source_path = json_extract(
                    candidate.metadata_json,
                    '$.inventory_preferred_path_v1'
                )
                AND preferred.observed_at_ms = candidate.observed_at_ms
                AND preferred.is_stale = 0
          )
        ORDER BY candidate.source_path
        LIMIT {}
        "#,
        cursor_clause, SOURCE_IMPORT_FILE_PAGE_SIZE,
    )
}
