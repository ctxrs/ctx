use ctx_history_capture_runtime::{SourceBackedRouteError, SourceBackedRouteErrorKind};

use super::{JsonlFamilyAdapter, JsonlFamilyError, JsonlFamilyRuntime, JsonlRuntimeError};

pub(super) fn route_invalid(error: impl std::fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, error.to_string())
}

pub(super) fn route_discovery<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    error: JsonlRuntimeError<R>,
) -> SourceBackedRouteError {
    SourceBackedRouteError::new(
        normalized_jsonl_error_kind(&error).unwrap_or_else(|| adapter.discovery_error_kind(&error)),
        error.to_string(),
    )
}

pub(super) fn route_scan<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    error: JsonlRuntimeError<R>,
) -> SourceBackedRouteError {
    let kind = if error.is_not_found() {
        Some(SourceBackedRouteErrorKind::SourceChanged)
    } else {
        normalized_jsonl_error_kind(&error)
    }
    .unwrap_or_else(|| adapter.scan_error_kind(&error));
    SourceBackedRouteError::new(kind, error.to_string())
}

pub(super) fn normalized_jsonl_error_kind<E: JsonlFamilyError>(
    error: &E,
) -> Option<SourceBackedRouteErrorKind> {
    if error.is_source_changed() {
        Some(SourceBackedRouteErrorKind::SourceChanged)
    } else if error.is_not_found() {
        None
    } else if error.is_source_unavailable() {
        Some(SourceBackedRouteErrorKind::Unavailable)
    } else if error.is_resource_unavailable() {
        Some(SourceBackedRouteErrorKind::ResourceUnavailable)
    } else if error.is_internal() {
        Some(SourceBackedRouteErrorKind::Internal)
    } else {
        None
    }
}

pub(super) fn route_internal(error: impl std::fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, error.to_string())
}

pub(super) fn contract_error<E: JsonlFamilyError>(error: impl std::fmt::Display) -> E {
    E::invalid_payload(error.to_string())
}
