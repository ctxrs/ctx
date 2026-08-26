pub mod provider_sources;
pub use ctx_history_source_io::{
    ProviderJsonlInventory, ProviderJsonlInventoryLimits, PROVIDER_JSONL_INVENTORY_MAX_DEPTH,
    PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES, PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS,
    PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES, PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
};

pub fn inventory_provider_jsonl_paths(
    root: &std::path::Path,
    limits: ProviderJsonlInventoryLimits,
) -> Result<ProviderJsonlInventory> {
    ctx_history_source_io::inventory_provider_jsonl_paths(root, limits).map_err(Into::into)
}

pub fn inventory_provider_regular_paths(
    root: &std::path::Path,
    limits: ProviderJsonlInventoryLimits,
) -> Result<ProviderJsonlInventory> {
    ctx_history_source_io::inventory_provider_regular_paths(root, limits).map_err(Into::into)
}

pub fn provider_regular_file_len(path: &std::path::Path) -> Result<u64> {
    ctx_history_source_io::provider_regular_file_len(path).map_err(Into::into)
}
pub use provider_sources::{
    configured_root_capabilities, configured_root_capability,
    discover_canonical_automatic_provider_sources_with_context,
    discover_lingma_inventory_with_authority, discover_provider_sources,
    discover_provider_sources_for_provider, discover_provider_sources_for_provider_report,
    discover_provider_sources_for_provider_with_context,
    discover_provider_sources_for_provider_with_projects, discover_provider_sources_report,
    discover_provider_sources_with_context, discover_provider_sources_with_context_and_work_budget,
    discover_provider_sources_with_projects, discover_warp_sources_with_authority,
    observe_ordinary_file, provider_paths_equivalent, provider_source_belongs_to_configured_root,
    provider_source_for_path, provider_source_for_path_with_data_root, provider_source_spec,
    provider_source_specs, provider_source_status_reason, released_provider_home,
    resolve_lingma_discovery_authority, resolve_warp_discovery_authority,
    validate_provider_source_roots_outside_data_root, ConfiguredRootCapability,
    ConfiguredRootCapabilityState, ConfiguredRootExpander, ConfiguredRootPathKind,
    DiscoveredLingmaDatabase, DiscoveredWarpSource, DiscoveryContext, DiscoveryIssue,
    DiscoveryIssueKind, DiscoveryPlatform, DiscoveryPlatformDirs, DiscoveryReport,
    LingmaDatabaseCatalogLineage, LingmaDiscoveredInventory, LingmaDiscoveryUnavailable,
    LingmaInventorySelector, LingmaVscodeClient, LingmaVscodeProfile, OrdinaryFileObservation,
    ProviderCatalogSupport, ProviderDefaultLocation, ProviderImportSupport, ProviderSource,
    ProviderSourceKind, ProviderSourceRootBoundaryError, ProviderSourceRouteProvenance,
    ProviderSourceSpec, ProviderSourceStatus, ProviderSourceStatusReason, WarpDiscoveryUnavailable,
    WarpInstalledPlatform, WarpInstalledSurfaceKey, WarpReleaseChannel, WarpTerminalSurface,
    DISCOVERY_ENV_ALLOWLIST,
};

pub use ctx_history_capture_model::{
    provider_root_encoded_path_len, provider_root_path_within_limit, provider_source_config_digest,
    stable_capture_uuid, CatalogSummary, OutputObservationKind, OutputOutcome,
    OutputOutcomeMetadata, ProviderImportFailure, ProviderImportSummary, ProviderImportWorkResult,
    ProviderRootDefinition, ProviderRootKind, ProviderRootSet, ProviderRootSetError,
    ProviderRootSourceIdentity, ProviderRouteRole, ProviderRouteRoleError,
    ProviderSourceFailureKind, MAX_CONFIGURED_PROVIDER_ROOTS, MAX_PROVIDER_ROOT_ENCODED_PATH_BYTES,
    MAX_PROVIDER_ROOT_SELECTOR_BYTES, MAX_PROVIDER_ROUTE_ROLE_BYTES,
};
mod error;
pub use error::{CaptureError, ProviderJsonlInventoryLimit, Result};

pub(crate) mod common {
    pub(crate) mod identity;
    pub(crate) mod io;
}
pub use common::identity::compute_payload_hash;

pub(crate) mod provider;

/// Reads only persisted route-control bytes; it never opens Hermes provider data.
pub fn hermes_route_control_exact_due(control: &[u8], now_ms: i64) -> Option<bool> {
    ctx_history_capture_composition::hermes_route_control_exact_due(control, now_ms)
}

/// Validates a Hermes control against its exact profile owner before reading
/// the persisted exact-reconciliation deadline.
pub fn hermes_route_control_exact_due_for_profile(
    control: &[u8],
    profile_source_descriptor: [u8; 32],
    now_ms: i64,
) -> Option<bool> {
    ctx_history_capture_composition::hermes_route_control_exact_due_for_profile(
        control,
        profile_source_descriptor,
        now_ms,
    )
}

/// Returns the stable physical database identity from one valid successful
/// Hermes control without opening provider data.
pub fn hermes_route_control_database_identity(control: &[u8]) -> Option<[u8; 32]> {
    ctx_history_capture_composition::hermes_route_control_database_identity(control)
}

pub use provider::adapter::{CaptureWorkLimit, ProviderAdapterContext, ProviderImportOptions};
pub use provider::source_backed::register_nanoclaw_source_backed_route_with_base_sources;
pub use provider::source_backed::{
    automatic_provider_root_coexistence_route_identity,
    automatic_provider_root_coexistence_source_lineage, automatic_source_backed_route_identity,
    build_automatic_source_backed_registry, build_automatic_source_backed_registry_from_report,
    build_automatic_source_backed_registry_from_report_with_retained_roots,
    explicit_source_catalog_lineage, legacy_automatic_source_backed_route_identity,
    prepare_automatic_route_splits, refresh_source_backed_generation,
    refresh_source_backed_generation_for_routes,
    refresh_source_backed_generation_with_detailed_progress,
    refresh_source_backed_generation_with_progress, register_astrbot_source_backed_route,
    register_codex_prompt_history_source_backed_route, register_crush_source_backed_route,
    register_cursor_source_backed_route, register_custom_history_source_backed_route,
    register_forgecode_explicit_source_backed_route, register_gemini_source_backed_route,
    register_goose_source_backed_route, register_hermes_explicit_source_backed_route,
    register_landed_source_backed_route, register_landed_source_backed_route_with_data_root,
    register_lingma_source_backed_route, register_nanoclaw_source_backed_route,
    register_shelley_source_backed_route, register_warp_source_backed_route,
    source_backed_refresh_work_budget, source_backed_refresh_writer_options,
    source_backed_route_constructor, source_backed_route_inventory,
    source_backed_source_failure_identity, BorrowedIndexManifestView, CaptureDocumentSpool,
    CaptureProviderRuntime, CommittedIndexManifestView, CrushProjectDatabaseV0,
    CrushProjectInventoryObservationV0, CrushProjectInventorySourceV0, IndexCaptureCommitReceipt,
    IndexCaptureVerifiedPin, IndexManifestView, IndexVerifiedCapture, RouteObservation,
    SourceBackedAutomaticRegistryBuild, SourceBackedAutomaticRegistryIssue,
    SourceBackedAutomaticUnavailableReason, SourceBackedCertifiedRemoval,
    SourceBackedCoordinatorError, SourceBackedCoordinatorResult, SourceBackedCurrentSourceProgress,
    SourceBackedCurrentSourceProgressStage, SourceBackedDetailedRefreshProgress,
    SourceBackedFailedRoute, SourceBackedFailedRouteOutcome, SourceBackedGenerationSink,
    SourceBackedLogicalSourceFailure, SourceBackedLogicalSourceFailures,
    SourceBackedProviderRegistry, SourceBackedProviderRouteMetadata,
    SourceBackedReconciliationDemand, SourceBackedRecordCompletion, SourceBackedRecordRejection,
    SourceBackedRecordRejectionClass, SourceBackedRecordRejections, SourceBackedRefreshExecutor,
    SourceBackedRefreshProgress, SourceBackedRefreshReceipt, SourceBackedRefreshScope,
    SourceBackedRevalidationTarget, SourceBackedRoute, SourceBackedRouteConstructor,
    SourceBackedRouteControlExpectation, SourceBackedRouteDriver, SourceBackedRouteError,
    SourceBackedRouteErrorKind, SourceBackedRouteMetadata, SourceBackedRouteResult,
    SourceBackedRouteSelection, SourceBackedSelectorAuthority, SourceBackedSourceFailureClass,
    SourceBackedSourceFailures, SourceBackedSuccessfulRouteOutcome, SourceBackedWatchCatalog,
    SourceBackedWatchTargetKind, SqliteInventoryCoverage, LANDED_SOURCE_BACKED_ROUTES,
    MAX_RECORDED_SOURCE_BACKED_FAILURES, MAX_SOURCE_BACKED_FAILURE_DETAIL_BYTES,
    MAX_SOURCE_BACKED_FAILURE_SELECTOR_BYTES, MAX_SOURCE_BACKED_ROUTE_CONTROL_BYTES,
};
