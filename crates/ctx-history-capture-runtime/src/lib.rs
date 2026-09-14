//! Runtime-neutral contracts used by capture implementations.
//!
//! This crate intentionally owns no provider, source, JSONL, or index
//! implementation. Capture-side adapters select concrete lookup and Core
//! preparation types at compile time, so this boundary adds neither dynamic
//! dispatch nor storage.

mod source_backed;

pub use source_backed::*;

use std::{
    collections::BTreeSet,
    error::Error,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use ctx_history_capture_model::{CoreRecordBatchProgress, CoreRecordProgress, SourceRouteIdentity};
use ctx_history_core::{
    CertifiedSource, CertifiedSourceAppend, CertifiedSourceDeletion, CertifiedSourceInventory,
    CoreRecord, SourceKey,
};
use uuid::Uuid;

/// A borrowed source-route member of an immutable capture snapshot.
///
/// The route and its members are held by the snapshot. This reference makes
/// the capture boundary independent of the persisted manifest representation.
#[derive(Clone, Copy)]
pub struct CaptureRouteRef<'a> {
    route_identity: &'a SourceRouteIdentity,
    sources: &'a [SourceKey],
    missing: bool,
}

impl<'a> CaptureRouteRef<'a> {
    pub fn new(
        route_identity: &'a SourceRouteIdentity,
        sources: &'a [SourceKey],
        missing: bool,
    ) -> Self {
        Self {
            route_identity,
            sources,
            missing,
        }
    }

    pub fn route_identity(self) -> &'a SourceRouteIdentity {
        self.route_identity
    }

    pub fn sources(self) -> &'a [SourceKey] {
        self.sources
    }

    pub fn is_missing(self) -> bool {
        self.missing
    }
}

impl std::fmt::Debug for CaptureRouteRef<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CaptureRouteRef")
            .field("route_identity", self.route_identity)
            .field("source_count", &self.sources.len())
            .field("missing", &self.missing)
            .finish()
    }
}

/// A borrowed Core-record aggregate aligned with one certified source in an
/// immutable capture snapshot.
#[derive(Clone, Copy)]
pub struct CaptureSourceAggregateRef<'a> {
    source_identity_digest: &'a str,
    indexed_documents: u64,
    core_record_accumulator: &'a str,
}

impl<'a> CaptureSourceAggregateRef<'a> {
    pub fn new(
        source_identity_digest: &'a str,
        indexed_documents: u64,
        core_record_accumulator: &'a str,
    ) -> Self {
        Self {
            source_identity_digest,
            indexed_documents,
            core_record_accumulator,
        }
    }

    pub fn source_identity_digest(self) -> &'a str {
        self.source_identity_digest
    }

    pub fn indexed_documents(self) -> u64 {
        self.indexed_documents
    }

    pub fn core_record_accumulator(self) -> &'a str {
        self.core_record_accumulator
    }
}

impl std::fmt::Debug for CaptureSourceAggregateRef<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CaptureSourceAggregateRef")
            .field("source_identity_digest", &self.source_identity_digest)
            .field("indexed_documents", &self.indexed_documents)
            .finish_non_exhaustive()
    }
}

/// Read-only capture facts for one already-verified generation.
///
/// Implementors must preserve source/aggregate positional alignment and use
/// a binary-search route lookup over their canonical route order.
pub trait ImmutableCaptureSnapshot {
    fn sources(&self) -> &[CertifiedSource];

    fn source_aggregates(&self) -> impl ExactSizeIterator<Item = CaptureSourceAggregateRef<'_>>;

    fn source_routes(&self) -> impl ExactSizeIterator<Item = CaptureRouteRef<'_>>;

    fn source_route(&self, route_identity: &SourceRouteIdentity) -> Option<CaptureRouteRef<'_>>;
}

/// A conclusive present-route snapshot. Its member vector is moved from the
/// coordinator into the lifecycle implementation without another collection.
#[derive(Debug)]
pub struct PresentCaptureRoute {
    route_identity: SourceRouteIdentity,
    sources: Vec<SourceKey>,
}

impl PresentCaptureRoute {
    pub fn new(route_identity: SourceRouteIdentity, sources: Vec<SourceKey>) -> Self {
        Self {
            route_identity,
            sources,
        }
    }

    pub fn into_parts(self) -> (SourceRouteIdentity, Vec<SourceKey>) {
        (self.route_identity, self.sources)
    }
}

/// A route-local certificate offered to a capture revalidation visitor.
#[derive(Debug, Clone, Copy)]
pub enum CaptureRevalidationTarget<'a> {
    Source(&'a CertifiedSource),
    Deletion(&'a CertifiedSourceDeletion),
}

/// A recovery that has already committed a predecessor migration but still
/// requires lifecycle recovery before capture can proceed.
#[derive(Debug, Clone)]
pub struct CaptureLifecycleRecovery {
    generation_id: String,
    detail: String,
}

impl CaptureLifecycleRecovery {
    pub fn new(generation_id: String, detail: String) -> Self {
        Self {
            generation_id,
            detail,
        }
    }

    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }

    pub fn into_parts(self) -> (String, String) {
        (self.generation_id, self.detail)
    }
}

/// Outcome of opening a lifecycle while preserving the locked-base recovery
/// distinction required by the coordinator.
pub enum CaptureLifecycleOpenOutcome<L> {
    Ready(L),
    RecoveryRequired { recovery: CaptureLifecycleRecovery },
}

/// Static writer lifecycle for capture coordination.
///
/// All concrete storage and publication authority stays behind this generic
/// port. The nested result returned by `visit_revalidation_targets` preserves
/// acquisition failures separately from capture visitor failures without type
/// erasure.
pub trait CaptureLifecycleSink: Sized {
    type Error: Error + Send + Sync + 'static;
    type OpenOptions;
    type BaseLookup: BaseEventLookup<Error = Self::Error>;
    type Preparation: CorePreparationPort<Failure = Self::Error>;
    type PinnedAppendBase;
    type CommittedSnapshot: ImmutableCaptureSnapshot;
    type VerifiedPublication;
    type Snapshot<'a>: ImmutableCaptureSnapshot
    where
        Self: 'a;

    fn invariant_error(detail: &'static str) -> Self::Error;

    fn open(
        root: &Path,
        options: Self::OpenOptions,
    ) -> Result<CaptureLifecycleOpenOutcome<Self>, Self::Error>;

    fn base_snapshot(&self) -> Option<Self::Snapshot<'_>>;

    fn base_source(&self, source: &SourceKey) -> Option<&CertifiedSource>;

    fn pinned_append_base(
        &self,
        route_identity: &SourceRouteIdentity,
        source: &SourceKey,
    ) -> Option<Self::PinnedAppendBase>;

    fn pinned_append_base_source(base: &Self::PinnedAppendBase) -> &CertifiedSource;

    fn base_event_lookup(&self) -> Self::BaseLookup;

    fn core_preparation(&self) -> Self::Preparation;

    fn set_route_plan(
        &mut self,
        selected: BTreeSet<SourceRouteIdentity>,
        carried_from_base: BTreeSet<SourceRouteIdentity>,
    ) -> Result<(), Self::Error>;

    fn begin_route_stage(&mut self, route_identity: SourceRouteIdentity)
        -> Result<(), Self::Error>;

    fn retain_unstaged_route_members(
        &mut self,
        route_identity: &SourceRouteIdentity,
    ) -> Result<(), Self::Error>;

    fn route_retains_unstaged_members(&self, route_identity: &SourceRouteIdentity) -> bool;

    fn register_route_revalidation(
        &mut self,
        route_identity: SourceRouteIdentity,
        revalidate: impl Fn() -> bool + Send + 'static,
    ) -> Result<(), Self::Error>;

    fn visit_revalidation_targets<E>(
        &self,
        visit: impl for<'a> FnMut(CaptureRevalidationTarget<'a>) -> Result<(), E>,
    ) -> Result<Result<(), E>, Self::Error>;

    fn finish_route_stage(
        &mut self,
        route_identity: &SourceRouteIdentity,
    ) -> Result<(), Self::Error>;

    fn rollback_route_stage(
        &mut self,
        route_identity: &SourceRouteIdentity,
    ) -> Result<(), Self::Error>;

    fn authorize_carried_route_retirement(
        &mut self,
        replacement_route: &SourceRouteIdentity,
        retired_route: &SourceRouteIdentity,
    ) -> Result<(), Self::Error>;

    fn retire_carried_route(
        &mut self,
        replacement_route: &SourceRouteIdentity,
        retired_route: &SourceRouteIdentity,
    ) -> Result<Vec<SourceKey>, Self::Error>;

    fn begin_source_replace(&mut self, source: SourceKey) -> Result<(), Self::Error>;

    fn begin_source_append(&mut self, source: SourceKey) -> Result<&CertifiedSource, Self::Error>;

    fn begin_source_append_from_base(
        &mut self,
        base: Self::PinnedAppendBase,
    ) -> Result<&CertifiedSource, Self::Error>;

    fn add_prepared(
        &mut self,
        prepared: <Self::Preparation as CorePreparationPort>::Prepared,
    ) -> Result<(), Self::Error>;

    fn certify_source(&mut self, certificate: CertifiedSource) -> Result<(), Self::Error>;

    fn certify_source_append(&mut self, append: CertifiedSourceAppend) -> Result<(), Self::Error>;

    fn retain_source(&mut self, certificate: CertifiedSource) -> Result<(), Self::Error>;

    fn certify_complete_inventory(
        &mut self,
        inventory: CertifiedSourceInventory,
    ) -> Result<(), Self::Error>;

    fn delete_source(
        &mut self,
        deletion: CertifiedSourceDeletion,
        inventory: CertifiedSourceInventory,
    ) -> Result<(), Self::Error>;

    fn carry_failed_route(
        &mut self,
        route_identity: &SourceRouteIdentity,
    ) -> Result<bool, Self::Error>;

    fn observe_missing_route(
        &mut self,
        route_identity: SourceRouteIdentity,
        observed_at_unix_ms: u64,
        revalidate_missing: impl Fn() -> bool + Send + 'static,
    ) -> Result<(), Self::Error>;

    fn set_present_routes(
        &mut self,
        routes: impl IntoIterator<Item = PresentCaptureRoute>,
    ) -> Result<(), Self::Error>;

    fn commit<F, I>(
        self,
        revalidate: F,
        revalidate_inventory: I,
    ) -> Result<CaptureCommitReceipt<Self::CommittedSnapshot>, Self::Error>
    where
        F: FnMut(CaptureRevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool;
}

/// Move-owned facts from one capture commit, parameterized by its immutable
/// snapshot representation.
pub struct CaptureCommitReceipt<S> {
    pub generation_id: String,
    pub opstamp: u64,
    pub indexed_documents: u64,
    pub certified_sources: usize,
    pub certified_source_bytes: u64,
    snapshot: S,
}

impl<S> CaptureCommitReceipt<S> {
    pub fn new(
        generation_id: String,
        opstamp: u64,
        indexed_documents: u64,
        certified_sources: usize,
        certified_source_bytes: u64,
        snapshot: S,
    ) -> Self {
        Self {
            generation_id,
            opstamp,
            indexed_documents,
            certified_sources,
            certified_source_bytes,
            snapshot,
        }
    }

    pub fn snapshot(&self) -> &S {
        &self.snapshot
    }

    pub fn into_parts(self) -> (String, u64, u64, usize, u64, S) {
        let Self {
            generation_id,
            opstamp,
            indexed_documents,
            certified_sources,
            certified_source_bytes,
            snapshot,
        } = self;
        (
            generation_id,
            opstamp,
            indexed_documents,
            certified_sources,
            certified_source_bytes,
            snapshot,
        )
    }
}

impl<S> std::fmt::Debug for CaptureCommitReceipt<S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CaptureCommitReceipt")
            .field("generation_id", &self.generation_id)
            .field("opstamp", &self.opstamp)
            .field("indexed_documents", &self.indexed_documents)
            .field("certified_sources", &self.certified_sources)
            .field("certified_source_bytes", &self.certified_source_bytes)
            .finish_non_exhaustive()
    }
}

/// Whether capture publication advanced the durable generation or exactly
/// reused the active generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapturePublicationDisposition {
    Published,
    Reused,
}

/// An already-open verified capture pin. Its concrete storage type remains
/// opaque at the runtime boundary.
pub struct VerifiedCapture<V> {
    verified: V,
}

impl<V> VerifiedCapture<V> {
    pub fn new(verified: V) -> Self {
        Self { verified }
    }

    pub fn into_inner(self) -> V {
        self.verified
    }
}

impl<V> AsRef<V> for VerifiedCapture<V> {
    fn as_ref(&self) -> &V {
        &self.verified
    }
}

impl<V> std::fmt::Debug for VerifiedCapture<V> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("VerifiedCapture(..)")
    }
}

/// One committed or exactly reused capture receipt together with its already
/// open verified pin.
pub struct CaptureCommitOutcome<S, V> {
    receipt: CaptureCommitReceipt<S>,
    disposition: CapturePublicationDisposition,
    verified: VerifiedCapture<V>,
}

impl<S, V> CaptureCommitOutcome<S, V> {
    pub fn new(
        receipt: CaptureCommitReceipt<S>,
        disposition: CapturePublicationDisposition,
        verified: VerifiedCapture<V>,
    ) -> Self {
        Self {
            receipt,
            disposition,
            verified,
        }
    }

    pub fn receipt(&self) -> &CaptureCommitReceipt<S> {
        &self.receipt
    }

    pub fn disposition(&self) -> CapturePublicationDisposition {
        self.disposition
    }

    pub fn verified(&self) -> &VerifiedCapture<V> {
        &self.verified
    }

    pub fn into_parts(
        self,
    ) -> (
        CaptureCommitReceipt<S>,
        CapturePublicationDisposition,
        VerifiedCapture<V>,
    ) {
        (self.receipt, self.disposition, self.verified)
    }
}

impl<S, V> std::fmt::Debug for CaptureCommitOutcome<S, V> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CaptureCommitOutcome")
            .field("receipt", &self.receipt)
            .field("disposition", &self.disposition)
            .field("verified", &self.verified)
            .finish()
    }
}

/// Looks up exact event identities from an immutable capture base.
pub trait BaseEventLookup: Clone + Send + Sync + 'static {
    type Error: Error + Send + Sync + 'static;

    fn contains(&self, event_id: Uuid) -> Result<bool, Self::Error>;
}

/// Classifies a concrete preparation failure without importing its authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorePreparationFailureKind {
    InvalidSource,
    Internal,
}

/// The result of admitting a prepared draft to an exact byte capacity.
///
/// A returned draft is boxed only on the uncommon capacity-exceeded path so
/// the normal prepared transport remains one contiguous `Vec` allocation.
#[derive(Debug)]
pub enum CoreMaterialization<P, D> {
    Prepared(P),
    CapacityExceeded(Box<D>),
}

/// Static bridge to a concrete Core-record preparation authority.
pub trait CorePreparationPort: Clone + Send + Sync + 'static {
    type Prepared: Send + 'static;
    type Draft: Send + 'static;
    type Failure: Error + Send + Sync + 'static;

    fn prepare(&self, record: CoreRecord) -> Result<Self::Prepared, Self::Failure>;

    fn prepare_draft(&self, record: CoreRecord) -> Result<Self::Draft, Self::Failure>;

    fn materialize_draft(
        &self,
        draft: Self::Draft,
        maximum_encoded_bytes: usize,
    ) -> Result<CoreMaterialization<Self::Prepared, Self::Draft>, Self::Failure>;

    fn prepared_source<'a>(&self, prepared: &'a Self::Prepared) -> &'a SourceKey;

    fn encoded_bytes(&self, prepared: &Self::Prepared) -> usize;

    fn failure_kind(&self, failure: &Self::Failure) -> CorePreparationFailureKind;
}

/// Prepared Core records may retain at most this many exact encoded bytes
/// while crossing a shared worker-to-writer route envelope. Reservations are
/// live, not cumulative: large routes continue streaming after the writer
/// consumes each bounded emission.
pub const CORE_ROUTE_MAX_LIVE_OUTPUT_BYTES: u64 = 256 * 1024 * 1024;

/// A protocol emission carries at most one ordinary JSONL/Codex page worth of
/// Core records. Fan-out projectors split additional batches at this bound.
pub const CORE_RECORD_BATCH_MAX_RECORDS: usize = 64;

/// Replacement-document workers may each stage one provider-bounded logical
/// snapshot. The common document family uses at most four such workers and a
/// 256 MiB per-snapshot bound, making this an explicit aggregate ceiling.
pub const CORE_ROUTE_MAX_PHYSICAL_SCRATCH_BYTES: u64 = 4 * 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreRouteResourceKind {
    CoreOutput,
    LogicalSourceScratch,
}

impl std::fmt::Display for CoreRouteResourceKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::CoreOutput => "live prepared Core-record output",
            Self::LogicalSourceScratch => "physical logical-source scratch",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreRouteResourceError {
    AccountingOverflow {
        kind: CoreRouteResourceKind,
        maximum: u64,
    },
    Unavailable {
        kind: CoreRouteResourceKind,
        maximum: u64,
        observed: u64,
    },
}

impl std::fmt::Display for CoreRouteResourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AccountingOverflow { kind, maximum } => write!(
                formatter,
                "shared route {kind} byte accounting overflowed (maximum {maximum})"
            ),
            Self::Unavailable {
                kind,
                maximum,
                observed,
            } => write!(
                formatter,
                "shared route {kind} byte limit exceeded: maximum {maximum}, observed {observed}"
            ),
        }
    }
}

#[derive(Debug)]
pub enum CorePreparationError<E> {
    Preparation {
        kind: CorePreparationFailureKind,
        failure: E,
    },
    Resource(CoreRouteResourceError),
    Internal(&'static str),
}

impl<E: std::fmt::Display> std::fmt::Display for CorePreparationError<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Preparation { failure, .. } => failure.fmt(formatter),
            Self::Resource(error) => error.fmt(formatter),
            Self::Internal(detail) => formatter.write_str(detail),
        }
    }
}

#[derive(Debug)]
struct CoreRouteByteBudget {
    maximum: u64,
    live: AtomicU64,
}

/// Cloneable resources shared by every scanner worker. Cloning this value
/// never creates another output or physical-scratch allowance.
#[derive(Debug, Clone)]
pub struct CoreRouteResources {
    leaf_worker_budget: usize,
    output: Arc<CoreRouteByteBudget>,
    scratch: Arc<CoreRouteByteBudget>,
}

impl CoreRouteResources {
    pub fn production(leaf_worker_budget: usize) -> Self {
        Self::with_byte_limits(
            leaf_worker_budget,
            CORE_ROUTE_MAX_LIVE_OUTPUT_BYTES,
            CORE_ROUTE_MAX_PHYSICAL_SCRATCH_BYTES,
        )
    }

    fn with_byte_limits(
        leaf_worker_budget: usize,
        maximum_live_output_bytes: u64,
        maximum_physical_scratch_bytes: u64,
    ) -> Self {
        Self {
            leaf_worker_budget: leaf_worker_budget.max(1),
            output: Arc::new(CoreRouteByteBudget {
                maximum: maximum_live_output_bytes,
                live: AtomicU64::new(0),
            }),
            scratch: Arc::new(CoreRouteByteBudget {
                maximum: maximum_physical_scratch_bytes,
                live: AtomicU64::new(0),
            }),
        }
    }

    pub fn for_test(
        leaf_worker_budget: usize,
        maximum_live_output_bytes: u64,
        maximum_physical_scratch_bytes: u64,
    ) -> Self {
        Self::with_byte_limits(
            leaf_worker_budget,
            maximum_live_output_bytes,
            maximum_physical_scratch_bytes,
        )
    }

    pub fn leaf_worker_budget(&self) -> usize {
        self.leaf_worker_budget
    }

    pub fn maximum_bytes(&self, kind: CoreRouteResourceKind) -> u64 {
        match kind {
            CoreRouteResourceKind::CoreOutput => self.output.maximum,
            CoreRouteResourceKind::LogicalSourceScratch => self.scratch.maximum,
        }
    }

    pub fn core_output_batch_reservation_bytes(&self) -> u64 {
        if self.output.maximum == 0 {
            return 0;
        }
        let workers = u64::try_from(self.leaf_worker_budget).unwrap_or(u64::MAX);
        self.output.maximum.checked_div(workers).unwrap_or(0).max(1)
    }

    pub fn reserve(
        &self,
        kind: CoreRouteResourceKind,
        bytes: usize,
    ) -> Result<CoreRouteByteLease, CoreRouteResourceError> {
        let budget = match kind {
            CoreRouteResourceKind::CoreOutput => &self.output,
            CoreRouteResourceKind::LogicalSourceScratch => &self.scratch,
        };
        let bytes =
            u64::try_from(bytes).map_err(|_| CoreRouteResourceError::AccountingOverflow {
                kind,
                maximum: budget.maximum,
            })?;
        let mut live = budget.live.load(Ordering::Acquire);
        loop {
            let Some(next) = live.checked_add(bytes) else {
                return Err(CoreRouteResourceError::AccountingOverflow {
                    kind,
                    maximum: budget.maximum,
                });
            };
            if next > budget.maximum {
                return Err(CoreRouteResourceError::Unavailable {
                    kind,
                    maximum: budget.maximum,
                    observed: next,
                });
            }
            match budget
                .live
                .compare_exchange_weak(live, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    return Ok(CoreRouteByteLease {
                        budget: Arc::clone(budget),
                        bytes,
                    });
                }
                Err(actual) => live = actual,
            }
        }
    }

    pub fn live_bytes(&self, kind: CoreRouteResourceKind) -> u64 {
        match kind {
            CoreRouteResourceKind::CoreOutput => &self.output,
            CoreRouteResourceKind::LogicalSourceScratch => &self.scratch,
        }
        .live
        .load(Ordering::Acquire)
    }
}

#[derive(Debug)]
pub struct CoreRouteByteLease {
    budget: Arc<CoreRouteByteBudget>,
    bytes: u64,
}

impl CoreRouteByteLease {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for CoreRouteByteLease {
    fn drop(&mut self) {
        if self.bytes != 0 {
            self.budget.live.fetch_sub(self.bytes, Ordering::AcqRel);
        }
    }
}

/// A prepared Core record held together with its exact live-byte lease.
pub struct CorePreparedCapture<P: CorePreparationPort> {
    prepared: P::Prepared,
    lease: CoreRouteByteLease,
}

impl<P: CorePreparationPort> std::fmt::Debug for CorePreparedCapture<P>
where
    P::Prepared: std::fmt::Debug,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CorePreparedCapture")
            .field("prepared", &self.prepared)
            .field("reserved_bytes", &self.lease.bytes())
            .finish()
    }
}

impl<P: CorePreparationPort> CorePreparedCapture<P> {
    pub fn new(
        record: CoreRecord,
        resources: &CoreRouteResources,
        port: &P,
    ) -> Result<Self, CorePreparationError<P::Failure>> {
        let prepared = Self::prepare(record, port)?;
        Self::from_prepared(prepared, resources, port)
    }

    pub fn prepare(
        record: CoreRecord,
        port: &P,
    ) -> Result<P::Prepared, CorePreparationError<P::Failure>> {
        port.prepare(record)
            .map_err(|failure| CorePreparationError::Preparation {
                kind: port.failure_kind(&failure),
                failure,
            })
    }

    pub fn prepare_draft(
        record: CoreRecord,
        port: &P,
    ) -> Result<P::Draft, CorePreparationError<P::Failure>> {
        port.prepare_draft(record)
            .map_err(|failure| CorePreparationError::Preparation {
                kind: port.failure_kind(&failure),
                failure,
            })
    }

    #[allow(clippy::type_complexity)]
    pub fn materialize_draft(
        draft: P::Draft,
        maximum_encoded_bytes: usize,
        port: &P,
    ) -> Result<CoreMaterialization<P::Prepared, P::Draft>, CorePreparationError<P::Failure>> {
        port.materialize_draft(draft, maximum_encoded_bytes)
            .map_err(|failure| CorePreparationError::Preparation {
                kind: port.failure_kind(&failure),
                failure,
            })
    }

    pub fn from_prepared(
        prepared: P::Prepared,
        resources: &CoreRouteResources,
        port: &P,
    ) -> Result<Self, CorePreparationError<P::Failure>> {
        let lease = resources
            .reserve(
                CoreRouteResourceKind::CoreOutput,
                port.encoded_bytes(&prepared),
            )
            .map_err(CorePreparationError::Resource)?;
        Ok(Self { prepared, lease })
    }

    pub fn into_prepared(self) -> (P::Prepared, CoreRouteByteLease) {
        let Self { prepared, lease } = self;
        (prepared, lease)
    }
}

/// A mutable worker-local Core-record batch with one shared output lease.
mod prepared_batch;
pub use prepared_batch::*;

#[cfg(test)]
mod tests;
