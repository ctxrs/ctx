impl Store {
    pub fn begin_provider_file_publication(
        &self,
        provider: CaptureProvider,
        observation: ProviderFileInventoryObservation<'_>,
        material_source_format: &str,
        kind: ProviderFilePublicationKind,
        created_at_ms: i64,
    ) -> Result<ProviderFilePublicationScope> {
        self.begin_provider_file_publication_inner(
            provider,
            observation,
            material_source_format,
            kind,
            created_at_ms,
            || {},
        )
    }

    /// Adopts a mutated durable publication whose inventory observation is no
    /// longer live. Retirement never accepts importer writes: it only prepares
    /// and reconciles the remaining owner material before atomically removing
    /// the stale observation and publication marker.
    pub fn begin_provider_file_publication_retirement(
        &self,
        provider: CaptureProvider,
        material_source_format: &str,
        material_source_root: &str,
        source_path: &str,
        created_at_ms: i64,
    ) -> Result<Option<ProviderFilePublicationScope>> {
        if material_source_format.trim().is_empty()
            || material_source_root.trim().is_empty()
            || source_path.trim().is_empty()
        {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        self.cleanup_abandoned_provider_file_publication()?;
        if self.provider_file_publication.borrow().is_some() {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        let lifecycle = Arc::new(AtomicBool::new(true));
        let owner_id = opaque_provider_file_owner_id(
            provider,
            material_source_format,
            material_source_root,
            source_path,
        );
        let (owner_lock, owner_lock_path) = self.acquire_provider_file_owner_lock(
            provider,
            material_source_format,
            material_source_root,
            source_path,
        )?;
        let scope = self.with_atomic_provider_file_update(|| {
            let Some(publication) = self.load_durable_provider_file_publication(
                provider,
                material_source_format,
                material_source_root,
                source_path,
                &owner_id,
            )?
            else {
                return Ok(None);
            };
            if !publication.mutation_started {
                return Err(StoreError::InvalidProviderFilePublicationScope);
            }
            if self.provider_file_publication_has_current_observation(publication.scope_id)? {
                return Ok(None);
            }

            let mut scope = ProviderFilePublicationScope {
                scope_id: publication.scope_id,
                store_identity: self.store_identity.digest().to_owned(),
                provider,
                inventory_source_format: publication.inventory_source_format,
                inventory_source_root: publication.inventory_source_root,
                source_path: publication.source_path,
                material_source_format: material_source_format.to_owned(),
                material_source_root: material_source_root.to_owned(),
                inventory_family: publication.inventory_family,
                inventory_generation: publication.inventory_generation,
                file_size_bytes: publication.file_size_bytes,
                file_modified_at_ms: publication.file_modified_at_ms,
                import_revision: publication.import_revision,
                metadata_json: publication.metadata_json,
                kind: publication.publication_kind,
                owner_id,
                staging_id: publication.staging_id,
                tracks_prior_material: publication.tracks_prior_material,
                reuse_staging_state: publication.staging_initialized,
                retires_observation: true,
                lifecycle: Arc::clone(&lifecycle),
                _owner_lock: owner_lock,
                _owner_lock_path: owner_lock_path.clone(),
            };
            self.publish_provider_file_publication_marker(&mut scope, created_at_ms)?;
            self.initialize_provider_file_publication_retirement(&mut scope)?;
            invalidate_semantic_searchable_item_stats(&self.conn)?;
            Ok(Some(scope))
        })?;
        let Some(scope) = scope else {
            return Ok(None);
        };

        self.provider_file_publication
            .replace(Some(ActiveProviderFilePublication {
                scope_id: scope.scope_id,
                owner_id: scope.owner_id.clone(),
                lifecycle: Arc::clone(&lifecycle),
                provider,
                material_source_format: material_source_format.to_owned(),
                material_source_root: material_source_root.to_owned(),
                source_path: source_path.to_owned(),
                retires_observation: true,
                _owner_lock_path: owner_lock_path,
                attached: false,
            }));
        if let Err(error) = self.reclaim_orphaned_provider_staging(&scope) {
            lifecycle.store(false, Ordering::Release);
            let _ = self.cleanup_active_provider_file_publication(scope.scope_id);
            return Err(error);
        }
        if scope.kind == ProviderFilePublicationKind::Replacement && scope.tracks_prior_material {
            if let Err(error) = self.attach_provider_file_publication_staging(&scope) {
                lifecycle.store(false, Ordering::Release);
                let _ = self.cleanup_active_provider_file_publication(scope.scope_id);
                return Err(error);
            }
        }
        Ok(Some(scope))
    }

    fn begin_provider_file_publication_inner(
        &self,
        provider: CaptureProvider,
        observation: ProviderFileInventoryObservation<'_>,
        material_source_format: &str,
        kind: ProviderFilePublicationKind,
        created_at_ms: i64,
        before_writer_transaction: impl FnOnce(),
    ) -> Result<ProviderFilePublicationScope> {
        validate_observation_identity(observation)?;
        if material_source_format.trim().is_empty() {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        let material_source_root = match observation {
            ProviderFileInventoryObservation::ObservedCatalog { .. } => observation.source_root(),
            ProviderFileInventoryObservation::SourceImport { .. } => observation.source_path(),
        }
        .to_owned();
        let material_source_format = material_source_format.to_owned();
        self.cleanup_abandoned_provider_file_publication()?;
        if self.provider_file_publication.borrow().is_some() {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        let lifecycle = Arc::new(AtomicBool::new(true));
        let scope_id = Uuid::new_v4();
        let owner_id = opaque_provider_file_owner_id(
            provider,
            &material_source_format,
            &material_source_root,
            observation.source_path(),
        );
        let staging_id =
            provider_file_staging_name(self.store_identity.digest(), &owner_id, scope_id);
        let (owner_lock, owner_lock_path) = self.acquire_provider_file_owner_lock(
            provider,
            &material_source_format,
            &material_source_root,
            observation.source_path(),
        )?;
        before_writer_transaction();
        let mut scope = ProviderFilePublicationScope {
            scope_id,
            store_identity: self.store_identity.digest().to_owned(),
            provider,
            inventory_source_format: observation.source_format().to_owned(),
            inventory_source_root: observation.source_root().to_owned(),
            source_path: observation.source_path().to_owned(),
            material_source_format: material_source_format.clone(),
            material_source_root: material_source_root.clone(),
            inventory_family: observation.inventory_family(),
            inventory_generation: observation.inventory_generation(),
            file_size_bytes: observation.file_size_bytes(),
            file_modified_at_ms: observation.file_modified_at_ms(),
            import_revision: observation.import_revision(),
            metadata_json: observation.metadata_json()?,
            kind,
            owner_id,
            staging_id,
            tracks_prior_material: false,
            reuse_staging_state: false,
            retires_observation: false,
            lifecycle: Arc::clone(&lifecycle),
            _owner_lock: owner_lock,
            _owner_lock_path: owner_lock_path.clone(),
        };
        self.with_atomic_provider_file_update(|| {
            if let Err(error) =
                self.ensure_provider_file_observation_is_current(provider, observation)
            {
                if !self.provider_file_publication_matches_candidate(
                    provider,
                    observation,
                    &material_source_format,
                    &material_source_root,
                )? {
                    return Err(error);
                }
            }
            scope.tracks_prior_material = self.provider_file_owner_has_prior_material(
                provider,
                &material_source_format,
                &material_source_root,
                observation.source_path(),
            )?;
            self.publish_provider_file_publication_marker(&mut scope, created_at_ms)?;
            invalidate_semantic_searchable_item_stats(&self.conn)
        })?;
        self.provider_file_publication
            .replace(Some(ActiveProviderFilePublication {
                scope_id: scope.scope_id,
                owner_id: scope.owner_id.clone(),
                lifecycle: Arc::clone(&lifecycle),
                provider,
                material_source_format,
                material_source_root,
                source_path: observation.source_path().to_owned(),
                retires_observation: false,
                _owner_lock_path: owner_lock_path,
                attached: false,
            }));
        if let Err(error) = self.reclaim_orphaned_provider_staging(&scope) {
            lifecycle.store(false, Ordering::Release);
            let _ = self.cleanup_active_provider_file_publication(scope.scope_id);
            return Err(error);
        }
        // Replacement staging also records material written by a first import.
        // That durable seen-set is required if the process dies after mutation
        // and the source observation later disappears or is revived.
        if scope.kind == ProviderFilePublicationKind::Replacement {
            if let Err(error) = self.attach_provider_file_publication_staging(&scope) {
                lifecycle.store(false, Ordering::Release);
                let _ = self.cleanup_active_provider_file_publication(scope.scope_id);
                return Err(error);
            }
            if self.take_provider_file_fault(ProviderFileFaultPoint::BeginAfterStaging) {
                lifecycle.store(false, Ordering::Release);
                let _ = self.cleanup_active_provider_file_publication(scope.scope_id);
                return Err(StoreError::ProviderFileStaging);
            }
        }
        Ok(scope)
    }

    pub fn provider_file_publication_phase(
        &self,
        scope: &ProviderFilePublicationScope,
    ) -> Result<ProviderFilePublicationPhase> {
        self.ensure_active_provider_file_publication(scope)?;
        let marker = self.load_replacement_marker(scope)?;
        self.ensure_scope_observation_allows_progress(scope, &marker)?;
        Ok(derive_provider_file_publication_phase(scope, &marker))
    }

    pub fn stage_provider_file_publication_completion(
        &self,
        scope: &ProviderFilePublicationScope,
        completion: &ProviderFilePublicationCompletion,
    ) -> Result<()> {
        self.ensure_active_provider_file_publication(scope)?;
        if scope.retires_observation || self.provider_file_write_scope.get().is_some() {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        let payload_json = serialize_provider_file_publication_completion(completion)?;
        self.with_atomic_provider_file_update(|| {
            let marker = self.load_replacement_marker(scope)?;
            self.ensure_scope_observation_allows_progress(scope, &marker)?;
            if scope.tracks_prior_material && !marker.preparation_complete {
                return Err(StoreError::ProviderFileReconciliationIncomplete);
            }
            match marker.completion_payload_json.as_deref() {
                Some(existing) if existing == payload_json.as_str() => return Ok(()),
                Some(_) => return Err(StoreError::InvalidProviderFilePublicationScope),
                None => {}
            }
            let changed = self.conn.execute(
                r#"
                UPDATE main.provider_file_publications
                SET completion_payload_json = ?2
                WHERE replacement_id = ?1 AND completion_payload_json IS NULL
                  AND (preparation_complete = 1 OR ?3 = 0)
                "#,
                params![
                    scope.scope_id.to_string(),
                    &payload_json,
                    scope.tracks_prior_material
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::InvalidProviderFilePublicationScope);
            }
            if self.take_provider_file_fault(ProviderFileFaultPoint::CompletionBeforeCommit) {
                return Err(StoreError::ProviderFileStaging);
            }
            Ok(())
        })
    }

    pub fn load_provider_file_publication_completion(
        &self,
        scope: &ProviderFilePublicationScope,
    ) -> Result<Option<ProviderFilePublicationCompletion>> {
        self.ensure_active_provider_file_publication(scope)?;
        if scope.retires_observation {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        let marker = self.load_replacement_marker(scope)?;
        self.ensure_scope_observation_allows_progress(scope, &marker)?;
        marker
            .completion_payload_json
            .as_deref()
            .map(parse_provider_file_publication_completion)
            .transpose()
    }
}
