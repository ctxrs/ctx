use chrono::{DateTime, Utc};

pub(crate) use ctx_history_provider_docproj::providers::auggie::native_path::source_backed;

use crate::provider::source_backed::{
    family::document::register_replacement_document_tree_route, CaptureProviderRuntime,
    SourceBackedCoordinatorResult, SourceBackedProviderRegistry, SourceBackedRouteSelection,
};
use crate::{ProviderAdapterContext, ProviderSource};
use ctx_history_core::SourceAnchorScope;

pub(crate) fn register_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let context = ProviderAdapterContext {
        machine_id: "source-backed-auggie".to_owned(),
        source_path: Some(source.path.clone()),
        source_root: Some(source.path.clone()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let adapter = source_backed::AuggieDocumentTreeAdapter::<CaptureProviderRuntime>::new_scoped(
        source_backed::AuggieSourceBackedRoot::explicit(source.path.clone()),
        context,
        source_root_lineage.map_or(SourceAnchorScope::Unqualified, SourceAnchorScope::Lineage),
    );
    register_replacement_document_tree_route(registry, source, selection, adapter)
}
