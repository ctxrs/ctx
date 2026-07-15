impl Store {
    pub fn retire_provider_file_publication(
        &self,
        scope: ProviderFilePublicationScope,
    ) -> Result<ProviderFileFinalizeOutcome> {
        self.validate_provider_file_publication_scope(&scope)?;
        if !scope.retires_observation || scope.kind != ProviderFilePublicationKind::Replacement {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        self.ensure_active_provider_file_publication(&scope)?;

        let durable_result = self.with_atomic_provider_file_update(|| {
            let marker = self.load_replacement_marker(&scope)?;
            self.validate_replacement_marker(&scope, &marker)?;
            self.ensure_scope_observation_allows_progress(&scope, &marker)?;
            if !marker.preparation_complete
                || (scope.tracks_prior_material && marker.cleanup_phase != CLEANUP_PHASE_COMPLETE)
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
            validate_successful_outcome(outcome)?;
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
            if let Some(checkpoint) = checkpoint {
                validate_checkpoint_for_outcome(outcome, checkpoint)?;
            }
            self.ensure_active_provider_file_publication(&scope)?;

            self.with_provider_file_publication_writes(&scope, |_| {
                self.with_atomic_provider_file_update(|| {
                    self.ensure_provider_file_observation_is_current(
                        outcome.provider,
                        outcome.observation,
                    )?;
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
