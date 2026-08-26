#![allow(
    unused_imports,
    reason = "this module owns the lower provider-facing JSONL compatibility surface"
)]

use std::{path::PathBuf, sync::Arc};

use crate::{
    CaptureError, ProviderBaseEventLookup, ProviderBaseEventLookupError,
    ProviderFallbackEventIdentityState, ProviderJsonlRouteDriver, ProviderJsonlRuntime,
    ProviderRuntimeBinding,
};

pub use ctx_history_jsonl::FallbackEventIdentityMode;
pub use ctx_history_jsonl::{
    bounded_checkpoint_fits, fit_jsonl_activity, jsonl_prefix_digest as prefix_digest,
    jsonl_single_file_inventory, jsonl_terminal_call_id_digest,
    new_jsonl_prefix_hasher as new_prefix_hasher, observe_opened_file,
    observe_opened_file_allow_append, ordered_pending_exchange_entries, read_bounded_record,
    read_bounded_record_complete_and_prefix_sha256,
    read_bounded_record_full_complete_and_prefix_sha256, read_bounded_record_unhashed,
    remember_pending_exchange, restore_hash_pending_exchange_entries,
    restore_ordered_pending_exchange_entries, selected_content_fits as jsonl_selected_content_fits,
    sorted_pending_exchange_entries, take_pending_exchange, JsonlActivityObservedBytes,
    JsonlAppendOccurrenceState, JsonlCheckpoint, JsonlCheckpointedTerminalAuthority,
    JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyExecutionPosition,
    JsonlFamilyInventoryMode, JsonlFamilyMembershipObservation, JsonlFamilyOpenedMember,
    JsonlFamilyOptimizedLeafOutcome, JsonlFamilyProjectionMode, JsonlFamilyProjector,
    JsonlFamilyPublication, JsonlFamilyRejectedLeaf, JsonlFamilyRootMissingMode,
    JsonlFamilySemanticExecutor, JsonlFamilySemanticPage, JsonlFamilySemanticPreflight,
    JsonlFamilySemanticSummary, JsonlFamilyTerminalProof, JsonlFileObservation,
    JsonlOrderedAppendOccurrenceState, JsonlOversizedRecordPolicy, JsonlPage,
    JsonlPendingExchangeLookup, JsonlPendingExchangeRemember, JsonlPendingExchangeState,
    JsonlPhysicalDigest, JsonlPhysicalRecord, JsonlPhysicalStreamPosition, JsonlProbe,
    JsonlRecordEvidence, JsonlRecordFraming, JsonlRecordRef, JsonlResumableSha256,
    JsonlScanOutcome, JsonlSemanticPreflightMode, JsonlSourceChange, JsonlSourceIdentity,
    JsonlTerminalAuthority, JsonlTerminalObservationRegion,
};
#[cfg(feature = "test-support")]
pub use ctx_history_jsonl::{
    checkpoint_admitted_revision_for_test, jsonl_prefix_hash_bytes, reset_jsonl_prefix_hash_bytes,
    revalidate_frozen_prefix, set_after_final_jsonl_prefix_hash_hook,
    set_after_jsonl_append_observation_route_binding_hook, set_after_jsonl_prefix_hash_hook,
    set_after_jsonl_semantic_preflight_hook, set_after_second_jsonl_prefix_hash_hook,
    set_after_standard_zstd_snapshot_hook, set_before_jsonl_terminal_physical_revalidation_hook,
};

pub type ProviderJsonlReader = ctx_history_jsonl::JsonlReader<CaptureError>;
pub type ProviderJsonlPhysicalStream = ctx_history_jsonl::JsonlPhysicalStream<CaptureError>;
pub type ProviderJsonlLeaf = ctx_history_jsonl::JsonlFamilyLeaf<CaptureError>;
pub type ProviderJsonlOpenedMember<'a> =
    ctx_history_jsonl::JsonlFamilyOpenedMember<'a, CaptureError>;
pub type ProviderJsonlInventory = ctx_history_jsonl::JsonlFamilyInventory<CaptureError>;
pub type ProviderJsonlMembershipObservation =
    ctx_history_jsonl::JsonlFamilyMembershipObservation<CaptureError>;
pub type ProviderJsonlTerminalProof = ctx_history_jsonl::JsonlFamilyTerminalProof<CaptureError>;
pub type ProviderJsonlOptimizedLeafOutcome =
    ctx_history_jsonl::JsonlFamilyOptimizedLeafOutcome<CaptureError>;
pub type ProviderJsonlWorkerContext<B> =
    ctx_history_jsonl::JsonlFamilyWorkerContext<ProviderJsonlRuntime<B>>;
pub type ProviderJsonlExecutionIo<B> =
    ctx_history_jsonl::JsonlFamilyExecutionIo<ProviderJsonlRuntime<B>>;
pub type ProviderJsonlAdapter<B> = dyn JsonlFamilyAdapter<Runtime = ProviderJsonlRuntime<B>>;

pub fn encode_bounded_checkpoint(
    prefix: &str,
    checkpoint: &impl serde::Serialize,
    maximum_bytes: usize,
    provider: &str,
) -> crate::Result<ctx_history_core::TypedKey> {
    ctx_history_jsonl::encode_bounded_checkpoint::<CaptureError>(
        prefix,
        checkpoint,
        maximum_bytes,
        provider,
    )
}

pub fn decode_bounded_checkpoint<T: serde::de::DeserializeOwned>(
    checkpoint: &ctx_history_core::TypedKey,
    prefix: &str,
    maximum_bytes: usize,
    provider: &str,
) -> crate::Result<T> {
    ctx_history_jsonl::decode_bounded_checkpoint::<T, CaptureError>(
        checkpoint,
        prefix,
        maximum_bytes,
        provider,
    )
}

pub fn probe_first_record<T, E>(
    source_path: &std::path::Path,
    source_file: &Arc<ctx_history_jsonl::OpenedProviderSourceFile<CaptureError>>,
    visit: impl FnOnce(ctx_history_jsonl::JsonlRecordRef<'_>) -> std::result::Result<T, E>,
) -> std::result::Result<(T, ctx_history_jsonl::JsonlProbe), E>
where
    E: From<CaptureError>,
{
    ctx_history_jsonl::probe_first_record(source_path, source_file, visit)
}

pub fn probe_records_until<T, E>(
    source_path: &std::path::Path,
    source_file: &Arc<ctx_history_jsonl::OpenedProviderSourceFile<CaptureError>>,
    max_records: usize,
    mut visit: impl FnMut(ctx_history_jsonl::JsonlRecordRef<'_>) -> std::result::Result<Option<T>, E>,
) -> std::result::Result<Option<(T, ctx_history_jsonl::JsonlProbe)>, E>
where
    E: From<CaptureError>,
{
    ctx_history_jsonl::probe_records_until(source_path, source_file, max_records, |record| {
        visit(record)
    })
}

pub fn provider_jsonl_family_driver<B: ProviderRuntimeBinding>(
    adapter: Arc<ProviderJsonlAdapter<B>>,
    root: PathBuf,
) -> ProviderJsonlRouteDriver<B> {
    ctx_history_jsonl::jsonl_family_driver(adapter, root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeSet, error::Error, path::Path};

    use ctx_history_capture_model::SourceRouteIdentity;
    use ctx_history_capture_runtime::{
        BaseEventLookup, CaptureCommitOutcome, CaptureCommitReceipt, CaptureLifecycleOpenOutcome,
        CaptureLifecycleSink, CapturePublicationContext, CapturePublicationDisposition,
        CaptureRevalidationTarget, CaptureRouteRef, CaptureSourceAggregateRef, CoreMaterialization,
        CorePreparationFailureKind, CorePreparationPort, DocumentRecordSpool,
        ImmutableCaptureSnapshot, PresentCaptureRoute, SourceBackedRouteResources,
        SourceBackedRouteResult,
    };
    use ctx_history_core::{
        CaptureProvider, CertifiedSource, CertifiedSourceAppend, CertifiedSourceDeletion,
        CertifiedSourceInventory, CoreRecord, SourceKey,
    };
    use uuid::Uuid;

    #[derive(Debug, Clone, Copy)]
    struct FakeLookup;

    impl BaseEventLookup for FakeLookup {
        type Error = CaptureError;

        fn contains(&self, _event_id: Uuid) -> Result<bool, Self::Error> {
            Ok(false)
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct FakePreparation;

    impl CorePreparationPort for FakePreparation {
        type Prepared = CoreRecord;
        type Draft = CoreRecord;
        type Failure = CaptureError;

        fn prepare(&self, record: CoreRecord) -> Result<Self::Prepared, Self::Failure> {
            Ok(record)
        }

        fn prepare_draft(&self, record: CoreRecord) -> Result<Self::Draft, Self::Failure> {
            Ok(record)
        }

        fn materialize_draft(
            &self,
            draft: Self::Draft,
            _maximum_encoded_bytes: usize,
        ) -> Result<CoreMaterialization<Self::Prepared, Self::Draft>, Self::Failure> {
            Ok(CoreMaterialization::Prepared(draft))
        }

        fn prepared_source<'a>(&self, prepared: &'a Self::Prepared) -> &'a SourceKey {
            &prepared.source
        }

        fn encoded_bytes(&self, _prepared: &Self::Prepared) -> usize {
            0
        }

        fn failure_kind(&self, _failure: &Self::Failure) -> CorePreparationFailureKind {
            CorePreparationFailureKind::Internal
        }
    }

    #[derive(Debug, Clone, Default)]
    struct FakeSnapshot;

    impl ImmutableCaptureSnapshot for FakeSnapshot {
        fn sources(&self) -> &[CertifiedSource] {
            &[]
        }

        fn source_aggregates(
            &self,
        ) -> impl ExactSizeIterator<Item = CaptureSourceAggregateRef<'_>> {
            std::iter::empty()
        }

        fn source_routes(&self) -> impl ExactSizeIterator<Item = CaptureRouteRef<'_>> {
            std::iter::empty()
        }

        fn source_route(
            &self,
            _route_identity: &SourceRouteIdentity,
        ) -> Option<CaptureRouteRef<'_>> {
            None
        }
    }

    #[derive(Debug, Default)]
    struct FakeLifecycle;

    impl CaptureLifecycleSink for FakeLifecycle {
        type Error = CaptureError;
        type OpenOptions = ();
        type BaseLookup = FakeLookup;
        type Preparation = FakePreparation;
        type PinnedAppendBase = CertifiedSource;
        type CommittedSnapshot = FakeSnapshot;
        type VerifiedPublication = ();
        type Snapshot<'a> = FakeSnapshot;

        fn invariant_error(detail: &'static str) -> Self::Error {
            CaptureError::SystemInvariant(detail)
        }

        fn open(
            _root: &Path,
            _options: Self::OpenOptions,
        ) -> Result<CaptureLifecycleOpenOutcome<Self>, Self::Error> {
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
            FakeLookup
        }

        fn core_preparation(&self) -> Self::Preparation {
            FakePreparation
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
            _visit: impl for<'a> FnMut(CaptureRevalidationTarget<'a>) -> Result<(), E>,
        ) -> Result<Result<(), E>, Self::Error> {
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

        fn begin_source_replace(&mut self, _source: SourceKey) -> Result<(), Self::Error> {
            Ok(())
        }

        fn begin_source_append(
            &mut self,
            _source: SourceKey,
        ) -> Result<&CertifiedSource, Self::Error> {
            unimplemented!()
        }

        fn begin_source_append_from_base(
            &mut self,
            _base: Self::PinnedAppendBase,
        ) -> Result<&CertifiedSource, Self::Error> {
            unimplemented!()
        }

        fn add_prepared(
            &mut self,
            _prepared: <Self::Preparation as CorePreparationPort>::Prepared,
        ) -> Result<(), Self::Error> {
            Ok(())
        }

        fn certify_source(&mut self, _certificate: CertifiedSource) -> Result<(), Self::Error> {
            Ok(())
        }

        fn certify_source_append(
            &mut self,
            _append: CertifiedSourceAppend,
        ) -> Result<(), Self::Error> {
            Ok(())
        }

        fn retain_source(&mut self, _certificate: CertifiedSource) -> Result<(), Self::Error> {
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
            Ok(CaptureCommitReceipt::new(
                "fake-generation".to_owned(),
                0,
                0,
                0,
                0,
                FakeSnapshot,
            ))
        }

        fn commit_with_metadata<F, I, M>(
            self,
            _revalidate: F,
            _revalidate_inventory: I,
            _metadata_factory: M,
        ) -> Result<
            CaptureCommitOutcome<Self::CommittedSnapshot, Self::VerifiedPublication>,
            Self::Error,
        >
        where
            F: FnMut(CaptureRevalidationTarget<'_>) -> bool,
            I: FnMut(&CertifiedSourceInventory) -> bool,
            M: for<'a> FnOnce(
                CapturePublicationContext<'a, Self::Snapshot<'a>>,
            ) -> Result<Vec<u8>, Self::Error>,
        {
            Ok(CaptureCommitOutcome::new(
                CaptureCommitReceipt::new("fake-generation".to_owned(), 0, 0, 0, 0, FakeSnapshot),
                CapturePublicationDisposition::Published,
                ctx_history_capture_runtime::VerifiedCapture::new(()),
            ))
        }
    }

    #[derive(Default)]
    struct FakeSpool(Vec<CoreRecord>);

    impl DocumentRecordSpool for FakeSpool {
        fn new(_resources: SourceBackedRouteResources) -> SourceBackedRouteResult<Self> {
            Ok(Self::default())
        }

        fn push(&mut self, record: CoreRecord) -> SourceBackedRouteResult<()> {
            self.0.push(record);
            Ok(())
        }

        fn replay(
            self,
            emit: impl FnMut(CoreRecord) -> SourceBackedRouteResult<()>,
        ) -> SourceBackedRouteResult<()> {
            self.0.into_iter().try_for_each(emit)
        }
    }

    struct FakeBinding;

    impl crate::ProviderRuntimeBinding for FakeBinding {
        type CaptureLifecycleSink = FakeLifecycle;
        type DocumentRecordSpool = FakeSpool;
    }

    #[derive(Default)]
    struct FakeAdapter;

    impl JsonlFamilyAdapter for FakeAdapter {
        type Runtime = ProviderJsonlRuntime<FakeBinding>;

        fn provider(&self) -> CaptureProvider {
            CaptureProvider::Codex
        }

        fn source_format(&self) -> &'static str {
            "provider-pack-fixture-jsonl"
        }

        fn schema_variant(&self) -> &'static str {
            "provider-pack-fixture-v1"
        }

        fn parser_revision(&self) -> &'static str {
            "provider-pack-fixture-parser-v1"
        }

        fn append_mode(&self) -> JsonlFamilyAppendMode {
            JsonlFamilyAppendMode::CertifiedSuffix
        }

        fn discover(&self, root: &Path) -> Result<ProviderJsonlInventory, CaptureError> {
            ProviderJsonlInventory::missing(self.provider(), root)
        }
    }

    #[test]
    fn provider_pack_shaped_generic_jsonl_driver_and_aliases_compile_lower_only() {
        let driver: ProviderJsonlRouteDriver<FakeBinding> =
            provider_jsonl_family_driver::<FakeBinding>(
                Arc::new(FakeAdapter),
                PathBuf::from("/tmp/provider-pack-shaped-jsonl"),
            );
        assert!(driver.uses_parallel_leaf_workers);
        assert!(driver.route_control_expectation.is_none());

        let _worker = ProviderJsonlWorkerContext::<FakeBinding>::default();
        let reader: Option<ProviderJsonlReader> = None;
        let stream: Option<ProviderJsonlPhysicalStream> = None;
        let inventory: Option<ProviderJsonlInventory> = None;
        let leaf: Option<ProviderJsonlLeaf> = None;
        let execution_io: Option<ProviderJsonlExecutionIo<FakeBinding>> = None;
        let checkpoint: Option<JsonlCheckpoint> = None;
        let probe: Option<JsonlProbe> = None;
        let fallback_state: Option<ProviderFallbackEventIdentityState<FakeBinding>> = None;
        let lookup_error: Option<ProviderBaseEventLookupError<FakeBinding>> = None;
        let adapter_alias: Option<Arc<ProviderJsonlAdapter<FakeBinding>>> = None;
        let _ = (
            reader,
            stream,
            inventory,
            leaf,
            execution_io,
            checkpoint,
            probe,
            fallback_state,
            lookup_error,
            adapter_alias,
        );
    }
}
