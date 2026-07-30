use super::*;

mod document;
mod event_file;
mod jsonl;
mod sqlite;

pub use document::*;
use event_file::*;
pub use jsonl::*;
pub use sqlite::*;

type DirectRouteRegistration = fn(
    &mut SourceBackedProviderRegistry,
    ProviderSource,
    SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()>;

#[derive(Clone, Copy)]
struct RouteEntry {
    provider: CaptureProvider,
    register: DirectRouteRegistration,
}

impl RouteEntry {
    const fn new(provider: CaptureProvider, register: DirectRouteRegistration) -> Self {
        Self { provider, register }
    }
}

fn direct_route_registration(
    entries: &[RouteEntry],
    provider: CaptureProvider,
) -> Option<DirectRouteRegistration> {
    entries
        .iter()
        .find(|entry| entry.provider == provider)
        .map(|entry| entry.register)
}

/// Mechanical registration entry for landed routes that require no additional
/// selector token beyond their selected path. Providers with compound
/// selectors have dedicated constructors so those selectors cannot be
/// fabricated here.
pub fn register_landed_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    register_landed_source_backed_route_inner(registry, source, selection, None)
}

pub fn register_landed_source_backed_route_with_data_root(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
) -> SourceBackedCoordinatorResult<()> {
    register_landed_source_backed_route_inner(registry, source, selection, Some(data_root))
}

fn register_landed_source_backed_route_inner(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: Option<&Path>,
) -> SourceBackedCoordinatorResult<()> {
    match source.provider {
        CaptureProvider::Codex
        | CaptureProvider::Claude
        | CaptureProvider::Pi
        | CaptureProvider::Antigravity
        | CaptureProvider::Tabnine
        | CaptureProvider::Windsurf
        | CaptureProvider::Gemini
        | CaptureProvider::Cursor
        | CaptureProvider::CopilotCli
        | CaptureProvider::FactoryAiDroid
        | CaptureProvider::QwenCode
        | CaptureProvider::KimiCodeCli
        | CaptureProvider::Junie
        | CaptureProvider::MistralVibe
        | CaptureProvider::Mux
        | CaptureProvider::OpenClaw
        | CaptureProvider::Qoder => jsonl::register_route(registry, source, selection),
        CaptureProvider::OpenCode
        | CaptureProvider::Kilo
        | CaptureProvider::KiroCli
        | CaptureProvider::Zed
        | CaptureProvider::Firebender
        | CaptureProvider::ForgeCode
        | CaptureProvider::DeepAgents
        | CaptureProvider::Hermes
        | CaptureProvider::Trae
        | CaptureProvider::MiMoCode => {
            let data_root = data_root.ok_or_else(|| {
                invalid_route(
                    source.provider,
                    "provider SQLite registration requires the selected ctx data root",
                )
            })?;
            sqlite::register_route(registry, source, selection, data_root)
        }
        CaptureProvider::Auggie
        | CaptureProvider::RovoDev
        | CaptureProvider::Continue
        | CaptureProvider::Cline
        | CaptureProvider::RooCode
        | CaptureProvider::CodeBuddy => document::register_route(registry, source, selection),
        CaptureProvider::OpenHands => register_openhands_route(registry, source, selection),
        provider => Err(invalid_route(
            provider,
            "this provider requires its compound-selector registration constructor",
        )),
    }
}
