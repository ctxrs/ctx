//! Finite-inventory SQLite providers for ctx agent history.
//!
//! The crate owns AstrBot, Crush, Lingma, Shelley, and their bounded
//! provider registration fragments. It depends only on provider-neutral
//! capture and source layers; the capture facade supplies the concrete index
//! lifecycle when composing routes.

#![allow(clippy::items_after_test_module)]
#![cfg_attr(any(test, feature = "test-support"), allow(dead_code, unused_imports))]

mod native_source;
pub mod provider;
pub mod registration;

pub use ctx_history_capture_model::{
    fnv1a64, DiscoveryReport, ProviderSource, ProviderSourceStatus,
};
pub use ctx_history_provider_runtime::{CaptureError, ProviderRouteControlExpectation, Result};
pub use ctx_history_source_discovery::DiscoveryContext;

pub(crate) fn discover_provider_sources_for_provider_with_context(
    context: &DiscoveryContext,
    provider: ctx_history_core::CaptureProvider,
) -> DiscoveryReport {
    // The pack only asks for its own provider cohort. The frozen lower catalog
    // contains its probes directly; unrelated provider fragments are not
    // linked into the pack.
    ctx_history_source_discovery::discover_provider_sources_for_provider_with_context(
        &SQLITE_INVENTORY_DISCOVERY_PROBES,
        context,
        provider,
    )
}

fn unused_cursor_probe(
    _path: &std::path::Path,
) -> ctx_history_source_discovery::CursorTranscriptProbeOutcome {
    ctx_history_source_discovery::CursorTranscriptProbeOutcome::NotFound
}

const SQLITE_INVENTORY_DISCOVERY_PROBES: ctx_history_source_discovery::StaticProviderProbeCatalog =
    ctx_history_source_discovery::StaticProviderProbeCatalog::new(
        ctx_history_source_discovery::CursorProbeFragment::new(unused_cursor_probe),
    );
pub use provider::providers::crush::native_path::source_backed::{
    crush_source_key, CrushProjectDatabaseV0, CrushProjectInventoryObservationV0,
    CrushProjectInventorySourceV0, CrushSourceBackedErrorV0, CrushSourceBackedResultV0,
};
pub use provider::providers::lingma::native_path::lingma_source_key;
pub const ASTRBOT_SQLITE_SOURCE_FORMAT: &str = "astrbot_data_v4_sqlite";
pub const CRUSH_SQLITE_SOURCE_FORMAT: &str = "crush_sqlite";
pub const LINGMA_SQLITE_SOURCE_FORMAT: &str = "lingma_sqlite";
pub const SHELLEY_SQLITE_SOURCE_FORMAT: &str = "shelley_sqlite";
pub const MAX_PROVIDER_SQLITE_VALUE_BYTES: usize =
    ctx_history_source_sqlite::MAX_PROVIDER_SQLITE_VALUE_BYTES;

pub mod lifecycle {
    pub use ctx_history_capture_runtime::{
        CaptureLifecycleSink, ChangedDocumentSink, CompleteDocumentTree, DocumentAppendBase,
        DocumentBaseRoute, DocumentLeafExecutionPolicy, DocumentLeafFingerprint,
        DocumentRecordSpool, DocumentSourceTerminal, ObservedDocumentLeaf, ReplacementDocumentTree,
        SourceBackedCoordinatorError, SourceBackedCoordinatorResult,
        SourceBackedCurrentSourceProgress, SourceBackedCurrentSourceProgressStage,
        SourceBackedReconciliationDemand, SourceBackedRecordRejectionClass,
        SourceBackedRecordRejectionDraft, SourceBackedRecordRejectionDrafts,
        SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
        SourceBackedRouteSelection, SourceBackedRouteWatchTargets, SourceBackedSelectorAuthority,
    };
}

pub(crate) mod common {
    pub(crate) mod io {
        ctx_history_source_io::define_mapped_source_io_compat!(crate::CaptureError);
    }
}

pub(crate) mod provider_sources {
    pub(crate) use ctx_history_source_sqlite::*;

    #[cfg(test)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum SqliteRetryDecision {
        DoNotRetry,
        DoNotRetryCorrupt,
        RetryBusyOrLocked,
        RetrySourceTransition,
        RouteFatalResource,
    }

    #[cfg(test)]
    pub(crate) fn sqlite_retry_decision(error: &SqliteSourceAccessError) -> SqliteRetryDecision {
        if error.is_systemic_resource_failure() {
            SqliteRetryDecision::RouteFatalResource
        } else if error.is_source_changed() {
            SqliteRetryDecision::RetrySourceTransition
        } else if error.is_provider_corruption() || error.is_ctx_owned_corruption() {
            SqliteRetryDecision::DoNotRetryCorrupt
        } else if error.is_busy_or_locked() {
            SqliteRetryDecision::RetryBusyOrLocked
        } else {
            SqliteRetryDecision::DoNotRetry
        }
    }
}

#[cfg(test)]
mod test_support_paths {
    pub(crate) fn tempdir() -> std::io::Result<tempfile::TempDir> {
        tempfile::tempdir()
    }
}

#[cfg(test)]
pub(crate) fn test_provider_sqlite_data_root() -> &'static std::path::Path {
    use std::sync::OnceLock;

    static ROOT: OnceLock<tempfile::TempDir> = OnceLock::new();
    ROOT.get_or_init(|| tempfile::tempdir().expect("provider SQLite test root"))
        .path()
}
