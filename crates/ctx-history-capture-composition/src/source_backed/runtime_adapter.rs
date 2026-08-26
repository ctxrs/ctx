use ctx_history_capture_runtime::{
    BaseEventLookup, CaptureCommitOutcome, CaptureCommitReceipt, CaptureLifecycleOpenOutcome,
    CaptureLifecycleRecovery, CaptureLifecycleSink, CapturePublicationContext,
    CapturePublicationDisposition, CaptureRevalidationTarget, CaptureRouteRef,
    CaptureSourceAggregateRef, CoreMaterialization, CorePreparationFailureKind,
    CorePreparationPort, CoreRouteByteLease, CoreRouteResourceKind, ImmutableCaptureSnapshot,
    PresentCaptureRoute, VerifiedCapture,
};
use ctx_history_core::{
    CertifiedSource, CertifiedSourceAppend, CertifiedSourceDeletion, CertifiedSourceInventory,
    CoreRecord, SourceKey,
};
use ctx_history_index::{
    AppliedProviderRoot, BaseEventIdentityLookup, CommitReceipt, CoreRecordPreparer,
    GenerationBaseCertifiedSource, GenerationManifest, GenerationWriter,
    GenerationWriterOpenOutcome, IndexError, PreparedCoreRecord, PreparedCoreRecordDraft,
    PreparedCoreRecordMaterialization, PublicationDisposition, PublicationMetadataContext,
    PublicationStage, PublishedGeneration, RevalidationTarget, SourceRouteSnapshot, VerifiedIndex,
    WriterOptions,
};
use std::{collections::BTreeSet, path::Path, sync::Arc};
use uuid::Uuid;

pub(crate) type IndexCaptureResult<T> = Result<T, IndexError>;

pub(crate) fn index_writer_invariant(detail: &'static str) -> IndexError {
    IndexError::WriterInvariant(detail)
}
pub(crate) fn invalid_source_route_identity() -> IndexError {
    IndexError::InvalidSourceRouteIdentity
}

pub(crate) fn index_source_route_identity(
    identity: Result<
        ctx_history_capture_model::SourceRouteIdentity,
        ctx_history_capture_model::SourceRouteIdentityError,
    >,
) -> IndexCaptureResult<ctx_history_capture_model::SourceRouteIdentity> {
    identity.map_err(|_| invalid_source_route_identity())
}

/// Capture-local adapter for the index-owned immutable base identity view.
///
/// This is deliberately a transparent compile-time boundary: capture callers
/// keep the concrete type, while the index remains the sole lookup authority.
#[repr(transparent)]
#[derive(Clone)]
pub struct IndexBaseEventLookup(BaseEventIdentityLookup);

impl From<BaseEventIdentityLookup> for IndexBaseEventLookup {
    fn from(lookup: BaseEventIdentityLookup) -> Self {
        Self(lookup)
    }
}

impl BaseEventLookup for IndexBaseEventLookup {
    type Error = IndexError;

    fn contains(&self, event_id: Uuid) -> Result<bool, Self::Error> {
        self.0.contains(event_id)
    }
}

/// Transparent capture adapter for the index-owned preparation authority.
///
/// All preparation remains static and concrete: the runtime envelope sees the
/// port type as a generic parameter and never erases this index value.
#[repr(transparent)]
#[derive(Clone)]
pub struct IndexCorePreparation(CoreRecordPreparer);

impl std::fmt::Debug for IndexCorePreparation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("IndexCorePreparation(..)")
    }
}

impl From<CoreRecordPreparer> for IndexCorePreparation {
    fn from(preparer: CoreRecordPreparer) -> Self {
        Self(preparer)
    }
}

impl CorePreparationPort for IndexCorePreparation {
    type Prepared = PreparedCoreRecord;
    type Draft = PreparedCoreRecordDraft;
    type Failure = IndexError;

    fn prepare(&self, record: CoreRecord) -> Result<Self::Prepared, Self::Failure> {
        self.0.prepare(record)
    }

    fn prepare_draft(&self, record: CoreRecord) -> Result<Self::Draft, Self::Failure> {
        self.0.prepare_draft(record)
    }

    fn materialize_draft(
        &self,
        draft: Self::Draft,
        maximum_encoded_bytes: usize,
    ) -> Result<CoreMaterialization<Self::Prepared, Self::Draft>, Self::Failure> {
        Ok(match draft.materialize(maximum_encoded_bytes)? {
            PreparedCoreRecordMaterialization::Prepared(prepared) => {
                CoreMaterialization::Prepared(prepared)
            }
            PreparedCoreRecordMaterialization::CapacityExceeded(draft) => {
                CoreMaterialization::CapacityExceeded(draft)
            }
        })
    }

    fn prepared_source<'a>(&self, prepared: &'a Self::Prepared) -> &'a SourceKey {
        prepared.source()
    }

    fn encoded_bytes(&self, prepared: &Self::Prepared) -> usize {
        prepared.encoded_core_bytes()
    }

    fn failure_kind(&self, failure: &Self::Failure) -> CorePreparationFailureKind {
        index_preparation_failure_kind(failure)
    }
}

fn index_preparation_failure_kind(failure: &IndexError) -> CorePreparationFailureKind {
    if matches!(
        failure,
        IndexError::ProjectionContract(_)
            | IndexError::CoreRecord(_)
            | IndexError::CoreRecordPolicyRevisionMismatch { .. }
            | IndexError::EmptyDocumentField { .. }
            | IndexError::DocumentFieldTooLarge { .. }
    ) {
        CorePreparationFailureKind::InvalidSource
    } else {
        CorePreparationFailureKind::Internal
    }
}

pub(crate) type SourceBackedRouteResourceKind = CoreRouteResourceKind;
pub(crate) type SourceBackedRouteByteReservation = CoreRouteByteLease;

/// The index owns automatic missing-route grace. Capture supplies the route
/// observation and terminal callback, while this adapter binds the established
/// policy without exposing an index-specific knob.
pub(crate) const AUTOMATIC_ROUTE_DELETION_MISSING_OBSERVATIONS: u32 =
    ctx_history_index::policy::AUTOMATIC_ROUTE_DELETION_GRACE_OBSERVATIONS;

#[doc(hidden)]
#[cfg(any(test, feature = "test-support"))]
pub const fn automatic_route_deletion_missing_observations_for_test() -> u32 {
    AUTOMATIC_ROUTE_DELETION_MISSING_OBSERVATIONS
}

/// Sole concrete lifecycle exchange between source-backed capture and index.
#[repr(transparent)]
pub struct IndexCaptureLifecycle(GenerationWriter);

impl CaptureLifecycleSink for IndexCaptureLifecycle {
    type Error = IndexError;
    type OpenOptions = WriterOptions;
    type BaseLookup = IndexBaseEventLookup;
    type Preparation = IndexCorePreparation;
    type PinnedAppendBase = GenerationBaseCertifiedSource;
    type CommittedSnapshot = CommittedIndexManifestView;
    type VerifiedPublication = IndexVerifiedCapture;
    type Snapshot<'a> = BorrowedIndexManifestView<'a>;

    fn invariant_error(detail: &'static str) -> Self::Error {
        index_writer_invariant(detail)
    }

    fn open(
        root: &Path,
        options: Self::OpenOptions,
    ) -> IndexCaptureResult<CaptureLifecycleOpenOutcome<Self>> {
        Ok(match GenerationWriter::open(root, options)? {
            GenerationWriterOpenOutcome::Ready(writer) => {
                CaptureLifecycleOpenOutcome::Ready(Self(writer))
            }
            GenerationWriterOpenOutcome::RecoveredCommittedMigration { writer, .. } => {
                CaptureLifecycleOpenOutcome::Ready(Self(writer))
            }
            GenerationWriterOpenOutcome::CommittedMigrationRecoveryRequired { recovery } => {
                CaptureLifecycleOpenOutcome::RecoveryRequired {
                    recovery: CaptureLifecycleRecovery::new(
                        recovery.generation_id().to_owned(),
                        recovery.detail().to_owned(),
                    ),
                }
            }
        })
    }

    fn base_snapshot(&self) -> Option<Self::Snapshot<'_>> {
        self.0.base_manifest().map(IndexManifestView::borrowed)
    }

    fn base_source(&self, source: &SourceKey) -> Option<&CertifiedSource> {
        self.0.base_manifest().and_then(|manifest| {
            let key = source.identity().digest();
            let index = manifest
                .sources
                .binary_search_by_key(&key, |candidate| {
                    #[cfg(test)]
                    super::record_base_source_manifest_visit();
                    candidate.observation().source().identity().digest()
                })
                .ok()?;
            manifest
                .sources
                .get(index)
                .filter(|candidate| candidate.observation().source().exact_descriptor_eq(source))
        })
    }

    fn pinned_append_base(
        &self,
        route_identity: &ctx_history_capture_model::SourceRouteIdentity,
        source: &SourceKey,
    ) -> Option<Self::PinnedAppendBase> {
        self.0
            .generation_base_certified_source(route_identity, source)
    }

    fn pinned_append_base_source(base: &Self::PinnedAppendBase) -> &CertifiedSource {
        base.certificate()
    }

    fn base_event_lookup(&self) -> Self::BaseLookup {
        self.0.base_event_identity_lookup().into()
    }

    fn core_preparation(&self) -> Self::Preparation {
        self.0.core_record_preparer().into()
    }

    fn set_route_plan(
        &mut self,
        selected: BTreeSet<ctx_history_capture_model::SourceRouteIdentity>,
        carried_from_base: BTreeSet<ctx_history_capture_model::SourceRouteIdentity>,
    ) -> Result<(), Self::Error> {
        self.0.set_source_route_plan(selected, carried_from_base)
    }

    fn begin_route_stage(
        &mut self,
        route_identity: ctx_history_capture_model::SourceRouteIdentity,
    ) -> Result<(), Self::Error> {
        self.0.begin_source_route_stage(route_identity)
    }

    fn retain_unstaged_route_members(
        &mut self,
        route_identity: &ctx_history_capture_model::SourceRouteIdentity,
    ) -> Result<(), Self::Error> {
        self.0.retain_unstaged_source_route_members(route_identity)
    }

    fn route_retains_unstaged_members(
        &self,
        route_identity: &ctx_history_capture_model::SourceRouteIdentity,
    ) -> bool {
        self.0.source_route_retains_unstaged_members(route_identity)
    }

    fn register_route_revalidation(
        &mut self,
        route_identity: ctx_history_capture_model::SourceRouteIdentity,
        revalidate: impl Fn() -> bool + Send + 'static,
    ) -> Result<(), Self::Error> {
        self.0
            .register_source_route_publication_revalidation(route_identity, revalidate)
    }

    fn visit_revalidation_targets<E>(
        &self,
        mut visit: impl for<'a> FnMut(CaptureRevalidationTarget<'a>) -> Result<(), E>,
    ) -> Result<Result<(), E>, Self::Error> {
        let targets = self.0.active_source_route_revalidation_targets()?;
        for target in targets {
            let target = match target {
                RevalidationTarget::Source(source) => CaptureRevalidationTarget::Source(source),
                RevalidationTarget::Deletion(deletion) => {
                    CaptureRevalidationTarget::Deletion(deletion)
                }
            };
            if let Err(error) = visit(target) {
                return Ok(Err(error));
            }
        }
        Ok(Ok(()))
    }

    fn finish_route_stage(
        &mut self,
        route_identity: &ctx_history_capture_model::SourceRouteIdentity,
    ) -> Result<(), Self::Error> {
        self.0.finish_source_route_stage(route_identity)
    }

    fn rollback_route_stage(
        &mut self,
        route_identity: &ctx_history_capture_model::SourceRouteIdentity,
    ) -> Result<(), Self::Error> {
        self.0.rollback_source_route_stage(route_identity)
    }

    fn authorize_carried_route_retirement(
        &mut self,
        replacement_route: &ctx_history_capture_model::SourceRouteIdentity,
        retired_route: &ctx_history_capture_model::SourceRouteIdentity,
    ) -> Result<(), Self::Error> {
        self.0
            .authorize_carried_source_route_retirement(replacement_route, retired_route)
    }

    fn retire_carried_route(
        &mut self,
        replacement_route: &ctx_history_capture_model::SourceRouteIdentity,
        retired_route: &ctx_history_capture_model::SourceRouteIdentity,
    ) -> Result<Vec<SourceKey>, Self::Error> {
        self.0
            .retire_carried_source_route(replacement_route, retired_route)
    }

    fn begin_source_replace(&mut self, source: SourceKey) -> Result<(), Self::Error> {
        self.0.begin_source(source)
    }

    fn begin_source_append(&mut self, source: SourceKey) -> Result<&CertifiedSource, Self::Error> {
        self.0.begin_source_append(source)
    }

    fn begin_source_append_from_base(
        &mut self,
        base: Self::PinnedAppendBase,
    ) -> Result<&CertifiedSource, Self::Error> {
        self.0.begin_source_append_from_base(base)
    }

    fn add_prepared(&mut self, prepared: PreparedCoreRecord) -> Result<(), Self::Error> {
        self.0.add_prepared_core_record(prepared)
    }

    fn certify_source(&mut self, certificate: CertifiedSource) -> Result<(), Self::Error> {
        self.0.certify_source(certificate)
    }

    fn certify_source_append(&mut self, append: CertifiedSourceAppend) -> Result<(), Self::Error> {
        self.0.certify_source_append(append)
    }

    fn retain_source(&mut self, certificate: CertifiedSource) -> Result<(), Self::Error> {
        self.0.retain_source(certificate)
    }

    fn certify_complete_inventory(
        &mut self,
        inventory: CertifiedSourceInventory,
    ) -> Result<(), Self::Error> {
        self.0.certify_complete_inventory(inventory)
    }

    fn delete_source(
        &mut self,
        deletion: CertifiedSourceDeletion,
        inventory: CertifiedSourceInventory,
    ) -> Result<(), Self::Error> {
        self.0.delete_source(deletion, inventory)
    }

    fn carry_failed_route(
        &mut self,
        route_identity: &ctx_history_capture_model::SourceRouteIdentity,
    ) -> Result<bool, Self::Error> {
        self.0.carry_failed_source_route_from_base(route_identity)
    }

    fn observe_missing_route(
        &mut self,
        route_identity: ctx_history_capture_model::SourceRouteIdentity,
        observed_at_unix_ms: u64,
        revalidate_missing: impl Fn() -> bool + Send + 'static,
    ) -> Result<(), Self::Error> {
        self.0
            .observe_certified_missing_route(
                route_identity,
                observed_at_unix_ms,
                AUTOMATIC_ROUTE_DELETION_MISSING_OBSERVATIONS,
                revalidate_missing,
            )
            .map(|_| ())
    }

    fn set_present_routes(
        &mut self,
        routes: impl IntoIterator<Item = PresentCaptureRoute>,
    ) -> Result<(), Self::Error> {
        let routes = routes
            .into_iter()
            .map(|route| {
                let (identity, sources) = route.into_parts();
                SourceRouteSnapshot::present(identity, sources)
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.0.set_present_source_routes(routes)
    }

    fn commit<F, I>(
        self,
        mut revalidate: F,
        revalidate_inventory: I,
    ) -> Result<CaptureCommitReceipt<Self::CommittedSnapshot>, Self::Error>
    where
        F: FnMut(CaptureRevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
    {
        self.0
            .commit_with_complete_inventory_revalidation(
                |target| revalidate(index_revalidation_target(target)),
                revalidate_inventory,
            )
            .map(capture_runtime_commit_receipt)
    }

    fn commit_with_metadata<F, I, M>(
        self,
        mut revalidate: F,
        revalidate_inventory: I,
        metadata_factory: M,
    ) -> Result<CaptureCommitOutcome<Self::CommittedSnapshot, Self::VerifiedPublication>, Self::Error>
    where
        F: FnMut(CaptureRevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
        M: for<'a> FnOnce(
            CapturePublicationContext<'a, Self::Snapshot<'a>>,
        ) -> Result<Vec<u8>, Self::Error>,
    {
        self.0
            .commit_with_complete_inventory_revalidation_and_publication_metadata(
                |target| revalidate(index_revalidation_target(target)),
                revalidate_inventory,
                |context| metadata_factory(capture_publication_context(context)),
            )
            .map(capture_commit_outcome)
    }
}

impl IndexCaptureLifecycle {
    pub(crate) fn begin_route_cohort_stage(
        &mut self,
        cohort_identity: ctx_history_capture_model::SourceRouteIdentity,
    ) -> Result<(), IndexError> {
        self.0.begin_source_route_cohort_stage(cohort_identity)
    }

    pub(crate) fn finish_route_cohort_stage(&mut self) -> Result<(), IndexError> {
        self.0.finish_source_route_cohort_stage()
    }

    pub(crate) fn rollback_route_cohort_stage(&mut self) -> Result<(), IndexError> {
        self.0.rollback_source_route_cohort_stage()
    }

    pub(crate) fn set_applied_provider_roots(
        &mut self,
        automatic_provider_discovery: bool,
        config_digest: String,
        roots: Vec<AppliedProviderRoot>,
    ) -> Result<(), IndexError> {
        self.0
            .set_applied_provider_roots(automatic_provider_discovery, config_digest, roots)
    }

    pub(crate) fn finalize_applied_provider_roots(
        &mut self,
        automatic_provider_discovery: bool,
        config_digest: String,
        roots: Vec<AppliedProviderRoot>,
    ) -> Result<(), IndexError> {
        self.0
            .finalize_applied_provider_roots(automatic_provider_discovery, config_digest, roots)
    }

    pub(crate) fn set_authorized_topology_route_retirements(
        &mut self,
        routes: BTreeSet<ctx_history_capture_model::SourceRouteIdentity>,
    ) -> Result<(), IndexError> {
        self.0.set_authorized_topology_route_retirements(routes)
    }

    pub(crate) fn commit_with_metadata_and_progress<F, I, M, P>(
        self,
        mut revalidate: F,
        revalidate_inventory: I,
        metadata_factory: M,
        report_progress: P,
    ) -> Result<IndexCaptureCommitOutcome, IndexError>
    where
        F: FnMut(CaptureRevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
        M: for<'a> FnOnce(
            CapturePublicationContext<'a, BorrowedIndexManifestView<'a>>,
        ) -> Result<Vec<u8>, IndexError>,
        P: FnMut(PublicationStage) -> Result<(), IndexError>,
    {
        self.0
            .commit_with_complete_inventory_revalidation_and_publication_metadata_and_progress(
                |target| revalidate(index_revalidation_target(target)),
                revalidate_inventory,
                |context| metadata_factory(capture_publication_context(context)),
                report_progress,
            )
            .map(capture_commit_outcome)
    }
}

fn index_revalidation_target(target: RevalidationTarget<'_>) -> CaptureRevalidationTarget<'_> {
    match target {
        RevalidationTarget::Source(source) => CaptureRevalidationTarget::Source(source),
        RevalidationTarget::Deletion(deletion) => CaptureRevalidationTarget::Deletion(deletion),
    }
}

/// Borrowed or move-owned projection of the index manifest for capture-only
/// publication facts. This is the sole concrete manifest exchange in Stage 4.
enum IndexManifestStorage<'a> {
    Borrowed(&'a GenerationManifest),
    Committed(Arc<GenerationManifest>),
}

/// Publicly neutral capture snapshot backed by an index manifest. Its concrete
/// index storage remains private to this runtime adapter.
pub struct IndexManifestView<'a>(IndexManifestStorage<'a>);

pub type BorrowedIndexManifestView<'a> = IndexManifestView<'a>;
pub type CommittedIndexManifestView = IndexManifestView<'static>;

/// Opaque move-owned index pin crossing the capture boundary. The adapter is
/// the only code that can expose its concrete verified index.
pub struct IndexVerifiedCapture(VerifiedIndex);

impl IndexVerifiedCapture {
    fn new(verified_index: VerifiedIndex) -> Self {
        Self(verified_index)
    }

    pub fn into_verified_index(self) -> VerifiedIndex {
        self.0
    }
}

impl std::fmt::Debug for IndexVerifiedCapture {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("IndexVerifiedCapture(..)")
    }
}

pub type IndexCaptureVerifiedPin = VerifiedCapture<IndexVerifiedCapture>;

/// Capture's move-owned receipt. The concrete index manifest remains private
/// behind the neutral snapshot projection.
pub struct IndexCaptureCommitReceipt {
    pub generation_id: String,
    pub opstamp: u64,
    pub indexed_documents: u64,
    pub certified_sources: usize,
    pub certified_source_bytes: u64,
    snapshot: CommittedIndexManifestView,
}
pub(crate) type IndexCaptureCommitOutcome =
    CaptureCommitOutcome<CommittedIndexManifestView, IndexVerifiedCapture>;

impl IndexCaptureCommitReceipt {
    pub(crate) fn new(receipt: CaptureCommitReceipt<CommittedIndexManifestView>) -> Self {
        let (
            generation_id,
            opstamp,
            indexed_documents,
            certified_sources,
            certified_source_bytes,
            snapshot,
        ) = receipt.into_parts();
        Self {
            generation_id,
            opstamp,
            indexed_documents,
            certified_sources,
            certified_source_bytes,
            snapshot,
        }
    }

    pub fn snapshot(&self) -> &CommittedIndexManifestView {
        &self.snapshot
    }

    pub fn into_parts(self) -> (String, u64, u64, usize, u64, CommittedIndexManifestView) {
        (
            self.generation_id,
            self.opstamp,
            self.indexed_documents,
            self.certified_sources,
            self.certified_source_bytes,
            self.snapshot,
        )
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn manifest(&self) -> &GenerationManifest {
        self.snapshot.manifest()
    }
}

impl std::fmt::Debug for IndexCaptureCommitReceipt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IndexCaptureCommitReceipt")
            .field("generation_id", &self.generation_id)
            .field("opstamp", &self.opstamp)
            .field("indexed_documents", &self.indexed_documents)
            .field("certified_sources", &self.certified_sources)
            .field("certified_source_bytes", &self.certified_source_bytes)
            .finish_non_exhaustive()
    }
}

impl<'a> IndexManifestView<'a> {
    pub(crate) fn borrowed(manifest: &'a GenerationManifest) -> Self {
        Self(IndexManifestStorage::Borrowed(manifest))
    }

    fn manifest(&self) -> &GenerationManifest {
        match &self.0 {
            IndexManifestStorage::Borrowed(manifest) => manifest,
            IndexManifestStorage::Committed(manifest) => manifest,
        }
    }

    /// Publication-only view of the pinned configured-root selector contract.
    /// The executor uses it to carry predecessor membership while an
    /// incompatible same-name replacement is still pending.
    pub(crate) fn provider_roots(&self) -> &[AppliedProviderRoot] {
        self.manifest().provider_roots()
    }
}

impl IndexManifestView<'static> {
    fn committed(manifest: Arc<GenerationManifest>) -> Self {
        Self(IndexManifestStorage::Committed(manifest))
    }
}

impl std::fmt::Debug for IndexManifestView<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("IndexManifestView(..)")
    }
}

impl ImmutableCaptureSnapshot for IndexManifestView<'_> {
    fn sources(&self) -> &[ctx_history_core::CertifiedSource] {
        &self.manifest().sources
    }

    fn source_aggregates(&self) -> impl ExactSizeIterator<Item = CaptureSourceAggregateRef<'_>> {
        self.manifest()
            .core_record_aggregates
            .iter()
            .map(|aggregate| {
                CaptureSourceAggregateRef::new(
                    aggregate.source_identity_digest(),
                    aggregate.indexed_documents(),
                    aggregate.core_record_accumulator(),
                )
            })
    }

    fn source_routes(&self) -> impl ExactSizeIterator<Item = CaptureRouteRef<'_>> {
        self.manifest().source_routes().iter().map(|route| {
            CaptureRouteRef::new(
                route.route_identity(),
                route.sources(),
                route.missing_state().is_some(),
            )
        })
    }

    fn source_route(
        &self,
        route_identity: &ctx_history_capture_model::SourceRouteIdentity,
    ) -> Option<CaptureRouteRef<'_>> {
        self.manifest().source_route(route_identity).map(|route| {
            CaptureRouteRef::new(
                route.route_identity(),
                route.sources(),
                route.missing_state().is_some(),
            )
        })
    }
}

pub(crate) fn capture_publication_context<'a>(
    context: PublicationMetadataContext<'a>,
) -> CapturePublicationContext<'a, BorrowedIndexManifestView<'a>> {
    let snapshot = IndexManifestView::borrowed(context.manifest());
    CapturePublicationContext::new(context.generation_id(), snapshot)
}

fn capture_runtime_commit_receipt(
    receipt: CommitReceipt,
) -> CaptureCommitReceipt<CommittedIndexManifestView> {
    let (
        generation_id,
        opstamp,
        indexed_documents,
        certified_sources,
        certified_source_bytes,
        manifest,
    ) = receipt.into_parts();
    CaptureCommitReceipt::new(
        generation_id,
        opstamp,
        indexed_documents,
        certified_sources,
        certified_source_bytes,
        IndexManifestView::committed(manifest),
    )
}

pub(crate) fn capture_commit_outcome(published: PublishedGeneration) -> IndexCaptureCommitOutcome {
    let (receipt, disposition, verified_index) = published.into_parts();
    let (
        generation_id,
        opstamp,
        indexed_documents,
        certified_sources,
        certified_source_bytes,
        manifest,
    ) = receipt.into_parts();
    CaptureCommitOutcome::new(
        CaptureCommitReceipt::new(
            generation_id,
            opstamp,
            indexed_documents,
            certified_sources,
            certified_source_bytes,
            IndexManifestView::committed(manifest),
        ),
        match disposition {
            PublicationDisposition::Published => CapturePublicationDisposition::Published,
            PublicationDisposition::Reused => CapturePublicationDisposition::Reused,
        },
        VerifiedCapture::new(IndexVerifiedCapture::new(verified_index)),
    )
}

#[cfg(test)]
mod tests;
