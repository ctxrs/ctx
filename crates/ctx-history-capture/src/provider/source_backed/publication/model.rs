use super::*;

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

pub(super) fn source_level_progress(
    progress: SourceBackedRefreshProgress,
) -> SourceBackedDetailedRefreshProgress {
    SourceBackedDetailedRefreshProgress {
        progress,
        current_source_progress: None,
    }
}

pub(super) const SOURCE_RECORD_PROGRESS_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
pub(super) struct SourceRecordProgress {
    pub(super) completed_records: u64,
    pub(super) completed_bytes: u64,
    last_emitted_records: u64,
    last_emitted_bytes: u64,
    last_emitted_at: Option<Instant>,
}

impl SourceRecordProgress {
    pub(super) fn advanced_at(
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

    pub(super) fn flush_at(&mut self, now: Instant) -> Option<(u64, u64)> {
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

pub(super) struct SourceBackedRefreshPlan {
    pub(super) scope: SourceBackedRefreshScope,
    #[cfg(test)]
    resource_limits: Option<(u64, u64)>,
}

impl SourceBackedRefreshPlan {
    pub(super) fn isolate(scope: SourceBackedRefreshScope) -> Self {
        Self {
            scope,
            #[cfg(test)]
            resource_limits: None,
        }
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

    pub(super) fn route_resources(&self, work_budget: usize) -> SourceBackedRouteResources {
        #[cfg(test)]
        if let Some((output, scratch)) = self.resource_limits {
            return SourceBackedRouteResources::for_test(work_budget, output, scratch);
        }
        SourceBackedRouteResources::production(work_budget)
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
    /// Failures confined to independently owned logical sources inside an
    /// otherwise successfully published provider route.
    pub logical_source_failures: SourceBackedLogicalSourceFailures,
    /// Bounded record-level diagnostics for provider inputs that completed and
    /// published valid peers with explicit rejected-record counts.
    pub record_rejections: SourceBackedRecordRejections,
    pub carried_unselected_route_ids: Vec<SourceRouteIdentity>,
    pub carried_failed_route_ids: Vec<SourceRouteIdentity>,
    pub(super) verified_publication: Option<SourceBackedVerifiedPublication>,
}

pub(super) struct SourceBackedVerifiedPublication {
    pub(super) disposition: PublicationDisposition,
    pub(super) verified_index: VerifiedIndex,
}

impl fmt::Debug for SourceBackedVerifiedPublication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceBackedVerifiedPublication")
            .field("disposition", &self.disposition)
            .field("generation_id", &self.verified_index.generation_id())
            .finish()
    }
}

impl SourceBackedRefreshReceipt {
    /// Takes the already-open verified pin returned by an opaque-metadata
    /// publication. Legacy publication entry points intentionally return none.
    pub fn take_verified_publication(&mut self) -> Option<(PublicationDisposition, VerifiedIndex)> {
        self.verified_publication
            .take()
            .map(|publication| (publication.disposition, publication.verified_index))
    }
}

/// Final capture-owned route facts made available to the control plane's
/// opaque metadata factory immediately before terminal revalidation. Core
/// binds the resulting bytes only when that complete source fence succeeds.
pub struct SourceBackedPublicationMetadataContext<'a> {
    publication: PublicationMetadataContext<'a>,
    selected_route_ids: &'a BTreeSet<SourceRouteIdentity>,
    failed_routes: &'a BTreeMap<SourceRouteIdentity, SourceBackedFailedRoute>,
    logical_source_failures: &'a SourceBackedLogicalSourceFailures,
    record_rejections: &'a SourceBackedRecordRejections,
    successful_route_outcomes: &'a [SourceBackedSuccessfulRouteOutcome],
    removed_source_count: usize,
}

impl<'a> SourceBackedPublicationMetadataContext<'a> {
    pub(super) fn new(
        publication: PublicationMetadataContext<'a>,
        selected_route_ids: &'a BTreeSet<SourceRouteIdentity>,
        failed_routes: &'a BTreeMap<SourceRouteIdentity, SourceBackedFailedRoute>,
        logical_source_failures: &'a SourceBackedLogicalSourceFailures,
        record_rejections: &'a SourceBackedRecordRejections,
        successful_route_outcomes: &'a [SourceBackedSuccessfulRouteOutcome],
        removed_source_count: usize,
    ) -> Self {
        Self {
            publication,
            selected_route_ids,
            failed_routes,
            logical_source_failures,
            record_rejections,
            successful_route_outcomes,
            removed_source_count,
        }
    }

    pub fn generation_id(&self) -> &str {
        self.publication.generation_id()
    }

    pub fn manifest(&self) -> &GenerationManifest {
        self.publication.manifest()
    }

    pub fn selected_route_ids(&self) -> impl ExactSizeIterator<Item = &SourceRouteIdentity> {
        self.selected_route_ids.iter()
    }

    pub fn successful_route_outcomes(&self) -> &[SourceBackedSuccessfulRouteOutcome] {
        self.successful_route_outcomes
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
            .filter_map(|route_id| self.commit.manifest().source_route(route_id))
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
