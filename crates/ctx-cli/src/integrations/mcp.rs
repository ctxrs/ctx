use std::{
    collections::BTreeMap,
    env, fs, io,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use clap::{Args, ValueEnum};
use serde_json::{json, Map, Value};
use toml_edit::{
    value as toml_value, Array as TomlArray, DocumentMut, Item, Table, Value as TomlValue,
};

use crate::{analytics, AnalyticsProperties};

const SERVER_NAME: &str = "ctx";
const SERVER_COMMAND: &str = "ctx";
const SERVER_ARGS: &[&str] = &["mcp", "serve"];

#[derive(Debug, Args)]
pub(crate) struct McpInstallArgs {
    #[arg(
        long = "agent",
        alias = "provider",
        value_enum,
        conflicts_with = "all_agents",
        help = "Install for one coding-agent client; --provider is accepted as an alias"
    )]
    pub(crate) agent: Vec<McpAgentArg>,
    #[arg(long, conflicts_with = "agent")]
    pub(crate) all_agents: bool,
    #[arg(
        long,
        help = "Install into the current project's MCP config when supported"
    )]
    pub(crate) project: bool,
    #[arg(long)]
    pub(crate) json: bool,
    #[arg(
        long,
        help = "Overwrite an existing ctx MCP server entry with different command or args"
    )]
    pub(crate) force: bool,
}

#[derive(Debug, Args)]
pub(crate) struct McpStatusArgs {
    #[arg(
        long = "agent",
        alias = "provider",
        value_enum,
        conflicts_with = "all_agents",
        help = "Inspect one coding-agent client; --provider is accepted as an alias"
    )]
    pub(crate) agent: Vec<McpAgentArg>,
    #[arg(long, conflicts_with = "agent")]
    pub(crate) all_agents: bool,
    #[arg(long, help = "Inspect the current project's MCP config when supported")]
    pub(crate) project: bool,
    #[arg(long)]
    pub(crate) json: bool,
}

impl McpInstallArgs {
    pub(crate) fn add_initial_analytics(&self, properties: &mut AnalyticsProperties) {
        insert_target_analytics(properties, &self.agent, self.all_agents, self.project);
        analytics::insert_bool(properties, "force", self.force);
    }
}

impl McpStatusArgs {
    pub(crate) fn add_initial_analytics(&self, properties: &mut AnalyticsProperties) {
        insert_target_analytics(properties, &self.agent, self.all_agents, self.project);
    }
}

fn insert_target_analytics(
    properties: &mut AnalyticsProperties,
    agents: &[McpAgentArg],
    all_agents: bool,
    project: bool,
) {
    analytics::insert_str(properties, "integration_name", "mcp");
    analytics::insert_str(
        properties,
        "integration_scope",
        if project { "project" } else { "global" },
    );
    analytics::insert_str(
        properties,
        "target_agent_group",
        if all_agents {
            "all"
        } else if agents.is_empty() {
            "detected"
        } else {
            "explicit"
        },
    );
    let count = if all_agents && project {
        McpAgentArg::PROJECT_CAPABLE.len()
    } else if all_agents {
        McpAgentArg::ALL.len()
    } else {
        agents.len()
    };
    analytics::insert_count_bucket(properties, "target_agents_count_bucket", count as u64);
}

#[derive(Debug, Clone)]
pub(crate) struct McpPathContext {
    home: PathBuf,
    xdg_config_home: PathBuf,
    cwd: PathBuf,
    env_overrides: BTreeMap<String, PathBuf>,
}

impl McpPathContext {
    pub(crate) fn from_env() -> Result<Self> {
        let home = home_dir().context("resolve home directory")?;
        let xdg_config_home =
            non_empty_env_path("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        let mut env_overrides = BTreeMap::new();
        for key in [
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
            "COPILOT_HOME",
            "MIMOCODE_HOME",
        ] {
            if let Some(path) = non_empty_env_path(key) {
                env_overrides.insert(key.to_owned(), path);
            }
        }
        Ok(Self {
            home,
            xdg_config_home,
            cwd: env::current_dir().context("resolve current directory")?,
            env_overrides,
        })
    }

    #[cfg(test)]
    fn for_tests(home: PathBuf, cwd: PathBuf) -> Self {
        Self {
            xdg_config_home: home.join(".config"),
            home,
            cwd,
            env_overrides: BTreeMap::new(),
        }
    }

    #[cfg(test)]
    fn with_xdg_config_home(mut self, value: PathBuf) -> Self {
        self.xdg_config_home = value;
        self
    }

    #[cfg(test)]
    fn with_env_override(mut self, key: &str, value: PathBuf) -> Self {
        self.env_overrides.insert(key.to_owned(), value);
        self
    }

    fn env_or_home_child(&self, key: &str, fallback_child: &str) -> PathBuf {
        self.env_overrides
            .get(key)
            .cloned()
            .unwrap_or_else(|| self.home.join(fallback_child))
    }

    fn mimocode_config_dir(&self) -> PathBuf {
        self.env_overrides
            .get("MIMOCODE_HOME")
            .map(|home| home.join("config"))
            .unwrap_or_else(|| self.xdg_config_home.join("mimocode"))
    }

    fn claude_user_config(&self) -> PathBuf {
        self.env_overrides
            .get("CLAUDE_CONFIG_DIR")
            .map(|dir| dir.join(".claude.json"))
            .unwrap_or_else(|| self.home.join(".claude.json"))
    }
}

fn home_dir() -> Option<PathBuf> {
    non_empty_env_path("HOME").or_else(|| non_empty_env_path("USERPROFILE"))
}

fn non_empty_env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum McpAgentArg {
    Codex,
    #[value(name = "claude-code", alias = "claude")]
    ClaudeCode,
    Cursor,
    #[value(name = "opencode", alias = "open-code")]
    OpenCode,
    #[value(name = "mimocode", alias = "mimo-code", alias = "mimo_code")]
    MiMoCode,
    #[value(name = "gemini-cli", alias = "gemini")]
    GeminiCli,
    #[value(name = "qwen-code", alias = "qwen")]
    QwenCode,
    Goose,
    Kiro,
    Warp,
    Continue,
    Cline,
    #[value(name = "github-copilot", alias = "copilot", alias = "copilot-cli")]
    GitHubCopilot,
    Zed,
    Windsurf,
    #[value(name = "roo-code", alias = "roo")]
    RooCode,
}

impl McpAgentArg {
    const ALL: &'static [Self] = &[
        Self::Codex,
        Self::ClaudeCode,
        Self::Cursor,
        Self::OpenCode,
        Self::MiMoCode,
        Self::GeminiCli,
        Self::QwenCode,
        Self::Goose,
        Self::Kiro,
        Self::Warp,
        Self::Continue,
        Self::Cline,
        Self::GitHubCopilot,
        Self::Zed,
        Self::Windsurf,
    ];
    const PROJECT_CAPABLE: &'static [Self] = &[
        Self::Codex,
        Self::ClaudeCode,
        Self::Cursor,
        Self::OpenCode,
        Self::MiMoCode,
        Self::GeminiCli,
        Self::QwenCode,
        Self::Kiro,
        Self::Warp,
        Self::Continue,
        Self::Zed,
        Self::RooCode,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude-code",
            Self::Cursor => "cursor",
            Self::OpenCode => "opencode",
            Self::MiMoCode => "mimocode",
            Self::GeminiCli => "gemini-cli",
            Self::QwenCode => "qwen-code",
            Self::Goose => "goose",
            Self::Kiro => "kiro",
            Self::Warp => "warp",
            Self::Continue => "continue",
            Self::Cline => "cline",
            Self::GitHubCopilot => "github-copilot",
            Self::Zed => "zed",
            Self::Windsurf => "windsurf",
            Self::RooCode => "roo-code",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::ClaudeCode => "Claude Code",
            Self::Cursor => "Cursor",
            Self::OpenCode => "OpenCode",
            Self::MiMoCode => "MiMo Code",
            Self::GeminiCli => "Gemini CLI",
            Self::QwenCode => "Qwen Code",
            Self::Goose => "Goose",
            Self::Kiro => "Kiro",
            Self::Warp => "Warp",
            Self::Continue => "Continue",
            Self::Cline => "Cline",
            Self::GitHubCopilot => "GitHub Copilot CLI",
            Self::Zed => "Zed",
            Self::Windsurf => "Windsurf",
            Self::RooCode => "Roo Code",
        }
    }

    fn detected(self, context: &McpPathContext) -> bool {
        match self {
            Self::Codex => {
                context.env_overrides.contains_key("CODEX_HOME")
                    || context.home.join(".codex").exists()
                    || Path::new("/etc/codex").exists()
            }
            Self::ClaudeCode => {
                context.env_overrides.contains_key("CLAUDE_CONFIG_DIR")
                    || context.home.join(".claude").exists()
                    || context.home.join(".claude.json").exists()
            }
            Self::Cursor => context.home.join(".cursor").exists(),
            Self::OpenCode => context.xdg_config_home.join("opencode").exists(),
            Self::MiMoCode => {
                context.env_overrides.contains_key("MIMOCODE_HOME")
                    || context.mimocode_config_dir().exists()
            }
            Self::GeminiCli => context.home.join(".gemini").exists(),
            Self::QwenCode => context.home.join(".qwen").exists(),
            Self::Goose => context.xdg_config_home.join("goose").exists(),
            Self::Kiro => context.home.join(".kiro").exists(),
            Self::Warp => context.home.join(".warp").exists(),
            Self::Continue => context.home.join(".continue").join("config.yaml").exists(),
            Self::Cline => context.home.join(".cline").exists(),
            Self::GitHubCopilot => {
                context.env_overrides.contains_key("COPILOT_HOME")
                    || context.home.join(".copilot").exists()
            }
            Self::Zed => context.xdg_config_home.join("zed").exists(),
            Self::Windsurf => context.home.join(".codeium").exists(),
            Self::RooCode => {
                context.home.join(".roo").exists() || context.cwd.join(".roo").exists()
            }
        }
    }

    fn target(self, project: bool, context: &McpPathContext) -> McpTarget {
        if project {
            return self.project_target(context);
        }
        self.global_target(context)
    }

    fn global_target(self, context: &McpPathContext) -> McpTarget {
        let (path, kind) = match self {
            Self::Codex => (
                context
                    .env_or_home_child("CODEX_HOME", ".codex")
                    .join("config.toml"),
                ConfigKind::CodexToml,
            ),
            Self::ClaudeCode => (
                context.claude_user_config(),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::StdioType,
                },
            ),
            Self::Cursor => (
                context.home.join(".cursor").join("mcp.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::StdioType,
                },
            ),
            Self::OpenCode => (
                context
                    .xdg_config_home
                    .join("opencode")
                    .join("opencode.json"),
                ConfigKind::opencode_json(),
            ),
            Self::MiMoCode => (
                context.mimocode_config_dir().join("mimocode.jsonc"),
                ConfigKind::opencode_json(),
            ),
            Self::GeminiCli => (
                context.home.join(".gemini").join("settings.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::Plain,
                },
            ),
            Self::QwenCode => (
                context.home.join(".qwen").join("settings.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::Plain,
                },
            ),
            Self::Goose => (
                context.xdg_config_home.join("goose").join("config.yaml"),
                ConfigKind::GooseYaml,
            ),
            Self::Kiro => (
                context.home.join(".kiro").join("settings").join("mcp.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::Plain,
                },
            ),
            Self::Warp => (
                context.home.join(".warp").join(".mcp.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::Plain,
                },
            ),
            Self::Continue => (
                context.home.join(".continue").join("config.yaml"),
                ConfigKind::ContinueYaml,
            ),
            Self::Cline => (
                context.home.join(".cline").join("mcp.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::ClineLocal,
                },
            ),
            Self::GitHubCopilot => (
                context
                    .env_or_home_child("COPILOT_HOME", ".copilot")
                    .join("mcp-config.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::CopilotLocal,
                },
            ),
            Self::Zed => (
                context.xdg_config_home.join("zed").join("settings.json"),
                ConfigKind::Json {
                    root: JsonRoot::ContextServers,
                    server: JsonServerShape::Plain,
                },
            ),
            Self::Windsurf => (
                context.home.join(".codeium").join("mcp_config.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::Plain,
                },
            ),
            Self::RooCode => {
                return McpTarget::unsupported(
                    self,
                    McpScope::Global,
                    "global Roo Code MCP config path is managed by the extension UI and is not stable across hosts",
                );
            }
        };
        McpTarget::supported(self, McpScope::Global, path, kind, self.detected(context))
    }

    fn project_target(self, context: &McpPathContext) -> McpTarget {
        let target = match self {
            Self::Codex => Some((
                context.cwd.join(".codex").join("config.toml"),
                ConfigKind::CodexToml,
            )),
            Self::ClaudeCode => Some((
                context.cwd.join(".mcp.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::StdioType,
                },
            )),
            Self::Cursor => Some((
                context.cwd.join(".cursor").join("mcp.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::StdioType,
                },
            )),
            Self::OpenCode => Some((
                context.cwd.join("opencode.json"),
                ConfigKind::opencode_json(),
            )),
            Self::MiMoCode => Some((
                context.cwd.join(".mimocode").join("mimocode.jsonc"),
                ConfigKind::opencode_json(),
            )),
            Self::GeminiCli => Some((
                context.cwd.join(".gemini").join("settings.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::Plain,
                },
            )),
            Self::QwenCode => Some((
                context.cwd.join(".qwen").join("settings.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::Plain,
                },
            )),
            Self::Kiro => Some((
                context.cwd.join(".kiro").join("settings").join("mcp.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::Plain,
                },
            )),
            Self::Warp => Some((
                context.cwd.join(".warp").join(".mcp.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::Plain,
                },
            )),
            Self::Continue => Some((
                context
                    .cwd
                    .join(".continue")
                    .join("mcpServers")
                    .join("ctx.yaml"),
                ConfigKind::ContinueYaml,
            )),
            Self::Zed => Some((
                context.cwd.join(".zed").join("settings.json"),
                ConfigKind::Json {
                    root: JsonRoot::ContextServers,
                    server: JsonServerShape::Plain,
                },
            )),
            Self::RooCode => Some((
                context.cwd.join(".roo").join("mcp.json"),
                ConfigKind::Json {
                    root: JsonRoot::McpServers,
                    server: JsonServerShape::Plain,
                },
            )),
            Self::Cline | Self::Goose | Self::GitHubCopilot | Self::Windsurf => None,
        };
        match target {
            Some((path, kind)) => McpTarget::supported(
                self,
                McpScope::Project,
                path,
                kind,
                project_detection_path(self, context).exists(),
            ),
            None => McpTarget::unsupported(
                self,
                McpScope::Project,
                "project-scoped MCP config is not documented for this agent",
            ),
        }
    }
}

fn project_detection_path(agent: McpAgentArg, context: &McpPathContext) -> PathBuf {
    match agent {
        McpAgentArg::Codex => context.cwd.join(".codex"),
        McpAgentArg::ClaudeCode => context.cwd.join(".mcp.json"),
        McpAgentArg::Cursor => context.cwd.join(".cursor"),
        McpAgentArg::OpenCode => context.cwd.join("opencode.json"),
        McpAgentArg::MiMoCode => context.cwd.join(".mimocode"),
        McpAgentArg::GeminiCli => context.cwd.join(".gemini"),
        McpAgentArg::QwenCode => context.cwd.join(".qwen"),
        McpAgentArg::Kiro => context.cwd.join(".kiro"),
        McpAgentArg::Warp => context.cwd.join(".warp"),
        McpAgentArg::Continue => context.cwd.join(".continue"),
        McpAgentArg::Zed => context.cwd.join(".zed"),
        McpAgentArg::RooCode => context.cwd.join(".roo"),
        McpAgentArg::Cline
        | McpAgentArg::Goose
        | McpAgentArg::GitHubCopilot
        | McpAgentArg::Windsurf => context.cwd.clone(),
    }
}

#[derive(Debug, Clone, Copy)]
enum McpScope {
    Global,
    Project,
}

impl McpScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

#[derive(Debug, Clone)]
struct McpTarget {
    agent: McpAgentArg,
    scope: McpScope,
    path: Option<PathBuf>,
    kind: Option<ConfigKind>,
    detected: bool,
    unsupported_reason: Option<String>,
}

impl McpTarget {
    fn supported(
        agent: McpAgentArg,
        scope: McpScope,
        path: PathBuf,
        kind: ConfigKind,
        detected: bool,
    ) -> Self {
        Self {
            agent,
            scope,
            path: Some(path),
            kind: Some(kind),
            detected,
            unsupported_reason: None,
        }
    }

    fn unsupported(agent: McpAgentArg, scope: McpScope, reason: &str) -> Self {
        Self {
            agent,
            scope,
            path: None,
            kind: None,
            detected: false,
            unsupported_reason: Some(reason.to_owned()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ConfigKind {
    CodexToml,
    GooseYaml,
    ContinueYaml,
    Json {
        root: JsonRoot,
        server: JsonServerShape,
    },
}

impl ConfigKind {
    fn opencode_json() -> Self {
        Self::Json {
            root: JsonRoot::Mcp,
            server: JsonServerShape::OpenCodeLocal,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum JsonRoot {
    McpServers,
    Mcp,
    ContextServers,
}

impl JsonRoot {
    fn key(self) -> &'static str {
        match self {
            Self::McpServers => "mcpServers",
            Self::Mcp => "mcp",
            Self::ContextServers => "context_servers",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum JsonServerShape {
    Plain,
    StdioType,
    OpenCodeLocal,
    CopilotLocal,
    ClineLocal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpConfigStatus {
    Current,
    Missing,
    Conflict,
    Invalid,
    Unsupported,
}

impl McpConfigStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Missing => "missing",
            Self::Conflict => "conflict",
            Self::Invalid => "invalid_config",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug)]
struct McpInstallResult {
    target: McpTarget,
    success: bool,
    previous_status: McpConfigStatus,
    status: McpConfigStatus,
    already_installed: bool,
    modified: bool,
    error: Option<String>,
}

impl McpInstallResult {
    fn to_json(&self) -> Value {
        json!({
            "agent": self.target.agent.id(),
            "agent_display_name": self.target.agent.display_name(),
            "scope": self.target.scope.as_str(),
            "path": self.target.path,
            "detected": self.target.detected,
            "supported": self.target.unsupported_reason.is_none(),
            "success": self.success,
            "previous_status": self.previous_status.as_str(),
            "status": self.status.as_str(),
            "already_installed": self.already_installed,
            "modified": self.modified,
            "error": self.error,
        })
    }
}

#[derive(Debug)]
struct McpStatusResult {
    target: McpTarget,
    status: McpConfigStatus,
    error: Option<String>,
}

impl McpStatusResult {
    fn to_json(&self) -> Value {
        json!({
            "agent": self.target.agent.id(),
            "agent_display_name": self.target.agent.display_name(),
            "scope": self.target.scope.as_str(),
            "path": self.target.path,
            "detected": self.target.detected,
            "supported": self.target.unsupported_reason.is_none(),
            "status": self.status.as_str(),
            "error": self.error,
        })
    }
}

pub(crate) fn run_install(
    args: McpInstallArgs,
    context: &McpPathContext,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let agents = selected_install_agents(&args, context);
    insert_selection_analytics(analytics_properties, &agents);
    let targets = agents
        .iter()
        .copied()
        .map(|agent| agent.target(args.project, context))
        .collect::<Vec<_>>();
    let results = targets
        .iter()
        .map(|target| install_target(target, args.force))
        .collect::<Vec<_>>();
    let failed = results.iter().filter(|result| !result.success).count();
    analytics::insert_str(
        analytics_properties,
        "install_result",
        if failed == 0 { "ok" } else { "partial_error" },
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "modified_targets_bucket",
        results.iter().filter(|result| result.modified).count() as u64,
    );
    if args.json {
        println!(
            "{}",
            json!({
                "integration": "mcp",
                "server": {
                    "name": SERVER_NAME,
                    "command": SERVER_COMMAND,
                    "args": SERVER_ARGS,
                },
                "scope": if args.project { "project" } else { "global" },
                "results": results.iter().map(McpInstallResult::to_json).collect::<Vec<_>>(),
            })
        );
    } else {
        print_install_results(&results);
    }
    if failed > 0 {
        return Err(anyhow!(
            "failed to install MCP integration for {failed} target(s)"
        ));
    }
    Ok(())
}

pub(crate) fn run_status(args: McpStatusArgs, context: &McpPathContext) -> Result<()> {
    let agents = selected_status_agents(&args, context);
    let targets = agents
        .iter()
        .copied()
        .map(|agent| agent.target(args.project, context))
        .collect::<Vec<_>>();
    let results = targets.iter().map(status_target).collect::<Vec<_>>();
    if args.json {
        println!(
            "{}",
            json!({
                "integration": "mcp",
                "server": {
                    "name": SERVER_NAME,
                    "command": SERVER_COMMAND,
                    "args": SERVER_ARGS,
                },
                "scope": if args.project { "project" } else { "global" },
                "results": results.iter().map(McpStatusResult::to_json).collect::<Vec<_>>(),
            })
        );
    } else {
        print_status_results(&results);
    }
    Ok(())
}

fn insert_selection_analytics(properties: &mut AnalyticsProperties, agents: &[McpAgentArg]) {
    analytics::insert_count_bucket(
        properties,
        "resolved_agents_count_bucket",
        agents.len() as u64,
    );
}

fn selected_install_agents(args: &McpInstallArgs, context: &McpPathContext) -> Vec<McpAgentArg> {
    if args.all_agents {
        return if args.project {
            McpAgentArg::PROJECT_CAPABLE.to_vec()
        } else {
            McpAgentArg::ALL.to_vec()
        };
    }
    if !args.agent.is_empty() {
        return dedupe_agents(args.agent.iter().copied());
    }
    if args.project {
        return detected_project_agents(context);
    }
    detected_agents(context)
}

fn selected_status_agents(args: &McpStatusArgs, context: &McpPathContext) -> Vec<McpAgentArg> {
    if args.all_agents {
        return if args.project {
            McpAgentArg::PROJECT_CAPABLE.to_vec()
        } else {
            McpAgentArg::ALL.to_vec()
        };
    }
    if !args.agent.is_empty() {
        return dedupe_agents(args.agent.iter().copied());
    }
    if args.project {
        return detected_project_agents(context);
    }
    detected_agents(context)
}

fn dedupe_agents(agents: impl IntoIterator<Item = McpAgentArg>) -> Vec<McpAgentArg> {
    let mut deduped = Vec::new();
    for agent in agents {
        if !deduped.contains(&agent) {
            deduped.push(agent);
        }
    }
    deduped
}

fn detected_agents(context: &McpPathContext) -> Vec<McpAgentArg> {
    McpAgentArg::ALL
        .iter()
        .copied()
        .filter(|agent| agent.detected(context))
        .collect()
}

fn detected_project_agents(context: &McpPathContext) -> Vec<McpAgentArg> {
    McpAgentArg::PROJECT_CAPABLE
        .iter()
        .copied()
        .filter(|agent| project_detection_path(*agent, context).exists())
        .collect()
}

fn install_target(target: &McpTarget, force: bool) -> McpInstallResult {
    let previous = status_target(target);
    if previous.status == McpConfigStatus::Current {
        return McpInstallResult {
            target: target.clone(),
            success: true,
            previous_status: previous.status,
            status: McpConfigStatus::Current,
            already_installed: true,
            modified: false,
            error: None,
        };
    }
    if matches!(
        previous.status,
        McpConfigStatus::Unsupported | McpConfigStatus::Invalid
    ) {
        return McpInstallResult {
            target: target.clone(),
            success: false,
            previous_status: previous.status,
            status: previous.status,
            already_installed: false,
            modified: false,
            error: previous.error,
        };
    }
    if previous.status == McpConfigStatus::Conflict && !force {
        return McpInstallResult {
            target: target.clone(),
            success: false,
            previous_status: previous.status,
            status: previous.status,
            already_installed: false,
            modified: false,
            error: Some(
                "existing ctx MCP server has different command or args; rerun with --force to overwrite"
                    .to_owned(),
            ),
        };
    }
    let result = write_target(target, force);
    match result {
        Ok(()) => McpInstallResult {
            target: target.clone(),
            success: true,
            previous_status: previous.status,
            status: McpConfigStatus::Current,
            already_installed: false,
            modified: true,
            error: None,
        },
        Err(err) => McpInstallResult {
            target: target.clone(),
            success: false,
            previous_status: previous.status,
            status: McpConfigStatus::Invalid,
            already_installed: false,
            modified: false,
            error: Some(err.to_string()),
        },
    }
}

fn status_target(target: &McpTarget) -> McpStatusResult {
    let Some(path) = target.path.as_ref() else {
        return McpStatusResult {
            target: target.clone(),
            status: McpConfigStatus::Unsupported,
            error: target.unsupported_reason.clone(),
        };
    };
    let Some(kind) = target.kind else {
        return McpStatusResult {
            target: target.clone(),
            status: McpConfigStatus::Unsupported,
            error: target.unsupported_reason.clone(),
        };
    };
    match read_target_status(path, kind) {
        Ok(status) => McpStatusResult {
            target: target.clone(),
            status,
            error: None,
        },
        Err(err) => McpStatusResult {
            target: target.clone(),
            status: McpConfigStatus::Invalid,
            error: Some(err.to_string()),
        },
    }
}

fn read_target_status(path: &Path, kind: ConfigKind) -> Result<McpConfigStatus> {
    let body = match fs::read_to_string(path) {
        Ok(body) => body,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(McpConfigStatus::Missing),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    if body.trim().is_empty() {
        return Ok(McpConfigStatus::Missing);
    }
    match kind {
        ConfigKind::CodexToml => status_codex_toml(&body),
        ConfigKind::GooseYaml => status_goose_yaml(&body),
        ConfigKind::ContinueYaml => status_continue_yaml(&body),
        ConfigKind::Json { root, .. } => status_json(&body, root),
    }
}

fn write_target(target: &McpTarget, force: bool) -> Result<()> {
    let path = target
        .path
        .as_ref()
        .ok_or_else(|| anyhow!("unsupported MCP target"))?;
    let kind = target
        .kind
        .ok_or_else(|| anyhow!("unsupported MCP target"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let existing = match fs::read_to_string(path) {
        Ok(body) => body,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    let body = match kind {
        ConfigKind::CodexToml => update_codex_toml(&existing, force)?,
        ConfigKind::GooseYaml => update_goose_yaml(&existing, force)?,
        ConfigKind::ContinueYaml => update_continue_yaml(&existing, force)?,
        ConfigKind::Json { root, server } => update_json(&existing, root, server, force)?,
    };
    fs::write(path, body).with_context(|| format!("write {}", path.display()))
}

fn status_json(body: &str, root: JsonRoot) -> Result<McpConfigStatus> {
    let doc: Value = serde_json::from_str(body).context("parse JSON config")?;
    let object = doc
        .as_object()
        .ok_or_else(|| anyhow!("JSON config root must be an object"))?;
    let Some(servers) = object.get(root.key()) else {
        return Ok(McpConfigStatus::Missing);
    };
    let servers = servers
        .as_object()
        .ok_or_else(|| anyhow!("{} must be an object", root.key()))?;
    let Some(server) = servers.get(SERVER_NAME) else {
        return Ok(McpConfigStatus::Missing);
    };
    Ok(if json_server_is_current(server) {
        McpConfigStatus::Current
    } else {
        McpConfigStatus::Conflict
    })
}

fn update_json(body: &str, root: JsonRoot, shape: JsonServerShape, force: bool) -> Result<String> {
    let mut doc = if body.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str(body).context("parse JSON config")?
    };
    let object = doc
        .as_object_mut()
        .ok_or_else(|| anyhow!("JSON config root must be an object"))?;
    let root_value = object
        .entry(root.key().to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let servers = root_value
        .as_object_mut()
        .ok_or_else(|| anyhow!("{} must be an object", root.key()))?;
    if let Some(existing) = servers.get(SERVER_NAME) {
        if json_server_is_current(existing) {
            return format_json(&doc);
        }
        if !force {
            return Err(anyhow!(
                "existing ctx MCP server has different command or args"
            ));
        }
    }
    servers.insert(SERVER_NAME.to_owned(), json_server_value(shape));
    format_json(&doc)
}

fn format_json(value: &Value) -> Result<String> {
    let mut body = serde_json::to_string_pretty(value)?;
    body.push('\n');
    Ok(body)
}

fn json_server_value(shape: JsonServerShape) -> Value {
    match shape {
        JsonServerShape::Plain => json!({
            "command": SERVER_COMMAND,
            "args": SERVER_ARGS,
        }),
        JsonServerShape::StdioType => json!({
            "type": "stdio",
            "command": SERVER_COMMAND,
            "args": SERVER_ARGS,
        }),
        JsonServerShape::OpenCodeLocal => json!({
            "type": "local",
            "command": [SERVER_COMMAND, "mcp", "serve"],
            "enabled": true,
        }),
        JsonServerShape::CopilotLocal => json!({
            "type": "local",
            "command": SERVER_COMMAND,
            "args": SERVER_ARGS,
            "tools": ["*"],
        }),
        JsonServerShape::ClineLocal => json!({
            "command": SERVER_COMMAND,
            "args": SERVER_ARGS,
            "disabled": false,
            "autoApprove": [],
        }),
    }
}

fn json_server_is_current(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if let Some(command) = object.get("command") {
        if command.as_str() == Some(SERVER_COMMAND) {
            return json_args_are_current(object.get("args"));
        }
        if command.as_array().is_some_and(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .eq([SERVER_COMMAND, "mcp", "serve"])
        }) {
            return true;
        }
    }
    false
}

fn json_args_are_current(value: Option<&Value>) -> bool {
    let Some(args) = value.and_then(Value::as_array) else {
        return false;
    };
    args.iter()
        .filter_map(Value::as_str)
        .eq(SERVER_ARGS.iter().copied())
}

fn status_codex_toml(body: &str) -> Result<McpConfigStatus> {
    let doc = body.parse::<DocumentMut>().context("parse TOML config")?;
    let Some(server) = doc
        .get("mcp_servers")
        .and_then(Item::as_table)
        .and_then(|servers| servers.get(SERVER_NAME))
        .and_then(Item::as_table)
    else {
        return Ok(McpConfigStatus::Missing);
    };
    Ok(if toml_server_is_current(server) {
        McpConfigStatus::Current
    } else {
        McpConfigStatus::Conflict
    })
}

fn update_codex_toml(body: &str, force: bool) -> Result<String> {
    let mut doc = if body.trim().is_empty() {
        DocumentMut::new()
    } else {
        body.parse::<DocumentMut>().context("parse TOML config")?
    };
    if !doc.contains_key("mcp_servers") {
        doc["mcp_servers"] = Item::Table(Table::new());
    }
    let servers = doc["mcp_servers"]
        .as_table_mut()
        .ok_or_else(|| anyhow!("mcp_servers must be a TOML table"))?;
    if let Some(existing) = servers.get(SERVER_NAME).and_then(Item::as_table) {
        if toml_server_is_current(existing) {
            return Ok(doc.to_string());
        }
        if !force {
            return Err(anyhow!(
                "existing ctx MCP server has different command or args"
            ));
        }
    }
    let mut table = Table::new();
    table["command"] = toml_value(SERVER_COMMAND);
    let mut args = TomlArray::default();
    for arg in SERVER_ARGS {
        args.push(*arg);
    }
    table["args"] = Item::Value(TomlValue::Array(args));
    servers[SERVER_NAME] = Item::Table(table);
    Ok(doc.to_string())
}

fn toml_server_is_current(table: &Table) -> bool {
    let command_ok = table
        .get("command")
        .and_then(Item::as_str)
        .is_some_and(|command| command == SERVER_COMMAND);
    let args_ok = table
        .get("args")
        .and_then(Item::as_array)
        .is_some_and(|args| {
            args.iter()
                .filter_map(TomlValue::as_str)
                .eq(SERVER_ARGS.iter().copied())
        });
    command_ok && args_ok
}

fn status_continue_yaml(body: &str) -> Result<McpConfigStatus> {
    let doc: serde_yaml::Value = serde_yaml::from_str(body).context("parse YAML config")?;
    let Some(servers) = yaml_mapping_get(&doc, "mcpServers") else {
        return Ok(McpConfigStatus::Missing);
    };
    let servers = servers
        .as_sequence()
        .ok_or_else(|| anyhow!("mcpServers must be a YAML sequence"))?;
    let Some(server) = continue_server_by_name(servers) else {
        return Ok(McpConfigStatus::Missing);
    };
    Ok(if continue_server_is_current(server) {
        McpConfigStatus::Current
    } else {
        McpConfigStatus::Conflict
    })
}

fn update_continue_yaml(body: &str, force: bool) -> Result<String> {
    let mut doc = if body.trim().is_empty() {
        let mut mapping = serde_yaml::Mapping::new();
        mapping.insert(
            serde_yaml::Value::String("name".to_owned()),
            serde_yaml::Value::String("ctx MCP".to_owned()),
        );
        mapping.insert(
            serde_yaml::Value::String("version".to_owned()),
            serde_yaml::Value::String("0.0.1".to_owned()),
        );
        mapping.insert(
            serde_yaml::Value::String("schema".to_owned()),
            serde_yaml::Value::String("v1".to_owned()),
        );
        serde_yaml::Value::Mapping(mapping)
    } else {
        serde_yaml::from_str(body).context("parse YAML config")?
    };
    let root = doc
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("YAML config root must be a mapping"))?;
    let servers_key = serde_yaml::Value::String("mcpServers".to_owned());
    let servers = root
        .entry(servers_key)
        .or_insert_with(|| serde_yaml::Value::Sequence(Vec::new()));
    let servers = servers
        .as_sequence_mut()
        .ok_or_else(|| anyhow!("mcpServers must be a YAML sequence"))?;
    if let Some(index) = continue_server_index(servers) {
        if continue_server_is_current(&servers[index]) {
            return format_yaml(&doc);
        }
        if !force {
            return Err(anyhow!(
                "existing ctx MCP server has different command or args"
            ));
        }
        servers[index] = continue_server_value();
    } else {
        servers.push(continue_server_value());
    }
    format_yaml(&doc)
}

fn continue_server_by_name(servers: &[serde_yaml::Value]) -> Option<&serde_yaml::Value> {
    continue_server_index(servers).map(|index| &servers[index])
}

fn continue_server_index(servers: &[serde_yaml::Value]) -> Option<usize> {
    servers.iter().position(|server| {
        yaml_mapping_get(server, "name").and_then(serde_yaml::Value::as_str) == Some(SERVER_NAME)
    })
}

fn continue_server_value() -> serde_yaml::Value {
    let mut mapping = serde_yaml::Mapping::new();
    mapping.insert(
        serde_yaml::Value::String("name".to_owned()),
        serde_yaml::Value::String(SERVER_NAME.to_owned()),
    );
    mapping.insert(
        serde_yaml::Value::String("type".to_owned()),
        serde_yaml::Value::String("stdio".to_owned()),
    );
    mapping.insert(
        serde_yaml::Value::String("command".to_owned()),
        serde_yaml::Value::String(SERVER_COMMAND.to_owned()),
    );
    mapping.insert(
        serde_yaml::Value::String("args".to_owned()),
        serde_yaml::Value::Sequence(
            SERVER_ARGS
                .iter()
                .map(|arg| serde_yaml::Value::String((*arg).to_owned()))
                .collect(),
        ),
    );
    serde_yaml::Value::Mapping(mapping)
}

fn continue_server_is_current(value: &serde_yaml::Value) -> bool {
    let Some(mapping) = value.as_mapping() else {
        return false;
    };
    let command = mapping
        .get(serde_yaml::Value::String("command".to_owned()))
        .and_then(serde_yaml::Value::as_str);
    let args = mapping
        .get(serde_yaml::Value::String("args".to_owned()))
        .and_then(serde_yaml::Value::as_sequence);
    command == Some(SERVER_COMMAND) && yaml_args_are_current(args)
}

fn status_goose_yaml(body: &str) -> Result<McpConfigStatus> {
    let doc: serde_yaml::Value = serde_yaml::from_str(body).context("parse YAML config")?;
    let Some(extensions) = yaml_mapping_get(&doc, "extensions") else {
        return Ok(McpConfigStatus::Missing);
    };
    let Some(server) = yaml_mapping_get(extensions, SERVER_NAME) else {
        return Ok(McpConfigStatus::Missing);
    };
    Ok(if goose_server_is_current(server) {
        McpConfigStatus::Current
    } else {
        McpConfigStatus::Conflict
    })
}

fn update_goose_yaml(body: &str, force: bool) -> Result<String> {
    let mut doc = if body.trim().is_empty() {
        serde_yaml::Value::Mapping(Default::default())
    } else {
        serde_yaml::from_str(body).context("parse YAML config")?
    };
    let root = doc
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("YAML config root must be a mapping"))?;
    let extensions_key = serde_yaml::Value::String("extensions".to_owned());
    let extensions = root
        .entry(extensions_key)
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));
    let extensions = extensions
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("extensions must be a YAML mapping"))?;
    let ctx_key = serde_yaml::Value::String(SERVER_NAME.to_owned());
    if let Some(existing) = extensions.get(&ctx_key) {
        if goose_server_is_current(existing) {
            return format_yaml(&doc);
        }
        if !force {
            return Err(anyhow!(
                "existing ctx MCP extension has different command or args"
            ));
        }
    }
    extensions.insert(ctx_key, goose_server_value());
    format_yaml(&doc)
}

fn format_yaml(value: &serde_yaml::Value) -> Result<String> {
    let mut body = serde_yaml::to_string(value)?;
    if !body.ends_with('\n') {
        body.push('\n');
    }
    Ok(body)
}

fn yaml_mapping_get<'a>(value: &'a serde_yaml::Value, key: &str) -> Option<&'a serde_yaml::Value> {
    value
        .as_mapping()?
        .get(serde_yaml::Value::String(key.to_owned()))
}

fn goose_server_value() -> serde_yaml::Value {
    let mut mapping = serde_yaml::Mapping::new();
    mapping.insert(
        serde_yaml::Value::String("enabled".to_owned()),
        serde_yaml::Value::Bool(true),
    );
    mapping.insert(
        serde_yaml::Value::String("name".to_owned()),
        serde_yaml::Value::String(SERVER_NAME.to_owned()),
    );
    mapping.insert(
        serde_yaml::Value::String("display_name".to_owned()),
        serde_yaml::Value::String("ctx".to_owned()),
    );
    mapping.insert(
        serde_yaml::Value::String("type".to_owned()),
        serde_yaml::Value::String("stdio".to_owned()),
    );
    mapping.insert(
        serde_yaml::Value::String("cmd".to_owned()),
        serde_yaml::Value::String(SERVER_COMMAND.to_owned()),
    );
    mapping.insert(
        serde_yaml::Value::String("args".to_owned()),
        serde_yaml::Value::Sequence(
            SERVER_ARGS
                .iter()
                .map(|arg| serde_yaml::Value::String((*arg).to_owned()))
                .collect(),
        ),
    );
    mapping.insert(
        serde_yaml::Value::String("timeout".to_owned()),
        serde_yaml::Value::Number(300.into()),
    );
    serde_yaml::Value::Mapping(mapping)
}

fn goose_server_is_current(value: &serde_yaml::Value) -> bool {
    let Some(mapping) = value.as_mapping() else {
        return false;
    };
    let cmd = mapping
        .get(serde_yaml::Value::String("cmd".to_owned()))
        .and_then(serde_yaml::Value::as_str)
        .or_else(|| {
            mapping
                .get(serde_yaml::Value::String("command".to_owned()))
                .and_then(serde_yaml::Value::as_str)
        });
    let args = mapping
        .get(serde_yaml::Value::String("args".to_owned()))
        .and_then(serde_yaml::Value::as_sequence);
    cmd == Some(SERVER_COMMAND) && yaml_args_are_current(args)
}

fn yaml_args_are_current(args: Option<&Vec<serde_yaml::Value>>) -> bool {
    args.is_some_and(|args| {
        args.iter()
            .filter_map(serde_yaml::Value::as_str)
            .eq(SERVER_ARGS.iter().copied())
    })
}

fn print_install_results(results: &[McpInstallResult]) {
    if results.is_empty() {
        println!("No detected MCP-capable coding agents found.");
        println!("Use --agent <name> or --all-agents to install a specific MCP config.");
        return;
    }
    let all_current = results.iter().all(|result| result.already_installed);
    let all_success = results.iter().all(|result| result.success);
    let any_modified = results.iter().any(|result| result.modified);
    let heading = if all_current {
        "ctx MCP integration already installed"
    } else if all_success && any_modified {
        "ctx MCP integration installed"
    } else {
        "ctx MCP integration"
    };
    println!("{heading}: {SERVER_COMMAND} {}", SERVER_ARGS.join(" "));
    for result in results {
        let verb = if result.already_installed {
            "current"
        } else if result.modified {
            "modified"
        } else if result.success {
            "ok"
        } else {
            "skipped"
        };
        let detail = result
            .error
            .as_deref()
            .map(|error| format!(" - {error}"))
            .unwrap_or_default();
        let path = result
            .target
            .path
            .as_ref()
            .map(|path| format!(" -> {}", path.display()))
            .unwrap_or_default();
        println!(
            "  {verb}: {}{}{}",
            result.target.agent.display_name(),
            path,
            detail
        );
    }
}

fn print_status_results(results: &[McpStatusResult]) {
    if results.is_empty() {
        println!("No detected MCP-capable coding agents found.");
        println!("Use --agent <name> or --all-agents to inspect a specific MCP config.");
        return;
    }
    println!(
        "ctx MCP integration status: {SERVER_COMMAND} {}",
        SERVER_ARGS.join(" ")
    );
    for result in results {
        let detail = result
            .error
            .as_deref()
            .map(|error| format!(" - {error}"))
            .unwrap_or_default();
        let path = result
            .target
            .path
            .as_ref()
            .map(|path| format!(" -> {}", path.display()))
            .unwrap_or_default();
        println!(
            "  {}: {} ({}){}{}",
            result.status.as_str(),
            result.target.agent.display_name(),
            result.target.scope.as_str(),
            path,
            detail
        );
    }
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod mcp_tests;
