impl Store {
    pub fn reconcile_provider_file_publication_slice(
        &self,
        scope: &ProviderFilePublicationScope,
        max_rows: usize,
    ) -> Result<ProviderFileReconciliationProgress> {
        if !(1..=PROVIDER_FILE_RECONCILIATION_MAX_ROWS).contains(&max_rows) {
            return Err(StoreError::ProviderFileReconciliationLimitOutOfRange {
                value: max_rows,
                max: PROVIDER_FILE_RECONCILIATION_MAX_ROWS,
            });
        }
        self.validate_provider_file_publication_scope(scope)?;
        if scope.kind != ProviderFilePublicationKind::Replacement {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        self.ensure_active_provider_file_publication(scope)?;
        if !scope.tracks_prior_material {
            let marker = self.load_replacement_marker(scope)?;
            self.validate_replacement_marker(scope, &marker)?;
            self.ensure_scope_observation_allows_progress(scope, &marker)?;
            if (scope.tracks_prior_material && !marker.preparation_complete)
                || (!scope.retires_observation && marker.completion_payload_json.is_none())
            {
                return Err(StoreError::ProviderFileReconciliationIncomplete);
            }
            return Ok(ProviderFileReconciliationProgress {
                rows_scanned: 0,
                complete: true,
                counts: ProviderFileReconciliationCounts::default(),
            });
        }
        if !self
            .provider_file_publication
            .borrow()
            .as_ref()
            .is_some_and(|active| active.attached)
        {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }

        self.begin_immediate_batch()?;
        let result = (|| {
            let mut marker = self.load_replacement_marker(scope)?;
            self.validate_replacement_marker(scope, &marker)?;
            self.ensure_scope_observation_allows_progress(scope, &marker)?;
            if !marker.preparation_complete {
                return Err(StoreError::ProviderFileReconciliationIncomplete);
            }
            if !scope.retires_observation && marker.completion_payload_json.is_none() {
                return Err(StoreError::ProviderFileReconciliationIncomplete);
            }
            marker.mutation_started = true;
            let mut rows_scanned = 0usize;
            while rows_scanned < max_rows && marker.cleanup_phase < CLEANUP_PHASE_COMPLETE {
                let remaining = max_rows - rows_scanned;
                let batch = self.reconcile_phase_batch(
                    scope,
                    marker.cleanup_phase,
                    marker.source_cursor.as_deref(),
                    marker.entity_cursor.as_deref(),
                    remaining,
                )?;
                rows_scanned += batch.visited;
                marker.counts = marker.counts.checked_add(batch.removed)?;
                if batch.phase_complete {
                    marker.cleanup_phase += 1;
                    marker.source_cursor = None;
                    marker.entity_cursor = None;
                } else {
                    marker.source_cursor = batch.source_cursor;
                    marker.entity_cursor = batch.entity_cursor;
                }
            }
            self.update_replacement_marker(scope, &marker)?;
            Ok(ProviderFileReconciliationProgress {
                rows_scanned,
                complete: marker.cleanup_phase == CLEANUP_PHASE_COMPLETE,
                counts: marker.counts,
            })
        })();
        match result {
            Ok(progress) => match self.commit_batch() {
                Ok(()) => Ok(progress),
                Err(error) => {
                    let _ = self.rollback_batch();
                    Err(error)
                }
            },
            Err(error) => {
                let _ = self.rollback_batch();
                Err(error)
            }
        }
    }

    pub fn prepare_provider_file_publication_slice(
        &self,
        scope: &ProviderFilePublicationScope,
        max_rows: usize,
    ) -> Result<ProviderFilePreparationProgress> {
        if !(1..=PROVIDER_FILE_PREPARATION_MAX_ROWS).contains(&max_rows) {
            return Err(StoreError::ProviderFileReconciliationLimitOutOfRange {
                value: max_rows,
                max: PROVIDER_FILE_PREPARATION_MAX_ROWS,
            });
        }
        self.ensure_active_provider_file_publication(scope)?;
        if scope.kind == ProviderFilePublicationKind::Incremental {
            return Ok(ProviderFilePreparationProgress {
                source_ids_staged: 0,
                rows_processed: 0,
                complete: true,
            });
        }
        if !scope.tracks_prior_material {
            return Ok(ProviderFilePreparationProgress {
                source_ids_staged: 0,
                rows_processed: 0,
                complete: true,
            });
        }
        if !self
            .provider_file_publication
            .borrow()
            .as_ref()
            .is_some_and(|active| active.attached)
        {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }

        self.begin_immediate_batch()?;
        let result = (|| {
            let mut marker = self.load_replacement_marker(scope)?;
            self.validate_replacement_marker(scope, &marker)?;
            self.ensure_scope_observation_allows_progress(scope, &marker)?;
            if marker.preparation_complete {
                return Ok(ProviderFilePreparationProgress {
                    source_ids_staged: 0,
                    rows_processed: 0,
                    complete: true,
                });
            }
            let reset_rows = if marker
                .preparation_cursor
                .as_deref()
                .is_some_and(is_retirement_reset_cursor)
            {
                self.reset_provider_file_publication_staging_slice(scope, &mut marker, max_rows)?
            } else {
                0
            };
            if marker
                .preparation_cursor
                .as_deref()
                .is_some_and(is_retirement_reset_cursor)
                || reset_rows == max_rows
            {
                self.update_replacement_marker(scope, &marker)?;
                return Ok(ProviderFilePreparationProgress {
                    source_ids_staged: 0,
                    rows_processed: reset_rows,
                    complete: false,
                });
            }
            let remaining_rows = max_rows.saturating_sub(reset_rows);
            let sqlite_limit = i64::try_from(remaining_rows + 1).map_err(|_| {
                StoreError::ProviderFileReconciliationLimitOutOfRange {
                    value: max_rows,
                    max: PROVIDER_FILE_PREPARATION_MAX_ROWS,
                }
            })?;
            let sql = format!(
                r#"
                SELECT source.id
                FROM capture_sources AS source
                WHERE {}
                  AND (?5 IS NULL OR source.id > ?5)
                ORDER BY source.id
                LIMIT ?6
                "#,
                material_owner_predicate("source", "?1", "?2", "?3", "?4")
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(
                params![
                    scope.provider.as_str(),
                    &scope.material_source_format,
                    &scope.material_source_root,
                    &scope.source_path,
                    marker.preparation_cursor.as_deref(),
                    sqlite_limit,
                ],
                |row| row.get::<_, String>(0),
            )?;
            let mut ids = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            let complete = ids.len() <= remaining_rows;
            if !complete {
                ids.pop();
            }
            let mut insert = self.conn.prepare_cached(&format!(
                "INSERT OR IGNORE INTO {STAGING_PRIOR_SOURCES_TABLE} \
                 (replacement_id, source_id) VALUES (?1, ?2)"
            ))?;
            let replacement_id = scope.scope_id.to_string();
            for id in &ids {
                insert.execute(params![&replacement_id, id])?;
            }
            marker.preparation_cursor = ids.last().cloned();
            marker.preparation_complete = complete;
            if complete && scope.retires_observation {
                let staged_state_exists: bool = self.conn.query_row(
                    &format!(
                        "SELECT EXISTS (SELECT 1 FROM {STAGING_SEEN_TABLE} WHERE replacement_id = ?1) \
                         OR EXISTS (SELECT 1 FROM {STAGING_PRIOR_SOURCES_TABLE} WHERE replacement_id = ?1)"
                    ),
                    params![&replacement_id],
                    |row| row.get(0),
                )?;
                if !staged_state_exists {
                    marker.cleanup_phase = CLEANUP_PHASE_COMPLETE;
                    marker.source_cursor = None;
                    marker.entity_cursor = None;
                }
            }
            self.update_replacement_marker(scope, &marker)?;
            if self.take_provider_file_fault(ProviderFileFaultPoint::PreparationBeforeCommit) {
                return Err(StoreError::ProviderFileStaging);
            }
            Ok(ProviderFilePreparationProgress {
                source_ids_staged: ids.len(),
                rows_processed: reset_rows.saturating_add(ids.len()),
                complete,
            })
        })();
        match result {
            Ok(progress) => match self.commit_batch() {
                Ok(()) => Ok(progress),
                Err(error) => {
                    let _ = self.rollback_batch();
                    Err(error)
                }
            },
            Err(error) => {
                let _ = self.rollback_batch();
                Err(error)
            }
        }
    }

    pub fn abandon_provider_file_publication(
        &self,
        scope: ProviderFilePublicationScope,
    ) -> Result<Option<ProviderFileMaintenanceWarning>> {
        self.validate_provider_file_publication_scope(&scope)?;
        let durable_result = if scope.retires_observation {
            self.advance_provider_file_publication_attempt(&scope, utc_now().timestamp_millis())
        } else {
            Ok(())
        };
        scope.lifecycle.store(false, Ordering::Release);
        let maintenance_warning = self
            .cleanup_active_provider_file_publication(scope.scope_id)
            .err();
        durable_result?;
        Ok(maintenance_warning)
    }

    /// Releases a failed publication and removes its durable marker only when
    /// importer writes have not started. `Continue` means the publication was
    /// cancelled; `Break` means mutation started and the durable marker remains
    /// fenced for replacement or retirement recovery. Either outcome carries
    /// a non-fatal staging cleanup warning when local maintenance was deferred.
    pub fn abort_provider_file_publication(
        &self,
        scope: ProviderFilePublicationScope,
    ) -> Result<
        ControlFlow<Option<ProviderFileMaintenanceWarning>, Option<ProviderFileMaintenanceWarning>>,
    > {
        let durable_result = self.abort_provider_file_publication_durable(&scope);

        scope.lifecycle.store(false, Ordering::Release);
        let maintenance_warning = self
            .cleanup_active_provider_file_publication(scope.scope_id)
            .err();
        let cancelled = durable_result?;
        Ok(if cancelled {
            ControlFlow::Continue(maintenance_warning)
        } else {
            ControlFlow::Break(maintenance_warning)
        })
    }

    fn abort_provider_file_publication_durable(
        &self,
        scope: &ProviderFilePublicationScope,
    ) -> Result<bool> {
        self.validate_provider_file_publication_scope(scope)?;
        if scope.retires_observation {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        self.ensure_active_provider_file_publication(scope)?;

        self.with_atomic_provider_file_update(|| {
            let marker = self.load_replacement_marker(scope)?;
            if marker.publication_kind != scope.kind {
                return Err(StoreError::InvalidProviderFilePublicationScope);
            }
            if marker.mutation_started {
                return Ok(false);
            }
            let deleted = self.conn.execute(
                "DELETE FROM provider_file_publications WHERE replacement_id = ?1 AND mutation_started = 0",
                params![scope.scope_id.to_string()],
            )?;
            if deleted != 1 {
                return Err(StoreError::InvalidProviderFilePublicationScope);
            }
            invalidate_semantic_searchable_item_stats(&self.conn)?;
            Ok(true)
        })
    }

    pub fn retire_provider_file_publication(
        &self,
        scope: ProviderFilePublicationScope,
    ) -> Result<ProviderFileFinalizeOutcome> {
        self.validate_provider_file_publication_scope(&scope)?;
        if !scope.retires_observation {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        self.ensure_active_provider_file_publication(&scope)?;

        let durable_result = self.with_atomic_provider_file_update(|| {
            let marker = self.load_replacement_marker(&scope)?;
            if marker.publication_kind != scope.kind {
                return Err(StoreError::InvalidProviderFilePublicationScope);
            }
            self.ensure_scope_observation_allows_progress(&scope, &marker)?;
            if (scope.kind == ProviderFilePublicationKind::Replacement
                && (!marker.preparation_complete
                    || (scope.tracks_prior_material
                        && marker.cleanup_phase != CLEANUP_PHASE_COMPLETE)))
                || !marker.mutation_started
            {
                return Err(StoreError::ProviderFileReconciliationIncomplete);
            }
            self.delete_provider_file_checkpoint_for_scope(&scope)?;
            self.retire_stale_provider_file_observation(&scope)?;
            let deleted = self.conn.execute(
                "DELETE FROM provider_file_publications WHERE replacement_id = ?1",
                params![scope.scope_id.to_string()],
            )?;
            if deleted != 1 {
                return Err(StoreError::InvalidProviderFilePublicationScope);
            }
            invalidate_semantic_searchable_item_stats(&self.conn)?;
            self.bump_semantic_replacement_revision()?;
            if self.take_provider_file_fault(ProviderFileFaultPoint::FinalizeBeforeCommit) {
                return Err(StoreError::ProviderFileStaging);
            }
            #[cfg(test)]
            {
                if self
                    .take_provider_file_fault(ProviderFileFaultPoint::RetirementFinalizeProcessExit)
                {
                    std::process::exit(37);
                }
            }
            Ok(marker.counts)
        });

        scope.lifecycle.store(false, Ordering::Release);
        let counts = match durable_result {
            Ok(counts) => counts,
            Err(error) => {
                let _ = self.cleanup_active_provider_file_publication(scope.scope_id);
                return Err(error);
            }
        };
        let maintenance_warning = self
            .cleanup_active_provider_file_publication(scope.scope_id)
            .err();
        Ok(ProviderFileFinalizeOutcome {
            reconciliation: counts,
            maintenance_warning,
        })
    }
}
