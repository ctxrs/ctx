use super::*;

mod inventory;
mod logical;
mod other;

pub use inventory::*;
pub use logical::*;
pub use other::*;

pub(super) fn sqlite_source_route_error(
    error: crate::provider_sources::SqliteSourceAccessError,
) -> SourceBackedRouteError {
    SourceBackedRouteError::new(sqlite_source_route_error_kind(&error), error.to_string())
}

pub(super) fn sqlite_source_route_error_kind(
    error: &crate::provider_sources::SqliteSourceAccessError,
) -> SourceBackedRouteErrorKind {
    if error.is_source_changed() {
        SourceBackedRouteErrorKind::SourceChanged
    } else if error.is_systemic_resource_failure() || error.is_busy_or_locked() {
        SourceBackedRouteErrorKind::ResourceUnavailable
    } else if error.is_ctx_owned_corruption() {
        SourceBackedRouteErrorKind::Internal
    } else if error.is_provider_corruption() || error.is_provider_path_unavailable() {
        SourceBackedRouteErrorKind::InvalidSource
    } else if error.is_operational_failure() {
        SourceBackedRouteErrorKind::Internal
    } else {
        SourceBackedRouteErrorKind::InvalidSource
    }
}

pub(super) fn sqlite_capture_route_error(
    error: &CaptureError,
) -> Option<SourceBackedRouteErrorKind> {
    match error {
        CaptureError::SourceChangedDuringCapture => Some(SourceBackedRouteErrorKind::SourceChanged),
        CaptureError::Io(error) | CaptureError::SystemIo { source: error, .. }
            if crate::provider_sources::resource_exhaustion_io_error(error) =>
        {
            Some(SourceBackedRouteErrorKind::ResourceUnavailable)
        }
        CaptureError::Sqlite(error)
            if crate::provider_sources::rusqlite_resource_failure(error)
                || crate::provider_sources::rusqlite_busy_or_locked(error) =>
        {
            Some(SourceBackedRouteErrorKind::ResourceUnavailable)
        }
        CaptureError::Io(_) | CaptureError::SystemIo { .. } | CaptureError::Sqlite(_) => {
            Some(SourceBackedRouteErrorKind::Internal)
        }
        _ => None,
    }
}

pub(super) fn register_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
) -> SourceBackedCoordinatorResult<()> {
    match source.provider {
        CaptureProvider::Zed => logical::register_zed_route(registry, source, selection, data_root),
        CaptureProvider::KiroCli => {
            crate::provider::providers::kiro::native_path::register_source_backed_route(
                registry, source, selection, data_root,
            )
        }
        CaptureProvider::Firebender => {
            crate::provider::providers::firebender::native_path::register_source_backed_route(
                registry, source, selection, data_root,
            )
        }
        CaptureProvider::DeepAgents => {
            logical::register_deepagents_route(registry, source, selection, data_root)
        }
        CaptureProvider::ForgeCode => {
            logical::register_forgecode_selected_route(registry, source, selection, data_root)
        }
        CaptureProvider::OpenCode | CaptureProvider::Kilo | CaptureProvider::MiMoCode => {
            logical::register_opencode_family_route(registry, source, selection, data_root)
        }
        CaptureProvider::Hermes => {
            logical::register_hermes_route(registry, source, selection, data_root)
        }
        CaptureProvider::Trae => {
            logical::register_trae_route(registry, source, selection, data_root)
        }
        provider => Err(invalid_route(
            provider,
            "this provider is not registered by the SQLite route family",
        )),
    }
}
