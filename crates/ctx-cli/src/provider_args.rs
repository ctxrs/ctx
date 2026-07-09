use clap::ValueEnum;
use ctx_history_core::CaptureProvider;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum NativeProviderArg {
    Codex,
    Pi,
    #[value(alias = "claude-code")]
    Claude,
    #[value(name = "opencode", alias = "open-code")]
    OpenCode,
    #[value(
        name = "kilo",
        alias = "kilo-code",
        alias = "kilo_code",
        alias = "kilocode"
    )]
    Kilo,
    #[value(name = "kiro-cli", alias = "kiro", alias = "kiro_cli")]
    KiroCli,
    Crush,
    Goose,
    #[value(alias = "antigravity-cli")]
    Antigravity,
    #[value(alias = "gemini-cli")]
    Gemini,
    #[value(alias = "tabnine-cli")]
    Tabnine,
    Cursor,
    #[value(
        name = "windsurf",
        alias = "windsurf-cascade",
        alias = "windsurf_cascade"
    )]
    Windsurf,
    Zed,
    #[value(alias = "copilot", alias = "copilot_cli", alias = "github-copilot")]
    CopilotCli,
    #[value(
        alias = "factoryai-droid",
        alias = "factory-droid",
        alias = "factory_ai_droid",
        alias = "droid"
    )]
    FactoryAiDroid,
    #[value(name = "qwen-code", alias = "qwen", alias = "qwen_code")]
    QwenCode,
    #[value(name = "kimi-code-cli", alias = "kimi", alias = "kimi_code_cli")]
    KimiCodeCli,
    #[value(name = "auggie", alias = "augment", alias = "augment-code")]
    Auggie,
    Junie,
    #[value(
        name = "firebender",
        alias = "firebender-jetbrains",
        alias = "firebender_jetbrains"
    )]
    Firebender,
    #[value(
        name = "forgecode",
        alias = "forge",
        alias = "forge-code",
        alias = "forge_code"
    )]
    ForgeCode,
    #[value(name = "deepagents", alias = "deep-agents", alias = "dcode")]
    DeepAgents,
    #[value(name = "mistral-vibe", alias = "mistral", alias = "mistral_vibe")]
    MistralVibe,
    Mux,
    #[value(name = "rovodev", alias = "rovo-dev", alias = "rovo_dev")]
    RovoDev,
    #[value(name = "openclaw", alias = "open-claw", alias = "open_claw")]
    OpenClaw,
    Hermes,
    #[value(name = "nanoclaw", alias = "nano-claw", alias = "nano_claw")]
    NanoClaw,
    #[value(name = "astrbot", alias = "astr-bot", alias = "astr_bot")]
    AstrBot,
    Shelley,
    #[value(alias = "continue-cli")]
    Continue,
    #[value(name = "openhands", alias = "open-hands", alias = "open_hands")]
    OpenHands,
    Cline,
    #[value(name = "roo", alias = "roo-code", alias = "roo_code")]
    RooCode,
    #[value(alias = "qoder-cn", alias = "qoder_cn")]
    Lingma,
    #[value(name = "mimocode", alias = "mimo-code", alias = "mimo_code")]
    MiMoCode,
    Qoder,
    Warp,
    #[value(name = "codebuddy", alias = "code-buddy", alias = "code_buddy")]
    CodeBuddy,
    #[value(alias = "trae-cn", alias = "trae_cn")]
    Trae,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum ProviderArg {
    Codex,
    Pi,
    #[value(alias = "claude-code")]
    Claude,
    #[value(name = "opencode", alias = "open-code")]
    OpenCode,
    #[value(
        name = "kilo",
        alias = "kilo-code",
        alias = "kilo_code",
        alias = "kilocode"
    )]
    Kilo,
    #[value(name = "kiro-cli", alias = "kiro", alias = "kiro_cli")]
    KiroCli,
    Crush,
    Goose,
    #[value(alias = "antigravity-cli")]
    Antigravity,
    #[value(alias = "gemini-cli")]
    Gemini,
    #[value(alias = "tabnine-cli")]
    Tabnine,
    Cursor,
    #[value(
        name = "windsurf",
        alias = "windsurf-cascade",
        alias = "windsurf_cascade"
    )]
    Windsurf,
    Zed,
    #[value(alias = "copilot", alias = "copilot_cli", alias = "github-copilot")]
    CopilotCli,
    #[value(
        alias = "factoryai-droid",
        alias = "factory-droid",
        alias = "factory_ai_droid",
        alias = "droid"
    )]
    FactoryAiDroid,
    #[value(name = "qwen-code", alias = "qwen", alias = "qwen_code")]
    QwenCode,
    #[value(name = "kimi-code-cli", alias = "kimi", alias = "kimi_code_cli")]
    KimiCodeCli,
    #[value(name = "auggie", alias = "augment", alias = "augment-code")]
    Auggie,
    Junie,
    #[value(
        name = "firebender",
        alias = "firebender-jetbrains",
        alias = "firebender_jetbrains"
    )]
    Firebender,
    #[value(
        name = "forgecode",
        alias = "forge",
        alias = "forge-code",
        alias = "forge_code"
    )]
    ForgeCode,
    #[value(name = "deepagents", alias = "deep-agents", alias = "dcode")]
    DeepAgents,
    #[value(name = "mistral-vibe", alias = "mistral", alias = "mistral_vibe")]
    MistralVibe,
    Mux,
    #[value(name = "rovodev", alias = "rovo-dev", alias = "rovo_dev")]
    RovoDev,
    #[value(name = "openclaw", alias = "open-claw", alias = "open_claw")]
    OpenClaw,
    Hermes,
    #[value(name = "nanoclaw", alias = "nano-claw", alias = "nano_claw")]
    NanoClaw,
    #[value(name = "astrbot", alias = "astr-bot", alias = "astr_bot")]
    AstrBot,
    Shelley,
    #[value(alias = "continue-cli")]
    Continue,
    #[value(name = "openhands", alias = "open-hands", alias = "open_hands")]
    OpenHands,
    Cline,
    #[value(name = "roo", alias = "roo-code", alias = "roo_code")]
    RooCode,
    #[value(alias = "qoder-cn", alias = "qoder_cn")]
    Lingma,
    #[value(name = "mimocode", alias = "mimo-code", alias = "mimo_code")]
    MiMoCode,
    Qoder,
    Warp,
    #[value(name = "codebuddy", alias = "code-buddy", alias = "code_buddy")]
    CodeBuddy,
    #[value(alias = "trae-cn", alias = "trae_cn")]
    Trae,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ImportFormatArg {
    #[value(name = "ctx-history-jsonl-v1", alias = "custom-history-jsonl-v1")]
    CtxHistoryJsonlV1,
}

impl ImportFormatArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::CtxHistoryJsonlV1 => "ctx-history-jsonl-v1",
        }
    }
}

impl NativeProviderArg {
    pub(crate) fn capture_provider(self) -> CaptureProvider {
        match self {
            Self::Codex => CaptureProvider::Codex,
            Self::Pi => CaptureProvider::Pi,
            Self::Claude => CaptureProvider::Claude,
            Self::OpenCode => CaptureProvider::OpenCode,
            Self::Kilo => CaptureProvider::Kilo,
            Self::KiroCli => CaptureProvider::KiroCli,
            Self::Crush => CaptureProvider::Crush,
            Self::Goose => CaptureProvider::Goose,
            Self::Antigravity => CaptureProvider::Antigravity,
            Self::Gemini => CaptureProvider::Gemini,
            Self::Tabnine => CaptureProvider::Tabnine,
            Self::Cursor => CaptureProvider::Cursor,
            Self::Windsurf => CaptureProvider::Windsurf,
            Self::Zed => CaptureProvider::Zed,
            Self::CopilotCli => CaptureProvider::CopilotCli,
            Self::FactoryAiDroid => CaptureProvider::FactoryAiDroid,
            Self::QwenCode => CaptureProvider::QwenCode,
            Self::KimiCodeCli => CaptureProvider::KimiCodeCli,
            Self::Auggie => CaptureProvider::Auggie,
            Self::Junie => CaptureProvider::Junie,
            Self::Firebender => CaptureProvider::Firebender,
            Self::ForgeCode => CaptureProvider::ForgeCode,
            Self::DeepAgents => CaptureProvider::DeepAgents,
            Self::MistralVibe => CaptureProvider::MistralVibe,
            Self::Mux => CaptureProvider::Mux,
            Self::RovoDev => CaptureProvider::RovoDev,
            Self::OpenClaw => CaptureProvider::OpenClaw,
            Self::Hermes => CaptureProvider::Hermes,
            Self::NanoClaw => CaptureProvider::NanoClaw,
            Self::AstrBot => CaptureProvider::AstrBot,
            Self::Shelley => CaptureProvider::Shelley,
            Self::Continue => CaptureProvider::Continue,
            Self::OpenHands => CaptureProvider::OpenHands,
            Self::Cline => CaptureProvider::Cline,
            Self::RooCode => CaptureProvider::RooCode,
            Self::Lingma => CaptureProvider::Lingma,
            Self::MiMoCode => CaptureProvider::MiMoCode,
            Self::Qoder => CaptureProvider::Qoder,
            Self::Warp => CaptureProvider::Warp,
            Self::CodeBuddy => CaptureProvider::CodeBuddy,
            Self::Trae => CaptureProvider::Trae,
        }
    }
}

impl ProviderArg {
    pub(crate) fn parse_name(value: &str) -> Option<Self> {
        Self::from_str(value, false).ok()
    }

    pub(crate) fn mcp_names() -> Vec<&'static str> {
        let mut names = Vec::new();
        for provider in Self::value_variants() {
            if !cli_supported_provider(provider.capture_provider()) {
                continue;
            }
            let cli_name = provider.cli_name();
            if !names.contains(&cli_name) {
                names.push(cli_name);
            }
            let storage_name = provider.capture_provider().as_str();
            if !names.contains(&storage_name) {
                names.push(storage_name);
            }
        }
        names.sort_unstable();
        names
    }

    pub(crate) fn capture_provider(self) -> CaptureProvider {
        match self {
            Self::Codex => CaptureProvider::Codex,
            Self::Pi => CaptureProvider::Pi,
            Self::Claude => CaptureProvider::Claude,
            Self::OpenCode => CaptureProvider::OpenCode,
            Self::Kilo => CaptureProvider::Kilo,
            Self::KiroCli => CaptureProvider::KiroCli,
            Self::Crush => CaptureProvider::Crush,
            Self::Goose => CaptureProvider::Goose,
            Self::Antigravity => CaptureProvider::Antigravity,
            Self::Gemini => CaptureProvider::Gemini,
            Self::Tabnine => CaptureProvider::Tabnine,
            Self::Cursor => CaptureProvider::Cursor,
            Self::Windsurf => CaptureProvider::Windsurf,
            Self::Zed => CaptureProvider::Zed,
            Self::CopilotCli => CaptureProvider::CopilotCli,
            Self::FactoryAiDroid => CaptureProvider::FactoryAiDroid,
            Self::QwenCode => CaptureProvider::QwenCode,
            Self::KimiCodeCli => CaptureProvider::KimiCodeCli,
            Self::Auggie => CaptureProvider::Auggie,
            Self::Junie => CaptureProvider::Junie,
            Self::Firebender => CaptureProvider::Firebender,
            Self::ForgeCode => CaptureProvider::ForgeCode,
            Self::DeepAgents => CaptureProvider::DeepAgents,
            Self::MistralVibe => CaptureProvider::MistralVibe,
            Self::Mux => CaptureProvider::Mux,
            Self::RovoDev => CaptureProvider::RovoDev,
            Self::OpenClaw => CaptureProvider::OpenClaw,
            Self::Hermes => CaptureProvider::Hermes,
            Self::NanoClaw => CaptureProvider::NanoClaw,
            Self::AstrBot => CaptureProvider::AstrBot,
            Self::Shelley => CaptureProvider::Shelley,
            Self::Continue => CaptureProvider::Continue,
            Self::OpenHands => CaptureProvider::OpenHands,
            Self::Cline => CaptureProvider::Cline,
            Self::RooCode => CaptureProvider::RooCode,
            Self::Lingma => CaptureProvider::Lingma,
            Self::MiMoCode => CaptureProvider::MiMoCode,
            Self::Qoder => CaptureProvider::Qoder,
            Self::Warp => CaptureProvider::Warp,
            Self::CodeBuddy => CaptureProvider::CodeBuddy,
            Self::Trae => CaptureProvider::Trae,
            Self::Custom => CaptureProvider::Custom,
        }
    }

    pub(crate) fn cli_name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Pi => "pi",
            Self::Claude => "claude",
            Self::OpenCode => "opencode",
            Self::Kilo => "kilo",
            Self::KiroCli => "kiro-cli",
            Self::Crush => "crush",
            Self::Goose => "goose",
            Self::Antigravity => "antigravity",
            Self::Gemini => "gemini",
            Self::Tabnine => "tabnine",
            Self::Cursor => "cursor",
            Self::Windsurf => "windsurf",
            Self::Zed => "zed",
            Self::CopilotCli => "copilot-cli",
            Self::FactoryAiDroid => "factory-ai-droid",
            Self::QwenCode => "qwen-code",
            Self::KimiCodeCli => "kimi-code-cli",
            Self::Auggie => "auggie",
            Self::Junie => "junie",
            Self::Firebender => "firebender",
            Self::ForgeCode => "forgecode",
            Self::DeepAgents => "deepagents",
            Self::MistralVibe => "mistral-vibe",
            Self::Mux => "mux",
            Self::RovoDev => "rovodev",
            Self::OpenClaw => "openclaw",
            Self::Hermes => "hermes",
            Self::NanoClaw => "nanoclaw",
            Self::AstrBot => "astrbot",
            Self::Shelley => "shelley",
            Self::Continue => "continue",
            Self::OpenHands => "openhands",
            Self::Cline => "cline",
            Self::RooCode => "roo",
            Self::Lingma => "lingma",
            Self::MiMoCode => "mimocode",
            Self::Qoder => "qoder",
            Self::Warp => "warp",
            Self::CodeBuddy => "codebuddy",
            Self::Trae => "trae",
            Self::Custom => "custom",
        }
    }
}

pub(crate) fn cli_supported_provider(provider: CaptureProvider) -> bool {
    matches!(
        provider,
        CaptureProvider::Codex
            | CaptureProvider::Claude
            | CaptureProvider::Pi
            | CaptureProvider::OpenCode
            | CaptureProvider::Kilo
            | CaptureProvider::KiroCli
            | CaptureProvider::Crush
            | CaptureProvider::Goose
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
            | CaptureProvider::NanoClaw
            | CaptureProvider::AstrBot
            | CaptureProvider::Shelley
            | CaptureProvider::Continue
            | CaptureProvider::OpenHands
            | CaptureProvider::Cline
            | CaptureProvider::RooCode
            | CaptureProvider::Lingma
            | CaptureProvider::MiMoCode
            | CaptureProvider::Qoder
            | CaptureProvider::Warp
            | CaptureProvider::CodeBuddy
            | CaptureProvider::Trae
            | CaptureProvider::Custom
    )
}
pub(crate) fn parse_native_provider_arg(
    value: &str,
) -> std::result::Result<NativeProviderArg, String> {
    let provider =
        NativeProviderArg::from_str(value, false).map_err(|_| compact_provider_error(value))?;
    if cli_supported_provider(provider.capture_provider()) {
        Ok(provider)
    } else {
        Err(compact_provider_error(value))
    }
}

pub(crate) fn parse_provider_arg(value: &str) -> std::result::Result<ProviderArg, String> {
    let provider =
        ProviderArg::from_str(value, false).map_err(|_| compact_provider_error(value))?;
    if cli_supported_provider(provider.capture_provider()) {
        Ok(provider)
    } else {
        Err(compact_provider_error(value))
    }
}

pub(crate) fn compact_provider_error(value: &str) -> String {
    format!(
        "unknown provider {value:?}; examples: codex, claude, cursor, pi, copilot-cli, opencode; run `ctx sources --all` to inspect every supported provider location"
    )
}
