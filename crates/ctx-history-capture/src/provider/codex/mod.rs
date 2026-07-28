pub(crate) mod catalog;
pub(crate) mod events;
pub(crate) mod history;
pub(crate) mod nativepath;
pub(crate) mod session;

pub(crate) const CODEX_CAPTURE_REVISION: u32 = 8;
pub(crate) const CODEX_POLICY_REVISION: u32 = 4;

pub use catalog::{catalog_codex_session_files, catalog_codex_session_tree};
pub use history::import_codex_history_jsonl;
#[doc(hidden)]
pub use nativepath::{
    build_codex_cold_store, hydrate_codex_locator, ingest_codex_source_backed_v0,
    CodexColdPromptHistoryOptions, CodexColdStoreOptions, CodexColdStoreOutcome,
    CodexHydratedRecordV0, CodexSourceBackedCountersV0, CodexSourceBackedErrorV0,
    CodexSourceBackedIngestReceiptV0, CodexSourceBackedPhaseTimingsV0, CodexSourceBackedResultV0,
};
pub use session::{
    import_codex_session_jsonl, import_codex_session_jsonl_tail, import_codex_session_paths,
    import_codex_session_tree,
};
