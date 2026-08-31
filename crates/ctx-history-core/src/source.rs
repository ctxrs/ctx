use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::CoreError;

text_enum! {
    pub enum CaptureProvider {
        Codex => "codex",
        GrokBuild => "grok_build",
        DeepSeekHarness => "deepseek_harness",
        Claude => "claude",
        Pi => "pi",
        OpenCode => "opencode",
        Kilo => "kilo",
        KiroCli => "kiro_cli",
        Antigravity => "antigravity",
        Gemini => "gemini",
        Tabnine => "tabnine",
        Cursor => "cursor",
        Zed => "zed",
        CopilotCli => "copilot_cli",
        FactoryAiDroid => "factory_ai_droid",
        QwenCode => "qwen_code",
        KimiCodeCli => "kimi_code_cli",
        Auggie => "auggie",
        Junie => "junie",
        Firebender => "firebender",
        ForgeCode => "forgecode",
        DeepAgents => "deepagents",
        MistralVibe => "mistral_vibe",
        Mux => "mux",
        RovoDev => "rovodev",
        OpenClaw => "openclaw",
        Hermes => "hermes",
        NanoClaw => "nanoclaw",
        AstrBot => "astrbot",
        Shelley => "shelley",
        Continue => "continue",
        OpenHands => "openhands",
        Cline => "cline",
        RooCode => "roo_code",
        Crush => "crush",
        Goose => "goose",
        Lingma => "lingma",
        Qoder => "qoder",
        Warp => "warp",
        CodeBuddy => "codebuddy",
        Fx => "fx",
        Shell => "shell",
        Git => "git",
        Jj => "jj",
        Gh => "gh",
        Custom => "custom",
        Unknown => "unknown",
        MiMoCode => "mimocode",
    }
    default Unknown
}

/// Canonical public provider vocabulary shared by persisted configuration and
/// final command transports.
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

impl CaptureProvider {
    /// Resolves the stable provider vocabulary accepted by persisted application
    /// configuration, including released command aliases.
    pub fn parse_config_name(value: &str) -> Option<Self> {
        provider_cli_specs()
            .find(|spec| {
                spec.cli_name == value
                    || spec.provider.as_str() == value
                    || spec.aliases.contains(&value)
            })
            .map(|spec| spec.provider)
    }

    /// Human-readable product name for ordinary history presentation.
    ///
    /// Machine identities, selectors, serialization, and protocol surfaces
    /// continue to use [`Self::as_str`].
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::GrokBuild => "Grok Build",
            Self::DeepSeekHarness => "DeepSeek Harness",
            Self::Claude => "Claude Code",
            Self::Pi => "Pi",
            Self::OpenCode => "OpenCode",
            Self::Kilo => "Kilo Code",
            Self::KiroCli => "Kiro",
            Self::Antigravity => "Antigravity",
            Self::Gemini => "Gemini",
            Self::Tabnine => "Tabnine",
            Self::Cursor => "Cursor",
            Self::Zed => "Zed",
            Self::CopilotCli => "GitHub Copilot",
            Self::FactoryAiDroid => "Factory AI Droid",
            Self::QwenCode => "Qwen Code",
            Self::KimiCodeCli => "Kimi Code",
            Self::Auggie => "Auggie",
            Self::Junie => "Junie",
            Self::Firebender => "Firebender",
            Self::ForgeCode => "ForgeCode",
            Self::DeepAgents => "Deep Agents",
            Self::MistralVibe => "Mistral Vibe",
            Self::Mux => "Mux",
            Self::RovoDev => "Rovo Dev",
            Self::OpenClaw => "OpenClaw",
            Self::Hermes => "Hermes Agent",
            Self::NanoClaw => "NanoClaw",
            Self::AstrBot => "AstrBot",
            Self::Shelley => "Shelley",
            Self::Continue => "Continue",
            Self::OpenHands => "OpenHands",
            Self::Cline => "Cline",
            Self::RooCode => "Roo Code",
            Self::Crush => "Crush",
            Self::Goose => "Goose",
            Self::Lingma => "Lingma",
            Self::Qoder => "Qoder",
            Self::Warp => "Warp",
            Self::CodeBuddy => "CodeBuddy",
            Self::Fx => "fx",
            Self::Shell => "Shell",
            Self::Git => "Git",
            Self::Jj => "Jujutsu",
            Self::Gh => "GitHub CLI",
            Self::Custom => "Custom",
            Self::Unknown => "Unknown",
            Self::MiMoCode => "MiMo Code",
        }
    }
}

/// Resolves the stable provider vocabulary accepted by persisted application
/// configuration. Kept as a free function for direct parser consumers.
pub fn parse_capture_provider_name(value: &str) -> Option<CaptureProvider> {
    CaptureProvider::parse_config_name(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_names_are_exhaustive_and_machine_contracts_stay_stable() {
        let cases = [
            (CaptureProvider::Codex, "codex", "Codex"),
            (CaptureProvider::GrokBuild, "grok_build", "Grok Build"),
            (
                CaptureProvider::DeepSeekHarness,
                "deepseek_harness",
                "DeepSeek Harness",
            ),
            (CaptureProvider::Claude, "claude", "Claude Code"),
            (CaptureProvider::Pi, "pi", "Pi"),
            (CaptureProvider::OpenCode, "opencode", "OpenCode"),
            (CaptureProvider::Kilo, "kilo", "Kilo Code"),
            (CaptureProvider::KiroCli, "kiro_cli", "Kiro"),
            (CaptureProvider::Antigravity, "antigravity", "Antigravity"),
            (CaptureProvider::Gemini, "gemini", "Gemini"),
            (CaptureProvider::Tabnine, "tabnine", "Tabnine"),
            (CaptureProvider::Cursor, "cursor", "Cursor"),
            (CaptureProvider::Zed, "zed", "Zed"),
            (CaptureProvider::CopilotCli, "copilot_cli", "GitHub Copilot"),
            (
                CaptureProvider::FactoryAiDroid,
                "factory_ai_droid",
                "Factory AI Droid",
            ),
            (CaptureProvider::QwenCode, "qwen_code", "Qwen Code"),
            (CaptureProvider::KimiCodeCli, "kimi_code_cli", "Kimi Code"),
            (CaptureProvider::Auggie, "auggie", "Auggie"),
            (CaptureProvider::Junie, "junie", "Junie"),
            (CaptureProvider::Firebender, "firebender", "Firebender"),
            (CaptureProvider::ForgeCode, "forgecode", "ForgeCode"),
            (CaptureProvider::DeepAgents, "deepagents", "Deep Agents"),
            (CaptureProvider::MistralVibe, "mistral_vibe", "Mistral Vibe"),
            (CaptureProvider::Mux, "mux", "Mux"),
            (CaptureProvider::RovoDev, "rovodev", "Rovo Dev"),
            (CaptureProvider::OpenClaw, "openclaw", "OpenClaw"),
            (CaptureProvider::Hermes, "hermes", "Hermes Agent"),
            (CaptureProvider::NanoClaw, "nanoclaw", "NanoClaw"),
            (CaptureProvider::AstrBot, "astrbot", "AstrBot"),
            (CaptureProvider::Shelley, "shelley", "Shelley"),
            (CaptureProvider::Continue, "continue", "Continue"),
            (CaptureProvider::OpenHands, "openhands", "OpenHands"),
            (CaptureProvider::Cline, "cline", "Cline"),
            (CaptureProvider::RooCode, "roo_code", "Roo Code"),
            (CaptureProvider::Crush, "crush", "Crush"),
            (CaptureProvider::Goose, "goose", "Goose"),
            (CaptureProvider::Lingma, "lingma", "Lingma"),
            (CaptureProvider::Qoder, "qoder", "Qoder"),
            (CaptureProvider::Warp, "warp", "Warp"),
            (CaptureProvider::CodeBuddy, "codebuddy", "CodeBuddy"),
            (CaptureProvider::Fx, "fx", "fx"),
            (CaptureProvider::Shell, "shell", "Shell"),
            (CaptureProvider::Git, "git", "Git"),
            (CaptureProvider::Jj, "jj", "Jujutsu"),
            (CaptureProvider::Gh, "gh", "GitHub CLI"),
            (CaptureProvider::Custom, "custom", "Custom"),
            (CaptureProvider::Unknown, "unknown", "Unknown"),
            (CaptureProvider::MiMoCode, "mimocode", "MiMo Code"),
        ];

        assert_eq!(cases.len(), CaptureProvider::variants().len());
        for (provider, machine_name, display_name) in cases {
            assert_eq!(provider.as_str(), machine_name);
            assert_eq!(provider.display_name(), display_name);
            assert_eq!(provider.to_string(), machine_name);
            assert_eq!(
                serde_json::to_string(&provider).unwrap(),
                format!("\"{machine_name}\"")
            );
            assert_eq!(
                serde_json::from_str::<CaptureProvider>(&format!("\"{machine_name}\"")).unwrap(),
                provider
            );
        }
    }
}
