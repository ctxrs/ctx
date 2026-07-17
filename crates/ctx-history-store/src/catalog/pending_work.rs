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

    pub fn import_pending_work_is_ready(&self) -> Result<bool> {
        match self.import_pending_work_selection_mode()? {
            ImportPendingWorkSelectionMode::Direct => Ok(true),
            ImportPendingWorkSelectionMode::Projection => self
                .conn
                .query_row(
                    "SELECT COUNT(*) = 2 AND COALESCE(MIN(completed), 0) = 1 \
                 FROM import_pending_reason_repairs \
                 WHERE inventory_family IN ('catalog_sessions', 'source_import_files')",
                    [],
                    |row| row.get(0),
                )
                .map_err(Into::into),
        }
    }

    pub fn ensure_import_pending_work_ready(&self) -> Result<()> {
        if self.import_pending_work_is_ready()? {
            Ok(())
        } else {
            Err(StoreError::ImportPendingWorkProjectionIncomplete)
        }
    }

    fn import_pending_work_selection_mode(&self) -> Result<ImportPendingWorkSelectionMode> {
        let mode = self.conn.query_row(
            "SELECT selection_mode FROM import_pending_work_state WHERE singleton = 1",
            [],
            |row| row.get::<_, String>(0),
        )?;
        match mode.as_str() {
            "direct" => Ok(ImportPendingWorkSelectionMode::Direct),
            "projection" => Ok(ImportPendingWorkSelectionMode::Projection),
            _ => Err(StoreError::ImportInventorySchemaIncompatible(
                "invalid pending-work selection mode",
            )),
        }
    }

    pub fn repair_import_pending_reasons(
        &self,
        max_rows: usize,
        max_bytes: usize,
        max_sqlite_time: Duration,
    ) -> Result<ImportPendingReasonRepairProgress> {
        let mut progress = self.import_pending_reason_repair_progress()?;
        if progress.complete {
            return Ok(progress);
        }
        if max_rows == 0 || max_bytes == 0 {
            return Err(StoreError::ImportPendingWorkRepairNoProgress);
        }

        let timeout = max_sqlite_time.max(Duration::from_millis(1));
        let started = std::time::Instant::now();
        let progress_started = started;
        self.conn
            .progress_handler(1_000, Some(move || progress_started.elapsed() >= timeout));
        let result = (|| -> Result<()> {
            while progress.visited_rows < max_rows && progress.processed_bytes < max_bytes {
                if started.elapsed() >= timeout {
                    break;
                }
                let remaining_bytes = max_bytes.saturating_sub(progress.processed_bytes);
                match self.advance_import_pending_reason_repair_step(remaining_bytes, max_bytes) {
                    Ok(Some(step)) => {
                        progress.visited_rows =
                            progress.visited_rows.saturating_add(step.visited_rows);
                        progress.processed_rows =
                            progress.processed_rows.saturating_add(step.processed_rows);
                        progress.processed_bytes = progress
                            .processed_bytes
                            .saturating_add(step.processed_bytes);
                        progress.classified_rows = progress
                            .classified_rows
                            .saturating_add(step.classified_rows);
                        let state = self.import_pending_reason_repair_progress()?;
                        progress.completed_families = state.completed_families;
                        progress.complete = state.complete;
                        if progress.complete {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) if import_pending_repair_interrupted(&error) => break,
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        })();
        self.conn.progress_handler(0, None::<fn() -> bool>);
        result?;
        if !progress.complete && progress.visited_rows == 0 {
            return Err(StoreError::ImportPendingWorkRepairTimedOut {
                timeout_ms: timeout.as_millis().min(u128::from(u64::MAX)) as u64,
            });
        }
        Ok(progress)
    }

    fn import_pending_reason_repair_progress(&self) -> Result<ImportPendingReasonRepairProgress> {
        let completed_families = self.conn.query_row(
            "SELECT COUNT(*) FROM import_pending_reason_repairs WHERE completed = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(ImportPendingReasonRepairProgress {
            completed_families,
            complete: completed_families == ImportPendingReasonRepairFamily::ALL.len(),
            ..ImportPendingReasonRepairProgress::default()
        })
    }

    fn advance_import_pending_reason_repair_step(
        &self,
        byte_budget: usize,
        max_bytes: usize,
    ) -> Result<Option<ImportPendingReasonRepairStep>> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let step = self.advance_import_pending_reason_repair_step_inner();
        match step {
            Ok(step) if step.processed_bytes <= byte_budget => {
                if let Err(error) = self.conn.execute_batch("COMMIT") {
                    let _ = self.conn.execute_batch("ROLLBACK");
                    return Err(error.into());
                }
                Ok(Some(step))
            }
            Ok(step) => {
                self.conn.execute_batch("ROLLBACK")?;
                if step.processed_bytes > max_bytes {
                    Err(StoreError::ImportPendingWorkRepairUnitTooLarge {
                        bytes: step.processed_bytes,
                        max_bytes,
                    })
                } else {
                    Ok(None)
                }
            }
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    fn advance_import_pending_reason_repair_step_inner(
        &self,
    ) -> Result<ImportPendingReasonRepairStep> {
        let repair = self
            .conn
            .query_row(
                r#"
            SELECT inventory_family, cursor_provider, cursor_source_root, cursor_source_path
            FROM import_pending_reason_repairs
            WHERE completed = 0
            ORDER BY CASE inventory_family WHEN 'catalog_sessions' THEN 0 ELSE 1 END
            LIMIT 1
            "#,
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((family, cursor_provider, cursor_source_root, cursor_source_path)) = repair else {
            return Ok(ImportPendingReasonRepairStep {
                visited_rows: 1,
                processed_rows: 0,
                processed_bytes: 1,
                classified_rows: 0,
            });
        };
        let family = ImportPendingReasonRepairFamily::from_str(&family)?;
        let source_cursor = (family == ImportPendingReasonRepairFamily::SourceImportFiles)
            .then(|| SourceImportPendingRepairCursor::from_ledger(cursor_source_path.as_deref()));
        let completed_source_path = match family {
            ImportPendingReasonRepairFamily::CatalogSessions => cursor_source_path.as_deref(),
            ImportPendingReasonRepairFamily::SourceImportFiles => source_cursor
                .as_ref()
                .and_then(|cursor| cursor.completed_path.as_deref()),
        };
        let row = self.next_import_pending_reason_repair_row(
            family,
            cursor_provider.as_deref(),
            cursor_source_root.as_deref(),
            completed_source_path,
        )?;
        let Some(row) = row else {
            self.conn.execute(
                "UPDATE import_pending_reason_repairs SET completed = 1 \
                 WHERE inventory_family = ?1",
                [family.as_str()],
            )?;
            return Ok(ImportPendingReasonRepairStep {
                visited_rows: 1,
                processed_rows: 0,
                processed_bytes: 1,
                classified_rows: 0,
            });
        };

        let row_bytes = row.estimated_bytes();
        let mut processed_bytes = row_bytes;
        let mut requires_work = row.requires_work_without_material();
        if row.requires_material_check() {
            match family {
                ImportPendingReasonRepairFamily::CatalogSessions => {
                    requires_work = !self.catalog_repair_material_exists(&row)?;
                }
                ImportPendingReasonRepairFamily::SourceImportFiles => {
                    if let Some(material_exists) =
                        self.source_repair_direct_material_exists(&row)?
                    {
                        requires_work = !material_exists;
                    } else {
                        let mut cursor = source_cursor.unwrap_or_default();
                        if !cursor.active_matches(&row) {
                            cursor.active_provider = Some(row.provider.clone());
                            cursor.active_source_root = Some(row.source_root.clone());
                            cursor.active_source_path = Some(row.source_path.clone());
                            cursor.material_rowid = 0;
                        }
                        if let Some(source) =
                            self.next_legacy_capture_source(cursor.material_rowid)?
                        {
                            let source_matches = legacy_capture_source_matches(&source, &row)?;
                            processed_bytes = row_bytes.saturating_add(source.estimated_bytes);
                            if source_matches {
                                requires_work = false;
                            } else {
                                cursor.material_rowid = source.rowid;
                                self.conn.execute(
                                    "UPDATE import_pending_reason_repairs \
                                     SET cursor_source_path = ?2 \
                                     WHERE inventory_family = ?1",
                                    params![family.as_str(), cursor.encode()?],
                                )?;
                                return Ok(ImportPendingReasonRepairStep {
                                    visited_rows: 1,
                                    processed_rows: 0,
                                    processed_bytes,
                                    classified_rows: 0,
                                });
                            }
                        } else {
                            requires_work = true;
                        }
                    }
                }
            }
        }

        let classified_rows = self.resync_import_pending_reason_row(family, &row, requires_work)?;
        self.advance_import_pending_reason_repair_cursor(family, &row)?;
        Ok(ImportPendingReasonRepairStep {
            visited_rows: 1,
            processed_rows: 1,
            processed_bytes,
            classified_rows,
        })
    }

    fn next_import_pending_reason_repair_row(
        &self,
        family: ImportPendingReasonRepairFamily,
        cursor_provider: Option<&str>,
        cursor_source_root: Option<&str>,
        cursor_source_path: Option<&str>,
    ) -> Result<Option<ImportPendingReasonRepairRow>> {
        let (sql, values): (&str, Vec<rusqlite::types::Value>) = match (
            family,
            cursor_provider,
            cursor_source_root,
            cursor_source_path,
        ) {
            (ImportPendingReasonRepairFamily::CatalogSessions, _, _, None) => (
                r#"
                SELECT provider, source_format, source_root, source_path,
                       external_session_id, metadata_json, indexed_status,
                       indexed_file_size_bytes, indexed_file_modified_at_ms,
                       file_size_bytes, file_modified_at_ms, import_revision,
                       indexed_import_revision, is_stale, pending_reason
                FROM catalog_sessions
                ORDER BY source_path
                LIMIT 1
                "#,
                Vec::new(),
            ),
            (ImportPendingReasonRepairFamily::CatalogSessions, _, _, Some(source_path)) => (
                r#"
                SELECT provider, source_format, source_root, source_path,
                       external_session_id, metadata_json, indexed_status,
                       indexed_file_size_bytes, indexed_file_modified_at_ms,
                       file_size_bytes, file_modified_at_ms, import_revision,
                       indexed_import_revision, is_stale, pending_reason
                FROM catalog_sessions
                WHERE source_path > ?1
                ORDER BY source_path
                LIMIT 1
                "#,
                vec![source_path.to_owned().into()],
            ),
            (ImportPendingReasonRepairFamily::SourceImportFiles, None, _, _) => (
                r#"
                SELECT provider, source_format, source_root, source_path,
                       NULL, metadata_json, indexed_status,
                       indexed_file_size_bytes, indexed_file_modified_at_ms,
                       file_size_bytes, file_modified_at_ms, import_revision,
                       indexed_import_revision, is_stale, pending_reason
                FROM source_import_files
                ORDER BY provider, source_root, source_path
                LIMIT 1
                "#,
                Vec::new(),
            ),
            (
                ImportPendingReasonRepairFamily::SourceImportFiles,
                Some(provider),
                Some(source_root),
                Some(source_path),
            ) => (
                r#"
                SELECT provider, source_format, source_root, source_path,
                       NULL, metadata_json, indexed_status,
                       indexed_file_size_bytes, indexed_file_modified_at_ms,
                       file_size_bytes, file_modified_at_ms, import_revision,
                       indexed_import_revision, is_stale, pending_reason
                FROM source_import_files
                WHERE (provider, source_root, source_path) > (?1, ?2, ?3)
                ORDER BY provider, source_root, source_path
                LIMIT 1
                "#,
                vec![
                    provider.to_owned().into(),
                    source_root.to_owned().into(),
                    source_path.to_owned().into(),
                ],
            ),
            (ImportPendingReasonRepairFamily::SourceImportFiles, _, _, _) => {
                return Err(StoreError::ImportInventorySchemaIncompatible(
                    "incomplete source pending-work repair cursor",
                ));
            }
        };
        self.conn
            .query_row(sql, rusqlite::params_from_iter(values), |row| {
                let indexed_status = parse_text_enum::<CatalogIndexedStatus>(row.get(6)?)?;
                let import_revision = row.get::<_, i64>(11)?;
                let indexed_import_revision = row.get::<_, Option<i64>>(12)?;
                Ok(ImportPendingReasonRepairRow {
                    provider: row.get(0)?,
                    source_format: row.get(1)?,
                    source_root: row.get(2)?,
                    source_path: row.get(3)?,
                    external_session_id: row.get(4)?,
                    metadata_json: row.get(5)?,
                    indexed_status,
                    indexed_file_size_bytes: row.get(7)?,
                    indexed_file_modified_at_ms: row.get(8)?,
                    file_size_bytes: row.get(9)?,
                    file_modified_at_ms: row.get(10)?,
                    import_revision,
                    indexed_import_revision,
                    is_stale: row.get(13)?,
                    pending_reason: row.get(14)?,
                    grandfather_indexed_revision: indexed_status == CatalogIndexedStatus::Indexed
                        && indexed_import_revision.is_none()
                        && import_revision == 1,
                })
            })
            .optional()
            .map_err(Into::into)
    }

    fn catalog_repair_material_exists(&self, row: &ImportPendingReasonRepairRow) -> Result<bool> {
        let Some(external_session_id) = row.external_session_id.as_deref() else {
            return Ok(false);
        };
        let provider = CaptureProvider::from_str(&row.provider)?;
        let material_source_format = expected_material_source_format(provider, &row.source_format);
        self.conn
            .query_row(
                r#"
            SELECT EXISTS (
              SELECT 1
              FROM sessions AS material_session
                   INDEXED BY idx_sessions_provider_external_session_id
              JOIN capture_sources AS source ON source.id = material_session.capture_source_id
              WHERE material_session.provider = ?1
                AND material_session.external_session_id = ?2
                AND source.provider = ?1
                AND source.source_format = ?3
                AND source.external_session_id = ?2
                AND (
                  (source.raw_source_path = ?5 AND (
                    source.source_root = ?4 OR source.source_root = source.raw_source_path
                    OR source.source_root IS NULL
                  ))
                  OR (source.raw_source_path IS NULL AND source.source_root = ?5)
                )
              LIMIT 1
            )
            "#,
                params![
                    &row.provider,
                    external_session_id,
                    material_source_format,
                    &row.source_root,
                    &row.source_path,
                ],
                |result| result.get(0),
            )
            .map_err(Into::into)
    }

    fn source_repair_direct_material_exists(
        &self,
        row: &ImportPendingReasonRepairRow,
    ) -> Result<Option<bool>> {
        let metadata: Value = serde_json::from_str(&row.metadata_json)?;
        if metadata.get("inventory_unit").and_then(Value::as_str) != Some("source_root") {
            return Ok(None);
        }
        let provider = CaptureProvider::from_str(&row.provider)?;
        let source_format = expected_material_source_format(provider, &row.source_format);
        let source_identity =
            pending_repair_source_root_identity(&row.provider, source_format, &row.source_root)?;
        self.conn
            .query_row(
                r#"
            SELECT EXISTS (
              SELECT 1
              FROM capture_sources INDEXED BY idx_capture_sources_provider_source_identity
              WHERE provider = ?1 AND source_format = ?2 AND source_identity = ?3
                AND source_root = ?4
              LIMIT 1
            )
            "#,
                params![
                    &row.provider,
                    source_format,
                    source_identity,
                    &row.source_root
                ],
                |result| result.get(0),
            )
            .map(Some)
            .map_err(Into::into)
    }

    fn next_legacy_capture_source(
        &self,
        after_rowid: i64,
    ) -> Result<Option<LegacyCaptureSourceRow>> {
        self.conn
            .query_row(
                r#"
            SELECT rowid, provider, source_format, source_root, raw_source_path,
                   length(provider) + COALESCE(length(source_format), 0)
                     + COALESCE(length(source_root), 0)
                     + COALESCE(length(raw_source_path), 0) + 128
            FROM capture_sources
            WHERE rowid > ?1
            ORDER BY rowid
            LIMIT 1
            "#,
                [after_rowid],
                |source| {
                    Ok(LegacyCaptureSourceRow {
                        rowid: source.get(0)?,
                        provider: source.get(1)?,
                        source_format: source.get(2)?,
                        source_root: source.get(3)?,
                        raw_source_path: source.get(4)?,
                        estimated_bytes: source.get::<_, usize>(5)?.max(1),
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    fn resync_import_pending_reason_row(
        &self,
        family: ImportPendingReasonRepairFamily,
        row: &ImportPendingReasonRepairRow,
        requires_work: bool,
    ) -> Result<usize> {
        self.stage_unprojected_pending_repair_row(family, row)?;
        let indexed_import_revision = row
            .grandfather_indexed_revision
            .then_some(row.import_revision)
            .or(row.indexed_import_revision);
        let pending_reason = row
            .pending_reason
            .as_deref()
            .or_else(|| requires_work.then_some("legacy"));
        let changed = match family {
            ImportPendingReasonRepairFamily::CatalogSessions => self.conn.execute(
                "UPDATE catalog_sessions \
                 SET indexed_import_revision = ?2, pending_reason = ?3 \
                 WHERE source_path = ?1",
                params![&row.source_path, indexed_import_revision, pending_reason],
            )?,
            ImportPendingReasonRepairFamily::SourceImportFiles => self.conn.execute(
                "UPDATE source_import_files \
                 SET indexed_import_revision = ?4, pending_reason = ?5 \
                 WHERE provider = ?1 AND source_root = ?2 AND source_path = ?3",
                params![
                    &row.provider,
                    &row.source_root,
                    &row.source_path,
                    indexed_import_revision,
                    pending_reason,
                ],
            )?,
        };
        debug_assert_eq!(changed, 1);
        Ok(usize::from(row.grandfather_indexed_revision)
            .saturating_add(usize::from(row.pending_reason.is_none() && requires_work)))
    }

    fn stage_unprojected_pending_repair_row(
        &self,
        family: ImportPendingReasonRepairFamily,
        row: &ImportPendingReasonRepairRow,
    ) -> Result<()> {
        if self.import_pending_work_selection_mode()? != ImportPendingWorkSelectionMode::Projection
            || row.is_stale
        {
            return Ok(());
        }
        let Some(reason) = row.pending_reason.as_deref() else {
            return Ok(());
        };
        let class = ImportPendingReason::from_str(reason)?.class();
        let inserted = match family {
            ImportPendingReasonRepairFamily::CatalogSessions => self.conn.execute(
                r#"
                INSERT INTO import_pending_work (
                  inventory_family, provider, source_root, source_path,
                  work_class, indexed_at_ms
                )
                SELECT 'catalog_sessions', provider, source_root, source_path, ?2, indexed_at_ms
                FROM catalog_sessions
                WHERE source_path = ?1
                ON CONFLICT DO NOTHING
                "#,
                params![&row.source_path, class.as_str()],
            )?,
            ImportPendingReasonRepairFamily::SourceImportFiles => self.conn.execute(
                r#"
                INSERT INTO import_pending_work (
                  inventory_family, provider, source_root, source_path,
                  work_class, indexed_at_ms
                )
                SELECT 'source_import_files', provider, source_root, source_path,
                       ?4, indexed_at_ms
                FROM source_import_files
                WHERE provider = ?1 AND source_root = ?2 AND source_path = ?3
                ON CONFLICT DO NOTHING
                "#,
                params![
                    &row.provider,
                    &row.source_root,
                    &row.source_path,
                    class.as_str()
                ],
            )?,
        };
        if inserted == 1 {
            self.conn.execute(
                r#"
                INSERT INTO import_pending_work_counts (
                  inventory_family, provider, source_root, work_class, pending_count
                ) VALUES (?1, ?2, ?3, ?4, 1)
                ON CONFLICT (inventory_family, provider, source_root, work_class)
                DO UPDATE SET pending_count = pending_count + 1
                "#,
                params![
                    family.as_str(),
                    &row.provider,
                    &row.source_root,
                    class.as_str()
                ],
            )?;
        }
        Ok(())
    }

    fn advance_import_pending_reason_repair_cursor(
        &self,
        family: ImportPendingReasonRepairFamily,
        row: &ImportPendingReasonRepairRow,
    ) -> Result<()> {
        let cursor_source_path = match family {
            ImportPendingReasonRepairFamily::CatalogSessions => row.source_path.clone(),
            ImportPendingReasonRepairFamily::SourceImportFiles => SourceImportPendingRepairCursor {
                completed_path: Some(row.source_path.clone()),
                ..SourceImportPendingRepairCursor::default()
            }
            .encode()?,
        };
        self.conn.execute(
            r#"
            UPDATE import_pending_reason_repairs
            SET cursor_provider = ?2, cursor_source_root = ?3,
                cursor_source_path = ?4
            WHERE inventory_family = ?1
            "#,
            params![
                family.as_str(),
                &row.provider,
                &row.source_root,
                cursor_source_path,
            ],
        )?;
        Ok(())
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
        self.ensure_import_pending_work_ready()?;
        let selection_mode = self.import_pending_work_selection_mode()?;
        let predicate = import_work_class_predicate("catalog", class);
        let inventory_published = catalog_inventory_material_published_predicate("catalog");
        let active_publication =
            crate::provider_files::catalog_candidate_is_global_publication("catalog");
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
        let ordinary_sql = catalog_import_work_ordinary_sql(selection_mode, class);
        let mut stmt = self.conn.prepare(&ordinary_sql)?;
        let rows = match selection_mode {
            ImportPendingWorkSelectionMode::Direct => stmt.query_map(
                params![
                    provider.as_str(),
                    source_root,
                    capped_i64(ordinary_limit as u64)
                ],
                catalog_import_work_from_row,
            )?,
            ImportPendingWorkSelectionMode::Projection => stmt.query_map(
                params![
                    provider.as_str(),
                    source_root,
                    class.as_str(),
                    capped_i64(ordinary_limit as u64)
                ],
                catalog_import_work_from_row,
            )?,
        };
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
        self.import_pending_work_count("catalog_sessions", provider, source_root, class)
    }

    fn import_pending_work_count(
        &self,
        inventory_family: &str,
        provider: CaptureProvider,
        source_root: &str,
        class: ImportWorkClass,
    ) -> Result<usize> {
        self.ensure_import_pending_work_ready()?;
        self.conn
            .query_row(
                r#"
            SELECT CASE
              WHEN EXISTS (
                SELECT 1 FROM import_inventory_generations
                WHERE provider = ?2 AND source_root = ?3
                  AND inventory_family = ?1 AND completed_generation = 0
              ) THEN 0
              ELSE COALESCE((
                SELECT pending_count FROM import_pending_work_counts
                WHERE inventory_family = ?1 AND provider = ?2 AND source_root = ?3
                  AND work_class = ?4
              ), 0)
            END
            "#,
                params![
                    inventory_family,
                    provider.as_str(),
                    source_root,
                    class.as_str()
                ],
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

fn import_pending_repair_interrupted(error: &StoreError) -> bool {
    matches!(
        error,
        StoreError::Sql(rusqlite::Error::SqliteFailure(sqlite_error, _))
            if sqlite_error.code == rusqlite::ErrorCode::OperationInterrupted
    )
}

fn catalog_import_work_ordinary_sql(
    selection_mode: ImportPendingWorkSelectionMode,
    class: ImportWorkClass,
) -> String {
    let select = r#"
        SELECT catalog.source_path, catalog.provider, catalog.source_format,
               catalog.source_root, catalog.external_session_id,
               catalog.parent_external_session_id, catalog.agent_type, catalog.role_hint,
               catalog.external_agent_id, catalog.cwd, catalog.session_started_at_ms,
               catalog.file_size_bytes, catalog.file_modified_at_ms,
               catalog.import_revision, catalog.cataloged_at_ms, catalog.metadata_json,
               catalog.pending_reason,
               CASE
                 WHEN catalog.pending_reason = 'fresh_append' THEN MAX(
                   catalog.file_size_bytes - COALESCE((
                     SELECT checkpoint.committed_byte_offset
                     FROM provider_file_checkpoints AS checkpoint
                     WHERE checkpoint.provider = catalog.provider
                       AND checkpoint.source_format = catalog.source_format
                       AND checkpoint.source_root = catalog.source_root
                       AND checkpoint.source_path = catalog.source_path
                   ), 0),
                   0
                 )
                 ELSE catalog.file_size_bytes
               END,
               catalog.indexed_at_ms, 0 AS has_active_publication
    "#;
    let inventory_published = catalog_inventory_material_published_predicate("catalog");
    match selection_mode {
        ImportPendingWorkSelectionMode::Direct => {
            let predicate = import_work_class_predicate("catalog", class);
            let index = match class {
                ImportWorkClass::Fresh => "idx_catalog_sessions_pending_fresh_attempt",
                ImportWorkClass::Recovery => "idx_catalog_sessions_pending_recovery_attempt",
            };
            format!(
                "{select} FROM catalog_sessions AS catalog INDEXED BY {index} \
                 WHERE catalog.provider = ?1 AND catalog.source_root = ?2 \
                   AND catalog.is_stale = 0 AND {predicate} \
                   AND ({inventory_published}) \
                 ORDER BY catalog.indexed_at_ms, catalog.source_path LIMIT ?3"
            )
        }
        ImportPendingWorkSelectionMode::Projection => format!(
            "{select} \
             FROM import_pending_work AS pending \
                  INDEXED BY idx_import_pending_work_selection \
             CROSS JOIN catalog_sessions AS catalog \
               ON catalog.source_path = pending.source_path \
              AND catalog.provider = pending.provider \
              AND catalog.source_root = pending.source_root \
             WHERE pending.inventory_family = 'catalog_sessions' \
               AND pending.provider = ?1 AND pending.source_root = ?2 \
               AND pending.work_class = ?3 AND ({inventory_published}) \
             ORDER BY pending.indexed_at_ms, pending.source_path LIMIT ?4"
        ),
    }
}

fn pending_repair_source_root_identity(
    provider: &str,
    source_format: &str,
    source_root: &str,
) -> Result<String> {
    let mut normalized_root = source_root.trim().replace('\\', "/");
    while normalized_root.len() > 1 && normalized_root.ends_with('/') {
        normalized_root.pop();
    }
    let identity_key = serde_json::to_string(&(
        "provider-source-identity-v1",
        provider,
        source_format,
        "root",
        normalized_root,
    ))?;
    let name = format!("ctx-ctx-history-capture:{identity_key}:provider-source-root");
    let mut bytes = [0_u8; 16];
    let first = pending_repair_fnv1a64(name.as_bytes()).to_be_bytes();
    let second = pending_repair_fnv1a64(format!("{name}:uuid-v7").as_bytes()).to_be_bytes();
    bytes[..6].copy_from_slice(&first[..6]);
    bytes[6] = 0x70 | (first[6] & 0x0f);
    bytes[7] = first[7];
    bytes[8] = 0x80 | (second[0] & 0x3f);
    bytes[9..].copy_from_slice(&second[1..]);
    Ok(Uuid::from_bytes(bytes).to_string())
}

fn pending_repair_fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn legacy_capture_source_matches(
    source: &LegacyCaptureSourceRow,
    row: &ImportPendingReasonRepairRow,
) -> Result<bool> {
    let provider = CaptureProvider::from_str(&row.provider)?;
    let expected_source_format = expected_material_source_format(provider, &row.source_format);
    if source.provider != row.provider
        || source.source_format.as_deref() != Some(expected_source_format)
    {
        return Ok(false);
    }
    let metadata: Value = serde_json::from_str(&row.metadata_json)?;
    if metadata.get("inventory_unit").and_then(Value::as_str) == Some("source_root") {
        return Ok(source.source_root.as_deref() == Some(&row.source_root));
    }
    let raw_source_path = source.raw_source_path.as_deref();
    let source_root = source.source_root.as_deref();
    Ok(raw_source_path == Some(&row.source_path)
        && (source_root == Some(&row.source_root)
            || source_root == raw_source_path
            || source_root.is_none()))
}
