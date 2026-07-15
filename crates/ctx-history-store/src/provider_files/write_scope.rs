impl Store {
    fn with_atomic_provider_file_update<T>(&self, update: impl FnOnce() -> Result<T>) -> Result<T> {
        self.begin_immediate_batch()?;
        match update() {
            Ok(value) => match self.commit_batch() {
                Ok(()) => Ok(value),
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

    pub(crate) fn with_provider_file_inventory_result_write<T>(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        source_path: &str,
        write: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        self.cleanup_abandoned_provider_file_publication()?;
        let owns_transaction = self.conn.is_autocommit();
        if owns_transaction {
            self.begin_immediate_batch()?;
        }
        let result = (|| {
            self.ensure_provider_file_inventory_result_write_allowed(
                provider,
                source_root,
                source_path,
            )?;
            write()
        })();
        if !owns_transaction {
            return result;
        }
        match result {
            Ok(value) => match self.commit_batch() {
                Ok(()) => Ok(value),
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

    fn ensure_provider_file_inventory_result_write_allowed(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        source_path: &str,
    ) -> Result<()> {
        let effective = effective_provider_file_publication_predicate("publication");
        let marker = self
            .conn
            .query_row(
                &format!(
                    r#"
                    SELECT publication.replacement_id, publication.provider,
                           publication.source_path
                    FROM provider_file_publications AS publication
                    WHERE publication.provider = ?1
                      AND publication.material_source_root = ?2
                      AND publication.source_path = ?3
                      AND ({effective})
                    LIMIT 1
                    "#
                ),
                params![provider.as_str(), source_root, source_path],
                provider_file_marker_from_row,
            )
            .optional()?;
        let Some((replacement_id, marker_provider, _)) = marker else {
            return Ok(());
        };
        let exact_capability = self
            .provider_file_publication
            .borrow()
            .as_ref()
            .is_some_and(|active| {
                active.lifecycle.load(Ordering::Acquire)
                    && active.scope_id.to_string() == replacement_id
                    && self.provider_file_write_scope.get() == Some(active.scope_id)
            });
        if exact_capability {
            return Ok(());
        }
        Err(StoreError::ProviderFileReplacementBusy {
            provider: marker_provider,
            owner_id: replacement_id,
        })
    }

    /// Runs importer writes with an explicit publication capability. Ordinary
    /// Store writes are rejected while this publication is active, so an
    /// unrelated caller on the same connection cannot join it accidentally.
    pub fn with_provider_file_publication_writes<T>(
        &self,
        scope: &ProviderFilePublicationScope,
        writes: impl FnOnce(&Self) -> Result<T>,
    ) -> Result<T> {
        self.with_provider_file_publication_writes_inner(scope, false, writes)
    }

    fn with_provider_file_publication_writes_inner<T>(
        &self,
        scope: &ProviderFilePublicationScope,
        allow_staged_completion: bool,
        writes: impl FnOnce(&Self) -> Result<T>,
    ) -> Result<T> {
        self.ensure_active_provider_file_publication(scope)?;
        if scope.retires_observation || self.provider_file_write_scope.get().is_some() {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        if !allow_staged_completion
            && self
                .load_replacement_marker(scope)?
                .completion_payload_json
                .is_some()
        {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        self.provider_file_write_scope.set(Some(scope.scope_id));
        let _reset = ProviderFileWriteScopeReset {
            scope: &self.provider_file_write_scope,
        };
        writes(self)
    }

    /// Mutable counterpart for importers whose established API requires a
    /// mutable store reference while publishing provider-file material.
    pub fn with_provider_file_publication_writes_mut<T, E>(
        &mut self,
        scope: &ProviderFilePublicationScope,
        writes: impl FnOnce(&mut Self) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E>
    where
        E: From<StoreError>,
    {
        self.ensure_active_provider_file_publication(scope)
            .map_err(E::from)?;
        if scope.retires_observation || self.provider_file_write_scope.get().is_some() {
            return Err(E::from(StoreError::InvalidProviderFilePublicationScope));
        }
        if self
            .load_replacement_marker(scope)
            .map_err(E::from)?
            .completion_payload_json
            .is_some()
        {
            return Err(E::from(StoreError::InvalidProviderFilePublicationScope));
        }
        self.provider_file_write_scope.set(Some(scope.scope_id));
        let write_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| writes(self)));
        self.provider_file_write_scope.set(None);
        match write_result {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    pub(crate) fn with_provider_file_publication_write<T>(
        &self,
        write: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        self.cleanup_abandoned_provider_file_publication()?;
        let owns_transaction = self.conn.is_autocommit();
        if owns_transaction {
            self.begin_immediate_batch()?;
        }
        let result = (|| {
            if let Some(active) = self.provider_file_publication.borrow().as_ref() {
                if !active.lifecycle.load(Ordering::Acquire) || active.retires_observation {
                    return Err(StoreError::InvalidProviderFilePublicationScope);
                }
                if self.provider_file_write_scope.get() != Some(active.scope_id) {
                    return Err(StoreError::ProviderFileReplacementBusy {
                        provider: active.provider.as_str().to_owned(),
                        owner_id: active.owner_id.clone(),
                    });
                }
                let current = replacement_observation_current_predicate("publication");
                let changed = self.conn.execute(
                    &format!(
                        r#"
                        UPDATE provider_file_publications AS publication
                        SET mutation_started = 1
                        WHERE publication.replacement_id = ?1
                          AND publication.preparation_complete = 1
                          AND ({current})
                        "#
                    ),
                    params![active.scope_id.to_string()],
                )?;
                if changed != 1 {
                    return Err(StoreError::ProviderFileObservationChanged {
                        provider: active.provider.as_str().to_owned(),
                        owner_id: opaque_provider_file_owner_id(
                            active.provider,
                            &active.material_source_format,
                            &active.material_source_root,
                            &active.source_path,
                        ),
                    });
                }
                invalidate_semantic_searchable_item_stats(&self.conn)?;
            } else if self.provider_file_write_scope.get().is_some() {
                return Err(StoreError::InvalidProviderFilePublicationScope);
            }
            let value = write()?;
            if self.take_provider_file_fault(ProviderFileFaultPoint::MutationBeforeCommit) {
                return Err(StoreError::ProviderFileStaging);
            }
            Ok(value)
        })();
        if !owns_transaction {
            return result;
        }
        match result {
            Ok(value) => match self.commit_batch() {
                Ok(()) => Ok(value),
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

    fn record_matching_provider_file_outcome(
        &self,
        outcome: ProviderFileImportOutcome<'_>,
        completion_kind: ProviderFileCompletionKind,
        has_safe_checkpoint: bool,
    ) -> Result<()> {
        validate_successful_outcome(outcome)?;
        self.ensure_provider_file_observation_is_current(outcome.provider, outcome.observation)?;
        let changed = match outcome.observation {
            ProviderFileInventoryObservation::Catalog { mut update, .. } => {
                let (prior_status, prior_error, prior_event_count) = self.conn.query_row(
                    r#"
                    SELECT indexed_status, indexed_error, last_imported_event_count
                    FROM catalog_sessions
                    WHERE provider = ?1 AND source_root = ?2 AND source_path = ?3
                    "#,
                    params![
                        outcome.provider.as_str(),
                        update.source_root,
                        update.source_path
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, Option<i64>>(2)?,
                        ))
                    },
                )?;
                let preserve_rejections = completion_kind
                    != ProviderFileCompletionKind::Replacement
                    && (prior_status == "completed_with_rejections"
                        || (prior_status == "indexed"
                            && outcome.status == CatalogIndexedStatus::Rejected));
                let status = if preserve_rejections {
                    CatalogIndexedStatus::CompletedWithRejections
                } else {
                    outcome.status
                };
                let error = if preserve_rejections {
                    prior_error.as_deref().or(outcome.error)
                } else {
                    outcome.error
                };
                let prior_event_count =
                    prior_event_count.map(nonnegative_i64_to_u64).transpose()?;
                update.event_count = match completion_kind {
                    ProviderFileCompletionKind::Replacement => update.event_count,
                    ProviderFileCompletionKind::AppendDelta => match update.event_count {
                        Some(delta) => {
                            Some(prior_event_count.unwrap_or(0).checked_add(delta).ok_or(
                                StoreError::ProviderFileReconciliationInconsistent {
                                    entity: "catalog event count",
                                },
                            )?)
                        }
                        None => prior_event_count,
                    },
                    ProviderFileCompletionKind::RetainCheckpoint => prior_event_count,
                };
                let changed = if completion_kind == ProviderFileCompletionKind::RetainCheckpoint {
                    self.record_catalog_source_import_result_preserving_legacy_cursor(
                        outcome.provider,
                        update,
                        status,
                        error,
                    )?
                } else {
                    self.record_catalog_source_import_result(
                        outcome.provider,
                        update,
                        status,
                        error,
                    )?
                };
                if changed == 1 && has_safe_checkpoint {
                    self.conn.execute(
                        r#"
                        UPDATE catalog_sessions
                        SET last_imported_at_ms = ?4,
                            last_imported_file_size_bytes = ?5,
                            last_imported_file_modified_at_ms = ?6,
                            last_imported_file_sha256 = ?7,
                            last_imported_event_count = ?8
                        WHERE provider = ?1 AND source_root = ?2 AND source_path = ?3
                          AND indexed_at_ms = ?4 AND indexed_file_size_bytes = ?5
                          AND indexed_file_modified_at_ms = ?6
                          AND indexed_import_revision = ?9
                        "#,
                        params![
                            outcome.provider.as_str(),
                            update.source_root,
                            update.source_path,
                            update.indexed_at_ms,
                            capped_i64(update.file_size_bytes),
                            update.file_modified_at_ms,
                            update.file_sha256,
                            update.event_count.map(capped_i64),
                            i64::from(update.import_revision),
                        ],
                    )?;
                }
                changed
            }
            ProviderFileInventoryObservation::SourceImport { update, .. } => {
                let (prior_status, prior_error) = self.conn.query_row(
                    r#"
                    SELECT indexed_status, indexed_error
                    FROM source_import_files
                    WHERE provider = ?1 AND source_root = ?2 AND source_path = ?3
                    "#,
                    params![
                        outcome.provider.as_str(),
                        update.source_root,
                        update.source_path
                    ],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                )?;
                let preserve_rejections = prior_status == "completed_with_rejections"
                    && completion_kind != ProviderFileCompletionKind::Replacement;
                let status = if preserve_rejections {
                    CatalogIndexedStatus::CompletedWithRejections
                } else {
                    outcome.status
                };
                let error = if preserve_rejections {
                    prior_error.as_deref().or(outcome.error)
                } else {
                    outcome.error
                };
                self.record_source_import_file_result(outcome.provider, update, status, error)?
            }
        };
        if changed != 1 {
            return Err(provider_file_observation_changed(
                outcome.provider,
                outcome.observation,
            ));
        }
        Ok(())
    }
}
