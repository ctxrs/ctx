//! Devin's provider-owned ingestion leaf.
//!
//! Devin persists one SQLite database holding every session, and each session
//! stores its messages as a forest rather than a list. The imported transcript
//! is the walk from the session's recorded main chain up its parent pointers,
//! with pre-compaction history spliced in wherever a chain node names the node
//! it summarized. Everything else in the forest — summarizer threads,
//! abandoned regenerated turns, and the pre-compaction copies Devin rewrote —
//! is counted but not imported.

mod chain;
mod database;
mod normalization;
mod registration;
mod schema;
mod source_backed;
mod stream;
mod tool_state;

#[cfg(test)]
#[path = "chain_tests.rs"]
mod chain_tests;
#[cfg(test)]
#[path = "normalization_tests.rs"]
mod normalization_tests;
#[cfg(test)]
#[path = "schema_tests.rs"]
mod schema_tests;
#[cfg(test)]
#[path = "source_backed_tests.rs"]
mod source_backed_tests;

pub fn devin_source_backed_driver<B: crate::SelectedSqliteCaptureBinding>(
    source_path: &std::path::Path,
    data_root: &std::path::Path,
) -> ctx_history_capture_runtime::SourceBackedRouteDriver<B::Lifecycle, B::RouteControl> {
    devin_source_backed_driver_scoped::<B>(
        source_path,
        data_root,
        ctx_history_core::SourceAnchorScope::Unqualified,
    )
}

pub fn devin_source_backed_driver_scoped<B: crate::SelectedSqliteCaptureBinding>(
    source_path: &std::path::Path,
    data_root: &std::path::Path,
    source_scope: ctx_history_core::SourceAnchorScope,
) -> ctx_history_capture_runtime::SourceBackedRouteDriver<B::Lifecycle, B::RouteControl> {
    registration::source_backed_driver_scoped::<B>(
        ctx_history_core::CaptureProvider::Devin.as_str(),
        crate::DEVIN_CLI_SESSIONS_SQLITE_SOURCE_FORMAT,
        source_path,
        data_root,
        source_scope,
    )
}
