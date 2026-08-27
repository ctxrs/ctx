use ctx_history_core::CaptureProvider;

use crate::HistoryProvider;

/// Canonical public provider vocabulary. Final transports may expose this
/// through their own parser shells, but must not duplicate spelling policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCliSpec {
    pub provider: CaptureProvider,
    pub cli_name: &'static str,
    pub aliases: &'static [&'static str],
}

const NATIVE_PROVIDER_CLI_SPECS: &[ProviderCliSpec] = &[
    ProviderCliSpec {
        provider: CaptureProvider::Codex,
        cli_name: "codex",
        aliases: &[],
    },
    ProviderCliSpec {
        provider: CaptureProvider::GrokBuild,
        cli_name: "grok-build",
        aliases: &["grok"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::DeepSeekHarness,
        cli_name: "deepseek-harness",
        aliases: &["dsh", "deepseek_harness"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::Pi,
        cli_name: "pi",
        aliases: &[],
    },
    ProviderCliSpec {
        provider: CaptureProvider::Claude,
        cli_name: "claude",
        aliases: &["claude-code"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::OpenCode,
        cli_name: "opencode",
        aliases: &["open-code"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::Kilo,
        cli_name: "kilo",
        aliases: &["kilo-code", "kilo_code", "kilocode"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::KiroCli,
        cli_name: "kiro-cli",
        aliases: &["kiro", "kiro_cli"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::Crush,
        cli_name: "crush",
        aliases: &[],
    },
    ProviderCliSpec {
        provider: CaptureProvider::Goose,
        cli_name: "goose",
        aliases: &[],
    },
    ProviderCliSpec {
        provider: CaptureProvider::Antigravity,
        cli_name: "antigravity",
        aliases: &["antigravity-cli"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::Gemini,
        cli_name: "gemini",
        aliases: &["gemini-cli"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::Tabnine,
        cli_name: "tabnine",
        aliases: &["tabnine-cli"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::Cursor,
        cli_name: "cursor",
        aliases: &[],
    },
    ProviderCliSpec {
        provider: CaptureProvider::Zed,
        cli_name: "zed",
        aliases: &[],
    },
    ProviderCliSpec {
        provider: CaptureProvider::CopilotCli,
        cli_name: "copilot-cli",
        aliases: &["copilot", "copilot_cli", "github-copilot"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::FactoryAiDroid,
        cli_name: "factory-ai-droid",
        aliases: &[
            "factoryai-droid",
            "factory-droid",
            "factory_ai_droid",
            "droid",
        ],
    },
    ProviderCliSpec {
        provider: CaptureProvider::QwenCode,
        cli_name: "qwen-code",
        aliases: &["qwen", "qwen_code"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::KimiCodeCli,
        cli_name: "kimi-code-cli",
        aliases: &["kimi", "kimi_code_cli"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::Auggie,
        cli_name: "auggie",
        aliases: &["augment", "augment-code"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::Junie,
        cli_name: "junie",
        aliases: &[],
    },
    ProviderCliSpec {
        provider: CaptureProvider::Firebender,
        cli_name: "firebender",
        aliases: &["firebender-jetbrains", "firebender_jetbrains"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::ForgeCode,
        cli_name: "forgecode",
        aliases: &["forge", "forge-code", "forge_code"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::DeepAgents,
        cli_name: "deepagents",
        aliases: &["deep-agents", "dcode"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::MistralVibe,
        cli_name: "mistral-vibe",
        aliases: &["mistral", "mistral_vibe"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::Mux,
        cli_name: "mux",
        aliases: &[],
    },
    ProviderCliSpec {
        provider: CaptureProvider::RovoDev,
        cli_name: "rovodev",
        aliases: &["rovo-dev", "rovo_dev"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::OpenClaw,
        cli_name: "openclaw",
        aliases: &["open-claw", "open_claw"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::Hermes,
        cli_name: "hermes",
        aliases: &[],
    },
    ProviderCliSpec {
        provider: CaptureProvider::NanoClaw,
        cli_name: "nanoclaw",
        aliases: &["nano-claw", "nano_claw"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::AstrBot,
        cli_name: "astrbot",
        aliases: &["astr-bot", "astr_bot"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::Shelley,
        cli_name: "shelley",
        aliases: &[],
    },
    ProviderCliSpec {
        provider: CaptureProvider::Continue,
        cli_name: "continue",
        aliases: &["continue-cli"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::OpenHands,
        cli_name: "openhands",
        aliases: &["open-hands", "open_hands"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::Cline,
        cli_name: "cline",
        aliases: &[],
    },
    ProviderCliSpec {
        provider: CaptureProvider::RooCode,
        cli_name: "roo",
        aliases: &["roo-code", "roo_code"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::Lingma,
        cli_name: "lingma",
        aliases: &["qoder-cn", "qoder_cn"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::MiMoCode,
        cli_name: "mimocode",
        aliases: &["mimo-code", "mimo_code"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::Qoder,
        cli_name: "qoder",
        aliases: &[],
    },
    ProviderCliSpec {
        provider: CaptureProvider::Warp,
        cli_name: "warp",
        aliases: &[],
    },
    ProviderCliSpec {
        provider: CaptureProvider::CodeBuddy,
        cli_name: "codebuddy",
        aliases: &["code-buddy", "code_buddy"],
    },
    ProviderCliSpec {
        provider: CaptureProvider::Fx,
        cli_name: "fx",
        aliases: &[],
    },
];

const CUSTOM_PROVIDER_CLI_SPEC: ProviderCliSpec = ProviderCliSpec {
    provider: CaptureProvider::Custom,
    cli_name: "custom",
    aliases: &[],
};

pub fn native_provider_cli_specs() -> &'static [ProviderCliSpec] {
    NATIVE_PROVIDER_CLI_SPECS
}

pub fn provider_cli_specs() -> impl Iterator<Item = ProviderCliSpec> {
    NATIVE_PROVIDER_CLI_SPECS
        .iter()
        .copied()
        .chain(std::iter::once(CUSTOM_PROVIDER_CLI_SPEC))
}

pub fn provider_cli_spec(provider: CaptureProvider) -> Option<ProviderCliSpec> {
    provider_cli_specs().find(|spec| spec.provider == provider)
}

pub fn cli_supported_provider(provider: CaptureProvider) -> bool {
    provider_cli_spec(provider).is_some()
}

pub fn provider_is_importable(provider: CaptureProvider) -> bool {
    ctx_history_capture::provider_source_specs()
        .iter()
        .find(|spec| spec.provider == provider)
        .is_some_and(|spec| spec.import_support.is_importable())
}

pub fn provider_cli_name(provider: CaptureProvider) -> &'static str {
    provider_cli_spec(provider).map_or_else(|| provider.as_str(), |spec| spec.cli_name)
}

/// Resolves canonical CLI names, persisted storage identifiers, and compatibility aliases.
pub fn parse_capture_provider_name(value: &str) -> Option<CaptureProvider> {
    provider_cli_specs()
        .find(|spec| {
            spec.cli_name == value
                || spec.provider.as_str() == value
                || spec.aliases.contains(&value)
        })
        .map(|spec| spec.provider)
}

pub fn parse_provider_name(value: &str) -> Option<HistoryProvider> {
    parse_capture_provider_name(value).map(HistoryProvider::from)
}

pub fn parse_native_provider_name(value: &str) -> Option<CaptureProvider> {
    parse_capture_provider_name(value).filter(|provider| *provider != CaptureProvider::Custom)
}

pub fn parse_provider(value: &str) -> std::result::Result<HistoryProvider, String> {
    parse_provider_name(value).ok_or_else(|| compact_provider_error(value))
}

pub fn parse_native_provider(value: &str) -> std::result::Result<CaptureProvider, String> {
    parse_native_provider_name(value).ok_or_else(|| compact_provider_error(value))
}

pub fn mcp_provider_names() -> Vec<&'static str> {
    let mut names = Vec::new();
    for spec in provider_cli_specs() {
        names.push(spec.cli_name);
        if spec.provider.as_str() != spec.cli_name {
            names.push(spec.provider.as_str());
        }
    }
    names.sort_unstable();
    names.dedup();
    names
}

pub fn compact_provider_error(value: &str) -> String {
    format!(
        "unknown provider {value:?}; examples: codex, claude, cursor, pi, copilot-cli, opencode; run `ctx sources --all` to inspect every supported provider location"
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderArg(pub HistoryProvider);

impl ProviderArg {
    pub fn capture_provider(self) -> ctx_history_core::CaptureProvider {
        self.0.capture_provider()
    }
}

#[cfg(test)]
#[path = "provider_args/tests.rs"]
mod tests;
