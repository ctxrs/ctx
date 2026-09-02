use std::{collections::BTreeSet, path::Path};

use ctx_history_capture_model::SourceRouteIdentity;
use ctx_history_capture_runtime::{
    CaptureCommitOutcome, CaptureCommitReceipt, CaptureLifecycleOpenOutcome, CaptureLifecycleSink,
    CapturePublicationDisposition, CaptureRevalidationTarget, CaptureRouteRef,
    CaptureSourceAggregateRef, ImmutableCaptureSnapshot, PresentCaptureRoute, VerifiedCapture,
};
use ctx_history_core::{
    CertifiedSource, CertifiedSourceAppend, CertifiedSourceDeletion, CertifiedSourceInventory,
    CoreRecord, SourceKey,
};

use crate::registration::tests::{NoopLookup, NoopPreparation};

use super::test_route_identity;

#[derive(Clone, Default)]
pub(super) struct TestSnapshot {
    sources: Vec<CertifiedSource>,
    route_identity: Option<SourceRouteIdentity>,
    route_sources: Vec<SourceKey>,
}

impl TestSnapshot {
    fn with_sources(mut sources: Vec<CertifiedSource>) -> Self {
        sources.sort_by_key(|source| source.observation().source().identity().digest());
        let route_sources = sources
            .iter()
            .map(|source| source.observation().source().clone())
            .collect();
        Self {
            sources,
            route_identity: Some(test_route_identity()),
            route_sources,
        }
    }
}

impl ImmutableCaptureSnapshot for TestSnapshot {
    fn sources(&self) -> &[CertifiedSource] {
        &self.sources
    }

    fn source_aggregates(&self) -> impl ExactSizeIterator<Item = CaptureSourceAggregateRef<'_>> {
        std::iter::empty()
    }

    fn source_routes(&self) -> impl ExactSizeIterator<Item = CaptureRouteRef<'_>> {
        self.route_identity
            .as_ref()
            .map(|identity| CaptureRouteRef::new(identity, &self.route_sources, false))
            .into_iter()
    }

    fn source_route(&self, route_identity: &SourceRouteIdentity) -> Option<CaptureRouteRef<'_>> {
        self.route_identity
            .as_ref()
            .filter(|identity| *identity == route_identity)
            .map(|identity| CaptureRouteRef::new(identity, &self.route_sources, false))
    }
}

#[derive(Default)]
pub(super) struct TestLifecycle {
    base_sources: Vec<CertifiedSource>,
    current_source: Option<SourceKey>,
    records: Vec<CoreRecord>,
    certified_sources: Vec<CertifiedSource>,
}

impl TestLifecycle {
    pub(super) fn with_base(base_sources: Vec<CertifiedSource>) -> Self {
        Self {
            base_sources,
            ..Self::default()
        }
    }

    pub(super) fn sources(&self) -> Vec<CertifiedSource> {
        let mut sources = self.certified_sources.clone();
        sources.sort_by_key(|source| source.observation().source().identity().digest());
        sources
    }

    fn snapshot(&self) -> TestSnapshot {
        TestSnapshot::with_sources(self.sources())
    }

    fn commit_receipt(self) -> CaptureCommitReceipt<TestSnapshot> {
        let snapshot = self.snapshot();
        let indexed_documents = snapshot
            .sources
            .iter()
            .map(|source| source.counts().indexed_documents)
            .sum();
        CaptureCommitReceipt::new(
            "sqlite-inventory-test-generation".to_owned(),
            1,
            indexed_documents,
            snapshot.sources.len(),
            snapshot
                .sources
                .iter()
                .map(|source| source.counts().certified_bytes)
                .sum(),
            snapshot,
        )
    }
}

impl CaptureLifecycleSink for TestLifecycle {
    type Error = std::io::Error;
    type OpenOptions = ();
    type BaseLookup = NoopLookup;
    type Preparation = NoopPreparation;
    type PinnedAppendBase = CertifiedSource;
    type CommittedSnapshot = TestSnapshot;
    type VerifiedPublication = ();
    type Snapshot<'a> = TestSnapshot;

    fn invariant_error(detail: &'static str) -> Self::Error {
        std::io::Error::other(detail)
    }

    fn open(
        _root: &Path,
        _options: Self::OpenOptions,
    ) -> Result<CaptureLifecycleOpenOutcome<Self>, Self::Error> {
        Ok(CaptureLifecycleOpenOutcome::Ready(Self::default()))
    }

    fn base_snapshot(&self) -> Option<Self::Snapshot<'_>> {
        (!self.base_sources.is_empty())
            .then(|| TestSnapshot::with_sources(self.base_sources.clone()))
    }

    fn base_source(&self, source: &SourceKey) -> Option<&CertifiedSource> {
        self.base_sources
            .iter()
            .find(|candidate| candidate.observation().source().exact_descriptor_eq(source))
    }

    fn pinned_append_base(
        &self,
        _route_identity: &SourceRouteIdentity,
        source: &SourceKey,
    ) -> Option<Self::PinnedAppendBase> {
        self.base_source(source).cloned()
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
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn begin_route_stage(
        &mut self,
        _route_identity: SourceRouteIdentity,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn retain_unstaged_route_members(
        &mut self,
        _route_identity: &SourceRouteIdentity,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn route_retains_unstaged_members(&self, _route_identity: &SourceRouteIdentity) -> bool {
        false
    }

    fn register_route_revalidation(
        &mut self,
        _route_identity: SourceRouteIdentity,
        _revalidate: impl Fn() -> bool + Send + 'static,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn visit_revalidation_targets<E>(
        &self,
        mut visit: impl for<'a> FnMut(CaptureRevalidationTarget<'a>) -> Result<(), E>,
    ) -> Result<Result<(), E>, Self::Error> {
        for source in &self.certified_sources {
            if let Err(error) = visit(CaptureRevalidationTarget::Source(source)) {
                return Ok(Err(error));
            }
        }
        Ok(Ok(()))
    }

    fn finish_route_stage(
        &mut self,
        _route_identity: &SourceRouteIdentity,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn rollback_route_stage(
        &mut self,
        _route_identity: &SourceRouteIdentity,
    ) -> Result<(), Self::Error> {
        self.current_source = None;
        Ok(())
    }

    fn authorize_carried_route_retirement(
        &mut self,
        _replacement_route: &SourceRouteIdentity,
        _retired_route: &SourceRouteIdentity,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn retire_carried_route(
        &mut self,
        _replacement_route: &SourceRouteIdentity,
        _retired_route: &SourceRouteIdentity,
    ) -> Result<Vec<SourceKey>, Self::Error> {
        Ok(Vec::new())
    }

    fn begin_source_replace(&mut self, source: SourceKey) -> Result<(), Self::Error> {
        self.current_source = Some(source);
        Ok(())
    }

    fn begin_source_append(&mut self, source: SourceKey) -> Result<&CertifiedSource, Self::Error> {
        self.current_source = Some(source.clone());
        self.base_sources
            .iter()
            .find(|candidate| {
                candidate
                    .observation()
                    .source()
                    .exact_descriptor_eq(&source)
            })
            .ok_or_else(|| std::io::Error::other("append source has no base"))
    }

    fn begin_source_append_from_base(
        &mut self,
        base: Self::PinnedAppendBase,
    ) -> Result<&CertifiedSource, Self::Error> {
        self.begin_source_append(base.observation().source().clone())
    }

    fn add_prepared(&mut self, prepared: CoreRecord) -> Result<(), Self::Error> {
        self.records.push(prepared);
        Ok(())
    }

    fn certify_source(&mut self, certificate: CertifiedSource) -> Result<(), Self::Error> {
        self.certified_sources.push(certificate);
        self.current_source = None;
        Ok(())
    }

    fn certify_source_append(&mut self, append: CertifiedSourceAppend) -> Result<(), Self::Error> {
        self.certified_sources.push(append.into_current());
        self.current_source = None;
        Ok(())
    }

    fn retain_source(&mut self, certificate: CertifiedSource) -> Result<(), Self::Error> {
        self.certified_sources.push(certificate);
        Ok(())
    }

    fn certify_complete_inventory(
        &mut self,
        _inventory: CertifiedSourceInventory,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn delete_source(
        &mut self,
        _deletion: CertifiedSourceDeletion,
        _inventory: CertifiedSourceInventory,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn carry_failed_route(
        &mut self,
        _route_identity: &SourceRouteIdentity,
    ) -> Result<bool, Self::Error> {
        Ok(false)
    }

    fn observe_missing_route(
        &mut self,
        _route_identity: SourceRouteIdentity,
        _observed_at_unix_ms: u64,
        _revalidate_missing: impl Fn() -> bool + Send + 'static,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn set_present_routes(
        &mut self,
        _routes: impl IntoIterator<Item = PresentCaptureRoute>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn commit<F, I>(
        self,
        _revalidate: F,
        _revalidate_inventory: I,
    ) -> Result<CaptureCommitReceipt<Self::CommittedSnapshot>, Self::Error>
    where
        F: FnMut(CaptureRevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
    {
        Ok(self.commit_receipt())
    }
}
