use super::*;

mod model;
mod ownership;
mod route_content;
mod route_outcomes;

#[cfg(test)]
pub use model::assert_carried_route_failure;
#[cfg(test)]
use model::SOURCE_RECORD_PROGRESS_INTERVAL;
use model::{
    source_level_progress, SourceBackedRefreshPlan, SourceBackedVerifiedPublication,
    SourceRecordProgress,
};
pub use model::{
    SourceBackedCertifiedRemoval, SourceBackedCurrentSourceProgress,
    SourceBackedCurrentSourceProgressStage, SourceBackedDetailedRefreshProgress,
    SourceBackedPublicationMetadataContext, SourceBackedRefreshProgress,
    SourceBackedRefreshReceipt, SourceBackedSuccessfulRouteOutcome,
};
use route_content::source_route_content_fingerprints;
use route_outcomes::successful_route_outcomes_for_manifest;

type SourceBackedPublicationMetadataFactory<'factory> =
    dyn for<'context> FnMut(
            SourceBackedPublicationMetadataContext<'context>,
        ) -> ctx_history_index::Result<Vec<u8>>
        + 'factory;

#[derive(Debug, Clone, Copy)]
struct SourceBackedRefreshExecutionBudget {
    discovery_duration: Duration,
    work_budget: usize,
}

impl SourceBackedRefreshExecutionBudget {
    const fn new(discovery_duration: Duration, work_budget: usize) -> Self {
        Self {
            discovery_duration,
            work_budget,
        }
    }
}

#[cfg(test)]
use ownership::source_owner_covers_base_source;
use ownership::{
    capture_staged_source_route_revalidation_receipts, require_complete_base_source_ownership,
    revalidate_staged_source_route,
};

#[cfg(test)]
thread_local! {
    static BEFORE_SOURCE_BACKED_COMMIT_HOOK: std::cell::RefCell<
        Option<Box<dyn FnOnce()>>,
    > = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn install_before_source_backed_commit_hook_for_test(hook: impl FnOnce() + 'static) {
    BEFORE_SOURCE_BACKED_COMMIT_HOOK.with(|slot| {
        let previous = slot.replace(Some(Box::new(hook)));
        assert!(
            previous.is_none(),
            "source-backed precommit test hooks must not be nested"
        );
    });
}

#[cfg(test)]
fn run_before_source_backed_commit_hook() {
    BEFORE_SOURCE_BACKED_COMMIT_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_source_backed_commit_hook() {}

/// Capture-owned executor that can be installed behind the daemon's
/// provider-neutral `SourceBackedRefreshExecutor` callback seam.
#[derive(Debug, Clone)]
pub struct SourceBackedRefreshExecutor {
    registry: SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    discovery_duration: Duration,
    work_budget: usize,
}

impl SourceBackedRefreshExecutor {
    pub fn new(registry: SourceBackedProviderRegistry, writer_options: WriterOptions) -> Self {
        Self::with_discovery_duration(registry, writer_options, Duration::ZERO)
    }

    pub fn with_discovery_duration(
        registry: SourceBackedProviderRegistry,
        writer_options: WriterOptions,
        discovery_duration: Duration,
    ) -> Self {
        let work_budget = source_backed_refresh_work_budget(writer_options.indexer_threads);
        Self {
            registry,
            writer_options,
            discovery_duration,
            work_budget,
        }
    }

    pub fn registry(&self) -> &SourceBackedProviderRegistry {
        &self.registry
    }

    pub fn refresh(
        &self,
        index_root: impl AsRef<Path>,
        report_progress: impl FnMut(SourceBackedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        let mut report_progress = report_progress;
        refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
            index_root,
            &self.registry,
            self.writer_options.clone(),
            SourceBackedRefreshExecutionBudget::new(self.discovery_duration, self.work_budget),
            SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::All),
            move |update| {
                if update.current_source_progress.is_some() {
                    return Ok(());
                }
                report_progress(update.into_legacy())
            },
            None,
        )
    }

    pub fn refresh_with_detailed_progress(
        &self,
        index_root: impl AsRef<Path>,
        report_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
            index_root,
            &self.registry,
            self.writer_options.clone(),
            SourceBackedRefreshExecutionBudget::new(self.discovery_duration, self.work_budget),
            SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::All),
            report_progress,
            None,
        )
    }

    pub fn refresh_scope(
        &self,
        index_root: impl AsRef<Path>,
        scope: SourceBackedRefreshScope,
        report_progress: impl FnMut(SourceBackedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        let mut report_progress = report_progress;
        refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
            index_root,
            &self.registry,
            self.writer_options.clone(),
            SourceBackedRefreshExecutionBudget::new(self.discovery_duration, self.work_budget),
            SourceBackedRefreshPlan::isolate(scope),
            move |update| {
                if update.current_source_progress.is_some() {
                    return Ok(());
                }
                report_progress(update.into_legacy())
            },
            None,
        )
    }

    pub fn refresh_scope_with_detailed_progress(
        &self,
        index_root: impl AsRef<Path>,
        scope: SourceBackedRefreshScope,
        report_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
            index_root,
            &self.registry,
            self.writer_options.clone(),
            SourceBackedRefreshExecutionBudget::new(self.discovery_duration, self.work_budget),
            SourceBackedRefreshPlan::isolate(scope),
            report_progress,
            None,
        )
    }

    /// Publishes one scope with control-plane metadata bound into the same
    /// opaque Core commit payload. The factory runs only for a pointer-
    /// advancing generation; exact reuse retains the active metadata.
    pub fn refresh_scope_with_detailed_progress_and_publication_metadata(
        &self,
        index_root: impl AsRef<Path>,
        scope: SourceBackedRefreshScope,
        report_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
        mut metadata_factory: impl for<'a> FnMut(
            SourceBackedPublicationMetadataContext<'a>,
        ) -> ctx_history_index::Result<Vec<u8>>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
            index_root,
            &self.registry,
            self.writer_options.clone(),
            SourceBackedRefreshExecutionBudget::new(self.discovery_duration, self.work_budget),
            SourceBackedRefreshPlan::isolate(scope),
            report_progress,
            Some(&mut metadata_factory),
        )
    }
}

/// Runs every executable route against one writer and publishes one atomic
/// generation. This is the capture-owned executor seam for the daemon.
pub fn refresh_source_backed_generation(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    refresh_source_backed_generation_with_progress(index_root, registry, writer_options, |_| Ok(()))
}

#[cfg(test)]
pub(crate) fn refresh_source_backed_generation_with_work_budget_for_test(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    work_budget: usize,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
        index_root,
        registry,
        writer_options,
        SourceBackedRefreshExecutionBudget::new(Duration::ZERO, work_budget),
        SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::All),
        |_| Ok(()),
        None,
    )
}

#[cfg(test)]
pub(crate) fn refresh_source_backed_generation_with_resource_limits_for_test(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    maximum_live_output_bytes: u64,
    maximum_physical_scratch_bytes: u64,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    let work_budget = source_backed_refresh_work_budget(writer_options.indexer_threads);
    refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
        index_root,
        registry,
        writer_options,
        SourceBackedRefreshExecutionBudget::new(Duration::ZERO, work_budget),
        SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::All)
            .with_resource_limits(maximum_live_output_bytes, maximum_physical_scratch_bytes),
        |_| Ok(()),
        None,
    )
}

pub fn refresh_source_backed_generation_with_progress(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    report_progress: impl FnMut(SourceBackedRefreshProgress) -> SourceBackedRouteResult<()>,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    let mut report_progress = report_progress;
    let work_budget = source_backed_refresh_work_budget(writer_options.indexer_threads);
    refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
        index_root,
        registry,
        writer_options,
        SourceBackedRefreshExecutionBudget::new(Duration::ZERO, work_budget),
        SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::All),
        move |update| {
            if update.current_source_progress.is_some() {
                return Ok(());
            }
            report_progress(update.into_legacy())
        },
        None,
    )
}

pub fn refresh_source_backed_generation_with_detailed_progress(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    report_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    let work_budget = source_backed_refresh_work_budget(writer_options.indexer_threads);
    refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
        index_root,
        registry,
        writer_options,
        SourceBackedRefreshExecutionBudget::new(Duration::ZERO, work_budget),
        SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::All),
        report_progress,
        None,
    )
}

pub fn refresh_source_backed_generation_for_routes(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    route_identities: impl IntoIterator<Item = SourceRouteIdentity>,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    let work_budget = source_backed_refresh_work_budget(writer_options.indexer_threads);
    refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
        index_root,
        registry,
        writer_options,
        SourceBackedRefreshExecutionBudget::new(Duration::ZERO, work_budget),
        SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::exact(route_identities)),
        |_| Ok(()),
        None,
    )
}

fn refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    execution_budget: SourceBackedRefreshExecutionBudget,
    plan: SourceBackedRefreshPlan,
    mut report_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
    mut metadata_factory: Option<&mut SourceBackedPublicationMetadataFactory<'_>>,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    let SourceBackedRefreshExecutionBudget {
        discovery_duration,
        work_budget,
    } = execution_budget;
    if matches!(&plan.scope, SourceBackedRefreshScope::All) {
        if let Some(unavailable) = registry.routes.iter().find(|route| {
            route.driver.is_none()
                && route.certified_missing_paths.is_empty()
                && route.metadata.source.status == ProviderSourceStatus::Unknown
        }) {
            return Err(SourceBackedCoordinatorError::UnavailableRoute {
                provider: unavailable.metadata.source.provider,
                detail: unavailable
                    .metadata
                    .unsupported_reason
                    .clone()
                    .unwrap_or_else(|| "route state is unavailable".to_owned()),
            });
        }
    }
    let executable_route_ids = registry
        .routes
        .iter()
        .filter(|route| route.driver.is_some() || !route.certified_missing_paths.is_empty())
        .filter_map(|route| route.metadata.route_identity.clone())
        .collect::<BTreeSet<_>>();
    let selected_route_ids = match &plan.scope {
        SourceBackedRefreshScope::All => executable_route_ids,
        SourceBackedRefreshScope::Exact(selected) => {
            if let Some(unknown) = selected.difference(&executable_route_ids).next() {
                return Err(SourceBackedCoordinatorError::InvalidRefreshScope {
                    route_id: unknown.as_str().to_owned(),
                });
            }
            selected.clone()
        }
    };
    let scanned_routes = registry
        .routes
        .iter()
        .filter(|route| route.driver.is_some())
        .filter(|route| {
            route
                .metadata
                .route_identity
                .as_ref()
                .is_some_and(|identity| selected_route_ids.contains(identity))
        })
        .count();
    let refresh_started = Instant::now();
    report_progress(source_level_progress(SourceBackedRefreshProgress {
        phase: "discovering",
        completed_sources: 0,
        total_sources: scanned_routes,
        current_source: None,
        completed_records: None,
        completed_bytes: None,
        stage_duration: discovery_duration,
        elapsed: discovery_duration,
        certified_source_count: None,
        certified_source_bytes: None,
    }))
    .map_err(SourceBackedCoordinatorError::Progress)?;
    let unsupported_routes = registry
        .routes
        .iter()
        .filter(|route| route.driver.is_none())
        .map(|route| route.metadata.clone())
        .collect();

    let scan_started = Instant::now();
    let index_root = index_root.as_ref();
    let mut failed_routes = BTreeMap::<SourceRouteIdentity, SourceBackedFailedRoute>::new();
    let mut logical_source_failures = SourceBackedLogicalSourceFailures::default();
    let mut record_rejections = SourceBackedRecordRejections::default();
    let mut carried_unselected_route_ids = BTreeSet::new();

    let mut prepared_successful_route_outcomes = None;
    let (commit, applied_removals, commit_duration, base_route_content, verified_publication) = {
        let mut writer = GenerationWriter::open(index_root, writer_options.clone())?;
        let base_route_content = source_route_content_fingerprints(writer.base_manifest());
        let base_route_ids = writer
            .base_manifest()
            .map(|manifest| {
                manifest
                    .source_routes()
                    .iter()
                    .map(|route| route.route_identity().clone())
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        if matches!(&plan.scope, SourceBackedRefreshScope::Exact(_))
            && carried_unselected_route_ids.is_empty()
        {
            carried_unselected_route_ids = base_route_ids
                .difference(&selected_route_ids)
                .cloned()
                .collect();
        }
        let attempt_selected = selected_route_ids.clone();
        let mut attempt_carried = carried_unselected_route_ids.clone();
        writer.set_source_route_plan(attempt_selected.clone(), attempt_carried.clone())?;

        let automatic_missing_observed_at_unix_ms = source_missing_observation_time();
        let mut owners = HashMap::new();
        let mut complete_inventory_owners = Vec::new();
        let mut applied_removals = Vec::new();
        let mut successful_this_attempt = BTreeSet::new();
        let mut completed_routes = 0;
        for (route_index, route) in registry.routes.iter().enumerate() {
            let Some(route_identity) = route.metadata.route_identity.as_ref() else {
                continue;
            };
            if !attempt_selected.contains(route_identity) {
                continue;
            }
            let Some(driver) = &route.driver else {
                continue;
            };
            report_progress(source_level_progress(SourceBackedRefreshProgress {
                phase: "refreshing",
                completed_sources: completed_routes,
                total_sources: scanned_routes,
                current_source: Some(route.metadata.source.path.display().to_string()),
                completed_records: Some(0),
                completed_bytes: Some(0),
                stage_duration: scan_started.elapsed(),
                elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
                certified_source_count: None,
                certified_source_bytes: None,
            }))
            .map_err(SourceBackedCoordinatorError::Progress)?;
            writer.begin_source_route_stage(route_identity.clone())?;
            if let Some(revalidate) = driver.revalidate_at_publication.as_ref() {
                let revalidate = Arc::clone(revalidate);
                writer.register_source_route_publication_revalidation(
                    route_identity.clone(),
                    move || revalidate(),
                )?;
            }
            let removal_checkpoint = applied_removals.len();
            let logical_failure_checkpoint =
                logical_source_failures.checkpoint(route_identity.clone());
            let record_rejection_checkpoint = record_rejections.checkpoint();
            let current_source = route.metadata.source.path.display().to_string();
            let record_progress = std::cell::RefCell::new(SourceRecordProgress::default());
            let progress_failure = std::cell::RefCell::new(None::<SourceBackedRouteError>);
            let scan_result = {
                let progress_callback = std::cell::RefCell::new(&mut report_progress);
                let mut report_record_progress = |delta| {
                    if let Some(error) = progress_failure.borrow().as_ref() {
                        return Err(SourceBackedCoordinatorError::Progress(error.clone()));
                    }
                    let Some((completed_records, completed_bytes)) = record_progress
                        .borrow_mut()
                        .advanced_at(delta, Instant::now())
                    else {
                        return Ok(());
                    };
                    match progress_callback.borrow_mut()(source_level_progress(
                        SourceBackedRefreshProgress {
                            phase: "refreshing",
                            completed_sources: completed_routes,
                            total_sources: scanned_routes,
                            current_source: Some(current_source.clone()),
                            completed_records: Some(completed_records),
                            completed_bytes: Some(completed_bytes),
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
                    match progress_callback.borrow_mut()(SourceBackedDetailedRefreshProgress {
                        progress: SourceBackedRefreshProgress {
                            phase: "refreshing",
                            completed_sources: completed_routes,
                            total_sources: scanned_routes,
                            current_source: Some(current_source.clone()),
                            completed_records: None,
                            completed_bytes: None,
                            stage_duration: scan_started.elapsed(),
                            elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
                            certified_source_count: None,
                            certified_source_bytes: None,
                        },
                        current_source_progress: Some(current_source_progress),
                    }) {
                        Ok(()) => Ok(()),
                        Err(error) => {
                            progress_failure.replace(Some(error.clone()));
                            Err(error)
                        }
                    }
                };
                let core_record_preparer = writer.core_record_preparer();
                let mut sink = SourceBackedGenerationSink {
                    writer: &mut writer,
                    core_record_preparer,
                    owners: &mut owners,
                    complete_inventories: &mut complete_inventory_owners,
                    applied_removals: &mut applied_removals,
                    route_index,
                    route_identity: route_identity.clone(),
                    resources: plan.route_resources(work_budget),
                    logical_source_failures: &mut logical_source_failures,
                    record_rejections: &mut record_rejections,
                    record_progress: Some(&mut report_record_progress),
                    current_source_progress: Some(&mut report_current_source_progress),
                };
                (driver.scan)(&mut sink)
            };
            let mut record_progress = record_progress.into_inner();
            if let Some(error) = progress_failure.into_inner() {
                return Err(SourceBackedCoordinatorError::Progress(error));
            }
            if let Some((completed_records, completed_bytes)) =
                record_progress.flush_at(Instant::now())
            {
                report_progress(source_level_progress(SourceBackedRefreshProgress {
                    phase: "refreshing",
                    completed_sources: completed_routes,
                    total_sources: scanned_routes,
                    current_source: Some(current_source),
                    completed_records: Some(completed_records),
                    completed_bytes: Some(completed_bytes),
                    stage_duration: scan_started.elapsed(),
                    elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
                    certified_source_count: None,
                    certified_source_bytes: None,
                }))
                .map_err(SourceBackedCoordinatorError::Progress)?;
            }
            match scan_result {
                Ok(()) => {
                    capture_staged_source_route_revalidation_receipts(
                        &writer,
                        route_index,
                        &mut owners,
                    )?;
                    report_progress(source_level_progress(SourceBackedRefreshProgress {
                        phase: "verifying",
                        completed_sources: completed_routes,
                        total_sources: scanned_routes,
                        current_source: None,
                        completed_records: None,
                        completed_bytes: None,
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
                        for retired_route in &route.retire_after_success {
                            let retired_sources = writer
                                .retire_carried_source_route(route_identity, retired_route)?;
                            attempt_carried.remove(retired_route);
                            carried_unselected_route_ids.remove(retired_route);
                            for source in retired_sources {
                                let digest = source.identity().digest();
                                if owners
                                    .insert(
                                        digest,
                                        SourceOwner {
                                            route_index,
                                            source: source.clone(),
                                            present: false,
                                            revalidation: None,
                                        },
                                    )
                                    .is_some()
                                {
                                    return Err(
                                        SourceBackedCoordinatorError::DuplicateSourceOwner {
                                            source_id: source.identity().to_string(),
                                        },
                                    );
                                }
                            }
                        }
                        writer.finish_source_route_stage(route_identity)?;
                        successful_this_attempt.insert(route_identity.clone());
                    } else {
                        writer.rollback_source_route_stage(route_identity)?;
                        owners.retain(|_, owner| owner.route_index != route_index);
                        complete_inventory_owners.retain(|owner| owner.route_index != route_index);
                        applied_removals.truncate(removal_checkpoint);
                        logical_source_failures.truncate(logical_failure_checkpoint);
                        record_rejections
                            .truncate(record_rejection_checkpoint.0, record_rejection_checkpoint.1);
                        let carried_forward =
                            writer.carry_failed_source_route_from_base(route_identity)?;
                        attempt_carried.insert(route_identity.clone());
                        failed_routes.insert(
                            route_identity.clone(),
                            SourceBackedFailedRoute::from_route(
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
                    writer.rollback_source_route_stage(route_identity)?;
                    owners.retain(|_, owner| owner.route_index != route_index);
                    complete_inventory_owners.retain(|owner| owner.route_index != route_index);
                    applied_removals.truncate(removal_checkpoint);
                    logical_source_failures.truncate(logical_failure_checkpoint);
                    record_rejections
                        .truncate(record_rejection_checkpoint.0, record_rejection_checkpoint.1);
                    let carried_forward =
                        writer.carry_failed_source_route_from_base(route_identity)?;
                    attempt_carried.insert(route_identity.clone());
                    failed_routes.insert(
                        route_identity.clone(),
                        SourceBackedFailedRoute::from_route(
                            route,
                            class,
                            carried_forward,
                            &source.detail,
                        )?,
                    );
                }
            }
            completed_routes += 1;
        }

        for route in registry
            .routes
            .iter()
            .filter(|route| !route.certified_missing_paths.is_empty())
        {
            let route_identity =
                route
                    .metadata
                    .route_identity
                    .as_ref()
                    .ok_or(IndexError::WriterInvariant(
                        "certified-missing source route has no route identity",
                    ))?;
            if !attempt_selected.contains(route_identity) {
                continue;
            }
            writer.begin_source_route_stage(route_identity.clone())?;
            report_progress(source_level_progress(SourceBackedRefreshProgress {
                phase: "verifying",
                completed_sources: completed_routes,
                total_sources: scanned_routes,
                current_source: None,
                completed_records: None,
                completed_bytes: None,
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
                writer.rollback_source_route_stage(route_identity)?;
                let carried_forward = writer.carry_failed_source_route_from_base(route_identity)?;
                attempt_carried.insert(route_identity.clone());
                failed_routes.insert(
                    route_identity.clone(),
                    SourceBackedFailedRoute::from_route(
                        route,
                        SourceBackedSourceFailureClass::SourceChanged,
                        carried_forward,
                        "certified-missing route changed during terminal verification",
                    )?,
                );
                continue;
            }
            writer.observe_certified_missing_route(
                route_identity.clone(),
                automatic_missing_observed_at_unix_ms,
                AUTOMATIC_ROUTE_DELETION_MISSING_OBSERVATIONS,
                move || {
                    paths
                        .iter()
                        .all(|path| path_presence(path) == PathPresence::Missing)
                },
            )?;
            writer.finish_source_route_stage(route_identity)?;
            successful_this_attempt.insert(route_identity.clone());
        }

        let mut present_routes = Vec::new();
        for (route_index, route) in registry.routes.iter().enumerate() {
            let Some(route_identity) = route.metadata.route_identity.as_ref() else {
                continue;
            };
            if route.driver.is_none() || !successful_this_attempt.contains(route_identity) {
                continue;
            }
            let members = owners
                .values()
                .filter(|owner| owner.route_index == route_index && owner.present)
                .map(|owner| owner.source.clone())
                .collect();
            present_routes.push(SourceRouteSnapshot::present(
                route_identity.clone(),
                members,
            )?);
        }
        writer.set_present_source_routes(present_routes)?;

        require_complete_base_source_ownership(
            &writer,
            registry,
            &owners,
            &complete_inventory_owners,
            &attempt_carried,
        )?;

        let has_carried_source = writer.base_manifest().is_some_and(|base| {
            base.source_routes().iter().any(|route| {
                attempt_carried.contains(route.route_identity()) && !route.sources().is_empty()
            })
        });
        let has_successful_retained_source = writer.base_manifest().is_some_and(|base| {
            base.source_routes().iter().any(|route| {
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
            if failed_routes.is_empty() {
                return Err(SourceBackedCoordinatorError::NoUsableLogicalSources {
                    failed_sources: logical_source_failures.clone(),
                });
            }
            return Err(SourceBackedCoordinatorError::NoUsableSourceRoutes {
                failed_routes: bounded_source_failures(failed_routes.values()),
            });
        }

        let commit_started = Instant::now();
        run_before_source_backed_commit_hook();
        let mut revalidate_source = |target: RevalidationTarget<'_>| {
            let source = match target {
                RevalidationTarget::Source(source) => source.observation().source(),
                RevalidationTarget::Deletion(deletion) => deletion.source(),
            };
            let Some(owner) = owners.get(&source.identity().digest()) else {
                return false;
            };
            if !owner.source.exact_descriptor_eq(source) {
                return false;
            }
            matches!(
                (&owner.revalidation, target),
                (
                    Some(SourceBackedRouteRevalidation::Source(expected)),
                    RevalidationTarget::Source(actual)
                ) if *expected == *actual
            ) || matches!(
                (&owner.revalidation, target),
                (
                    Some(SourceBackedRouteRevalidation::Deletion(expected)),
                    RevalidationTarget::Deletion(actual)
                ) if *expected == *actual
            )
        };
        let mut revalidate_inventory = |inventory: &CertifiedSourceInventory| {
            complete_inventory_owners
                .iter()
                .any(|owner| owner.inventory == *inventory)
        };
        let (commit, verified_publication) = if let Some(factory) = metadata_factory.as_mut() {
            let published = writer
                .commit_with_complete_inventory_revalidation_and_publication_metadata(
                    &mut revalidate_source,
                    &mut revalidate_inventory,
                    |publication| {
                        let outcomes = successful_route_outcomes_for_manifest(
                            &selected_route_ids,
                            &failed_routes,
                            &logical_source_failures,
                            &base_route_content,
                            publication.manifest(),
                        );
                        prepared_successful_route_outcomes = Some(outcomes.clone());
                        factory(SourceBackedPublicationMetadataContext::new(
                            publication,
                            &selected_route_ids,
                            &failed_routes,
                            &logical_source_failures,
                            &record_rejections,
                            &outcomes,
                            applied_removals.len(),
                        ))
                    },
                )?;
            let (commit, disposition, verified) = published.into_parts();
            (
                commit,
                Some(SourceBackedVerifiedPublication {
                    disposition,
                    verified_index: verified,
                }),
            )
        } else {
            (
                writer.commit_with_complete_inventory_revalidation(
                    &mut revalidate_source,
                    &mut revalidate_inventory,
                )?,
                None,
            )
        };
        (
            commit,
            applied_removals,
            commit_started.elapsed(),
            base_route_content,
            verified_publication,
        )
    };

    let successful_route_ids = selected_route_ids
        .iter()
        .filter(|identity| !failed_routes.contains_key(*identity))
        .cloned()
        .collect::<BTreeSet<_>>();
    let successful_route_outcomes = prepared_successful_route_outcomes.unwrap_or_else(|| {
        successful_route_outcomes_for_manifest(
            &selected_route_ids,
            &failed_routes,
            &logical_source_failures,
            &base_route_content,
            commit.manifest(),
        )
    });
    for route in &registry.routes {
        if route
            .metadata
            .route_identity
            .as_ref()
            .is_some_and(|identity| successful_route_ids.contains(identity))
        {
            if let Some(after_publication) = route
                .driver
                .as_ref()
                .and_then(|driver| driver.after_successful_publication.as_ref())
            {
                after_publication();
            }
        }
    }
    let scan_stage_duration = scan_started.elapsed();
    let _ = report_progress(source_level_progress(SourceBackedRefreshProgress {
        phase: "committed",
        completed_sources: scanned_routes,
        total_sources: scanned_routes,
        current_source: None,
        completed_records: None,
        completed_bytes: None,
        stage_duration: commit_duration,
        elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
        certified_source_count: Some(commit.certified_sources),
        certified_source_bytes: Some(commit.certified_source_bytes),
    }));
    let certified_source_count = commit.certified_sources;
    let certified_source_bytes = commit.certified_source_bytes;
    let sources = commit.manifest().sources.clone();
    let source_failures = bounded_source_failures(failed_routes.values());
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
        carried_unselected_route_ids: carried_unselected_route_ids.into_iter().collect(),
        carried_failed_route_ids: failed_routes
            .values()
            .filter(|failure| failure.carried_forward)
            .map(|failure| failure.route_identity.clone())
            .collect(),
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

fn bounded_source_failures<'a>(
    failures: impl IntoIterator<Item = &'a SourceBackedFailedRoute>,
) -> SourceBackedSourceFailures {
    SourceBackedSourceFailures::from_failures(failures.into_iter().cloned())
}

fn source_missing_observation_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod ownership_tests;
