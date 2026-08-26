use super::*;
use crate::provider::source_backed::family::document::register_replacement_document_tree_route_with_authority;
#[cfg(test)]
use ctx_history_providers_sqlite_logical::logical_sqlite_route_plan;
use ctx_history_providers_sqlite_logical::{
    explicit_forgecode_route_plan, logical_sqlite_route_plan_scoped, LogicalSqliteRoutePlan,
    LogicalSqliteRuntimeBinding,
};

pub(crate) struct CaptureLogicalSqliteBinding;

impl LogicalSqliteRuntimeBinding for CaptureLogicalSqliteBinding {
    type Lifecycle = crate::provider::source_backed::family::document::CaptureDocumentLifecycle;
    type Spool = crate::provider::source_backed::family::document::CaptureDocumentSpool;
    type RouteControl =
        crate::provider::source_backed::family::document::CaptureDocumentRouteControl;
}

fn register_logical_plan(
    registry: &mut SourceBackedProviderRegistry,
    selection: SourceBackedRouteSelection,
    plan: LogicalSqliteRoutePlan<CaptureLogicalSqliteBinding>,
) -> SourceBackedCoordinatorResult<()> {
    let authority = plan.selector_authority();
    macro_rules! register {
        ($source:expr, $adapter:expr) => {
            register_replacement_document_tree_route_with_authority(
                registry, $source, selection, authority, $adapter,
            )
        };
    }
    match plan {
        LogicalSqliteRoutePlan::DeepAgents { source, adapter } => register!(source, adapter),
        LogicalSqliteRoutePlan::ForgeCode {
            source, adapter, ..
        } => register!(source, adapter),
        LogicalSqliteRoutePlan::OpenCodeFamily { source, adapter } => register!(source, adapter),
        LogicalSqliteRoutePlan::Zed { source, adapter } => register!(source, adapter),
    }
}

pub(super) fn register_deepagents_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let provider = source.provider;
    let plan = logical_sqlite_route_plan_scoped(
        source,
        selection,
        data_root,
        source_root_lineage.map_or(
            ctx_history_core::SourceAnchorScope::Unqualified,
            ctx_history_core::SourceAnchorScope::Lineage,
        ),
    )
    .map_err(|error| invalid_route(provider, error.to_string()))?;
    register_logical_plan(registry, selection, plan)
}
pub(super) fn register_opencode_family_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let provider = source.provider;
    let plan = logical_sqlite_route_plan_scoped(
        source,
        selection,
        data_root,
        source_root_lineage.map_or(
            ctx_history_core::SourceAnchorScope::Unqualified,
            ctx_history_core::SourceAnchorScope::Lineage,
        ),
    )
    .map_err(|error| invalid_route(provider, error.to_string()))?;
    register_logical_plan(registry, selection, plan)
}

pub(super) fn register_zed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let provider = source.provider;
    let plan = logical_sqlite_route_plan_scoped(
        source,
        selection,
        data_root,
        source_root_lineage.map_or(
            ctx_history_core::SourceAnchorScope::Unqualified,
            ctx_history_core::SourceAnchorScope::Lineage,
        ),
    )
    .map_err(|error| invalid_route(provider, error.to_string()))?;
    register_logical_plan(registry, selection, plan)
}
pub(super) fn register_forgecode_selected_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let provider = source.provider;
    let plan = logical_sqlite_route_plan_scoped(
        source,
        selection,
        data_root,
        source_root_lineage.map_or(
            ctx_history_core::SourceAnchorScope::Unqualified,
            ctx_history_core::SourceAnchorScope::Lineage,
        ),
    )
    .map_err(|error| invalid_route(provider, error.to_string()))?;
    register_logical_plan(registry, selection, plan)
}

pub fn register_forgecode_explicit_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    data_root: &Path,
    catalog_lineage: [u8; 32],
) -> SourceBackedCoordinatorResult<()> {
    let provider = source.provider;
    let plan = explicit_forgecode_route_plan(source, data_root, catalog_lineage)
        .map_err(|error| invalid_route(provider, error.to_string()))?;
    register_logical_plan(registry, SourceBackedRouteSelection::ExplicitManual, plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_binding_consumes_exact_pack_registration_authorities() {
        let data_root = crate::test_provider_sqlite_data_root();
        for provider in [
            CaptureProvider::DeepAgents,
            CaptureProvider::OpenCode,
            CaptureProvider::Kilo,
            CaptureProvider::MiMoCode,
            CaptureProvider::Zed,
        ] {
            let plan = logical_sqlite_route_plan::<CaptureLogicalSqliteBinding>(
                source(provider),
                SourceBackedRouteSelection::Automatic,
                data_root,
            )
            .unwrap();
            assert_eq!(
                plan.selector_authority(),
                SourceBackedSelectorAuthority::DiscoveredWinner,
                "{provider:?}"
            );
        }

        let selected = logical_sqlite_route_plan::<CaptureLogicalSqliteBinding>(
            source(CaptureProvider::ForgeCode),
            SourceBackedRouteSelection::Automatic,
            data_root,
        )
        .unwrap();
        assert_eq!(
            selected.selector_authority(),
            SourceBackedSelectorAuthority::SelectedWithRetainedExplicit
        );
        let explicit = explicit_forgecode_route_plan::<CaptureLogicalSqliteBinding>(
            source(CaptureProvider::ForgeCode),
            data_root,
            [7; 32],
        )
        .unwrap();
        assert_eq!(
            explicit.selector_authority(),
            SourceBackedSelectorAuthority::ExplicitPath
        );

        assert!(logical_sqlite_route_plan::<CaptureLogicalSqliteBinding>(
            source(CaptureProvider::Hermes),
            SourceBackedRouteSelection::Automatic,
            data_root,
        )
        .is_err());
        assert!(logical_sqlite_route_plan::<CaptureLogicalSqliteBinding>(
            source(CaptureProvider::ForgeCode),
            SourceBackedRouteSelection::ExplicitManual,
            data_root,
        )
        .is_err());
    }

    fn source(provider: CaptureProvider) -> ProviderSource {
        ProviderSource {
            provider,
            path: PathBuf::from("provider.sqlite"),
            exists: true,
            source_format: "logical_sqlite_registration_test",
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: crate::ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
            route_provenance: Default::default(),
        }
    }
}
