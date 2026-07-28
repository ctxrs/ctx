//! Provider-owned Codex NativePath discovery and parsing.
//!
//! Store publication is intentionally separate and consumes these
//! provider-private observations, source-bound proofs, and bounded row pages.

mod checkpoint;
mod cold_store;
mod producer;
mod prompt_history;
#[cfg(codex_nativepath_qualification)]
mod qualification;
mod reader;
mod record;
mod root;
mod rows;
mod source;
mod source_backed;
mod vertical;

pub(crate) use checkpoint::CodexNativeCheckpoint;
pub use cold_store::{CodexColdPromptHistoryOptions, CodexColdStoreOptions, CodexColdStoreOutcome};
pub(crate) use producer::{
    run_codex_bounded_producers, CodexOrderedProducerItem, CodexProducerConfig,
};
pub(crate) use prompt_history::import_codex_native_prompt_history;
#[cfg(codex_nativepath_qualification)]
pub use qualification::{
    qualify_codex_native_session_root, CodexNativePathQualificationEvidence,
    QualificationInputIdentity, QualificationProducerCounters, QualificationStoreCounters,
};
pub(crate) use reader::{
    revalidate_codex_source_observation, CodexNativeFrontier, CodexNativeOwnedPage,
    CodexNativePage, CodexNativeProOutputPage, CodexNativeProfile, CodexNativeScanner,
    CodexSourceScan,
};
#[cfg(test)]
pub(crate) use reader::{
    CodexNativePageReceipt, CodexNativeProOutputPageReceipt, CodexParseDisposition,
    MAX_CODEX_PAGE_BYTES, MAX_CODEX_PAGE_ROWS, MAX_CODEX_RECORD_BYTES,
};
pub(crate) use root::{
    import_codex_native_session_files, import_codex_native_session_files_with_catalog,
    import_codex_native_session_root, import_codex_native_session_root_with_catalog,
};
#[cfg(test)]
pub(crate) use rows::CodexEventRow;
pub(crate) use rows::{CodexFileTouch, CodexSessionRow};
pub(crate) use source::{
    classify_source_lifecycle, discover_codex_catalog_sources, CodexAppendProof,
    CodexCatalogSource, CodexCheckpointGeneration, CodexFileObservation, CodexKnownSource,
    CodexSourceIdentity, CodexSourceLifecycle,
};
pub use source_backed::{
    hydrate_codex_locator, ingest_codex_source_backed_v0, CodexHydratedRecordV0,
    CodexSourceBackedCountersV0, CodexSourceBackedErrorV0, CodexSourceBackedIngestReceiptV0,
    CodexSourceBackedPhaseTimingsV0, CodexSourceBackedResultV0,
};
pub(crate) use vertical::{
    finish_pending_codex_native_retirement, prepare_codex_native_output_replay,
    prepare_codex_native_producer_task, retire_codex_native_source_route,
    retire_replaced_codex_native_source_route, CodexNativeOutputReplay, CodexNativeProducerStep,
    CodexNativeRootGroup, CodexNativeStoreOptions,
};
#[cfg(test)]
mod cold_store_tests;
#[cfg(test)]
mod tests;

pub fn build_codex_cold_store(
    options: CodexColdStoreOptions,
) -> crate::Result<CodexColdStoreOutcome> {
    let target = options.target_store_path.clone();
    map_cold_store_install_result(&target, cold_store::build_codex_cold_store(options))
}

fn map_cold_store_install_result(
    target: &std::path::Path,
    result: crate::Result<CodexColdStoreOutcome>,
) -> crate::Result<CodexColdStoreOutcome> {
    match result {
        Err(error)
            if cold_install_primitive_unavailable(&error)
                && std::fs::symlink_metadata(target)
                    .is_err_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(CodexColdStoreOutcome::OrdinaryStoreRequired)
        }
        result => result,
    }
}

fn cold_install_primitive_unavailable(error: &crate::CaptureError) -> bool {
    let crate::CaptureError::Store(ctx_history_store::StoreError::Io(error)) = error else {
        return false;
    };
    error.kind() == std::io::ErrorKind::Unsupported
}
