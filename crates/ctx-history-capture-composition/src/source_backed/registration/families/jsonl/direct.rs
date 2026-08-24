use super::*;

const DIRECT_ROUTES: &[RouteEntry] = &[
    RouteEntry::new(CaptureProvider::Gemini, register_gemini_source_backed_route),
    RouteEntry::new(
        CaptureProvider::Antigravity,
        register_direct_jsonl_source_backed_route,
    ),
    RouteEntry::new(
        CaptureProvider::GrokBuild,
        register_direct_jsonl_source_backed_route,
    ),
    RouteEntry::new(
        CaptureProvider::Tabnine,
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
    RouteEntry::new(CaptureProvider::Claude, register_claude_source_backed_route),
];

pub(super) fn registration(provider: CaptureProvider) -> Option<DirectRouteRegistration> {
    direct_route_registration(DIRECT_ROUTES, provider)
}

fn register_claude_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        ctx_history_provider_claude_cursor::claude_jsonl_adapter::<CaptureProviderRuntime>(),
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

pub(in crate::source_backed) fn register_configured_claude_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        ctx_history_provider_claude_cursor::claude_jsonl_adapter_for_named_home::<
            CaptureProviderRuntime,
        >(source_root_lineage),
        source.path.clone(),
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

fn register_direct_jsonl_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    use crate::provider::source_backed::family::jsonl::NativeJsonlCaptureRuntime;
    use ctx_history_provider_native_jsonl::native_path::{
        antigravity_source_backed_adapter, copilot_source_backed_adapter,
        factory_droid_source_backed_adapter, grok_build_source_backed_adapter,
        qoder_source_backed_adapter, qwen_code_source_backed_adapter,
        tabnine_source_backed_adapter,
    };

    let adapter = match source.provider {
        CaptureProvider::Antigravity => {
            antigravity_source_backed_adapter::<NativeJsonlCaptureRuntime>()
        }
        CaptureProvider::CopilotCli => copilot_source_backed_adapter::<NativeJsonlCaptureRuntime>(),
        CaptureProvider::FactoryAiDroid => {
            factory_droid_source_backed_adapter::<NativeJsonlCaptureRuntime>()
        }
        CaptureProvider::GrokBuild => {
            grok_build_source_backed_adapter::<NativeJsonlCaptureRuntime>()
        }
        CaptureProvider::Qoder => qoder_source_backed_adapter::<NativeJsonlCaptureRuntime>(),
        CaptureProvider::QwenCode => qwen_code_source_backed_adapter::<NativeJsonlCaptureRuntime>(),
        CaptureProvider::Tabnine => tabnine_source_backed_adapter::<NativeJsonlCaptureRuntime>(),
        provider => {
            return Err(invalid_route(
                provider,
                "provider is not a direct native-JSONL family member",
            ));
        }
    };
    let driver = ctx_history_jsonl::jsonl_family_driver(Arc::new(adapter), source.path.clone());
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
    let adapter = ctx_history_provider_gemini::nativepath::gemini_jsonl_adapter::<
        crate::provider::source_backed::family::jsonl::GeminiCaptureJsonlRuntime,
    >();
    let driver = crate::provider::source_backed::family::jsonl::gemini_jsonl_family_driver(
        adapter,
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

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn gemini_legacy_v1_source_backed_driver_for_test(
    root: std::path::PathBuf,
) -> SourceBackedRouteDriver {
    let adapter = ctx_history_provider_gemini::nativepath::gemini_legacy_v1_jsonl_adapter_for_test::<
        crate::provider::source_backed::family::jsonl::GeminiCaptureJsonlRuntime,
    >();
    crate::provider::source_backed::family::jsonl::gemini_jsonl_family_driver(adapter, root)
}
