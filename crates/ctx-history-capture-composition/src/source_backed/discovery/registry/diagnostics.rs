use super::{
    SourceBackedAutomaticUnavailableReason, SourceBackedCoordinatorError,
    SourceBackedRouteErrorKind, WarpDiscoveryUnavailable,
};

pub(super) fn automatic_registration_rejected(
    error: SourceBackedCoordinatorError,
) -> SourceBackedAutomaticUnavailableReason {
    let kind = match &error {
        SourceBackedCoordinatorError::RouteScan { source, .. }
        | SourceBackedCoordinatorError::RouteRegistration { source, .. }
        | SourceBackedCoordinatorError::Progress(source)
        | SourceBackedCoordinatorError::CoreEmission(source) => source.kind,
        SourceBackedCoordinatorError::UnavailableRoute { .. } => {
            SourceBackedRouteErrorKind::Unavailable
        }
        SourceBackedCoordinatorError::InvalidRoute { .. }
        | SourceBackedCoordinatorError::InvalidRefreshScope { .. } => {
            SourceBackedRouteErrorKind::Unsupported
        }
        _ => SourceBackedRouteErrorKind::Internal,
    };
    SourceBackedAutomaticUnavailableReason::RegistrationRejected {
        diagnostic: match &error {
            SourceBackedCoordinatorError::RouteScan { source, .. }
            | SourceBackedCoordinatorError::RouteRegistration { source, .. }
            | SourceBackedCoordinatorError::Progress(source)
            | SourceBackedCoordinatorError::CoreEmission(source) => source.diagnostic,
            _ => None,
        },
        kind,
        detail: error.to_string(),
    }
}

pub(super) const fn warp_discovery_unavailable_detail(
    error: WarpDiscoveryUnavailable,
) -> &'static str {
    match error {
        WarpDiscoveryUnavailable::UnsupportedPlatform { .. } => {
            "Warp installed-surface authority is unavailable on this platform"
        }
        WarpDiscoveryUnavailable::WindowsLocalDataRootUnavailable => {
            "Warp installed-surface authority has no Windows local-data root"
        }
        WarpDiscoveryUnavailable::ProviderSpecUnavailable => {
            "Warp provider discovery specification is unavailable"
        }
        WarpDiscoveryUnavailable::SourceCandidateRejected { .. } => {
            "Warp installed-surface discovery rejected the selected source within fixed bounds"
        }
        WarpDiscoveryUnavailable::SourceNotSelected => {
            "Warp source is absent from authoritative installed-surface discovery"
        }
    }
}
