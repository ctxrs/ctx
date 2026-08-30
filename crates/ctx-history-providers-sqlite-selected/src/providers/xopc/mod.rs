//! XOPC's provider-owned SQLite session importer.

mod source_backed;

pub fn xopc_source_backed_driver<B: crate::SelectedSqliteCaptureBinding>(
    source_path: &std::path::Path,
    data_root: &std::path::Path,
) -> crate::Result<
    ctx_history_capture_runtime::SourceBackedRouteDriver<B::Lifecycle, B::RouteControl>,
> {
    xopc_source_backed_driver_scoped::<B>(
        source_path,
        data_root,
        ctx_history_core::SourceAnchorScope::Unqualified,
    )
}

pub fn xopc_source_backed_driver_scoped<B: crate::SelectedSqliteCaptureBinding>(
    source_path: &std::path::Path,
    data_root: &std::path::Path,
    source_scope: ctx_history_core::SourceAnchorScope,
) -> crate::Result<
    ctx_history_capture_runtime::SourceBackedRouteDriver<B::Lifecycle, B::RouteControl>,
> {
    source_backed::source_backed_driver_scoped::<B>(source_path, data_root, source_scope)
        .map_err(|error| crate::CaptureError::InvalidPayload(error.to_string()))
}
