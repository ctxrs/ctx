use super::*;
use crate::provider::source_backed::family::document::register_replacement_document_tree_route_with_authority;
use ctx_history_core::SourceAnchorScope;
#[cfg(test)]
use ctx_history_provider_docproj::OPENHANDS_FILE_EVENTS_SOURCE_FORMAT;

mod automatic;
use automatic::openhands_automatic_retirement;

/// OpenHands event-file conversations now use the common replacement-document
/// lifecycle. Each conversation is an independently staged logical source;
/// inventory and terminal tree revalidation remain route-wide authorities.
pub(super) fn register_openhands_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    register_openhands_route_with_current_root(
        registry,
        source,
        selection,
        None,
        source_root_lineage,
    )
}

pub(in crate::source_backed) fn register_openhands_automatic_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    current_root: &Path,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    register_openhands_route_with_current_root(
        registry,
        source,
        SourceBackedRouteSelection::Automatic,
        Some(current_root),
        source_root_lineage,
    )
}

fn register_openhands_route_with_current_root(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    current_root: Option<&Path>,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let authority = landed_format_route(source.provider, source.source_format)
        .ok_or_else(|| invalid_route(source.provider, "unknown OpenHands source format"))?
        .selector_authority;
    let automatic_retirement = openhands_automatic_retirement(&source, selection, current_root)?;
    let source_anchor_scope =
        source_root_lineage.map_or(SourceAnchorScope::Unqualified, SourceAnchorScope::Lineage);
    let adapter = if source.source_format == OPENHANDS_CURRENT_CLI_SOURCE_FORMAT {
        OpenHandsEventFileAdapterV2::<CaptureProviderRuntime>::new_current_conversations_scoped(
            source.path.clone(),
            source_anchor_scope,
        )
    } else {
        OpenHandsEventFileAdapterV2::<CaptureProviderRuntime>::new_scoped(
            source.path.clone(),
            source_anchor_scope,
        )
    };
    register_replacement_document_tree_route_with_authority(
        registry, source, selection, authority, adapter,
    )?;
    if let Some((replacement, retired)) = automatic_retirement {
        registry.retire_automatic_routes_after_success(&replacement, [retired])?;
    }
    Ok(())
}

#[cfg(test)]
mod automatic_lifecycle_tests;

#[cfg(test)]
mod test_helpers;

#[cfg(test)]
mod tests;
