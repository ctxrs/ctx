enum ImportCandidate {
    Retirement(ProviderFilePublicationRetirementWork),
    Catalog {
        source_index: usize,
        work: CatalogImportWork,
    },
    SourceFile {
        source_index: usize,
        work: SourceImportFileWork,
    },
}

impl ImportCandidate {
    fn has_active_publication(&self) -> bool {
        match self {
            Self::Retirement(_) => true,
            Self::Catalog { work, .. } => work.has_active_publication,
            Self::SourceFile { work, .. } => work.has_active_publication,
        }
    }

    fn estimated_bytes(&self) -> u64 {
        match self {
            Self::Retirement(work) => work.estimated_bytes,
            Self::Catalog { work, .. } => work.estimated_bytes,
            Self::SourceFile { work, .. } => work.estimated_bytes,
        }
    }

    fn last_attempt_at_ms(&self) -> Option<i64> {
        match self {
            Self::Retirement(work) => Some(work.last_attempt_at_ms),
            Self::Catalog { work, .. } => work.last_attempt_at_ms,
            Self::SourceFile { work, .. } => work.last_attempt_at_ms,
        }
    }

    fn identity(&self) -> String {
        match self {
            Self::Retirement(work) => retirement_work_identity(work),
            Self::Catalog { work, .. } => catalog_work_identity(work),
            Self::SourceFile { work, .. } => source_file_work_identity(work),
        }
    }

    fn stable_identity(&self, sources: &[PlannedImportSource]) -> String {
        match self {
            Self::Retirement(work) => format!(
                "{}\u{0}{}\u{0}{}\u{0}{}",
                work.provider.as_str(),
                work.material_source_format,
                work.material_source_root,
                work.source_path
            ),
            Self::Catalog { source_index, work } => {
                stable_source_work_identity(&sources[*source_index], &work.session.source_path)
            }
            Self::SourceFile { source_index, work } => {
                stable_source_work_identity(&sources[*source_index], &work.file.source_path)
            }
        }
    }
}

enum SelectedCandidate {
    Catalog(CatalogImportWork),
    SourceFile(SourceImportFileWork),
}

fn push_source_candidate(
    slice: &mut ImportSlice,
    source_index: usize,
    preinventory: SourcePreinventory,
    candidate: SelectedCandidate,
) {
    let bytes = match &candidate {
        SelectedCandidate::Catalog(work) => work.estimated_bytes,
        SelectedCandidate::SourceFile(work) => work.estimated_bytes,
    };
    if let Some(selected) = slice
        .sources
        .iter_mut()
        .find(|selected| selected.source_index == source_index)
    {
        match (&mut selected.work, candidate) {
            (SelectedImportWork::Catalog(work), SelectedCandidate::Catalog(candidate)) => {
                work.push(candidate);
            }
            (SelectedImportWork::SourceFiles(work), SelectedCandidate::SourceFile(candidate)) => {
                work.push(candidate);
            }
            _ => unreachable!("one source cannot mix catalog and source-file work"),
        }
        selected.stats.files = selected.stats.files.saturating_add(1);
        selected.stats.bytes = selected.stats.bytes.saturating_add(bytes);
        return;
    }

    let work = match candidate {
        SelectedCandidate::Catalog(work) => SelectedImportWork::Catalog(vec![work]),
        SelectedCandidate::SourceFile(work) => SelectedImportWork::SourceFiles(vec![work]),
    };
    slice.sources.push(SelectedImportSource {
        source_index,
        preinventory,
        work,
        stats: SourceStats {
            files: 1,
            bytes,
            change_token: None,
        },
        attempts_persisted: false,
    });
}

fn retain_current_generations(
    store: &Store,
    slice: &mut ImportSlice,
    state: &mut ImportExecutionState,
) -> Result<()> {
    for selected in &mut slice.sources {
        match (&selected.preinventory, &mut selected.work) {
            (
                SourcePreinventory::CodexSessionCatalog {
                    inventory_generation,
                    ..
                },
                SelectedImportWork::Catalog(work),
            ) => {
                let mut retained = Vec::with_capacity(work.len());
                for candidate in std::mem::take(work) {
                    match persist_catalog_attempt_started(store, &candidate, *inventory_generation)
                    {
                        Ok(1) => retained.push(candidate),
                        Ok(_) => state.mark_validation_skip(catalog_work_identity(&candidate)),
                        Err(error) if provider_publication_blocks_attempt(&error) => {
                            if catalog_candidate_matches_publication(
                                store,
                                &candidate,
                                *inventory_generation,
                            )? {
                                retained.push(candidate);
                            } else {
                                state.mark_validation_skip(catalog_work_identity(&candidate));
                            }
                        }
                        Err(error) => return Err(error),
                    }
                }
                *work = retained;
            }
            (
                SourcePreinventory::SourceRoot {
                    inventory_generation,
                    ..
                }
                | SourcePreinventory::SourceImportFiles {
                    inventory_generation,
                    ..
                },
                SelectedImportWork::SourceFiles(work),
            ) => {
                let mut retained = Vec::with_capacity(work.len());
                for candidate in std::mem::take(work) {
                    match persist_source_file_attempt_started(
                        store,
                        &candidate,
                        *inventory_generation,
                    ) {
                        Ok(1) => retained.push(candidate),
                        Ok(_) => state.mark_validation_skip(source_file_work_identity(&candidate)),
                        Err(error) if provider_publication_blocks_attempt(&error) => {
                            if source_file_candidate_matches_publication(
                                store,
                                &candidate,
                                *inventory_generation,
                            )? {
                                retained.push(candidate);
                            } else {
                                state.mark_validation_skip(source_file_work_identity(&candidate));
                            }
                        }
                        Err(error) => return Err(error),
                    }
                }
                *work = retained;
            }
            _ => {}
        }
        selected.stats.files = selected.work.unit_count();
        selected.stats.bytes = selected_work_bytes(&selected.work);
        selected.attempts_persisted = true;
    }
    recompute_slice_totals(slice);
    Ok(())
}

fn catalog_candidate_matches_publication(
    store: &Store,
    candidate: &CatalogImportWork,
    inventory_generation: u64,
) -> Result<bool> {
    let material_source_format = canonical_provider_material_source_format(
        candidate.session.provider,
        &candidate.session.source_format,
    )
    .unwrap_or(&candidate.session.source_format);
    store
        .provider_file_publication_matches_candidate(
            candidate.session.provider,
            ctx_history_store::ProviderFileInventoryObservation::ObservedCatalog {
                source_format: &candidate.session.source_format,
                update: ctx_history_store::CatalogSourceIndexUpdate {
                    source_root: &candidate.session.source_root,
                    source_path: &candidate.session.source_path,
                    file_size_bytes: candidate.session.file_size_bytes,
                    file_modified_at_ms: candidate.session.file_modified_at_ms,
                    import_revision: candidate.session.import_revision,
                    inventory_generation,
                    file_sha256: None,
                    event_count: None,
                    indexed_at_ms: 0,
                },
                metadata: &candidate.session.metadata,
            },
            material_source_format,
            &candidate.session.source_root,
        )
        .map_err(Into::into)
}

fn source_file_candidate_matches_publication(
    store: &Store,
    candidate: &SourceImportFileWork,
    inventory_generation: u64,
) -> Result<bool> {
    let material_source_format = canonical_provider_material_source_format(
        candidate.file.provider,
        &candidate.file.source_format,
    )
    .unwrap_or(&candidate.file.source_format);
    store
        .provider_file_publication_matches_candidate(
            candidate.file.provider,
            ctx_history_store::ProviderFileInventoryObservation::SourceImport {
                source_format: &candidate.file.source_format,
                update: ctx_history_store::SourceImportFileIndexUpdate {
                    source_root: &candidate.file.source_root,
                    source_path: &candidate.file.source_path,
                    file_size_bytes: candidate.file.file_size_bytes,
                    file_modified_at_ms: candidate.file.file_modified_at_ms,
                    import_revision: candidate.file.import_revision,
                    inventory_generation,
                    metadata: &candidate.file.metadata,
                    indexed_at_ms: 0,
                },
            },
            material_source_format,
            &candidate.file.source_root,
        )
        .map_err(Into::into)
}

fn invalidate_catalog_publication_observation(
    store: &Store,
    candidate: &CatalogImportWork,
    inventory_generation: u64,
) -> Result<bool> {
    let Some(owner) = store.effective_provider_file_publication_inventory_owner()? else {
        return Ok(false);
    };
    let metadata_matches = owner.metadata_json.as_deref().is_none_or(|metadata| {
        serde_json::from_str::<serde_json::Value>(metadata)
            .is_ok_and(|metadata| metadata == candidate.session.metadata)
    });
    if owner.inventory_family != ProviderFileInventoryFamily::Catalog
        || owner.provider != candidate.session.provider
        || owner.source_format != candidate.session.source_format
        || owner.source_root != candidate.session.source_root
        || owner.source_path != candidate.session.source_path
        || owner.inventory_generation != inventory_generation
        || owner.file_size_bytes != candidate.session.file_size_bytes
        || owner.file_modified_at_ms != candidate.session.file_modified_at_ms
        || owner.import_revision != candidate.session.import_revision
        || !metadata_matches
    {
        return Ok(false);
    }
    store
        .invalidate_effective_provider_file_publication_observation(
            &owner,
            ctx_history_core::utc_now().timestamp_millis(),
        )
        .map_err(Into::into)
}

fn invalidate_source_file_publication_observation(
    store: &Store,
    candidate: &SourceImportFileWork,
    inventory_generation: u64,
) -> Result<bool> {
    let Some(owner) = store.effective_provider_file_publication_inventory_owner()? else {
        return Ok(false);
    };
    let metadata_matches = owner
        .metadata_json
        .as_deref()
        .and_then(|metadata| serde_json::from_str::<serde_json::Value>(metadata).ok())
        .is_some_and(|metadata| metadata == candidate.file.metadata);
    if owner.inventory_family != ProviderFileInventoryFamily::SourceImport
        || owner.provider != candidate.file.provider
        || owner.source_format != candidate.file.source_format
        || owner.source_root != candidate.file.source_root
        || owner.source_path != candidate.file.source_path
        || owner.inventory_generation != inventory_generation
        || owner.file_size_bytes != candidate.file.file_size_bytes
        || owner.file_modified_at_ms != candidate.file.file_modified_at_ms
        || owner.import_revision != candidate.file.import_revision
        || !metadata_matches
    {
        return Ok(false);
    }
    store
        .invalidate_effective_provider_file_publication_observation(
            &owner,
            ctx_history_core::utc_now().timestamp_millis(),
        )
        .map_err(Into::into)
}

fn recompute_slice_totals(slice: &mut ImportSlice) {
    slice
        .sources
        .retain(|selected| selected.work.unit_count() > 0);
    slice.units = slice
        .retirements
        .len()
        .saturating_add(slice.sources.iter().map(|source| source.stats.files).sum());
    slice.bytes = slice
        .retirements
        .iter()
        .fold(0_u64, |total, work| {
            total.saturating_add(work.estimated_bytes)
        })
        .saturating_add(slice.sources.iter().fold(0_u64, |total, source| {
            total.saturating_add(source.stats.bytes)
        }));
}

fn file_observation_is_current_or_compatible_append(
    path: &Path,
    expected_bytes: u64,
    expected_modified_ms: i64,
    expected_metadata: &serde_json::Value,
    allow_append: bool,
) -> bool {
    let Ok(observation) = ctx_history_capture::observe_ordinary_file(path) else {
        return false;
    };
    let current_bytes = observation.len();
    let token_matches = expected_metadata
        .get("file_observation_token_v1")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|expected| expected == observation.token_hex());
    (current_bytes == expected_bytes
        && super::catalog::system_time_ms(observation.modified_at()) == expected_modified_ms
        && token_matches)
        || (allow_append && current_bytes > expected_bytes)
}

fn active_append_observation_is_compatible(
    store: &Store,
    source: &crate::provider_sources::SourceInfo,
    candidate: &SourceImportFileWork,
    current: &ctx_history_store::SourceImportFile,
) -> Result<bool> {
    Ok(candidate.has_active_publication
        && source.mutation_contract
            == ctx_history_capture::ProviderFileMutationContract::AppendOnlyNewlineDelimited
        && store.effective_provider_file_publication_has_staged_completion()?
        && candidate.file.provider == current.provider
        && candidate.file.source_format == current.source_format
        && candidate.file.source_root == current.source_root
        && candidate.file.source_path == current.source_path
        && candidate.file.import_revision == current.import_revision
        && append_observation_metadata_is_compatible(&candidate.file.metadata, &current.metadata)
        && current.file_size_bytes > candidate.file.file_size_bytes)
}

fn append_observation_metadata_is_compatible(
    previous: &serde_json::Value,
    current: &serde_json::Value,
) -> bool {
    let (Some(mut previous), Some(mut current)) =
        (previous.as_object().cloned(), current.as_object().cloned())
    else {
        return false;
    };
    if !previous
        .get("dependencies")
        .and_then(serde_json::Value::as_array)
        .is_some_and(Vec::is_empty)
        || !current
            .get("dependencies")
            .and_then(serde_json::Value::as_array)
            .is_some_and(Vec::is_empty)
    {
        return false;
    }
    previous.remove("change_token_v1");
    current.remove("change_token_v1");
    previous == current
}

fn selected_work_bytes(work: &SelectedImportWork) -> u64 {
    match work {
        SelectedImportWork::Catalog(work) => work.iter().fold(0_u64, |total, work| {
            total.saturating_add(work.estimated_bytes)
        }),
        SelectedImportWork::SourceFiles(work) => work.iter().fold(0_u64, |total, work| {
            total.saturating_add(work.estimated_bytes)
        }),
    }
}

fn stable_source_work_identity(plan: &PlannedImportSource, source_path: &str) -> String {
    format!(
        "{}\u{0}{}\u{0}{}\u{0}{}",
        plan.source.provider.as_str(),
        plan.source.source_format,
        plan.source.path.display(),
        source_path
    )
}

fn retirement_work_identity(work: &ProviderFilePublicationRetirementWork) -> String {
    format!(
        "retirement\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
        work.provider.as_str(),
        work.material_source_format,
        work.material_source_root,
        work.source_path
    )
}

fn catalog_work_identity(work: &CatalogImportWork) -> String {
    format!(
        "catalog\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
        work.session.provider.as_str(),
        work.session.source_format,
        work.session.source_root,
        work.session.source_path,
        work.session.file_size_bytes,
        work.session.file_modified_at_ms,
        work.session.import_revision,
    )
}

fn source_file_work_identity(work: &SourceImportFileWork) -> String {
    format!(
        "source-file\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
        work.file.provider.as_str(),
        work.file.source_format,
        work.file.source_root,
        work.file.source_path,
        work.file.file_size_bytes,
        work.file.file_modified_at_ms,
        work.file.import_revision,
        work.file.metadata,
    )
}

fn observe_current_preinventory(
    store: &Store,
    plan: &PlannedImportSource,
    selected: &SelectedImportSource,
) -> Result<SourcePreinventory> {
    if source_uses_import_file_manifest(&plan.source) {
        let (files, inventory_generation) = match (&selected.preinventory, &selected.work) {
            (
                SourcePreinventory::SourceImportFiles {
                    inventory_generation,
                    ..
                },
                SelectedImportWork::SourceFiles(work),
            ) => {
                let candidate = work
                    .iter()
                    .find(|candidate| candidate.has_active_publication)
                    .ok_or_else(|| {
                        anyhow::Error::new(ctx_history_capture::CaptureError::SystemInvariant(
                            "pre-lock manifest observation has no active publication candidate",
                        ))
                    })?;
                let current =
                    observe_selected_source_import_file(&plan.source, &candidate.file.source_path)?;
                let observation_changed = current.as_ref().is_none_or(|current| {
                    !same_source_import_observation(&candidate.file, current)
                });
                let generation_is_current = store.current_source_import_inventory_generation(
                    plan.source.provider,
                    &candidate.file.source_root,
                )? == Some(*inventory_generation);
                if observation_changed && !generation_is_current {
                    let files = collect_source_import_files(&plan.source)?;
                    let persisted =
                        persist_new_source_import_observation(store, &plan.source, &files)?;
                    (files, persisted.inventory_generation)
                } else {
                    let mut files = match &selected.preinventory {
                        SourcePreinventory::SourceImportFiles { files, .. } => files.clone(),
                        _ => unreachable!("matched manifested source-file preinventory"),
                    };
                    let owner_was_cached = files
                        .iter()
                        .any(|file| file.source_path == candidate.file.source_path);
                    if observation_changed || !owner_was_cached {
                        files.retain(|file| file.source_path != candidate.file.source_path);
                        files.extend(current);
                        files.sort_by(|left, right| left.source_path.cmp(&right.source_path));
                    }
                    (files, *inventory_generation)
                }
            }
            _ => {
                return Err(anyhow::Error::new(
                    ctx_history_capture::CaptureError::SystemInvariant(
                        "manifest publication selection has no source-file inventory",
                    ),
                ));
            }
        };
        return Ok(SourcePreinventory::SourceImportFiles {
            files,
            inventory_generation,
        });
    }
    let (_, file) = observe_source_root(&plan.source)?;
    let persisted =
        persist_new_source_import_observation(store, &plan.source, std::slice::from_ref(&file))?;
    Ok(SourcePreinventory::SourceRoot {
        file,
        inventory_generation: persisted.inventory_generation,
    })
}

fn list_source_work(
    store: &Store,
    plan: &PlannedImportSource,
    preinventory: &SourcePreinventory,
    class: ImportWorkClass,
    limit: usize,
) -> Result<Option<SelectedImportWork>> {
    match preinventory {
        SourcePreinventory::CodexSessionCatalog { .. } => {
            let source_root = super::catalog::codex_catalog_root_identity(&plan.source.path)?;
            Ok(Some(SelectedImportWork::Catalog(
                store.list_catalog_import_work(plan.source.provider, source_root, class, limit)?,
            )))
        }
        SourcePreinventory::SourceImportFiles { files, .. } => {
            let source_root = files
                .first()
                .map(|file| file.source_root.as_str())
                .map(Ok)
                .unwrap_or_else(|| persisted_import_identity(&plan.source.path, "source root"))?;
            Ok(Some(SelectedImportWork::SourceFiles(
                store.list_source_import_file_work(
                    plan.source.provider,
                    source_root,
                    class,
                    limit,
                )?,
            )))
        }
        SourcePreinventory::SourceRoot { file, .. } => Ok(Some(SelectedImportWork::SourceFiles(
            store.list_source_import_file_work(
                plan.source.provider,
                &file.source_root,
                class,
                limit,
            )?,
        ))),
        SourcePreinventory::None => Ok(None),
    }
}

fn import_work_counts(store: &Store, sources: &[PlannedImportSource]) -> Result<(usize, usize)> {
    Ok((
        import_work_count(store, sources, ImportWorkClass::Fresh)?,
        import_work_count(store, sources, ImportWorkClass::Recovery)?,
    ))
}

fn import_work_count(
    store: &Store,
    sources: &[PlannedImportSource],
    class: ImportWorkClass,
) -> Result<usize> {
    let mut count = 0usize;
    if class == ImportWorkClass::Recovery {
        count = store
            .list_provider_file_publication_retirement_work(IMPORT_PENDING_REPORT_LIMIT)?
            .len();
    }
    for plan in sources {
        let remaining = IMPORT_PENDING_REPORT_LIMIT.saturating_sub(count);
        if remaining == 0 {
            break;
        }
        let source_count = match &plan.preinventory {
            SourcePreinventory::CodexSessionCatalog { .. } => {
                let source_root = super::catalog::codex_catalog_root_identity(&plan.source.path)?;
                store
                    .list_catalog_import_work(plan.source.provider, source_root, class, remaining)?
                    .len()
            }
            SourcePreinventory::SourceImportFiles { files, .. } => {
                let source_root = files
                    .first()
                    .map(|file| file.source_root.as_str())
                    .map(Ok)
                    .unwrap_or_else(|| {
                        persisted_import_identity(&plan.source.path, "source root")
                    })?;
                store
                    .list_source_import_file_work(
                        plan.source.provider,
                        source_root,
                        class,
                        remaining,
                    )?
                    .len()
            }
            SourcePreinventory::SourceRoot { file, .. } => store
                .list_source_import_file_work(
                    plan.source.provider,
                    &file.source_root,
                    class,
                    remaining,
                )?
                .len(),
            SourcePreinventory::None => continue,
        };
        count = count.saturating_add(source_count);
    }
    Ok(count.min(IMPORT_PENDING_REPORT_LIMIT))
}

pub(crate) fn bounded_unplanned_root_work_counts(
    store: &Store,
    provider: ctx_history_core::CaptureProvider,
    source_root: &str,
) -> Result<(usize, usize)> {
    Ok((
        bounded_unplanned_root_work_count(store, provider, source_root, ImportWorkClass::Fresh)?,
        bounded_unplanned_root_work_count(store, provider, source_root, ImportWorkClass::Recovery)?,
    ))
}

fn bounded_unplanned_root_work_count(
    store: &Store,
    provider: ctx_history_core::CaptureProvider,
    source_root: &str,
    class: ImportWorkClass,
) -> Result<usize> {
    let catalog = store.list_catalog_import_work(
        provider,
        source_root,
        class,
        IMPORT_PENDING_REPORT_LIMIT,
    )?;
    let remaining = IMPORT_PENDING_REPORT_LIMIT.saturating_sub(catalog.len());
    let source_files =
        store.list_source_import_file_work(provider, source_root, class, remaining)?;
    Ok(catalog
        .len()
        .saturating_add(source_files.len())
        .min(IMPORT_PENDING_REPORT_LIMIT))
}

fn execution_budget(reported_units: usize) -> usize {
    if reported_units >= IMPORT_PENDING_REPORT_LIMIT {
        usize::MAX
    } else {
        reported_units
    }
}
