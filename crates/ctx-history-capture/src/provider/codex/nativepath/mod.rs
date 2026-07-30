//! Provider-owned Codex source discovery, parsing, indexing, and hydration.

mod checkpoint;
mod prompt_history;
mod reader;
mod record;
mod rows;
mod source;
mod source_backed;

pub(crate) use checkpoint::CodexNativeCheckpoint;
pub(crate) use prompt_history::{
    observe_codex_prompt_history_source_backed_explicit_v0,
    revalidate_codex_prompt_history_source_backed_v0, scan_codex_prompt_history_source_backed_v0,
    CodexPromptHistorySourceBackedDispositionV0, CodexPromptHistorySourceBackedInputV0,
    CodexPromptHistorySourceBackedResolverV0,
};
pub(crate) use reader::{
    open_codex_source_capability, opened_codex_file_observation,
    revalidate_codex_source_observation, CodexNativeOwnedPage, CodexNativeScanner, CodexSourceScan,
};
#[cfg(test)]
pub(crate) use reader::{
    CodexNativeFrontier, CodexParseDisposition, MAX_CODEX_PAGE_BYTES, MAX_CODEX_PAGE_ROWS,
    MAX_CODEX_RECORD_BYTES,
};
pub(crate) use rows::CodexSessionRow;
pub(crate) use source::{
    discover_codex_catalog_sources, CodexAppendProof, CodexCheckpointGeneration,
    CodexFileObservation,
};
#[cfg(test)]
pub(crate) use source::{CodexCatalogSource, CodexSourceIdentity};
pub(crate) use source_backed::{
    discover_codex_root_inventory_v0, ingest_codex_sources_serial_v0, managed_codex_session_source,
    observe_codex_explicit_session_source_backed_v0,
    source_observation as codex_source_observation,
    writer_base_sources as codex_writer_base_sources, CodexExplicitSessionSourceBackedInputV0,
    CodexTerminalSourceEvidenceV0,
};
pub use source_backed::{
    hydrate_codex_locator, ingest_codex_source_backed_v0, CodexHydratedRecordV0,
    CodexLocatorResolverV0, CodexSourceBackedCountersV0, CodexSourceBackedErrorV0,
    CodexSourceBackedIngestReceiptV0, CodexSourceBackedPhaseTimingsV0, CodexSourceBackedResultV0,
};
#[cfg(test)]
mod tests;
