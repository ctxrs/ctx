use super::*;

/// Registers a Warp database under its stable installed-surface key.
pub fn register_warp_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    surface_key: impl Into<String>,
) -> SourceBackedCoordinatorResult<()> {
    let selected = WarpSourceSelectionV0::new(source.path.clone(), surface_key)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let capture_selection = selected.clone();
    let hydration_selection = selected;
    let driver = captured_route_driver(
        &source,
        move |sink| {
            let snapshot =
                project_warp_source_backed_v0(capture_selection.clone()).map_err(route_error)?;
            sink.begin(snapshot.source)?;
            for document in snapshot.documents {
                sink.document(document)?;
            }
            sink.certify(snapshot.certified_source)
        },
        provider_format_scope(CaptureProvider::Warp, "warp_sqlite"),
        move |request| {
            let hydrated = resolve_warp_locator_v0(&hydration_selection, request.locator())
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                })?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: hydrated.provider_bytes,
            })
        },
    );
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
    platform_root: impl Into<std::path::PathBuf>,
    retained_routes: Vec<(std::path::PathBuf, std::path::PathBuf)>,
) -> SourceBackedCoordinatorResult<()> {
    let mut selected =
        GooseSourceBackedSelectionV0::exact(source.path.clone(), platform_root.into());
    if !retained_routes.is_empty() {
        selected = selected
            .with_explicit_retained_routes(
                retained_routes
                    .into_iter()
                    .map(|(database, root)| GooseSourceRouteV0::exact(database, root))
                    .collect(),
            )
            .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    }
    let capture_selection = selected.clone();
    let hydration_selection = selected;
    let driver = captured_route_driver(
        &source,
        move |sink| {
            let adapter =
                GooseSourceBackedAdapterV0::open(capture_selection.clone()).map_err(route_error)?;
            sink.begin(adapter.source().clone())?;
            let mut scan = adapter.scan().map_err(route_error)?;
            while let Some(page) = scan.next_page().map_err(route_error)? {
                for document in page.into_documents() {
                    sink.document(document)?;
                }
            }
            sink.certify(scan.finish().map_err(route_error)?.certificate().clone())
        },
        provider_format_scope(CaptureProvider::Goose, "goose_sessions_sqlite"),
        move |request| {
            GooseSourceBackedResolverV0::new(hydration_selection.clone())
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                })?
                .hydrate_event(request)
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::SelectedWithRetainedExplicit,
        driver,
    )?);
    Ok(())
}
