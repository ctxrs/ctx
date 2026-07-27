mod context;
mod discovery;
mod ordinary_file;
mod probes;
mod reasons;
mod resolvers;
mod selectors;
mod specs;
mod types;

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
#[cfg(test)]
pub(crate) use ordinary_file::forbid_ordinary_file_content_open;
pub use ordinary_file::{observe_ordinary_file, OrdinaryFileObservation};
pub(crate) use ordinary_file::{
    observe_ordinary_file_strong_metadata, open_ordinary_file_without_following,
};
pub use specs::{provider_source_spec, provider_source_specs};
pub use types::{
    DiscoveryIssue, DiscoveryIssueKind, DiscoveryReport, ProviderCatalogSupport,
    ProviderDefaultLocation, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceSpec, ProviderSourceStatus,
};

#[cfg(test)]
mod tests;
