use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

use ctx_history_capture_model::SourceRouteIdentity;
use ctx_history_capture_runtime::{
    BaseEventLookup, CaptureCommitOutcome, CaptureCommitReceipt, CaptureLifecycleOpenOutcome,
    CapturePublicationDisposition, CaptureRevalidationTarget, CoreMaterialization,
    CorePreparationFailureKind, CorePreparationPort, ImmutableCaptureSnapshot, PresentCaptureRoute,
    SourceBackedCertifiedRemoval, SourceBackedLogicalSourceFailures, SourceBackedRecordRejections,
    SourceBackedRouteResources,
};
use ctx_history_core::{
    CaptureProvider, CertifiedSource, CertifiedSourceAppend, CertifiedSourceDeletion,
    CertifiedSourceInventory, CoreRecord, SourceKey, TypedKey,
};
use rusqlite::Connection;
use uuid::Uuid;

use super::*;

#[derive(Clone, Default)]
pub(crate) struct NoopLookup;

impl BaseEventLookup for NoopLookup {
    type Error = std::io::Error;

    fn contains(&self, _event_id: Uuid) -> std::result::Result<bool, Self::Error> {
        Ok(false)
    }
}

#[derive(Clone, Default)]
pub(crate) struct NoopPreparation;

impl CorePreparationPort for NoopPreparation {
    type Prepared = CoreRecord;
    type Draft = CoreRecord;
    type Failure = std::io::Error;

    fn prepare(&self, record: CoreRecord) -> std::result::Result<Self::Prepared, Self::Failure> {
        Ok(record)
    }

    fn prepare_draft(&self, record: CoreRecord) -> std::result::Result<Self::Draft, Self::Failure> {
        Ok(record)
    }

    fn materialize_draft(
        &self,
        draft: Self::Draft,
        _maximum_encoded_bytes: usize,
    ) -> std::result::Result<CoreMaterialization<Self::Prepared, Self::Draft>, Self::Failure> {
        Ok(CoreMaterialization::Prepared(draft))
    }

    fn prepared_source<'a>(&self, prepared: &'a Self::Prepared) -> &'a SourceKey {
        &prepared.source
    }

    fn encoded_bytes(&self, prepared: &Self::Prepared) -> usize {
        prepared
            .encode_stored()
            .map(|encoded| encoded.len())
            .unwrap_or(0)
    }

    fn failure_kind(&self, _failure: &Self::Failure) -> CorePreparationFailureKind {
        CorePreparationFailureKind::Internal
    }
}

#[derive(Clone, Default)]
pub(crate) struct NoopSnapshot;

impl ImmutableCaptureSnapshot for NoopSnapshot {
    fn sources(&self) -> &[CertifiedSource] {
        &[]
    }

    fn source_aggregates(
        &self,
    ) -> impl ExactSizeIterator<Item = ctx_history_capture_runtime::CaptureSourceAggregateRef<'_>>
    {
        std::iter::empty()
    }

    fn source_routes(
        &self,
    ) -> impl ExactSizeIterator<Item = ctx_history_capture_runtime::CaptureRouteRef<'_>> {
        std::iter::empty()
    }

    fn source_route(
        &self,
        _route_identity: &SourceRouteIdentity,
    ) -> Option<ctx_history_capture_runtime::CaptureRouteRef<'_>> {
        None
    }
}

#[derive(Default)]
pub(crate) struct NoopLifecycle;

impl CaptureLifecycleSink for NoopLifecycle {
    type Error = std::io::Error;
    type OpenOptions = ();
    type BaseLookup = NoopLookup;
    type Preparation = NoopPreparation;
    type PinnedAppendBase = CertifiedSource;
    type CommittedSnapshot = NoopSnapshot;
    type VerifiedPublication = ();
    type Snapshot<'a> = NoopSnapshot;

    fn invariant_error(detail: &'static str) -> Self::Error {
        std::io::Error::other(detail)
    }

    fn open(
        _root: &std::path::Path,
        _options: Self::OpenOptions,
    ) -> std::result::Result<CaptureLifecycleOpenOutcome<Self>, Self::Error> {
        Ok(CaptureLifecycleOpenOutcome::Ready(Self))
    }

    fn base_snapshot(&self) -> Option<Self::Snapshot<'_>> {
        None
    }

    fn base_source(&self, _source: &SourceKey) -> Option<&CertifiedSource> {
        None
    }

    fn pinned_append_base(
        &self,
        _route_identity: &SourceRouteIdentity,
        _source: &SourceKey,
    ) -> Option<Self::PinnedAppendBase> {
        None
    }

    fn pinned_append_base_source(base: &Self::PinnedAppendBase) -> &CertifiedSource {
        base
    }

    fn base_event_lookup(&self) -> Self::BaseLookup {
        NoopLookup
    }

    fn core_preparation(&self) -> Self::Preparation {
        NoopPreparation
    }

    fn set_route_plan(
        &mut self,
        _selected: BTreeSet<SourceRouteIdentity>,
        _carried_from_base: BTreeSet<SourceRouteIdentity>,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn begin_route_stage(
        &mut self,
        _route_identity: SourceRouteIdentity,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn retain_unstaged_route_members(
        &mut self,
        _route_identity: &SourceRouteIdentity,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn route_retains_unstaged_members(&self, _route_identity: &SourceRouteIdentity) -> bool {
        false
    }

    fn register_route_revalidation(
        &mut self,
        _route_identity: SourceRouteIdentity,
        _revalidate: impl Fn() -> bool + Send + 'static,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn visit_revalidation_targets<E>(
        &self,
        _visit: impl for<'a> FnMut(CaptureRevalidationTarget<'a>) -> std::result::Result<(), E>,
    ) -> std::result::Result<std::result::Result<(), E>, Self::Error> {
        Ok(Ok(()))
    }

    fn finish_route_stage(
        &mut self,
        _route_identity: &SourceRouteIdentity,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn rollback_route_stage(
        &mut self,
        _route_identity: &SourceRouteIdentity,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn authorize_carried_route_retirement(
        &mut self,
        _replacement_route: &SourceRouteIdentity,
        _retired_route: &SourceRouteIdentity,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn retire_carried_route(
        &mut self,
        _replacement_route: &SourceRouteIdentity,
        _retired_route: &SourceRouteIdentity,
    ) -> std::result::Result<Vec<SourceKey>, Self::Error> {
        Ok(Vec::new())
    }

    fn begin_source_replace(&mut self, _source: SourceKey) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn begin_source_append(
        &mut self,
        _source: SourceKey,
    ) -> std::result::Result<&CertifiedSource, Self::Error> {
        Err(std::io::Error::other("no append base"))
    }

    fn begin_source_append_from_base(
        &mut self,
        base: Self::PinnedAppendBase,
    ) -> std::result::Result<&CertifiedSource, Self::Error> {
        let _ = base;
        Err(std::io::Error::other("no append base"))
    }

    fn add_prepared(
        &mut self,
        _prepared: <Self::Preparation as CorePreparationPort>::Prepared,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn certify_source(
        &mut self,
        _certificate: CertifiedSource,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn certify_source_append(
        &mut self,
        _append: CertifiedSourceAppend,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn retain_source(
        &mut self,
        _certificate: CertifiedSource,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn certify_complete_inventory(
        &mut self,
        _inventory: CertifiedSourceInventory,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn delete_source(
        &mut self,
        _deletion: CertifiedSourceDeletion,
        _inventory: CertifiedSourceInventory,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn carry_failed_route(
        &mut self,
        _route_identity: &SourceRouteIdentity,
    ) -> std::result::Result<bool, Self::Error> {
        Ok(false)
    }

    fn observe_missing_route(
        &mut self,
        _route_identity: SourceRouteIdentity,
        _observed_at_unix_ms: u64,
        _revalidate_missing: impl Fn() -> bool + Send + 'static,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn set_present_routes(
        &mut self,
        _routes: impl IntoIterator<Item = PresentCaptureRoute>,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn commit<F, I>(
        self,
        _revalidate: F,
        _revalidate_inventory: I,
    ) -> std::result::Result<CaptureCommitReceipt<Self::CommittedSnapshot>, Self::Error>
    where
        F: FnMut(CaptureRevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
    {
        Ok(CaptureCommitReceipt::new(
            "noop-generation".to_owned(),
            1,
            0,
            0,
            0,
            NoopSnapshot,
        ))
    }
}
