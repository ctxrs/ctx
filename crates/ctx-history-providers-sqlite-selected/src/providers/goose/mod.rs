mod content;
mod normalization;
mod position;
mod schema;
mod source_backed;
mod stream;

pub(crate) use source_backed::{GooseSourceBackedAdapterV0, GooseSourceBackedSelectionV0};

pub use source_backed::GooseSourceRouteV0 as GooseSourceRoute;

pub fn goose_source_backed_driver<B: crate::SelectedSqliteCaptureBinding>(
    source_path: &std::path::Path,
    data_root: &std::path::Path,
    platform_root: std::path::PathBuf,
    retained_routes: Vec<GooseSourceRoute>,
) -> crate::Result<
    ctx_history_capture_runtime::SourceBackedRouteDriver<B::Lifecycle, B::RouteControl>,
> {
    goose_source_backed_driver_scoped::<B>(
        source_path,
        data_root,
        platform_root,
        retained_routes,
        ctx_history_core::SourceAnchorScope::Unqualified,
    )
}

pub fn goose_source_backed_driver_scoped<B: crate::SelectedSqliteCaptureBinding>(
    source_path: &std::path::Path,
    data_root: &std::path::Path,
    platform_root: std::path::PathBuf,
    retained_routes: Vec<GooseSourceRoute>,
    source_scope: ctx_history_core::SourceAnchorScope,
) -> crate::Result<
    ctx_history_capture_runtime::SourceBackedRouteDriver<B::Lifecycle, B::RouteControl>,
> {
    let mut selected = GooseSourceBackedSelectionV0::exact(data_root, source_path, platform_root);
    if !retained_routes.is_empty() {
        selected = selected
            .with_explicit_retained_routes(retained_routes)
            .map_err(|error| crate::CaptureError::InvalidPayload(error.to_string()))?;
    }
    let adapter = GooseSourceBackedAdapterV0::<B>::open_scoped(selected, source_scope)
        .map_err(|error| crate::CaptureError::InvalidPayload(error.to_string()))?;
    Ok(
        ctx_history_capture_runtime::replacement_document_tree_driver(
            crate::document_inventory_authority(
                ctx_history_core::CaptureProvider::Goose.as_str(),
                crate::GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
                source_path,
            ),
            adapter,
        ),
    )
}

#[cfg(test)]
pub(crate) mod tests;
