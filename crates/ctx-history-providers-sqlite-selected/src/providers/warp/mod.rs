mod nativepath;
mod schema;
mod source_backed;
mod wire;

pub(crate) use source_backed::{project_warp_source_backed_v0_scoped, WarpSourceSelectionV0};

pub fn warp_source_backed_driver<B: crate::SelectedSqliteCaptureBinding>(
    source_path: &std::path::Path,
    data_root: &std::path::Path,
    surface_key: impl Into<String>,
) -> crate::Result<
    ctx_history_capture_runtime::SourceBackedRouteDriver<B::Lifecycle, B::RouteControl>,
> {
    warp_source_backed_driver_scoped::<B>(
        source_path,
        data_root,
        surface_key,
        ctx_history_core::SourceAnchorScope::Unqualified,
    )
}

pub fn warp_source_backed_driver_scoped<B: crate::SelectedSqliteCaptureBinding>(
    source_path: &std::path::Path,
    data_root: &std::path::Path,
    surface_key: impl Into<String>,
    source_scope: ctx_history_core::SourceAnchorScope,
) -> crate::Result<
    ctx_history_capture_runtime::SourceBackedRouteDriver<B::Lifecycle, B::RouteControl>,
> {
    let selection = WarpSourceSelectionV0::new(data_root, source_path, surface_key)
        .map_err(|error| crate::CaptureError::InvalidPayload(error.to_string()))?;
    let adapter = project_warp_source_backed_v0_scoped::<B>(selection, source_scope)
        .map_err(|error| crate::CaptureError::InvalidPayload(error.to_string()))?;
    Ok(
        ctx_history_capture_runtime::replacement_document_tree_driver(
            crate::document_inventory_authority(
                ctx_history_core::CaptureProvider::Warp.as_str(),
                crate::WARP_SQLITE_SOURCE_FORMAT,
                source_path,
            ),
            adapter,
        ),
    )
}

#[cfg(test)]
#[path = "source_backed_tests.rs"]
mod source_backed_tests;
