pub(crate) mod catalog;
pub(crate) mod events;
pub(crate) mod nativepath;

pub(crate) const CODEX_CAPTURE_REVISION: u32 = 8;
pub(crate) const CODEX_POLICY_REVISION: u32 = 4;

#[doc(hidden)]
pub use nativepath::{
    hydrate_codex_locator, ingest_codex_source_backed_v0, CodexHydratedRecordV0,
    CodexLocatorResolverV0, CodexSourceBackedCountersV0, CodexSourceBackedErrorV0,
    CodexSourceBackedIngestReceiptV0, CodexSourceBackedPhaseTimingsV0, CodexSourceBackedResultV0,
};
