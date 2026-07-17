use crate::commands::import::{
    stable_reinventory_sources, DrainFixedPointBlocker, ImportInventoryFailures,
    ImportMaintenanceStep, ImportSourceIdentity,
};

#[cfg(test)]
thread_local! {
    static SEARCH_REINVENTORY_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
fn inject_search_reinventory_hook_once(hook: impl FnOnce() + 'static) {
    SEARCH_REINVENTORY_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn run_search_reinventory_hook() {
    SEARCH_REINVENTORY_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_search_reinventory_hook() {}

#[allow(clippy::too_many_arguments)]
fn execute_search_refresh_work(
    data_root: &Path,
    store: &mut Store,
    refresh_source_count: usize,
    had_indexed_content: bool,
    search_projection_needs_backfill: bool,
    plugin_sources: Vec<HistorySourcePluginSource>,
    refresh: RefreshArg,
    json_output: bool,
    execution_policy: ImportExecutionPolicy,
    plan: &ImportPlan,
    execution_state: &mut crate::commands::import::ImportExecutionState,
    inventory_failures: Vec<ImportSourceFailure>,
    _failed_inventory_pending: (usize, usize),
    planned_total_bytes: u64,
) -> Result<SearchRefreshExecution> {
    let mut inventory_failures = ImportInventoryFailures::new(inventory_failures);
    inventory_failures.reconcile(&plan.sources, Vec::new());
    let mut failed_inventory_pending = inventory_failures.pending_counts(store, plan)?;
    let mut totals = ImportTotals::default();
    let mut deferred_units = 0usize;
    let mut completed_units = 0usize;

    let progress_arg = match refresh {
        RefreshArg::Wait if json_output => ProgressArg::Json,
        RefreshArg::Wait => ProgressArg::Auto,
        RefreshArg::Background | RefreshArg::Off => ProgressArg::None,
    };
    let progress = ProgressReporter::new(
        progress_arg,
        json_output,
        "search-refresh",
        planned_total_bytes,
    );
    totals.fresh_units_pending = failed_inventory_pending.0;
    totals.recovery_units_pending = failed_inventory_pending.1;
    let mut first_refresh_failure = None::<String>;
    for failure in inventory_failures.values() {
        first_refresh_failure.get_or_insert_with(|| failure.error.clone());
        totals.add_source_failure(&failure.stats);
        progress.warning(format!(
            "skipped {} during inventory: {}",
            failure.source.provider.as_str(),
            one_line_error(&failure.error)
        ));
    }
    let tolerate_source_errors = refresh == RefreshArg::Background;
    let mut imported_native_sources = BTreeSet::new();
    let mut failed_native_sources = BTreeSet::new();
    let fresh_slice_limit = execution_policy.fresh_slice_limit();
    let mut drain_pass_made_durable_progress = false;
    let mut drain_pass_retryable_blocker = false;
    loop {
        let initial_maintenance = match repair_import_maintenance(store) {
            Ok(maintenance) => maintenance,
            Err(error) => return Err(refresh_failure_with_totals(error, store, plan, totals)),
        };
        match initial_maintenance {
            ImportMaintenanceStep::Complete => break,
            ImportMaintenanceStep::Progress => {
                totals.durable_progress = true;
                drain_pass_made_durable_progress = true;
                if execution_policy == ImportExecutionPolicy::Drain {
                    continue;
                }
            }
            ImportMaintenanceStep::Pending(reason) => progress.warning(reason.diagnostic()),
        }
        let (fresh_units_pending, recovery_units_pending) = match plan.pending_counts(store) {
            Ok(counts) => counts,
            Err(error) => return Err(refresh_failure_with_totals(error, store, plan, totals)),
        };
        totals.fresh_units_pending = fresh_units_pending.saturating_add(failed_inventory_pending.0);
        totals.recovery_units_pending = recovery_units_pending
            .saturating_add(failed_inventory_pending.1)
            .saturating_add(1);
        return Ok(SearchRefreshExecution { totals });
    }
    let mut fresh_units = match plan.pending_count(store, ImportWorkClass::Fresh) {
        Ok(pending) => pending,
        Err(error) => return Err(refresh_failure_with_totals(error, store, plan, totals)),
    };
    loop {
        execution_state.begin_new_pass();
        if fresh_units == 0 {
            break;
        }
        match execute_search_refresh_plan_class(
            store,
            plan,
            execution_state,
            ImportWorkClass::Fresh,
            fresh_units,
            fresh_slice_limit,
            &progress,
            json_output,
            tolerate_source_errors,
            &mut totals,
            &mut first_refresh_failure,
            &mut imported_native_sources,
            &mut failed_native_sources,
            execution_policy == ImportExecutionPolicy::Drain,
        ) {
            Ok(result) => result,
            Err(error) => {
                return Err(refresh_failure_with_totals(error, store, plan, totals));
            }
        };
        drain_pass_made_durable_progress |= result.made_durable_progress;
        drain_pass_retryable_blocker |= result.retryable_blocker;
        deferred_units = result.result.deferred_units;
        completed_units = completed_units.saturating_add(result.result.completed_units);
        fresh_units = match plan.pending_count(store, ImportWorkClass::Fresh) {
            Ok(pending) => pending,
            Err(error) => return Err(refresh_failure_with_totals(error, store, plan, totals)),
        };
        if fresh_units == 0 {
            deferred_units = 0;
            break;
        }
        if fresh_slice_limit.is_some() || !result.result.made_durable_progress() {
            break;
        }
    }
    if execution_policy != ImportExecutionPolicy::Drain
        && (deferred_units > 0 || (totals.durable_progress && completed_units == 0))
    {
        let (fresh_units_pending, recovery_units_pending) =
            match reported_pending_counts(store, plan) {
                Ok((fresh_units_pending, recovery_units_pending)) => {
                    totals.fresh_units_pending =
                        fresh_units_pending.saturating_add(failed_inventory_pending.0);
                    totals.recovery_units_pending =
                        recovery_units_pending.saturating_add(failed_inventory_pending.1);
                    (fresh_units_pending, recovery_units_pending)
                }
                Err(error) => return Err(refresh_failure_with_totals(error, store, plan, totals)),
            };
        let publication_fenced = match store.has_pending_provider_file_publications() {
            Ok(pending) => pending,
            Err(error) => {
                return Err(refresh_failure_with_totals(
                    error.into(),
                    store,
                    plan,
                    totals,
                ));
            }
        };
        if publication_fenced && (fresh_units_pending > 0 || recovery_units_pending > 0) {
            return Ok(SearchRefreshExecution { totals });
        }
    }

    if !plugin_sources.is_empty() {
        for plugin_source in plugin_sources {
            progress.message(
                "refreshing",
                format!("running history source plugin {}", plugin_source.label()),
            );
            let import_result =
                import_history_source_plugin(store, &plugin_source, data_root, false).with_context(
                    || format!("refresh history source plugin {}", plugin_source.label()),
                );
            match import_result {
                Ok((summary, stats)) => {
                    warn_on_rejected_records(
                        &progress,
                        json_output,
                        &plugin_source.label(),
                        &summary,
                    );
                    totals.add(&summary, &stats);
                    progress.done(
                        "refreshing",
                        format!("refreshed history source plugin {}", plugin_source.label()),
                        0,
                    );
                }
                Err(err)
                    if refresh == RefreshArg::Background
                        && import_error_scope(&err) == ImportFailureScope::Source =>
                {
                    let error = error_summary(&err);
                    first_refresh_failure.get_or_insert_with(|| error.clone());
                    add_refresh_source_failure(&mut totals, &SourceStats::default(), &err);
                    progress.done(
                        "refreshing",
                        format!(
                            "skipped history source plugin {}: {}",
                            plugin_source.label(),
                            one_line_error(&error)
                        ),
                        0,
                    );
                }
                Err(err) => {
                    return Err(refresh_failure_with_totals(err, store, plan, totals));
                }
            }
        }
    }

    let recovery_slice_limit = execution_policy.recovery_slice_limit();
    let (drain_plan, maintenance_complete, final_checkpoint_allowed) = if execution_policy
        == ImportExecutionPolicy::Drain
    {
        match drain_search_refresh_to_fixed_point(
            store,
            plan,
            &progress,
            json_output,
            tolerate_source_errors,
            &mut totals,
            &mut first_refresh_failure,
            &mut imported_native_sources,
            &mut failed_native_sources,
            &mut inventory_failures,
            &mut failed_inventory_pending,
            drain_pass_made_durable_progress,
            drain_pass_retryable_blocker,
        ) {
            Ok((plan, complete, checkpoint_allowed)) => (Some(plan), complete, checkpoint_allowed),
            Err(error) => return Err(refresh_failure_with_totals(error, store, plan, totals)),
        }
    } else {
        execution_state.begin_new_pass();
        let maintenance = match repair_import_maintenance(store) {
            Ok(maintenance) => maintenance,
            Err(error) => return Err(refresh_failure_with_totals(error, store, plan, totals)),
        };
        totals.durable_progress |= maintenance.made_durable_progress();
        if maintenance != ImportMaintenanceStep::Complete {
            if let Some(reason) = maintenance.pending_reason() {
                progress.warning(reason.diagnostic());
            }
            let (fresh_units_pending, recovery_units_pending) = match plan.pending_counts(store) {
                Ok(counts) => counts,
                Err(error) => {
                    return Err(refresh_failure_with_totals(error, store, plan, totals));
                }
            };
            totals.fresh_units_pending =
                fresh_units_pending.saturating_add(failed_inventory_pending.0);
            totals.recovery_units_pending = recovery_units_pending
                .saturating_add(failed_inventory_pending.1)
                .saturating_add(1);
            return Ok(SearchRefreshExecution { totals });
        }
        let recovery_units = match plan.pending_count(store, ImportWorkClass::Recovery) {
            Ok(pending) => pending,
            Err(error) => return Err(refresh_failure_with_totals(error, store, plan, totals)),
        };
        let recovery_retryable_blocked = match execute_search_refresh_plan_class(
            store,
            plan,
            execution_state,
            ImportWorkClass::Recovery,
            recovery_units,
            recovery_slice_limit,
            &progress,
            json_output,
            tolerate_source_errors,
            &mut totals,
            &mut first_refresh_failure,
            &mut imported_native_sources,
            &mut failed_native_sources,
            false,
        ) {
            Ok(result) => result.retryable_blocker,
            Err(error) => return Err(refresh_failure_with_totals(error, store, plan, totals)),
        };
        let mut maintenance_complete = maintenance == ImportMaintenanceStep::Complete;
        let trailing_maintenance = match repair_import_maintenance(store) {
            Ok(maintenance) => maintenance,
            Err(error) => return Err(refresh_failure_with_totals(error, store, plan, totals)),
        };
        totals.durable_progress |= trailing_maintenance.made_durable_progress();
        maintenance_complete &= trailing_maintenance == ImportMaintenanceStep::Complete;
        (
            None,
            maintenance_complete,
            !recovery_retryable_blocked && maintenance_complete,
        )
    };
    let plan = drain_plan.as_ref().unwrap_or(plan);

    if search_projection_needs_backfill {
        if let Err(error) = store.refresh_search_index() {
            return Err(refresh_failure_with_totals(
                error.into(),
                store,
                plan,
                totals,
            ));
        }
    }

    let all_sources_failed = all_refresh_sources_failed(refresh_source_count, &totals);
    let all_rejected_without_prior_index = !had_indexed_content
        && totals.imported_sessions == 0
        && totals.imported_events == 0
        && totals.failed > 0;
    if refresh == RefreshArg::Background && (all_sources_failed || all_rejected_without_prior_index)
    {
        let detail = first_refresh_failure
            .map(|error| format!("; first failure: {error}"))
            .or_else(|| {
                (totals.failed > 0).then(|| {
                    format!(
                        "; background refresh imported no content and reported {} failure(s)",
                        totals.failed
                    )
                })
            })
            .unwrap_or_default();
        return Err(refresh_failure_with_totals(
            anyhow!("all search refresh sources failed{detail}"),
            store,
            plan,
            totals,
        ));
    }

    let (fresh_units_pending, recovery_units_pending) = match reported_pending_counts(store, plan) {
        Ok(counts) => counts,
        Err(error) => {
            return Err(refresh_failure_with_totals(error, store, plan, totals));
        }
    };
    totals.fresh_units_pending = fresh_units_pending.saturating_add(failed_inventory_pending.0);
    totals.recovery_units_pending = recovery_units_pending
        .saturating_add(failed_inventory_pending.1)
        .saturating_add(usize::from(!maintenance_complete));

    if maintenance_complete && final_checkpoint_allowed {
        if let Err(error) = store.checkpoint_wal_truncate_if_larger_than(WAL_TRUNCATE_MIN_BYTES) {
            return Err(anyhow::Error::new(SearchRefreshFailure {
                error: error.into(),
                totals,
            }));
        }
    }
    Ok(SearchRefreshExecution { totals })
}

#[allow(clippy::too_many_arguments)]
fn drain_search_refresh_to_fixed_point(
    store: &mut Store,
    initial_plan: &ImportPlan,
    progress: &ProgressReporter,
    json_output: bool,
    tolerate_source_errors: bool,
    totals: &mut ImportTotals,
    first_refresh_failure: &mut Option<String>,
    imported_sources: &mut BTreeSet<ImportSourceIdentity>,
    failed_sources: &mut BTreeSet<ImportSourceIdentity>,
    inventory_failures: &mut ImportInventoryFailures,
    failed_inventory_pending: &mut (usize, usize),
    mut pass_made_durable_progress: bool,
    mut pass_retryable_blocker: bool,
) -> Result<(ImportPlan, bool, bool)> {
    let reinventory_sources = stable_reinventory_sources(initial_plan, inventory_failures);
    let mut plan = ImportPlan::build(store, initial_plan.sources.clone())?;
    let mut execution_state = crate::commands::import::ImportExecutionState::for_plan(&plan);
    let mut installed_mixed_inventory_plan = false;

    loop {
        execution_state.begin_new_pass();
        let maintenance = repair_import_maintenance(store)?;
        let mut maintenance_complete = maintenance == ImportMaintenanceStep::Complete;
        let mut maintenance_pending = maintenance.pending_reason();
        if maintenance.made_durable_progress() {
            totals.durable_progress = true;
            pass_made_durable_progress = true;
        }

        if maintenance_complete {
            for class in [ImportWorkClass::Fresh, ImportWorkClass::Recovery] {
                execution_state.begin_new_pass();
                let pending_units = plan.pending_count(store, class)?;
                let result = execute_search_refresh_plan_class(
                    store,
                    &plan,
                    &mut execution_state,
                    class,
                    pending_units,
                    None,
                    progress,
                    json_output,
                    tolerate_source_errors,
                    totals,
                    first_refresh_failure,
                    imported_sources,
                    failed_sources,
                    true,
                )?;
                pass_made_durable_progress |= result.made_durable_progress;
                pass_retryable_blocker |= result.retryable_blocker;
            }

            let trailing = repair_import_maintenance(store)?;
            if trailing.made_durable_progress() {
                totals.durable_progress = true;
                pass_made_durable_progress = true;
            }
            maintenance_complete = trailing == ImportMaintenanceStep::Complete;
            maintenance_pending = trailing.pending_reason();
        }

        let (fresh_units_pending, recovery_units_pending) = plan.pending_counts(store)?;
        let has_pending_work = fresh_units_pending > 0
            || recovery_units_pending > 0
            || !maintenance_complete
            || !inventory_failures.is_empty()
            || store.has_pending_provider_file_publications()?
            || store.provider_file_publication_retirement_work_count()? > 0;
        let inventory_blocked = !inventory_failures.is_empty()
            && (installed_mixed_inventory_plan || !pass_made_durable_progress);
        let blocker = maintenance_pending
            .map(DrainFixedPointBlocker::Maintenance)
            .or_else(|| {
                (pass_retryable_blocker || inventory_blocked)
                    .then_some(DrainFixedPointBlocker::RetryableExternal)
            });
        match crate::commands::import::drain_fixed_point_action(
            has_pending_work,
            pass_made_durable_progress,
            blocker,
        )? {
            crate::commands::import::DrainFixedPointAction::Complete => {
                return Ok((plan, maintenance_complete, true));
            }
            crate::commands::import::DrainFixedPointAction::RetryableBlocked(blocker) => {
                progress.warning(blocker.diagnostic());
                return Ok((plan, maintenance_complete, false));
            }
            crate::commands::import::DrainFixedPointAction::Reinventory => {}
        }

        run_search_reinventory_hook();
        let inventory = inventory_import_sources(store, reinventory_sources.clone(), false)
            .context("re-inventory search refresh sources after import progress")?;
        let changes = inventory_failures.reconcile(&inventory.sources, inventory.failures);
        for failure in changes.removed {
            totals.remove_source_failure(&failure.stats);
        }
        for failure in changes.added {
            totals.add_source_failure(&failure.stats);
        }
        for failure in changes.newly_failed {
            first_refresh_failure.get_or_insert_with(|| failure.error.clone());
            progress.warning(format!(
                "skipped {} during inventory: {}",
                failure.source.provider.as_str(),
                one_line_error(&failure.error)
            ));
        }
        plan = ImportPlan::build(store, inventory.sources)?;
        *failed_inventory_pending = inventory_failures.pending_counts(store, &plan)?;
        execution_state = crate::commands::import::ImportExecutionState::for_plan(&plan);
        pass_made_durable_progress = false;
        pass_retryable_blocker = !inventory_failures.is_empty();
        installed_mixed_inventory_plan = true;
    }
}

fn all_refresh_sources_failed(source_count: usize, totals: &ImportTotals) -> bool {
    source_count > 0 && totals.imported_sources == 0 && totals.failed_sources >= source_count
}

fn reported_pending_counts(store: &Store, plan: &ImportPlan) -> Result<(usize, usize)> {
    if !store.import_pending_work_is_ready()? {
        return Ok((0, 1));
    }
    let (fresh, mut recovery) = plan.pending_counts(store)?;
    if fresh == 0 && recovery == 0 && store.has_pending_provider_file_publications()? {
        recovery = 1;
    }
    Ok((fresh, recovery))
}

fn refresh_failure_with_totals(
    error: anyhow::Error,
    store: &Store,
    plan: &ImportPlan,
    mut totals: ImportTotals,
) -> anyhow::Error {
    if let Ok((fresh, recovery)) = reported_pending_counts(store, plan) {
        totals.fresh_units_pending = totals.fresh_units_pending.saturating_add(fresh);
        totals.recovery_units_pending = totals.recovery_units_pending.saturating_add(recovery);
    }
    anyhow::Error::new(SearchRefreshFailure { error, totals })
}

#[allow(clippy::too_many_arguments)]
fn execute_search_refresh_plan_class(
    store: &mut Store,
    plan: &ImportPlan,
    execution_state: &mut crate::commands::import::ImportExecutionState,
    class: ImportWorkClass,
    remaining_units: usize,
    max_slices: Option<usize>,
    progress: &ProgressReporter,
    json_output: bool,
    tolerate_source_errors: bool,
    totals: &mut ImportTotals,
    first_refresh_failure: &mut Option<String>,
    imported_sources: &mut BTreeSet<ImportSourceIdentity>,
    failed_sources: &mut BTreeSet<ImportSourceIdentity>,
    drain_retirements: bool,
) -> Result<SearchClassExecution> {
    execute_search_refresh_plan_class_tracked(
        store,
        plan,
        execution_state,
        class,
        remaining_units,
        max_slices,
        progress,
        json_output,
        tolerate_source_errors,
        totals,
        first_refresh_failure,
        imported_sources,
        failed_sources,
        drain_retirements,
        || {},
    )
}

#[derive(Debug)]
struct SearchClassExecution {
    result: crate::commands::import::ImportExecutionResult,
    made_durable_progress: bool,
    retryable_blocker: bool,
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn execute_search_refresh_plan_class_with_pre_lock_hook(
    store: &mut Store,
    plan: &ImportPlan,
    execution_state: &mut crate::commands::import::ImportExecutionState,
    class: ImportWorkClass,
    remaining_units: usize,
    max_slices: Option<usize>,
    progress: &ProgressReporter,
    json_output: bool,
    tolerate_source_errors: bool,
    totals: &mut ImportTotals,
    first_refresh_failure: &mut Option<String>,
    imported_sources: &mut BTreeSet<ImportSourceIdentity>,
    failed_sources: &mut BTreeSet<ImportSourceIdentity>,
    drain_retirements: bool,
    before_bulk_lock: impl FnMut(),
) -> Result<crate::commands::import::ImportExecutionResult> {
    Ok(execute_search_refresh_plan_class_tracked(
        store,
        plan,
        execution_state,
        class,
        remaining_units,
        max_slices,
        progress,
        json_output,
        tolerate_source_errors,
        totals,
        first_refresh_failure,
        imported_sources,
        failed_sources,
        drain_retirements,
        before_bulk_lock,
    )?
    .result)
}

#[allow(clippy::too_many_arguments)]
fn execute_search_refresh_plan_class_tracked(
    store: &mut Store,
    plan: &ImportPlan,
    execution_state: &mut crate::commands::import::ImportExecutionState,
    class: ImportWorkClass,
    mut remaining_units: usize,
    max_slices: Option<usize>,
    progress: &ProgressReporter,
    json_output: bool,
    tolerate_source_errors: bool,
    totals: &mut ImportTotals,
    first_refresh_failure: &mut Option<String>,
    imported_sources: &mut BTreeSet<ImportSourceIdentity>,
    failed_sources: &mut BTreeSet<ImportSourceIdentity>,
    drain_retirements: bool,
    mut before_bulk_lock: impl FnMut(),
) -> Result<SearchClassExecution> {
    let mut completed_bytes = 0u64;
    let mut completed_slices = 0usize;
    let mut execution_result = crate::commands::import::ImportExecutionResult::default();
    let mut made_durable_progress = false;
    let mut retryable_blocker = false;
    while remaining_units > 0 && max_slices.is_none_or(|limit| completed_slices < limit) {
        let Some(executable) = plan
            .select_slice_for_execution_with_pre_lock_hook(
                store,
                class,
                remaining_units,
                execution_state,
                &mut before_bulk_lock,
            )
            .context("select locked search refresh slice")?
        else {
            break;
        };
        let ExecutableImportSlice {
            slice,
            bulk_guard,
            validation_failures,
        } = executable;
        if slice.is_empty() && validation_failures.is_empty() {
            store.finish_event_search_bulk_mode(&bulk_guard)?;
            break;
        }
        let validation_units = validation_failures
            .iter()
            .map(|failure| failure.stats.files)
            .sum::<usize>();
        let selected_units = slice.units.saturating_add(validation_units);
        remaining_units = remaining_units.saturating_sub(selected_units);
        completed_slices += 1;
        let mut system_error = None;
        let mut completed_units = 0usize;
        let mut deferred_units = 0usize;
        let mut maintenance_progress = false;
        let mut source_durable_progress = false;
        let mut stop_admission = false;
        for validation_failure in validation_failures {
            if !tolerate_source_errors {
                system_error = Some(validation_failure.error);
                break;
            }
            let source_plan = &plan.sources[validation_failure.source_index];
            let error = error_summary(&validation_failure.error);
            first_refresh_failure.get_or_insert_with(|| error.clone());
            let first_source_result =
                failed_sources.insert(ImportSourceIdentity::new(&source_plan.source));
            add_refresh_source_failure(
                totals,
                &validation_failure.stats,
                &validation_failure.error,
            );
            if !first_source_result {
                totals.failed_sources = totals.failed_sources.saturating_sub(1);
            }
            progress.done(
                "refreshing",
                format!(
                    "skipped {}: {}",
                    source_plan.source.provider.as_str(),
                    one_line_error(&error)
                ),
                completed_bytes,
            );
        }
        for retirement in &slice.retirements {
            if system_error.is_some() {
                break;
            }
            execution_state.record_retirement_attempt(retirement);
            progress.message("repairing", "repairing prior hidden provider history");
            match recover_provider_file_publication_retirement(store, retirement, drain_retirements)
            {
                Ok(outcome) => {
                    maintenance_progress |= outcome.made_durable_progress;
                    if outcome.completed {
                        completed_units = completed_units.saturating_add(1);
                    }
                    for warning in outcome.maintenance_warnings {
                        progress.warning(warning.to_string());
                    }
                }
                Err(error) => {
                    system_error = Some(error);
                    break;
                }
            }
        }
        for selected in slice.sources {
            if system_error.is_some() {
                break;
            }
            let source_plan = &plan.sources[selected.source_index];
            let (phase, message) = import_work_progress_message(class, source_plan.source.provider);
            progress.message(phase, message);
            let source_progress =
                progress.codex_import_callback(&source_plan.source, completed_bytes);
            execution_state.record_source_attempt(&selected.work);
            if let Err(error) = selected.persist_attempt_started(store) {
                system_error = Some(error);
                break;
            }
            let import_result = import_selected_source(
                store,
                &source_plan.source,
                source_progress,
                &selected.preinventory,
                &selected.work,
            );
            let (outcome, import_error) = match import_result {
                Ok(result) => (Some(result.outcome), result.remaining_error),
                Err(error) => (None, Some(error)),
            };
            let mut outcome_completed_units = 0usize;
            let mut outcome_completed_bytes = 0u64;
            let mut outcome_deferred_units = 0usize;
            let mut outcome_rejected_without_content = false;
            let had_outcome = outcome.is_some();
            if let Some(outcome) = outcome {
                stop_admission |= outcome.stop_admission;
                let made_durable_progress = outcome.made_durable_progress();
                execution_state.record_source_outcome(
                    selected.source_index,
                    &selected.work,
                    outcome.post_import_preinventory.clone(),
                );
                source_durable_progress |= made_durable_progress;
                outcome_completed_units = outcome.completed_units;
                outcome_completed_bytes = outcome.completed_bytes;
                outcome_deferred_units = outcome.deferred_units;
                completed_units = completed_units.saturating_add(outcome.completed_units);
                let deferred = outcome.deferred_units;
                deferred_units = deferred_units.saturating_add(deferred);
                warn_on_rejected_records(
                    progress,
                    json_output,
                    source_plan.source.provider.as_str(),
                    &outcome.summary,
                );
                outcome_rejected_without_content =
                    outcome.summary.failed > 0 && !outcome.summary.has_accepted_content();
                let reportable_no_op = outcome.completed_units == 0
                    && deferred == 0
                    && import_error.is_none()
                    && outcome.summary != ProviderImportSummary::default();
                if outcome.completed_units > 0 || reportable_no_op {
                    let completed_stats = SourceStats {
                        files: if reportable_no_op {
                            selected.stats.files
                        } else {
                            outcome.completed_units
                        },
                        bytes: if reportable_no_op {
                            selected.stats.bytes
                        } else {
                            outcome.completed_bytes
                        },
                        change_token: selected.stats.change_token,
                    };
                    completed_bytes = completed_bytes.saturating_add(completed_stats.bytes);
                    if outcome_rejected_without_content {
                        let first_source_result =
                            failed_sources.insert(ImportSourceIdentity::new(&source_plan.source));
                        totals.add_rejected_source(&outcome.summary, &completed_stats);
                        if !first_source_result {
                            totals.failed_sources = totals.failed_sources.saturating_sub(1);
                        }
                    } else {
                        let first_source_result =
                            imported_sources.insert(ImportSourceIdentity::new(&source_plan.source));
                        totals.add(&outcome.summary, &completed_stats);
                        if !first_source_result {
                            totals.imported_sources = totals.imported_sources.saturating_sub(1);
                        }
                    }
                    let (phase, message) = import_work_progress_done(class, &source_plan.source);
                    progress.done(phase, message, completed_bytes);
                } else if deferred > 0 && !made_durable_progress {
                    progress.done(
                        phase,
                        format!(
                            "Deferred incomplete {} history.",
                            source_plan.source.provider.as_str()
                        ),
                        completed_bytes,
                    );
                }
                if deferred > 0 && !made_durable_progress {
                    progress.warning(format!(
                            "{deferred} {} history unit(s) remain pending until their current write completes.",
                            source_plan.source.provider.as_str()
                        ));
                }
            }
            if let Some(err) = import_error {
                if !had_outcome {
                    execution_state.record_source_outcome(
                        selected.source_index,
                        &selected.work,
                        None,
                    );
                }
                if tolerate_source_errors && import_error_scope(&err) == ImportFailureScope::Source
                {
                    retryable_blocker |= crate::commands::import::import_error_retryability(&err)
                        == crate::commands::import::ImportRetryability::Retryable;
                    if let Some(warning) = publication_recovery_maintenance_warning(&err) {
                        progress.warning(warning.to_string());
                    }
                    let error = error_summary(&err);
                    first_refresh_failure.get_or_insert_with(|| error.clone());
                    let failure_stats = SourceStats {
                        files: selected.stats.files.saturating_sub(
                            outcome_completed_units.saturating_add(outcome_deferred_units),
                        ),
                        bytes: selected.stats.bytes.saturating_sub(outcome_completed_bytes),
                        change_token: selected.stats.change_token,
                    };
                    if !outcome_rejected_without_content {
                        let first_source_result =
                            failed_sources.insert(ImportSourceIdentity::new(&source_plan.source));
                        add_refresh_source_failure(totals, &failure_stats, &err);
                        if !first_source_result {
                            totals.failed_sources = totals.failed_sources.saturating_sub(1);
                        }
                    }
                    progress.done(
                        "refreshing",
                        format!(
                            "skipped {}: {}",
                            source_plan.source.provider.as_str(),
                            one_line_error(&error)
                        ),
                        completed_bytes,
                    );
                } else {
                    if let Some(warning) = publication_recovery_maintenance_warning(&err) {
                        progress.warning(warning.to_string());
                    }
                    system_error = Some(err);
                    break;
                }
            }
            if outcome_deferred_units > 0 && store.has_pending_provider_file_publications()? {
                break;
            }
            if stop_admission {
                break;
            }
        }
        match store.finish_event_search_bulk_mode(&bulk_guard) {
            Ok(ctx_history_store::EventSearchBulkMaintenanceOutcome::Complete) => {}
            Ok(ctx_history_store::EventSearchBulkMaintenanceOutcome::Pending) => {
                stop_admission = true;
            }
            Err(ctx_history_store::StoreError::WalCheckpointBusy { .. }) => {
                stop_admission = true;
            }
            Err(error) => return Err(error).context("finish search refresh bulk mode"),
        }
        match class {
            ImportWorkClass::Fresh => {
                totals.fresh_units_processed =
                    totals.fresh_units_processed.saturating_add(completed_units);
            }
            ImportWorkClass::Recovery => {
                totals.recovery_units_processed = totals
                    .recovery_units_processed
                    .saturating_add(completed_units);
            }
        }
        execution_result.add_slice(
            selected_units,
            completed_units,
            deferred_units,
            maintenance_progress || source_durable_progress,
        );
        made_durable_progress |=
            completed_units > 0 || maintenance_progress || source_durable_progress;
        retryable_blocker |= stop_admission || deferred_units > 0;
        if stop_admission {
            execution_result.stop_admission();
        }
        totals.durable_progress |=
            completed_units > 0 || maintenance_progress || source_durable_progress;
        if let Some(error) = system_error {
            return Err(error);
        }
        if stop_admission {
            break;
        }
        if deferred_units > 0 && store.has_pending_provider_file_publications()? {
            break;
        }
    }
    Ok(SearchClassExecution {
        result: execution_result,
        made_durable_progress,
        retryable_blocker,
    })
}

fn add_refresh_source_failure(
    totals: &mut ImportTotals,
    stats: &SourceStats,
    error: &anyhow::Error,
) {
    if let Some(summary) = rejected_source_summary(error) {
        totals.add_rejected_source(&summary, stats);
    } else {
        totals.add_source_failure(stats);
    }
}

fn warn_on_rejected_records(
    progress: &ProgressReporter,
    json_output: bool,
    source: &str,
    summary: &ProviderImportSummary,
) {
    if summary.failed == 0 {
        return;
    }
    let first_failure = summary
        .failures
        .first()
        .map(|failure| {
            format!(
                "; first failure at line {}: {}",
                failure.line, failure.error
            )
        })
        .unwrap_or_default();
    let warning = format!(
        "refreshed {source} with {} rejected history record(s){first_failure}",
        summary.failed
    );
    if progress.is_enabled() {
        progress.warning(warning);
    } else if !json_output {
        eprintln!("warning: {warning}");
    }
}
