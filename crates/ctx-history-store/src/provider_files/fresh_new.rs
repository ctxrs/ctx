const FRESH_NEW_DURABLE_ONLY_ERROR: &str =
    "fresh-new atomic eligibility changed; retry through durable replacement";

impl Store {
    /// Atomically publishes a group of previously unseen provider files.
    ///
    /// The caller parses before entering this boundary. `source_is_stable` is
    /// checked both before and after materialization so changed files cannot be
    /// published under an older inventory observation.
    #[doc(hidden)]
    pub fn commit_fresh_new_atomic_batch<T, E>(
        &mut self,
        outcomes: &[ProviderFileImportOutcome<'_>],
        checkpoints: &[ProviderFileCheckpoint],
        visible_external_session_ids: &[(CaptureProvider, String)],
        mut source_is_stable: impl FnMut() -> std::result::Result<bool, E>,
        write: impl FnOnce(&mut Store) -> std::result::Result<T, E>,
    ) -> std::result::Result<Option<T>, E>
    where
        E: From<StoreError>,
    {
        if outcomes.is_empty() || outcomes.len() != checkpoints.len() {
            return Err(E::from(StoreError::InvalidProviderFileCheckpoint(
                "fresh-new outcomes and checkpoints must be non-empty and aligned",
            )));
        }
        self.begin_immediate_batch().map_err(E::from)?;
        let eligibility = self.ensure_fresh_new_batch_eligible(
            outcomes,
            checkpoints,
            visible_external_session_ids,
        );
        match eligibility {
            Ok(true) => {}
            Ok(false) => {
                let _ = self.rollback_batch();
                self.route_fresh_new_batch_durable_only(outcomes)
                    .map_err(E::from)?;
                return Ok(None);
            }
            Err(error) => {
                let _ = self.rollback_batch();
                self.route_fresh_new_batch_durable_only(outcomes)
                    .map_err(E::from)?;
                return Err(E::from(error));
            }
        }
        match source_is_stable() {
            Ok(true) => {}
            Ok(false) => {
                let _ = self.rollback_batch();
                self.route_fresh_new_batch_durable_only(outcomes)
                    .map_err(E::from)?;
                return Ok(None);
            }
            Err(error) => {
                let _ = self.rollback_batch();
                self.route_fresh_new_batch_durable_only(outcomes)
                    .map_err(E::from)?;
                return Err(error);
            }
        }

        let value = match write(self) {
            Ok(value) => value,
            Err(error) => {
                let _ = self.rollback_batch();
                self.route_fresh_new_batch_durable_only(outcomes)
                    .map_err(E::from)?;
                return Err(error);
            }
        };
        match source_is_stable() {
            Ok(true) => {}
            Ok(false) => {
                let _ = self.rollback_batch();
                self.route_fresh_new_batch_durable_only(outcomes)
                    .map_err(E::from)?;
                return Ok(None);
            }
            Err(error) => {
                let _ = self.rollback_batch();
                self.route_fresh_new_batch_durable_only(outcomes)
                    .map_err(E::from)?;
                return Err(error);
            }
        }

        for (outcome, checkpoint) in outcomes.iter().copied().zip(checkpoints) {
            if let Err(error) = validate_checkpoint_for_outcome(outcome, checkpoint)
                .and_then(|()| {
                    self.record_matching_provider_file_outcome(
                        outcome,
                        ProviderFileCompletionKind::Replacement,
                        true,
                    )
                })
                .and_then(|()| self.replace_provider_file_checkpoint(outcome, Some(checkpoint)))
            {
                let _ = self.rollback_batch();
                self.route_fresh_new_batch_durable_only(outcomes)
                    .map_err(E::from)?;
                return Err(E::from(error));
            }
        }
        match self.commit_batch() {
            Ok(()) => Ok(Some(value)),
            Err(error) => {
                let _ = self.rollback_batch();
                self.route_fresh_new_batch_durable_only(outcomes)
                    .map_err(E::from)?;
                Err(E::from(error))
            }
        }
    }

    fn ensure_fresh_new_batch_eligible(
        &self,
        outcomes: &[ProviderFileImportOutcome<'_>],
        checkpoints: &[ProviderFileCheckpoint],
        visible_external_session_ids: &[(CaptureProvider, String)],
    ) -> Result<bool> {
        if self.has_pending_provider_file_publications()? {
            return Ok(false);
        }
        let mut paths = std::collections::HashSet::new();
        for (outcome, checkpoint) in outcomes.iter().copied().zip(checkpoints) {
            if !paths.insert((
                outcome.provider.as_str(),
                outcome.observation.source_root(),
                outcome.observation.source_path(),
            )) || outcome.status == CatalogIndexedStatus::Rejected
                || outcome.status == CatalogIndexedStatus::Failed
            {
                return Ok(false);
            }
            self.ensure_provider_file_observation_is_current(
                outcome.provider,
                outcome.observation,
            )?;
            if !self.fresh_new_pending_reason_is_current(outcome)?
                || self.provider_file_checkpoint(checkpoint.key())?.is_some()
                || self.fresh_new_path_has_prior_material(outcome)?
                || self.fresh_new_path_has_publication(outcome)?
            {
                return Ok(false);
            }
            validate_checkpoint_for_outcome(outcome, checkpoint)?;
        }

        let mut identities = std::collections::HashSet::new();
        for (provider, external_session_id) in visible_external_session_ids {
            if external_session_id.trim().is_empty()
                || !identities.insert((provider.as_str(), external_session_id.as_str()))
                || self.fresh_new_visible_identity_exists(*provider, external_session_id)?
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn fresh_new_pending_reason_is_current(
        &self,
        outcome: ProviderFileImportOutcome<'_>,
    ) -> Result<bool> {
        let observation = outcome.observation;
        let table = match observation {
            ProviderFileInventoryObservation::ObservedCatalog { .. } => "catalog_sessions",
            ProviderFileInventoryObservation::SourceImport { .. } => "source_import_files",
        };
        self.conn
            .query_row(
                &format!(
                    "SELECT EXISTS (SELECT 1 FROM {table} WHERE provider = ?1 \
                     AND source_root = ?2 AND source_path = ?3 AND is_stale = 0 \
                     AND pending_reason = 'fresh_new' AND indexed_status = 'pending')"
                ),
                params![
                    outcome.provider.as_str(),
                    observation.source_root(),
                    observation.source_path(),
                ],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    fn fresh_new_path_has_publication(
        &self,
        outcome: ProviderFileImportOutcome<'_>,
    ) -> Result<bool> {
        self.conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM provider_file_publications \
                 WHERE provider = ?1 AND inventory_source_root = ?2 AND source_path = ?3)",
                params![
                    outcome.provider.as_str(),
                    outcome.observation.source_root(),
                    outcome.observation.source_path(),
                ],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    fn fresh_new_path_has_prior_material(
        &self,
        outcome: ProviderFileImportOutcome<'_>,
    ) -> Result<bool> {
        let observation = outcome.observation;
        let material_format = ctx_history_core::canonical_provider_material_source_format(
            outcome.provider,
            observation.source_format(),
        )
        .ok_or(StoreError::InvalidProviderFileCheckpoint(
            "fresh-new source format has no canonical material format",
        ))?;
        let material_root_is_source_path = matches!(
            observation,
            ProviderFileInventoryObservation::SourceImport { .. }
        );
        self.conn
            .query_row(
                r#"
                SELECT EXISTS (
                    SELECT 1 FROM capture_sources AS source
                    WHERE source.provider = ?1 AND source.source_format = ?2
                      AND (source.raw_source_path = ?3 OR (?4 AND source.source_root = ?3))
                    UNION ALL
                    SELECT 1
                    FROM history_record_links AS link
                    JOIN capture_sources AS source ON source.id = link.source_id
                    WHERE source.provider = ?1 AND source.source_format = ?2
                      AND (source.raw_source_path = ?3 OR (?4 AND source.source_root = ?3))
                    LIMIT 1
                )
                "#,
                params![
                    outcome.provider.as_str(),
                    material_format,
                    observation.source_path(),
                    material_root_is_source_path,
                ],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    fn fresh_new_visible_identity_exists(
        &self,
        provider: CaptureProvider,
        external_session_id: &str,
    ) -> Result<bool> {
        self.conn
            .query_row(
                r#"
                SELECT EXISTS (
                    SELECT 1 FROM sessions
                    WHERE provider = ?1 AND external_session_id = ?2
                    UNION ALL
                    SELECT 1 FROM capture_sources
                    WHERE provider = ?1 AND external_session_id = ?2
                    LIMIT 1
                )
                "#,
                params![provider.as_str(), external_session_id],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    fn route_fresh_new_batch_durable_only(
        &self,
        outcomes: &[ProviderFileImportOutcome<'_>],
    ) -> Result<()> {
        crate::connection::with_immediate_transaction(&self.conn, || {
            for outcome in outcomes.iter().copied() {
                let changed = match outcome.observation {
                    ProviderFileInventoryObservation::ObservedCatalog {
                        mut update,
                        metadata,
                        ..
                    } => {
                        update.inventory_generation = self
                            .current_provider_file_inventory_generation(
                                outcome.provider,
                                update.source_root,
                                CATALOG_INVENTORY_FAMILY,
                            )?;
                        self.record_observed_catalog_source_import_result(
                            outcome.provider,
                            update,
                            metadata,
                            CatalogIndexedStatus::Failed,
                            Some(FRESH_NEW_DURABLE_ONLY_ERROR),
                        )?
                    }
                    ProviderFileInventoryObservation::SourceImport { mut update, .. } => {
                        update.inventory_generation = self
                            .current_provider_file_inventory_generation(
                                outcome.provider,
                                update.source_root,
                                SOURCE_IMPORT_INVENTORY_FAMILY,
                            )?;
                        self.record_source_import_file_result(
                            outcome.provider,
                            update,
                            CatalogIndexedStatus::Failed,
                            Some(FRESH_NEW_DURABLE_ONLY_ERROR),
                        )?
                    }
                };
                if changed != 1 {
                    return Err(provider_file_observation_changed(
                        outcome.provider,
                        outcome.observation,
                    ));
                }
            }
            Ok(())
        })
    }

    /// Persists the transition from optimistic FreshNew work to the durable
    /// replacement path without materializing any source content.
    #[doc(hidden)]
    pub fn defer_fresh_new_atomic_batch(
        &mut self,
        outcomes: &[ProviderFileImportOutcome<'_>],
    ) -> Result<()> {
        self.route_fresh_new_batch_durable_only(outcomes)
    }

    /// Atomically records deterministic FreshNew parse rejections. No content
    /// or append checkpoint is allowed for these terminal outcomes.
    #[doc(hidden)]
    pub fn reject_fresh_new_atomic_batch<E>(
        &mut self,
        outcomes: &[ProviderFileImportOutcome<'_>],
        mut source_is_stable: impl FnMut() -> std::result::Result<bool, E>,
    ) -> std::result::Result<bool, E>
    where
        E: From<StoreError>,
    {
        if outcomes.is_empty() {
            return Ok(true);
        }
        self.begin_immediate_batch().map_err(E::from)?;
        let result: std::result::Result<bool, E> = (|| {
            let mut paths = std::collections::HashSet::new();
            for outcome in outcomes.iter().copied() {
                if outcome.status != CatalogIndexedStatus::Rejected
                    || !paths.insert((
                        outcome.provider.as_str(),
                        outcome.observation.source_root(),
                        outcome.observation.source_path(),
                    ))
                {
                    return Err(E::from(StoreError::InvalidProviderFileCheckpoint(
                        "fresh-new rejection outcomes must be unique and rejected",
                    )));
                }
                self.ensure_provider_file_observation_is_current(
                    outcome.provider,
                    outcome.observation,
                )?;
                if !self.fresh_new_pending_reason_is_current(outcome)?
                    || self.fresh_new_path_has_prior_material(outcome)?
                    || self.fresh_new_path_has_publication(outcome)?
                {
                    return Ok(false);
                }
                self.record_matching_provider_file_outcome(
                    outcome,
                    ProviderFileCompletionKind::Replacement,
                    false,
                )?;
            }
            source_is_stable()
        })();
        match result {
            Ok(true) => {
                self.commit_batch().map_err(E::from)?;
                Ok(true)
            }
            Ok(false) => {
                self.rollback_batch().map_err(E::from)?;
                self.route_fresh_new_batch_durable_only(outcomes)
                    .map_err(E::from)?;
                Ok(false)
            }
            Err(error) => {
                let _ = self.rollback_batch();
                self.route_fresh_new_batch_durable_only(outcomes)
                    .map_err(E::from)?;
                Err(error)
            }
        }
    }
}
