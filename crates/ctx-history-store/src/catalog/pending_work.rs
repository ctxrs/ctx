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
        self.with_provider_file_inventory_result_write(
            provider,
            update.source_root,
            update.source_path,
            || {
                self.record_catalog_source_import_result_inner(
                    provider,
                    update,
                    status,
                    error,
                    status.preserves_native_resume_checkpoint(),
                )
            },
        )
    }

    pub(crate) fn record_catalog_source_import_result_preserving_legacy_cursor(
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
                    provider, update, status, error, false,
                )
            },
        )
    }

    fn record_catalog_source_import_result_inner(
        &self,
        provider: CaptureProvider,
        update: CatalogSourceIndexUpdate<'_>,
        status: CatalogIndexedStatus,
        error: Option<&str>,
        advance_legacy_cursor: bool,
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
