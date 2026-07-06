#[allow(unused_imports)]
use super::*;

#[derive(Debug)]
pub(crate) struct IncrementalCatchUpSummary {
    pub(crate) catalog: CatalogSummary,
    pub(crate) import: ProviderImportSummary,
    pub(crate) pending_sessions: usize,
}
