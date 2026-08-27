use std::path::PathBuf;

use super::paths::PathContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkillAgentArg {
    Universal,
    Codex,
    GrokBuild,
    ClaudeCode,
    Cursor,
    OpenCode,
    MiMoCode,
    Amp,
    GeminiCli,
    Antigravity,
    AntigravityCli,
    GitHubCopilot,
    Pi,
    Goose,
}

impl SkillAgentArg {
    pub const ALL: &'static [Self] = &[
        Self::Universal,
        Self::Codex,
        Self::GrokBuild,
        Self::ClaudeCode,
        Self::Cursor,
        Self::OpenCode,
        Self::MiMoCode,
        Self::Amp,
        Self::GeminiCli,
        Self::Antigravity,
        Self::AntigravityCli,
        Self::GitHubCopilot,
        Self::Pi,
        Self::Goose,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Universal => "universal",
            Self::Codex => "codex",
            Self::GrokBuild => "grok-build",
            Self::ClaudeCode => "claude-code",
            Self::Cursor => "cursor",
            Self::OpenCode => "opencode",
            Self::MiMoCode => "mimocode",
            Self::Amp => "amp",
            Self::GeminiCli => "gemini-cli",
            Self::Antigravity => "antigravity",
            Self::AntigravityCli => "antigravity-cli",
            Self::GitHubCopilot => "github-copilot",
            Self::Pi => "pi",
            Self::Goose => "goose",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Universal => "Universal .agents",
            Self::Codex => "Codex",
            Self::GrokBuild => "Grok Build",
            Self::ClaudeCode => "Claude Code",
            Self::Cursor => "Cursor",
            Self::OpenCode => "OpenCode",
            Self::MiMoCode => "MiMo Code",
            Self::Amp => "Amp",
            Self::GeminiCli => "Gemini CLI",
            Self::Antigravity => "Antigravity",
            Self::AntigravityCli => "Antigravity CLI",
            Self::GitHubCopilot => "GitHub Copilot",
            Self::Pi => "Pi",
            Self::Goose => "Goose",
        }
    }

    pub fn project_skills_dir(self) -> &'static str {
        match self {
            Self::ClaudeCode => ".claude/skills",
            Self::GrokBuild => ".grok/skills",
            Self::Pi => ".pi/skills",
            Self::Goose => ".goose/skills",
            Self::Universal
            | Self::Codex
            | Self::Cursor
            | Self::OpenCode
            | Self::MiMoCode
            | Self::Amp
            | Self::GeminiCli
            | Self::Antigravity
            | Self::AntigravityCli
            | Self::GitHubCopilot => ".agents/skills",
        }
    }

    pub fn global_skills_dir(self, context: &PathContext) -> PathBuf {
        match self {
            Self::Universal => context.home.join(".agents").join("skills"),
            Self::Codex => context
                .env_or_home_child("CODEX_HOME", ".codex")
                .join("skills"),
            Self::GrokBuild => context
                .env_or_home_child("GROK_HOME", ".grok")
                .join("skills"),
            Self::ClaudeCode => context
                .env_or_home_child("CLAUDE_CONFIG_DIR", ".claude")
                .join("skills"),
            Self::Cursor => context.home.join(".cursor").join("skills"),
            Self::OpenCode => context.xdg_config_home.join("opencode").join("skills"),
            Self::MiMoCode => context.mimocode_config_dir().join("skills"),
            Self::Amp => context.xdg_config_home.join("agents").join("skills"),
            Self::GeminiCli => context.home.join(".gemini").join("skills"),
            Self::Antigravity => context
                .home
                .join(".gemini")
                .join("antigravity")
                .join("skills"),
            Self::AntigravityCli => context
                .home
                .join(".gemini")
                .join("antigravity-cli")
                .join("skills"),
            Self::GitHubCopilot => context.home.join(".copilot").join("skills"),
            Self::Pi => context.home.join(".pi").join("agent").join("skills"),
            Self::Goose => context.xdg_config_home.join("goose").join("skills"),
        }
    }

    pub fn global_skills_authority_root(self, context: &PathContext) -> PathBuf {
        match self {
            Self::Codex => context
                .env_overrides
                .get("CODEX_HOME")
                .cloned()
                .unwrap_or_else(|| context.home.clone()),
            Self::GrokBuild => context
                .env_overrides
                .get("GROK_HOME")
                .cloned()
                .unwrap_or_else(|| context.home.clone()),
            Self::ClaudeCode => context
                .env_overrides
                .get("CLAUDE_CONFIG_DIR")
                .cloned()
                .unwrap_or_else(|| context.home.clone()),
            Self::OpenCode | Self::Amp | Self::Goose => context.xdg_config_home.clone(),
            Self::MiMoCode => context
                .env_overrides
                .get("MIMOCODE_CONFIG_DIR")
                .or_else(|| context.env_overrides.get("MIMOCODE_HOME"))
                .cloned()
                .unwrap_or_else(|| context.xdg_config_home.clone()),
            Self::Universal
            | Self::Cursor
            | Self::GeminiCli
            | Self::Antigravity
            | Self::AntigravityCli
            | Self::GitHubCopilot
            | Self::Pi => context.home.clone(),
        }
    }

    pub fn needs_agent_specific_default(self) -> bool {
        self != Self::GrokBuild && self.project_skills_dir() != ".agents/skills"
    }

    pub fn detect_dir(self, context: &PathContext) -> Option<PathBuf> {
        match self {
            Self::Universal => Some(context.home.join(".agents")),
            Self::Codex => Some(context.env_or_home_child("CODEX_HOME", ".codex")),
            Self::GrokBuild => Some(context.env_or_home_child("GROK_HOME", ".grok")),
            Self::ClaudeCode => Some(context.env_or_home_child("CLAUDE_CONFIG_DIR", ".claude")),
            Self::Cursor => Some(context.home.join(".cursor")),
            Self::OpenCode => Some(context.xdg_config_home.join("opencode")),
            Self::MiMoCode => Some(context.mimocode_config_dir()),
            Self::Amp => Some(context.xdg_config_home.join("amp")),
            Self::GeminiCli => Some(context.home.join(".gemini")),
            Self::Antigravity => Some(context.home.join(".gemini").join("antigravity")),
            Self::AntigravityCli => Some(context.home.join(".gemini").join("antigravity-cli")),
            Self::GitHubCopilot => Some(context.home.join(".copilot")),
            Self::Pi => Some(context.home.join(".pi").join("agent")),
            Self::Goose => Some(context.xdg_config_home.join("goose")),
        }
    }
}

pub fn picker_agents() -> &'static [SkillAgentArg] {
    &[
        SkillAgentArg::Universal,
        SkillAgentArg::ClaudeCode,
        SkillAgentArg::Codex,
        SkillAgentArg::GrokBuild,
        SkillAgentArg::Cursor,
        SkillAgentArg::OpenCode,
        SkillAgentArg::MiMoCode,
        SkillAgentArg::GeminiCli,
        SkillAgentArg::Antigravity,
        SkillAgentArg::AntigravityCli,
        SkillAgentArg::GitHubCopilot,
        SkillAgentArg::Pi,
        SkillAgentArg::Goose,
        SkillAgentArg::Amp,
    ]
}

pub fn agent_from_name(value: &str) -> Option<SkillAgentArg> {
    match value.to_ascii_lowercase().as_str() {
        "universal" | "agents" | ".agents" => Some(SkillAgentArg::Universal),
        "codex" => Some(SkillAgentArg::Codex),
        "grok-build" | "grok" => Some(SkillAgentArg::GrokBuild),
        "claude" | "claude-code" | "claudecode" => Some(SkillAgentArg::ClaudeCode),
        "cursor" => Some(SkillAgentArg::Cursor),
        "opencode" | "open-code" => Some(SkillAgentArg::OpenCode),
        "mimocode" | "mimo-code" | "mimo_code" => Some(SkillAgentArg::MiMoCode),
        "amp" => Some(SkillAgentArg::Amp),
        "gemini" | "gemini-cli" => Some(SkillAgentArg::GeminiCli),
        "antigravity" => Some(SkillAgentArg::Antigravity),
        "antigravity-cli" => Some(SkillAgentArg::AntigravityCli),
        "github-copilot" | "copilot" => Some(SkillAgentArg::GitHubCopilot),
        "pi" => Some(SkillAgentArg::Pi),
        "goose" => Some(SkillAgentArg::Goose),
        _ => None,
    }
}

pub fn parse_skill_agent(value: &str) -> Result<SkillAgentArg, String> {
    agent_from_name(value).ok_or_else(|| format!("unknown skill agent: {value}"))
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::*;

    #[test]
    fn grok_build_uses_canonical_id_and_only_documented_alias() {
        assert_eq!(
            parse_skill_agent("grok-build"),
            Ok(SkillAgentArg::GrokBuild)
        );
        assert_eq!(parse_skill_agent("grok"), Ok(SkillAgentArg::GrokBuild));
        assert!(parse_skill_agent("grokbuild").is_err());
        assert!(parse_skill_agent("grok_build").is_err());
        assert_eq!(SkillAgentArg::GrokBuild.id(), "grok-build");
        assert_eq!(SkillAgentArg::GrokBuild.display_name(), "Grok Build");
    }

    #[test]
    fn grok_build_uses_native_global_and_project_skill_dirs() {
        let context = PathContext::for_tests(PathBuf::from("/home/tester"), PathBuf::from("/repo"));
        assert_eq!(
            SkillAgentArg::GrokBuild.global_skills_dir(&context),
            PathBuf::from("/home/tester/.grok/skills")
        );
        assert_eq!(
            SkillAgentArg::GrokBuild.project_skills_dir(),
            ".grok/skills"
        );

        let override_context = context.with_env_override("GROK_HOME", PathBuf::from("/grok-home"));
        assert_eq!(
            SkillAgentArg::GrokBuild.global_skills_dir(&override_context),
            PathBuf::from("/grok-home/skills")
        );
    }

    #[test]
    fn grok_build_detection_accepts_env_override_or_default_home() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let cwd = temp.path().join("repo");
        let env_context = PathContext::for_tests(home.clone(), cwd.clone())
            .with_env_override("GROK_HOME", temp.path().join("grok-home"));
        assert!(env_context.agent_detected(SkillAgentArg::GrokBuild));

        fs::create_dir_all(home.join(".grok")).unwrap();
        let home_context = PathContext::for_tests(home, cwd);
        assert!(home_context.agent_detected(SkillAgentArg::GrokBuild));
    }

    #[test]
    fn grok_build_reads_universal_skills_without_an_automatic_native_copy() {
        assert!(!SkillAgentArg::GrokBuild.needs_agent_specific_default());
    }
}
