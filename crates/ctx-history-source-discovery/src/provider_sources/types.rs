pub use ctx_history_capture_model::{
    DiscoveryIssue, DiscoveryIssueKind, DiscoveryReport, ProviderCatalogSupport,
    ProviderDefaultLocation, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceRouteProvenance, ProviderSourceSpec, ProviderSourceStatus,
    ProviderSourceStatusReason,
};

/// Applies provider discovery policy to a captured source observation.
pub fn provider_source_status_reason(
    _source: &ProviderSource,
) -> Option<ProviderSourceStatusReason> {
    None
}
