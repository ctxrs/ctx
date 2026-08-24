use super::*;

use crate::provider::codex::nativepath::{
    codex_session_root_rank, CodexExplicitSessionSourceBackedInputV0,
    CodexPromptHistoryJsonlFamilyAdapterV0, CodexPromptHistorySourceBackedInputV0,
    CodexSessionJsonlFamilyAdapterV0,
};
use crate::provider::source_backed::family::CaptureProviderRuntime;

pub(super) fn register_codex_session_tree_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    register_codex_session_tree_routes_with_identity(registry, vec![source], selection, None, false)
}

pub(in crate::source_backed) fn register_configured_codex_session_tree_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    register_codex_session_tree_routes_with_identity(
        registry,
        vec![source],
        selection,
        source_root_lineage,
        true,
    )
}

pub(in crate::source_backed) fn register_configured_codex_session_tree_routes(
    registry: &mut SourceBackedProviderRegistry,
    sources: Vec<ProviderSource>,
    selection: SourceBackedRouteSelection,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    register_codex_session_tree_routes_with_identity(
        registry,
        sources,
        selection,
        source_root_lineage,
        true,
    )
}

pub(in crate::source_backed) fn register_codex_session_tree_routes(
    registry: &mut SourceBackedProviderRegistry,
    sources: Vec<ProviderSource>,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    register_codex_session_tree_routes_with_identity(registry, sources, selection, None, false)
}

fn register_codex_session_tree_routes_with_identity(
    registry: &mut SourceBackedProviderRegistry,
    mut sources: Vec<ProviderSource>,
    selection: SourceBackedRouteSelection,
    source_root_lineage: Option<[u8; 32]>,
    provider_root_identity: bool,
) -> SourceBackedCoordinatorResult<()> {
    if sources.is_empty() {
        return Err(invalid_route(
            CaptureProvider::Codex,
            "Codex session-tree authority has no roots",
        ));
    }
    if sources.iter().any(|source| {
        source.provider != CaptureProvider::Codex
            || source.source_format != "codex_session_jsonl_tree"
    }) {
        return Err(invalid_route(
            CaptureProvider::Codex,
            "Codex session-tree authority contains a non-Codex root",
        ));
    }
    sources.sort_by(|left, right| {
        codex_session_root_rank(&left.path)
            .cmp(&codex_session_root_rank(&right.path))
            .then_with(|| left.path.cmp(&right.path))
    });
    sources.dedup_by(|left, right| left.path == right.path);
    let source = sources.first().cloned().ok_or_else(|| {
        invalid_route(CaptureProvider::Codex, "Codex session-tree root is absent")
    })?;
    let roots = sources
        .iter()
        .map(|source| source.path.clone())
        .collect::<Vec<_>>();
    let coordinator = registry
        .codex_generation
        .get_or_insert_with(|| Arc::new(CodexGenerationNormalizationCoordinatorV0::default()))
        .clone();
    let generation = coordinator
        .register_session_tree(roots, source_root_lineage)
        .map_err(|error| invalid_route(CaptureProvider::Codex, error.to_string()))?;
    let participant = generation.participant();
    let adapter = CodexSessionJsonlFamilyAdapterV0::<CaptureProviderRuntime>::new(generation);
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        Arc::new(adapter),
        source.path.clone(),
    );
    let mut route = executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?;
    if provider_root_identity {
        route.apply_provider_root_route_identity(source_root_lineage)?;
    }
    route.registration_sources = sources;
    route.codex_generation_participant = Some(participant);
    registry.register(route);
    Ok(())
}

pub(super) fn register_codex_explicit_session_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let input = CodexExplicitSessionSourceBackedInputV0::discover(&source.path)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let route_path = input.path().to_path_buf();
    let coordinator = registry
        .codex_generation
        .get_or_insert_with(|| Arc::new(CodexGenerationNormalizationCoordinatorV0::default()))
        .clone();
    let generation = coordinator
        .register_explicit_session(input.clone())
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let participant = generation.participant();
    let adapter = CodexSessionJsonlFamilyAdapterV0::<CaptureProviderRuntime>::new(generation);
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        Arc::new(adapter),
        route_path,
    );
    let mut route = executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::ExplicitPath,
        driver,
    )?;
    route.codex_generation_participant = Some(participant);
    registry.register(route);
    Ok(())
}
// SHA-256("ctx.codex.prompt-history.default-catalog-lineage.v0"). This is
// catalog-route identity, not a digest of the user-specific source path.
pub(in crate::source_backed) const CODEX_PROMPT_HISTORY_DEFAULT_CATALOG_LINEAGE_V0: [u8; 32] = [
    0x2d, 0x2e, 0xb3, 0x41, 0xde, 0xe9, 0x7a, 0xd3, 0x15, 0xec, 0xfa, 0xb3, 0x33, 0x20, 0x7c, 0x44,
    0x53, 0x18, 0xb9, 0x32, 0x1c, 0xc1, 0x6b, 0xf2, 0x2c, 0xdb, 0x09, 0x68, 0xe0, 0xf1, 0xf5, 0x0a,
];

/// Registers Codex's one default prompt-history catalog route while retaining
/// the opened ordinary-file authority for scanning and revalidation. The
/// selected path never participates in public source identity.
pub fn register_codex_prompt_history_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let catalog_lineage = match selection {
        SourceBackedRouteSelection::Automatic => CODEX_PROMPT_HISTORY_DEFAULT_CATALOG_LINEAGE_V0,
        SourceBackedRouteSelection::ExplicitManual => {
            explicit_source_catalog_lineage(source.provider, "codex_history_jsonl", &source.path)
        }
    };
    let input =
        CodexPromptHistorySourceBackedInputV0::explicit(source.path.clone(), catalog_lineage);
    let adapter = CodexPromptHistoryJsonlFamilyAdapterV0::<CaptureProviderRuntime>::new(input)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let route_path = adapter.route_path().to_path_buf();
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        Arc::new(adapter),
        route_path,
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

pub(in crate::source_backed) fn register_configured_codex_prompt_history_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let catalog_lineage =
        source_root_lineage.unwrap_or(CODEX_PROMPT_HISTORY_DEFAULT_CATALOG_LINEAGE_V0);
    let input =
        CodexPromptHistorySourceBackedInputV0::explicit(source.path.clone(), catalog_lineage);
    let adapter = CodexPromptHistoryJsonlFamilyAdapterV0::<CaptureProviderRuntime>::new(input)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let route_path = adapter.route_path().to_path_buf();
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        Arc::new(adapter),
        route_path,
    );
    let mut route = executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?;
    route.apply_provider_root_route_identity(source_root_lineage)?;
    registry.register(route);
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "codex/prompt_history_lifecycle_tests.rs"]
mod prompt_history_lifecycle_tests;
