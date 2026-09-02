//! Provider-local runtime used only by native JSONL projection tests.
//!
//! Projection tests exercise dialect parsing and Core-record construction
//! directly. They do not open, stage, or commit a capture lifecycle, but the
//! generic JSONL adapter requires a lifecycle type. Keeping this inert
//! implementation here avoids a test-only cycle back to `ctx-history-capture`.

use std::{collections::BTreeSet, path::Path};

use ctx_history_capture_model::SourceRouteIdentity;
use ctx_history_capture_runtime::{
    BaseEventLookup, CaptureCommitReceipt, CaptureLifecycleOpenOutcome, CaptureLifecycleSink,
    CaptureRevalidationTarget, CaptureRouteRef, CaptureSourceAggregateRef, CoreMaterialization,
    CorePreparationFailureKind, CorePreparationPort, ImmutableCaptureSnapshot, PresentCaptureRoute,
};
use ctx_history_core::{
    CertifiedSource, CertifiedSourceAppend, CertifiedSourceDeletion, CertifiedSourceInventory,
    CoreRecord, SourceKey,
};
use ctx_history_jsonl::JsonlFamilyRuntime;

use crate::{NativeJsonlError, NativeJsonlRuntime};

pub(crate) struct NativeJsonlTestRuntime;

impl JsonlFamilyRuntime for NativeJsonlTestRuntime {
    type Error = NativeJsonlError;
    type Lifecycle = NativeJsonlTestLifecycle;
    type WorkerServices = ();
    type RouteControl = ();

    fn begin_worker_leaf(_services: &mut Self::WorkerServices) {}
}

impl NativeJsonlRuntime for NativeJsonlTestRuntime {}

#[derive(Clone, Default)]
pub(crate) struct NativeJsonlTestLookup;

impl BaseEventLookup for NativeJsonlTestLookup {
    type Error = NativeJsonlError;

    fn contains(&self, _event_id: uuid::Uuid) -> Result<bool, Self::Error> {
        Ok(false)
    }
}

#[derive(Clone, Default)]
pub(super) struct NativeJsonlTestPreparation;

pub(super) struct NativeJsonlTestPrepared {
    source: SourceKey,
    encoded_bytes: usize,
}

impl CorePreparationPort for NativeJsonlTestPreparation {
    type Prepared = NativeJsonlTestPrepared;
    type Draft = CoreRecord;
    type Failure = NativeJsonlError;

    fn prepare(&self, record: CoreRecord) -> Result<Self::Prepared, Self::Failure> {
        let encoded_bytes = record
            .encode_stored()
            .map_err(|error| NativeJsonlError::InvalidPayload(error.to_string()))?
            .len();
        Ok(NativeJsonlTestPrepared {
            source: record.source,
            encoded_bytes,
        })
    }

    fn prepare_draft(&self, record: CoreRecord) -> Result<Self::Draft, Self::Failure> {
        record
            .validate_contract()
            .map_err(|error| NativeJsonlError::InvalidPayload(error.to_string()))?;
        Ok(record)
    }

    fn materialize_draft(
        &self,
        draft: Self::Draft,
        maximum_encoded_bytes: usize,
    ) -> Result<CoreMaterialization<Self::Prepared, Self::Draft>, Self::Failure> {
        let encoded_bytes = draft
            .encode_stored()
            .map_err(|error| NativeJsonlError::InvalidPayload(error.to_string()))?
            .len();
        if encoded_bytes > maximum_encoded_bytes {
            return Ok(CoreMaterialization::CapacityExceeded(Box::new(draft)));
        }
        Ok(CoreMaterialization::Prepared(NativeJsonlTestPrepared {
            source: draft.source,
            encoded_bytes,
        }))
    }

    fn prepared_source<'a>(&self, prepared: &'a Self::Prepared) -> &'a SourceKey {
        &prepared.source
    }

    fn encoded_bytes(&self, prepared: &Self::Prepared) -> usize {
        prepared.encoded_bytes
    }

    fn failure_kind(&self, _failure: &Self::Failure) -> CorePreparationFailureKind {
        CorePreparationFailureKind::InvalidSource
    }
}

#[derive(Clone, Default)]
pub(super) struct NativeJsonlTestSnapshot;

impl ImmutableCaptureSnapshot for NativeJsonlTestSnapshot {
    fn sources(&self) -> &[CertifiedSource] {
        &[]
    }

    fn source_aggregates(&self) -> impl ExactSizeIterator<Item = CaptureSourceAggregateRef<'_>> {
        std::iter::empty()
    }

    fn source_routes(&self) -> impl ExactSizeIterator<Item = CaptureRouteRef<'_>> {
        std::iter::empty()
    }

    fn source_route(&self, _route_identity: &SourceRouteIdentity) -> Option<CaptureRouteRef<'_>> {
        None
    }
}

pub(super) struct NativeJsonlTestLifecycle;

impl NativeJsonlTestLifecycle {
    fn unsupported() -> NativeJsonlError {
        NativeJsonlError::SystemInvariant(
            "provider-local JSONL test runtime does not execute capture lifecycle operations",
        )
    }
}

impl CaptureLifecycleSink for NativeJsonlTestLifecycle {
    type Error = NativeJsonlError;
    type OpenOptions = ();
    type BaseLookup = NativeJsonlTestLookup;
    type Preparation = NativeJsonlTestPreparation;
    type PinnedAppendBase = CertifiedSource;
    type CommittedSnapshot = NativeJsonlTestSnapshot;
    type VerifiedPublication = ();
    type Snapshot<'a> = NativeJsonlTestSnapshot;

    fn invariant_error(detail: &'static str) -> Self::Error {
        NativeJsonlError::SystemInvariant(detail)
    }

    fn open(
        _root: &Path,
        _options: Self::OpenOptions,
    ) -> Result<CaptureLifecycleOpenOutcome<Self>, Self::Error> {
        Err(Self::unsupported())
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
        NativeJsonlTestLookup
    }

    fn core_preparation(&self) -> Self::Preparation {
        NativeJsonlTestPreparation
    }

    fn set_route_plan(
        &mut self,
        _selected: BTreeSet<SourceRouteIdentity>,
        _carried_from_base: BTreeSet<SourceRouteIdentity>,
    ) -> Result<(), Self::Error> {
        Err(Self::unsupported())
    }

    fn begin_route_stage(
        &mut self,
        _route_identity: SourceRouteIdentity,
    ) -> Result<(), Self::Error> {
        Err(Self::unsupported())
    }

    fn retain_unstaged_route_members(
        &mut self,
        _route_identity: &SourceRouteIdentity,
    ) -> Result<(), Self::Error> {
        Err(Self::unsupported())
    }

    fn route_retains_unstaged_members(&self, _route_identity: &SourceRouteIdentity) -> bool {
        false
    }

    fn register_route_revalidation(
        &mut self,
        _route_identity: SourceRouteIdentity,
        _revalidate: impl Fn() -> bool + Send + 'static,
    ) -> Result<(), Self::Error> {
        Err(Self::unsupported())
    }

    fn visit_revalidation_targets<E>(
        &self,
        _visit: impl for<'a> FnMut(CaptureRevalidationTarget<'a>) -> Result<(), E>,
    ) -> Result<Result<(), E>, Self::Error> {
        Ok(Ok(()))
    }

    fn finish_route_stage(
        &mut self,
        _route_identity: &SourceRouteIdentity,
    ) -> Result<(), Self::Error> {
        Err(Self::unsupported())
    }

    fn rollback_route_stage(
        &mut self,
        _route_identity: &SourceRouteIdentity,
    ) -> Result<(), Self::Error> {
        Err(Self::unsupported())
    }

    fn authorize_carried_route_retirement(
        &mut self,
        _replacement_route: &SourceRouteIdentity,
        _retired_route: &SourceRouteIdentity,
    ) -> Result<(), Self::Error> {
        Err(Self::unsupported())
    }

    fn retire_carried_route(
        &mut self,
        _replacement_route: &SourceRouteIdentity,
        _retired_route: &SourceRouteIdentity,
    ) -> Result<Vec<SourceKey>, Self::Error> {
        Err(Self::unsupported())
    }

    fn begin_source_replace(&mut self, _source: SourceKey) -> Result<(), Self::Error> {
        Err(Self::unsupported())
    }

    fn begin_source_append(&mut self, _source: SourceKey) -> Result<&CertifiedSource, Self::Error> {
        Err(Self::unsupported())
    }

    fn begin_source_append_from_base(
        &mut self,
        _base: Self::PinnedAppendBase,
    ) -> Result<&CertifiedSource, Self::Error> {
        Err(Self::unsupported())
    }

    fn add_prepared(&mut self, _prepared: NativeJsonlTestPrepared) -> Result<(), Self::Error> {
        Err(Self::unsupported())
    }

    fn certify_source(&mut self, _certificate: CertifiedSource) -> Result<(), Self::Error> {
        Err(Self::unsupported())
    }

    fn certify_source_append(&mut self, _append: CertifiedSourceAppend) -> Result<(), Self::Error> {
        Err(Self::unsupported())
    }

    fn retain_source(&mut self, _certificate: CertifiedSource) -> Result<(), Self::Error> {
        Err(Self::unsupported())
    }

    fn certify_complete_inventory(
        &mut self,
        _inventory: CertifiedSourceInventory,
    ) -> Result<(), Self::Error> {
        Err(Self::unsupported())
    }

    fn delete_source(
        &mut self,
        _deletion: CertifiedSourceDeletion,
        _inventory: CertifiedSourceInventory,
    ) -> Result<(), Self::Error> {
        Err(Self::unsupported())
    }

    fn carry_failed_route(
        &mut self,
        _route_identity: &SourceRouteIdentity,
    ) -> Result<bool, Self::Error> {
        Err(Self::unsupported())
    }

    fn observe_missing_route(
        &mut self,
        _route_identity: SourceRouteIdentity,
        _observed_at_unix_ms: u64,
        _revalidate_missing: impl Fn() -> bool + Send + 'static,
    ) -> Result<(), Self::Error> {
        Err(Self::unsupported())
    }

    fn set_present_routes(
        &mut self,
        _routes: impl IntoIterator<Item = PresentCaptureRoute>,
    ) -> Result<(), Self::Error> {
        Err(Self::unsupported())
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
        Err(Self::unsupported())
    }
}
