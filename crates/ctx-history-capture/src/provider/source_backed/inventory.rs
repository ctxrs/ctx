use super::*;

/// Whether a route was selected by provider discovery or supplied manually.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedRouteSelection {
    Automatic,
    ExplicitManual,
}

/// Provider-specific authority that must survive central registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedSelectorAuthority {
    DiscoveredWinner,
    ExplicitPath,
    CatalogLineage,
    ExactCwd,
    NamedSurface,
    SelectedWithRetainedExplicit,
}

/// Exact hydration coverage advertised by a landed adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedHydrationSupport {
    Full,
    Unsupported,
}

/// Static inventory of one landed provider/source-format registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceBackedProviderRouteMetadata {
    pub provider: CaptureProvider,
    /// Format carried by the discovered or explicitly selected root.
    pub source_format: &'static str,
    /// Format carried by the `SourceKey`s certified by the adapter.
    pub certified_source_format: &'static str,
    pub automatic: bool,
    pub explicit_manual: bool,
    pub selector_authority: SourceBackedSelectorAuthority,
    pub exact_hydration: SourceBackedHydrationSupport,
    pub hydration_limitation: Option<&'static str>,
    pub unsupported_reason: Option<&'static str>,
}

macro_rules! route {
    (
        $provider:ident, $selected_format:literal => $certified_format:literal,
        $automatic:literal, $explicit:literal, $authority:ident, $hydration:ident
    ) => {
        SourceBackedProviderRouteMetadata {
            provider: CaptureProvider::$provider,
            source_format: $selected_format,
            certified_source_format: $certified_format,
            automatic: $automatic,
            explicit_manual: $explicit,
            selector_authority: SourceBackedSelectorAuthority::$authority,
            exact_hydration: SourceBackedHydrationSupport::$hydration,
            hydration_limitation: None,
            unsupported_reason: None,
        }
    };
    (
        $provider:ident, $format:literal, $automatic:literal, $explicit:literal,
        $authority:ident, $hydration:ident
    ) => {
        SourceBackedProviderRouteMetadata {
            provider: CaptureProvider::$provider,
            source_format: $format,
            certified_source_format: $format,
            automatic: $automatic,
            explicit_manual: $explicit,
            selector_authority: SourceBackedSelectorAuthority::$authority,
            exact_hydration: SourceBackedHydrationSupport::$hydration,
            hydration_limitation: None,
            unsupported_reason: None,
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedRouteConstructor {
    ProviderSource,
    CatalogLineage,
    FiniteInventory,
    DiscoveryContext,
    ExactCwd,
    NamedSurface,
    SelectedWithRetainedRoutes,
}

pub const fn source_backed_route_constructor(
    provider: CaptureProvider,
) -> Option<SourceBackedRouteConstructor> {
    Some(match provider {
        CaptureProvider::Custom | CaptureProvider::NanoClaw => {
            SourceBackedRouteConstructor::CatalogLineage
        }
        CaptureProvider::Crush | CaptureProvider::Lingma => {
            SourceBackedRouteConstructor::FiniteInventory
        }
        CaptureProvider::AstrBot => SourceBackedRouteConstructor::DiscoveryContext,
        CaptureProvider::Shelley => SourceBackedRouteConstructor::ExactCwd,
        CaptureProvider::Warp => SourceBackedRouteConstructor::NamedSurface,
        CaptureProvider::Goose => SourceBackedRouteConstructor::SelectedWithRetainedRoutes,
        CaptureProvider::Codex
        | CaptureProvider::Claude
        | CaptureProvider::Pi
        | CaptureProvider::OpenCode
        | CaptureProvider::Kilo
        | CaptureProvider::KiroCli
        | CaptureProvider::Antigravity
        | CaptureProvider::Gemini
        | CaptureProvider::Tabnine
        | CaptureProvider::Cursor
        | CaptureProvider::Windsurf
        | CaptureProvider::Zed
        | CaptureProvider::CopilotCli
        | CaptureProvider::FactoryAiDroid
        | CaptureProvider::QwenCode
        | CaptureProvider::KimiCodeCli
        | CaptureProvider::Auggie
        | CaptureProvider::Junie
        | CaptureProvider::Firebender
        | CaptureProvider::ForgeCode
        | CaptureProvider::DeepAgents
        | CaptureProvider::MistralVibe
        | CaptureProvider::Mux
        | CaptureProvider::RovoDev
        | CaptureProvider::OpenClaw
        | CaptureProvider::Hermes
        | CaptureProvider::Continue
        | CaptureProvider::OpenHands
        | CaptureProvider::Cline
        | CaptureProvider::RooCode
        | CaptureProvider::Qoder
        | CaptureProvider::CodeBuddy
        | CaptureProvider::Trae
        | CaptureProvider::MiMoCode => SourceBackedRouteConstructor::ProviderSource,
        _ => return None,
    })
}

/// The central landed-adapter inventory. Adding a provider is deliberately a
/// data entry plus one private driver registration, not a new public trait.
pub const LANDED_SOURCE_BACKED_ROUTES: &[SourceBackedProviderRouteMetadata] = &[
    route!(
        Custom,
        "ctx_history_jsonl_v1",
        false,
        true,
        CatalogLineage,
        Full
    ),
    route!(
        Codex,
        "codex_session_jsonl_tree" => "codex_session_jsonl",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Codex,
        "codex_history_jsonl" => "codex_history_jsonl",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Codex,
        "codex_session_jsonl" => "codex_session_jsonl",
        false,
        true,
        ExplicitPath,
        Full
    ),
    route!(
        Claude,
        "claude_projects_jsonl_tree",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(Pi, "pi_session_jsonl", true, true, DiscoveredWinner, Full),
    route!(
        OpenCode,
        "opencode_sqlite",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(Kilo, "kilo_sqlite", true, true, DiscoveredWinner, Full),
    route!(
        KiroCli,
        "kiro_cli_sqlite",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Antigravity,
        "antigravity_cli_transcript_jsonl_tree",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Gemini,
        "gemini_cli_chat_recording_jsonl",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Tabnine,
        "tabnine_cli_chat_recording_jsonl",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Cursor,
        "cursor_agent_transcript_jsonl_tree" => "cursor_agent_transcript_jsonl_tree",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Cursor,
        "cursor_agent_transcript_jsonl" => "cursor_agent_transcript_jsonl_tree",
        false,
        true,
        ExplicitPath,
        Full
    ),
    route!(
        Windsurf,
        "windsurf_cascade_hook_transcript_jsonl_tree" => "windsurf_cascade_hook_transcript_jsonl",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Windsurf,
        "windsurf_cascade_hook_transcript_jsonl" => "windsurf_cascade_hook_transcript_jsonl",
        false,
        true,
        ExplicitPath,
        Full
    ),
    route!(
        Zed,
        "zed_threads_sqlite",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        CopilotCli,
        "copilot_cli_session_events_jsonl",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        FactoryAiDroid,
        "factory_ai_droid_sessions_jsonl",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        QwenCode,
        "qwen_code_chat_jsonl_tree" => "qwen_code_chat_jsonl",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        QwenCode,
        "qwen_code_chat_jsonl" => "qwen_code_chat_jsonl",
        false,
        true,
        ExplicitPath,
        Full
    ),
    route!(
        KimiCodeCli,
        "kimi_code_cli_wire_jsonl_tree" => "kimi_code_cli_wire_jsonl",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        KimiCodeCli,
        "kimi_code_cli_wire_jsonl" => "kimi_code_cli_wire_jsonl",
        false,
        true,
        ExplicitPath,
        Full
    ),
    route!(
        Auggie,
        "auggie_session_json",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Junie,
        "junie_session_events_jsonl_tree" => "junie_session_events_jsonl_tree",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Junie,
        "junie_session_events_jsonl" => "junie_session_events_jsonl_tree",
        false,
        true,
        ExplicitPath,
        Full
    ),
    route!(
        Firebender,
        "firebender_chat_history_sqlite",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        ForgeCode,
        "forgecode_sqlite",
        true,
        true,
        SelectedWithRetainedExplicit,
        Full
    ),
    route!(
        DeepAgents,
        "deepagents_sessions_sqlite",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        MistralVibe,
        "mistral_vibe_session_jsonl_tree" => "mistral_vibe_session_jsonl",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        MistralVibe,
        "mistral_vibe_session_jsonl" => "mistral_vibe_session_jsonl",
        false,
        true,
        ExplicitPath,
        Full
    ),
    route!(
        Mux,
        "mux_session_jsonl_tree" => "mux_session_jsonl",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Mux,
        "mux_session_jsonl" => "mux_session_jsonl",
        false,
        true,
        ExplicitPath,
        Full
    ),
    route!(
        RovoDev,
        "rovodev_session_json_tree",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        OpenClaw,
        "openclaw_session_jsonl_tree",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Hermes,
        "hermes_state_sqlite",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        NanoClaw,
        "nanoclaw_project",
        false,
        true,
        CatalogLineage,
        Full
    ),
    route!(
        AstrBot,
        "astrbot_data_v4_sqlite",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(Shelley, "shelley_sqlite", true, false, ExactCwd, Full),
    route!(
        Continue,
        "continue_cli_sessions_json",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        OpenHands,
        "openhands_file_events",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Cline,
        "cline_task_directory_json",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        RooCode,
        "roo_task_directory_json",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Crush,
        "crush_sqlite",
        true,
        true,
        SelectedWithRetainedExplicit,
        Full
    ),
    route!(
        Goose,
        "goose_sessions_sqlite",
        true,
        true,
        SelectedWithRetainedExplicit,
        Full
    ),
    route!(Lingma, "lingma_sqlite", true, true, DiscoveredWinner, Full),
    route!(
        Qoder,
        "qoder_transcript_jsonl_tree" => "qoder_transcript_jsonl",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(
        Qoder,
        "qoder_transcript_jsonl" => "qoder_transcript_jsonl",
        false,
        true,
        ExplicitPath,
        Full
    ),
    route!(Warp, "warp_sqlite", true, true, NamedSurface, Full),
    route!(
        CodeBuddy,
        "codebuddy_history_json",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
    route!(Trae, "trae_state_vscdb", true, true, ExplicitPath, Full),
    route!(
        MiMoCode,
        "mimocode_sqlite",
        true,
        true,
        DiscoveredWinner,
        Full
    ),
];

pub fn source_backed_route_inventory() -> &'static [SourceBackedProviderRouteMetadata] {
    LANDED_SOURCE_BACKED_ROUTES
}
