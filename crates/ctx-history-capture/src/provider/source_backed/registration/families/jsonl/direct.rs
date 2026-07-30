use super::*;

const DIRECT_ROUTES: &[RouteEntry] = &[
    RouteEntry::new(CaptureProvider::Gemini, register_gemini_source_backed_route),
    RouteEntry::new(
        CaptureProvider::Antigravity,
        register_direct_jsonl_source_backed_route,
    ),
    RouteEntry::new(
        CaptureProvider::Tabnine,
        register_direct_jsonl_source_backed_route,
    ),
    RouteEntry::new(
        CaptureProvider::Windsurf,
        register_direct_jsonl_source_backed_route,
    ),
    RouteEntry::new(
        CaptureProvider::CopilotCli,
        register_direct_jsonl_source_backed_route,
    ),
    RouteEntry::new(
        CaptureProvider::FactoryAiDroid,
        register_direct_jsonl_source_backed_route,
    ),
    RouteEntry::new(
        CaptureProvider::QwenCode,
        register_direct_jsonl_source_backed_route,
    ),
    RouteEntry::new(
        CaptureProvider::Qoder,
        register_direct_jsonl_source_backed_route,
    ),
    RouteEntry::new(
        CaptureProvider::Claude,
        crate::provider::providers::claude::nativepath::register_source_backed_route,
    ),
];

pub(super) fn registration(provider: CaptureProvider) -> Option<DirectRouteRegistration> {
    direct_route_registration(DIRECT_ROUTES, provider)
}

fn register_direct_jsonl_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    use crate::provider::providers::native_jsonl::native_path::{
        antigravity_source_backed_adapter, copilot_source_backed_adapter,
        factory_droid_source_backed_adapter, qoder_source_backed_adapter,
        qwen_code_source_backed_adapter, tabnine_source_backed_adapter,
        windsurf_source_backed_adapter,
    };

    let adapter = match source.provider {
        CaptureProvider::Antigravity => antigravity_source_backed_adapter(),
        CaptureProvider::CopilotCli => copilot_source_backed_adapter(),
        CaptureProvider::FactoryAiDroid => factory_droid_source_backed_adapter(),
        CaptureProvider::Qoder => qoder_source_backed_adapter(),
        CaptureProvider::QwenCode => qwen_code_source_backed_adapter(),
        CaptureProvider::Tabnine => tabnine_source_backed_adapter(),
        CaptureProvider::Windsurf => windsurf_source_backed_adapter(),
        provider => {
            return Err(invalid_route(
                provider,
                "provider is not a direct native-JSONL family member",
            ));
        }
    };
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

/// Registers the landed Gemini adapter without moving any provider parsing
/// logic into the coordinator.
pub fn register_gemini_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    crate::provider::providers::gemini::nativepath::register_source_backed_route(
        registry, source, selection,
    )
}
