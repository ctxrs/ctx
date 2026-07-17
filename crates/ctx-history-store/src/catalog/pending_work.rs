const CATALOG_EXPLICIT_RESCAN_PAGE_SQL: &str = "SELECT rowid \
     FROM catalog_sessions INDEXED BY idx_catalog_sessions_provider_source_root_stale \
     WHERE provider = ?1 AND source_root = ?2 AND is_stale = 0 AND rowid > ?3 \
     ORDER BY rowid LIMIT ?4";

impl Store {
    pub fn list_pending_catalog_sessions(
        &self,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<Vec<CatalogSession>> {
        let visible = crate::provider_files::catalog_material_visible_predicate("catalog_sessions");
        let mut stmt = self.conn.prepare(
            format!(
                "{} WHERE provider = ?1
                       AND source_root = ?2
                       AND is_stale = 0
                       AND {visible}
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

    pub fn repair_import_pending_reasons(
        &self,
        limit: usize,
    ) -> Result<ImportPendingReasonRepairProgress> {
        with_immediate_transaction(&self.conn, || {
            self.conn.execute_batch(
                r#"
                INSERT OR IGNORE INTO import_pending_reason_repairs (
                  inventory_family, completed
                )
                SELECT 'catalog_sessions', NOT EXISTS (SELECT 1 FROM catalog_sessions)
                UNION ALL
                SELECT 'source_import_files', NOT EXISTS (SELECT 1 FROM source_import_files);
                "#,
            )?;

            let mut progress = ImportPendingReasonRepairProgress::default();
            let mut remaining = limit;
            for family in ImportPendingReasonRepairFamily::ALL {
                let (cursor_provider, cursor_source_root, cursor_source_path, completed) =
                    self.conn.query_row(
                        r#"
                        SELECT cursor_provider, cursor_source_root, cursor_source_path, completed
                        FROM import_pending_reason_repairs
                        WHERE inventory_family = ?1
                        "#,
                        [family.as_str()],
                        |row| {
                            Ok((
                                row.get::<_, Option<String>>(0)?,
                                row.get::<_, Option<String>>(1)?,
                                row.get::<_, Option<String>>(2)?,
                                row.get::<_, bool>(3)?,
                            ))
                        },
                    )?;
                if completed || remaining == 0 {
                    continue;
                }

                let batch_limit = remaining;
                let rows = self.list_import_pending_reason_repair_rows(
                    family,
                    cursor_provider.as_deref(),
                    cursor_source_root.as_deref(),
                    cursor_source_path.as_deref(),
                    batch_limit,
                )?;
                if rows.is_empty() {
                    self.conn.execute(
                        "UPDATE import_pending_reason_repairs SET completed = 1 \
                         WHERE inventory_family = ?1",
                        [family.as_str()],
                    )?;
                    continue;
                }

                let processed = rows.len();
                for row in &rows {
                    if row.grandfather_indexed_revision {
                        progress.classified_rows +=
                            self.grandfather_legacy_indexed_revision(family, row)?;
                    }
                    if row.requires_work {
                        progress.classified_rows +=
                            self.classify_legacy_pending_reason_row(family, row)?;
                    }
                }
                progress.processed_rows += processed;
                remaining -= processed;

                let last = rows.last().expect("non-empty repair batch");
                let family_completed = processed < batch_limit
                    || !self.import_pending_reason_repair_rows_remain(family, last)?;
                self.conn.execute(
                    r#"
                    UPDATE import_pending_reason_repairs
                    SET cursor_provider = ?2, cursor_source_root = ?3,
                        cursor_source_path = ?4, completed = ?5
                    WHERE inventory_family = ?1
                    "#,
                    params![
                        family.as_str(),
                        &last.provider,
                        &last.source_root,
                        &last.source_path,
                        family_completed,
                    ],
                )?;
            }

            progress.completed_families = self.conn.query_row(
                "SELECT COUNT(*) FROM import_pending_reason_repairs WHERE completed = 1",
                [],
                |row| row.get(0),
            )?;
            progress.complete =
                progress.completed_families == ImportPendingReasonRepairFamily::ALL.len();
            Ok(progress)
        })
    }

    fn list_import_pending_reason_repair_rows(
        &self,
        family: ImportPendingReasonRepairFamily,
        cursor_provider: Option<&str>,
        cursor_source_root: Option<&str>,
        cursor_source_path: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ImportPendingReasonRepairRow>> {
        let (sql, query_params): (String, Vec<rusqlite::types::Value>) = match family {
            ImportPendingReasonRepairFamily::CatalogSessions => {
                let material_exists = catalog_material_exists_sql("catalog");
                (
                    format!(
                        r#"
                        SELECT provider, source_root, source_path,
                               indexed_status = 'indexed'
                                 AND indexed_import_revision IS NULL
                                 AND import_revision = 1,
                               pending_reason IS NULL AND is_stale = 0 AND (
                                 indexed_status IN ('pending', 'failed')
                                 OR (
                                   indexed_status IN ('indexed', 'completed_with_rejections')
                                   AND (
                                     indexed_file_size_bytes IS NULL
                                     OR indexed_file_modified_at_ms IS NULL
                                     OR indexed_file_size_bytes != file_size_bytes
                                     OR indexed_file_modified_at_ms != file_modified_at_ms
                                     OR (indexed_import_revision IS NULL AND import_revision != 1)
                                     OR indexed_import_revision != import_revision
                                     OR NOT ({material_exists})
                                   )
                                 )
                               )
                        FROM catalog_sessions AS catalog
                        WHERE ?1 IS NULL OR source_path > ?1
                        ORDER BY source_path
                        LIMIT ?2
                        "#
                    ),
                    vec![
                        cursor_source_path.map(str::to_owned).into(),
                        capped_i64(limit as u64).into(),
                    ],
                )
            }
            ImportPendingReasonRepairFamily::SourceImportFiles => {
                let material_exists = source_import_material_exists_sql("source_file");
                (
                    format!(
                        r#"
                        SELECT provider, source_root, source_path,
                               indexed_status = 'indexed'
                                 AND indexed_import_revision IS NULL
                                 AND import_revision = 1,
                               pending_reason IS NULL AND is_stale = 0 AND (
                                 indexed_status IN ('pending', 'failed')
                                 OR (
                                   indexed_status IN ('indexed', 'completed_with_rejections')
                                   AND (
                                     indexed_file_size_bytes IS NULL
                                     OR indexed_file_modified_at_ms IS NULL
                                     OR indexed_file_size_bytes != file_size_bytes
                                     OR indexed_file_modified_at_ms != file_modified_at_ms
                                     OR (indexed_import_revision IS NULL AND import_revision != 1)
                                     OR indexed_import_revision != import_revision
                                     OR NOT ({material_exists})
                                   )
                                 )
                               )
                        FROM source_import_files AS source_file
                        WHERE (
                            ?1 IS NULL
                            OR provider > ?1
                            OR (provider = ?1 AND source_root > ?2)
                            OR (
                              provider = ?1 AND source_root = ?2
                              AND source_path > ?3
                            )
                          )
                        ORDER BY provider, source_root, source_path
                        LIMIT ?4
                        "#
                    ),
                    vec![
                        cursor_provider.map(str::to_owned).into(),
                        cursor_source_root.map(str::to_owned).into(),
                        cursor_source_path.map(str::to_owned).into(),
                        capped_i64(limit as u64).into(),
                    ],
                )
            }
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(query_params), |row| {
            Ok(ImportPendingReasonRepairRow {
                provider: row.get(0)?,
                source_root: row.get(1)?,
                source_path: row.get(2)?,
                grandfather_indexed_revision: row.get(3)?,
                requires_work: row.get(4)?,
            })
        })?;
        collect_rows(rows)
    }

    fn import_pending_reason_repair_rows_remain(
        &self,
        family: ImportPendingReasonRepairFamily,
        cursor: &ImportPendingReasonRepairRow,
    ) -> Result<bool> {
        match family {
            ImportPendingReasonRepairFamily::CatalogSessions => self
                .conn
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM catalog_sessions WHERE source_path > ?1)",
                    [&cursor.source_path],
                    |row| row.get(0),
                )
                .map_err(Into::into),
            ImportPendingReasonRepairFamily::SourceImportFiles => self
                .conn
                .query_row(
                    r#"
                    SELECT EXISTS (
                      SELECT 1 FROM source_import_files
                      WHERE provider > ?1
                         OR (provider = ?1 AND source_root > ?2)
                         OR (
                           provider = ?1 AND source_root = ?2
                           AND source_path > ?3
                         )
                    )
                    "#,
                    params![&cursor.provider, &cursor.source_root, &cursor.source_path],
                    |row| row.get(0),
                )
                .map_err(Into::into),
        }
    }

    fn classify_legacy_pending_reason_row(
        &self,
        family: ImportPendingReasonRepairFamily,
        row: &ImportPendingReasonRepairRow,
    ) -> Result<usize> {
        let changed = match family {
            ImportPendingReasonRepairFamily::CatalogSessions => self.conn.execute(
                r#"
                UPDATE catalog_sessions SET pending_reason = 'legacy'
                WHERE source_path = ?1 AND pending_reason IS NULL
                "#,
                [&row.source_path],
            )?,
            ImportPendingReasonRepairFamily::SourceImportFiles => self.conn.execute(
                r#"
                UPDATE source_import_files SET pending_reason = 'legacy'
                WHERE provider = ?1 AND source_root = ?2 AND source_path = ?3
                  AND pending_reason IS NULL
                "#,
                params![&row.provider, &row.source_root, &row.source_path],
            )?,
        };
        Ok(changed)
    }

    fn grandfather_legacy_indexed_revision(
        &self,
        family: ImportPendingReasonRepairFamily,
        row: &ImportPendingReasonRepairRow,
    ) -> Result<usize> {
        let changed = match family {
            ImportPendingReasonRepairFamily::CatalogSessions => self.conn.execute(
                r#"
                UPDATE catalog_sessions SET indexed_import_revision = import_revision
                WHERE source_path = ?1 AND indexed_status = 'indexed'
                  AND indexed_import_revision IS NULL AND import_revision = 1
                "#,
                [&row.source_path],
            )?,
            ImportPendingReasonRepairFamily::SourceImportFiles => self.conn.execute(
                r#"
                UPDATE source_import_files SET indexed_import_revision = import_revision
                WHERE provider = ?1 AND source_root = ?2 AND source_path = ?3
                  AND indexed_status = 'indexed'
                  AND indexed_import_revision IS NULL AND import_revision = 1
                "#,
                params![&row.provider, &row.source_root, &row.source_path],
            )?,
        };
        Ok(changed)
    }

    pub fn list_catalog_import_work(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        class: ImportWorkClass,
        limit: usize,
    ) -> Result<Vec<CatalogImportWork>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let predicate = import_work_class_predicate("catalog", class);
        let inventory_published = catalog_inventory_material_published_predicate("catalog");
        let active_publication =
            crate::provider_files::catalog_candidate_is_global_publication("catalog");
        let order = import_work_order("catalog", class);
        let active_path = self
            .effective_provider_file_publication_inventory_owner()?
            .filter(|owner| {
                owner.inventory_family == ProviderFileInventoryFamily::Catalog
                    && owner.provider == provider
                    && owner.source_root == source_root
            })
            .map(|owner| owner.source_path);
        let mut work = Vec::with_capacity(limit);
        if let Some(active_path) = active_path.as_deref() {
            let mut active_stmt = self.conn.prepare(&format!(
                r#"
                SELECT source_path, provider, source_format, source_root,
                       external_session_id, parent_external_session_id, agent_type, role_hint,
                       external_agent_id, cwd, session_started_at_ms, file_size_bytes,
                       file_modified_at_ms, import_revision, cataloged_at_ms, metadata_json,
                       pending_reason,
                       CASE
                         WHEN pending_reason = 'fresh_append' THEN MAX(
                           file_size_bytes - COALESCE((
                             SELECT checkpoint.committed_byte_offset
                             FROM provider_file_checkpoints AS checkpoint
                             WHERE checkpoint.provider = catalog.provider
                               AND checkpoint.source_format = catalog.source_format
                               AND checkpoint.source_root = catalog.source_root
                               AND checkpoint.source_path = catalog.source_path
                           ), 0),
                           0
                         )
                         ELSE file_size_bytes
                       END,
                       indexed_at_ms, 1 AS has_active_publication
                FROM catalog_sessions AS catalog
                WHERE provider = ?1 AND source_root = ?2 AND source_path = ?3
                  AND is_stale = 0 AND {predicate} AND ({inventory_published})
                  AND ({active_publication})
                LIMIT 1
                "#
            ))?;
            work.extend(
                active_stmt
                    .query_map(
                        params![provider.as_str(), source_root, active_path],
                        catalog_import_work_from_row,
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
            SELECT source_path, provider, source_format, source_root,
                   external_session_id, parent_external_session_id, agent_type, role_hint,
                   external_agent_id, cwd, session_started_at_ms, file_size_bytes,
                   file_modified_at_ms, import_revision, cataloged_at_ms, metadata_json,
                   pending_reason,
                   CASE
                     WHEN pending_reason = 'fresh_append' THEN MAX(
                       file_size_bytes - COALESCE((
                         SELECT checkpoint.committed_byte_offset
                         FROM provider_file_checkpoints AS checkpoint
                         WHERE checkpoint.provider = catalog.provider
                           AND checkpoint.source_format = catalog.source_format
                           AND checkpoint.source_root = catalog.source_root
                           AND checkpoint.source_path = catalog.source_path
                       ), 0),
                       0
                     )
                     ELSE file_size_bytes
                   END,
                   indexed_at_ms, 0 AS has_active_publication
            FROM catalog_sessions AS catalog
            WHERE provider = ?1 AND source_root = ?2 AND is_stale = 0
              AND {predicate} AND ({inventory_published})
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
            catalog_import_work_from_row,
        )?;
        let ordinary = collect_rows(rows)?;
        let remaining = limit.saturating_sub(work.len());
        work.extend(
            ordinary
                .into_iter()
                .filter(|candidate| {
                    active_path
                        .as_deref()
                        .is_none_or(|path| candidate.session.source_path != path)
                })
                .take(remaining),
        );
        Ok(work)
    }

    pub fn catalog_import_work_count(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        class: ImportWorkClass,
    ) -> Result<usize> {
        let predicate = import_work_class_predicate("catalog", class);
        let inventory_published = catalog_inventory_material_published_predicate("catalog");
        self.conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM catalog_sessions AS catalog \
                     WHERE provider = ?1 AND source_root = ?2 AND is_stale = 0 \
                       AND {predicate} AND ({inventory_published})"
                ),
                params![provider.as_str(), source_root],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub fn schedule_catalog_source_explicit_rescan(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        inventory_generation: u64,
    ) -> Result<usize> {
        with_immediate_transaction(&self.conn, || {
            let legacy = self.conn.execute(
                r#"
                UPDATE catalog_sessions
                SET pending_reason = 'legacy'
                WHERE provider = ?1 AND source_root = ?2 AND is_stale = 0
                  AND indexed_status IN ('pending', 'failed') AND pending_reason IS NULL
                  AND EXISTS (
                    SELECT 1 FROM import_inventory_generations AS inventory
                    WHERE inventory.provider = ?1 AND inventory.source_root = ?2
                      AND inventory.inventory_family = 'catalog_sessions'
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
                UPDATE catalog_sessions
                SET pending_reason = 'explicit_rescan'
                WHERE provider = ?1 AND source_root = ?2 AND is_stale = 0
                  AND indexed_status = 'indexed'
                  AND pending_reason IS NULL
                  AND EXISTS (
                    SELECT 1 FROM import_inventory_generations AS inventory
                    WHERE inventory.provider = ?1 AND inventory.source_root = ?2
                      AND inventory.inventory_family = 'catalog_sessions'
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
    pub fn schedule_catalog_source_explicit_rescan_page(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        inventory_generation: u64,
        after_rowid: Option<i64>,
        limit: usize,
    ) -> Result<(usize, usize, Option<i64>, bool)> {
        let limit = limit.clamp(1, Self::INVENTORY_PATH_PAGE_LIMIT);
        with_immediate_transaction(&self.conn, || {
            if !self.catalog_inventory_generation_is_complete(
                provider,
                source_root,
                inventory_generation,
            )? {
                return Err(StoreError::Sql(rusqlite::Error::InvalidQuery));
            }
            let mut statement = self.conn.prepare(CATALOG_EXPLICIT_RESCAN_PAGE_SQL)?;
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
                "UPDATE catalog_sessions \
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
                         AND inventory.inventory_family = 'catalog_sessions' \
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

    pub fn list_active_catalog_sessions_for_source(
        &self,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<Vec<CatalogSession>> {
        let visible = crate::provider_files::catalog_material_visible_predicate("catalog_sessions");
        let mut stmt = self.conn.prepare(
            format!(
                "{} WHERE provider = ?1
                       AND source_root = ?2
                       AND is_stale = 0
                       AND {visible}
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

    /// Compatibility helper for catalog rows created before observation tokens.
    #[doc(hidden)]
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

    pub(crate) fn record_catalog_source_import_result(
        &self,
        provider: CaptureProvider,
        update: CatalogSourceIndexUpdate<'_>,
        status: CatalogIndexedStatus,
        error: Option<&str>,
    ) -> Result<usize> {
        self.with_provider_file_inventory_result_write(
            provider,
            update.source_root,
            update.source_path,
            || {
                self.record_catalog_source_import_result_inner(
                    provider,
                    update,
                    None,
                    status,
                    error,
                    status.preserves_native_resume_checkpoint(),
                )
            },
        )
    }

    pub fn record_observed_catalog_source_import_result(
        &self,
        provider: CaptureProvider,
        update: CatalogSourceIndexUpdate<'_>,
        metadata: &serde_json::Value,
        status: CatalogIndexedStatus,
        error: Option<&str>,
    ) -> Result<usize> {
        self.with_provider_file_inventory_result_write(
            provider,
            update.source_root,
            update.source_path,
            || {
                self.record_catalog_source_import_result_inner(
                    provider,
                    update,
                    Some(metadata),
                    status,
                    error,
                    status.preserves_native_resume_checkpoint(),
                )
            },
        )
    }

    pub(crate) fn record_observed_catalog_source_import_result_preserving_legacy_cursor(
        &self,
        provider: CaptureProvider,
        update: CatalogSourceIndexUpdate<'_>,
        metadata: &serde_json::Value,
        status: CatalogIndexedStatus,
        error: Option<&str>,
    ) -> Result<usize> {
        self.with_provider_file_inventory_result_write(
            provider,
            update.source_root,
            update.source_path,
            || {
                self.record_catalog_source_import_result_inner(
                    provider,
                    update,
                    Some(metadata),
                    status,
                    error,
                    false,
                )
            },
        )
    }

    fn record_catalog_source_import_result_inner(
        &self,
        provider: CaptureProvider,
        update: CatalogSourceIndexUpdate<'_>,
        metadata: Option<&serde_json::Value>,
        status: CatalogIndexedStatus,
        error: Option<&str>,
        advance_legacy_cursor: bool,
    ) -> Result<usize> {
        if metadata.is_some_and(|metadata| {
            metadata
                .get("file_observation_token_v1")
                .and_then(serde_json::Value::as_str)
                .is_none_or(str::is_empty)
        }) {
            return Err(StoreError::InvalidProviderFileCheckpoint(
                "catalog observation token is required",
            ));
        }
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
                    pending_reason = CASE
                        WHEN ?8 = 'failed' THEN CASE
                            WHEN pending_reason IN ('fresh_append', 'recovery_retry')
                                THEN 'recovery_retry'
                            ELSE 'recovery_replacement'
                        END
                        WHEN ?8 = 'pending' THEN COALESCE(pending_reason, 'legacy')
                        ELSE NULL
                    END,
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
                  AND ((?14 IS NULL AND json_extract(
                            metadata_json,
                            '$.file_observation_token_v1'
                        ) IS NULL)
                       OR metadata_json IS ?14)
                  AND EXISTS (
                      SELECT 1
                      FROM import_inventory_generations AS inventory
                      WHERE inventory.provider = ?1
                        AND inventory.source_root = ?2
                        AND inventory.inventory_family = 'catalog_sessions'
                        AND inventory.current_generation = ?13
                  )
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
                advance_legacy_cursor,
                i64::from(update.import_revision),
                capped_i64(update.inventory_generation),
                metadata.map(serde_json::to_string).transpose()?,
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
        let visible = crate::provider_files::catalog_material_visible_predicate("catalog_sessions");
        self.conn
            .query_row(
                &format!(
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
                      AND {visible}
                    "#
                ),
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
}
