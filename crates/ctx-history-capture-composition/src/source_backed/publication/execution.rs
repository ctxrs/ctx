use super::*;

mod configured_root_replacement;
mod route_attempts;
mod route_controls;

use configured_root_replacement::{
    applied_provider_root_config_digest, deferred_configured_root_retirements,
    pending_configured_root_replacement_cohorts, roots_with_pending_configured_root_replacements,
};
use route_attempts::{
    execute_scheduled_routes, ScheduledRouteExecution, ScheduledRouteExecutionOutcome,
};
use route_controls::successful_route_controls;

pub(super) fn refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    execution: SourceBackedRefreshExecutionBudget,
    selection: (
        SourceBackedRefreshPlan,
        &BTreeMap<SourceRouteIdentity, Vec<u8>>,
    ),
    mut emit_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
    mut metadata_factory: Option<&mut SourceBackedPublicationMetadataFactory<'_>>,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    let (plan, base_route_controls) = selection;
    let SourceBackedRefreshExecutionBudget {
        discovery_duration,
        work_budget,
    } = execution;
    let RefreshPrelude {
        selected_route_ids,
        scanned_routes,
        providers,
        unsupported_routes,
    } = prepare_refresh(registry, &plan)?;
    let attempt_history_progress = plan.attempt_history_progress.clone();
    let exact_scan_accounting = std::cell::RefCell::new(AttemptExactScanAccounting::default());
    let exact_scan_accounting_valid = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut report_progress = |mut update: SourceBackedDetailedRefreshProgress| {
        update.exact_scan_progress = exact_scan_accounting_valid
            .load(std::sync::atomic::Ordering::SeqCst)
            .then(|| exact_scan_accounting.borrow().snapshot(scanned_routes))
            .flatten();
        emit_progress(update)
    };
    let refresh_started = Instant::now();
    report_progress(discovery_started_progress(
        scanned_routes,
        providers.clone(),
        discovery_duration,
    ))
    .map_err(SourceBackedCoordinatorError::Progress)?;

    let scan_started = Instant::now();
    let index_root = index_root.as_ref();
    let mut failed_routes = BTreeMap::<SourceRouteIdentity, SourceBackedFailedRoute>::new();
    let mut logical_source_failures = SourceBackedLogicalSourceFailures::default();
    let mut record_rejections = SourceBackedRecordRejections::default();
    let mut carried_unselected_route_ids = BTreeSet::new();

    let mut prepared_successful_route_outcomes = None;
    let (
        commit,
        applied_removals,
        complete_inventory_route_ids,
        commit_duration,
        base_route_content,
        mut route_controls,
        verified_publication,
    ) = {
        let open = IndexCaptureLifecycle::open(index_root, writer_options)?;
        let mut lifecycle = match open {
            CaptureLifecycleOpenOutcome::Ready(lifecycle) => lifecycle,
            CaptureLifecycleOpenOutcome::RecoveryRequired { recovery } => {
                let (generation_id, detail) = recovery.into_parts();
                return Err(
                    SourceBackedCoordinatorError::CommittedPredecessorMigrationRecovery {
                        generation_id,
                        detail,
                    },
                );
            }
        };
        // Exact/watch preserves pinned aliases; cold exact commits admitted definitions.
        let base_snapshot = lifecycle.base_snapshot();
        let base_route_content = source_route_content_fingerprints(base_snapshot.as_ref());
        let base_provider_roots = base_snapshot
            .as_ref()
            .map(|snapshot| snapshot.provider_roots().to_vec())
            .unwrap_or_default();
        let pending_configured_root_replacement_cohorts =
            pending_configured_root_replacement_cohorts(registry, &base_provider_roots);
        let install_provider_roots =
            matches!(&plan.publication_scope, SourceBackedRefreshScope::All)
                || lifecycle.base_snapshot().is_none();
        let requested_provider_roots = provider_roots_for_publication(registry)?;
        let base_route_ids = lifecycle
            .base_snapshot()
            .map(|snapshot| {
                snapshot
                    .source_routes()
                    .map(|route| route.route_identity().clone())
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        let configured_route_ids = configured_provider_root_route_ids(registry);
        if install_provider_roots {
            if let Some((automatic, _digest, roots)) = requested_provider_roots.as_ref() {
                let roots = roots_with_pending_configured_root_replacements(
                    roots.clone(),
                    &base_provider_roots,
                    &pending_configured_root_replacement_cohorts,
                    None,
                )?;
                let digest = applied_provider_root_config_digest(*automatic, &roots);
                lifecycle.set_applied_provider_roots(*automatic, digest, roots)?;
            }
        }
        if matches!(&plan.publication_scope, SourceBackedRefreshScope::All) {
            // Omitted base routes carry unless a typed topology transition
            // retires them; certified-missing routes are already selected.
            carried_unselected_route_ids
                .extend(base_route_ids.difference(&selected_route_ids).cloned());
        }
        let automatic_retirements =
            if matches!(&plan.publication_scope, SourceBackedRefreshScope::All) {
                let retirements = automatic_carried_route_retirements(
                    registry,
                    &selected_route_ids,
                    &base_route_ids,
                )?;
                carried_unselected_route_ids.extend(retirements.values().flatten().cloned());
                retirements
            } else {
                BTreeMap::new()
            };
        let deferred_configured_root_retirements =
            if matches!(&plan.publication_scope, SourceBackedRefreshScope::All) {
                deferred_configured_root_retirements(
                    registry,
                    &base_provider_roots,
                    &pending_configured_root_replacement_cohorts,
                )
            } else {
                Vec::new()
            };
        if matches!(&plan.publication_scope, SourceBackedRefreshScope::All) {
            carried_unselected_route_ids.extend(
                registry
                    .routes
                    .iter()
                    .filter(|route| route.driver.is_some())
                    .flat_map(|route| route.retire_after_success.iter())
                    .filter(|route| {
                        base_route_ids.contains(*route) && !selected_route_ids.contains(*route)
                    })
                    .cloned(),
            );
            carried_unselected_route_ids
                .retain(|route| !registry.provider_root_route_retirements.contains(route));
            lifecycle.set_authorized_topology_route_retirements(
                registry.provider_root_route_retirements.clone(),
            )?;
        }
        if matches!(&plan.publication_scope, SourceBackedRefreshScope::Exact(_))
            && carried_unselected_route_ids.is_empty()
        {
            carried_unselected_route_ids = base_route_ids
                .difference(&selected_route_ids)
                .cloned()
                .collect();
        }
        if let Some(coordinator) = registry.codex_generation.as_ref() {
            let selected_participants = registry
                .routes
                .iter()
                .filter(|route| {
                    route
                        .metadata
                        .route_identity
                        .as_ref()
                        .is_some_and(|identity| selected_route_ids.contains(identity))
                })
                .filter_map(|route| route.codex_generation_participant)
                .collect::<Vec<_>>();
            if !selected_participants.is_empty() {
                coordinator
                    .select(&selected_participants)
                    .map_err(|error| SourceBackedCoordinatorError::RouteScan {
                        provider: CaptureProvider::Codex,
                        source: SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::InvalidSource,
                            error.to_string(),
                        ),
                    })?;
                let needs_exhaustive_catalog = registry.routes.iter().any(|route| {
                    route.codex_generation_participant.is_some()
                        && route
                            .metadata
                            .route_identity
                            .as_ref()
                            .is_some_and(|identity| {
                                selected_route_ids.contains(identity)
                                    && !plan.route_worksets.contains_key(identity)
                            })
                });
                if needs_exhaustive_catalog {
                    coordinator.prepare_selected().map_err(|error| {
                        SourceBackedCoordinatorError::RouteScan {
                            provider: CaptureProvider::Codex,
                            source: SourceBackedRouteError::new(
                                SourceBackedRouteErrorKind::InvalidSource,
                                error.to_string(),
                            ),
                        }
                    })?;
                }
            }
        }
        let attempt_selected = publication_selected_route_ids(
            registry,
            &selected_route_ids,
            &base_route_ids,
            &configured_route_ids,
        );
        let attempt_carried = carried_unselected_route_ids.clone();
        lifecycle.set_route_plan(attempt_selected.clone(), attempt_carried.clone())?;

        let ScheduledRouteExecutionOutcome {
            owners,
            complete_inventory_owners,
            partial_routes,
            applied_removals,
            mut successful_this_attempt,
            completed_routes,
            attempt_carried,
        } = execute_scheduled_routes(ScheduledRouteExecution {
            lifecycle: &mut lifecycle,
            registry,
            plan: &plan,
            attempt_selected: &attempt_selected,
            attempt_carried,
            pending_configured_root_replacement_cohorts:
                &pending_configured_root_replacement_cohorts,
            install_provider_roots,
            automatic_retirements: &automatic_retirements,
            deferred_configured_root_retirements: &deferred_configured_root_retirements,
            base_route_controls,
            work_budget,
            carried_unselected_route_ids: &mut carried_unselected_route_ids,
            failed_routes: &mut failed_routes,
            logical_source_failures: &mut logical_source_failures,
            record_rejections: &mut record_rejections,
            attempt_history_progress: &attempt_history_progress,
            exact_scan_accounting: &exact_scan_accounting,
            exact_scan_accounting_valid: &exact_scan_accounting_valid,
            providers: &providers,
            scanned_routes,
            discovery_duration,
            refresh_started: &refresh_started,
            scan_started: &scan_started,
            report_progress: &mut report_progress,
        })?;

        for route in registry
            .routes
            .iter()
            .filter(|route| !route.certified_missing_paths.is_empty())
        {
            let route_identity = route.metadata.route_identity.as_ref().ok_or_else(|| {
                index_writer_invariant("certified-missing source route has no route identity")
            })?;
            if !selected_route_ids.contains(route_identity) {
                continue;
            }
            if attempt_selected.contains(route_identity) {
                continue;
            }
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
            if !paths
                .iter()
                .all(|path| path_presence(path) == PathPresence::Missing)
            {
                exact_scan_accounting.borrow_mut().revoke();
                exact_scan_accounting_valid.store(false, std::sync::atomic::Ordering::SeqCst);
                failed_routes.insert(
                    route_identity.clone(),
                    source_backed_failed_route_from_route(
                        route,
                        SourceBackedSourceFailureClass::SourceChanged,
                        false,
                        "certified-missing route changed during terminal verification",
                    )?,
                );
                continue;
            }
            successful_this_attempt.insert(route_identity.clone());
        }
        for barrier in &registry.automatic_split_cohort_barriers {
            if barrier.cohort.iter().any(|route| {
                !successful_this_attempt.contains(route) || partial_routes.contains(route)
            }) {
                return Err(SourceBackedCoordinatorError::InvalidRoute {
                    provider: CaptureProvider::Unknown,
                    detail: format!(
                        "automatic split cohort did not terminally succeed before retiring {}",
                        barrier.predecessor.as_str()
                    ),
                });
            }
        }
        if install_provider_roots {
            if let Some((automatic, _digest, roots)) = requested_provider_roots.as_ref() {
                let roots = roots_with_pending_configured_root_replacements(
                    roots.clone(),
                    &base_provider_roots,
                    &pending_configured_root_replacement_cohorts,
                    Some((&successful_this_attempt, &partial_routes)),
                )?;
                let digest = applied_provider_root_config_digest(*automatic, &roots);
                lifecycle.finalize_applied_provider_roots(*automatic, digest, roots)?;
            }
        }
        lifecycle.set_present_routes(registry.routes.iter().enumerate().filter_map(
            |(route_index, route)| {
                let route_identity = route.metadata.route_identity.as_ref()?;
                if route.driver.is_none()
                    || !successful_this_attempt.contains(route_identity)
                    || partial_routes.contains(route_identity)
                {
                    return None;
                }
                let members = owners
                    .values()
                    .filter(|owner| owner.route_index == route_index && owner.present)
                    .map(|owner| owner.source.clone())
                    .collect::<Vec<_>>();
                if members.is_empty()
                    && omit_empty_automatic_route(registry, route_identity, &configured_route_ids)
                {
                    return None;
                }
                Some(PresentCaptureRoute::new(route_identity.clone(), members))
            },
        ))?;

        require_complete_base_source_ownership(
            &lifecycle,
            registry,
            &owners,
            &complete_inventory_owners,
            &attempt_carried,
            &partial_routes,
            &successful_this_attempt,
            &failed_routes,
            &logical_source_failures,
        )?;

        let has_carried_source = lifecycle.base_snapshot().is_some_and(|base| {
            base.source_routes().any(|route| {
                attempt_carried.contains(route.route_identity()) && !route.sources().is_empty()
            })
        });
        let has_successful_retained_source = lifecycle.base_snapshot().is_some_and(|base| {
            base.source_routes().any(|route| {
                successful_this_attempt.contains(route.route_identity())
                    && !route.sources().is_empty()
            })
        });
        let has_successful_source = owners.values().any(|owner| owner.present);
        if (!failed_routes.is_empty() || !logical_source_failures.is_empty())
            && !has_carried_source
            && !has_successful_retained_source
            && !has_successful_source
        {
            if !failed_routes.is_empty() {
                return Err(SourceBackedCoordinatorError::NoUsableSourceRoutes {
                    failed_routes: bounded_source_failures(failed_routes.values()),
                });
            }
            return Err(SourceBackedCoordinatorError::NoUsableLogicalSources {
                failed_sources: logical_source_failures.clone(),
            });
        }

        let history_progress = attempt_history_progress.snapshot();
        report_progress(source_level_progress(SourceBackedRefreshProgress {
            phase: "committing",
            completed_sources: scanned_routes,
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
        let commit_started = Instant::now();
        run_before_source_backed_commit_hook();
        let mut revalidate_source = |target: CaptureRevalidationTarget<'_>| {
            let source = match target {
                CaptureRevalidationTarget::Source(source) => source.observation().source(),
                CaptureRevalidationTarget::Deletion(deletion) => deletion.source(),
            };
            let valid = owners
                .get(&source.identity().digest())
                .filter(|owner| owner.source.exact_descriptor_eq(source))
                .is_some_and(|owner| {
                    matches!(
                        (&owner.revalidation, target),
                        (
                            Some(SourceBackedRouteRevalidation::Source(expected)),
                            CaptureRevalidationTarget::Source(actual)
                        ) if *expected == *actual
                    ) || matches!(
                        (&owner.revalidation, target),
                        (
                            Some(SourceBackedRouteRevalidation::Deletion(expected)),
                            CaptureRevalidationTarget::Deletion(actual)
                        ) if expected.as_ref() == actual
                    )
                });
            if !valid {
                exact_scan_accounting_valid.store(false, std::sync::atomic::Ordering::SeqCst);
            }
            valid
        };
        let mut revalidate_inventory = |inventory: &CertifiedSourceInventory| {
            let valid = complete_inventory_owners
                .iter()
                .any(|owner| owner.inventory == *inventory);
            if !valid {
                exact_scan_accounting_valid.store(false, std::sync::atomic::Ordering::SeqCst);
            }
            valid
        };
        let complete_inventory_route_ids = complete_inventory_owners
            .iter()
            .filter_map(|owner| {
                registry
                    .routes
                    .get(owner.route_index)
                    .and_then(|route| route.metadata.route_identity.clone())
            })
            .chain(
                registry
                    .routes
                    .iter()
                    .filter(|route| !route.certified_missing_paths.is_empty())
                    .filter_map(|route| route.metadata.route_identity.as_ref())
                    .filter(|route_identity| successful_this_attempt.contains(*route_identity))
                    .cloned(),
            )
            .collect::<BTreeSet<_>>();
        let mut report_publication_stage = |stage: PublicationStage| {
            report_progress(source_level_progress(SourceBackedRefreshProgress {
                phase: stage.as_str(),
                completed_sources: scanned_routes,
                total_sources: scanned_routes,
                current_source: None,
                completed_records: None,
                completed_bytes: None,
                providers: providers.clone(),
                processed_sessions: history_progress.processed_sessions,
                processed_messages: history_progress.processed_messages,
                processed_tool_calls: history_progress.processed_tool_calls,
                processed_bytes: history_progress.processed_bytes,
                stage_duration: commit_started.elapsed(),
                elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
                certified_source_count: None,
                certified_source_bytes: None,
            }))
            .map_err(|error| {
                IndexError::PublicationMetadata(format!(
                    "persist pre-publication progress: {error}"
                ))
            })
        };
        let route_controls =
            successful_route_controls(registry, &successful_this_attempt, base_route_controls)?;
        let (commit, verified_publication) = if let Some(factory) = metadata_factory.as_mut() {
            let published = lifecycle.commit_with_metadata_and_progress(
                &mut revalidate_source,
                &mut revalidate_inventory,
                |publication| {
                    let mut live_route_controls = route_controls.clone();
                    live_route_controls
                        .retain(|route, _| publication.snapshot().source_route(route).is_some());
                    let outcomes = successful_route_outcomes_for_snapshot(
                        &selected_route_ids,
                        &failed_routes,
                        &logical_source_failures,
                        &base_route_content,
                        publication.snapshot(),
                    );
                    prepared_successful_route_outcomes = Some(outcomes.clone());
                    factory(SourceBackedPublicationMetadataContext::new(
                        publication,
                        &selected_route_ids,
                        &failed_routes,
                        &logical_source_failures,
                        &record_rejections,
                        &outcomes,
                        &complete_inventory_route_ids,
                        &live_route_controls,
                        applied_removals.len(),
                    ))
                },
                &mut report_publication_stage,
            )?;
            let (commit, disposition, verified) = published.into_parts();
            (
                IndexCaptureCommitReceipt::new(commit),
                Some(SourceBackedVerifiedPublication {
                    disposition,
                    verified_index: verified,
                }),
            )
        } else {
            (
                IndexCaptureCommitReceipt::new(
                    lifecycle.commit(&mut revalidate_source, &mut revalidate_inventory)?,
                ),
                None,
            )
        };
        (
            commit,
            applied_removals,
            complete_inventory_route_ids,
            commit_started.elapsed(),
            base_route_content,
            route_controls,
            verified_publication,
        )
    };
    let successful_route_ids = selected_route_ids
        .iter()
        .filter(|identity| !failed_routes.contains_key(*identity))
        .cloned()
        .collect::<BTreeSet<_>>();
    let successful_route_outcomes = prepared_successful_route_outcomes.unwrap_or_else(|| {
        successful_route_outcomes_for_snapshot(
            &selected_route_ids,
            &failed_routes,
            &logical_source_failures,
            &base_route_content,
            commit.snapshot(),
        )
    });
    run_after_successful_publication(registry, &successful_route_ids);
    let scan_stage_duration = scan_started.elapsed();
    let history_progress = attempt_history_progress.snapshot();
    let _ = report_progress(committed_progress(
        scanned_routes,
        providers,
        history_progress,
        commit_duration,
        discovery_duration.saturating_add(refresh_started.elapsed()),
        commit.certified_sources,
        commit.certified_source_bytes,
    ));
    let certified_source_count = commit.certified_sources;
    let certified_source_bytes = commit.certified_source_bytes;
    let sources = commit.snapshot().sources().to_vec();
    let source_failures = bounded_source_failures(failed_routes.values());
    route_controls.retain(|route, _| commit.snapshot().source_route(route).is_some());
    Ok(SourceBackedRefreshReceipt {
        commit,
        sources,
        removals: applied_removals,
        scanned_routes,
        unsupported_routes,
        discovery_duration,
        scan_stage_duration,
        commit_duration,
        certified_source_count,
        certified_source_bytes,
        selected_route_ids: selected_route_ids.into_iter().collect(),
        successful_route_ids: successful_route_ids.into_iter().collect(),
        successful_route_outcomes,
        complete_inventory_route_ids: complete_inventory_route_ids.into_iter().collect(),
        carried_unselected_route_ids: carried_unselected_route_ids.into_iter().collect(),
        carried_failed_route_ids: failed_routes
            .values()
            .filter(|failure| failure.carried_forward)
            .map(|failure| failure.route_identity.clone())
            .collect(),
        route_controls,
        source_failures,
        logical_source_failures,
        record_rejections,
        verified_publication,
        failed_routes: failed_routes
            .values()
            .map(SourceBackedFailedRouteOutcome::from)
            .collect(),
    })
}
