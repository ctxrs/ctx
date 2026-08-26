use super::configured_root_replacement::{
    replacement_route_schedule, DeferredConfiguredRootRetirement,
    PendingConfiguredRootReplacementCohort,
};
use super::*;

#[derive(Clone)]
struct ReplacementCohortAttemptCheckpoint {
    cohort_id: String,
    route_ids: BTreeSet<SourceRouteIdentity>,
    route_indices: BTreeSet<usize>,
    attempt_carried: BTreeSet<SourceRouteIdentity>,
    carried_unselected: BTreeSet<SourceRouteIdentity>,
    successful: BTreeSet<SourceRouteIdentity>,
    partial: BTreeSet<SourceRouteIdentity>,
    failed_routes: BTreeMap<SourceRouteIdentity, SourceBackedFailedRoute>,
    logical_failures: SourceBackedLogicalSourceFailures,
    record_rejections: SourceBackedRecordRejections,
    applied_removals_len: usize,
}

struct ReplacementCohortAttemptState<'a, R> {
    owners: &'a mut HashMap<[u8; 32], SourceOwner>,
    complete_inventory_owners: &'a mut Vec<CompleteInventoryOwner>,
    applied_removals: &'a mut Vec<R>,
    attempt_carried: &'a mut BTreeSet<SourceRouteIdentity>,
    carried_unselected: &'a mut BTreeSet<SourceRouteIdentity>,
    successful: &'a mut BTreeSet<SourceRouteIdentity>,
    partial: &'a mut BTreeSet<SourceRouteIdentity>,
    failed_routes: &'a mut BTreeMap<SourceRouteIdentity, SourceBackedFailedRoute>,
    logical_failures: &'a mut SourceBackedLogicalSourceFailures,
    record_rejections: &'a mut SourceBackedRecordRejections,
}

fn finish_replacement_cohort_attempt<R>(
    lifecycle: &mut IndexCaptureLifecycle,
    registry: &SourceBackedProviderRegistry,
    cohorts: &BTreeMap<String, PendingConfiguredRootReplacementCohort>,
    checkpoint: ReplacementCohortAttemptCheckpoint,
    state: ReplacementCohortAttemptState<'_, R>,
    exact_scan_accounting_valid: &std::sync::atomic::AtomicBool,
) -> SourceBackedCoordinatorResult<()> {
    let cohort = cohorts.get(&checkpoint.cohort_id).ok_or_else(|| {
        SourceBackedCoordinatorError::Index(index_writer_invariant(
            "active configured-root replacement cohort disappeared",
        ))
    })?;
    let cohort_succeeded = cohort.has_scanning_member
        && !cohort.has_unidentified_member
        && !cohort.routes.is_empty()
        && cohort
            .routes
            .iter()
            .all(|route| state.successful.contains(route) && !state.partial.contains(route));
    if cohort_succeeded {
        lifecycle.finish_route_cohort_stage()?;
        return Ok(());
    }

    let mut observed_failures = checkpoint
        .route_ids
        .iter()
        .filter_map(|route| {
            state
                .failed_routes
                .get(route)
                .cloned()
                .map(|failure| (route.clone(), failure))
        })
        .collect::<BTreeMap<_, _>>();
    lifecycle.rollback_route_cohort_stage()?;
    state
        .owners
        .retain(|_, owner| !checkpoint.route_indices.contains(&owner.route_index));
    state
        .complete_inventory_owners
        .retain(|owner| !checkpoint.route_indices.contains(&owner.route_index));
    state
        .applied_removals
        .truncate(checkpoint.applied_removals_len);
    *state.attempt_carried = checkpoint.attempt_carried;
    *state.carried_unselected = checkpoint.carried_unselected;
    *state.successful = checkpoint.successful;
    *state.partial = checkpoint.partial;
    *state.failed_routes = checkpoint.failed_routes;
    *state.logical_failures = checkpoint.logical_failures;
    *state.record_rejections = checkpoint.record_rejections;
    exact_scan_accounting_valid.store(false, std::sync::atomic::Ordering::SeqCst);

    for route_index in checkpoint.route_indices {
        let route = registry.routes.get(route_index).ok_or_else(|| {
            SourceBackedCoordinatorError::Index(index_writer_invariant(
                "configured-root replacement route index disappeared",
            ))
        })?;
        let route_identity = route.metadata.route_identity.as_ref().ok_or_else(|| {
            SourceBackedCoordinatorError::Index(index_writer_invariant(
                "configured-root replacement route lost its identity",
            ))
        })?;
        let carried_forward = lifecycle.carry_failed_route(route_identity)?;
        state.attempt_carried.insert(route_identity.clone());
        let mut failure = match observed_failures.remove(route_identity) {
            Some(failure) => failure,
            None => source_backed_failed_route_from_route(
                route,
                SourceBackedSourceFailureClass::Unavailable,
                carried_forward,
                "configured-root replacement cohort did not complete",
            )?,
        };
        failure.carried_forward = carried_forward;
        state.failed_routes.insert(route_identity.clone(), failure);
    }
    Ok(())
}

pub(super) struct ScheduledRouteExecution<'a, Progress> {
    pub(super) lifecycle: &'a mut IndexCaptureLifecycle,
    pub(super) registry: &'a SourceBackedProviderRegistry,
    pub(super) plan: &'a SourceBackedRefreshPlan,
    pub(super) attempt_selected: &'a BTreeSet<SourceRouteIdentity>,
    pub(super) attempt_carried: BTreeSet<SourceRouteIdentity>,
    pub(super) pending_configured_root_replacement_cohorts:
        &'a BTreeMap<String, PendingConfiguredRootReplacementCohort>,
    pub(super) install_provider_roots: bool,
    pub(super) automatic_retirements: &'a BTreeMap<SourceRouteIdentity, Vec<SourceRouteIdentity>>,
    pub(super) deferred_configured_root_retirements: &'a [DeferredConfiguredRootRetirement],
    pub(super) base_route_controls: &'a BTreeMap<SourceRouteIdentity, Vec<u8>>,
    pub(super) work_budget: usize,
    pub(super) carried_unselected_route_ids: &'a mut BTreeSet<SourceRouteIdentity>,
    pub(super) failed_routes: &'a mut BTreeMap<SourceRouteIdentity, SourceBackedFailedRoute>,
    pub(super) logical_source_failures: &'a mut SourceBackedLogicalSourceFailures,
    pub(super) record_rejections: &'a mut SourceBackedRecordRejections,
    pub(super) attempt_history_progress: &'a SharedAttemptHistoryProgress,
    pub(super) exact_scan_accounting: &'a std::cell::RefCell<AttemptExactScanAccounting>,
    pub(super) exact_scan_accounting_valid: &'a Arc<std::sync::atomic::AtomicBool>,
    pub(super) providers: &'a Vec<CaptureProvider>,
    pub(super) scanned_routes: usize,
    pub(super) discovery_duration: Duration,
    pub(super) refresh_started: &'a Instant,
    pub(super) scan_started: &'a Instant,
    pub(super) report_progress: &'a mut Progress,
}

pub(super) struct ScheduledRouteExecutionOutcome {
    pub(super) owners: HashMap<[u8; 32], SourceOwner>,
    pub(super) complete_inventory_owners: Vec<CompleteInventoryOwner>,
    pub(super) partial_routes: BTreeSet<SourceRouteIdentity>,
    pub(super) applied_removals: Vec<SourceBackedCertifiedRemoval>,
    pub(super) successful_this_attempt: BTreeSet<SourceRouteIdentity>,
    pub(super) completed_routes: usize,
    pub(super) attempt_carried: BTreeSet<SourceRouteIdentity>,
}

pub(super) fn execute_scheduled_routes<Progress>(
    execution: ScheduledRouteExecution<'_, Progress>,
) -> SourceBackedCoordinatorResult<ScheduledRouteExecutionOutcome>
where
    Progress: FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
{
    let ScheduledRouteExecution {
        lifecycle,
        registry,
        plan,
        attempt_selected,
        mut attempt_carried,
        pending_configured_root_replacement_cohorts,
        install_provider_roots,
        automatic_retirements,
        deferred_configured_root_retirements,
        base_route_controls,
        work_budget,
        carried_unselected_route_ids,
        failed_routes,
        logical_source_failures,
        record_rejections,
        attempt_history_progress,
        exact_scan_accounting,
        exact_scan_accounting_valid,
        providers,
        scanned_routes,
        discovery_duration,
        refresh_started,
        scan_started,
        report_progress,
    } = execution;

    let automatic_missing_observed_at_unix_ms = source_missing_observation_time();
    let mut owners = HashMap::new();
    let mut complete_inventory_owners = Vec::new();
    let mut partial_routes = BTreeSet::new();
    let mut applied_removals = Vec::new();
    let mut successful_this_attempt = BTreeSet::new();
    let mut completed_routes = 0;
    for route in registry.routes.iter().filter(|route| {
        matches!(
            route.metadata.source.status,
            ProviderSourceStatus::Missing | ProviderSourceStatus::Unknown
        ) && route.driver.is_none()
            && route.certified_missing_paths.is_empty()
            && route.metadata.route_identity.is_some()
    }) {
        let route_identity = route
            .metadata
            .route_identity
            .as_ref()
            .expect("configured unavailable route identity");
        if !attempt_selected.contains(route_identity) {
            continue;
        }
        let carried_forward = lifecycle.carry_failed_route(route_identity)?;
        attempt_carried.insert(route_identity.clone());
        failed_routes.insert(
            route_identity.clone(),
            source_backed_failed_route_from_route(
                route,
                SourceBackedSourceFailureClass::Unavailable,
                carried_forward,
                route
                    .metadata
                    .unsupported_reason
                    .as_deref()
                    .unwrap_or("configured route is unavailable"),
            )?,
        );
    }
    let no_replacement_cohorts = BTreeMap::new();
    let publication_replacement_cohorts = if install_provider_roots {
        pending_configured_root_replacement_cohorts
    } else {
        &no_replacement_cohorts
    };
    let route_schedule =
        replacement_route_schedule(registry, attempt_selected, publication_replacement_cohorts);
    let mut active_replacement_cohort = None::<ReplacementCohortAttemptCheckpoint>;
    for scheduled in route_schedule {
        let route_index = scheduled.route_index;
        let route = registry.routes.get(route_index).ok_or_else(|| {
            SourceBackedCoordinatorError::Index(index_writer_invariant(
                "scheduled source route index disappeared",
            ))
        })?;
        let route_identity = route.metadata.route_identity.as_ref().ok_or_else(|| {
            SourceBackedCoordinatorError::Index(index_writer_invariant(
                "scheduled source route lost its identity",
            ))
        })?;
        if let Some(cohort_id) = scheduled.cohort_id.as_ref() {
            if active_replacement_cohort.is_none() {
                let cohort = publication_replacement_cohorts
                    .get(cohort_id)
                    .ok_or_else(|| {
                        SourceBackedCoordinatorError::Index(index_writer_invariant(
                            "scheduled configured-root replacement cohort disappeared",
                        ))
                    })?;
                let route_indices = registry
                    .routes
                    .iter()
                    .enumerate()
                    .filter_map(|(candidate_index, candidate)| {
                        let candidate_identity = candidate.metadata.route_identity.as_ref()?;
                        (attempt_selected.contains(candidate_identity)
                            && cohort.routes.contains(candidate_identity)
                            && (candidate.driver.is_some()
                                || !candidate.certified_missing_paths.is_empty()))
                        .then_some(candidate_index)
                    })
                    .collect::<BTreeSet<_>>();
                let route_ids = route_indices
                    .iter()
                    .filter_map(|candidate_index| {
                        registry.routes[*candidate_index]
                            .metadata
                            .route_identity
                            .clone()
                    })
                    .collect();
                lifecycle.begin_route_cohort_stage(route_identity.clone())?;
                active_replacement_cohort = Some(ReplacementCohortAttemptCheckpoint {
                    cohort_id: cohort_id.clone(),
                    route_ids,
                    route_indices,
                    attempt_carried: attempt_carried.clone(),
                    carried_unselected: carried_unselected_route_ids.clone(),
                    successful: successful_this_attempt.clone(),
                    partial: partial_routes.clone(),
                    failed_routes: failed_routes.clone(),
                    logical_failures: logical_source_failures.clone(),
                    record_rejections: record_rejections.clone(),
                    applied_removals_len: applied_removals.len(),
                });
            }
        }
        if route.driver.is_none() {
            lifecycle.begin_route_stage(route_identity.clone())?;
            let history_progress = attempt_history_progress.snapshot();
            report_progress(source_level_progress(SourceBackedRefreshProgress {
                phase: "verifying",
                completed_sources: completed_routes,
                total_sources: scanned_routes,
                current_source: None,
                completed_records: None,
                completed_bytes: None,
                providers: providers.clone(),
                processed_sessions: history_progress.processed_sessions,
                processed_messages: history_progress.processed_messages,
                processed_tool_calls: history_progress.processed_tool_calls,
                processed_bytes: history_progress.processed_bytes,
                stage_duration: scan_started.elapsed(),
                elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
                certified_source_count: None,
                certified_source_bytes: None,
            }))
            .map_err(SourceBackedCoordinatorError::Progress)?;
            let paths = route.certified_missing_paths.clone();
            if paths
                .iter()
                .all(|path| path_presence(path) == PathPresence::Missing)
            {
                let accounting_valid = Arc::clone(exact_scan_accounting_valid);
                lifecycle.observe_missing_route(
                    route_identity.clone(),
                    automatic_missing_observed_at_unix_ms,
                    move || {
                        let valid = paths
                            .iter()
                            .all(|path| path_presence(path) == PathPresence::Missing);
                        if !valid {
                            accounting_valid.store(false, std::sync::atomic::Ordering::SeqCst);
                        }
                        valid
                    },
                )?;
                lifecycle.finish_route_stage(route_identity)?;
                successful_this_attempt.insert(route_identity.clone());
            } else {
                exact_scan_accounting.borrow_mut().revoke();
                exact_scan_accounting_valid.store(false, std::sync::atomic::Ordering::SeqCst);
                lifecycle.rollback_route_stage(route_identity)?;
                let carried_forward = lifecycle.carry_failed_route(route_identity)?;
                attempt_carried.insert(route_identity.clone());
                failed_routes.insert(
                    route_identity.clone(),
                    source_backed_failed_route_from_route(
                        route,
                        SourceBackedSourceFailureClass::SourceChanged,
                        carried_forward,
                        "certified-missing route changed during terminal verification",
                    )?,
                );
            }
            if scheduled.cohort_last {
                let checkpoint = active_replacement_cohort.take().ok_or_else(|| {
                    SourceBackedCoordinatorError::Index(index_writer_invariant(
                        "configured-root replacement cohort lost its attempt checkpoint",
                    ))
                })?;
                finish_replacement_cohort_attempt(
                    &mut *lifecycle,
                    registry,
                    publication_replacement_cohorts,
                    checkpoint,
                    ReplacementCohortAttemptState {
                        owners: &mut owners,
                        complete_inventory_owners: &mut complete_inventory_owners,
                        applied_removals: &mut applied_removals,
                        attempt_carried: &mut attempt_carried,
                        carried_unselected: &mut *carried_unselected_route_ids,
                        successful: &mut successful_this_attempt,
                        partial: &mut partial_routes,
                        failed_routes: &mut *failed_routes,
                        logical_failures: &mut *logical_source_failures,
                        record_rejections: &mut *record_rejections,
                    },
                    exact_scan_accounting_valid,
                )?;
            }
            continue;
        }
        let driver = route
            .driver
            .as_ref()
            .expect("scheduled scanned source route driver");
        exact_scan_accounting.borrow_mut().begin_route();
        attempt_history_progress.reset_parallel_byte_debt();
        let history_progress = attempt_history_progress.snapshot();
        report_progress(source_level_progress(SourceBackedRefreshProgress {
            phase: "refreshing",
            completed_sources: completed_routes,
            total_sources: scanned_routes,
            current_source: Some(route.metadata.source.path.display().to_string()),
            completed_records: Some(0),
            completed_bytes: Some(0),
            providers: providers.clone(),
            processed_sessions: history_progress.processed_sessions,
            processed_messages: history_progress.processed_messages,
            processed_tool_calls: history_progress.processed_tool_calls,
            processed_bytes: history_progress.processed_bytes,
            stage_duration: scan_started.elapsed(),
            elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
            certified_source_count: None,
            certified_source_bytes: None,
        }))
        .map_err(SourceBackedCoordinatorError::Progress)?;
        lifecycle.begin_route_stage(route_identity.clone())?;
        if let Some(revalidate) = driver.revalidate_at_publication.as_ref() {
            let revalidate = Arc::clone(revalidate);
            let accounting_valid = Arc::clone(exact_scan_accounting_valid);
            lifecycle.register_route_revalidation(route_identity.clone(), move || {
                let valid = revalidate();
                if !valid {
                    accounting_valid.store(false, std::sync::atomic::Ordering::SeqCst);
                }
                valid
            })?;
        }
        let removal_checkpoint = applied_removals.len();
        let logical_failure_checkpoint = logical_source_failures.checkpoint(route_identity.clone());
        let record_rejection_checkpoint = record_rejections.checkpoint();
        let current_source = route.metadata.source.path.display().to_string();
        let record_progress = std::cell::RefCell::new(SourceRecordProgress::default());
        let progress_failure = std::cell::RefCell::new(None::<SourceBackedRouteError>);
        let automatic_route_retirements = automatic_retirements
            .get(route_identity)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let deferred_retirements = deferred_configured_root_retirements
            .iter()
            .filter(|retirement| &retirement.owner == route_identity)
            .map(|retirement| retirement.predecessor.clone())
            .collect::<BTreeSet<_>>();
        for retired_route in route
            .retire_after_success
            .iter()
            .filter(|retired_route| !deferred_retirements.contains(*retired_route))
            .chain(automatic_route_retirements)
        {
            lifecycle.authorize_carried_route_retirement(route_identity, retired_route)?;
        }
        let scan_result = {
            let progress_callback = std::cell::RefCell::new(&mut *report_progress);
            let mut report_record_progress = |delta| {
                if let Some(error) = progress_failure.borrow().as_ref() {
                    return Err(SourceBackedCoordinatorError::Progress(error.clone()));
                }
                exact_scan_accounting.borrow_mut().observe(&delta);
                attempt_history_progress.advance_coordinator(&delta);
                let Some(source_progress) = record_progress.borrow_mut().advanced_at(
                    delta,
                    Instant::now(),
                    SOURCE_RECORD_PROGRESS_INTERVAL,
                ) else {
                    return Ok(());
                };
                let history_progress = attempt_history_progress.snapshot();
                match progress_callback.borrow_mut()(source_level_progress(
                    SourceBackedRefreshProgress {
                        phase: "refreshing",
                        completed_sources: completed_routes,
                        total_sources: scanned_routes,
                        current_source: Some(current_source.clone()),
                        completed_records: Some(source_progress.completed_records),
                        completed_bytes: Some(source_progress.completed_bytes),
                        providers: providers.clone(),
                        processed_sessions: history_progress.processed_sessions,
                        processed_messages: history_progress.processed_messages,
                        processed_tool_calls: history_progress.processed_tool_calls,
                        processed_bytes: history_progress.processed_bytes,
                        stage_duration: scan_started.elapsed(),
                        elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
                        certified_source_count: None,
                        certified_source_bytes: None,
                    },
                )) {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        progress_failure.replace(Some(error.clone()));
                        Err(SourceBackedCoordinatorError::Progress(error))
                    }
                }
            };
            let mut report_current_source_progress = |current_source_progress| {
                if let Some(error) = progress_failure.borrow().as_ref() {
                    return Err(error.clone());
                }
                let history_progress = attempt_history_progress.snapshot();
                match progress_callback.borrow_mut()(SourceBackedDetailedRefreshProgress {
                    progress: SourceBackedRefreshProgress {
                        phase: "refreshing",
                        completed_sources: completed_routes,
                        total_sources: scanned_routes,
                        current_source: Some(current_source.clone()),
                        completed_records: None,
                        completed_bytes: None,
                        providers: providers.clone(),
                        processed_sessions: history_progress.processed_sessions,
                        processed_messages: history_progress.processed_messages,
                        processed_tool_calls: history_progress.processed_tool_calls,
                        processed_bytes: history_progress.processed_bytes,
                        stage_duration: scan_started.elapsed(),
                        elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
                        certified_source_count: None,
                        certified_source_bytes: None,
                    },
                    current_source_progress: Some(current_source_progress),
                    exact_scan_progress: None,
                }) {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        progress_failure.replace(Some(error.clone()));
                        Err(error)
                    }
                }
            };
            let core_record_preparer = lifecycle.core_preparation();
            let mut sink = SourceBackedGenerationSink {
                lifecycle: &mut *lifecycle,
                core_record_preparer,
                owners: &mut owners,
                complete_inventories: &mut complete_inventory_owners,
                applied_removals: &mut applied_removals,
                route_index,
                route_identity: route_identity.clone(),
                base_route_aliases: route.base_route_aliases.clone(),
                base_route_control: base_route_controls.get(route_identity).cloned(),
                resources: plan.route_resources_for(route_identity, work_budget),
                logical_source_failures: &mut *logical_source_failures,
                record_rejections: &mut *record_rejections,
                record_progress: Some(&mut report_record_progress),
                current_source_progress: Some(&mut report_current_source_progress),
                intermediate_progress_last_emitted_at: None,
                intermediate_progress_pending_stage: None,
                last_progress_session_id: None,
                exact_scan_total_bytes: None,
                exact_scan_accounting_enabled: false,
            };
            (driver.scan)(&mut sink)
        };
        let mut record_progress = record_progress.into_inner();
        if let Some(error) = progress_failure.into_inner() {
            return Err(SourceBackedCoordinatorError::Progress(error));
        }
        if let Some(source_progress) = record_progress.flush_at(Instant::now()) {
            let history_progress = attempt_history_progress.snapshot();
            report_progress(source_level_progress(SourceBackedRefreshProgress {
                phase: "refreshing",
                completed_sources: completed_routes,
                total_sources: scanned_routes,
                current_source: Some(current_source),
                completed_records: Some(source_progress.completed_records),
                completed_bytes: Some(source_progress.completed_bytes),
                providers: providers.clone(),
                processed_sessions: history_progress.processed_sessions,
                processed_messages: history_progress.processed_messages,
                processed_tool_calls: history_progress.processed_tool_calls,
                processed_bytes: history_progress.processed_bytes,
                stage_duration: scan_started.elapsed(),
                elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
                certified_source_count: None,
                certified_source_bytes: None,
            }))
            .map_err(SourceBackedCoordinatorError::Progress)?;
        }
        let terminal_route_for_eta = exact_scan_accounting
            .borrow_mut()
            .finish_route(scan_result.is_ok());
        if scan_result.is_err() {
            // Joined workers retain failed-attempt facts, while later
            // routes cannot consume their byte debt.
            attempt_history_progress.reset_parallel_byte_debt();
        } else if attempt_history_progress.parallel_byte_debt() != 0 {
            return Err(SourceBackedCoordinatorError::RouteScan {
                provider: route.metadata.source.provider,
                source: SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "parallel scanner byte debt was not reconciled before route completion",
                ),
            });
        }
        match scan_result {
            Ok(()) => {
                let replacement_control_identity = if route
                    .controlled_retire_after_success
                    .is_empty()
                {
                    None
                } else {
                    driver
                        .publication_control
                        .as_ref()
                        .map(|control| {
                            control().map_err(|source| SourceBackedCoordinatorError::RouteScan {
                                provider: route.metadata.source.provider,
                                source,
                            })
                        })
                        .transpose()?
                        .flatten()
                        .as_deref()
                        .and_then(|control| {
                            driver
                                .route_control_expectation
                                .as_ref()
                                .and_then(|expectation| expectation.retirement_identity(control))
                        })
                };
                let dynamic_retirements = route
                    .controlled_retire_after_success
                    .iter()
                    .filter(|candidate| {
                        Some(candidate.expected_identity) == replacement_control_identity
                    })
                    .map(|candidate| candidate.route_identity.clone())
                    .collect::<Vec<_>>();
                for retired_route in &dynamic_retirements {
                    lifecycle.authorize_carried_route_retirement(route_identity, retired_route)?;
                }
                let route_is_partial = lifecycle.route_retains_unstaged_members(route_identity);
                capture_staged_source_route_revalidation_receipts(
                    &*lifecycle,
                    route_index,
                    &mut owners,
                )?;
                let history_progress = attempt_history_progress.snapshot();
                report_progress(source_level_progress(SourceBackedRefreshProgress {
                    phase: "verifying",
                    completed_sources: completed_routes,
                    total_sources: scanned_routes,
                    current_source: None,
                    completed_records: None,
                    completed_bytes: None,
                    providers: providers.clone(),
                    processed_sessions: history_progress.processed_sessions,
                    processed_messages: history_progress.processed_messages,
                    processed_tool_calls: history_progress.processed_tool_calls,
                    processed_bytes: history_progress.processed_bytes,
                    stage_duration: scan_started.elapsed(),
                    elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
                    certified_source_count: None,
                    certified_source_bytes: None,
                }))
                .map_err(SourceBackedCoordinatorError::Progress)?;
                if revalidate_staged_source_route(
                    route.metadata.source.provider,
                    route_index,
                    driver,
                    &owners,
                    &complete_inventory_owners,
                )? {
                    let ready_deferred_retirements = if route_is_partial {
                        BTreeSet::new()
                    } else {
                        deferred_configured_root_retirements
                            .iter()
                            .filter(|retirement| {
                                &retirement.owner == route_identity
                                    && retirement.cohort_complete
                                    && retirement.cohort.iter().all(|member| {
                                        member == route_identity
                                            || (successful_this_attempt.contains(member)
                                                && !partial_routes.contains(member))
                                    })
                            })
                            .map(|retirement| retirement.predecessor.clone())
                            .collect::<BTreeSet<_>>()
                    };
                    for retired_route in &ready_deferred_retirements {
                        lifecycle
                            .authorize_carried_route_retirement(route_identity, retired_route)?;
                    }
                    for retired_route in route
                        .retire_after_success
                        .iter()
                        .filter(|retired_route| !deferred_retirements.contains(*retired_route))
                        .chain(automatic_route_retirements)
                        .chain(&dynamic_retirements)
                        .chain(&ready_deferred_retirements)
                    {
                        let retired_sources =
                            lifecycle.retire_carried_route(route_identity, retired_route)?;
                        attempt_carried.remove(retired_route);
                        carried_unselected_route_ids.remove(retired_route);
                        for source in retired_sources {
                            let digest = source.identity().digest();
                            match owners.entry(digest) {
                                std::collections::hash_map::Entry::Vacant(entry) => {
                                    entry.insert(SourceOwner {
                                        route_index,
                                        source,
                                        present: false,
                                        revalidation: None,
                                    });
                                }
                                std::collections::hash_map::Entry::Occupied(entry)
                                    if entry.get().route_index == route_index
                                        && entry.get().source.exact_descriptor_eq(&source) => {}
                                std::collections::hash_map::Entry::Occupied(_) => {
                                    return Err(
                                        SourceBackedCoordinatorError::DuplicateSourceOwner {
                                            source_id: source.identity().to_string(),
                                        },
                                    );
                                }
                            }
                        }
                    }
                    lifecycle.finish_route_stage(route_identity)?;
                    if route_is_partial {
                        partial_routes.insert(route_identity.clone());
                    }
                    successful_this_attempt.insert(route_identity.clone());
                } else {
                    if !terminal_route_for_eta {
                        exact_scan_accounting.borrow_mut().revoke();
                        exact_scan_accounting_valid
                            .store(false, std::sync::atomic::Ordering::SeqCst);
                    }
                    lifecycle.rollback_route_stage(route_identity)?;
                    owners.retain(|_, owner| owner.route_index != route_index);
                    complete_inventory_owners.retain(|owner| owner.route_index != route_index);
                    applied_removals.truncate(removal_checkpoint);
                    logical_source_failures.truncate(logical_failure_checkpoint);
                    record_rejections
                        .truncate(record_rejection_checkpoint.0, record_rejection_checkpoint.1);
                    let carried_forward = lifecycle.carry_failed_route(route_identity)?;
                    attempt_carried.insert(route_identity.clone());
                    failed_routes.insert(
                        route_identity.clone(),
                        source_backed_failed_route_from_route(
                            route,
                            SourceBackedSourceFailureClass::SourceChanged,
                            carried_forward,
                            "source route changed during terminal revalidation",
                        )?,
                    );
                }
            }
            Err(source) => {
                let Some(class) = source.kind.source_failure_class() else {
                    return Err(SourceBackedCoordinatorError::RouteScan {
                        provider: route.metadata.source.provider,
                        source,
                    });
                };
                if !terminal_route_for_eta {
                    exact_scan_accounting.borrow_mut().revoke();
                    exact_scan_accounting_valid.store(false, std::sync::atomic::Ordering::SeqCst);
                }
                lifecycle.rollback_route_stage(route_identity)?;
                owners.retain(|_, owner| owner.route_index != route_index);
                complete_inventory_owners.retain(|owner| owner.route_index != route_index);
                applied_removals.truncate(removal_checkpoint);
                logical_source_failures.truncate(logical_failure_checkpoint);
                record_rejections
                    .truncate(record_rejection_checkpoint.0, record_rejection_checkpoint.1);
                let carried_forward = lifecycle.carry_failed_route(route_identity)?;
                attempt_carried.insert(route_identity.clone());
                failed_routes.insert(
                    route_identity.clone(),
                    source_backed_failed_route_from_route(
                        route,
                        class,
                        carried_forward,
                        &source.detail,
                    )?,
                );
            }
        }
        completed_routes += 1;
        if scheduled.cohort_last {
            let checkpoint = active_replacement_cohort.take().ok_or_else(|| {
                SourceBackedCoordinatorError::Index(index_writer_invariant(
                    "configured-root replacement cohort lost its attempt checkpoint",
                ))
            })?;
            finish_replacement_cohort_attempt(
                &mut *lifecycle,
                registry,
                publication_replacement_cohorts,
                checkpoint,
                ReplacementCohortAttemptState {
                    owners: &mut owners,
                    complete_inventory_owners: &mut complete_inventory_owners,
                    applied_removals: &mut applied_removals,
                    attempt_carried: &mut attempt_carried,
                    carried_unselected: &mut *carried_unselected_route_ids,
                    successful: &mut successful_this_attempt,
                    partial: &mut partial_routes,
                    failed_routes: &mut *failed_routes,
                    logical_failures: &mut *logical_source_failures,
                    record_rejections: &mut *record_rejections,
                },
                exact_scan_accounting_valid,
            )?;
        }
    }

    if active_replacement_cohort.is_some() {
        return Err(SourceBackedCoordinatorError::Index(index_writer_invariant(
            "configured-root replacement cohort did not reach its terminal member",
        )));
    }

    Ok(ScheduledRouteExecutionOutcome {
        owners,
        complete_inventory_owners,
        partial_routes,
        applied_removals,
        successful_this_attempt,
        completed_routes,
        attempt_carried,
    })
}
