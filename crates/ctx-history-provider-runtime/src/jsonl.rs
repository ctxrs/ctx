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
