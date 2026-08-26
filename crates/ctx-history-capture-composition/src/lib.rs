//! Index-backed composition of provider-owned capture routes.

pub mod source_backed;

pub use ctx_history_provider_runtime::{CaptureError, ProviderAdapterContext, Result};
pub use ctx_history_source_discovery::{
    path_presence, provider_source_spec, provider_source_specs,
    validate_provider_source_roots_outside_data_root, CrushDiscoveredProjectInventory,
    CrushProjectInventorySelector, CrushProjectInventorySelectorError, DiscoveryContext,
    DiscoveryIssue, DiscoveryPlatform, DiscoveryPlatformDirs, DiscoveryReport,
    LingmaInventorySelector, PathPresence, ProviderCatalogSupport, ProviderImportSupport,
    ProviderSource, ProviderSourceKind, ProviderSourceRouteProvenance, ProviderSourceSpec,
    ProviderSourceStatus, StaticProviderProbeCatalog, OPENHANDS_CURRENT_CLI_SOURCE_FORMAT,
};

#[cfg(test)]
pub(crate) use ctx_history_provider_gemini::GEMINI_CLI_SOURCE_FORMAT;

#[cfg(test)]
pub(crate) fn provider_source_for_path(
    provider: ctx_history_core::CaptureProvider,
    path: std::path::PathBuf,
) -> ProviderSource {
    ctx_history_source_discovery::provider_source_for_path(&test_provider_probes(), provider, path)
}

#[cfg(test)]
pub(crate) fn test_provider_probes() -> StaticProviderProbeCatalog {
    use ctx_history_source_discovery::{CursorProbeFragment, CursorTranscriptProbeOutcome};
    fn cursor(_: &std::path::Path) -> CursorTranscriptProbeOutcome {
        CursorTranscriptProbeOutcome::NotFound
    }
    StaticProviderProbeCatalog::new(CursorProbeFragment::new(cursor))
}

pub use source_backed::*;

pub fn hermes_route_control_exact_due(control: &[u8], now_ms: i64) -> Option<bool> {
    ctx_history_provider_hermes::hermes_route_control_exact_due(control, now_ms)
}

pub fn hermes_route_control_exact_due_for_profile(
    control: &[u8],
    profile_source_descriptor: [u8; 32],
    now_ms: i64,
) -> Option<bool> {
    ctx_history_provider_hermes::hermes_route_control_exact_due_for_profile(
        control,
        profile_source_descriptor,
        now_ms,
    )
}

pub fn hermes_route_control_database_identity(control: &[u8]) -> Option<[u8; 32]> {
    ctx_history_provider_hermes::hermes_route_control_database_identity(control)
}

#[cfg(test)]
pub(crate) mod common {
    pub(crate) mod io {
        pub(crate) use ctx_history_provider_runtime::source_io::{
            open_provider_source_file, OpenedProviderSourceFile, ProviderSourceRoot,
        };
    }
}
pub(crate) mod provider {
    pub(crate) use ctx_history_provider_codex::codex;
    pub(crate) mod providers {
        pub(crate) use crate::providers::auggie;
    }
    pub(crate) mod source_backed {
        pub(crate) use crate::source_backed::*;
    }
}

pub(crate) mod providers;

#[cfg(test)]
pub(crate) mod test_support_paths;

#[cfg(test)]
pub(crate) fn test_provider_sqlite_data_root() -> &'static std::path::Path {
    use std::sync::OnceLock;
    static ROOT: OnceLock<tempfile::TempDir> = OnceLock::new();
    ROOT.get_or_init(|| test_support_paths::tempdir().expect("provider SQLite test root"))
        .path()
}
