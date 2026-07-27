use super::*;

impl NativePathPublicationGroup<'_> {
    /// Durably stages one bounded page of the canonical entities retained by
    /// the current provider-owned source generation.
    pub fn stage_source_generation_page(
        &mut self,
        key: &NativePathSourceGenerationKey,
        retained: &NativePathRetainedSourceEntities,
    ) -> Result<()> {
        self.ensure_mutable()?;
        let encoded_bytes = key.bound_value_bytes().and_then(|key_bytes| {
            key_bytes
                .checked_add(retained.bound_value_bytes())
                .ok_or(StoreError::NativePathSourceGenerationConflict)
        });
        let encoded_bytes = match encoded_bytes {
            Ok(value) => value,
            Err(error) => return self.poison_with(error),
        };
        self.charge_core_mutations(retained.len().saturating_add(1), encoded_bytes)?;
        match self.with_write_scope(|store| store.stage_source_generation_page_tx(key, retained)) {
            Ok(()) => Ok(()),
            Err(error) => self.poison_with(error),
        }
    }

    /// Previews the exact stable retirement page before cursor
    /// classification. This is a read-only operation in the group's existing
    /// `BEGIN IMMEDIATE` transaction; a newly published cursor cannot commit
    /// unless the matching retirement page is subsequently applied.
    pub fn preview_source_generation_retirement_page(
        &mut self,
        key: &NativePathSourceGenerationKey,
        after: Option<&NativePathSourceEntityFrontier>,
        limit: usize,
    ) -> Result<NativePathSourceRetirementPage> {
        self.ensure_open()?;
        if self.is_poisoned()
            || self.attempted_mutation_units != 0
            || self.journal_prepared
            || !matches!(self.cursor_state, CursorPublicationState::None)
            || limit == 0
            || limit > (NATIVE_PATH_MAX_MUTATION_UNITS.saturating_sub(2) / 2)
        {
            return self.poison_with(StoreError::NativePathSourceGenerationConflict);
        }
        if let Some(preview) = &self.source_retirement_preview {
            if preview.key == *key && preview.after.as_ref() == after && preview.limit == limit {
                return Ok(preview.page.clone());
            }
            return self.poison_with(StoreError::NativePathSourceGenerationConflict);
        }
        let page = match self
            .store
            .preview_source_generation_retirement_page_tx(key, after, limit)
        {
            Ok(value) => value,
            Err(error) => return self.poison_with(error),
        };
        self.source_retirement_preview = Some(NativePathSourceRetirementPreview {
            key: key.clone(),
            after: after.cloned(),
            limit,
            page: page.clone(),
        });
        Ok(page)
    }

    /// Retires one bounded, stable page of canonical rows omitted from the
    /// provider-owned current generation. Capture sources and routes remain;
    /// only the omitted canonical entities are soft-deleted.
    pub fn retire_source_generation_page(
        &mut self,
        key: &NativePathSourceGenerationKey,
        after: Option<&NativePathSourceEntityFrontier>,
        limit: usize,
        retired_at_ms: i64,
    ) -> Result<NativePathSourceRetirementPage> {
        self.ensure_mutable()?;
        if limit == 0 || limit > (NATIVE_PATH_MAX_MUTATION_UNITS.saturating_sub(2) / 2) {
            return self.poison_with(StoreError::NativePathSourceGenerationConflict);
        }
        if let Some(preview) = &self.source_retirement_preview {
            if preview.key != *key || preview.after.as_ref() != after || preview.limit != limit {
                return self.poison_with(StoreError::NativePathSourceGenerationConflict);
            }
        }
        let retired_at = match ms_to_time(retired_at_ms) {
            Ok(value) => value,
            Err(error) => return self.poison_with(StoreError::Sql(error)),
        };
        let preparation = match self.with_write_scope(|store| {
            store.prepare_source_generation_retirement_page_tx(key, after, limit)
        }) {
            Ok(value) => value,
            Err(error) => return self.poison_with(error),
        };
        let (candidates, next_after, done) = match preparation {
            NativePathSourceRetirementPreparation::Replay(page) => {
                self.consume_source_retirement_preview(&page)?;
                return Ok(page);
            }
            NativePathSourceRetirementPreparation::Work {
                candidates,
                next_after,
                done,
            } => (candidates, next_after, done),
        };

        let frontier_bytes = std::mem::size_of::<Uuid>()
            .saturating_add(
                after
                    .map(|value| value.kind.as_str().len())
                    .unwrap_or_default(),
            )
            .saturating_add(
                next_after
                    .as_ref()
                    .map(|value| value.kind.as_str().len())
                    .unwrap_or_default(),
            );
        let key_bytes = match key.bound_value_bytes() {
            Ok(value) => value,
            Err(error) => return self.poison_with(error),
        };
        self.charge_core_mutations(
            candidates.len().saturating_add(1),
            key_bytes.saturating_add(frontier_bytes),
        )?;

        let mut retired = 0_usize;
        for candidate in &candidates {
            if candidate.retained {
                continue;
            }
            match candidate.kind {
                NativePathSourceEntityKind::SessionEdge => {
                    let mut edge = match self.store.get_session_edge(candidate.id) {
                        Ok(value) => value,
                        Err(error) => return self.poison_with(error),
                    };
                    let expected_actor =
                        match canonical_actor_by_id(&self.store.conn, edge.from_session_id) {
                            Ok(Some(value)) => value,
                            Ok(None) => {
                                return self
                                    .poison_with(StoreError::NotFound(edge.from_session_id));
                            }
                            Err(error) => return self.poison_with(StoreError::Sql(error)),
                        };
                    edge.timestamps.updated_at = retired_at;
                    edge.sync.deleted_at = Some(retired_at);
                    self.upsert_projection_neutral_session_edge(&expected_actor, &edge)?;
                }
                NativePathSourceEntityKind::Run => {
                    let mut run = match self.store.get_run(candidate.id) {
                        Ok(value) => value,
                        Err(error) => return self.poison_with(error),
                    };
                    run.timestamps.updated_at = retired_at;
                    run.sync.deleted_at = Some(retired_at);
                    self.upsert_run(&run)?;
                }
                NativePathSourceEntityKind::Event => {
                    let mut event = match self.store.get_event(candidate.id) {
                        Ok(value) => value,
                        Err(error) => return self.poison_with(error),
                    };
                    event.sync.deleted_at = Some(retired_at);
                    self.upsert_event_exact(&event)?;
                }
                NativePathSourceEntityKind::FileTouch => {
                    let mut file = match self.store.get_file_touched(candidate.id) {
                        Ok(value) => value,
                        Err(error) => return self.poison_with(error),
                    };
                    file.timestamps.updated_at = retired_at;
                    file.sync.deleted_at = Some(retired_at);
                    self.upsert_file_touched(&file)?;
                }
                NativePathSourceEntityKind::Session => {
                    let mut session = match self.store.get_session(candidate.id) {
                        Ok(value) => value,
                        Err(error) => return self.poison_with(error),
                    };
                    session.timestamps.updated_at = retired_at;
                    session.sync.deleted_at = Some(retired_at);
                    self.upsert_session(&session)?;
                }
            }
            retired = retired.saturating_add(1);
        }

        let page = NativePathSourceRetirementPage {
            next_after,
            done,
            inspected: candidates.len(),
            retired,
        };
        self.consume_source_retirement_preview(&page)?;
        match self.with_write_scope(|store| {
            store.finish_source_generation_retirement_page_tx(key, after, &page)
        }) {
            Ok(()) => Ok(page),
            Err(error) => self.poison_with(error),
        }
    }

    fn consume_source_retirement_preview(
        &mut self,
        page: &NativePathSourceRetirementPage,
    ) -> Result<()> {
        let Some(preview) = self.source_retirement_preview.take() else {
            return Ok(());
        };
        if preview.page != *page {
            return self.poison_with(StoreError::NativePathSourceGenerationConflict);
        }
        Ok(())
    }

    fn upsert_event_exact(&mut self, event: &Event) -> Result<()> {
        self.ensure_mutable()?;
        self.charge_core_mutations(1, 0)?;
        let encoded_bytes = match self
            .with_write_scope(|store| store.upsert_event_with_native_path_accounting(event))
        {
            Ok(value) => value,
            Err(error) => return self.poison_with(error),
        };
        self.charge_core_mutations(0, encoded_bytes)
    }
}
