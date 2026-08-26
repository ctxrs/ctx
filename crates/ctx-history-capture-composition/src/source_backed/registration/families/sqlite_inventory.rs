use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use ctx_history_core::TypedKey;
use ctx_history_providers_sqlite_inventory::registration::{
    astrbot_registration_scoped, astrbot_released_registration_scoped, crush_registration_scoped,
    lingma_registration_scoped, shelley_registration, SqliteInventoryCoverage,
};

use super::*;
use crate::provider::source_backed::family::document::{
    install_sqlite_inventory_registration, CaptureDocumentLifecycle, CaptureDocumentSpool,
};

pub type SqliteInventoryRouteAuthority = (Option<[u8; 32]>, SqliteInventoryCoverage);

pub fn register_astrbot_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    discovery: DiscoveryContext,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    install_sqlite_inventory_registration(
        registry,
        astrbot_registration_scoped::<CaptureDocumentLifecycle, CaptureDocumentSpool>(
            source,
            selection,
            data_root,
            discovery,
            source_root_lineage.map_or(
                ctx_history_core::SourceAnchorScope::Unqualified,
                ctx_history_core::SourceAnchorScope::Lineage,
            ),
        ),
    )
}

pub fn register_astrbot_released_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    identity_source: ProviderSource,
    identity_home: &Path,
    data_root: &Path,
) -> SourceBackedCoordinatorResult<()> {
    let provider = source.provider;
    let registration =
        astrbot_released_registration_scoped::<CaptureDocumentLifecycle, CaptureDocumentSpool>(
            source,
            identity_source,
            identity_home,
            data_root,
            ctx_history_core::SourceAnchorScope::Unqualified,
        )
        .map_err(|error| invalid_route(provider, error.to_string()))?;
    install_sqlite_inventory_registration(registry, registration)
}

pub fn register_crush_source_backed_route<I>(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    inventory: Arc<I>,
    source_root_lineage: Option<[u8; 32]>,
    coverage: SqliteInventoryCoverage,
) -> SourceBackedCoordinatorResult<()>
where
    I: CrushProjectInventorySourceV0 + Send + Sync + 'static,
{
    install_sqlite_inventory_registration(
        registry,
        crush_registration_scoped::<I, CaptureDocumentLifecycle, CaptureDocumentSpool>(
            source,
            selection,
            data_root,
            inventory,
            source_root_lineage.map_or(
                ctx_history_core::SourceAnchorScope::Unqualified,
                ctx_history_core::SourceAnchorScope::Lineage,
            ),
            coverage,
        ),
    )
}

pub fn register_lingma_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    authority_key: TypedKey,
    databases: Vec<(PathBuf, TypedKey)>,
    route_authority: SqliteInventoryRouteAuthority,
) -> SourceBackedCoordinatorResult<()> {
    let (source_root_lineage, coverage) = route_authority;
    let provider = source.provider;
    let registration =
        lingma_registration_scoped::<CaptureDocumentLifecycle, CaptureDocumentSpool>(
            source,
            selection,
            data_root,
            authority_key,
            databases,
            source_root_lineage.map_or(
                ctx_history_core::SourceAnchorScope::Unqualified,
                ctx_history_core::SourceAnchorScope::Lineage,
            ),
            coverage,
        )
        .map_err(|error| invalid_route(provider, error.to_string()))?;
    install_sqlite_inventory_registration(registry, registration)
}

pub fn register_shelley_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    exact_cwd: impl Into<PathBuf>,
) -> SourceBackedCoordinatorResult<()> {
    let provider = source.provider;
    let registration = shelley_registration::<CaptureDocumentLifecycle, CaptureDocumentSpool>(
        source, selection, data_root, exact_cwd,
    )
    .map_err(|source| SourceBackedCoordinatorError::RouteRegistration { provider, source })?;
    install_sqlite_inventory_registration(registry, registration)
}
