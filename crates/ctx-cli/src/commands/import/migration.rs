//! Import integration seam for the source-backed data transition.
//!
//! Call this after request discovery and before any path that can invoke
//! `Store::open`. A released schema-46 root must be handed to the rebuild
//! worker instead of entering the legacy provider writer.

use std::path::Path;

use anyhow::Result;
use ctx_history_capture::ProviderSourceStatus;

use crate::{
    provider_sources::SourceInfo,
    upgrade::data_migration::{self, AvailableProviderSource, MigrationDecision, MigrationMarker},
};

#[allow(dead_code)]
pub(super) fn prepare_before_store_open(
    data_root: &Path,
    sources: &[SourceInfo],
) -> Result<MigrationDecision> {
    data_migration::prepare(data_root, &available_sources(sources))
}

#[allow(dead_code)]
pub(super) fn inspect_before_store_open(data_root: &Path) -> Result<Option<MigrationMarker>> {
    data_migration::inspect(data_root)
}

fn available_sources(sources: &[SourceInfo]) -> Vec<AvailableProviderSource> {
    sources
        .iter()
        .filter(|source| source.exists && source.status == ProviderSourceStatus::Available)
        .map(|source| {
            AvailableProviderSource::new(
                source.provider.as_str(),
                source.source_format,
                source.path.clone(),
            )
        })
        .collect()
}
