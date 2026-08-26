#![allow(
    unused_imports,
    reason = "the compatibility surface preserves provider-local JSONL imports"
)]

use std::{path::PathBuf, sync::Arc};

use ctx_history_provider_gemini::GeminiError;
use ctx_history_provider_native_jsonl::{NativeJsonlError, NativeJsonlRuntime};

pub(crate) type JsonlFamilyRuntime =
    ctx_history_provider_runtime::ProviderJsonlRuntime<super::CaptureProviderRuntime>;

pub(crate) type FallbackEventIdentityState =
    ctx_history_provider_runtime::ProviderFallbackEventIdentityState<super::CaptureProviderRuntime>;
#[cfg(test)]
pub(crate) type JsonlReader = ctx_history_provider_runtime::ProviderJsonlReader;
#[cfg(test)]
pub(crate) type JsonlFamilyWorkerContext =
    ctx_history_provider_runtime::ProviderJsonlWorkerContext<super::CaptureProviderRuntime>;

pub(crate) use ctx_history_provider_runtime::FallbackEventIdentityMode;
pub(crate) use ctx_history_provider_runtime::{
    bounded_checkpoint_fits, decode_bounded_checkpoint, encode_bounded_checkpoint,
    jsonl_selected_content_fits, jsonl_single_file_inventory, new_prefix_hasher,
    observe_opened_file, observe_opened_file_allow_append, prefix_digest, probe_first_record,
    probe_records_until, provider_jsonl_family_driver as jsonl_family_driver, read_bounded_record,
    read_bounded_record_complete_and_prefix_sha256,
    read_bounded_record_full_complete_and_prefix_sha256, read_bounded_record_unhashed,
    JsonlAppendOccurrenceState, JsonlCheckpoint, JsonlFamilyAdapter, JsonlFamilyAppendMode,
    JsonlFamilyExecutionPosition, JsonlFamilyInventoryMode, JsonlFamilyProjectionMode,
    JsonlFamilyProjector, JsonlFamilyPublication, JsonlFamilyRejectedLeaf,
    JsonlFamilyRootMissingMode, JsonlFamilySemanticExecutor, JsonlFamilySemanticPage,
    JsonlFamilySemanticPreflight, JsonlFamilySemanticSummary, JsonlFileObservation,
    JsonlOversizedRecordPolicy, JsonlPage, JsonlPhysicalDigest, JsonlPhysicalRecord,
    JsonlPhysicalStreamPosition, JsonlProbe, JsonlRecordEvidence, JsonlRecordFraming,
    JsonlRecordRef, JsonlScanOutcome, JsonlSemanticPreflightMode, JsonlSourceChange,
    JsonlSourceIdentity,
};

/// Capture-only composition for the extracted native-JSONL provider package.
/// Native JSONL retains parser/source errors while capture owns lifecycle,
/// route control, and publication.
pub(crate) struct NativeJsonlCaptureRuntime;

impl ctx_history_jsonl::JsonlFamilyRuntime for NativeJsonlCaptureRuntime {
    type Error = NativeJsonlError;
    type Lifecycle = super::super::IndexCaptureLifecycle;
    type WorkerServices = ();
    type RouteControl = super::super::SourceBackedRouteControlExpectation;

    fn begin_worker_leaf(_services: &mut Self::WorkerServices) {}
}

impl NativeJsonlRuntime for NativeJsonlCaptureRuntime {
    fn tabnine_unavailable_source(
        path: &std::path::Path,
        error: NativeJsonlError,
    ) -> NativeJsonlError {
        NativeJsonlError::ProviderSource {
            provider: ctx_history_core::CaptureProvider::Tabnine.as_str(),
            path: path.to_path_buf(),
            kind: ctx_history_capture_model::ProviderSourceFailureKind::Io,
            detail: error.to_string(),
        }
    }
}
#[cfg(test)]
pub(crate) use ctx_history_provider_runtime::{
    checkpoint_admitted_revision_for_test, jsonl_prefix_hash_bytes, reset_jsonl_prefix_hash_bytes,
    revalidate_frozen_prefix, set_after_final_jsonl_prefix_hash_hook,
    set_after_jsonl_append_observation_route_binding_hook, set_after_jsonl_prefix_hash_hook,
    set_after_jsonl_semantic_preflight_hook, set_after_second_jsonl_prefix_hash_hook,
    set_after_standard_zstd_snapshot_hook, set_before_jsonl_terminal_physical_revalidation_hook,
};

/// Concrete coordinator binding for the extracted Gemini provider. The provider
/// owns source-error classification while capture remains the lifecycle,
/// registration, route, and publication authority.
pub(crate) struct GeminiCaptureJsonlRuntime;

impl ctx_history_jsonl::JsonlFamilyRuntime for GeminiCaptureJsonlRuntime {
    type Error = GeminiError;
    type Lifecycle = super::super::IndexCaptureLifecycle;
    type WorkerServices = ();
    type RouteControl = super::super::SourceBackedRouteControlExpectation;

    fn begin_worker_leaf(_services: &mut Self::WorkerServices) {}
}

pub(crate) fn gemini_jsonl_family_driver(
    adapter: Arc<dyn JsonlFamilyAdapter<Runtime = GeminiCaptureJsonlRuntime>>,
    root: PathBuf,
) -> super::super::SourceBackedRouteDriver {
    ctx_history_jsonl::jsonl_family_driver(adapter, root)
}
