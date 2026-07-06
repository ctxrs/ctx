#[allow(unused_imports)]
use super::*;

impl Store {
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
                    THEN catalog_sessions.indexed_at_ms
                    ELSE NULL
                END,
                indexed_file_size_bytes = CASE
                    WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                     AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN catalog_sessions.indexed_file_size_bytes
                    ELSE NULL
                END,
                indexed_file_modified_at_ms = CASE
                    WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                     AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN catalog_sessions.indexed_file_modified_at_ms
                    ELSE NULL
                END,
                indexed_status = CASE
                    WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                     AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN catalog_sessions.indexed_status
                    ELSE 'pending'
                END,
                indexed_error = CASE
                    WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                     AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN catalog_sessions.indexed_error
                    ELSE NULL
                END,
                indexed_event_count = CASE
                    WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                     AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN catalog_sessions.indexed_event_count
                    ELSE NULL
                END,
                last_imported_at_ms = CASE
                    WHEN catalog_sessions.file_size_bytes = excluded.file_size_bytes
                     AND catalog_sessions.file_modified_at_ms = excluded.file_modified_at_ms
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

    pub fn mark_catalog_source_indexed(
        &self,
        provider: CaptureProvider,
        update: CatalogSourceIndexUpdate<'_>,
    ) -> Result<usize> {
        let changed = self.conn.execute(
            r#"
            UPDATE catalog_sessions
            SET indexed_at_ms = ?4,
                indexed_file_size_bytes = ?5,
                indexed_file_modified_at_ms = ?6,
                indexed_status = ?8,
                indexed_error = NULL,
                indexed_event_count = ?7,
                last_imported_at_ms = ?4,
                last_imported_file_size_bytes = ?5,
                last_imported_file_modified_at_ms = ?6,
                last_imported_file_sha256 = ?9,
                last_imported_event_count = ?7
            WHERE provider = ?1
              AND source_root = ?2
              AND source_path = ?3
              AND is_stale = 0
            "#,
            params![
                provider.as_str(),
                update.source_root,
                update.source_path,
                update.indexed_at_ms,
                capped_i64(update.file_size_bytes),
                update.file_modified_at_ms,
                update.event_count.map(capped_i64),
                CatalogIndexedStatus::Indexed.as_str(),
                update.file_sha256,
            ],
        )?;
        Ok(changed)
    }

    pub fn mark_catalog_source_failed(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        source_path: &str,
        error: &str,
        indexed_at_ms: i64,
    ) -> Result<usize> {
        let changed = self.conn.execute(
            r#"
            UPDATE catalog_sessions
            SET indexed_at_ms = ?4,
                indexed_file_size_bytes = NULL,
                indexed_file_modified_at_ms = NULL,
                indexed_status = ?6,
                indexed_error = ?5,
                indexed_event_count = NULL
            WHERE provider = ?1
              AND source_root = ?2
              AND source_path = ?3
              AND is_stale = 0
            "#,
            params![
                provider.as_str(),
                source_root,
                source_path,
                indexed_at_ms,
                error,
                CatalogIndexedStatus::Failed.as_str(),
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
                file_size_bytes, file_modified_at_ms, observed_at_ms, is_stale,
                metadata_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8)
            ON CONFLICT(provider, source_root, source_path) DO UPDATE SET
                source_format = excluded.source_format,
                file_size_bytes = excluded.file_size_bytes,
                file_modified_at_ms = excluded.file_modified_at_ms,
                observed_at_ms = excluded.observed_at_ms,
                is_stale = 0,
                indexed_at_ms = CASE
                    WHEN source_import_files.file_size_bytes = excluded.file_size_bytes
                     AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN source_import_files.indexed_at_ms
                    ELSE NULL
                END,
                indexed_file_size_bytes = CASE
                    WHEN source_import_files.file_size_bytes = excluded.file_size_bytes
                     AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN source_import_files.indexed_file_size_bytes
                    ELSE NULL
                END,
                indexed_file_modified_at_ms = CASE
                    WHEN source_import_files.file_size_bytes = excluded.file_size_bytes
                     AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN source_import_files.indexed_file_modified_at_ms
                    ELSE NULL
                END,
                indexed_status = CASE
                    WHEN source_import_files.file_size_bytes = excluded.file_size_bytes
                     AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN source_import_files.indexed_status
                    ELSE 'pending'
                END,
                indexed_error = CASE
                    WHEN source_import_files.file_size_bytes = excluded.file_size_bytes
                     AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                    THEN source_import_files.indexed_error
                    ELSE NULL
                END,
                metadata_json = excluded.metadata_json
            WHERE source_import_files.source_format IS NOT excluded.source_format
               OR source_import_files.file_size_bytes != excluded.file_size_bytes
               OR source_import_files.file_modified_at_ms != excluded.file_modified_at_ms
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
                   AND (
                       indexed_status != 'indexed'
                       OR indexed_file_size_bytes IS NULL
                       OR indexed_file_modified_at_ms IS NULL
                       OR indexed_file_size_bytes != file_size_bytes
                       OR indexed_file_modified_at_ms != file_modified_at_ms
                   )
                 ORDER BY source_path",
                source_import_file_select_sql("")
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
        let changed = self.conn.execute(
            r#"
            UPDATE source_import_files
            SET indexed_at_ms = ?4,
                indexed_file_size_bytes = ?5,
                indexed_file_modified_at_ms = ?6,
                indexed_status = ?7,
                indexed_error = NULL
            WHERE provider = ?1
              AND source_root = ?2
              AND source_path = ?3
              AND is_stale = 0
            "#,
            params![
                provider.as_str(),
                update.source_root,
                update.source_path,
                update.indexed_at_ms,
                capped_i64(update.file_size_bytes),
                update.file_modified_at_ms,
                CatalogIndexedStatus::Indexed.as_str(),
            ],
        )?;
        Ok(changed)
    }

    pub fn mark_source_import_file_failed(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        source_path: &str,
        error: &str,
        indexed_at_ms: i64,
    ) -> Result<usize> {
        let changed = self.conn.execute(
            r#"
            UPDATE source_import_files
            SET indexed_at_ms = ?4,
                indexed_file_size_bytes = NULL,
                indexed_file_modified_at_ms = NULL,
                indexed_status = ?6,
                indexed_error = ?5
            WHERE provider = ?1
              AND source_root = ?2
              AND source_path = ?3
              AND is_stale = 0
            "#,
            params![
                provider.as_str(),
                source_root,
                source_path,
                indexed_at_ms,
                error,
                CatalogIndexedStatus::Failed.as_str(),
            ],
        )?;
        Ok(changed)
    }
}
