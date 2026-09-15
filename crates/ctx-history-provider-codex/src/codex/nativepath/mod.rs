//! Provider-owned Codex source discovery, parsing, and direct Core indexing.

mod checkpoint;
mod patch_literals;
pub mod prompt_history;
mod raw_json;
mod reader;
mod record;
mod rows;
mod source;
pub mod source_backed;

pub use prompt_history::{
    CodexPromptHistoryJsonlFamilyAdapterV0, CodexPromptHistoryProjector,
    CodexPromptHistorySourceBackedInputV0,
};
pub(crate) use reader::{opened_codex_file_observation, CodexNativeScanner};
pub(crate) use rows::CodexSessionRow;
pub(crate) use source::{discover_codex_catalog_sources, CodexFileObservation};
pub use source_backed::{
    absolute_lexical_path, codex_session_root_rank, CodexExplicitSessionSourceBackedInputV0,
    CodexGenerationNormalizationCoordinatorV0, CodexSessionJsonlFamilyAdapterV0,
};
