mod ownership;
mod route;

pub use ownership::*;
pub use route::*;

use std::{
    collections::{BTreeSet, HashMap},
    fmt,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use ctx_history_capture_model::{
    CoreRecordBatchProgress, SourceBackedRecordProgressDelta, SourceRouteIdentity,
};
use ctx_history_core::{
    CaptureProvider, CertifiedSource, CertifiedSourceAppend, CertifiedSourceDeletion,
    CertifiedSourceInventory, CoreRecord, SourceKey,
};
use thiserror::Error;

use crate::{
    CaptureLifecycleSink, CorePreparationError, CorePreparationFailureKind, CorePreparedBatch,
    CorePreparedBatchBuilder, CorePreparedCapture, CoreRecordProgress, CoreRouteResourceError,
    ImmutableCaptureSnapshot, CORE_RECORD_BATCH_MAX_RECORDS,
};

use super::{
    diagnostics::{self, *},
    SourceBackedCertifiedRemoval, SourceBackedCurrentSourceProgress,
    SourceBackedCurrentSourceProgressStage, SourceBackedRouteResources,
};

const SOURCE_BACKED_INTERMEDIATE_PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

pub const MAX_RECORDED_SOURCE_BACKED_FAILURES: usize = 64;
pub const MAX_SOURCE_BACKED_FAILURE_SELECTOR_BYTES: usize = 512;
pub const MAX_SOURCE_BACKED_FAILURE_DETAIL_BYTES: usize = 512;
pub const MAX_RECORDED_SOURCE_BACKED_RECORD_REJECTIONS: usize = 64;
pub const MAX_SOURCE_BACKED_ROUTE_CONTROL_BYTES: usize = 4 * 1024;
pub const MAX_SOURCE_BACKED_REJECTION_PAYLOAD_TYPE_BYTES: usize = 128;

pub type SourceBackedCoordinatorResult<T, E> = Result<T, SourceBackedCoordinatorError<E>>;
pub type SourceBackedRouteResult<T> = Result<T, SourceBackedRouteError>;

// Bounded route, source, and record diagnostics live in the diagnostics module.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedDeletionDisposition {
    Deferred,
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedRouteErrorKind {
    Unavailable,
    SourceChanged,
    InvalidSource,
    Unsupported,
    ResourceUnavailable,
    Internal,
}

impl SourceBackedRouteErrorKind {
    pub fn source_failure_class(self) -> Option<SourceBackedSourceFailureClass> {
        match self {
            Self::Unavailable => Some(SourceBackedSourceFailureClass::Unavailable),
            Self::SourceChanged => Some(SourceBackedSourceFailureClass::SourceChanged),
            Self::InvalidSource => Some(SourceBackedSourceFailureClass::Unreadable),
            Self::Unsupported => Some(SourceBackedSourceFailureClass::Incompatible),
            Self::ResourceUnavailable | Self::Internal => None,
        }
    }

    /// Only these failures are narrow enough for a family with a complete,
    /// stable inventory and exact source ownership to retain one source while
    /// publishing certified peers. Source drift, schema ambiguity, aggregate
    /// resource failure, and internal failures remain route-fatal.
    pub const fn is_logical_source_failure(self) -> bool {
        matches!(self, Self::Unavailable | Self::InvalidSource)
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{kind:?}: {detail}")]
pub struct SourceBackedRouteError {
    pub kind: SourceBackedRouteErrorKind,
    pub detail: String,
}

impl SourceBackedRouteError {
    pub fn new(kind: SourceBackedRouteErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

/// Preserves both failures from an explicit source cleanup while retaining the
/// stronger route-level failure class for coordinator policy.
pub fn combine_primary_and_cleanup_route_errors(
    primary: SourceBackedRouteError,
    cleanup: SourceBackedRouteError,
) -> SourceBackedRouteError {
    let kind = if route_error_severity(primary.kind) >= route_error_severity(cleanup.kind) {
        primary.kind
    } else {
        cleanup.kind
    };
    SourceBackedRouteError::new(
        kind,
        format!(
            "{}; explicit SQLite snapshot cleanup also failed: {}",
            primary.detail, cleanup.detail
        ),
    )
}

const fn route_error_severity(kind: SourceBackedRouteErrorKind) -> u8 {
    match kind {
        SourceBackedRouteErrorKind::Internal => 6,
        SourceBackedRouteErrorKind::ResourceUnavailable => 5,
        SourceBackedRouteErrorKind::SourceChanged => 4,
        SourceBackedRouteErrorKind::InvalidSource => 3,
        SourceBackedRouteErrorKind::Unsupported => 2,
        SourceBackedRouteErrorKind::Unavailable => 1,
    }
}

impl From<CoreRouteResourceError> for SourceBackedRouteError {
    fn from(error: CoreRouteResourceError) -> Self {
        Self::new(
            SourceBackedRouteErrorKind::ResourceUnavailable,
            error.to_string(),
        )
    }
}

impl<E: fmt::Display> From<CorePreparationError<E>> for SourceBackedRouteError {
    fn from(error: CorePreparationError<E>) -> Self {
        match error {
            CorePreparationError::Preparation { kind, failure } => Self::new(
                match kind {
                    CorePreparationFailureKind::InvalidSource => {
                        SourceBackedRouteErrorKind::InvalidSource
                    }
                    CorePreparationFailureKind::Internal => SourceBackedRouteErrorKind::Internal,
                },
                failure.to_string(),
            ),
            CorePreparationError::Resource(error) => error.into(),
            CorePreparationError::Internal(detail) => {
                Self::new(SourceBackedRouteErrorKind::Internal, detail)
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum SourceBackedCoordinatorError<E>
where
    E: std::error::Error + 'static,
{
    #[error(transparent)]
    Index(#[from] E),
    #[error(
        "predecessor generation migration committed successor {generation_id}, but writer recovery is still required: {detail}"
    )]
    CommittedPredecessorMigrationRecovery {
        generation_id: String,
        detail: String,
    },
    #[error("invalid source-backed route for {provider}: {detail}")]
    InvalidRoute {
        provider: CaptureProvider,
        detail: String,
    },
    #[error("source-backed scan failed for {provider}: {source}")]
    RouteScan {
        provider: CaptureProvider,
        #[source]
        source: SourceBackedRouteError,
    },
    #[error("source-backed route registration failed for {provider}: {source}")]
    RouteRegistration {
        provider: CaptureProvider,
        #[source]
        source: SourceBackedRouteError,
    },
    #[error("source-backed refresh has an unknown or unavailable route for {provider}: {detail}")]
    UnavailableRoute {
        provider: CaptureProvider,
        detail: String,
    },
    #[error("source {source_id} was staged by more than one provider route")]
    DuplicateSourceOwner { source_id: String },
    #[error("base source {source_id} was not claimed by any provider route in this refresh")]
    UnclaimedBaseSource { source_id: String },
    #[error("source deletion was not certified by its supplied authoritative inventory")]
    InvalidDeletionWitness,
    #[error("retained source deletion {source_id} could not be recertified: {detail}")]
    RetainedDeletionRecertification { source_id: String, detail: String },
    #[error("source-backed refresh progress callback failed: {0}")]
    Progress(SourceBackedRouteError),
    #[error("source-backed Core-record emission failed: {0}")]
    CoreEmission(SourceBackedRouteError),
    #[error("logical-source outcome is inconsistent: {detail}")]
    InvalidLogicalSourceFailure { detail: &'static str },
    #[error("selected source-backed route {route_id} is unknown or not executable")]
    InvalidRefreshScope { route_id: String },
    #[error(
        "source-backed refresh completed with source failures but retained no usable source: {failed_routes}"
    )]
    NoUsableSourceRoutes {
        failed_routes: SourceBackedSourceFailures,
    },
    #[error(
        "source-backed refresh completed with logical-source failures but retained no usable source"
    )]
    NoUsableLogicalSources {
        failed_sources: SourceBackedLogicalSourceFailures,
    },
}

/// The only write surface provider drivers receive. It exposes staging and
/// certification, but never generation commit.
pub struct SourceBackedGenerationSink<'writer, L: CaptureLifecycleSink> {
    pub lifecycle: &'writer mut L,
    pub core_record_preparer: L::Preparation,
    pub owners: &'writer mut HashMap<[u8; 32], SourceOwner>,
    pub complete_inventories: &'writer mut Vec<CompleteInventoryOwner>,
    pub applied_removals: &'writer mut Vec<SourceBackedCertifiedRemoval>,
    pub route_index: usize,
    pub route_identity: SourceRouteIdentity,
    /// Exact predecessor routes this successor may consult during one
    /// exhaustive, staged topology migration.  These aliases are supplied by
    /// composition, never discovered by provider code.
    pub base_route_aliases: BTreeSet<SourceRouteIdentity>,
    pub base_route_control: Option<Vec<u8>>,
    pub resources: SourceBackedRouteResources,
    pub logical_source_failures: &'writer mut SourceBackedLogicalSourceFailures,
    pub record_rejections: &'writer mut SourceBackedRecordRejections,
    pub record_progress: Option<
        &'writer mut dyn FnMut(
            SourceBackedRecordProgressDelta,
        ) -> SourceBackedCoordinatorResult<(), L::Error>,
    >,
    pub current_source_progress: Option<
        &'writer mut dyn FnMut(SourceBackedCurrentSourceProgress) -> SourceBackedRouteResult<()>,
    >,
    pub intermediate_progress_last_emitted_at: Option<Instant>,
    pub intermediate_progress_pending_stage: Option<SourceBackedCurrentSourceProgressStage>,
    pub last_progress_session_id: Option<[u8; 32]>,
    /// Private estimator accounting, carried on existing progress callbacks.
    #[doc(hidden)]
    pub exact_scan_total_bytes: Option<u64>,
    #[doc(hidden)]
    pub exact_scan_accounting_enabled: bool,
}

impl<L: CaptureLifecycleSink> SourceBackedGenerationSink<'_, L> {
    #[allow(clippy::too_many_arguments)]
    pub fn new<'writer>(
        lifecycle: &'writer mut L,
        owners: &'writer mut HashMap<[u8; 32], SourceOwner>,
        complete_inventories: &'writer mut Vec<CompleteInventoryOwner>,
        applied_removals: &'writer mut Vec<SourceBackedCertifiedRemoval>,
        route_index: usize,
        route_identity: SourceRouteIdentity,
        base_route_control: Option<Vec<u8>>,
        resources: SourceBackedRouteResources,
        logical_source_failures: &'writer mut SourceBackedLogicalSourceFailures,
        record_rejections: &'writer mut SourceBackedRecordRejections,
        record_progress: Option<
            &'writer mut dyn FnMut(
                SourceBackedRecordProgressDelta,
            ) -> SourceBackedCoordinatorResult<(), L::Error>,
        >,
        current_source_progress: Option<
            &'writer mut dyn FnMut(
                SourceBackedCurrentSourceProgress,
            ) -> SourceBackedRouteResult<()>,
        >,
        last_progress_session_id: Option<[u8; 32]>,
    ) -> SourceBackedGenerationSink<'writer, L> {
        let core_record_preparer = lifecycle.core_preparation();
        SourceBackedGenerationSink {
            lifecycle,
            core_record_preparer,
            owners,
            complete_inventories,
            applied_removals,
            route_index,
            route_identity,
            base_route_aliases: BTreeSet::new(),
            base_route_control,
            resources,
            logical_source_failures,
            record_rejections,
            record_progress,
            current_source_progress,
            intermediate_progress_last_emitted_at: None,
            intermediate_progress_pending_stage: None,
            last_progress_session_id,
            exact_scan_total_bytes: None,
            exact_scan_accounting_enabled: false,
        }
    }

    pub fn reconciliation_demand(&self) -> SourceBackedReconciliationDemand {
        self.resources.reconciliation_demand()
    }

    pub fn member_workset(&self) -> Option<&BTreeSet<PathBuf>> {
        self.resources.member_workset()
    }

    pub fn base_route_control(&self) -> Option<&[u8]> {
        self.base_route_control.as_deref()
    }

    pub fn route_identity(&self) -> &SourceRouteIdentity {
        &self.route_identity
    }

    pub fn base_snapshot(&self) -> Option<L::Snapshot<'_>> {
        self.lifecycle.base_snapshot()
    }

    /// Carries unmentioned members of this exact route from the locked Core
    /// base while changed members are replaced atomically.
    pub fn retain_unstaged_base_route_sources(
        &mut self,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        self.lifecycle
            .retain_unstaged_route_members(&self.route_identity)?;
        Ok(())
    }
}

type CoreRecordEmission<L> = CorePreparedCapture<<L as CaptureLifecycleSink>::Preparation>;
pub type CoreRecordEmissionBatch<L> = CorePreparedBatch<<L as CaptureLifecycleSink>::Preparation>;
pub type CoreRecordEmissionBatchBuilder<L> =
    CorePreparedBatchBuilder<<L as CaptureLifecycleSink>::Preparation>;
pub const SOURCE_BACKED_CORE_RECORD_BATCH_MAX_RECORDS: usize = CORE_RECORD_BATCH_MAX_RECORDS;

impl<L: CaptureLifecycleSink> SourceBackedGenerationSink<'_, L> {
    /// Returns the capture-facing lookup pinned to this writer's base generation.
    pub fn base_event_lookup(&self) -> L::BaseLookup {
        self.lifecycle.base_event_lookup()
    }

    pub fn route_resources(&self) -> SourceBackedRouteResources {
        self.resources.clone()
    }

    pub fn report_current_source_progress(
        &mut self,
        progress: SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()> {
        self.current_source_progress
            .as_mut()
            .map_or(Ok(()), |report| report(progress))
    }

    /// Publishes route activity independently from accepted Core accounting.
    /// All stages share one route-wide cadence. Activity inside the gate is
    /// coalesced to the latest stage rather than bypassing the rate limit.
    pub(crate) fn report_intermediate_activity(
        &mut self,
        stage: SourceBackedCurrentSourceProgressStage,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        if self.current_source_progress.is_none() {
            self.intermediate_progress_pending_stage = None;
            return Ok(());
        }
        self.intermediate_progress_pending_stage = Some(stage);
        self.flush_intermediate_activity()
    }

    pub(crate) fn flush_intermediate_activity(
        &mut self,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        let Some(stage) = self.intermediate_progress_pending_stage else {
            return Ok(());
        };
        let now = Instant::now();
        let interval_elapsed = self
            .intermediate_progress_last_emitted_at
            .is_none_or(|last| {
                now.saturating_duration_since(last) >= SOURCE_BACKED_INTERMEDIATE_PROGRESS_INTERVAL
            });
        if !interval_elapsed {
            return Ok(());
        }
        self.report_current_source_progress(SourceBackedCurrentSourceProgress::new(stage))
            .map_err(SourceBackedCoordinatorError::Progress)?;
        self.intermediate_progress_last_emitted_at = Some(now);
        self.intermediate_progress_pending_stage = None;
        Ok(())
    }

    fn report_index_writer_activity(&mut self) -> SourceBackedCoordinatorResult<(), L::Error> {
        if self.resources.intermediate_activity_generation() == 0 {
            return Ok(());
        }
        self.resources
            .record_intermediate_activity(SourceBackedCurrentSourceProgressStage::IndexWriting);
        self.report_intermediate_activity(SourceBackedCurrentSourceProgressStage::IndexWriting)
    }

    pub fn base_source(&self, source: &SourceKey) -> Option<&CertifiedSource> {
        self.lifecycle.base_source(source)
    }

    /// Returns the retained certificate only when this exact route owns the
    /// requested source. Both the route members and generation sources are in
    /// canonical source-identity order, so lifecycle implementations can keep
    /// this lookup logarithmic without materializing the rest of the route.
    pub fn base_route_source(&self, source: &SourceKey) -> Option<&CertifiedSource> {
        let snapshot = self.lifecycle.base_snapshot()?;
        let key = source.identity().digest();
        let owned = std::iter::once(&self.route_identity)
            .chain(self.base_route_aliases.iter())
            .any(|route_identity| {
                snapshot.source_route(route_identity).is_some_and(|route| {
                    route
                        .sources()
                        .binary_search_by_key(&key, |candidate| candidate.identity().digest())
                        .ok()
                        .and_then(|index| route.sources().get(index))
                        .is_some_and(|candidate| candidate.exact_descriptor_eq(source))
                })
            });
        if !owned {
            return None;
        }
        self.lifecycle.base_source(source)
    }

    pub fn pinned_append_base(&self, source: &SourceKey) -> Option<L::PinnedAppendBase> {
        std::iter::once(&self.route_identity)
            .chain(self.base_route_aliases.iter())
            .find_map(|route_identity| self.lifecycle.pinned_append_base(route_identity, source))
    }

    /// Returns only the prior certified sources retained by this route. A
    /// provider route must not infer ownership from the provider family alone:
    /// another retained route may intentionally cover the same input tree.
    pub fn base_route_sources(
        &self,
    ) -> SourceBackedCoordinatorResult<HashMap<SourceKey, CertifiedSource>, L::Error> {
        let Some(snapshot) = self.lifecycle.base_snapshot() else {
            return Ok(HashMap::new());
        };
        let mut sources = HashMap::new();
        for route_identity in
            std::iter::once(&self.route_identity).chain(self.base_route_aliases.iter())
        {
            let Some(route) = snapshot.source_route(route_identity) else {
                continue;
            };
            for source in route.sources() {
                let certificate = snapshot
                    .sources()
                    .iter()
                    .find(|candidate| candidate.observation().source().exact_descriptor_eq(source))
                    .cloned()
                    .ok_or_else(|| {
                        L::invariant_error("source-route snapshot names a missing certified source")
                    })?;
                if sources.insert(source.clone(), certificate).is_some() {
                    return Err(SourceBackedCoordinatorError::Index(L::invariant_error(
                        "source-route migration aliases overlap on one certified source",
                    )));
                }
            }
        }
        Ok(sources)
    }

    /// Whether an exact source is retained or has already been claimed by a
    /// different route in this refresh. Such a source is outside this route's
    /// mutation authority even when its selected filesystem root overlaps.
    pub fn source_owned_by_other_route(&self, source: &SourceKey) -> bool {
        let owned_in_attempt = self.owners.values().any(|owner| {
            owner.route_index != self.route_index && owner.source.exact_descriptor_eq(source)
        });
        owned_in_attempt
            || self.lifecycle.base_snapshot().is_some_and(|snapshot| {
                snapshot.source_routes().any(|route| {
                    route.route_identity() != &self.route_identity
                        && !self.base_route_aliases.contains(route.route_identity())
                        && route
                            .sources()
                            .iter()
                            .any(|candidate| candidate.exact_descriptor_eq(source))
                })
            })
    }

    pub fn begin_source(
        &mut self,
        source: SourceKey,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        self.claim_present(&source)?;
        self.lifecycle.begin_source_replace(source)?;
        Ok(())
    }

    pub fn begin_source_append(
        &mut self,
        source: SourceKey,
    ) -> SourceBackedCoordinatorResult<&CertifiedSource, L::Error> {
        self.claim_present(&source)?;
        self.lifecycle
            .begin_source_append(source)
            .map_err(Into::into)
    }

    pub fn begin_source_append_from_base(
        &mut self,
        base: L::PinnedAppendBase,
    ) -> SourceBackedCoordinatorResult<&CertifiedSource, L::Error> {
        let source = L::pinned_append_base_source(&base)
            .observation()
            .source()
            .clone();
        self.claim_present(&source)?;
        self.lifecycle
            .begin_source_append_from_base(base)
            .map_err(Into::into)
    }

    pub fn add_core_record(
        &mut self,
        record: CoreRecord,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        let progress = CoreRecordProgress::from_record(&record);
        let emission =
            CoreRecordEmission::<L>::new(record, &self.resources, &self.core_record_preparer)
                .map_err(SourceBackedRouteError::from)
                .map_err(SourceBackedCoordinatorError::CoreEmission)?;
        self.accept_core_record_emission(emission)?;
        self.report_record_progress(
            1,
            0,
            std::slice::from_ref(&progress.session_id),
            progress.messages,
            progress.tool_calls,
        )
    }

    pub fn add_core_record_emission(
        &mut self,
        emission: CoreRecordEmission<L>,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        self.accept_core_record_emission(emission)?;
        self.report_record_progress(1, 0, &[], 0, 0)
    }

    fn accept_core_record_emission(
        &mut self,
        emission: CoreRecordEmission<L>,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        let (prepared, reservation) = emission.into_prepared();
        self.report_index_writer_activity()?;
        self.lifecycle.add_prepared(prepared)?;
        self.flush_intermediate_activity()?;
        drop(reservation);
        Ok(())
    }

    pub fn add_core_records_with_completed_bytes(
        &mut self,
        records: Vec<CoreRecord>,
        completed_bytes: u64,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        let accepted_records = u64::try_from(records.len()).map_err(|_| {
            SourceBackedCoordinatorError::CoreEmission(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                "Core-record page count overflowed",
            ))
        })?;
        let mut progress = CoreRecordBatchProgress::default();
        for record in records {
            progress.push(CoreRecordProgress::from_record(&record));
            let emission =
                CoreRecordEmission::<L>::new(record, &self.resources, &self.core_record_preparer)
                    .map_err(SourceBackedRouteError::from)
                    .map_err(SourceBackedCoordinatorError::CoreEmission)?;
            self.accept_core_record_emission(emission)?;
        }
        self.report_record_progress(
            accepted_records,
            completed_bytes,
            &progress.session_ids,
            progress.messages,
            progress.tool_calls,
        )
    }

    pub fn add_core_record_emission_batch(
        &mut self,
        batch: CoreRecordEmissionBatch<L>,
        completed_bytes: u64,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        let accepted_records = u64::try_from(batch.len()).map_err(|_| {
            SourceBackedCoordinatorError::CoreEmission(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                "Core-record emission batch count overflowed",
            ))
        })?;
        let (prepared_records, reservation) = batch.into_prepared();
        for prepared in prepared_records {
            self.report_index_writer_activity()?;
            self.lifecycle.add_prepared(prepared)?;
            self.flush_intermediate_activity()?;
        }
        drop(reservation);
        // Parallel workers already publish exact history facts before their
        // rendezvous. Reconcile source-local bytes once, after the whole batch
        // is admitted, without recounting those facts.
        self.report_record_progress(accepted_records, completed_bytes, &[], 0, 0)
    }

    pub fn record_logical_source_failure(
        &mut self,
        source: SourceKey,
        failure: SourceBackedRouteError,
        carried_forward: bool,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        self.record_logical_source_failure_with_empty_admission(
            source,
            failure,
            carried_forward,
            false,
        )
    }

    pub fn record_logical_source_quarantine(
        &mut self,
        source: SourceKey,
        failure: SourceBackedRouteError,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        self.record_logical_source_failure_with_empty_admission(source, failure, false, true)
    }

    fn record_logical_source_failure_with_empty_admission(
        &mut self,
        source: SourceKey,
        failure: SourceBackedRouteError,
        carried_forward: bool,
        allows_empty_route: bool,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        if !failure.kind.is_logical_source_failure() {
            return Err(SourceBackedCoordinatorError::InvalidLogicalSourceFailure {
                detail: "non-local failure was reported as a logical-source outcome",
            });
        }
        let class = failure.kind.source_failure_class().ok_or(
            SourceBackedCoordinatorError::InvalidLogicalSourceFailure {
                detail: "logical-source failure has no stable failure class",
            },
        )?;
        self.logical_source_failures
            .record(SourceBackedLogicalSourceFailure {
                route_index: self.route_index,
                route_identity: self.route_identity.clone(),
                source,
                class,
                carried_forward,
                detail: diagnostics::bounded_text(
                    &failure.detail,
                    MAX_SOURCE_BACKED_FAILURE_DETAIL_BYTES,
                ),
                allows_empty_route,
            });
        Ok(())
    }

    fn record_rejection_with_provenance(
        &mut self,
        rejection: SourceBackedRecordRejectionDraft,
        committed: bool,
    ) {
        self.record_rejections.record(SourceBackedRecordRejection {
            route_index: self.route_index,
            route_identity: self.route_identity.clone(),
            source: rejection.source,
            provider: rejection.provider,
            source_selector: diagnostics::bounded_text(
                &rejection.source_selector,
                MAX_SOURCE_BACKED_FAILURE_SELECTOR_BYTES,
            ),
            line_number: rejection.line_number,
            payload_type: rejection.payload_type.map(|payload_type| {
                diagnostics::bounded_text(
                    &payload_type,
                    MAX_SOURCE_BACKED_REJECTION_PAYLOAD_TYPE_BYTES,
                )
            }),
            class: rejection.class,
            detail: diagnostics::bounded_text(
                &rejection.detail,
                MAX_SOURCE_BACKED_FAILURE_DETAIL_BYTES,
            ),
            committed,
        });
    }

    pub fn record_rejections(&mut self, rejections: SourceBackedRecordRejectionDrafts) {
        self.record_rejections_with_provenance(rejections, true);
    }

    pub fn record_failed_attempt_rejections(
        &mut self,
        rejections: SourceBackedRecordRejectionDrafts,
    ) {
        self.record_rejections_with_provenance(rejections, false);
    }

    fn record_rejections_with_provenance(
        &mut self,
        rejections: SourceBackedRecordRejectionDrafts,
        committed: bool,
    ) {
        let (rejections, omitted) = rejections.into_parts();
        for rejection in rejections {
            self.record_rejection_with_provenance(rejection, committed);
        }
        self.record_omitted_rejections(omitted);
    }

    pub fn record_omitted_rejections(&mut self, omitted: usize) {
        self.record_rejections.record_omitted(omitted);
    }

    pub fn report_completed_bytes(
        &mut self,
        bytes: u64,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        self.report_record_progress(0, bytes, &[], 0, 0)
    }

    /// Enables optional exact accounting from observations the scanner already
    /// owns. The total is piggybacked on its next normal progress callback.
    pub fn enable_exact_scan_accounting(&mut self, bytes: u64) {
        self.exact_scan_total_bytes = Some(bytes);
        self.exact_scan_accounting_enabled = true;
    }

    /// Reports the ordinary certified-byte delta while allowing a scanner to
    /// account for a terminal physical suffix that is not publication data.
    pub fn report_completed_bytes_with_exact(
        &mut self,
        bytes: u64,
        exact_bytes: Option<u64>,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        if exact_bytes.is_none() {
            self.exact_scan_total_bytes = None;
            self.exact_scan_accounting_enabled = false;
        }
        self.report_record_progress_with_exact(0, bytes, exact_bytes, &[], 0, 0)
    }

    fn report_record_progress(
        &mut self,
        accepted_records: u64,
        completed_bytes: u64,
        session_ids: &[[u8; 32]],
        messages: u64,
        tool_calls: u64,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        self.report_record_progress_with_exact(
            accepted_records,
            completed_bytes,
            None,
            session_ids,
            messages,
            tool_calls,
        )
    }

    fn report_record_progress_with_exact(
        &mut self,
        accepted_records: u64,
        completed_bytes: u64,
        exact_completed_bytes: Option<u64>,
        session_ids: &[[u8; 32]],
        messages: u64,
        tool_calls: u64,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        if let Some(report_progress) = self.record_progress.as_mut() {
            let mut session_transitions = Vec::new();
            for session_id in session_ids {
                if self.last_progress_session_id.as_ref() != Some(session_id) {
                    session_transitions.push(*session_id);
                    self.last_progress_session_id = Some(*session_id);
                }
            }
            report_progress(SourceBackedRecordProgressDelta {
                accepted_records,
                completed_bytes,
                exact_total_bytes: self.exact_scan_total_bytes.take(),
                exact_completed_bytes: self
                    .exact_scan_accounting_enabled
                    .then_some(exact_completed_bytes.unwrap_or(completed_bytes)),
                session_ids: session_transitions,
                messages,
                tool_calls,
            })?;
        }
        Ok(())
    }

    pub fn certify_source(
        &mut self,
        certificate: CertifiedSource,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        let source = certificate.observation().source().clone();
        self.lifecycle.certify_source(certificate.clone())?;
        self.record_revalidation(&source, SourceBackedRouteRevalidation::Source(certificate))?;
        Ok(())
    }

    pub fn certify_source_append(
        &mut self,
        append: CertifiedSourceAppend,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        let certificate = append.current().clone();
        let source = certificate.observation().source().clone();
        self.lifecycle.certify_source_append(append)?;
        self.record_revalidation(&source, SourceBackedRouteRevalidation::Source(certificate))?;
        Ok(())
    }

    pub fn retain_source(
        &mut self,
        certificate: CertifiedSource,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        self.claim_present(certificate.observation().source())?;
        let source = certificate.observation().source().clone();
        self.lifecycle.retain_source(certificate.clone())?;
        self.record_revalidation(&source, SourceBackedRouteRevalidation::Source(certificate))?;
        Ok(())
    }

    pub fn certify_complete_inventory(
        &mut self,
        inventory: CertifiedSourceInventory,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        self.lifecycle
            .certify_complete_inventory(inventory.clone())?;
        self.complete_inventories.push(CompleteInventoryOwner {
            route_index: self.route_index,
            inventory,
        });
        Ok(())
    }

    pub fn delete_source(
        &mut self,
        deletion: CertifiedSourceDeletion,
        inventory: CertifiedSourceInventory,
    ) -> SourceBackedCoordinatorResult<SourceBackedDeletionDisposition, L::Error> {
        if !deletion.verifies(&inventory) {
            return Err(SourceBackedCoordinatorError::InvalidDeletionWitness);
        }
        self.claim_absent(deletion.source())?;
        self.lifecycle
            .delete_source(deletion.clone(), inventory.clone())?;
        self.record_revalidation(
            deletion.source(),
            SourceBackedRouteRevalidation::Deletion(Box::new(deletion.clone())),
        )?;
        self.applied_removals.push(SourceBackedCertifiedRemoval {
            deletion,
            inventory,
        });
        Ok(SourceBackedDeletionDisposition::Deleted)
    }

    pub fn replace_source(
        &mut self,
        certificate: CertifiedSource,
        core_records: impl IntoIterator<Item = CoreRecord>,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        self.begin_source(certificate.observation().source().clone())?;
        for record in core_records {
            self.add_core_record(record)?;
        }
        self.certify_source(certificate)
    }

    pub fn claim_present(
        &mut self,
        source: &SourceKey,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        self.claim(source, true)
    }

    pub fn claim_absent(
        &mut self,
        source: &SourceKey,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        self.claim(source, false)
    }

    fn claim(
        &mut self,
        source: &SourceKey,
        present: bool,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        let digest = source.identity().digest();
        match self.owners.get(&digest) {
            Some(owner)
                if owner.route_index != self.route_index
                    || !owner.source.exact_descriptor_eq(source) =>
            {
                return Err(SourceBackedCoordinatorError::DuplicateSourceOwner {
                    source_id: source.identity().to_string(),
                });
            }
            Some(owner) if owner.present != present => {
                return Err(SourceBackedCoordinatorError::DuplicateSourceOwner {
                    source_id: source.identity().to_string(),
                });
            }
            Some(_) => {}
            None => {
                self.owners.insert(
                    digest,
                    SourceOwner {
                        route_index: self.route_index,
                        source: source.clone(),
                        present,
                        revalidation: None,
                    },
                );
            }
        }
        Ok(())
    }

    fn record_revalidation(
        &mut self,
        source: &SourceKey,
        revalidation: SourceBackedRouteRevalidation,
    ) -> SourceBackedCoordinatorResult<(), L::Error> {
        let owner = self
            .owners
            .get_mut(&source.identity().digest())
            .filter(|owner| {
                owner.route_index == self.route_index
                    && owner.source.exact_descriptor_eq(source)
                    && owner.revalidation.is_none()
            })
            .ok_or_else(|| L::invariant_error("source certification lost its route-local owner"))?;
        owner.revalidation = Some(revalidation);
        Ok(())
    }
}
