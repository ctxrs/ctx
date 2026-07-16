pub(crate) fn import_manifested_source(
    store: &mut Store,
    source: &SourceInfo,
    record_id: Uuid,
    progress: Option<CodexSessionImportProgressCallback>,
    preinventoried_files: Option<&[SourceImportFile]>,
    preinventory_generation: Option<u64>,
    force_selection: bool,
) -> Result<ProviderImportSummary> {
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
    import_manifested_source_with_importer(
        store,
        source,
        record_id,
        preinventoried_files,
        preinventory_generation,
        force_selection,
        &mut import_file,
    )
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
    record_id: Uuid,
    preinventoried_files: Option<&[SourceImportFile]>,
    _preinventory_generation: Option<u64>,
    force_selection: bool,
    import_file: &mut dyn FnMut(&mut Store, &SourceInfo) -> Result<ProviderImportSummary>,
) -> Result<ProviderImportSummary> {
    let source_root = persisted_import_identity(&source.path, "source root")?.to_owned();
    let collected_files;
    let files = match preinventoried_files {
        Some(files) => files,
        None => {
            collected_files = collect_source_import_files(source).with_context(|| {
                format!("inventory import files from {}", source.path.display())
            })?;
            persist_new_source_import_observation(store, source, &collected_files)?;
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
    let selected_files = if force_selection {
        files.to_vec()
    } else {
        store.list_pending_source_import_files(source.provider, &source_root)?
    };
    if selected_files.is_empty() {
        return Ok(ProviderImportSummary::default());
    }

    let mut summary = ProviderImportSummary::default();
    let mut outcomes = Vec::with_capacity(selected_files.len());
    let mut system_error = None;
    for pending_file in selected_files {
        let path = PathBuf::from(&pending_file.source_path);
        let mut pending_source = explicit_path_source(source.provider, path);
        pending_source.source_format = source.source_format;
        let imported = import_file(store, &pending_source);
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
    let persisted_outcomes = persist_reobserved_manifested_outcomes(store, source, &outcomes);
    if let Some(error) = system_error {
        return Err(error);
    }
    let current_outcomes = persisted_outcomes?;
    let mut source_error = None;
    for outcome in outcomes {
        match outcome.result {
            ManifestedImportResult::Imported(file_summary) => summary.merge_from(file_summary),
            ManifestedImportResult::SourceFailure(error)
                if current_outcomes.contains(&outcome.observation.source_path) =>
            {
                if source_error.is_none() {
                    source_error = Some(error);
                }
            }
            ManifestedImportResult::SourceFailure(_) | ManifestedImportResult::SystemFailure => {}
        }
    }
    let _ = record_id;
    if let Some(error) = source_error {
        return Err(error);
    }
    Ok(summary)
}

fn persist_reobserved_manifested_outcomes(
    store: &Store,
    source: &SourceInfo,
    outcomes: &[ManifestedImportOutcome],
) -> Result<BTreeSet<String>> {
    let current_files = collect_source_import_files(source)
        .with_context(|| format!("re-inventory import files from {}", source.path.display()))?;
    let current_by_path = current_files
        .iter()
        .map(|file| (file.source_path.as_str(), file))
        .collect::<BTreeMap<_, _>>();
    let mut persisted_outcomes = Vec::new();
    for outcome in outcomes {
        let Some(current) = current_by_path.get(outcome.observation.source_path.as_str()) else {
            continue;
        };
        if !same_source_import_observation(&outcome.observation, current) {
            continue;
        }
        persisted_outcomes.push(SourceImportObservationOutcome {
            file: current,
            status: outcome.status,
            error: outcome.error.as_deref(),
        });
    }
    persist_source_import_observation_with_outcomes(
        store,
        source,
        &current_files,
        &persisted_outcomes,
    )?;
    Ok(persisted_outcomes
        .iter()
        .map(|outcome| outcome.file.source_path.clone())
        .collect())
}

fn same_source_import_observation(left: &SourceImportFile, right: &SourceImportFile) -> bool {
    left.provider == right.provider
        && left.source_format == right.source_format
        && left.source_root == right.source_root
        && left.source_path == right.source_path
        && left.file_size_bytes == right.file_size_bytes
        && left.file_modified_at_ms == right.file_modified_at_ms
        && left.import_revision == right.import_revision
        && left.metadata == right.metadata
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
