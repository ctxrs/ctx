fn import_selected_fresh_new_group(
    store: &mut Store,
    source: &SourceInfo,
    preinventory: &SourcePreinventory,
    selection: &SelectedImportWork,
    record: &HistoryRecord,
) -> Result<Option<ProviderImportBatchOutcome>> {
    if !selection.is_fresh_new_group() {
        return Ok(None);
    }
    let inventory_generation = preinventory.inventory_generation().ok_or_else(|| {
        anyhow::Error::new(CaptureError::SystemInvariant(
            "FreshNew selection has no inventory generation",
        ))
    })?;
    let context = match selection {
        SelectedImportWork::Catalog(_) => FreshNewImportContext {
            machine_id: CodexSessionImportOptions::default().machine_id,
            history_record: record.clone(),
        },
        SelectedImportWork::SourceFiles(_) => FreshNewImportContext {
            machine_id: PiSessionImportOptions::default().machine_id,
            history_record: record.clone(),
        },
    };
    let fresh_new = match selection {
        SelectedImportWork::Catalog(work) if source.provider == CaptureProvider::Codex => {
            import_codex_fresh_new_batch(store, work.clone(), inventory_generation, context)?
        }
        SelectedImportWork::SourceFiles(work) if source.provider == CaptureProvider::Pi => {
            import_pi_fresh_new_batch(store, work.clone(), inventory_generation, context)?
        }
        SelectedImportWork::Catalog(_) | SelectedImportWork::SourceFiles(_) => {
            return Err(anyhow::Error::new(CaptureError::SystemInvariant(
                "FreshNew selection does not match its source provider",
            )))
        }
    };
    let (outcome, maintenance_error) =
        provider_batch_outcome_from_fresh_new(selection, preinventory, fresh_new);
    if let Some(error) = maintenance_error {
        return Err(provider_import_batch_error(outcome, anyhow::anyhow!(error)));
    }
    Ok(Some(outcome))
}

fn provider_batch_outcome_from_fresh_new(
    selection: &SelectedImportWork,
    preinventory: &SourcePreinventory,
    fresh_new: FreshNewImportOutcome,
) -> (ProviderImportBatchOutcome, Option<String>) {
    let completed_paths = fresh_new
        .committed_paths
        .iter()
        .chain(&fresh_new.rejected_paths)
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let completed_bytes = match selection {
        SelectedImportWork::Catalog(work) => work.iter().fold(0_u64, |total, candidate| {
            if completed_paths.contains(candidate.session.source_path.as_str()) {
                total.saturating_add(candidate.estimated_bytes)
            } else {
                total
            }
        }),
        SelectedImportWork::SourceFiles(work) => work.iter().fold(0_u64, |total, candidate| {
            if completed_paths.contains(candidate.file.source_path.as_str()) {
                total.saturating_add(candidate.estimated_bytes)
            } else {
                total
            }
        }),
    };
    let completed_units = fresh_new
        .committed_paths
        .len()
        .saturating_add(fresh_new.rejected_paths.len());
    let deferred_units = fresh_new
        .durable_only_paths
        .len()
        .saturating_add(fresh_new.remainder_paths.len());
    let durable_progress = completed_units > 0 || !fresh_new.durable_only_paths.is_empty();
    (
        ProviderImportBatchOutcome {
            summary: fresh_new.summary,
            completed_units,
            completed_bytes,
            deferred_units,
            durable_progress,
            stop_admission: fresh_new.maintenance_pending || fresh_new.maintenance_error.is_some(),
            post_import_inventory_generation: preinventory.inventory_generation(),
            post_import_preinventory: Some(preinventory.clone()),
        },
        fresh_new.maintenance_error,
    )
}
