use super::*;

mod codex;
mod direct;
mod other;

pub use codex::*;
pub use direct::*;
pub use other::*;

pub(super) fn register_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    if let Some(register) = direct::registration(source.provider) {
        return register(registry, source, selection);
    }
    match source.provider {
        CaptureProvider::Codex if source.source_format == "codex_history_jsonl" => {
            codex::register_codex_prompt_history_source_backed_route(registry, source, selection)
        }
        CaptureProvider::Codex if source.source_format == "codex_session_jsonl_tree" => {
            codex::register_codex_session_tree_route(registry, source, selection)
        }
        CaptureProvider::Codex if source.source_format == "codex_session_jsonl" => {
            codex::register_codex_explicit_session_route(registry, source, selection)
        }
        CaptureProvider::Codex => Err(invalid_route(
            source.provider,
            "unknown Codex source format",
        )),
        CaptureProvider::Cursor => {
            other::register_cursor_source_backed_route(registry, source, selection)
        }
        CaptureProvider::Pi => other::register_pi_route(registry, source, selection),
        CaptureProvider::Junie => other::register_junie_route(registry, source, selection),
        CaptureProvider::KimiCodeCli => other::register_kimi_route(registry, source, selection),
        CaptureProvider::MistralVibe => other::register_mistral_route(registry, source, selection),
        CaptureProvider::OpenClaw => other::register_openclaw_route(registry, source, selection),
        CaptureProvider::Mux => other::register_mux_route(registry, source, selection),
        provider => Err(invalid_route(
            provider,
            "this provider is not registered by the JSONL route family",
        )),
    }
}
