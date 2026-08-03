use super::*;

mod families;

pub use families::*;

pub(crate) fn executable_route(
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    authority: SourceBackedSelectorAuthority,
    driver: SourceBackedRouteDriver,
) -> SourceBackedCoordinatorResult<SourceBackedRoute> {
    match selection {
        SourceBackedRouteSelection::Automatic => {
            SourceBackedRoute::automatic(source, authority, driver)
        }
        SourceBackedRouteSelection::ExplicitManual => SourceBackedRoute::explicit_manual(
            source,
            if authority == SourceBackedSelectorAuthority::DiscoveredWinner {
                SourceBackedSelectorAuthority::ExplicitPath
            } else {
                authority
            },
            driver,
        ),
    }
}

pub(in crate::provider::source_backed) fn validate_executable_route(
    source: &ProviderSource,
    selection: SourceBackedRouteSelection,
    selector_authority: SourceBackedSelectorAuthority,
) -> SourceBackedCoordinatorResult<&'static SourceBackedProviderRouteMetadata> {
    let known = landed_format_route(source.provider, source.source_format);
    let Some(known) = known else {
        return Err(invalid_route(
            source.provider,
            format!(
                "source format {:?} has no landed route",
                source.source_format
            ),
        ));
    };
    let selected_mode_supported = match selection {
        SourceBackedRouteSelection::Automatic => known.automatic,
        SourceBackedRouteSelection::ExplicitManual => known.explicit_manual,
    };
    if !selected_mode_supported
        || known.unsupported_reason.is_some()
        || !source.import_support.is_importable()
        || source.source_kind == ProviderSourceKind::DetectionOnly
        || source.status == ProviderSourceStatus::Unsupported
        || source.unsupported_reason.is_some()
    {
        return Err(invalid_route(
            source.provider,
            source
                .unsupported_reason
                .or(known.unsupported_reason)
                .unwrap_or("the selected automatic/manual mode is unsupported"),
        ));
    }
    if selection == SourceBackedRouteSelection::Automatic
        && source.import_support != ProviderImportSupport::Native
    {
        return Err(invalid_route(
            source.provider,
            "an explicit-only provider source cannot be registered automatically",
        ));
    }
    if selector_authority != known.selector_authority
        && !matches!(
            (selection, selector_authority),
            (
                SourceBackedRouteSelection::ExplicitManual,
                SourceBackedSelectorAuthority::ExplicitPath
            )
        )
    {
        return Err(invalid_route(
            source.provider,
            "the route omitted or changed its provider selector authority",
        ));
    }
    Ok(known)
}

pub(in crate::provider::source_backed) fn landed_format_route(
    provider: CaptureProvider,
    selected_source_format: &str,
) -> Option<&'static SourceBackedProviderRouteMetadata> {
    LANDED_SOURCE_BACKED_ROUTES
        .iter()
        .find(|route| route.provider == provider && route.source_format == selected_source_format)
}

pub(crate) fn invalid_route(
    provider: CaptureProvider,
    detail: impl Into<String>,
) -> SourceBackedCoordinatorError {
    SourceBackedCoordinatorError::InvalidRoute {
        provider,
        detail: detail.into(),
    }
}

pub(crate) fn route_error(error: impl fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, error.to_string())
}

pub(crate) fn route_coordinator_error(
    error: SourceBackedCoordinatorError,
) -> SourceBackedRouteError {
    match error {
        SourceBackedCoordinatorError::CoreEmission(source) => source,
        error => {
            SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, error.to_string())
        }
    }
}

fn capture_coordinator_error(
    failure: &mut Option<SourceBackedRouteError>,
    error: SourceBackedCoordinatorError,
) -> CaptureError {
    let error = route_coordinator_error(error);
    let detail = error.to_string();
    *failure = Some(error);
    CaptureError::InvalidPayload(detail)
}
