use super::*;

mod document;
mod event_file;
mod hermes;
mod jsonl;
mod openclaw;
mod sqlite;
mod sqlite_inventory;

pub use document::*;
pub(in crate::source_backed) use event_file::register_openhands_automatic_route;
use event_file::register_openhands_route;
pub use hermes::*;
pub use jsonl::*;
pub use sqlite::*;
pub use sqlite_inventory::*;

type DirectRouteRegistration = fn(
    &mut SourceBackedProviderRegistry,
    ProviderSource,
    SourceBackedRouteSelection,
    Option<[u8; 32]>,
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
    register_landed_source_backed_route_inner(registry, source, selection, None, None)
}

pub fn register_landed_source_backed_route_with_data_root(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
) -> SourceBackedCoordinatorResult<()> {
    register_landed_source_backed_route_inner(registry, source, selection, Some(data_root), None)
}

/// Registers a landed source with the one configured-root lineage scope that
/// adapters use for durable provider-native identity. `None` is the released
/// unqualified contract.
pub(in crate::source_backed) fn register_landed_source_backed_route_with_data_root_and_lineage(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    register_landed_source_backed_route_inner(
        registry,
        source,
        selection,
        Some(data_root),
        source_root_lineage,
    )
}

fn register_landed_source_backed_route_inner(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: Option<&Path>,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    match source.provider {
        CaptureProvider::Codex
        | CaptureProvider::GrokBuild
        | CaptureProvider::DeepSeekHarness
        | CaptureProvider::Claude
        | CaptureProvider::Pi
        | CaptureProvider::Antigravity
        | CaptureProvider::Tabnine
        | CaptureProvider::Gemini
        | CaptureProvider::Cursor
        | CaptureProvider::CopilotCli
        | CaptureProvider::FactoryAiDroid
        | CaptureProvider::QwenCode
        | CaptureProvider::KimiCodeCli
        | CaptureProvider::Junie
        | CaptureProvider::MistralVibe
        | CaptureProvider::Mux
        | CaptureProvider::Qoder => {
            jsonl::register_route(registry, source, selection, source_root_lineage)
        }
        CaptureProvider::OpenClaw => {
            openclaw::register_route(registry, source, selection, data_root, source_root_lineage)
        }
        CaptureProvider::Hermes => {
            let data_root = data_root.ok_or_else(|| {
                invalid_route(
                    source.provider,
                    "Hermes registration requires the selected ctx data root",
                )
            })?;
            hermes::register_hermes_source_backed_route(
                registry,
                source,
                selection,
                data_root,
                source_root_lineage,
            )
        }
        CaptureProvider::OpenCode
        | CaptureProvider::Kilo
        | CaptureProvider::KiroCli
        | CaptureProvider::Zed
        | CaptureProvider::Firebender
        | CaptureProvider::ForgeCode
        | CaptureProvider::DeepAgents
        | CaptureProvider::MiMoCode => {
            let data_root = data_root.ok_or_else(|| {
                invalid_route(
                    source.provider,
                    "provider SQLite registration requires the selected ctx data root",
                )
            })?;
            sqlite::register_route(registry, source, selection, data_root, source_root_lineage)
        }
        CaptureProvider::Auggie
        | CaptureProvider::RovoDev
        | CaptureProvider::Continue
        | CaptureProvider::Cline
        | CaptureProvider::RooCode
        | CaptureProvider::CodeBuddy => {
            document::register_route(registry, source, selection, data_root, source_root_lineage)
        }
        CaptureProvider::OpenHands => {
            register_openhands_route(registry, source, selection, source_root_lineage)
        }
        provider => Err(invalid_route(
            provider,
            "this provider requires its compound-selector registration constructor",
        )),
    }
}
