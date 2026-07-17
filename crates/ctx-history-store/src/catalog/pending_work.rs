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
                    "SELECT state.projection_version = 2 \
                        AND state.legacy_cleanup_complete = 1 \
                        AND state.material_scan_complete = 1 \
                        AND (SELECT COUNT(*) = 2 AND COALESCE(MIN(completed), 0) = 1 \
                             FROM import_pending_reason_repairs \
                             WHERE inventory_family IN ('catalog_sessions', 'source_import_files')) \
                     FROM import_pending_work_state AS state WHERE state.singleton = 1",
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
        self.conn.busy_timeout(Duration::ZERO)?;
        self.conn
            .progress_handler(1_000, Some(move || progress_started.elapsed() >= timeout));
        let begin = self.conn.execute_batch("BEGIN IMMEDIATE");
        if let Err(error) = begin {
            self.conn.progress_handler(0, None::<fn() -> bool>);
            self.conn.busy_timeout(self.busy_timeout)?;
            if import_pending_repair_busy(&error) {
                return Err(StoreError::ImportPendingWorkRepairBusy);
            }
            return Err(error.into());
        }
        let result = (|| -> Result<()> {
            if self.rearm_uninitialized_legacy_material_scan()? {
                progress.visited_rows = 1;
                progress.processed_bytes = 1;
            }
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
                    }
                    Ok(None) => break,
                    Err(error) if import_pending_repair_interrupted(&error) => break,
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        })();
        let result = match result {
            Ok(()) => match self.conn.execute_batch("COMMIT") {
                Ok(()) => Ok(()),
                Err(error) => {
                    let _ = self.conn.execute_batch("ROLLBACK");
                    Err(StoreError::from(error))
                }
            },
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        };
        self.conn.progress_handler(0, None::<fn() -> bool>);
        self.conn.busy_timeout(self.busy_timeout)?;
        result?;
        progress.committed_transactions = 1;
        let state = self.import_pending_reason_repair_progress()?;
        progress.completed_families = state.completed_families;
        progress.complete = state.complete;
        if !progress.complete && progress.visited_rows == 0 {
            return Err(StoreError::ImportPendingWorkRepairTimedOut {
                timeout_ms: timeout.as_millis().min(u128::from(u64::MAX)) as u64,
            });
        }
        Ok(progress)
    }

    fn rearm_uninitialized_legacy_material_scan(&self) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE import_pending_work_state \
             SET material_scan_complete = 0 \
             WHERE singleton = 1 AND selection_mode = 'projection' \
               AND projection_version = 2 AND material_scan_complete = 1 \
               AND EXISTS (SELECT 1 FROM capture_sources \
                           WHERE rowid > material_cursor_rowid LIMIT 1)",
            [],
        )?;
        Ok(changed == 1)
    }

    fn import_pending_reason_repair_progress(&self) -> Result<ImportPendingReasonRepairProgress> {
        let completed_families = self.conn.query_row(
            "SELECT COUNT(*) FROM import_pending_reason_repairs WHERE completed = 1",
            [],
            |row| row.get(0),
        )?;
        let state_ready = self.conn.query_row(
            "SELECT projection_version = 2 AND legacy_cleanup_complete = 1 \
                    AND material_scan_complete = 1 \
             FROM import_pending_work_state WHERE singleton = 1",
            [],
            |row| row.get::<_, bool>(0),
        )?;
        Ok(ImportPendingReasonRepairProgress {
            completed_families,
            complete: state_ready
                && completed_families == ImportPendingReasonRepairFamily::ALL.len(),
            ..ImportPendingReasonRepairProgress::default()
        })
    }

    fn advance_import_pending_reason_repair_step(
        &self,
        byte_budget: usize,
        max_bytes: usize,
    ) -> Result<Option<ImportPendingReasonRepairStep>> {
        if let Some(step) = self.cleanup_legacy_pending_projection(byte_budget, max_bytes)? {
            return Ok(Some(step));
        }
        if let Some(step) = self.advance_legacy_material_owner_scan(byte_budget, max_bytes)? {
            return Ok(Some(step));
        }
        let Some((family, cursor_rowid)) = self.next_pending_reason_repair_family()? else {
            return Ok(Some(ImportPendingReasonRepairStep {
                visited_rows: 1,
                processed_rows: 0,
                processed_bytes: 1,
                classified_rows: 0,
            }));
        };
        let Some(preflight) =
            self.next_import_pending_reason_repair_preflight(family, cursor_rowid)?
        else {
            self.conn.execute(
                "UPDATE import_pending_reason_repairs SET completed = 1 \
                 WHERE inventory_family = ?1",
                [family.as_str()],
            )?;
            return Ok(Some(ImportPendingReasonRepairStep {
                visited_rows: 1,
                processed_rows: 0,
                processed_bytes: 1,
                classified_rows: 0,
            }));
        };
        if preflight.estimated_bytes > max_bytes {
            return Err(StoreError::ImportPendingWorkRepairUnitTooLarge {
                bytes: preflight.estimated_bytes,
                max_bytes,
            });
        }
        if preflight.estimated_bytes > byte_budget {
            return Ok(None);
        }
        let row = self.load_import_pending_reason_repair_row(family, preflight.rowid)?;
        let mut requires_work = row.requires_work_without_material();
        if row.requires_material_check() {
            requires_work = match family {
                ImportPendingReasonRepairFamily::CatalogSessions => {
                    !self.catalog_repair_material_exists(&row)?
                }
                ImportPendingReasonRepairFamily::SourceImportFiles => {
                    !self.source_repair_material_exists(&row)?
                }
            };
        }
        let classified_rows = self.resync_import_pending_reason_row(family, &row, requires_work)?;
        self.advance_import_pending_reason_repair_cursor(family, &row)?;
        Ok(Some(ImportPendingReasonRepairStep {
            visited_rows: 1,
            processed_rows: 1,
            processed_bytes: preflight.estimated_bytes,
            classified_rows,
        }))
    }

    fn cleanup_legacy_pending_projection(
        &self,
        byte_budget: usize,
        max_bytes: usize,
    ) -> Result<Option<ImportPendingReasonRepairStep>> {
        let (cleanup_complete, phase, cursor_family, cursor_provider, cursor_root, cursor_tail) =
            self.conn.query_row(
                "SELECT legacy_cleanup_complete, legacy_cleanup_phase, \
                    legacy_cleanup_inventory_family, legacy_cleanup_provider, \
                    legacy_cleanup_source_root, legacy_cleanup_tail \
             FROM import_pending_work_state WHERE singleton = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, bool>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )?;
        if cleanup_complete {
            return Ok(None);
        }
        let (table, tail_column) = match phase.as_str() {
            "work" => ("import_pending_work", "source_path"),
            "counts" => ("import_pending_work_counts", "work_class"),
            _ => {
                return Err(StoreError::ImportInventorySchemaIncompatible(
                    "invalid pending-work legacy cleanup phase",
                ));
            }
        };
        let keyset_predicate =
            format!("(inventory_family, provider, source_root, {tail_column}) > (?1, ?2, ?3, ?4)");
        let preflight_sql = format!(
            "SELECT length(CAST(inventory_family AS BLOB)) \
                    + length(CAST(provider AS BLOB)) \
                    + length(CAST(source_root AS BLOB)) \
                    + length(CAST({tail_column} AS BLOB)) + 128 \
             FROM {table} \
             WHERE projection_version != 2 AND {keyset_predicate} \
             ORDER BY inventory_family, provider, source_root, {tail_column} LIMIT 1"
        );
        let estimated_bytes = self
            .conn
            .query_row(
                &preflight_sql,
                params![&cursor_family, &cursor_provider, &cursor_root, &cursor_tail],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map(pending_repair_estimated_bytes);
        let Some(estimated_bytes) = estimated_bytes else {
            if phase == "work" {
                self.conn.execute(
                    "UPDATE import_pending_work_state \
                     SET legacy_cleanup_phase = 'counts', \
                         legacy_cleanup_inventory_family = '', \
                         legacy_cleanup_provider = '', legacy_cleanup_source_root = '', \
                         legacy_cleanup_tail = '' WHERE singleton = 1",
                    [],
                )?;
            } else {
                self.conn.execute(
                    "UPDATE import_pending_work_state SET legacy_cleanup_complete = 1 \
                     WHERE singleton = 1",
                    [],
                )?;
            }
            return Ok(Some(ImportPendingReasonRepairStep {
                visited_rows: 1,
                processed_rows: 0,
                processed_bytes: 1,
                classified_rows: 0,
            }));
        };
        if estimated_bytes > max_bytes {
            return Err(StoreError::ImportPendingWorkRepairUnitTooLarge {
                bytes: estimated_bytes,
                max_bytes,
            });
        }
        if estimated_bytes > byte_budget {
            return Ok(None);
        }
        let load_sql = format!(
            "SELECT inventory_family, provider, source_root, {tail_column} \
             FROM {table} \
             WHERE projection_version != 2 AND {keyset_predicate} \
             ORDER BY inventory_family, provider, source_root, {tail_column} LIMIT 1"
        );
        let row = self.conn.query_row(
            &load_sql,
            params![&cursor_family, &cursor_provider, &cursor_root, &cursor_tail],
            |row| {
                Ok(LegacyPendingProjectionRow {
                    inventory_family: row.get(0)?,
                    provider: row.get(1)?,
                    source_root: row.get(2)?,
                    tail: row.get(3)?,
                    estimated_bytes,
                })
            },
        )?;
        let delete_sql = format!(
            "DELETE FROM {table} WHERE projection_version != 2 \
             AND inventory_family = ?1 AND provider = ?2 \
             AND source_root = ?3 AND {tail_column} = ?4"
        );
        let deleted = self.conn.execute(
            &delete_sql,
            params![
                &row.inventory_family,
                &row.provider,
                &row.source_root,
                &row.tail
            ],
        )?;
        if deleted != 1 {
            return Err(StoreError::ImportInventorySchemaIncompatible(
                "pending-work legacy cleanup lost its keyset row",
            ));
        }
        self.conn.execute(
            "UPDATE import_pending_work_state \
             SET legacy_cleanup_inventory_family = ?1, legacy_cleanup_provider = ?2, \
                 legacy_cleanup_source_root = ?3, legacy_cleanup_tail = ?4 \
             WHERE singleton = 1",
            params![
                &row.inventory_family,
                &row.provider,
                &row.source_root,
                &row.tail
            ],
        )?;
        Ok(Some(ImportPendingReasonRepairStep {
            visited_rows: 1,
            processed_rows: 1,
            processed_bytes: row.estimated_bytes,
            classified_rows: 0,
        }))
    }

    fn advance_legacy_material_owner_scan(
        &self,
        byte_budget: usize,
        max_bytes: usize,
    ) -> Result<Option<ImportPendingReasonRepairStep>> {
        let (cursor, complete) = self.conn.query_row(
            "SELECT material_cursor_rowid, material_scan_complete \
             FROM import_pending_work_state WHERE singleton = 1",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?)),
        )?;
        if complete {
            return Ok(None);
        }
        let preflight = self
            .conn
            .query_row(
                r#"
                SELECT rowid,
                       length(CAST(id AS BLOB)) + length(CAST(provider AS BLOB))
                         + COALESCE(length(CAST(source_format AS BLOB)), 0)
                         + COALESCE(length(CAST(source_identity AS BLOB)), 0)
                         + COALESCE(length(CAST(source_root AS BLOB)), 0)
                         + COALESCE(length(CAST(raw_source_path AS BLOB)), 0) + 128
                FROM capture_sources WHERE rowid > ?1 ORDER BY rowid LIMIT 1
                "#,
                [cursor],
                |row| {
                    Ok(ImportPendingReasonRepairPreflight {
                        rowid: row.get(0)?,
                        estimated_bytes: pending_repair_estimated_bytes(row.get(1)?),
                    })
                },
            )
            .optional()?;
        let Some(preflight) = preflight else {
            self.conn.execute(
                "UPDATE import_pending_work_state SET material_scan_complete = 1 \
                 WHERE singleton = 1",
                [],
            )?;
            return Ok(Some(ImportPendingReasonRepairStep {
                visited_rows: 1,
                processed_rows: 0,
                processed_bytes: 1,
                classified_rows: 0,
            }));
        };
        if preflight.estimated_bytes > max_bytes {
            return Err(StoreError::ImportPendingWorkRepairUnitTooLarge {
                bytes: preflight.estimated_bytes,
                max_bytes,
            });
        }
        if preflight.estimated_bytes > byte_budget {
            return Ok(None);
        }
        let source = self.conn.query_row(
            "SELECT rowid, id, provider, source_format, source_root, raw_source_path \
             FROM capture_sources WHERE rowid = ?1",
            [preflight.rowid],
            |row| {
                Ok(LegacyCaptureSourceRow {
                    rowid: row.get(0)?,
                    id: row.get(1)?,
                    provider: row.get(2)?,
                    source_format: row.get(3)?,
                    source_root: row.get(4)?,
                    raw_source_path: row.get(5)?,
                })
            },
        )?;
        self.project_legacy_material_owner(&source)?;
        self.conn.execute(
            "UPDATE import_pending_work_state SET material_cursor_rowid = ?1 \
             WHERE singleton = 1",
            [source.rowid],
        )?;
        Ok(Some(ImportPendingReasonRepairStep {
            visited_rows: 1,
            processed_rows: 1,
            processed_bytes: preflight.estimated_bytes,
            classified_rows: 0,
        }))
    }

    fn project_legacy_material_owner(&self, source: &LegacyCaptureSourceRow) -> Result<()> {
        let Some(source_format) = source.source_format.as_deref() else {
            return Ok(());
        };
        if let Some(source_root) = source
            .source_root
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            self.conn.execute(
                "INSERT OR IGNORE INTO import_pending_legacy_material_owners (\
                   projection_version, owner_kind, provider, source_format, \
                   owner_source_root, source_path, capture_source_id\
                 ) VALUES (2, 'root', ?1, ?2, ?3, '', ?4)",
                params![&source.provider, source_format, source_root, &source.id],
            )?;
        }
        if let Some(raw_path) = source
            .raw_source_path
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            let owner_root = match source.source_root.as_deref() {
                Some(root) if root != raw_path => root,
                _ => "",
            };
            self.conn.execute(
                "INSERT OR IGNORE INTO import_pending_legacy_material_owners (\
                   projection_version, owner_kind, provider, source_format, \
                   owner_source_root, source_path, capture_source_id\
                 ) VALUES (2, 'path', ?1, ?2, ?3, ?4, ?5)",
                params![
                    &source.provider,
                    source_format,
                    owner_root,
                    raw_path,
                    &source.id
                ],
            )?;
        }
        Ok(())
    }

    fn next_pending_reason_repair_family(
        &self,
    ) -> Result<Option<(ImportPendingReasonRepairFamily, i64)>> {
        self.conn
            .query_row(
                "SELECT inventory_family, cursor_rowid FROM import_pending_reason_repairs \
                 WHERE completed = 0 \
                 ORDER BY CASE inventory_family WHEN 'catalog_sessions' THEN 0 ELSE 1 END \
                 LIMIT 1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
            .map(|(family, cursor)| {
                Ok((ImportPendingReasonRepairFamily::from_str(&family)?, cursor))
            })
            .transpose()
    }

    fn next_import_pending_reason_repair_preflight(
        &self,
        family: ImportPendingReasonRepairFamily,
        cursor_rowid: i64,
    ) -> Result<Option<ImportPendingReasonRepairPreflight>> {
        let table = family.as_str();
        let external_session = if family == ImportPendingReasonRepairFamily::CatalogSessions {
            "COALESCE(length(CAST(external_session_id AS BLOB)), 0) +"
        } else {
            ""
        };
        let sql = format!(
            "SELECT rowid, length(CAST(provider AS BLOB)) \
               + length(CAST(source_format AS BLOB)) + length(CAST(source_root AS BLOB)) \
               + length(CAST(source_path AS BLOB)) + {external_session} \
                 length(CAST(metadata_json AS BLOB)) + 256 \
             FROM {table} WHERE rowid > ?1 ORDER BY rowid LIMIT 1"
        );
        self.conn
            .query_row(&sql, [cursor_rowid], |row| {
                Ok(ImportPendingReasonRepairPreflight {
                    rowid: row.get(0)?,
                    estimated_bytes: pending_repair_estimated_bytes(row.get(1)?),
                })
            })
            .optional()
            .map_err(Into::into)
    }

    fn load_import_pending_reason_repair_row(
        &self,
        family: ImportPendingReasonRepairFamily,
        rowid: i64,
    ) -> Result<ImportPendingReasonRepairRow> {
        let (table, external_session) = match family {
            ImportPendingReasonRepairFamily::CatalogSessions => {
                ("catalog_sessions", "external_session_id")
            }
            ImportPendingReasonRepairFamily::SourceImportFiles => ("source_import_files", "NULL"),
        };
        let sql = format!(
            "SELECT rowid, provider, source_format, source_root, source_path, \
                    {external_session}, metadata_json, indexed_status, \
                    indexed_file_size_bytes, indexed_file_modified_at_ms, \
                    file_size_bytes, file_modified_at_ms, import_revision, \
                    indexed_import_revision, is_stale, pending_reason \
             FROM {table} WHERE rowid = ?1"
        );
        self.conn
            .query_row(&sql, [rowid], |row| {
                let indexed_status = parse_text_enum::<CatalogIndexedStatus>(row.get(7)?)?;
                let import_revision = row.get::<_, i64>(12)?;
                let indexed_import_revision = row.get::<_, Option<i64>>(13)?;
                Ok(ImportPendingReasonRepairRow {
                    rowid: row.get(0)?,
                    provider: row.get(1)?,
                    source_format: row.get(2)?,
                    source_root: row.get(3)?,
                    source_path: row.get(4)?,
                    external_session_id: row.get(5)?,
                    metadata_json: row.get(6)?,
                    indexed_status,
                    indexed_file_size_bytes: row.get(8)?,
                    indexed_file_modified_at_ms: row.get(9)?,
                    file_size_bytes: row.get(10)?,
                    file_modified_at_ms: row.get(11)?,
                    import_revision,
                    indexed_import_revision,
                    is_stale: row.get(14)?,
                    pending_reason: row.get(15)?,
                    grandfather_indexed_revision: indexed_status == CatalogIndexedStatus::Indexed
                        && indexed_import_revision.is_none()
                        && import_revision == 1,
                })
            })
            .map_err(Into::into)
    }

    fn catalog_repair_material_exists(&self, row: &ImportPendingReasonRepairRow) -> Result<bool> {
        let Some(external_session_id) = row.external_session_id.as_deref() else {
            return Ok(false);
        };
        let provider = pending_repair_capture_provider(&row.provider)?;
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

    fn source_repair_material_exists(&self, row: &ImportPendingReasonRepairRow) -> Result<bool> {
        let metadata: Value = serde_json::from_str(&row.metadata_json)?;
        let provider = pending_repair_capture_provider(&row.provider)?;
        let source_format = expected_material_source_format(provider, &row.source_format);
        let source_root_unit =
            metadata.get("inventory_unit").and_then(Value::as_str) == Some("source_root");
        if self.import_pending_work_selection_mode()? == ImportPendingWorkSelectionMode::Direct {
            return self
                .conn
                .query_row(
                    r#"
                    SELECT EXISTS (
                      SELECT 1
                      FROM capture_sources
                           INDEXED BY idx_capture_sources_provider_material_owner
                      WHERE provider = ?1 AND source_format = ?2
                        AND (
                          (?5 AND source_root = ?3)
                          OR (NOT ?5 AND raw_source_path = ?4 AND (
                            source_root = ?3 OR source_root = raw_source_path
                            OR source_root IS NULL
                          ))
                        )
                      LIMIT 1
                    )
                    "#,
                    params![
                        &row.provider,
                        source_format,
                        &row.source_root,
                        &row.source_path,
                        source_root_unit
                    ],
                    |result| result.get(0),
                )
                .map_err(Into::into);
        }
        if source_root_unit {
            let source_identity = pending_repair_source_root_identity(
                &row.provider,
                source_format,
                &row.source_root,
            )?;
            let identity_exists = self.conn.query_row(
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
            )?;
            if identity_exists {
                return Ok(true);
            }
        }
        let (owner_kind, owner_root, source_path) = if source_root_unit {
            ("root", row.source_root.as_str(), "")
        } else {
            ("path", row.source_root.as_str(), row.source_path.as_str())
        };
        self.conn
            .query_row(
                r#"
                SELECT EXISTS (
                  SELECT 1
                  FROM import_pending_legacy_material_owners AS owner
                  JOIN capture_sources AS source ON source.id = owner.capture_source_id
                  WHERE owner.projection_version = 2 AND owner.owner_kind = ?1
                    AND owner.provider = ?2 AND owner.source_format = ?3
                    AND owner.source_path = ?5
                    AND (owner.owner_source_root = ?4 OR owner.owner_source_root = '')
                  LIMIT 1
                )
                "#,
                params![
                    owner_kind,
                    &row.provider,
                    source_format,
                    owner_root,
                    source_path
                ],
                |result| result.get(0),
            )
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
                  work_class, indexed_at_ms, projection_version
                )
                SELECT 'catalog_sessions', provider, source_root, source_path, ?2, indexed_at_ms, 2
                FROM catalog_sessions
                WHERE source_path = ?1
                ON CONFLICT (inventory_family, provider, source_root, source_path)
                DO UPDATE SET work_class = excluded.work_class,
                              indexed_at_ms = excluded.indexed_at_ms,
                              projection_version = 2
                WHERE import_pending_work.projection_version != 2
                "#,
                params![&row.source_path, class.as_str()],
            )?,
            ImportPendingReasonRepairFamily::SourceImportFiles => self.conn.execute(
                r#"
                INSERT INTO import_pending_work (
                  inventory_family, provider, source_root, source_path,
                  work_class, indexed_at_ms, projection_version
                )
                SELECT 'source_import_files', provider, source_root, source_path,
                       ?4, indexed_at_ms, 2
                FROM source_import_files
                WHERE provider = ?1 AND source_root = ?2 AND source_path = ?3
                ON CONFLICT (inventory_family, provider, source_root, source_path)
                DO UPDATE SET work_class = excluded.work_class,
                              indexed_at_ms = excluded.indexed_at_ms,
                              projection_version = 2
                WHERE import_pending_work.projection_version != 2
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
                  inventory_family, provider, source_root, work_class,
                  pending_count, projection_version
                ) VALUES (?1, ?2, ?3, ?4, 1, 2)
                ON CONFLICT (inventory_family, provider, source_root, work_class)
                DO UPDATE SET pending_count = CASE
                    WHEN projection_version = 2 THEN pending_count + 1
                    ELSE 1
                END, projection_version = 2
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
        self.conn.execute(
            r#"
            UPDATE import_pending_reason_repairs
            SET cursor_provider = ?2, cursor_source_root = ?3,
                cursor_source_path = ?4, cursor_rowid = ?5
            WHERE inventory_family = ?1
            "#,
            params![
                family.as_str(),
                &row.provider,
                &row.source_root,
                &row.source_path,
                row.rowid,
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
                  AND work_class = ?4 AND projection_version = 2
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

fn import_pending_repair_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(sqlite_error, _)
            if matches!(
                sqlite_error.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

fn pending_repair_estimated_bytes(value: i64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX).max(1)
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
               AND pending.work_class = ?3 AND pending.projection_version = 2 \
               AND ({inventory_published}) \
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

fn pending_repair_capture_provider(value: &str) -> Result<CaptureProvider> {
    CaptureProvider::from_str(value).map_err(|_| {
        StoreError::ImportInventorySchemaIncompatible(
            "pending-work repair encountered an invalid capture provider",
        )
    })
}
