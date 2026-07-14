impl Store {
    pub fn finalize_provider_file_publication(
        &self,
        scope: ProviderFilePublicationScope,
        outcome: ProviderFileImportOutcome<'_>,
        commit: ProviderFilePublicationCommit<'_>,
    ) -> Result<ProviderFileFinalizeOutcome> {
        self.validate_provider_file_publication_scope(&scope)?;
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

        let durable_result = self.with_provider_file_publication_writes(&scope, |_| {
            self.with_atomic_provider_file_update(|| {
                self.ensure_provider_file_observation_is_current(
                    outcome.provider,
                    outcome.observation,
                )?;
                let counts = if completion_kind == ProviderFileCompletionKind::Replacement {
                    let marker = self.load_replacement_marker(&scope)?;
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
                if self.take_provider_file_fault(ProviderFileFaultPoint::FinalizeBeforeCommit) {
                    return Err(StoreError::ProviderFileStaging);
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
                            checkpoint.ok_or(StoreError::InvalidProviderFilePublicationScope)?,
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
                Ok(counts)
            })
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
}
