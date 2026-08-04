use super::*;

#[cfg(test)]
use crate::provider::codex::nativepath::CodexSourceBackedCountersV0;
use crate::provider::codex::nativepath::{
    codex_session_root_rank, CodexExplicitSessionJsonlFamilyAdapterV0,
    CodexExplicitSessionSourceBackedInputV0, CodexPromptHistoryJsonlFamilyAdapterV0,
    CodexPromptHistorySourceBackedInputV0, CodexSessionTreeJsonlFamilyAdapterV0,
};

#[cfg(test)]
type ExplicitCodexStageHook = Box<dyn FnOnce(CodexSourceBackedCountersV0)>;

#[cfg(test)]
type CodexSessionTreeStageHook = Box<dyn FnOnce(CodexSourceBackedCountersV0)>;

#[cfg(test)]
std::thread_local! {
    static AFTER_EXPLICIT_CODEX_STAGE_HOOK:
        std::cell::RefCell<Option<ExplicitCodexStageHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
std::thread_local! {
    static AFTER_CODEX_SESSION_TREE_STAGE_HOOK:
        std::cell::RefCell<Option<CodexSessionTreeStageHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_after_explicit_codex_stage_hook(
    hook: impl FnOnce(CodexSourceBackedCountersV0) + 'static,
) {
    AFTER_EXPLICIT_CODEX_STAGE_HOOK.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "explicit Codex stage hook is already installed"
        );
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
pub(crate) fn set_after_codex_session_tree_stage_hook(
    hook: impl FnOnce(CodexSourceBackedCountersV0) + 'static,
) {
    AFTER_CODEX_SESSION_TREE_STAGE_HOOK.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "Codex session-tree stage hook is already installed"
        );
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn run_after_codex_session_tree_stage_hook(counters: CodexSourceBackedCountersV0) {
    let hook = AFTER_CODEX_SESSION_TREE_STAGE_HOOK.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook(counters);
    }
}

#[cfg(test)]
fn run_after_explicit_codex_stage_hook(counters: CodexSourceBackedCountersV0) {
    let hook = AFTER_EXPLICIT_CODEX_STAGE_HOOK.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook(counters);
    }
}

pub(super) fn register_codex_session_tree_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    register_codex_session_tree_routes(registry, vec![source], selection)
}

pub(in crate::provider::source_backed) fn register_codex_session_tree_routes(
    registry: &mut SourceBackedProviderRegistry,
    mut sources: Vec<ProviderSource>,
    selection: SourceBackedRouteSelection,
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
    let adapter = CodexSessionTreeJsonlFamilyAdapterV0::new(roots)
        .map_err(|error| invalid_route(CaptureProvider::Codex, error.to_string()))?;
    #[cfg(test)]
    let adapter = adapter.with_after_stage_observer(run_after_codex_session_tree_stage_hook);
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        Arc::new(adapter),
        source.path.clone(),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
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
    let adapter = CodexExplicitSessionJsonlFamilyAdapterV0::new(input);
    #[cfg(test)]
    let adapter = adapter.with_after_stage_observer(run_after_explicit_codex_stage_hook);
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        Arc::new(adapter),
        route_path,
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::ExplicitPath,
        driver,
    )?);
    Ok(())
}
// SHA-256("ctx.codex.prompt-history.default-catalog-lineage.v0"). This is
// catalog-route identity, not a digest of the user-specific source path.
pub(in crate::provider::source_backed) const CODEX_PROMPT_HISTORY_DEFAULT_CATALOG_LINEAGE_V0: [u8;
    32] = [
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
    let input = CodexPromptHistorySourceBackedInputV0::explicit(
        source.path.clone(),
        CODEX_PROMPT_HISTORY_DEFAULT_CATALOG_LINEAGE_V0,
    );
    let adapter = CodexPromptHistoryJsonlFamilyAdapterV0::new(input)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ProviderCatalogSupport, ProviderImportSupport, ProviderSourceKind, ProviderSourceStatus,
    };

    #[test]
    fn codex_session_tree_registration_does_not_inventory_the_root() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let sessions = temp.path().join("sessions-not-created");
        let source = ProviderSource {
            provider: CaptureProvider::Codex,
            path: sessions,
            exists: true,
            source_format: "codex_session_jsonl_tree",
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        };
        let mut registry = SourceBackedProviderRegistry::new();

        register_codex_session_tree_routes(
            &mut registry,
            vec![source],
            SourceBackedRouteSelection::Automatic,
        )
        .unwrap();

        assert_eq!(registry.routes().count(), 1);
    }
}
