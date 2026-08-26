use super::*;
use crate::provider::source_backed::{
    executable_route, family::document::CaptureSelectedSqliteBinding,
};

pub(super) fn register_firebender_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let driver = ctx_history_providers_sqlite_selected::firebender_source_backed_driver_scoped::<
        CaptureSelectedSqliteBinding,
    >(
        &source.path,
        data_root,
        source_root_lineage.map_or(
            ctx_history_core::SourceAnchorScope::Unqualified,
            ctx_history_core::SourceAnchorScope::Lineage,
        ),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

pub(super) fn register_kiro_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let driver = ctx_history_providers_sqlite_selected::kiro_source_backed_driver_scoped::<
        CaptureSelectedSqliteBinding,
    >(
        &source.path,
        data_root,
        source_root_lineage.map_or(
            ctx_history_core::SourceAnchorScope::Unqualified,
            ctx_history_core::SourceAnchorScope::Lineage,
        ),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

/// Registers a Warp database under its stable installed-surface key.
pub fn register_warp_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    surface_key: impl Into<String>,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let driver = ctx_history_providers_sqlite_selected::warp_source_backed_driver_scoped::<
        CaptureSelectedSqliteBinding,
    >(
        &source.path,
        data_root,
        surface_key,
        source_root_lineage.map_or(
            ctx_history_core::SourceAnchorScope::Unqualified,
            ctx_history_core::SourceAnchorScope::Lineage,
        ),
    )
    .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::NamedSurface,
        driver,
    )?);
    Ok(())
}

/// Registers Goose's selected database and the exact platform root needed to
/// resolve attachments. Historical routes are retained only when supplied.
pub fn register_goose_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    platform_root: impl Into<std::path::PathBuf>,
    retained_routes: Vec<(std::path::PathBuf, std::path::PathBuf)>,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let retained_routes = retained_routes
        .into_iter()
        .map(|(database, root)| {
            ctx_history_providers_sqlite_selected::GooseSourceRoute::exact(database, root)
        })
        .collect();
    let driver = ctx_history_providers_sqlite_selected::goose_source_backed_driver_scoped::<
        CaptureSelectedSqliteBinding,
    >(
        &source.path,
        data_root,
        platform_root.into(),
        retained_routes,
        source_root_lineage.map_or(
            ctx_history_core::SourceAnchorScope::Unqualified,
            ctx_history_core::SourceAnchorScope::Lineage,
        ),
    )
    .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::SelectedWithRetainedExplicit,
        driver,
    )?);
    Ok(())
}
