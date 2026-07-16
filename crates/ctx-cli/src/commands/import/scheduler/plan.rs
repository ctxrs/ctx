impl ImportPlan {
    pub(crate) fn build(store: &Store, sources: Vec<PlannedImportSource>) -> Result<Self> {
        let (fresh_units, recovery_units) = import_work_counts(store, &sources)?;
        Ok(Self {
            sources,
            fresh_units: execution_budget(fresh_units),
            recovery_units: execution_budget(recovery_units),
        })
    }

    #[cfg(test)]
    pub(crate) fn select_slice(
        &self,
        store: &Store,
        class: ImportWorkClass,
        max_units: usize,
    ) -> Result<ImportSlice> {
        let state = ImportExecutionState::for_plan(self);
        self.select_slice_with_state(store, class, max_units, &state, None)
    }

    pub(crate) fn select_slice_for_execution_with_pre_lock_hook(
        &self,
        store: &Store,
        class: ImportWorkClass,
        max_units: usize,
        state: &mut ImportExecutionState,
        before_bulk_lock: impl FnOnce(),
    ) -> Result<Option<ExecutableImportSlice>> {
        let provisional = self
            .select_slice_with_state(store, class, max_units, state, None)
            .context("select provisional import slice")?;
        if provisional.is_empty() {
            return Ok(None);
        }
        before_bulk_lock();
        let validation_failures = self
            .observe_provisional_sources(store, state, &provisional)
            .context("observe import slice before bulk lock")?;
        let bulk_guard = store
            .begin_event_search_bulk_mode()
            .context("begin import search bulk mode")?;
        match self
            .revalidate_slice(
                store,
                class,
                max_units,
                state,
                &provisional,
                validation_failures,
            )
            .context("revalidate locked import slice")
        {
            Ok((slice, validation_failures)) => Ok(Some(ExecutableImportSlice {
                slice,
                bulk_guard,
                validation_failures,
            })),
            Err(error) => {
                if let Err(finish_error) = store.finish_event_search_bulk_mode(&bulk_guard) {
                    return Err(finish_error.into());
                }
                Err(error)
            }
        }
    }

    pub(crate) fn pending_counts(&self, store: &Store) -> Result<(usize, usize)> {
        import_work_counts(store, &self.sources)
    }

    pub(crate) fn pending_count(&self, store: &Store, class: ImportWorkClass) -> Result<usize> {
        Ok(execution_budget(import_work_count(
            store,
            &self.sources,
            class,
        )?))
    }

    fn select_slice_with_state(
        &self,
        store: &Store,
        class: ImportWorkClass,
        max_units: usize,
        state: &ImportExecutionState,
        eligible_sources: Option<&BTreeSet<usize>>,
    ) -> Result<ImportSlice> {
        let ordinary_slice_limit = IMPORT_SLICE_MAX_UNITS.min(max_units);
        if ordinary_slice_limit == 0 {
            return Ok(ImportSlice::empty());
        }
        if !store.event_search_bulk_admission_outcome()?.is_complete() {
            return Ok(ImportSlice::empty());
        }

        let mut candidates = Vec::new();
        let fetch_limit = ordinary_slice_limit;
        if class == ImportWorkClass::Recovery {
            for work in store.list_provider_file_publication_retirement_work(fetch_limit)? {
                candidates.push(ImportCandidate::Retirement(work));
                if candidates.len() >= ordinary_slice_limit {
                    break;
                }
            }
        }
        for (source_index, plan) in self.sources.iter().enumerate() {
            if eligible_sources.is_some_and(|eligible| !eligible.contains(&source_index)) {
                continue;
            }
            let preinventory = state
                .observed_preinventories
                .get(source_index)
                .and_then(Option::as_ref)
                .unwrap_or(&plan.preinventory);
            let Some(work) = list_source_work(store, plan, preinventory, class, fetch_limit)?
            else {
                continue;
            };
            match work {
                SelectedImportWork::Catalog(work) => {
                    candidates.extend(
                        work.into_iter()
                            .map(|work| ImportCandidate::Catalog { source_index, work })
                            .take(ordinary_slice_limit),
                    );
                }
                SelectedImportWork::SourceFiles(work) => {
                    candidates.extend(
                        work.into_iter()
                            .map(|work| ImportCandidate::SourceFile { source_index, work })
                            .take(ordinary_slice_limit),
                    );
                }
            }
        }
        candidates.sort_by(|left, right| self.compare_candidates(left, right));
        if store.has_pending_provider_file_publications()? {
            if !candidates
                .first()
                .is_some_and(ImportCandidate::has_active_publication)
            {
                return Ok(ImportSlice::empty());
            }
            if state.has_attempted(&candidates[0].identity()) {
                return Ok(ImportSlice::empty());
            }
            candidates.truncate(1);
        } else {
            candidates.retain(|candidate| !state.has_attempted(&candidate.identity()));
        }

        let fresh_new_group = candidates
            .first()
            .and_then(ImportCandidate::fresh_new_group_key);
        if let Some(group) = fresh_new_group {
            let source_index = match group {
                FreshNewGroupKey::CodexCatalog(source_index)
                | FreshNewGroupKey::PiSourceFiles(source_index) => source_index,
            };
            let plan = &self.sources[source_index];
            let preinventory = state
                .observed_preinventories
                .get(source_index)
                .and_then(Option::as_ref)
                .unwrap_or(&plan.preinventory);
            let expanded =
                list_source_work(store, plan, preinventory, class, FRESH_NEW_BATCH_MAX_PATHS)?;
            candidates = match expanded {
                Some(SelectedImportWork::Catalog(work)) => work
                    .into_iter()
                    .map(|work| ImportCandidate::Catalog { source_index, work })
                    .filter(|candidate| candidate.fresh_new_group_key() == Some(group))
                    .collect(),
                Some(SelectedImportWork::SourceFiles(work)) => work
                    .into_iter()
                    .map(|work| ImportCandidate::SourceFile { source_index, work })
                    .filter(|candidate| candidate.fresh_new_group_key() == Some(group))
                    .collect(),
                None => Vec::new(),
            };
            candidates.sort_by(|left, right| self.compare_candidates(left, right));
            candidates.retain(|candidate| !state.has_attempted(&candidate.identity()));
        } else {
            // FreshNew work is admitted only when it leads an otherwise ordered
            // slice. This keeps ordinary changed/recovery work ahead of it and
            // prevents multiple atomic groups from entering one scheduler slice.
            candidates.retain(|candidate| candidate.fresh_new_group_key().is_none());
        }

        let mut slice = ImportSlice::empty();
        let slice_limit = if fresh_new_group.is_some() {
            FRESH_NEW_BATCH_MAX_PATHS
        } else {
            ordinary_slice_limit
        };
        for candidate in candidates {
            if slice.units >= slice_limit {
                break;
            }
            let bytes = candidate.estimated_bytes();
            let byte_limit = if fresh_new_group.is_some() {
                FRESH_NEW_BATCH_MAX_BYTES
            } else {
                IMPORT_SLICE_TARGET_BYTES
            };
            let exceeds_target = slice.units > 0
                && if fresh_new_group.is_some() {
                    slice.bytes.saturating_add(bytes) >= byte_limit
                } else {
                    slice.bytes.saturating_add(bytes) > byte_limit
                };
            if exceeds_target {
                break;
            }
            slice.units += 1;
            slice.bytes = slice.bytes.saturating_add(bytes);
            match candidate {
                ImportCandidate::Retirement(work) => slice.retirements.push(work),
                ImportCandidate::Catalog { source_index, work } => push_source_candidate(
                    &mut slice,
                    source_index,
                    self.selected_preinventory(state, source_index),
                    SelectedCandidate::Catalog(work),
                ),
                ImportCandidate::SourceFile { source_index, work } => push_source_candidate(
                    &mut slice,
                    source_index,
                    self.selected_preinventory(state, source_index),
                    SelectedCandidate::SourceFile(work),
                ),
            }
        }
        Ok(slice)
    }

    fn selected_preinventory(
        &self,
        state: &ImportExecutionState,
        source_index: usize,
    ) -> SourcePreinventory {
        state
            .observed_preinventories
            .get(source_index)
            .and_then(Option::as_ref)
            .unwrap_or(&self.sources[source_index].preinventory)
            .clone()
    }

    fn compare_candidates(&self, left: &ImportCandidate, right: &ImportCandidate) -> Ordering {
        let publication_order = right
            .has_active_publication()
            .cmp(&left.has_active_publication());
        if publication_order != Ordering::Equal {
            return publication_order;
        }
        let attempt_order = left
            .last_attempt_at_ms()
            .is_some()
            .cmp(&right.last_attempt_at_ms().is_some())
            .then_with(|| left.last_attempt_at_ms().cmp(&right.last_attempt_at_ms()));
        if attempt_order != Ordering::Equal {
            return attempt_order;
        }
        left.stable_identity(&self.sources)
            .cmp(&right.stable_identity(&self.sources))
    }

    fn revalidate_slice(
        &self,
        store: &Store,
        class: ImportWorkClass,
        max_units: usize,
        state: &mut ImportExecutionState,
        provisional: &ImportSlice,
        mut validation_failures: Vec<SourceValidationFailure>,
    ) -> Result<(ImportSlice, Vec<SourceValidationFailure>)> {
        let mut eligible_sources = BTreeSet::new();
        for selected in &provisional.sources {
            if self.selection_needs_prelock_observation(selected)
                && state.observed_preinventories[selected.source_index].is_none()
            {
                continue;
            }
            eligible_sources.insert(selected.source_index);
        }
        validation_failures.extend(self.refresh_changed_manifest_units(
            store,
            provisional,
            state,
        )?);
        let mut slice =
            self.select_slice_with_state(store, class, max_units, state, Some(&eligible_sources))?;
        validation_failures
            .extend(self.retain_current_file_observations(store, &mut slice, state)?);
        retain_current_generations(store, &mut slice, state)?;
        Ok((slice, validation_failures))
    }

    fn observe_provisional_sources(
        &self,
        store: &Store,
        state: &mut ImportExecutionState,
        provisional: &ImportSlice,
    ) -> Result<Vec<SourceValidationFailure>> {
        let mut failures = Vec::new();
        for selected in &provisional.sources {
            if !self.selection_needs_prelock_observation(selected)
                || state.observed_preinventories[selected.source_index].is_some()
            {
                continue;
            }
            let plan = &self.sources[selected.source_index];
            match observe_current_preinventory(store, plan, selected) {
                Ok(preinventory) => {
                    state.observed_preinventories[selected.source_index] = Some(preinventory);
                }
                Err(error)
                    if import_error_scope(&error) == ImportFailureScope::Source
                        && import_failure_type(&error) == ImportFailureType::NotFound
                        && matches!(
                            &selected.work,
                            SelectedImportWork::SourceFiles(work)
                                if work.iter().any(|candidate| candidate.has_active_publication)
                        ) =>
                {
                    let inventory_generation = selected
                        .preinventory
                        .inventory_generation()
                        .unwrap_or_default();
                    if let SelectedImportWork::SourceFiles(work) = &selected.work {
                        for candidate in work
                            .iter()
                            .filter(|candidate| candidate.has_active_publication)
                        {
                            invalidate_source_file_publication_observation(
                                store,
                                candidate,
                                inventory_generation,
                            )?;
                            state.mark_validation_skip(source_file_work_identity(candidate));
                        }
                    }
                }
                Err(error) if import_error_scope(&error) == ImportFailureScope::Source => {
                    state.record_source_attempt(&selected.work);
                    failures.push(SourceValidationFailure {
                        source_index: selected.source_index,
                        stats: selected.stats,
                        error,
                    });
                }
                Err(error) => return Err(error),
            }
        }
        Ok(failures)
    }

    fn selection_needs_prelock_observation(&self, selected: &SelectedImportSource) -> bool {
        let SelectedImportWork::SourceFiles(work) = &selected.work else {
            return false;
        };
        !source_uses_import_file_manifest(&self.sources[selected.source_index].source)
            || work
                .iter()
                .any(|candidate| candidate.has_active_publication)
    }

    fn refresh_changed_manifest_units(
        &self,
        store: &Store,
        provisional: &ImportSlice,
        state: &mut ImportExecutionState,
    ) -> Result<Vec<SourceValidationFailure>> {
        let mut failures = Vec::new();
        for selected in &provisional.sources {
            let SourcePreinventory::SourceImportFiles {
                inventory_generation,
                ..
            } = &selected.preinventory
            else {
                continue;
            };
            let SelectedImportWork::SourceFiles(work) = &selected.work else {
                continue;
            };
            let source = &self.sources[selected.source_index].source;
            for candidate in work {
                let (current, current_generation, persist_current) =
                    if let Some(SourcePreinventory::SourceImportFiles {
                        files,
                        inventory_generation,
                    }) = state
                        .observed_preinventories
                        .get(selected.source_index)
                        .and_then(Option::as_ref)
                    {
                        let current = files
                            .iter()
                            .find(|file| file.source_path == candidate.file.source_path)
                            .cloned();
                        (
                            Ok(current),
                            *inventory_generation,
                            *inventory_generation
                                == selected.preinventory.inventory_generation().unwrap_or(0),
                        )
                    } else if source_uses_import_file_manifest(source) {
                        (
                            observe_selected_source_import_file(
                                source,
                                &candidate.file.source_path,
                            ),
                            *inventory_generation,
                            true,
                        )
                    } else {
                        let Some(SourcePreinventory::SourceRoot {
                            file,
                            inventory_generation,
                        }) = state
                            .observed_preinventories
                            .get(selected.source_index)
                            .and_then(Option::as_ref)
                        else {
                            continue;
                        };
                        (Ok(Some(file.clone())), *inventory_generation, false)
                    };
                match current {
                    Ok(Some(current)) => {
                        let compatible_append = active_append_observation_is_compatible(
                            store, source, candidate, &current,
                        )?;
                        if !compatible_append
                            && !same_source_import_observation(&candidate.file, &current)
                        {
                            let invalidated_publication =
                                invalidate_source_file_publication_observation(
                                    store,
                                    candidate,
                                    *inventory_generation,
                                )?;
                            if persist_current {
                                store.upsert_source_import_files(
                                    current_generation,
                                    std::slice::from_ref(&current),
                                )?;
                            }
                            if invalidated_publication {
                                state.mark_validation_skip(source_file_work_identity(candidate));
                            }
                        }
                    }
                    Ok(None) => {
                        invalidate_source_file_publication_observation(
                            store,
                            candidate,
                            *inventory_generation,
                        )?;
                        state.mark_validation_skip(source_file_work_identity(candidate));
                        if !candidate.has_active_publication {
                            failures.push(SourceValidationFailure {
                                source_index: selected.source_index,
                                stats: SourceStats {
                                    files: 1,
                                    bytes: candidate.estimated_bytes,
                                    change_token: None,
                                },
                                error: anyhow::Error::new(std::io::Error::new(
                                    std::io::ErrorKind::NotFound,
                                    format!(
                                        "import unit disappeared before indexing: {}",
                                        candidate.file.source_path
                                    ),
                                )),
                            });
                        }
                    }
                    Err(error) if import_error_scope(&error) == ImportFailureScope::Source => {
                        state.mark_validation_skip(source_file_work_identity(candidate));
                        failures.push(SourceValidationFailure {
                            source_index: selected.source_index,
                            stats: SourceStats {
                                files: 1,
                                bytes: candidate.estimated_bytes,
                                change_token: None,
                            },
                            error,
                        });
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(failures)
    }

    fn retain_current_file_observations(
        &self,
        store: &Store,
        slice: &mut ImportSlice,
        state: &mut ImportExecutionState,
    ) -> Result<Vec<SourceValidationFailure>> {
        let mut failures = Vec::new();
        for selected in &mut slice.sources {
            let source = &self.sources[selected.source_index].source;
            match &mut selected.work {
                SelectedImportWork::Catalog(work) => {
                    let mut retained = Vec::with_capacity(work.len());
                    for candidate in std::mem::take(work) {
                        let allow_append = candidate.has_active_publication
                            && source.mutation_contract
                                == ctx_history_capture::ProviderFileMutationContract::AppendOnlyNewlineDelimited
                            && store.effective_provider_file_publication_has_staged_completion()?;
                        if file_observation_is_current_or_compatible_append(
                            Path::new(&candidate.session.source_path),
                            candidate.session.file_size_bytes,
                            candidate.session.file_modified_at_ms,
                            &candidate.session.metadata,
                            allow_append,
                        ) {
                            retained.push(candidate);
                        } else {
                            invalidate_catalog_publication_observation(
                                store,
                                &candidate,
                                selected.preinventory.inventory_generation().unwrap_or(0),
                            )?;
                            state.mark_validation_skip(catalog_work_identity(&candidate));
                        }
                    }
                    *work = retained;
                }
                SelectedImportWork::SourceFiles(_)
                    if matches!(
                        &selected.preinventory,
                        SourcePreinventory::SourceRoot { .. }
                    ) || !source_uses_import_file_manifest(source) => {}
                SelectedImportWork::SourceFiles(work) => {
                    let mut retained = Vec::with_capacity(work.len());
                    for candidate in std::mem::take(work) {
                        match observe_selected_source_import_file(
                            source,
                            &candidate.file.source_path,
                        ) {
                            Ok(Some(current)) => {
                                if same_source_import_observation(&candidate.file, &current)
                                    || active_append_observation_is_compatible(
                                        store, source, &candidate, &current,
                                    )?
                                {
                                    retained.push(candidate);
                                } else {
                                    state.mark_validation_skip(source_file_work_identity(
                                        &candidate,
                                    ));
                                }
                            }
                            Ok(None) => {
                                state.mark_validation_skip(source_file_work_identity(&candidate));
                            }
                            Err(error)
                                if import_error_scope(&error) == ImportFailureScope::Source =>
                            {
                                state.mark_validation_skip(source_file_work_identity(&candidate));
                                failures.push(SourceValidationFailure {
                                    source_index: selected.source_index,
                                    stats: SourceStats {
                                        files: 1,
                                        bytes: candidate.estimated_bytes,
                                        change_token: None,
                                    },
                                    error,
                                });
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    *work = retained;
                }
            }
            selected.stats.files = selected.work.unit_count();
            selected.stats.bytes = selected_work_bytes(&selected.work);
        }
        recompute_slice_totals(slice);
        Ok(failures)
    }
}
