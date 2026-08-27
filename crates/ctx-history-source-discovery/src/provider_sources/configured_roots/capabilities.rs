use ctx_history_core::CaptureProvider;

use super::{
    ConfiguredRootCapability, ConfiguredRootCapabilityState, ConfiguredRootExpander,
    ConfiguredRootPathKind,
};

const fn exact_source(
    expected_path_kind: ConfiguredRootPathKind,
    source_format: &'static str,
    route_role: &'static str,
) -> ConfiguredRootCapabilityState {
    ConfiguredRootCapabilityState::Enabled {
        expected_path_kind,
        expander: ConfiguredRootExpander::ExactSource {
            source_format,
            route_role,
        },
    }
}

const fn compound_source(expander: ConfiguredRootExpander) -> ConfiguredRootCapabilityState {
    ConfiguredRootCapabilityState::Enabled {
        expected_path_kind: ConfiguredRootPathKind::Directory,
        expander,
    }
}

const INTENTIONAL_AUTOMATIC_EXACT: ConfiguredRootCapabilityState =
    ConfiguredRootCapabilityState::IntentionalAutomaticExact;

// Keep this table in the exact landed provider-spec order. It is the one
// exhaustive configured-root capability declaration for all 42 providers.
pub(super) const CONFIGURED_ROOT_CAPABILITIES: &[ConfiguredRootCapability] = &[
    ConfiguredRootCapability {
        provider: CaptureProvider::Codex,
        state: compound_source(ConfiguredRootExpander::CodexHomeV1),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::GrokBuild,
        state: exact_source(
            ConfiguredRootPathKind::Directory,
            "grok_build_session_updates_jsonl_tree",
            "grok-build-sessions",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::DeepSeekHarness,
        state: exact_source(
            ConfiguredRootPathKind::Directory,
            "deepseek_harness_session_jsonl_tree",
            "deepseek-harness-sessions",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::Pi,
        state: exact_source(
            ConfiguredRootPathKind::Directory,
            "pi_session_jsonl",
            "pi-sessions",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::Claude,
        state: compound_source(ConfiguredRootExpander::ClaudeHomeV1),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::OpenCode,
        state: exact_source(
            ConfiguredRootPathKind::File,
            "opencode_sqlite",
            "opencode-database",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::Kilo,
        state: exact_source(ConfiguredRootPathKind::File, "kilo_sqlite", "kilo-database"),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::MiMoCode,
        state: exact_source(
            ConfiguredRootPathKind::File,
            "mimocode_sqlite",
            "mimocode-database",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::KiroCli,
        state: exact_source(
            ConfiguredRootPathKind::File,
            "kiro_cli_sqlite",
            "kiro-cli-database",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::Crush,
        state: exact_source(
            ConfiguredRootPathKind::File,
            "crush_sqlite",
            "crush-project-database",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::Goose,
        state: exact_source(
            ConfiguredRootPathKind::File,
            "goose_sessions_sqlite",
            "goose-sessions-database",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::Antigravity,
        state: exact_source(
            ConfiguredRootPathKind::Directory,
            "antigravity_cli_transcript_jsonl_tree",
            "antigravity-brain",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::Gemini,
        state: exact_source(
            ConfiguredRootPathKind::Directory,
            "gemini_cli_chat_recording_jsonl",
            "gemini-chats",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::Tabnine,
        state: exact_source(
            ConfiguredRootPathKind::Directory,
            "tabnine_cli_chat_recording_jsonl",
            "tabnine-agent-history",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::Cursor,
        state: exact_source(
            ConfiguredRootPathKind::Directory,
            "cursor_agent_transcript_jsonl_tree",
            "cursor-projects",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::Zed,
        state: exact_source(
            ConfiguredRootPathKind::File,
            "zed_threads_sqlite",
            "zed-threads-database",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::CopilotCli,
        state: exact_source(
            ConfiguredRootPathKind::Directory,
            "copilot_cli_session_events_jsonl",
            "copilot-session-state",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::FactoryAiDroid,
        state: exact_source(
            ConfiguredRootPathKind::Directory,
            "factory_ai_droid_sessions_jsonl",
            "factory-droid-sessions",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::QwenCode,
        state: exact_source(
            ConfiguredRootPathKind::Directory,
            "qwen_code_chat_jsonl_tree",
            "qwen-projects",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::KimiCodeCli,
        state: exact_source(
            ConfiguredRootPathKind::Directory,
            "kimi_code_cli_wire_jsonl_tree",
            "kimi-history",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::Auggie,
        state: exact_source(
            ConfiguredRootPathKind::Directory,
            "auggie_session_json",
            "auggie-sessions",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::Junie,
        state: exact_source(
            ConfiguredRootPathKind::Directory,
            "junie_session_events_jsonl_tree",
            "junie-sessions",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::Firebender,
        state: exact_source(
            ConfiguredRootPathKind::File,
            "firebender_chat_history_sqlite",
            "firebender-chat-history-database",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::ForgeCode,
        state: exact_source(
            ConfiguredRootPathKind::File,
            "forgecode_sqlite",
            "forgecode-database",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::DeepAgents,
        state: exact_source(
            ConfiguredRootPathKind::File,
            "deepagents_sessions_sqlite",
            "deepagents-sessions-database",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::MistralVibe,
        state: exact_source(
            ConfiguredRootPathKind::Directory,
            "mistral_vibe_session_jsonl_tree",
            "mistral-vibe-sessions",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::Mux,
        state: exact_source(
            ConfiguredRootPathKind::Directory,
            "mux_session_jsonl_tree",
            "mux-sessions",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::RovoDev,
        state: exact_source(
            ConfiguredRootPathKind::Directory,
            "rovodev_session_json_tree",
            "rovodev-sessions",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::OpenClaw,
        state: compound_source(ConfiguredRootExpander::OpenClawStateRootV1),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::Hermes,
        state: exact_source(
            ConfiguredRootPathKind::File,
            "hermes_state_sqlite",
            "hermes-profile-database",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::NanoClaw,
        state: INTENTIONAL_AUTOMATIC_EXACT,
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::AstrBot,
        state: exact_source(
            ConfiguredRootPathKind::File,
            "astrbot_data_v4_sqlite",
            "astrbot-instance-database",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::Shelley,
        state: INTENTIONAL_AUTOMATIC_EXACT,
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::Continue,
        state: exact_source(
            ConfiguredRootPathKind::Directory,
            "continue_cli_sessions_json",
            "continue-sessions",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::OpenHands,
        state: compound_source(ConfiguredRootExpander::OpenHandsKindV1),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::Cline,
        state: compound_source(ConfiguredRootExpander::ClineCommonDataRootV1),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::RooCode,
        state: exact_source(
            ConfiguredRootPathKind::Directory,
            "roo_task_directory_json",
            "roo-task-store",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::Lingma,
        state: exact_source(
            ConfiguredRootPathKind::File,
            "lingma_sqlite",
            "lingma-client-profile-database",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::Qoder,
        state: exact_source(
            ConfiguredRootPathKind::Directory,
            "qoder_transcript_jsonl_tree",
            "qoder-projects",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::Warp,
        state: exact_source(
            ConfiguredRootPathKind::File,
            "warp_sqlite",
            "warp-surface-database",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::CodeBuddy,
        state: exact_source(
            ConfiguredRootPathKind::Directory,
            "codebuddy_history_json",
            "codebuddy-history",
        ),
    },
    ConfiguredRootCapability {
        provider: CaptureProvider::Fx,
        state: exact_source(
            ConfiguredRootPathKind::Directory,
            "fx_sessions_tree",
            "fx-sessions",
        ),
    },
];
