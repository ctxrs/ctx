impl Store {
    pub fn upsert_source_import_files(
        &self,
        inventory_generation: u64,
        files: &[SourceImportFile],
    ) -> Result<usize> {
        if files.is_empty() {
            return Ok(0);
        }
        let mut stmt = self.conn.prepare(
            r#"
                INSERT INTO source_import_files (
                    provider, source_format, source_root, source_path,
                    file_size_bytes, file_modified_at_ms, import_revision, observed_at_ms, is_stale,
                    pending_reason, metadata_json
                )
                SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9, ?10
                WHERE EXISTS (
                    SELECT 1
                    FROM import_inventory_generations AS inventory
                    WHERE inventory.provider = ?1
                      AND inventory.source_root = ?3
                      AND inventory.inventory_family = 'source_import_files'
                      AND inventory.current_generation = ?11
                )
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
                         AND ((json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'source_root'
                               AND json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'logical_import_unit')
                              OR source_import_files.metadata_json IS excluded.metadata_json)
                        THEN source_import_files.indexed_at_ms
                        ELSE NULL
                    END,
                    indexed_file_size_bytes = CASE
                        WHEN source_import_files.source_format IS excluded.source_format
                         AND source_import_files.file_size_bytes = excluded.file_size_bytes
                         AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                         AND source_import_files.import_revision = excluded.import_revision
                         AND ((json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'source_root'
                               AND json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'logical_import_unit')
                              OR source_import_files.metadata_json IS excluded.metadata_json)
                        THEN source_import_files.indexed_file_size_bytes
                        ELSE NULL
                    END,
                    indexed_file_modified_at_ms = CASE
                        WHEN source_import_files.source_format IS excluded.source_format
                         AND source_import_files.file_size_bytes = excluded.file_size_bytes
                         AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                         AND source_import_files.import_revision = excluded.import_revision
                         AND ((json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'source_root'
                               AND json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'logical_import_unit')
                              OR source_import_files.metadata_json IS excluded.metadata_json)
                        THEN source_import_files.indexed_file_modified_at_ms
                        ELSE NULL
                    END,
                    indexed_status = CASE
                        WHEN source_import_files.source_format IS excluded.source_format
                         AND source_import_files.file_size_bytes = excluded.file_size_bytes
                         AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                         AND source_import_files.import_revision = excluded.import_revision
                         AND ((json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'source_root'
                               AND json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'logical_import_unit')
                              OR source_import_files.metadata_json IS excluded.metadata_json)
                        THEN source_import_files.indexed_status
                        WHEN excluded.file_size_bytes > source_import_files.file_size_bytes
                         AND source_import_files.source_format IS excluded.source_format
                         AND source_import_files.import_revision = excluded.import_revision
                         AND source_import_files.indexed_status = 'completed_with_rejections'
                         AND source_import_files.indexed_file_size_bytes = source_import_files.file_size_bytes
                         AND source_import_files.indexed_file_modified_at_ms = source_import_files.file_modified_at_ms
                         AND source_import_files.indexed_import_revision = source_import_files.import_revision
                        THEN source_import_files.indexed_status
                        ELSE 'pending'
                    END,
                    indexed_error = CASE
                        WHEN source_import_files.source_format IS excluded.source_format
                         AND source_import_files.file_size_bytes = excluded.file_size_bytes
                         AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                         AND source_import_files.import_revision = excluded.import_revision
                         AND ((json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'source_root'
                               AND json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'logical_import_unit')
                              OR source_import_files.metadata_json IS excluded.metadata_json)
                        THEN source_import_files.indexed_error
                        WHEN excluded.file_size_bytes > source_import_files.file_size_bytes
                         AND source_import_files.source_format IS excluded.source_format
                         AND source_import_files.import_revision = excluded.import_revision
                         AND source_import_files.indexed_status = 'completed_with_rejections'
                         AND source_import_files.indexed_file_size_bytes = source_import_files.file_size_bytes
                         AND source_import_files.indexed_file_modified_at_ms = source_import_files.file_modified_at_ms
                         AND source_import_files.indexed_import_revision = source_import_files.import_revision
                        THEN source_import_files.indexed_error
                        ELSE NULL
                    END,
                    indexed_import_revision = CASE
                        WHEN source_import_files.source_format IS excluded.source_format
                         AND source_import_files.file_size_bytes = excluded.file_size_bytes
                         AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                         AND source_import_files.import_revision = excluded.import_revision
                         AND ((json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'source_root'
                               AND json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'logical_import_unit')
                              OR source_import_files.metadata_json IS excluded.metadata_json)
                        THEN source_import_files.indexed_import_revision
                        ELSE NULL
                    END,
                    pending_reason = excluded.pending_reason,
                    metadata_json = excluded.metadata_json
                WHERE EXISTS (
                    SELECT 1
                    FROM import_inventory_generations AS inventory
                    WHERE inventory.provider = excluded.provider
                      AND inventory.source_root = excluded.source_root
                      AND inventory.inventory_family = 'source_import_files'
                      AND inventory.current_generation = ?11
                )
                  AND (
                       source_import_files.source_format IS NOT excluded.source_format
                    OR source_import_files.file_size_bytes != excluded.file_size_bytes
                    OR source_import_files.file_modified_at_ms != excluded.file_modified_at_ms
                    OR source_import_files.import_revision != excluded.import_revision
                    OR source_import_files.is_stale != 0
                    OR source_import_files.metadata_json IS NOT excluded.metadata_json
                    OR source_import_files.pending_reason IS NOT excluded.pending_reason
                  )
                "#,
        )?;
        let mut changed = 0;
        for file in files {
            let pending_reason = self.classify_source_import_pending_reason(file)?;
            changed += stmt.execute(params![
                file.provider.as_str(),
                file.source_format.as_str(),
                file.source_root.as_str(),
                file.source_path.as_str(),
                capped_i64(file.file_size_bytes),
                file.file_modified_at_ms,
                i64::from(file.import_revision),
                file.observed_at_ms,
                pending_reason.map(ImportPendingReason::as_str),
                serde_json::to_string(&file.metadata)?,
                capped_i64(inventory_generation),
            ])?;
        }
        Ok(changed)
    }

    pub fn mark_source_import_missing_paths_stale(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        current_paths: &[String],
        observed_at_ms: i64,
        inventory_generation: u64,
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
                  AND EXISTS (
                      SELECT 1
                      FROM import_inventory_generations AS inventory
                      WHERE inventory.provider = ?1
                        AND inventory.source_root = ?2
                        AND inventory.inventory_family = 'source_import_files'
                        AND inventory.current_generation = ?4
                  )
                  AND NOT EXISTS (
                      SELECT 1
                      FROM temp_source_import_current_paths AS current
                      WHERE current.source_path = source_import_files.source_path
                  )
                "#,
            params![
                provider.as_str(),
                source_root,
                observed_at_ms,
                capped_i64(inventory_generation)
            ],
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
        let visible = crate::provider_files::source_import_file_material_visible_predicate(
            "source_import_files",
        );
        let mut stmt = self.conn.prepare(
            format!(
                "{} WHERE provider = ?1
                       AND source_root = ?2
                       AND is_stale = 0
                       AND {visible}
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

    pub fn list_source_import_file_work(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        class: ImportWorkClass,
        limit: usize,
    ) -> Result<Vec<SourceImportFileWork>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let predicate = import_work_class_predicate("source_file", class);
        let order = import_work_order("source_file", class);
        let mut stmt = self.conn.prepare(&format!(
            r#"
            SELECT provider, source_format, source_root, source_path,
                   file_size_bytes, file_modified_at_ms, import_revision,
                   observed_at_ms, metadata_json, pending_reason,
                   CASE
                     WHEN pending_reason = 'fresh_append' THEN MAX(
                       file_size_bytes - COALESCE((
                         SELECT checkpoint.committed_byte_offset
                         FROM provider_file_checkpoints AS checkpoint
                         WHERE checkpoint.provider = source_file.provider
                           AND checkpoint.source_format = source_file.source_format
                           AND checkpoint.source_root = source_file.source_root
                           AND checkpoint.source_path = source_file.source_path
                       ), 0),
                       0
                     )
                     ELSE file_size_bytes
                   END,
                   indexed_at_ms
            FROM source_import_files AS source_file
            WHERE provider = ?1 AND source_root = ?2 AND is_stale = 0
              AND {predicate}
            ORDER BY {order}
            LIMIT ?3
            "#
        ))?;
        let rows = stmt.query_map(
            params![provider.as_str(), source_root, capped_i64(limit as u64)],
            |row| {
                Ok(SourceImportFileWork {
                    file: source_import_file_from_row(row)?,
                    reason: parse_text_enum(row.get(9)?)?,
                    estimated_bytes: nonnegative_i64_to_u64(row.get(10)?)?,
                    last_attempt_at_ms: row.get(11)?,
                })
            },
        )?;
        collect_rows(rows)
    }

    pub fn source_import_file_work_count(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        class: ImportWorkClass,
    ) -> Result<usize> {
        let predicate = import_work_class_predicate("source_file", class);
        self.conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM source_import_files AS source_file \
                     WHERE provider = ?1 AND source_root = ?2 AND is_stale = 0 \
                       AND {predicate}"
                ),
                params![provider.as_str(), source_root],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub fn schedule_source_import_explicit_rescan(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        inventory_generation: u64,
    ) -> Result<usize> {
        self.conn
            .execute(
                r#"
                UPDATE source_import_files
                SET pending_reason = 'explicit_rescan'
                WHERE provider = ?1 AND source_root = ?2 AND is_stale = 0
                  AND EXISTS (
                    SELECT 1 FROM import_inventory_generations AS inventory
                    WHERE inventory.provider = ?1 AND inventory.source_root = ?2
                      AND inventory.inventory_family = 'source_import_files'
                      AND inventory.current_generation = ?3
                  )
                "#,
                params![
                    provider.as_str(),
                    source_root,
                    capped_i64(inventory_generation)
                ],
            )
            .map_err(Into::into)
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
        self.with_provider_file_inventory_result_write(
            provider,
            update.source_root,
            update.source_path,
            || self.record_source_import_file_result_inner(provider, update, status, error),
        )
    }

    fn record_source_import_file_result_inner(
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
                    indexed_import_revision = ?9,
                    pending_reason = CASE
                        WHEN ?7 = 'failed' THEN CASE
                            WHEN pending_reason IN ('fresh_append', 'recovery_retry')
                                THEN 'recovery_retry'
                            ELSE 'recovery_replacement'
                        END
                        WHEN ?7 = 'pending' THEN COALESCE(pending_reason, 'legacy')
                        ELSE NULL
                    END
                WHERE provider = ?1
                  AND source_root = ?2
                  AND source_path = ?3
                  AND is_stale = 0
                  AND file_size_bytes = ?5
                  AND file_modified_at_ms = ?6
                  AND import_revision = ?9
                  AND metadata_json IS ?11
                  AND EXISTS (
                      SELECT 1
                      FROM import_inventory_generations AS inventory
                      WHERE inventory.provider = ?1
                        AND inventory.source_root = ?2
                        AND inventory.inventory_family = 'source_import_files'
                        AND inventory.current_generation = ?10
                  )
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
                capped_i64(update.inventory_generation),
                serde_json::to_string(update.metadata)?,
            ],
        )?;
        Ok(changed)
    }
}
