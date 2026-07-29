mod context;
mod discovery;
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
    discover_provider_sources_with_context, discover_provider_sources_with_projects,
    provider_source_for_path,
};
pub use lingma::{
    discover_lingma_inventory_with_authority, resolve_lingma_discovery_authority,
    DiscoveredLingmaDatabase, LingmaDatabaseCatalogLineage, LingmaDiscoveredInventory,
    LingmaDiscoveryUnavailable, LingmaInventorySelector, LingmaVscodeClient, LingmaVscodeProfile,
};
#[cfg(test)]
pub(crate) use ordinary_file::forbid_ordinary_file_content_open;
pub use ordinary_file::{observe_ordinary_file, OrdinaryFileObservation};
pub(crate) use ordinary_file::{
    observe_ordinary_file_strong_metadata, open_ordinary_file_without_following,
};
pub(crate) use resolvers::{
    CrushDiscoveredProjectInventory, CrushProjectInventorySelector,
    CrushProjectInventorySelectorError,
};
pub use specs::{provider_source_spec, provider_source_specs};
pub(crate) use sqlite_source::{
    open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
    SqliteSourceAccessError, SqliteSourceDirectoryAuthority, SqliteSourceEvidence,
    SqliteSourceReadSnapshot,
};
pub use types::{
    DiscoveryIssue, DiscoveryIssueKind, DiscoveryReport, ProviderCatalogSupport,
    ProviderDefaultLocation, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceSpec, ProviderSourceStatus,
};
pub use warp::{
    discover_warp_sources_with_authority, resolve_warp_discovery_authority, DiscoveredWarpSource,
    WarpDiscoveryUnavailable, WarpInstalledPlatform, WarpInstalledSurfaceKey, WarpReleaseChannel,
    WarpTerminalSurface,
};

#[cfg(test)]
mod tests;
