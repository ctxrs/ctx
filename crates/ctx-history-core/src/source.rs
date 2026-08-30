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
        Xopc => "xopc",
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

impl CaptureProvider {
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
            Self::Xopc => "XOPC",
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
            (CaptureProvider::Xopc, "xopc", "XOPC"),
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
