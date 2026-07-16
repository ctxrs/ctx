impl Store {
    pub fn finalize_provider_file_publication(
        &self,
        scope: ProviderFilePublicationScope,
        outcome: ProviderFileImportOutcome<'_>,
        commit: ProviderFilePublicationCommit<'_>,
    ) -> Result<ProviderFileFinalizeOutcome> {
        let durable_result = (|| {
            self.validate_provider_file_publication_scope(&scope)?;
            if scope.retires_observation {
                return Err(StoreError::InvalidProviderFilePublicationScope);
            }
            validate_scope_matches_outcome(&scope, outcome)?;
            let (completion_kind, checkpoint) = match commit {
                ProviderFilePublicationCommit::Append(checkpoint) => {
                    if scope.kind != ProviderFilePublicationKind::Incremental {
                        return Err(StoreError::InvalidProviderFilePublicationScope);
                    }
                    (ProviderFileCompletionKind::AppendDelta, Some(checkpoint))
                }
                ProviderFilePublicationCommit::RetainCheckpoint => {
                    if scope.kind != ProviderFilePublicationKind::Incremental {
                        return Err(StoreError::InvalidProviderFilePublicationScope);
                    }
                    (ProviderFileCompletionKind::RetainCheckpoint, None)
                }
                ProviderFilePublicationCommit::Replacement(checkpoint) => {
                    if scope.kind != ProviderFilePublicationKind::Replacement {
                        return Err(StoreError::InvalidProviderFilePublicationScope);
                    }
                    (ProviderFileCompletionKind::Replacement, checkpoint)
                }
            };
            validate_provider_file_completion_outcome(
                outcome,
                completion_kind,
                checkpoint.is_some(),
                scope.tracks_prior_material,
            )?;
            if let Some(checkpoint) = checkpoint {
                validate_checkpoint_for_outcome(outcome, checkpoint)?;
            }
            self.ensure_active_provider_file_publication(&scope)?;

            self.with_provider_file_publication_writes_inner(&scope, true, |_| {
                self.with_atomic_provider_file_update(|| {
                    self.ensure_scope_observation_is_current(&scope)?;
                    let marker = self.load_replacement_marker(&scope)?;
                    if marker.publication_kind != scope.kind {
                        return Err(StoreError::InvalidProviderFilePublicationScope);
                    }
                    let counts = if completion_kind == ProviderFileCompletionKind::Replacement {
                        self.validate_replacement_marker(&scope, &marker)?;
                        if !marker.preparation_complete
                            || (scope.tracks_prior_material
                                && marker.cleanup_phase != CLEANUP_PHASE_COMPLETE)
                        {
                            return Err(StoreError::ProviderFileReconciliationIncomplete);
                        }
                        marker.counts
                    } else {
                        ProviderFileReconciliationCounts::default()
                    };
                    if outcome.status == CatalogIndexedStatus::Rejected {
                        self.ensure_rejected_publication_has_no_material(&scope)?;
                    }
                    self.record_matching_provider_file_outcome(
                        outcome,
                        completion_kind,
                        checkpoint.is_some(),
                    )?;
                    match completion_kind {
                        ProviderFileCompletionKind::Replacement => {
                            self.replace_provider_file_checkpoint(outcome, checkpoint)?;
                        }
                        ProviderFileCompletionKind::AppendDelta => {
                            self.advance_provider_file_checkpoint(
                                checkpoint
                                    .ok_or(StoreError::InvalidProviderFilePublicationScope)?,
                            )?;
                        }
                        ProviderFileCompletionKind::RetainCheckpoint => {}
                    }
                    let deleted = self.conn.execute(
                        "DELETE FROM provider_file_publications WHERE replacement_id = ?1",
                        params![scope.scope_id.to_string()],
                    )?;
                    if deleted != 1 {
                        return Err(StoreError::InvalidProviderFilePublicationScope);
                    }
                    invalidate_semantic_searchable_item_stats(&self.conn)?;
                    if completion_kind == ProviderFileCompletionKind::Replacement
                        && scope.tracks_prior_material
                    {
                        self.bump_semantic_replacement_revision()?;
                    }
                    if self.take_provider_file_fault(ProviderFileFaultPoint::FinalizeBeforeCommit) {
                        return Err(StoreError::ProviderFileStaging);
                    }
                    Ok(counts)
                })
            })
        })();

        let counts = match durable_result {
            Ok(counts) => counts,
            Err(error) => {
                let _ = self.abort_provider_file_publication_durable(&scope);
                scope.lifecycle.store(false, Ordering::Release);
                let _ = self.cleanup_active_provider_file_publication(scope.scope_id);
                return Err(error);
            }
        };
        scope.lifecycle.store(false, Ordering::Release);
        let maintenance_warning = self
            .cleanup_active_provider_file_publication(scope.scope_id)
            .err();
        Ok(ProviderFileFinalizeOutcome {
            reconciliation: counts,
            maintenance_warning,
        })
    }

    pub fn semantic_replacement_revision(&self) -> Result<u64> {
        self.conn
            .query_row(
                "SELECT current_revision FROM semantic_replacement_revision WHERE singleton = 1",
                [],
                |row| nonnegative_i64_to_u64(row.get(0)?),
            )
            .map_err(StoreError::from)
    }

    pub fn has_pending_provider_file_publications(&self) -> Result<bool> {
        has_fenced_provider_file_publications(&self.conn)
    }

    pub fn effective_provider_file_publication_inventory_owner(
        &self,
    ) -> Result<Option<ProviderFilePublicationInventoryOwner>> {
        let global_id = global_provider_file_publication_id_sql();
        self.conn
            .query_row(
                &format!(
                    r#"
                    SELECT publication.provider, publication.inventory_family,
                           publication.inventory_source_format,
                           publication.inventory_source_root, publication.source_path,
                           publication.inventory_generation, publication.file_size_bytes,
                           publication.file_modified_at_ms, publication.import_revision,
                           publication.metadata_json
                    FROM provider_file_publications AS publication
                    WHERE publication.replacement_id = ({global_id})
                    "#
                ),
                [],
                |row| {
                    let provider = CaptureProvider::from_str(&row.get::<_, String>(0)?)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?;
                    let inventory_family = match row.get::<_, String>(1)?.as_str() {
                        CATALOG_INVENTORY_FAMILY => ProviderFileInventoryFamily::Catalog,
                        SOURCE_IMPORT_INVENTORY_FAMILY => ProviderFileInventoryFamily::SourceImport,
                        _ => return Err(rusqlite::Error::InvalidQuery),
                    };
                    Ok(ProviderFilePublicationInventoryOwner {
                        provider,
                        inventory_family,
                        source_format: row.get(2)?,
                        source_root: row.get(3)?,
                        source_path: row.get(4)?,
                        inventory_generation: nonnegative_i64_to_u64(row.get(5)?)?,
                        file_size_bytes: nonnegative_i64_to_u64(row.get(6)?)?,
                        file_modified_at_ms: row.get(7)?,
                        import_revision: nonnegative_i64_to_u32(row.get(8)?)?,
                        metadata_json: row.get(9)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn effective_provider_file_publication_has_staged_completion(&self) -> Result<bool> {
        let global_id = global_provider_file_publication_id_sql();
        self.conn
            .query_row(
                &format!(
                    "SELECT COALESCE((SELECT completion_payload_json IS NOT NULL \
                     FROM provider_file_publications WHERE replacement_id = ({global_id})), 0)"
                ),
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    pub fn invalidate_effective_provider_file_publication_observation(
        &self,
        owner: &ProviderFilePublicationInventoryOwner,
        invalidated_at_ms: i64,
    ) -> Result<bool> {
        self.with_atomic_provider_file_update(|| {
            if self
                .effective_provider_file_publication_inventory_owner()?
                .as_ref()
                != Some(owner)
            {
                return Ok(false);
            }
            let global_id = global_provider_file_publication_id_sql();
            let (publication_id, mutation_started) = self.conn.query_row(
                &format!(
                    "SELECT replacement_id, mutation_started FROM provider_file_publications \
                     WHERE replacement_id = ({global_id})"
                ),
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
            )?;
            let changed = match owner.inventory_family {
                ProviderFileInventoryFamily::Catalog => self.conn.execute(
                    r#"
                        UPDATE catalog_sessions
                        SET is_stale = 1, cataloged_at_ms = ?9
                        WHERE provider = ?1 AND source_format = ?2
                          AND source_root = ?3 AND source_path = ?4
                          AND file_size_bytes = ?5 AND file_modified_at_ms = ?6
                          AND import_revision = ?7 AND is_stale = 0
                        "#,
                    params![
                        owner.provider.as_str(),
                        &owner.source_format,
                        &owner.source_root,
                        &owner.source_path,
                        capped_i64(owner.file_size_bytes),
                        owner.file_modified_at_ms,
                        i64::from(owner.import_revision),
                        &owner.metadata_json,
                        invalidated_at_ms,
                    ],
                )?,
                ProviderFileInventoryFamily::SourceImport => self.conn.execute(
                    r#"
                        UPDATE source_import_files
                        SET is_stale = 1, observed_at_ms = ?9
                        WHERE provider = ?1 AND source_format = ?2
                          AND source_root = ?3 AND source_path = ?4
                          AND file_size_bytes = ?5 AND file_modified_at_ms = ?6
                          AND import_revision = ?7 AND metadata_json IS ?8
                          AND is_stale = 0
                        "#,
                    params![
                        owner.provider.as_str(),
                        &owner.source_format,
                        &owner.source_root,
                        &owner.source_path,
                        capped_i64(owner.file_size_bytes),
                        owner.file_modified_at_ms,
                        i64::from(owner.import_revision),
                        &owner.metadata_json,
                        invalidated_at_ms,
                    ],
                )?,
            };
            let publication_changed = if mutation_started {
                self.conn.execute(
                    "UPDATE provider_file_publications \
                     SET inventory_observation_invalidated = 1, \
                         updated_at_ms = MAX(updated_at_ms + 1, ?1) \
                     WHERE replacement_id = ?2 AND mutation_started != 0",
                    params![invalidated_at_ms, publication_id],
                )?
            } else if changed == 1 {
                self.conn.execute(
                    "DELETE FROM provider_file_publications \
                     WHERE replacement_id = ?1 AND mutation_started = 0",
                    params![publication_id],
                )?
            } else {
                return Ok(false);
            };
            if publication_changed != 1 {
                return Err(StoreError::InvalidProviderFilePublicationScope);
            }
            Ok(true)
        })
    }

    pub fn provider_file_publication_matches_candidate(
        &self,
        provider: CaptureProvider,
        observation: ProviderFileInventoryObservation<'_>,
        material_source_format: &str,
        material_source_root: &str,
    ) -> Result<bool> {
        let owner_id = opaque_provider_file_owner_id(
            provider,
            material_source_format,
            material_source_root,
            observation.source_path(),
        );
        let global_id = global_provider_file_publication_id_sql();
        self.conn
            .query_row(
                &format!(
                    r#"
                    SELECT EXISTS (
                        SELECT 1 FROM provider_file_publications AS publication
                        WHERE publication.replacement_id = ({global_id})
                          AND publication.owner_id = ?1
                          AND publication.provider = ?2
                          AND publication.inventory_family = ?3
                          AND publication.inventory_source_format = ?4
                          AND publication.inventory_source_root = ?5
                          AND publication.source_path = ?6
                          AND publication.material_source_format = ?7
                          AND publication.material_source_root = ?8
                          AND publication.file_size_bytes = ?9
                          AND publication.file_modified_at_ms = ?10
                          AND publication.import_revision = ?11
                          AND publication.metadata_json IS ?12
                          AND ({})
                    )
                    "#,
                    provider_file_retirement_observation_current_predicate("publication")
                ),
                params![
                    owner_id,
                    provider.as_str(),
                    observation.inventory_family(),
                    observation.source_format(),
                    observation.source_root(),
                    observation.source_path(),
                    material_source_format,
                    material_source_root,
                    capped_i64(observation.file_size_bytes()),
                    observation.file_modified_at_ms(),
                    i64::from(observation.import_revision()),
                    observation.metadata_json()?,
                ],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    pub fn provider_file_publication_retirement_work_count(&self) -> Result<usize> {
        let current_observation =
            provider_file_retirement_observation_current_predicate("publication");
        self.conn
            .query_row(
                &format!(
                    r#"
                SELECT COUNT(*)
                FROM provider_file_publications AS publication
                WHERE publication.mutation_started != 0
                  AND NOT ({current_observation})
                "#
                ),
                [],
                |row| nonnegative_i64_to_usize(row.get(0)?),
            )
            .map_err(StoreError::from)
    }

    pub fn list_provider_file_publication_retirement_work(
        &self,
        limit: usize,
    ) -> Result<Vec<ProviderFilePublicationRetirementWork>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let current_observation =
            provider_file_retirement_observation_current_predicate("publication");
        let mut stmt = self.conn.prepare(&format!(
            r#"
            SELECT publication.provider, publication.material_source_format,
                   publication.material_source_root, publication.source_path,
                   publication.file_size_bytes, publication.updated_at_ms
            FROM provider_file_publications AS publication
            WHERE publication.mutation_started != 0
              AND NOT ({current_observation})
            ORDER BY publication.updated_at_ms, publication.replacement_id
            LIMIT ?1
            "#
        ))?;
        let rows = stmt.query_map(params![capped_i64(limit as u64)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                nonnegative_i64_to_u64(row.get(4)?)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        let mut work = Vec::new();
        for row in rows {
            let (
                provider,
                material_source_format,
                material_source_root,
                source_path,
                bytes,
                last_attempt_at_ms,
            ) = row?;
            let provider = CaptureProvider::from_str(&provider)
                .map_err(|_| StoreError::InvalidProviderFilePublicationScope)?;
            work.push(ProviderFilePublicationRetirementWork {
                provider,
                material_source_format,
                material_source_root,
                source_path,
                estimated_bytes: bytes,
                last_attempt_at_ms,
            });
        }
        Ok(work)
    }
}
