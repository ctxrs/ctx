mod context;
mod discovery;
mod event_files;
mod lingma;
mod ordinary_file;
mod probes;
mod reasons;
mod resolvers;
mod selectors;
mod specs;
mod sqlite_source;
mod types;
mod warp;

pub use context::{
    DiscoveryContext, DiscoveryPlatform, DiscoveryPlatformDirs, DISCOVERY_ENV_ALLOWLIST,
};
pub use discovery::{
    discover_provider_sources, discover_provider_sources_for_provider,
    discover_provider_sources_for_provider_report,
    discover_provider_sources_for_provider_with_context,
    discover_provider_sources_for_provider_with_projects, discover_provider_sources_report,
    discover_provider_sources_with_context, discover_provider_sources_with_context_and_work_budget,
    discover_provider_sources_with_projects, provider_source_for_path,
    validate_provider_source_roots_outside_data_root, ProviderSourceRootBoundaryError,
};
#[cfg(test)]
pub(crate) use event_files::count_event_file_io;
pub(crate) use event_files::{
    EventFileCoordinates, EventFileGroup, EventFileInventory, EventFileInventoryError,
    EventFileLimits,
};
pub use lingma::{
    discover_lingma_inventory_with_authority, resolve_lingma_discovery_authority,
    DiscoveredLingmaDatabase, LingmaDatabaseCatalogLineage, LingmaDiscoveredInventory,
    LingmaDiscoveryUnavailable, LingmaInventorySelector, LingmaVscodeClient, LingmaVscodeProfile,
};
#[cfg(test)]
pub(crate) use ordinary_file::forbid_ordinary_file_content_open;
pub(crate) use ordinary_file::open_ordinary_file_without_following;
pub use ordinary_file::{observe_ordinary_file, OrdinaryFileObservation};
pub(crate) use resolvers::PathPresence;
pub(crate) use resolvers::{
    path_presence, CrushDiscoveredProjectInventory, CrushProjectInventorySelector,
    CrushProjectInventorySelectorError,
};
pub use specs::{provider_source_spec, provider_source_specs};
#[cfg(test)]
pub(crate) use sqlite_source::{
    fail_next_opened_snapshot_cleanup_for_test,
    open_root_handle_sqlite_source_online_backup_after_private_source_copy_for_test,
    SqliteRetryDecision, SqliteSourceSnapshotCounters,
};
pub(crate) use sqlite_source::{
    open_root_handle_sqlite_source_snapshot, resource_exhaustion_io_error,
    retain_sqlite_source_directory_authority, rusqlite_busy_or_locked, rusqlite_resource_failure,
    SqliteArtifactKind, SqliteCleanupStatus, SqliteFailurePhase, SqliteLogicalSnapshot,
    SqliteSourceAccessError, SqliteSourceDirectoryAuthority, SqliteSourceEvidence,
    SqliteSourceProgressError, SqliteSourceReadSnapshot,
};
pub use types::{
    DiscoveryIssue, DiscoveryIssueKind, DiscoveryReport, ProviderCatalogSupport,
    ProviderDefaultLocation, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceSpec, ProviderSourceStatus, ProviderSourceStatusReason,
};
pub use warp::{
    discover_warp_sources_with_authority, resolve_warp_discovery_authority, DiscoveredWarpSource,
    WarpDiscoveryUnavailable, WarpInstalledPlatform, WarpInstalledSurfaceKey, WarpReleaseChannel,
    WarpTerminalSurface,
};

#[cfg(test)]
mod tests;
