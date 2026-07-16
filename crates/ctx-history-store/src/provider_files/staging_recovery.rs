impl Store {
    fn reset_provider_file_publication_staging_slice(
        &self,
        scope: &ProviderFilePublicationScope,
        marker: &mut ReplacementMarker,
        max_rows: usize,
    ) -> Result<usize> {
        let replacement_id = scope.scope_id.to_string();
        let mut processed = 0usize;
        while processed < max_rows {
            let remaining = max_rows - processed;
            let cursor = marker
                .preparation_cursor
                .as_deref()
                .ok_or(StoreError::InvalidProviderFilePublicationScope)?;
            let (entity_kind, prior_kind, next_cursor) = match cursor {
                RETIREMENT_RESET_BATCH_CURSOR => {
                    (None, None, RETIREMENT_RESET_HISTORY_RECORD_CURSOR)
                }
                RETIREMENT_RESET_HISTORY_RECORD_CURSOR => (
                    Some("history_record"),
                    Some(PRIOR_HISTORY_RECORD_KIND),
                    RETIREMENT_RESET_CAPTURE_SOURCE_CURSOR,
                ),
                RETIREMENT_RESET_CAPTURE_SOURCE_CURSOR => (
                    Some(CURRENT_CAPTURE_SOURCE_KIND),
                    Some(PRIOR_CAPTURE_SOURCE_KIND),
                    RETIREMENT_RESET_SEEN_CURSOR,
                ),
                RETIREMENT_RESET_SEEN_CURSOR => (None, None, ""),
                _ => return Err(StoreError::InvalidProviderFilePublicationScope),
            };

            let rows = if cursor == RETIREMENT_RESET_BATCH_CURSOR {
                let mut stmt = self.conn.prepare_cached(&format!(
                    "SELECT source_id, entity_id FROM {STAGING_BATCH_TABLE} \
                     WHERE replacement_id = ?1 ORDER BY source_id, entity_id LIMIT ?2"
                ))?;
                let rows = stmt
                    .query_map(
                        params![&replacement_id, capped_i64(remaining as u64)],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for (source_id, entity_id) in &rows {
                    self.conn.execute(
                        &format!(
                            "DELETE FROM {STAGING_BATCH_TABLE} WHERE replacement_id = ?1 \
                             AND source_id = ?2 AND entity_id = ?3"
                        ),
                        params![&replacement_id, source_id, entity_id],
                    )?;
                }
                rows.len()
            } else if let (Some(entity_kind), Some(prior_kind)) = (entity_kind, prior_kind) {
                let mut stmt = self.conn.prepare_cached(&format!(
                    "SELECT entity_id FROM {STAGING_SEEN_TABLE} WHERE replacement_id = ?1 \
                     AND entity_kind = ?2 ORDER BY entity_id LIMIT ?3"
                ))?;
                let ids = stmt
                    .query_map(
                        params![&replacement_id, entity_kind, capped_i64(remaining as u64)],
                        |row| row.get::<_, String>(0),
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for id in &ids {
                    self.conn.execute(
                        &format!(
                            "INSERT OR IGNORE INTO {STAGING_SEEN_TABLE} \
                             (replacement_id, entity_kind, entity_id) VALUES (?1, ?2, ?3)"
                        ),
                        params![&replacement_id, prior_kind, id],
                    )?;
                    self.conn.execute(
                        &format!(
                            "DELETE FROM {STAGING_SEEN_TABLE} WHERE replacement_id = ?1 \
                             AND entity_kind = ?2 AND entity_id = ?3"
                        ),
                        params![&replacement_id, entity_kind, id],
                    )?;
                }
                ids.len()
            } else {
                let mut stmt = self.conn.prepare_cached(&format!(
                    "SELECT entity_kind, entity_id FROM {STAGING_SEEN_TABLE} \
                     WHERE replacement_id = ?1 AND entity_kind NOT IN (?2, ?3) \
                     ORDER BY entity_kind, entity_id LIMIT ?4"
                ))?;
                let rows = stmt
                    .query_map(
                        params![
                            &replacement_id,
                            PRIOR_HISTORY_RECORD_KIND,
                            PRIOR_CAPTURE_SOURCE_KIND,
                            capped_i64(remaining as u64)
                        ],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for (entity_kind, entity_id) in &rows {
                    self.conn.execute(
                        &format!(
                            "DELETE FROM {STAGING_SEEN_TABLE} WHERE replacement_id = ?1 \
                             AND entity_kind = ?2 AND entity_id = ?3"
                        ),
                        params![&replacement_id, entity_kind, entity_id],
                    )?;
                }
                rows.len()
            };
            processed = processed.saturating_add(rows);
            if rows == remaining {
                break;
            }
            marker.preparation_cursor = (!next_cursor.is_empty()).then(|| next_cursor.to_owned());
            if next_cursor.is_empty() {
                break;
            }
        }
        Ok(processed)
    }

    fn reset_provider_file_publication_staging(
        &self,
        scope: &ProviderFilePublicationScope,
    ) -> Result<()> {
        let replacement_id = scope.scope_id.to_string();
        for table in [STAGING_BATCH_TABLE, STAGING_PRIOR_SOURCES_TABLE] {
            self.conn.execute(
                &format!("DELETE FROM {table} WHERE replacement_id = ?1"),
                params![&replacement_id],
            )?;
        }
        for (current_kind, prior_kind) in [
            ("history_record", PRIOR_HISTORY_RECORD_KIND),
            (CURRENT_CAPTURE_SOURCE_KIND, PRIOR_CAPTURE_SOURCE_KIND),
        ] {
            self.conn.execute(
                &format!(
                    "INSERT OR IGNORE INTO {STAGING_SEEN_TABLE} \
                     (replacement_id, entity_kind, entity_id) \
                     SELECT replacement_id, ?2, entity_id FROM {STAGING_SEEN_TABLE} \
                     WHERE replacement_id = ?1 AND entity_kind = ?3"
                ),
                params![&replacement_id, prior_kind, current_kind],
            )?;
        }
        self.conn.execute(
            &format!(
                "DELETE FROM {STAGING_SEEN_TABLE} WHERE replacement_id = ?1 \
                 AND entity_kind NOT IN (?2, ?3)"
            ),
            params![
                &replacement_id,
                PRIOR_HISTORY_RECORD_KIND,
                PRIOR_CAPTURE_SOURCE_KIND
            ],
        )?;
        let changed = self.conn.execute(
            r#"
            UPDATE provider_file_publications
            SET staging_initialized = 1,
                preparation_complete = CASE WHEN ?2 THEN 0 ELSE 1 END,
                preparation_cursor = NULL,
                cleanup_phase = 0,
                cleanup_source_cursor = NULL,
                cleanup_entity_cursor = NULL,
                removed_artifacts = 0,
                removed_summaries = 0,
                removed_history_record_links = 0,
                removed_history_records = 0,
                removed_history_record_tags = 0,
                removed_record_edges = 0,
                removed_audit_log_entries = 0,
                removed_vcs_workspaces = 0,
                removed_vcs_changes = 0,
                removed_events = 0,
                removed_runs = 0,
                removed_files_touched = 0,
                removed_session_edges = 0,
                tombstoned_sessions = 0,
                completion_payload_json = NULL
             WHERE replacement_id = ?1
            "#,
            params![&replacement_id, scope.tracks_prior_material],
        )?;
        if changed != 1 {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        Ok(())
    }

    fn initialize_provider_file_publication_retirement(
        &self,
        scope: &mut ProviderFilePublicationScope,
    ) -> Result<()> {
        let retirement_started: bool = self.conn.query_row(
            "SELECT retirement_started FROM provider_file_publications WHERE replacement_id = ?1",
            params![scope.scope_id.to_string()],
            |row| row.get(0),
        )?;
        if retirement_started {
            let marker = self.load_replacement_marker(scope)?;
            let replacement_id = scope.scope_id.to_string();
            let staged_state_exists: bool = self.conn.query_row(
                &format!(
                    "SELECT EXISTS (SELECT 1 FROM {STAGING_SEEN_TABLE} WHERE replacement_id = ?1) \
                     OR EXISTS (SELECT 1 FROM {STAGING_PRIOR_SOURCES_TABLE} WHERE replacement_id = ?1)"
                ),
                params![&replacement_id],
                |row| row.get(0),
            )?;
            let progress_without_state = marker.cleanup_phase != CLEANUP_PHASE_COMPLETE
                && !staged_state_exists
                && (marker.preparation_cursor.is_some()
                    || marker.preparation_complete
                    || marker.cleanup_phase != CLEANUP_PHASE_LINKS
                    || marker.source_cursor.is_some()
                    || marker.entity_cursor.is_some()
                    || marker.counts != ProviderFileReconciliationCounts::default());
            if progress_without_state && scope.tracks_prior_material {
                let changed = self.conn.execute(
                    r#"
                    UPDATE provider_file_publications
                    SET preparation_complete = 0, preparation_cursor = ?2,
                        cleanup_phase = 0, cleanup_source_cursor = NULL,
                        cleanup_entity_cursor = NULL, removed_artifacts = 0,
                        removed_summaries = 0, removed_history_record_links = 0,
                        removed_history_records = 0, removed_history_record_tags = 0,
                        removed_record_edges = 0, removed_audit_log_entries = 0,
                        removed_vcs_workspaces = 0, removed_vcs_changes = 0,
                        removed_events = 0, removed_runs = 0, removed_files_touched = 0,
                        removed_session_edges = 0, tombstoned_sessions = 0
                    WHERE replacement_id = ?1 AND retirement_started = 1
                    "#,
                    params![&replacement_id, RETIREMENT_RESET_BATCH_CURSOR],
                )?;
                if changed != 1 {
                    return Err(StoreError::InvalidProviderFilePublicationScope);
                }
            }
            scope.reuse_staging_state = true;
            return Ok(());
        }

        scope.kind = ProviderFilePublicationKind::Replacement;
        // A mutated publication may own material even when its durable staging
        // was created by an older schema. Preparation discovers that material
        // in bounded slices, so retirement never needs an eager owner scan.
        scope.tracks_prior_material = true;
        let changed = self.conn.execute(
            r#"
            UPDATE provider_file_publications
            SET publication_kind = 'replacement', tracks_prior_material = ?2,
                retirement_started = 1, staging_initialized = 1,
                preparation_complete = CASE WHEN ?2 THEN 0 ELSE 1 END,
                preparation_cursor = CASE WHEN ?2 THEN ?3 ELSE NULL END,
                cleanup_phase = 0, cleanup_source_cursor = NULL,
                cleanup_entity_cursor = NULL, removed_artifacts = 0,
                removed_summaries = 0, removed_history_record_links = 0,
                removed_history_records = 0, removed_history_record_tags = 0,
                removed_record_edges = 0, removed_audit_log_entries = 0,
                removed_vcs_workspaces = 0, removed_vcs_changes = 0,
                removed_events = 0, removed_runs = 0, removed_files_touched = 0,
                removed_session_edges = 0, tombstoned_sessions = 0,
                completion_payload_json = NULL
            WHERE replacement_id = ?1 AND retirement_started = 0
            "#,
            params![
                scope.scope_id.to_string(),
                scope.tracks_prior_material,
                RETIREMENT_RESET_BATCH_CURSOR
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        scope.reuse_staging_state = true;
        Ok(())
    }

    fn reclaim_orphaned_provider_staging(
        &self,
        scope: &ProviderFilePublicationScope,
    ) -> Result<()> {
        const MAX_RECLAIMED_PER_BEGIN: usize = 64;
        let lock_owner_id = provider_file_owner_lock_name(
            self.store_identity.digest(),
            scope.provider,
            &scope.material_source_format,
            &scope.material_source_root,
            &scope.source_path,
        );
        let prefix = format!("{STAGING_DIR_PREFIX}-{lock_owner_id}-");
        let root = self.store_identity.private_root();
        let mut reclaimed = 0usize;
        for entry in fs::read_dir(&root)? {
            if reclaimed >= MAX_RECLAIMED_PER_BEGIN {
                break;
            }
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.starts_with(&prefix) {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            if !metadata.file_type().is_dir()
                || metadata.file_type().is_symlink()
                || metadata_is_reparse_point(&metadata)
            {
                return Err(StoreError::ProviderFileStaging);
            }
            validate_existing_private_lock_dir(&entry.path(), &metadata)
                .map_err(|_| StoreError::ProviderFileStaging)?;
            for child in [
                "seen.sqlite-journal",
                "seen.sqlite-wal",
                "seen.sqlite-shm",
                "seen.sqlite",
            ] {
                let child_path = entry.path().join(child);
                if child_path.exists() {
                    validate_existing_private_staging_file_for_removal(&child_path)
                        .map_err(|_| StoreError::ProviderFileStaging)?;
                }
                match fs::remove_file(child_path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(StoreError::ProviderFileStaging),
                }
            }
            fs::remove_dir(entry.path()).map_err(|_| StoreError::ProviderFileStaging)?;
            reclaimed += 1;
        }
        Ok(())
    }

    fn cleanup_active_provider_file_publication(
        &self,
        scope_id: Uuid,
    ) -> std::result::Result<(), ProviderFileMaintenanceWarning> {
        let Some(active_scope_id) = self
            .provider_file_publication
            .borrow()
            .as_ref()
            .map(|active| active.scope_id)
        else {
            return Ok(());
        };
        if active_scope_id != scope_id {
            return Err(ProviderFileMaintenanceWarning::StagingCleanupDeferred {
                publication_id: scope_id.to_string(),
                operation: "scope-mismatch",
            });
        }
        if self.take_provider_file_fault(ProviderFileFaultPoint::Cleanup) {
            return Err(ProviderFileMaintenanceWarning::StagingCleanupDeferred {
                publication_id: scope_id.to_string(),
                operation: "fault-injection",
            });
        }

        self.provider_file_publication.replace(None);
        Ok(())
    }

    fn cleanup_abandoned_provider_file_publication(&self) -> Result<()> {
        let abandoned = self
            .provider_file_publication
            .borrow()
            .as_ref()
            .filter(|active| !active.lifecycle.load(Ordering::Acquire))
            .map(|active| active.scope_id);
        if let Some(scope_id) = abandoned {
            self.cleanup_active_provider_file_publication(scope_id)
                .map_err(maintenance_warning_as_error)?;
        }
        Ok(())
    }

    pub(crate) fn cleanup_provider_file_publication_on_drop(&self) {
        #[cfg(test)]
        self.provider_file_fault.set(None);
        let scope_id = self
            .provider_file_publication
            .borrow()
            .as_ref()
            .map(|active| active.scope_id);
        if let Some(scope_id) = scope_id {
            let _ = self.cleanup_active_provider_file_publication(scope_id);
        }
    }

    fn ensure_active_provider_file_publication(
        &self,
        scope: &ProviderFilePublicationScope,
    ) -> Result<()> {
        self.validate_provider_file_publication_scope(scope)?;
        let active = self.provider_file_publication.borrow();
        let Some(active) = active.as_ref() else {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        };
        if active.scope_id != scope.scope_id || !active.lifecycle.load(Ordering::Acquire) {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        Ok(())
    }

    fn validate_provider_file_import_read_scope(
        &self,
        scope: &ProviderFilePublicationScope,
    ) -> Result<()> {
        self.ensure_active_provider_file_publication(scope)
    }

    fn validate_provider_file_publication_scope(
        &self,
        scope: &ProviderFilePublicationScope,
    ) -> Result<()> {
        if scope.store_identity != self.store_identity.digest()
            || !scope.lifecycle.load(Ordering::Acquire)
        {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        Ok(())
    }

    fn provider_file_owner_has_prior_material(
        &self,
        provider: CaptureProvider,
        material_source_format: &str,
        material_source_root: &str,
        source_path: &str,
    ) -> Result<bool> {
        provider_file_owner_has_prior_material(
            &self.conn,
            provider,
            material_source_format,
            material_source_root,
            source_path,
        )
    }

    fn provider_file_publication_has_staged_material(&self, scope_id: Uuid) -> Result<bool> {
        self.conn
            .query_row(
                &format!(
                    "SELECT EXISTS (SELECT 1 FROM {STAGING_SEEN_TABLE} \
                     WHERE replacement_id = ?1)"
                ),
                params![scope_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }
}
