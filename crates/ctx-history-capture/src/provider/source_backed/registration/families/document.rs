use super::*;

const DIRECT_ROUTES: &[RouteEntry] = &[
    RouteEntry::new(
        CaptureProvider::Auggie,
        crate::provider::providers::auggie::native_path::register_source_backed_route,
    ),
    RouteEntry::new(
        CaptureProvider::CodeBuddy,
        crate::provider::providers::codebuddy::native_path::register_source_backed_route,
    ),
];

pub(super) fn register_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    if let Some(register) = direct_route_registration(DIRECT_ROUTES, source.provider) {
        return register(registry, source, selection);
    }
    match source.provider {
        CaptureProvider::Cline | CaptureProvider::RooCode => {
            register_task_json_route(registry, source, selection)
        }
        CaptureProvider::RovoDev => register_rovodev_route(registry, source, selection),
        CaptureProvider::Continue => register_continue_route(registry, source, selection),
        provider => Err(invalid_route(
            provider,
            "this provider is not registered by the document route family",
        )),
    }
}

pub(super) fn register_task_json_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let selected = vec![source.clone()];
    let provider = source.provider;
    let resolver = match provider {
        CaptureProvider::Cline => cline_task_json_source_backed_resolver(&selected),
        CaptureProvider::RooCode => roo_task_json_source_backed_resolver(&selected),
        _ => unreachable!("caller restricts task JSON providers"),
    }
    .map_err(|error| invalid_route(provider, error.to_string()))?;
    let adapter = match provider {
        CaptureProvider::Cline => cline_task_json_source_backed_adapter(&selected),
        CaptureProvider::RooCode => roo_task_json_source_backed_adapter(&selected),
        _ => unreachable!("caller restricts task JSON providers"),
    }
    .with_resolver(resolver);
    crate::provider::source_backed::family::document::register_replacement_document_tree_route(
        registry, source, selection, adapter,
    )
}

/// Registers one explicit NanoClaw compound project with caller-owned catalog
/// lineage.
pub fn register_nanoclaw_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    catalog_lineage: [u8; 32],
) -> SourceBackedCoordinatorResult<()> {
    let adapter = NanoClawDocumentTreeAdapter::new(source.path.clone(), catalog_lineage)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    crate::provider::source_backed::family::document::register_replacement_document_tree_route_with_authority(
        registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
        SourceBackedSelectorAuthority::CatalogLineage,
        adapter,
    )
}

pub(super) fn register_rovodev_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let context = ProviderAdapterContext {
        machine_id: "source-backed-rovodev".to_owned(),
        source_path: Some(source.path.clone()),
        source_root: Some(source.path.clone()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let adapter = RovoDevDocumentTreeAdapter::new(source.path.clone(), context);
    crate::provider::source_backed::family::document::register_replacement_document_tree_route(
        registry, source, selection, adapter,
    )
}
pub(super) fn register_continue_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let outcome: ContinueSourceBackedOutcome =
        ContinueSourceBackedReader::register(registry, source, selection);
    outcome
}
