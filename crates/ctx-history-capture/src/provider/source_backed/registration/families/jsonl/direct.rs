use super::*;

const DIRECT_ROUTES: &[RouteEntry] = &[
    RouteEntry::new(CaptureProvider::Gemini, register_gemini_source_backed_route),
    RouteEntry::new(
        CaptureProvider::Antigravity,
        crate::provider::providers::native_jsonl::native_path::register_source_backed_route,
    ),
    RouteEntry::new(
        CaptureProvider::Tabnine,
        crate::provider::providers::native_jsonl::native_path::register_source_backed_route,
    ),
    RouteEntry::new(
        CaptureProvider::Windsurf,
        crate::provider::providers::native_jsonl::native_path::register_source_backed_route,
    ),
    RouteEntry::new(
        CaptureProvider::CopilotCli,
        crate::provider::providers::native_jsonl::native_path::register_source_backed_route,
    ),
    RouteEntry::new(
        CaptureProvider::FactoryAiDroid,
        crate::provider::providers::native_jsonl::native_path::register_source_backed_route,
    ),
    RouteEntry::new(
        CaptureProvider::QwenCode,
        crate::provider::providers::native_jsonl::native_path::register_source_backed_route,
    ),
    RouteEntry::new(
        CaptureProvider::Qoder,
        crate::provider::providers::native_jsonl::native_path::register_source_backed_route,
    ),
    RouteEntry::new(
        CaptureProvider::Claude,
        crate::provider::providers::claude::nativepath::register_source_backed_route,
    ),
];

pub(super) fn registration(provider: CaptureProvider) -> Option<DirectRouteRegistration> {
    direct_route_registration(DIRECT_ROUTES, provider)
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
