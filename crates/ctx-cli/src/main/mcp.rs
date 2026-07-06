#[allow(unused_imports)]
use super::*;

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
            Self::Qoder => "qoder",
            Self::Warp => "warp",
            Self::CodeBuddy => "codebuddy",
            Self::Trae => "trae",
            Self::Custom => "custom",
        }
    }
}
