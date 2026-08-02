use super::*;

mod ownership;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedRefreshProgress {
    pub phase: &'static str,
    pub completed_sources: usize,
    pub total_sources: usize,
    pub current_source: Option<String>,
    /// Core records accepted for the active source route. No total is implied.
    pub completed_records: Option<u64>,
    /// Authoritative logical source bytes completed for the active route. No total is implied.
    pub completed_bytes: Option<u64>,
    /// Time spent in the current phase when this event was emitted.
    pub stage_duration: Duration,
    /// Total measured discovery plus refresh time at this event.
    pub elapsed: Duration,
    /// Commit-derived source evidence, available only after publication.
    pub certified_source_count: Option<usize>,
    /// Commit-derived byte evidence, available only after publication.
    pub certified_source_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedCurrentSourceProgressStage {
    SourceFamilyCopy,
    OnlineBackup,
    LogicalFingerprint,
    LogicalScan,
}

impl SourceBackedCurrentSourceProgressStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceFamilyCopy => "source_family_copy",
            Self::OnlineBackup => "online_backup",
            Self::LogicalFingerprint => "logical_fingerprint",
            Self::LogicalScan => "logical_scan",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceBackedCurrentSourceProgress {
    pub stage: SourceBackedCurrentSourceProgressStage,
    pub snapshot_pages_completed: Option<u64>,
    pub snapshot_pages_total: Option<u64>,
    pub snapshot_bytes_completed: Option<u64>,
    pub snapshot_bytes_total: Option<u64>,
    pub logical_rows_scanned: Option<u64>,
    pub logical_certified_bytes: Option<u64>,
}

impl SourceBackedCurrentSourceProgress {
    pub const fn new(stage: SourceBackedCurrentSourceProgressStage) -> Self {
        Self {
            stage,
            snapshot_pages_completed: None,
            snapshot_pages_total: None,
            snapshot_bytes_completed: None,
            snapshot_bytes_total: None,
            logical_rows_scanned: None,
            logical_certified_bytes: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedDetailedRefreshProgress {
    pub progress: SourceBackedRefreshProgress,
    pub current_source_progress: Option<SourceBackedCurrentSourceProgress>,
}

impl SourceBackedDetailedRefreshProgress {
    pub fn into_legacy(self) -> SourceBackedRefreshProgress {
        self.progress
    }
}

fn source_level_progress(
    progress: SourceBackedRefreshProgress,
) -> SourceBackedDetailedRefreshProgress {
    SourceBackedDetailedRefreshProgress {
        progress,
        current_source_progress: None,
    }
}

const SOURCE_RECORD_PROGRESS_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
struct SourceRecordProgress {
    completed_records: u64,
    completed_bytes: u64,
    last_emitted_records: u64,
    last_emitted_bytes: u64,
    last_emitted_at: Option<Instant>,
}

impl SourceRecordProgress {
    fn advanced_at(
        &mut self,
        delta: SourceBackedRecordProgressDelta,
        now: Instant,
    ) -> Option<(u64, u64)> {
        self.completed_records = self
            .completed_records
            .saturating_add(delta.accepted_records);
        self.completed_bytes = self.completed_bytes.saturating_add(delta.completed_bytes);
        let should_emit = self.last_emitted_at.is_none_or(|last| {
            now.saturating_duration_since(last) >= SOURCE_RECORD_PROGRESS_INTERVAL
        });
        should_emit.then(|| self.mark_emitted(now))
    }

    fn flush_at(&mut self, now: Instant) -> Option<(u64, u64)> {
        (self.completed_records != self.last_emitted_records
            || self.completed_bytes != self.last_emitted_bytes)
            .then(|| self.mark_emitted(now))
    }

    fn mark_emitted(&mut self, now: Instant) -> (u64, u64) {
        self.last_emitted_at = Some(now);
        self.last_emitted_records = self.completed_records;
        self.last_emitted_bytes = self.completed_bytes;
        (self.completed_records, self.completed_bytes)
    }
}

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
            self.discovery_duration,
            self.work_budget,
            SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::All),
            move |update| {
                if update.current_source_progress.is_some() {
                    return Ok(());
                }
                report_progress(update.into_legacy())
            },
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
            self.discovery_duration,
            self.work_budget,
            SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::All),
            report_progress,
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
            self.discovery_duration,
            self.work_budget,
            SourceBackedRefreshPlan::isolate(scope),
            move |update| {
                if update.current_source_progress.is_some() {
                    return Ok(());
                }
                report_progress(update.into_legacy())
            },
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
            self.discovery_duration,
            self.work_budget,
            SourceBackedRefreshPlan::isolate(scope),
            report_progress,
        )
    }
}

struct SourceBackedRefreshPlan {
    scope: SourceBackedRefreshScope,
}

impl SourceBackedRefreshPlan {
    fn isolate(scope: SourceBackedRefreshScope) -> Self {
        Self { scope }
    }
}

#[derive(Debug)]
pub struct SourceBackedRefreshReceipt {
    pub commit: CommitReceipt,
    /// The exact retained source set committed by `commit`, copied from its
    /// immutable manifest rather than from a later [`VerifiedIndex`] reopen.
    pub sources: Vec<CertifiedSource>,
    /// Transition-local certified leaf removals applied by this refresh.
    /// Prior-generation removals are never copied forward.
    pub removals: Vec<SourceBackedCertifiedRemoval>,
    pub scanned_routes: usize,
    pub unsupported_routes: Vec<SourceBackedRouteMetadata>,
    pub discovery_duration: Duration,
    pub scan_stage_duration: Duration,
    pub commit_duration: Duration,
    pub certified_source_count: usize,
    pub certified_source_bytes: u64,
    pub selected_route_ids: Vec<SourceRouteIdentity>,
    pub successful_route_ids: Vec<SourceRouteIdentity>,
    pub successful_route_outcomes: Vec<SourceBackedSuccessfulRouteOutcome>,
    pub failed_routes: Vec<SourceBackedFailedRouteOutcome>,
    pub source_failures: SourceBackedSourceFailures,
    pub carried_unselected_route_ids: Vec<SourceRouteIdentity>,
    pub carried_failed_route_ids: Vec<SourceRouteIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedSuccessfulRouteOutcome {
    pub route_identity: SourceRouteIdentity,
    pub changed: bool,
}

#[cfg(test)]
pub fn assert_carried_route_failure(
    receipt: &SourceBackedRefreshReceipt,
    retained_generation: &str,
    class: SourceBackedSourceFailureClass,
) {
    assert_eq!(receipt.commit.generation_id, retained_generation);
    assert!(receipt.successful_route_ids.is_empty());
    assert_eq!(receipt.failed_routes.len(), 1);
    let failure = &receipt.failed_routes[0];
    assert_eq!(failure.class, class);
    assert!(failure.carried_forward);
    assert_eq!(
        receipt.carried_failed_route_ids,
        vec![failure.route_identity.clone()]
    );
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedCertifiedRemoval {
    pub deletion: CertifiedSourceDeletion,
    pub inventory: CertifiedSourceInventory,
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
        Duration::ZERO,
        work_budget,
        SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::All),
        move |update| {
            if update.current_source_progress.is_some() {
                return Ok(());
            }
            report_progress(update.into_legacy())
        },
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
        Duration::ZERO,
        work_budget,
        SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::All),
        report_progress,
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
        Duration::ZERO,
        work_budget,
        SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::exact(route_identities)),
        |_| Ok(()),
    )
}

fn refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    discovery_duration: Duration,
    work_budget: usize,
    plan: SourceBackedRefreshPlan,
    mut report_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
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
    let mut carried_unselected_route_ids = BTreeSet::new();

    let (commit, applied_removals, commit_duration, base_route_content) = {
        let mut writer = GenerationWriter::open(index_root, writer_options.clone())?;
        let base_route_content = selected_route_ids
            .iter()
            .map(|route_identity| {
                (
                    route_identity.clone(),
                    source_route_content_fingerprint(writer.base_manifest(), route_identity),
                )
            })
            .collect::<BTreeMap<_, _>>();
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
            let removal_checkpoint = applied_removals.len();
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
                let mut sink = SourceBackedGenerationSink {
                    writer: &mut writer,
                    owners: &mut owners,
                    complete_inventories: &mut complete_inventory_owners,
                    applied_removals: &mut applied_removals,
                    route_index,
                    leaf_worker_budget: work_budget,
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
                        route_index,
                        driver,
                        &owners,
                        &complete_inventory_owners,
                    )? {
                        writer.finish_source_route_stage(route_identity)?;
                        successful_this_attempt.insert(route_identity.clone());
                    } else {
                        writer.rollback_source_route_stage(route_identity)?;
                        owners.retain(|_, owner| owner.route_index != route_index);
                        complete_inventory_owners.retain(|owner| owner.route_index != route_index);
                        applied_removals.truncate(removal_checkpoint);
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
        if !failed_routes.is_empty()
            && !has_carried_source
            && !has_successful_retained_source
            && !has_successful_source
        {
            return Err(SourceBackedCoordinatorError::NoUsableSourceRoutes {
                failed_routes: bounded_source_failures(failed_routes.values()),
            });
        }

        let commit_started = Instant::now();
        run_before_source_backed_commit_hook();
        let commit = writer.commit_with_complete_inventory_revalidation(
            |target| {
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
            },
            |inventory| {
                complete_inventory_owners
                    .iter()
                    .any(|owner| owner.inventory == *inventory)
            },
        )?;
        (
            commit,
            applied_removals,
            commit_started.elapsed(),
            base_route_content,
        )
    };

    let successful_route_ids = selected_route_ids
        .iter()
        .filter(|identity| !failed_routes.contains_key(*identity))
        .cloned()
        .collect::<Vec<_>>();
    let successful_route_outcomes = successful_route_ids
        .iter()
        .cloned()
        .map(|route_identity| SourceBackedSuccessfulRouteOutcome {
            changed: base_route_content.get(&route_identity)
                != Some(&source_route_content_fingerprint(
                    Some(commit.manifest()),
                    &route_identity,
                )),
            route_identity,
        })
        .collect::<Vec<_>>();
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
        successful_route_ids,
        successful_route_outcomes,
        carried_unselected_route_ids: carried_unselected_route_ids.into_iter().collect(),
        carried_failed_route_ids: failed_routes
            .values()
            .filter(|failure| failure.carried_forward)
            .map(|failure| failure.route_identity.clone())
            .collect(),
        source_failures,
        failed_routes: failed_routes
            .values()
            .map(SourceBackedFailedRouteOutcome::from)
            .collect(),
    })
}

fn source_route_content_fingerprint(
    manifest: Option<&GenerationManifest>,
    route_identity: &SourceRouteIdentity,
) -> [u8; 32] {
    let sources = manifest
        .and_then(|manifest| manifest.source_route(route_identity))
        .map(SourceRouteSnapshot::sources)
        .unwrap_or_default();
    let mut digest = Sha256::new();
    digest.update(b"ctx.source-route-content-v1\0");
    digest.update((sources.len() as u64).to_be_bytes());
    for source in sources {
        digest.update(source.identity().digest());
        match manifest.and_then(|manifest| source_core_record_aggregate(manifest, source)) {
            Some(aggregate) => {
                digest.update([1]);
                digest.update(aggregate.indexed_documents().to_be_bytes());
                digest.update(aggregate.semantic_eligible_documents().to_be_bytes());
                digest.update(aggregate.core_record_accumulator().as_bytes());
            }
            None => digest.update([0]),
        }
    }
    digest.finalize().into()
}

fn source_core_record_aggregate<'manifest>(
    manifest: &'manifest GenerationManifest,
    source: &SourceKey,
) -> Option<&'manifest ctx_history_index::SourceCoreRecordAggregate> {
    manifest
        .sources
        .binary_search_by_key(&source.identity().digest(), |candidate| {
            candidate.observation().source().identity().digest()
        })
        .ok()
        .and_then(|index| manifest.core_record_aggregates.get(index))
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
mod ownership_tests {
    use ctx_history_core::{
        ProjectionContractError, SourceAnchor, SourceInventoryObservation, TypedKey,
    };

    use super::*;

    fn descriptor(schema_variant: &str, lineage: u8) -> SourceKey {
        SourceKey::derive(
            CaptureProvider::Gemini.as_str(),
            "ownership-test",
            schema_variant,
            1,
            SourceAnchor::CatalogLineage([lineage; 32]),
        )
        .unwrap()
    }

    fn inventory_owner(
        route_index: usize,
        authority: u8,
        sources: Vec<SourceKey>,
    ) -> CompleteInventoryOwner {
        let observation = SourceInventoryObservation::new(
            CaptureProvider::Gemini.as_str(),
            "ownership-test-root",
            TypedKey::U64(u64::from(authority)),
            "ownership-test-revision",
            vec![authority],
        )
        .unwrap();
        CompleteInventoryOwner {
            route_index,
            inventory: CertifiedSourceInventory::certify(
                observation.clone(),
                observation,
                "ownership-test-discovery",
                sources,
            )
            .unwrap(),
        }
    }

    #[test]
    fn base_ownership_accepts_exact_or_one_inventory_certified_descriptor_replacement() {
        let descriptor_a = descriptor("schema-a", 1);
        let descriptor_b = descriptor("schema-b", 1);
        let exact_owner = SourceOwner {
            route_index: 3,
            source: descriptor_a.clone(),
            present: true,
            revalidation: None,
        };
        assert!(source_owner_covers_base_source(
            &descriptor_a,
            &exact_owner,
            &[]
        ));

        let replacement_owner = SourceOwner {
            route_index: 3,
            source: descriptor_b.clone(),
            present: true,
            revalidation: None,
        };
        let inventory = inventory_owner(3, 1, vec![descriptor_b]);
        assert!(source_owner_covers_base_source(
            &descriptor_a,
            &replacement_owner,
            &[inventory]
        ));
    }

    #[test]
    fn descriptor_replacement_ownership_rejects_absence_wrong_route_ambiguity_and_lineage() {
        let descriptor_a = descriptor("schema-a", 1);
        let descriptor_b = descriptor("schema-b", 1);
        let replacement_owner = SourceOwner {
            route_index: 3,
            source: descriptor_b.clone(),
            present: true,
            revalidation: None,
        };

        assert!(!source_owner_covers_base_source(
            &descriptor_a,
            &replacement_owner,
            &[]
        ));
        assert!(!source_owner_covers_base_source(
            &descriptor_a,
            &replacement_owner,
            &[inventory_owner(4, 1, vec![descriptor_b.clone()])]
        ));
        assert!(!source_owner_covers_base_source(
            &descriptor_a,
            &replacement_owner,
            &[
                inventory_owner(3, 1, vec![descriptor_b.clone()]),
                inventory_owner(3, 2, vec![descriptor_b]),
            ]
        ));

        let unrelated_owner = SourceOwner {
            route_index: 3,
            source: descriptor("schema-b", 2),
            present: true,
            revalidation: None,
        };
        assert!(!source_owner_covers_base_source(
            &descriptor_a,
            &unrelated_owner,
            &[inventory_owner(3, 3, vec![unrelated_owner.source.clone()])]
        ));
    }

    #[test]
    fn inventory_rejects_two_descriptors_for_one_canonical_lineage() {
        let descriptor_a = descriptor("schema-a", 1);
        let descriptor_b = descriptor("schema-b", 1);
        let observation = SourceInventoryObservation::new(
            CaptureProvider::Gemini.as_str(),
            "ownership-test-root",
            TypedKey::U64(1),
            "ownership-test-revision",
            vec![1],
        )
        .unwrap();
        assert_eq!(
            CertifiedSourceInventory::certify(
                observation.clone(),
                observation,
                "ownership-test-discovery",
                vec![descriptor_a, descriptor_b],
            )
            .unwrap_err(),
            ProjectionContractError::DuplicateInventorySource
        );
    }

    #[test]
    fn source_record_progress_is_prompt_throttled_monotonic_and_flushable() {
        let started = Instant::now();
        let mut progress = SourceRecordProgress::default();
        let accepted = SourceBackedRecordProgressDelta {
            accepted_records: 1,
            completed_bytes: 0,
        };
        let bytes = SourceBackedRecordProgressDelta {
            accepted_records: 0,
            completed_bytes: 512,
        };

        assert_eq!(progress.advanced_at(bytes, started), Some((0, 512)));
        assert_eq!(
            progress.advanced_at(accepted, started + Duration::from_millis(500)),
            None
        );
        assert_eq!(
            progress.advanced_at(bytes, started + SOURCE_RECORD_PROGRESS_INTERVAL),
            Some((1, 1_024))
        );
        assert_eq!(
            progress.advanced_at(accepted, started + Duration::from_millis(1_100)),
            None
        );
        assert_eq!(
            progress.flush_at(started + Duration::from_millis(1_100)),
            Some((2, 1_024))
        );
        assert_eq!(
            progress.flush_at(started + Duration::from_millis(1_100)),
            None
        );

        let mut next_source = SourceRecordProgress::default();
        assert_eq!(next_source.completed_records, 0);
        assert_eq!(next_source.completed_bytes, 0);
        assert_eq!(next_source.advanced_at(accepted, started), Some((1, 0)));
    }
}
