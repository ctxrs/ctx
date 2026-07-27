//! Provider-owned Codex NativePath discovery and parsing.
//!
//! Store publication is intentionally separate and consumes these
//! provider-private observations, source-bound proofs, and bounded row pages.

mod checkpoint;
mod prompt_history;
mod reader;
mod record;
mod root;
mod rows;
mod source;
mod vertical;

pub(crate) use checkpoint::CodexNativeCheckpoint;
pub(crate) use prompt_history::import_codex_native_prompt_history;
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
pub(crate) use root::{import_codex_native_session_files, import_codex_native_session_root};
#[cfg(test)]
pub(crate) use rows::CodexEventRow;
pub(crate) use rows::{CodexFileTouch, CodexSessionRow};
pub(crate) use source::{
    classify_source_lifecycle, discover_codex_catalog_sources, CodexAppendProof,
    CodexCatalogSource, CodexCheckpointGeneration, CodexFileObservation, CodexKnownSource,
    CodexSourceIdentity, CodexSourceLifecycle,
};
pub(crate) use vertical::{
    prepare_codex_native_output_replay, prepare_codex_native_source,
    retire_codex_native_source_route, retire_replaced_codex_native_source_route,
    CodexNativeOutputReplay, CodexNativePreparedSource, CodexNativeRootGroup,
    CodexNativeSourceAdmission, CodexNativeStoreOptions,
};
#[cfg(test)]
mod tests;
