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
        if !scope.tracks_prior_material {
            self.ensure_active_provider_file_publication(scope)?;
            return Ok(ProviderFileReconciliationProgress {
                rows_scanned: 0,
                complete: true,
                counts: ProviderFileReconciliationCounts::default(),
            });
        }
        self.ensure_active_provider_file_publication(scope)?;
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
            self.ensure_scope_observation_is_current(scope)?;
            let mut marker = self.load_replacement_marker(scope)?;
            if !marker.preparation_complete {
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
                complete: true,
            });
        }
        if !scope.tracks_prior_material {
            return Ok(ProviderFilePreparationProgress {
                source_ids_staged: 0,
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
            self.ensure_scope_observation_is_current(scope)?;
            let mut marker = self.load_replacement_marker(scope)?;
            if marker.preparation_complete {
                return Ok(ProviderFilePreparationProgress {
                    source_ids_staged: 0,
                    complete: true,
                });
            }
            let sqlite_limit = i64::try_from(max_rows + 1).map_err(|_| {
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
            let complete = ids.len() <= max_rows;
            if !complete {
                ids.pop();
            }
            let mut insert = self.conn.prepare_cached(&format!(
                "INSERT OR IGNORE INTO {STAGING_SCHEMA}.prior_sources (id) VALUES (?1)"
            ))?;
            for id in &ids {
                insert.execute(params![id])?;
            }
            marker.preparation_cursor = ids.last().cloned();
            marker.preparation_complete = complete;
            self.update_replacement_marker(scope, &marker)?;
            Ok(ProviderFilePreparationProgress {
                source_ids_staged: ids.len(),
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
    ) -> Result<()> {
        self.validate_provider_file_publication_scope(&scope)?;
        scope.lifecycle.store(false, Ordering::Release);
        self.cleanup_active_provider_file_publication(scope.scope_id)
            .map_err(maintenance_warning_as_error)
    }
}
