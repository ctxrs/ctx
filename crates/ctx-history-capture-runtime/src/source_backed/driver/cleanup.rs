use super::{route_error_severity, SourceBackedRouteError};

/// Preserves both failures from an explicit source cleanup while retaining the
/// stronger route-level failure class for coordinator policy.
pub fn combine_primary_and_cleanup_route_errors(
    primary: SourceBackedRouteError,
    cleanup: SourceBackedRouteError,
) -> SourceBackedRouteError {
    let kind = if route_error_severity(primary.kind) >= route_error_severity(cleanup.kind) {
        primary.kind
    } else {
        cleanup.kind
    };
    // Policy severity does not establish one shared cause for both failures.
    let diagnostic = if primary.diagnostic == cleanup.diagnostic {
        primary.diagnostic
    } else {
        None
    };
    let mut result = SourceBackedRouteError::new(
        kind,
        format!(
            "{}; explicit SQLite snapshot cleanup also failed: {}",
            primary.detail, cleanup.detail
        ),
    );
    result.diagnostic = diagnostic;
    result
}
