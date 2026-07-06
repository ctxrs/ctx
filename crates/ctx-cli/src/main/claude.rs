#[allow(unused_imports)]
use super::*;

pub(crate) const DEFAULT_VISIBLE_SOURCE_PROVIDERS: &[CaptureProvider] = &[
    CaptureProvider::Claude,
    CaptureProvider::Codex,
    CaptureProvider::Cursor,
    CaptureProvider::Pi,
    CaptureProvider::CopilotCli,
    CaptureProvider::OpenCode,
];

#[derive(Debug, Args, Clone)]
pub(crate) struct SourcesArgs {
    #[arg(long)]
    pub(crate) json: bool,
    #[arg(
        long,
        value_parser = parse_provider_arg,
        hide_possible_values = true,
        help = "Show sources for one provider, for example codex, claude, cursor, pi, copilot-cli, or opencode"
    )]
    pub(crate) provider: Option<ProviderArg>,
    #[arg(long, help = "Show every supported provider location")]
    pub(crate) all: bool,
    #[arg(long, help = "Show missing locations for every known provider")]
    pub(crate) show_missing: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ImportArgs {
    #[arg(
        long,
        value_parser = parse_native_provider_arg,
        hide_possible_values = true,
        help = "Import one provider, for example codex, claude, cursor, pi, copilot-cli, or opencode"
    )]
    pub(crate) provider: Option<NativeProviderArg>,
    #[arg(
        long,
        help = "Import exactly this path; native provider paths require --provider"
    )]
    pub(crate) path: Option<PathBuf>,
    #[arg(long = "history-source", conflicts_with_all = ["provider", "path", "format", "all"])]
    pub(crate) history_source: Option<String>,
    #[arg(
        long = "history-source-manifest",
        conflicts_with_all = ["provider", "path", "format"]
    )]
    pub(crate) history_source_manifest: Vec<PathBuf>,
    #[arg(long = "reset-cursor")]
    pub(crate) reset_cursor: bool,
    #[arg(
        long,
        value_enum,
        requires = "path",
        conflicts_with_all = ["provider", "all", "history_source"]
    )]
    pub(crate) format: Option<ImportFormatArg>,
    #[arg(long, conflicts_with_all = ["provider", "path", "format", "history_source"])]
    pub(crate) all: bool,
    #[arg(long)]
    pub(crate) resume: bool,
    #[arg(long)]
    pub(crate) json: bool,
    #[arg(long, value_enum, default_value_t = ProgressArg::Auto)]
    pub(crate) progress: ProgressArg,
}

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
    Qoder,
    Warp,
    #[value(name = "codebuddy", alias = "code-buddy", alias = "code_buddy")]
    CodeBuddy,
    #[value(alias = "trae-cn", alias = "trae_cn")]
    Trae,
    Custom,
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
            Self::Qoder => CaptureProvider::Qoder,
            Self::Warp => CaptureProvider::Warp,
            Self::CodeBuddy => CaptureProvider::CodeBuddy,
            Self::Trae => CaptureProvider::Trae,
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
            | CaptureProvider::Qoder
            | CaptureProvider::Warp
            | CaptureProvider::CodeBuddy
            | CaptureProvider::Trae
            | CaptureProvider::Custom
    )
}

pub(crate) fn compact_provider_error(value: &str) -> String {
    format!(
        "unknown provider {value:?}; examples: codex, claude, cursor, pi, copilot-cli, opencode; run `ctx sources --all` to inspect every supported provider location"
    )
}

pub(crate) fn source_uses_incremental_event_search(source: &SourceInfo) -> bool {
    matches!(
        source.provider,
        CaptureProvider::Codex
            | CaptureProvider::Claude
            | CaptureProvider::Pi
            | CaptureProvider::Cursor
            | CaptureProvider::OpenCode
            | CaptureProvider::Kilo
            | CaptureProvider::KiroCli
            | CaptureProvider::Crush
            | CaptureProvider::Goose
            | CaptureProvider::Warp
            | CaptureProvider::Antigravity
            | CaptureProvider::Gemini
            | CaptureProvider::Tabnine
            | CaptureProvider::Windsurf
            | CaptureProvider::Qoder
            | CaptureProvider::CopilotCli
            | CaptureProvider::FactoryAiDroid
            | CaptureProvider::Continue
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
            | CaptureProvider::Cline
            | CaptureProvider::RooCode
            | CaptureProvider::CodeBuddy
            | CaptureProvider::Trae
    )
}
