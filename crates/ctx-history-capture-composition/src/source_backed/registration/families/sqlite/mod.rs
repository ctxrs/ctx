use super::*;

mod logical;
mod other;

pub use logical::*;
pub use other::*;

pub(super) fn register_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    match source.provider {
        CaptureProvider::Zed => {
            logical::register_zed_route(registry, source, selection, data_root, source_root_lineage)
        }
        CaptureProvider::KiroCli => other::register_kiro_source_backed_route(
            registry,
            source,
            selection,
            data_root,
            source_root_lineage,
        ),
        CaptureProvider::Firebender => other::register_firebender_source_backed_route(
            registry,
            source,
            selection,
            data_root,
            source_root_lineage,
        ),
        CaptureProvider::DeepAgents => logical::register_deepagents_route(
            registry,
            source,
            selection,
            data_root,
            source_root_lineage,
        ),
        CaptureProvider::ForgeCode => logical::register_forgecode_selected_route(
            registry,
            source,
            selection,
            data_root,
            source_root_lineage,
        ),
        CaptureProvider::OpenCode | CaptureProvider::Kilo | CaptureProvider::MiMoCode => {
            logical::register_opencode_family_route(
                registry,
                source,
                selection,
                data_root,
                source_root_lineage,
            )
        }
        provider => Err(invalid_route(
            provider,
            "this provider is not registered by the SQLite route family",
        )),
    }
}
