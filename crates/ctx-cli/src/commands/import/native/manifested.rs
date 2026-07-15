fn import_manifested_source(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    options: ManifestedImportOptions<'_>,
) -> Result<ProviderImportBatchOutcome> {
    let mut import_file = |store: &mut Store, pending_source: &SourceInfo| {
        import_one_source_inner(
            store,
            pending_source,
            progress.clone(),
            false,
            true,
            &SourcePreinventory::None,
        )
    };
    import_manifested_source_with_importer(store, source, options, &mut import_file)
}

#[derive(Clone, Copy)]
struct ManifestedImportOptions<'a> {
    preinventoried_files: Option<&'a [SourceImportFile]>,
    preinventory_generation: Option<u64>,
    force_selection: bool,
    selection: Option<&'a SelectedImportWork>,
}

impl<'a> ManifestedImportOptions<'a> {
    fn new(
        preinventoried_files: Option<&'a [SourceImportFile]>,
        preinventory_generation: Option<u64>,
        force_selection: bool,
        selection: Option<&'a SelectedImportWork>,
    ) -> Self {
        Self {
            preinventoried_files,
            preinventory_generation,
            force_selection,
            selection,
        }
    }
}

struct ManifestedImportOutcome {
    observation: SourceImportFile,
    status: CatalogIndexedStatus,
    error: Option<String>,
    result: ManifestedImportResult,
}

enum ManifestedImportResult {
    Imported(ProviderImportSummary),
    SourceFailure(anyhow::Error),
    SystemFailure,
}

fn import_manifested_source_with_importer(
    store: &mut Store,
    source: &SourceInfo,
    options: ManifestedImportOptions<'_>,
    import_file: &mut dyn FnMut(&mut Store, &SourceInfo) -> Result<ProviderImportSummary>,
) -> Result<ProviderImportBatchOutcome> {
    let ManifestedImportOptions {
        preinventoried_files,
        preinventory_generation,
        force_selection,
        selection,
    } = options;
    let source_root = persisted_import_identity(&source.path, "source root")?.to_owned();
    let collected_files;
    let mut outcome_generation = preinventory_generation;
    let files = match preinventoried_files {
        Some(files) => files,
        None => {
            collected_files = collect_source_import_files(source).with_context(|| {
                format!("inventory import files from {}", source.path.display())
            })?;
            let persisted = persist_new_source_import_observation(store, source, &collected_files)?;
            outcome_generation = Some(persisted.inventory_generation);
            &collected_files
        }
    };
    if files.is_empty() {
        return Err(anyhow!(
            "no importable {} history files found under {}",
            source.provider.as_str(),
            source.path.display()
        ));
    }
    let selected_files = match selection {
        Some(SelectedImportWork::SourceFiles(work)) => {
            work.iter().map(|work| work.file.clone()).collect()
        }
        Some(SelectedImportWork::Catalog(_)) => {
            return Err(anyhow::Error::new(CaptureError::SystemInvariant(
                "catalog work selected for a manifested source",
            )))
        }
        None if force_selection => files.to_vec(),
        None => store.list_pending_source_import_files(source.provider, &source_root)?,
    };
    if selected_files.is_empty() {
        return Ok(ProviderImportBatchOutcome::completed(
            ProviderImportSummary::default(),
            0,
        ));
    }
    let selected_work_by_path = match selection {
        Some(SelectedImportWork::SourceFiles(work)) => work
            .iter()
            .map(|work| (work.file.source_path.as_str(), work))
            .collect::<BTreeMap<_, _>>(),
        Some(SelectedImportWork::Catalog(_)) | None => BTreeMap::new(),
    };

    let mut summary = ProviderImportSummary::default();
    let mut deferred_units = 0;
    let mut outcomes = Vec::with_capacity(selected_files.len());
    let mut system_error = None;
    for pending_file in selected_files {
        let path = PathBuf::from(&pending_file.source_path);
        let mut pending_source = explicit_path_source(source.provider, path);
        pending_source.source_format = source.source_format;
        let imported = if provider_file_mutation_contract(source.provider, source.source_format)
            == ProviderFileMutationContract::AppendOnlyNewlineDelimited
        {
            match selected_work_by_path.get(pending_file.source_path.as_str()) {
                Some(work) => match import_manifested_append_source_file_work(
                    store,
                    &pending_source,
                    work,
                    preinventory_generation.ok_or_else(|| {
                        anyhow::Error::new(CaptureError::SystemInvariant(
                            "selected source-file work has no inventory generation",
                        ))
                    })?,
                ) {
                    Ok(AppendImportOutcome::Imported(summary)) => Some(Ok(summary)),
                    Ok(AppendImportOutcome::Deferred) => {
                        deferred_units += 1;
                        None
                    }
                    Err(error) => Some(Err(error)),
                },
                None => Some(import_file(store, &pending_source)),
            }
        } else {
            Some(import_file(store, &pending_source))
        };
        let Some(imported) = imported else {
            continue;
        };
        match imported {
            Ok(file_summary) => {
                let status = provider_summary_import_status(&file_summary);
                let error =
                    (file_summary.failed > 0).then(|| source_import_file_failure(&file_summary));
                outcomes.push(ManifestedImportOutcome {
                    observation: pending_file,
                    status,
                    error,
                    result: ManifestedImportResult::Imported(file_summary),
                });
            }
            Err(err) => {
                if publication_recovery_required(&err) {
                    if system_error.is_none() {
                        system_error = Some(err);
                    }
                    continue;
                }
                if let Some(file_summary) = rejected_source_summary(&err) {
                    let status = provider_summary_import_status(&file_summary);
                    let error = (file_summary.failed > 0)
                        .then(|| source_import_file_failure(&file_summary));
                    outcomes.push(ManifestedImportOutcome {
                        observation: pending_file,
                        status,
                        error,
                        result: ManifestedImportResult::Imported(file_summary),
                    });
                    continue;
                }
                let failure_scope = import_error_scope(&err);
                let error = error_summary(&err);
                let status = import_error_status(&err);
                let is_system_failure = failure_scope == ImportFailureScope::System;
                if is_system_failure {
                    outcomes.push(ManifestedImportOutcome {
                        observation: pending_file,
                        status,
                        error: Some(error),
                        result: ManifestedImportResult::SystemFailure,
                    });
                    system_error = Some(err);
                    break;
                } else {
                    outcomes.push(ManifestedImportOutcome {
                        observation: pending_file,
                        status,
                        error: Some(error),
                        result: ManifestedImportResult::SourceFailure(err),
                    });
                }
            }
        }
    }
    let outcome_generation = outcome_generation.ok_or_else(|| {
        anyhow::Error::new(CaptureError::SystemInvariant(
            "manifested import outcomes have no inventory generation",
        ))
    })?;
    let persisted = match persist_reobserved_manifested_outcomes(
        store,
        source,
        outcome_generation,
        &outcomes,
    ) {
        Ok(persisted) => persisted,
        Err(persist_error) => {
            let mut partial_summary = ProviderImportSummary::default();
            for outcome in &outcomes {
                if let ManifestedImportResult::Imported(file_summary) = &outcome.result {
                    partial_summary.merge_from(file_summary.clone());
                }
            }
            return Err(provider_import_batch_error(
                ProviderImportBatchOutcome {
                    summary: partial_summary,
                    completed_units: 0,
                    completed_bytes: 0,
                    deferred_units,
                    post_import_inventory_generation: None,
                    post_import_preinventory: None,
                },
                system_error.unwrap_or(persist_error),
            ));
        }
    };
    let mut source_error = None;
    let mut completed_paths = BTreeSet::new();
    for outcome in outcomes {
        let source_path = outcome.observation.source_path.clone();
        match outcome.result {
            ManifestedImportResult::Imported(file_summary) => {
                if persisted.current_outcomes.contains(&source_path) {
                    completed_paths.insert(source_path);
                }
                summary.merge_from(file_summary);
            }
            ManifestedImportResult::SourceFailure(error)
                if persisted
                    .current_outcomes
                    .contains(&outcome.observation.source_path) =>
            {
                if import_error_retryability(&error) == ImportRetryability::Terminal {
                    completed_paths.insert(source_path.clone());
                    summary.failed = summary.failed.saturating_add(1);
                    summary.failures.push(ProviderImportFailure {
                        line: 0,
                        error: format!("{source_path}: {}", error_summary(&error)),
                    });
                } else if source_error.is_none() {
                    source_error = Some(error);
                }
            }
            ManifestedImportResult::SourceFailure(_) | ManifestedImportResult::SystemFailure => {}
        }
    }
    let completed_bytes = completed_paths
        .iter()
        .filter_map(|source_path| selected_work_by_path.get(source_path.as_str()))
        .fold(0_u64, |total, work| {
            total.saturating_add(work.estimated_bytes)
        });
    let outcome = ProviderImportBatchOutcome {
        summary,
        completed_units: completed_paths.len(),
        completed_bytes,
        deferred_units,
        post_import_inventory_generation: Some(persisted.inventory_generation),
        post_import_preinventory: None,
    };
    if let Some(error) = system_error {
        return Err(provider_import_batch_error(outcome, error));
    }
    if let Some(error) = source_error {
        return Err(provider_import_batch_error(outcome, error));
    }
    Ok(outcome)
}

struct PersistedManifestedOutcomes {
    current_outcomes: BTreeSet<String>,
    inventory_generation: u64,
}

fn persist_reobserved_manifested_outcomes(
    store: &Store,
    source: &SourceInfo,
    inventory_generation: u64,
    outcomes: &[ManifestedImportOutcome],
) -> Result<PersistedManifestedOutcomes> {
    let mut current_outcomes = BTreeSet::new();
    for outcome in outcomes {
        let Some(current) =
            observe_selected_source_import_file(source, &outcome.observation.source_path)?
        else {
            continue;
        };
        if !same_source_import_observation(&outcome.observation, &current) {
            continue;
        }
        let changed = store.record_source_import_file_result(
            current.provider,
            SourceImportFileIndexUpdate {
                source_root: &current.source_root,
                source_path: &current.source_path,
                file_size_bytes: current.file_size_bytes,
                file_modified_at_ms: current.file_modified_at_ms,
                import_revision: current.import_revision,
                inventory_generation,
                metadata: &current.metadata,
                indexed_at_ms: utc_now().timestamp_millis(),
            },
            outcome.status,
            outcome.error.as_deref(),
        )?;
        if changed == 1 {
            current_outcomes.insert(current.source_path);
        }
    }
    Ok(PersistedManifestedOutcomes {
        current_outcomes,
        inventory_generation,
    })
}

fn import_error_status(error: &anyhow::Error) -> CatalogIndexedStatus {
    match import_error_retryability(error) {
        ImportRetryability::Retryable => CatalogIndexedStatus::Failed,
        ImportRetryability::Terminal => CatalogIndexedStatus::Rejected,
    }
}

fn source_import_file_failure(summary: &ProviderImportSummary) -> String {
    let Some(failure) = summary.failures.first() else {
        return "provider import failed".to_owned();
    };
    match failure.line {
        0 => failure.error.clone(),
        line => format!("line {line}: {}", failure.error),
    }
}
