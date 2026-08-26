use std::path::Path;

use super::*;
use crate::provider::source_backed::{
    family::document::register_replacement_document_tree_route, CaptureProviderRuntime,
};

pub(super) fn register_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: Option<&Path>,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    if source.source_format
        != ctx_history_provider_openclaw_sqlite::OPENCLAW_AGENT_SQLITE_SOURCE_FORMAT
    {
        return jsonl::register_route(registry, source, selection, source_root_lineage);
    }
    let data_root = data_root.ok_or_else(|| {
        invalid_route(
            source.provider,
            "OpenClaw SQLite registration requires the selected ctx data root",
        )
    })?;
    let adapter = ctx_history_provider_openclaw_sqlite::OpenClawSqliteAdapter::<
        CaptureProviderRuntime,
    >::new_scoped(
        data_root,
        &source.path,
        source_root_lineage.map_or(
            ctx_history_core::SourceAnchorScope::Unqualified,
            ctx_history_core::SourceAnchorScope::Lineage,
        ),
    );
    register_replacement_document_tree_route(registry, source, selection, adapter)
}
