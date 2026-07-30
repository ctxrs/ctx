use super::*;

mod families;

pub use families::*;

pub(crate) fn provider_format_scope(
    provider: CaptureProvider,
    source_format: &'static str,
) -> impl Fn(&SourceKey) -> bool + Send + Sync + 'static {
    move |source| source.provider() == provider.as_str() && source.source_format() == source_format
}

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

pub(crate) fn route_capture_error(error: CaptureError) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Unavailable, error.to_string())
}

pub(crate) fn route_error(error: impl fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, error.to_string())
}

pub(crate) fn route_coordinator_error(
    error: SourceBackedCoordinatorError,
) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, error.to_string())
}

fn capture_coordinator_error(error: SourceBackedCoordinatorError) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

pub(in crate::provider::source_backed) fn codex_display_bytes(
    hydrated: CodexHydratedRecordV0,
) -> Result<Vec<u8>, HydrationFailure> {
    hydrated
        .decoded_display_text
        .map(String::into_bytes)
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::UnsupportedParserRevision,
                "Codex record has no exact decoded display text",
            )
        })
}

pub(in crate::provider::source_backed) fn firebender_display_bytes(
    messages_json: &[u8],
    message_index: u64,
) -> Result<Vec<u8>, HydrationFailure> {
    let messages = serde_json::from_slice::<Vec<serde_json::Value>>(messages_json)
        .map_err(|error| hydration_failure(HydrationFailureKind::StaleRecordEvidence, error))?;
    let index = usize::try_from(message_index).map_err(|_| {
        hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Firebender message index exceeds platform limits",
        )
    })?;
    let message = messages.get(index).ok_or_else(|| {
        hydration_failure(
            HydrationFailureKind::MissingRecord,
            "Firebender message is absent from its verified source row",
        )
    })?;
    firebender_message_text(message)
        .map(String::into_bytes)
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::UnsupportedParserRevision,
                "Firebender message has no exact decoded display text",
            )
        })
}

pub(crate) fn hydration_failure(
    kind: HydrationFailureKind,
    detail: impl fmt::Display,
) -> HydrationFailure {
    HydrationFailure {
        kind,
        detail: detail.to_string(),
    }
}
