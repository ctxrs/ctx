use ctx_history_core::CaptureProvider;

pub const DEFAULT_PROVIDER_IMPORT_REVISION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderImportRevision {
    pub provider: CaptureProvider,
    pub source_format: &'static str,
    pub revision: u32,
}

macro_rules! revision {
    ($provider:ident, $source_format:literal) => {
        ProviderImportRevision {
            provider: CaptureProvider::$provider,
            source_format: $source_format,
            revision: DEFAULT_PROVIDER_IMPORT_REVISION,
        }
    };
}

pub const PROVIDER_IMPORT_REVISIONS: &[ProviderImportRevision] = &[
    revision!(Codex, "codex_session_jsonl_tree"),
    revision!(Codex, "codex_session_jsonl"),
    revision!(Codex, "codex_history_jsonl"),
    revision!(Pi, "pi_session_jsonl"),
    revision!(Claude, "claude_projects_jsonl_tree"),
    revision!(OpenCode, "opencode_sqlite"),
    revision!(Kilo, "kilo_sqlite"),
    revision!(MiMoCode, "mimocode_sqlite"),
    revision!(KiroCli, "kiro_cli_sqlite"),
    revision!(Crush, "crush_sqlite"),
    revision!(Goose, "goose_sessions_sqlite"),
    revision!(Antigravity, "antigravity_cli_transcript_jsonl_tree"),
    revision!(Gemini, "gemini_cli_chat_recording_jsonl"),
    revision!(Tabnine, "tabnine_cli_chat_recording_jsonl"),
    revision!(Cursor, "cursor_agent_transcript_jsonl_tree"),
    revision!(Cursor, "cursor_agent_transcript_jsonl"),
    revision!(Windsurf, "windsurf_cascade_hook_transcript_jsonl_tree"),
    revision!(Windsurf, "windsurf_cascade_hook_transcript_jsonl"),
    revision!(Zed, "zed_threads_sqlite"),
    revision!(CopilotCli, "copilot_cli_session_events_jsonl"),
    revision!(FactoryAiDroid, "factory_ai_droid_sessions_jsonl"),
    revision!(QwenCode, "qwen_code_chat_jsonl_tree"),
    revision!(QwenCode, "qwen_code_chat_jsonl"),
    revision!(KimiCodeCli, "kimi_code_cli_wire_jsonl_tree"),
    revision!(KimiCodeCli, "kimi_code_cli_wire_jsonl"),
    revision!(Auggie, "auggie_session_json"),
    revision!(Junie, "junie_session_events_jsonl_tree"),
    revision!(Junie, "junie_session_events_jsonl"),
    revision!(Firebender, "firebender_chat_history_sqlite"),
    revision!(ForgeCode, "forgecode_sqlite"),
    revision!(DeepAgents, "deepagents_sessions_sqlite"),
    revision!(MistralVibe, "mistral_vibe_session_jsonl_tree"),
    revision!(MistralVibe, "mistral_vibe_session_jsonl"),
    revision!(Mux, "mux_session_jsonl_tree"),
    revision!(Mux, "mux_session_jsonl"),
    revision!(RovoDev, "rovodev_session_json_tree"),
    revision!(OpenClaw, "openclaw_session_jsonl_tree"),
    revision!(Hermes, "hermes_state_sqlite"),
    revision!(NanoClaw, "nanoclaw_project"),
    revision!(AstrBot, "astrbot_data_v4_sqlite"),
    revision!(Shelley, "shelley_sqlite"),
    revision!(Continue, "continue_cli_sessions_json"),
    revision!(OpenHands, "openhands_file_events"),
    revision!(Cline, "cline_task_directory_json"),
    revision!(RooCode, "roo_task_directory_json"),
    revision!(Lingma, "lingma_sqlite"),
    revision!(Trae, "trae_state_vscdb"),
    revision!(Qoder, "qoder_transcript_jsonl_tree"),
    revision!(Qoder, "qoder_transcript_jsonl"),
    revision!(Warp, "warp_sqlite"),
    revision!(CodeBuddy, "codebuddy_history_json"),
];

pub fn provider_import_revision(provider: CaptureProvider, source_format: &str) -> u32 {
    PROVIDER_IMPORT_REVISIONS
        .iter()
        .find(|entry| entry.provider == provider && entry.source_format == source_format)
        .map(|entry| entry.revision)
        .unwrap_or(DEFAULT_PROVIDER_IMPORT_REVISION)
}
