//! Provider-owned Codex source discovery, parsing, and direct Core indexing.

mod checkpoint;
mod prompt_history;
mod reader;
mod record;
mod rows;
mod source;
mod source_backed;

pub(crate) use checkpoint::{CodexNativeCheckpoint, MAX_CODEX_TOOL_CONTEXTS};
pub(crate) use prompt_history::{
    CodexPromptHistoryJsonlFamilyAdapterV0, CodexPromptHistorySourceBackedInputV0,
};
#[cfg(test)]
pub(crate) use reader::revalidate_codex_source_observation;
#[cfg(test)]
pub(crate) use reader::{
    open_codex_source_capability, CodexNativeFrontier, CodexParseDisposition, MAX_CODEX_PAGE_BYTES,
    MAX_CODEX_PAGE_ROWS, MAX_CODEX_RECORD_BYTES, MAX_CODEX_SOURCE_BACKED_SINGLE_ROW_PAGE_BYTES,
};
pub(crate) use reader::{
    opened_codex_file_observation, CodexNativeOwnedPage, CodexNativeScanner, CodexSourceScan,
};
pub(crate) use rows::CodexSessionRow;
pub(crate) use source::{
    discover_codex_catalog_sources, CodexAppendProof, CodexCheckpointGeneration,
    CodexFileObservation,
};
#[cfg(test)]
pub(crate) use source::{CodexCatalogSource, CodexSourceIdentity};
pub(crate) use source_backed::{
    codex_session_root_rank, CodexExplicitSessionJsonlFamilyAdapterV0,
    CodexExplicitSessionSourceBackedInputV0, CodexGenerationNormalizationCoordinatorV0,
    CodexSessionTreeJsonlFamilyAdapterV0,
};
#[cfg(test)]
pub(crate) use source_backed::{
    install_after_codex_lineage_normalization_hook_v0, install_after_codex_metadata_inventory_hook,
    CodexSourceBackedCountersV0,
};
#[cfg(test)]
mod tests;
