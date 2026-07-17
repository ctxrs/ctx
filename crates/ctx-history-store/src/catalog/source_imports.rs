const SOURCE_IMPORT_PERSIST_BATCH_ROWS: usize = 64;
const SOURCE_IMPORT_PERSIST_BATCH_BYTES: u64 = 8 * 1024 * 1024;
const SOURCE_IMPORT_PERSIST_ROW_OVERHEAD_BYTES: u64 = 256;
const SOURCE_IMPORT_ACTIVE_PATH_INVENTORY_PAGE_SQL: &str = "SELECT rowid, source_path \
     FROM source_import_files INDEXED BY idx_source_import_files_provider_source_root_stale \
     WHERE provider = ?1 AND source_root = ?2 AND is_stale = 0 AND rowid > ?3 \
     ORDER BY rowid LIMIT ?4";
const SOURCE_IMPORT_EXPLICIT_RESCAN_PAGE_SQL: &str = "SELECT rowid \
     FROM source_import_files INDEXED BY idx_source_import_files_provider_source_root_stale \
     WHERE provider = ?1 AND source_root = ?2 AND is_stale = 0 AND rowid > ?3 \
     ORDER BY rowid LIMIT ?4";

fn source_import_persist_row_bytes(file: &SourceImportFile, metadata_json: &str) -> u64 {
    [
        file.provider.as_str(),
        file.source_format.as_str(),
        file.source_root.as_str(),
        file.source_path.as_str(),
        metadata_json,
    ]
    .into_iter()
    .fold(SOURCE_IMPORT_PERSIST_ROW_OVERHEAD_BYTES, |bytes, value| {
        bytes.saturating_add(value.len() as u64)
    })
}

fn source_import_current_path_batch(current_paths: &[String]) -> (usize, u64) {
    let mut count = 0;
    let mut bytes = 0_u64;
    for source_path in current_paths.iter().take(SOURCE_IMPORT_PERSIST_BATCH_ROWS) {
        let row_bytes =
            SOURCE_IMPORT_PERSIST_ROW_OVERHEAD_BYTES.saturating_add(source_path.len() as u64);
        if count > 0 && bytes.saturating_add(row_bytes) > SOURCE_IMPORT_PERSIST_BATCH_BYTES {
            break;
        }
        count += 1;
        bytes = bytes.saturating_add(row_bytes);
        if bytes >= SOURCE_IMPORT_PERSIST_BATCH_BYTES {
            break;
        }
    }
    (count, bytes)
}

impl Store {
    #[doc(hidden)]
    pub fn list_source_import_inventory_paths_page(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        after_rowid: Option<i64>,
        limit: usize,
    ) -> Result<Vec<(i64, String)>> {
        let limit = limit.clamp(1, 64);
        let mut statement = self
            .conn
            .prepare(SOURCE_IMPORT_ACTIVE_PATH_INVENTORY_PAGE_SQL)?;
        collect_rows(statement.query_map(
            params![
                provider.as_str(),
                source_root,
                after_rowid.unwrap_or(0),
                capped_i64(limit as u64)
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?)
    }

    #[doc(hidden)]
    pub fn mark_source_import_inventory_paths_stale(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        paths: &[String],
        observed_at_ms: i64,
        inventory_generation: u64,
    ) -> Result<usize> {
        if paths.is_empty() {
            return Ok(0);
        }
        if paths.len() > Self::INVENTORY_PATH_PAGE_LIMIT {
            return Err(StoreError::Sql(rusqlite::Error::InvalidQuery));
        }
        let placeholders = vec!["?"; paths.len()].join(", ");
        let sql = format!(
            "UPDATE source_import_files SET is_stale = 1, observed_at_ms = ?3 \
             WHERE provider = ?1 AND source_root = ?2 \
               AND EXISTS (\
                   SELECT 1 FROM import_inventory_generations AS inventory \
                   WHERE inventory.provider = ?1 AND inventory.source_root = ?2 \
                     AND inventory.inventory_family = 'source_import_files' \
                     AND inventory.current_generation = ?4\
               ) \
               AND source_path IN ({placeholders})"
        );
        let mut parameters: Vec<rusqlite::types::Value> = vec![
            provider.as_str().to_owned().into(),
            source_root.to_owned().into(),
            observed_at_ms.into(),
            capped_i64(inventory_generation).into(),
        ];
        parameters.extend(paths.iter().cloned().map(Into::into));
        self.conn
            .execute(&sql, rusqlite::params_from_iter(parameters))
            .map_err(StoreError::from)
    }

    fn classify_source_import_pending_reason(
        &self,
        file: &SourceImportFile,
        metadata_json: &str,
    ) -> Result<Option<ImportPendingReason>> {
        let prior = self
            .conn
            .query_row(
                r#"
                SELECT source_format, file_size_bytes, file_modified_at_ms,
                       import_revision, is_stale,
                       indexed_file_size_bytes, indexed_file_modified_at_ms,
                       indexed_status, indexed_import_revision, pending_reason, metadata_json
                FROM source_import_files
                WHERE provider = ?1 AND source_root = ?2 AND source_path = ?3
                "#,
                params![file.provider.as_str(), &file.source_root, &file.source_path],
                |row| {
                    Ok(SourceImportPendingState {
                        source_format: row.get(0)?,
                        file_size_bytes: nonnegative_i64_to_u64(row.get(1)?)?,
                        file_modified_at_ms: row.get(2)?,
                        import_revision: nonnegative_i64_to_u32(row.get(3)?)?,
                        is_stale: row.get(4)?,
                        indexed_file_size_bytes: row
                            .get::<_, Option<i64>>(5)?
                            .map(nonnegative_i64_to_u64)
                            .transpose()?,
                        indexed_file_modified_at_ms: row.get(6)?,
                        indexed_status: parse_text_enum(row.get(7)?)?,
                        indexed_import_revision: row
                            .get::<_, Option<i64>>(8)?
                            .map(nonnegative_i64_to_u32)
                            .transpose()?,
                        pending_reason: row
                            .get::<_, Option<String>>(9)?
                            .map(parse_text_enum)
                            .transpose()?,
                        metadata_json: row.get(10)?,
                    })
                },
            )
            .optional()?;
        let Some(prior) = prior else {
            return Ok(Some(ImportPendingReason::FreshNew));
        };
        if self.provider_file_publication_was_abandoned(
            file.provider,
            "source_import_files",
            &prior.source_format,
            &file.source_root,
            &file.source_path,
        )? {
            return Ok(Some(ImportPendingReason::AbandonedPublication));
        }
        let same_identity = prior.source_format == file.source_format;
        let same_fingerprint = same_identity
            && prior.file_size_bytes == file.file_size_bytes
            && prior.file_modified_at_ms == file.file_modified_at_ms
            && prior.import_revision == file.import_revision
            && prior.metadata_json == metadata_json
            && !prior.is_stale;
        if same_fingerprint && prior.pending_reason == Some(ImportPendingReason::ExplicitRescan) {
            return Ok(prior.pending_reason);
        }
        if !same_fingerprint {
            if let Some(reason) = prior
                .pending_reason
                .filter(|reason| reason.requires_replacement())
            {
                return Ok(Some(reason));
            }
            let parser_revision_only = same_identity
                && prior.file_size_bytes == file.file_size_bytes
                && prior.file_modified_at_ms == file.file_modified_at_ms
                && prior.metadata_json == metadata_json
                && prior.import_revision != file.import_revision
                && !prior.is_stale;
            if parser_revision_only {
                return Ok(Some(ImportPendingReason::ParserRevision));
            }
            let grew_in_place = same_identity
                && prior.import_revision == file.import_revision
                && source_import_metadata_matches_owner_growth(
                    &prior.metadata_json,
                    &file.metadata,
                )
                && !prior.is_stale
                && file.file_size_bytes > prior.file_size_bytes;
            if grew_in_place
                && matches!(
                    prior.pending_reason,
                    Some(ImportPendingReason::FreshAppend | ImportPendingReason::RecoveryRetry)
                )
                && self.source_import_incremental_material_is_supported(&prior, file)?
            {
                return Ok(prior.pending_reason);
            }
            if self.source_import_observation_is_append(&prior, file)? {
                return Ok(Some(ImportPendingReason::FreshAppend));
            }
            return Ok(Some(ImportPendingReason::FreshChanged));
        }
        match prior.indexed_status {
            CatalogIndexedStatus::Failed => Ok(Some(ImportPendingReason::retry_after_failure(
                prior.pending_reason,
            ))),
            CatalogIndexedStatus::Pending => Ok(Some(
                prior.pending_reason.unwrap_or(ImportPendingReason::Legacy),
            )),
            CatalogIndexedStatus::Indexed | CatalogIndexedStatus::CompletedWithRejections => {
                let indexed_matches = prior.indexed_file_size_bytes == Some(file.file_size_bytes)
                    && prior.indexed_file_modified_at_ms == Some(file.file_modified_at_ms);
                if prior.indexed_import_revision != Some(file.import_revision) {
                    Ok(Some(ImportPendingReason::ParserRevision))
                } else if !indexed_matches {
                    Ok(Some(
                        prior.pending_reason.unwrap_or(ImportPendingReason::Legacy),
                    ))
                } else if !self.source_import_material_exists(file)? {
                    Ok(Some(ImportPendingReason::MissingMaterial))
                } else {
                    Ok(None)
                }
            }
            CatalogIndexedStatus::Rejected => Ok(None),
        }
    }

    fn source_import_observation_is_append(
        &self,
        prior: &SourceImportPendingState,
        file: &SourceImportFile,
    ) -> Result<bool> {
        if prior.source_format != file.source_format
            || prior.import_revision != file.import_revision
            || prior.is_stale
            || file.file_size_bytes <= prior.file_size_bytes
            || !matches!(
                prior.indexed_status,
                CatalogIndexedStatus::Indexed | CatalogIndexedStatus::CompletedWithRejections
            )
            || prior.indexed_file_size_bytes != Some(prior.file_size_bytes)
            || prior.indexed_file_modified_at_ms != Some(prior.file_modified_at_ms)
            || prior.indexed_import_revision != Some(prior.import_revision)
        {
            return Ok(false);
        }
        Ok(self.provider_file_checkpoint_matches_prior_observation(
            file.provider,
            &file.source_format,
            &file.source_root,
            &file.source_path,
            file.import_revision,
            prior.file_size_bytes,
        )? && self.source_import_material_exists(file)?)
    }

    fn source_import_incremental_material_is_supported(
        &self,
        prior: &SourceImportPendingState,
        file: &SourceImportFile,
    ) -> Result<bool> {
        let checkpoint_precedes_growth = self
            .provider_file_checkpoint(ProviderFileCheckpointKey {
                provider: file.provider,
                source_format: &file.source_format,
                source_root: &file.source_root,
                source_path: &file.source_path,
            })?
            .is_some_and(|checkpoint| {
                checkpoint.import_revision == file.import_revision
                    && checkpoint.committed_byte_offset <= prior.file_size_bytes
            });
        Ok(checkpoint_precedes_growth && self.source_import_material_exists(file)?)
    }

    fn source_import_material_exists(&self, file: &SourceImportFile) -> Result<bool> {
        let metadata_json = serde_json::to_string(&file.metadata)?;
        let material_source_format =
            expected_material_source_format(file.provider, &file.source_format);
        self.conn
            .query_row(
                r#"
                SELECT EXISTS (
                    SELECT 1
                    FROM capture_sources AS source
                    WHERE source.provider = ?1
                      AND source.source_format = ?2
                      AND (
                          (
                              json_extract(?5, '$.inventory_unit') = 'source_root'
                              AND source.source_root = ?3
                          )
                          OR (
                              json_extract(?5, '$.inventory_unit') IS NOT 'source_root'
                              AND source.raw_source_path = ?4
                              AND (
                                  source.source_root = ?3
                                  OR source.source_root = source.raw_source_path
                                  OR source.source_root IS NULL
                              )
                          )
                      )
                    LIMIT 1
                )
                "#,
                params![
                    file.provider.as_str(),
                    material_source_format,
                    &file.source_root,
                    &file.source_path,
                    metadata_json
                ],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    fn provider_file_checkpoint_matches_prior_observation(
        &self,
        provider: CaptureProvider,
        source_format: &str,
        source_root: &str,
        source_path: &str,
        import_revision: u32,
        prior_size_bytes: u64,
    ) -> Result<bool> {
        self.conn
            .query_row(
                r#"
                SELECT EXISTS (
                    SELECT 1
                    FROM provider_file_checkpoints
                    WHERE provider = ?1 AND source_format = ?2
                      AND source_root = ?3 AND source_path = ?4
                      AND import_revision = ?5
                      AND committed_byte_offset <= ?6
                )
                "#,
                params![
                    provider.as_str(),
                    source_format,
                    source_root,
                    source_path,
                    i64::from(import_revision),
                    capped_i64(prior_size_bytes)
                ],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    fn provider_file_publication_was_abandoned(
        &self,
        provider: CaptureProvider,
        inventory_family: &str,
        source_format: &str,
        source_root: &str,
        source_path: &str,
    ) -> Result<bool> {
        self.conn
            .query_row(
                r#"
                SELECT EXISTS (
                    SELECT 1
                    FROM provider_file_publications
                    WHERE provider = ?1 AND inventory_family = ?2
                      AND inventory_source_format = ?3
                      AND inventory_source_root = ?4 AND source_path = ?5
                      AND mutation_started = 1
                )
                "#,
                params![
                    provider.as_str(),
                    inventory_family,
                    source_format,
                    source_root,
                    source_path
                ],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub fn upsert_source_import_files(
        &self,
        inventory_generation: u64,
        files: &[SourceImportFile],
    ) -> Result<usize> {
        self.upsert_source_import_files_with_pacing(inventory_generation, files, |_| {})
    }

    #[doc(hidden)]
    pub fn upsert_source_import_files_with_pacing(
        &self,
        inventory_generation: u64,
        files: &[SourceImportFile],
        mut pace: impl FnMut(u64),
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
        let mut start = 0;
        while start < files.len() {
            let mut batch = Vec::with_capacity(
                SOURCE_IMPORT_PERSIST_BATCH_ROWS.min(files.len().saturating_sub(start)),
            );
            let mut batch_bytes = 0_u64;
            for file in files[start..].iter().take(SOURCE_IMPORT_PERSIST_BATCH_ROWS) {
                let metadata_json = serde_json::to_string(&file.metadata)?;
                let row_bytes = source_import_persist_row_bytes(file, &metadata_json);
                if !batch.is_empty()
                    && batch_bytes.saturating_add(row_bytes) > SOURCE_IMPORT_PERSIST_BATCH_BYTES
                {
                    break;
                }
                batch_bytes = batch_bytes.saturating_add(row_bytes);
                batch.push((file, metadata_json));
                if batch_bytes >= SOURCE_IMPORT_PERSIST_BATCH_BYTES {
                    break;
                }
            }

            pace(batch_bytes);
            let batch_len = batch.len();
            for (file, metadata_json) in batch {
                let pending_reason =
                    self.classify_source_import_pending_reason(file, &metadata_json)?;
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
                    metadata_json,
                    capped_i64(inventory_generation),
                ])?;
            }
            start = start.saturating_add(batch_len);
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
        self.mark_source_import_missing_paths_stale_with_pacing(
            provider,
            source_root,
            current_paths,
            observed_at_ms,
            inventory_generation,
            |_| {},
        )
    }

    #[doc(hidden)]
    pub fn mark_source_import_missing_paths_stale_with_pacing(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        current_paths: &[String],
        observed_at_ms: i64,
        inventory_generation: u64,
        mut pace: impl FnMut(u64),
    ) -> Result<usize> {
        if self.conn.is_autocommit() {
            return with_immediate_transaction(&self.conn, || {
                self.mark_source_import_missing_paths_stale_with_pacing(
                    provider,
                    source_root,
                    current_paths,
                    observed_at_ms,
                    inventory_generation,
                    pace,
                )
            });
        }
        self.conn.execute_batch(
                "CREATE TEMP TABLE IF NOT EXISTS temp_source_import_current_paths (source_path TEXT PRIMARY KEY)",
            )?;
        self.conn
            .execute("DELETE FROM temp_source_import_current_paths", [])?;
        {
            let mut stmt = self.conn.prepare(
                "INSERT OR IGNORE INTO temp_source_import_current_paths (source_path) VALUES (?1)",
            )?;
            let mut start = 0;
            while start < current_paths.len() {
                let (batch_len, batch_bytes) =
                    source_import_current_path_batch(&current_paths[start..]);
                pace(batch_bytes);
                for source_path in &current_paths[start..start + batch_len] {
                    stmt.execute(params![source_path])?;
                }
                start = start.saturating_add(batch_len);
            }
        }
        let mut changed = 0usize;
        loop {
            let (batch_rows, batch_bytes) = self.conn.query_row(
                r#"
                SELECT COUNT(*), COALESCE(SUM(
                    length(provider) + length(source_format) + length(source_root)
                    + length(source_path) + length(metadata_json) + ?4
                ), 0)
                FROM (
                    SELECT provider, source_format, source_root, source_path, metadata_json
                    FROM source_import_files
                    WHERE provider = ?1
                      AND source_root = ?2
                      AND is_stale = 0
                      AND EXISTS (
                          SELECT 1
                          FROM import_inventory_generations AS inventory
                          WHERE inventory.provider = ?1
                            AND inventory.source_root = ?2
                            AND inventory.inventory_family = 'source_import_files'
                            AND inventory.current_generation = ?3
                      )
                      AND NOT EXISTS (
                          SELECT 1
                          FROM temp_source_import_current_paths AS current
                          WHERE current.source_path = source_import_files.source_path
                      )
                    ORDER BY source_path
                    LIMIT 64
                )
                "#,
                params![
                    provider.as_str(),
                    source_root,
                    capped_i64(inventory_generation),
                    capped_i64(SOURCE_IMPORT_PERSIST_ROW_OVERHEAD_BYTES),
                ],
                |row| {
                    Ok((
                        row.get::<_, usize>(0)?,
                        nonnegative_i64_to_u64(row.get(1)?)?,
                    ))
                },
            )?;
            if batch_rows == 0 {
                break;
            }
            pace(batch_bytes);
            let batch_changed = self.conn.execute(
                r#"
                UPDATE source_import_files
                SET is_stale = 1, observed_at_ms = ?3
                WHERE rowid IN (
                    SELECT rowid
                    FROM source_import_files
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
                    ORDER BY source_path
                    LIMIT 64
                  )
                "#,
                params![
                    provider.as_str(),
                    source_root,
                    observed_at_ms,
                    capped_i64(inventory_generation)
                ],
            )?;
            changed = changed.saturating_add(batch_changed);
            if batch_changed == 0 {
                break;
            }
        }
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
        let active_publication =
            crate::provider_files::source_file_candidate_is_global_publication("source_file");
        let order = import_work_order("source_file", class);
        let active_path = self
            .effective_provider_file_publication_inventory_owner()?
            .filter(|owner| {
                owner.inventory_family == ProviderFileInventoryFamily::SourceImport
                    && owner.provider == provider
                    && owner.source_root == source_root
            })
            .map(|owner| owner.source_path);
        let mut work = Vec::with_capacity(limit);
        if let Some(active_path) = active_path.as_deref() {
            let mut active_stmt = self.conn.prepare(&format!(
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
                       indexed_at_ms, 1 AS has_active_publication
                FROM source_import_files AS source_file
                WHERE provider = ?1 AND source_root = ?2 AND source_path = ?3
                  AND is_stale = 0 AND {predicate} AND ({active_publication})
                LIMIT 1
                "#
            ))?;
            work.extend(
                active_stmt
                    .query_map(
                        params![provider.as_str(), source_root, active_path],
                        source_import_file_work_from_row,
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?,
            );
        }
        if work.len() >= limit {
            return Ok(work);
        }
        let ordinary_limit = limit.saturating_add(usize::from(!work.is_empty()));
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
                   indexed_at_ms, 0 AS has_active_publication
            FROM source_import_files AS source_file
            WHERE provider = ?1 AND source_root = ?2 AND is_stale = 0
              AND {predicate}
            ORDER BY {order}
            LIMIT ?3
            "#
        ))?;
        let rows = stmt.query_map(
            params![
                provider.as_str(),
                source_root,
                capped_i64(ordinary_limit as u64)
            ],
            source_import_file_work_from_row,
        )?;
        let ordinary = collect_rows(rows)?;
        let remaining = limit.saturating_sub(work.len());
        work.extend(
            ordinary
                .into_iter()
                .filter(|candidate| {
                    active_path
                        .as_deref()
                        .is_none_or(|path| candidate.file.source_path != path)
                })
                .take(remaining),
        );
        Ok(work)
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
        with_immediate_transaction(&self.conn, || {
            let legacy = self.conn.execute(
                r#"
                UPDATE source_import_files
                SET pending_reason = 'legacy'
                WHERE provider = ?1 AND source_root = ?2 AND is_stale = 0
                  AND indexed_status IN ('pending', 'failed') AND pending_reason IS NULL
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
            )?;
            let explicit = self.conn.execute(
                r#"
                UPDATE source_import_files
                SET pending_reason = 'explicit_rescan'
                WHERE provider = ?1 AND source_root = ?2 AND is_stale = 0
                  AND indexed_status = 'indexed'
                  AND pending_reason IS NULL
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
            )?;
            Ok(legacy.saturating_add(explicit))
        })
    }

    #[doc(hidden)]
    pub fn schedule_source_import_explicit_rescan_page(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        inventory_generation: u64,
        after_rowid: Option<i64>,
        limit: usize,
    ) -> Result<(usize, usize, Option<i64>, bool)> {
        let limit = limit.clamp(1, Self::INVENTORY_PATH_PAGE_LIMIT);
        with_immediate_transaction(&self.conn, || {
            if !self.source_import_inventory_generation_is_complete(
                provider,
                source_root,
                inventory_generation,
            )? {
                return Err(StoreError::Sql(rusqlite::Error::InvalidQuery));
            }
            let mut statement = self.conn.prepare(SOURCE_IMPORT_EXPLICIT_RESCAN_PAGE_SQL)?;
            let rowids = collect_rows(statement.query_map(
                params![
                    provider.as_str(),
                    source_root,
                    after_rowid.unwrap_or(0),
                    capped_i64(limit as u64),
                ],
                |row| row.get::<_, i64>(0),
            )?)?;
            let rows_visited = rowids.len();
            let next_cursor = rowids.last().copied();
            let complete = rows_visited < limit;
            if rowids.is_empty() {
                return Ok((rows_visited, 0, next_cursor, complete));
            }
            let placeholders = (4..4 + rowids.len())
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "UPDATE source_import_files \
                 SET pending_reason = CASE \
                     WHEN indexed_status = 'indexed' THEN 'explicit_rescan' \
                     ELSE 'legacy' \
                 END \
                 WHERE provider = ?1 AND source_root = ?2 AND is_stale = 0 \
                   AND pending_reason IS NULL \
                   AND indexed_status IN ('indexed', 'pending', 'failed') \
                   AND EXISTS ( \
                       SELECT 1 FROM import_inventory_generations AS inventory \
                       WHERE inventory.provider = ?1 AND inventory.source_root = ?2 \
                         AND inventory.inventory_family = 'source_import_files' \
                         AND inventory.current_generation = ?3 \
                         AND inventory.completed_generation = ?3 \
                   ) \
                   AND rowid IN ({placeholders})"
            );
            let mut parameters: Vec<rusqlite::types::Value> = vec![
                provider.as_str().to_owned().into(),
                source_root.to_owned().into(),
                capped_i64(inventory_generation).into(),
            ];
            parameters.extend(rowids.into_iter().map(Into::into));
            let rows_changed = self
                .conn
                .execute(&sql, rusqlite::params_from_iter(parameters))?;
            Ok((rows_visited, rows_changed, next_cursor, complete))
        })
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
