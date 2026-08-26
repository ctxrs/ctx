use super::*;
use ctx_history_capture_model::SharedAttemptHistoryProgress;

pub(super) struct SourceBackedRefreshPlan {
    pub(super) scope: SourceBackedRefreshScope,
    pub(super) publication_scope: SourceBackedRefreshScope,
    pub(super) reconciliation_demand: SourceBackedReconciliationDemand,
    pub(super) route_worksets: BTreeMap<SourceRouteIdentity, BTreeSet<PathBuf>>,
    pub(super) attempt_history_progress: SharedAttemptHistoryProgress,
    #[cfg(test)]
    resource_limits: Option<(u64, u64)>,
}

impl SourceBackedRefreshPlan {
    pub(super) fn isolate(scope: SourceBackedRefreshScope) -> Self {
        Self {
            publication_scope: scope.clone(),
            scope,
            reconciliation_demand: SourceBackedReconciliationDemand::Exhaustive,
            route_worksets: BTreeMap::new(),
            attempt_history_progress: SharedAttemptHistoryProgress::default(),
            #[cfg(test)]
            resource_limits: None,
        }
    }

    pub(super) fn with_publication_scope(mut self, scope: SourceBackedRefreshScope) -> Self {
        self.publication_scope = scope;
        self
    }

    pub(super) fn with_reconciliation_demand(
        mut self,
        demand: SourceBackedReconciliationDemand,
    ) -> Self {
        self.reconciliation_demand = demand;
        self
    }

    pub(super) fn with_route_worksets(
        mut self,
        route_worksets: BTreeMap<SourceRouteIdentity, BTreeSet<PathBuf>>,
    ) -> Self {
        self.route_worksets = route_worksets;
        self
    }

    pub(super) fn with_attempt_history_progress(
        mut self,
        progress: SharedAttemptHistoryProgress,
    ) -> Self {
        self.attempt_history_progress = progress;
        self
    }

    #[cfg(test)]
    pub(super) fn with_resource_limits(
        mut self,
        maximum_live_output_bytes: u64,
        maximum_physical_scratch_bytes: u64,
    ) -> Self {
        self.resource_limits = Some((maximum_live_output_bytes, maximum_physical_scratch_bytes));
        self
    }

    pub(super) fn route_resources_for(
        &self,
        route: &SourceRouteIdentity,
        work_budget: usize,
    ) -> SourceBackedRouteResources {
        #[cfg(test)]
        if let Some((output, scratch)) = self.resource_limits {
            return SourceBackedRouteResources::for_test(work_budget, output, scratch)
                .with_reconciliation_demand(self.reconciliation_demand)
                .with_member_workset(self.route_worksets.get(route).cloned())
                .with_attempt_history_progress(self.attempt_history_progress.clone());
        }
        SourceBackedRouteResources::production(work_budget)
            .with_reconciliation_demand(self.reconciliation_demand)
            .with_member_workset(self.route_worksets.get(route).cloned())
            .with_attempt_history_progress(self.attempt_history_progress.clone())
    }
}

#[derive(Debug)]
pub struct SourceBackedRefreshReceipt {
    pub commit: IndexCaptureCommitReceipt,
    /// The exact retained source set committed by `commit`, copied from its
    /// immutable snapshot rather than from a later pin reopen.
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
    /// Successful routes whose terminal fence certified a complete inventory.
    pub complete_inventory_route_ids: Vec<SourceRouteIdentity>,
    pub failed_routes: Vec<SourceBackedFailedRouteOutcome>,
    pub source_failures: SourceBackedSourceFailures,
    /// Failures confined to independently owned logical sources inside an
    /// otherwise successfully published provider route.
    pub logical_source_failures: SourceBackedLogicalSourceFailures,
    /// Bounded record-level diagnostics for provider inputs that completed and
    /// published valid peers with explicit rejected-record counts.
    pub record_rejections: SourceBackedRecordRejections,
    pub carried_unselected_route_ids: Vec<SourceRouteIdentity>,
    pub carried_failed_route_ids: Vec<SourceRouteIdentity>,
    /// Durable provider-private state aligned to the exact live route set.
    pub route_controls: BTreeMap<SourceRouteIdentity, Vec<u8>>,
    pub(super) verified_publication: Option<SourceBackedVerifiedPublication>,
}

pub(super) struct SourceBackedVerifiedPublication {
    pub(super) disposition: CapturePublicationDisposition,
    pub(super) verified_index: IndexCaptureVerifiedPin,
}

impl fmt::Debug for SourceBackedVerifiedPublication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceBackedVerifiedPublication")
            .field("disposition", &self.disposition)
            .field("verified_index", &self.verified_index)
            .finish()
    }
}

impl SourceBackedRefreshReceipt {
    /// Takes the already-open verified pin returned by an opaque-metadata
    /// publication. Legacy publication entry points intentionally return none.
    pub fn take_verified_publication(
        &mut self,
    ) -> Option<(CapturePublicationDisposition, IndexCaptureVerifiedPin)> {
        self.verified_publication
            .take()
            .map(|publication| (publication.disposition, publication.verified_index))
    }
}

/// Final capture-owned route facts made available to the control plane's
/// opaque metadata factory immediately before terminal revalidation. Core
/// binds the resulting bytes only when that complete source fence succeeds.
pub struct SourceBackedPublicationMetadataContext<'a> {
    publication: CapturePublicationContext<'a, BorrowedIndexManifestView<'a>>,
    selected_route_ids: &'a BTreeSet<SourceRouteIdentity>,
    failed_routes: &'a BTreeMap<SourceRouteIdentity, SourceBackedFailedRoute>,
    logical_source_failures: &'a SourceBackedLogicalSourceFailures,
    record_rejections: &'a SourceBackedRecordRejections,
    successful_route_outcomes: &'a [SourceBackedSuccessfulRouteOutcome],
    complete_inventory_route_ids: &'a BTreeSet<SourceRouteIdentity>,
    route_controls: &'a BTreeMap<SourceRouteIdentity, Vec<u8>>,
    removed_source_count: usize,
}

impl<'a> SourceBackedPublicationMetadataContext<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        publication: CapturePublicationContext<'a, BorrowedIndexManifestView<'a>>,
        selected_route_ids: &'a BTreeSet<SourceRouteIdentity>,
        failed_routes: &'a BTreeMap<SourceRouteIdentity, SourceBackedFailedRoute>,
        logical_source_failures: &'a SourceBackedLogicalSourceFailures,
        record_rejections: &'a SourceBackedRecordRejections,
        successful_route_outcomes: &'a [SourceBackedSuccessfulRouteOutcome],
        complete_inventory_route_ids: &'a BTreeSet<SourceRouteIdentity>,
        route_controls: &'a BTreeMap<SourceRouteIdentity, Vec<u8>>,
        removed_source_count: usize,
    ) -> Self {
        Self {
            publication,
            selected_route_ids,
            failed_routes,
            logical_source_failures,
            record_rejections,
            successful_route_outcomes,
            complete_inventory_route_ids,
            route_controls,
            removed_source_count,
        }
    }

    pub fn generation_id(&self) -> &str {
        self.publication.generation_id()
    }

    pub fn snapshot(&self) -> &BorrowedIndexManifestView<'a> {
        self.publication.snapshot()
    }

    pub fn selected_route_ids(&self) -> impl ExactSizeIterator<Item = &SourceRouteIdentity> {
        self.selected_route_ids.iter()
    }

    pub fn successful_route_outcomes(&self) -> &[SourceBackedSuccessfulRouteOutcome] {
        self.successful_route_outcomes
    }

    pub fn complete_inventory_route_ids(
        &self,
    ) -> impl ExactSizeIterator<Item = &SourceRouteIdentity> {
        self.complete_inventory_route_ids.iter()
    }

    pub fn route_controls(&self) -> &BTreeMap<SourceRouteIdentity, Vec<u8>> {
        self.route_controls
    }

    pub fn failed_routes(&self) -> impl ExactSizeIterator<Item = &SourceBackedFailedRoute> {
        self.failed_routes.values()
    }

    pub fn failed_route_outcomes(&self) -> Vec<SourceBackedFailedRouteOutcome> {
        self.failed_routes
            .values()
            .map(SourceBackedFailedRouteOutcome::from)
            .collect()
    }

    pub fn source_failures(&self) -> SourceBackedSourceFailures {
        bounded_source_failures(self.failed_routes.values())
    }

    pub fn logical_source_failures(&self) -> &SourceBackedLogicalSourceFailures {
        self.logical_source_failures
    }

    pub fn record_rejections(&self) -> &SourceBackedRecordRejections {
        self.record_rejections
    }

    pub fn removed_source_count(&self) -> usize {
        self.removed_source_count
    }
}

impl SourceBackedRefreshReceipt {
    /// Record-level completion is derived from durable certified counts, so an
    /// exact replay preserves `completed_with_rejections` even when no input
    /// record is reparsed and no fresh diagnostic is emitted. Exact-scope
    /// refreshes consider only successfully selected route members, not
    /// carried history belonging to routes outside the requested scope.
    pub fn record_completion(&self) -> SourceBackedRecordCompletion {
        let rejected_sources = self
            .sources
            .iter()
            .filter(|source| source.counts().rejected_records != 0)
            .map(|source| source.observation().source().identity().digest())
            .collect::<HashSet<_>>();
        let successful_source_has_rejections = self
            .successful_route_ids
            .iter()
            .filter_map(|route_id| self.commit.snapshot().source_route(route_id))
            .flat_map(|route| route.sources())
            .any(|route_source| rejected_sources.contains(&route_source.identity().digest()));
        if successful_source_has_rejections || !self.record_rejections.is_empty() {
            SourceBackedRecordCompletion::CompletedWithRejections
        } else {
            SourceBackedRecordCompletion::Completed
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedSuccessfulRouteOutcome {
    pub route_identity: SourceRouteIdentity,
    pub changed: bool,
    /// Exact logical-source failure count for this successful route, including
    /// entries omitted from the bounded diagnostic vector.
    pub logical_source_failure_total: usize,
    /// Exact retryable subset of `logical_source_failure_total`, including
    /// entries omitted from the bounded diagnostic vector.
    pub logical_source_retryable_failure_total: usize,
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
