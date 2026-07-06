use super::*;

#[derive(Debug, Args)]
pub(crate) struct SkillArgs {
    #[command(subcommand)]
    pub(super) command: SkillCommand,
}

#[derive(Debug, Subcommand)]
pub(super) enum SkillCommand {
    #[command(about = "Install or refresh the bundled ctx agent-history skill")]
    Install(SkillInstallArgs),
    #[command(about = "Check whether the bundled ctx agent-history skill is installed")]
    Status(SkillStatusArgs),
}

#[derive(Debug, Args)]
pub(super) struct SkillInstallArgs {
    #[arg(long = "agent", value_enum, conflicts_with = "all_agents")]
    pub(super) agent: Vec<SkillAgentArg>,
    #[arg(long, conflicts_with = "agent")]
    pub(super) all_agents: bool,
    #[arg(
        long,
        help = "Install into the current project instead of global agent dirs"
    )]
    pub(super) project: bool,
    #[arg(long)]
    pub(super) json: bool,
    #[arg(long, help = "Overwrite locally modified bundled skill files")]
    pub(super) force: bool,
}

#[derive(Debug, Args)]
pub(super) struct SkillStatusArgs {
    #[arg(long = "agent", value_enum, conflicts_with = "all_agents")]
    pub(super) agent: Vec<SkillAgentArg>,
    #[arg(long, conflicts_with = "agent")]
    pub(super) all_agents: bool,
    #[arg(
        long,
        help = "Check the current project's skill dirs instead of global dirs"
    )]
    pub(super) project: bool,
    #[arg(long)]
    pub(super) json: bool,
}

impl SkillArgs {
    pub(crate) fn json_output(&self) -> bool {
        match &self.command {
            SkillCommand::Install(args) => args.json,
            SkillCommand::Status(args) => args.json,
        }
    }

    pub(crate) fn add_initial_analytics(&self, properties: &mut AnalyticsProperties) {
        analytics::insert_str(properties, "skill_name", BUNDLED_SKILL_NAME);
        match &self.command {
            SkillCommand::Install(args) => {
                analytics::insert_str(properties, "skill_action", "install");
                insert_target_analytics(properties, &args.agent, args.all_agents, args.project);
            }
            SkillCommand::Status(args) => {
                analytics::insert_str(properties, "skill_action", "status");
                insert_target_analytics(properties, &args.agent, args.all_agents, args.project);
            }
        }
    }
}

fn insert_target_analytics(
    properties: &mut AnalyticsProperties,
    agents: &[SkillAgentArg],
    all_agents: bool,
    project: bool,
) {
    analytics::insert_str(
        properties,
        "skill_scope",
        if project { "project" } else { "global" },
    );
    analytics::insert_str(
        properties,
        "target_agent_group",
        if all_agents {
            "all"
        } else if agents.is_empty() {
            "default"
        } else {
            "explicit"
        },
    );
    let count = if all_agents {
        SkillAgentArg::ALL.len()
    } else {
        agents.len().max(1)
    };
    analytics::insert_count_bucket(properties, "target_agents_count_bucket", count as u64);
}

pub(crate) fn run(args: SkillArgs, analytics_properties: &mut AnalyticsProperties) -> Result<()> {
    let context = PathContext::from_env()?;
    match args.command {
        SkillCommand::Install(args) => run_install(args, &context, analytics_properties),
        SkillCommand::Status(args) => run_status(args, &context, analytics_properties),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub(super) enum SkillAgentArg {
    #[value(name = "universal", alias = "agents")]
    Universal,
    Codex,
    #[value(name = "claude-code", alias = "claude")]
    ClaudeCode,
    Cursor,
    #[value(name = "opencode", alias = "open-code")]
    OpenCode,
    Amp,
    #[value(name = "gemini-cli", alias = "gemini")]
    GeminiCli,
    Antigravity,
    #[value(name = "antigravity-cli")]
    AntigravityCli,
    #[value(name = "github-copilot", alias = "copilot")]
    GitHubCopilot,
    Pi,
    Goose,
}

impl SkillAgentArg {
    pub(super) const ALL: &'static [Self] = &[
        Self::Universal,
        Self::Codex,
        Self::ClaudeCode,
        Self::Cursor,
        Self::OpenCode,
        Self::Amp,
        Self::GeminiCli,
        Self::Antigravity,
        Self::AntigravityCli,
        Self::GitHubCopilot,
        Self::Pi,
        Self::Goose,
    ];

    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Universal => "universal",
            Self::Codex => "codex",
            Self::ClaudeCode => "claude-code",
            Self::Cursor => "cursor",
            Self::OpenCode => "opencode",
            Self::Amp => "amp",
            Self::GeminiCli => "gemini-cli",
            Self::Antigravity => "antigravity",
            Self::AntigravityCli => "antigravity-cli",
            Self::GitHubCopilot => "github-copilot",
            Self::Pi => "pi",
            Self::Goose => "goose",
        }
    }

    pub(super) fn display_name(self) -> &'static str {
        match self {
            Self::Universal => "Universal .agents",
            Self::Codex => "Codex",
            Self::ClaudeCode => "Claude Code",
            Self::Cursor => "Cursor",
            Self::OpenCode => "OpenCode",
            Self::Amp => "Amp",
            Self::GeminiCli => "Gemini CLI",
            Self::Antigravity => "Antigravity",
            Self::AntigravityCli => "Antigravity CLI",
            Self::GitHubCopilot => "GitHub Copilot",
            Self::Pi => "Pi",
            Self::Goose => "Goose",
        }
    }

    pub(super) fn project_skills_dir(self) -> &'static str {
        match self {
            Self::ClaudeCode => ".claude/skills",
            Self::Pi => ".pi/skills",
            Self::Goose => ".goose/skills",
            Self::Universal
            | Self::Codex
            | Self::Cursor
            | Self::OpenCode
            | Self::Amp
            | Self::GeminiCli
            | Self::Antigravity
            | Self::AntigravityCli
            | Self::GitHubCopilot => ".agents/skills",
        }
    }

    pub(super) fn global_skills_dir(self, context: &PathContext) -> PathBuf {
        match self {
            Self::Universal => context.home.join(".agents").join("skills"),
            Self::Codex => context
                .env_or_home_child("CODEX_HOME", ".codex")
                .join("skills"),
            Self::ClaudeCode => context
                .env_or_home_child("CLAUDE_CONFIG_DIR", ".claude")
                .join("skills"),
            Self::Cursor => context.home.join(".cursor").join("skills"),
            Self::OpenCode => context.xdg_config_home.join("opencode").join("skills"),
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

    pub(super) fn needs_agent_specific_default(self) -> bool {
        self.project_skills_dir() != ".agents/skills"
    }

    pub(super) fn detect_dir(self, context: &PathContext) -> Option<PathBuf> {
        match self {
            Self::Universal => Some(context.home.join(".agents")),
            Self::Codex => Some(context.env_or_home_child("CODEX_HOME", ".codex")),
            Self::ClaudeCode => Some(context.env_or_home_child("CLAUDE_CONFIG_DIR", ".claude")),
            Self::Cursor => Some(context.home.join(".cursor")),
            Self::OpenCode => Some(context.xdg_config_home.join("opencode")),
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
