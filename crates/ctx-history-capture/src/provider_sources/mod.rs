mod append_import;
mod discovery;
mod import_units;
mod incremental_jsonl;
mod mutation_contracts;
mod probes;
mod reasons;
mod revisions;
mod specs;
mod types;

pub use discovery::{
    discover_provider_sources, discover_provider_sources_for_provider, provider_source_for_path,
};
pub use incremental_jsonl::{
    open_provider_jsonl, ClaudeProjectsJsonlResumeState, CodexSessionJsonlResumeState,
    CodexToolCallResumeContext, ProviderFileStableIdentity, ProviderJsonlAppendCheckpoint,
    ProviderJsonlOpenDecision, ProviderJsonlOpenMode, ProviderJsonlReader, ProviderJsonlRecordRead,
    ProviderJsonlReplacementReason, ProviderJsonlResumeState, TabnineJsonlResumeState,
};
pub(crate) use incremental_jsonl::{
    CODEX_RESUME_MAX_ENCODED_BYTES, CODEX_RESUME_MAX_PENDING_TOOL_CALLS,
};
pub use mutation_contracts::provider_file_mutation_contract;
pub use revisions::{
    provider_import_revision, ProviderImportRevision, DEFAULT_PROVIDER_IMPORT_REVISION,
    PROVIDER_IMPORT_REVISIONS,
};
pub use specs::{provider_source_spec, provider_source_specs};
pub use types::{
    ProviderCatalogSupport, ProviderDefaultLocation, ProviderFileMutationContract,
    ProviderImportDependency, ProviderImportSupport, ProviderImportUnitGrouping,
    ProviderImportUnitOwner, ProviderImportUnitSpec, ProviderSource, ProviderSourceKind,
    ProviderSourceSpec, ProviderSourceStatus,
};

#[cfg(test)]
mod tests;
pub use append_import::{
    import_append_capable_provider_file, provider_canonical_material_source_format,
    ProviderAdmittedJsonlAppendCheckpoint, ProviderAppendFileImportDecision,
    ProviderAppendFileImportMode, ProviderAppendFileImportOptions, ProviderAppendFileImportResult,
    ProviderAppendFileImportWithoutCheckpoint,
};
