use std::{
    collections::BTreeMap,
    env, fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use clap::{Args, ValueEnum};
use ctx_history_core::utc_now;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{analytics, AnalyticsProperties};

const COMMAND_NAME: &str = "ctx-history";
const METADATA_FILE: &str = ".ctx-slash-commands.json";

const COMMAND_INSTRUCTIONS: &str = r#"# ctx History

Use ctx to search local coding-agent history for this request.

User request: $ARGUMENTS

Search local agent history with `ctx`; text and Markdown history output is
automatically framed as untrusted. Inspect cited events or sessions before
making claims, and return a concise answer with ctx citations. Use `--json`
only when piping to a script, `jq`, or extracting exact machine fields.
"#;

const WINDSURF_WORKFLOW: &str = r#"# ctx History

Search local coding-agent history with ctx.

1. Treat any text after `/ctx-history` as the user request.
2. Search with `ctx search "<query>"`.
3. Inspect relevant citations with `ctx show event <id> --window 5` or `ctx show session <id>`.
4. Answer concisely and include ctx citations for claims based on local history.
5. Use `--json` only when piping to a script, `jq`, or extracting exact machine fields.
"#;

#[derive(Debug, Args)]
pub(crate) struct SlashCommandInstallArgs {
    #[arg(long = "agent", value_enum, conflicts_with = "all_agents")]
    agent: Vec<SlashCommandAgentArg>,
    #[arg(long, conflicts_with = "agent")]
    all_agents: bool,
    #[arg(
        long,
        help = "Install into the current project instead of global agent dirs"
    )]
    project: bool,
    #[arg(long)]
    pub(crate) json: bool,
    #[arg(long, help = "Overwrite locally modified ctx-managed command files")]
    force: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum SlashCommandAgentArg {
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
    Antigravity,
    #[value(name = "github-copilot", alias = "copilot")]
    GitHubCopilot,
    Pi,
    Goose,
    Continue,
    Windsurf,
}

impl SlashCommandAgentArg {
    const ALL: &'static [Self] = &[
        Self::Codex,
        Self::ClaudeCode,
        Self::Cursor,
        Self::OpenCode,
        Self::MiMoCode,
        Self::GeminiCli,
        Self::QwenCode,
        Self::Antigravity,
        Self::GitHubCopilot,
        Self::Pi,
        Self::Goose,
        Self::Continue,
        Self::Windsurf,
    ];

    const WRITABLE: &'static [Self] = &[
        Self::OpenCode,
        Self::MiMoCode,
        Self::GeminiCli,
        Self::QwenCode,
        Self::Windsurf,
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
            Self::Antigravity => "antigravity",
            Self::GitHubCopilot => "github-copilot",
            Self::Pi => "pi",
            Self::Goose => "goose",
            Self::Continue => "continue",
            Self::Windsurf => "windsurf",
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
            Self::Antigravity => "Antigravity",
            Self::GitHubCopilot => "GitHub Copilot",
            Self::Pi => "Pi",
            Self::Goose => "Goose",
            Self::Continue => "Continue",
            Self::Windsurf => "Windsurf",
        }
    }

    fn detected(self, context: &PathContext) -> bool {
        match self {
            Self::OpenCode => context.xdg_config_home.join("opencode").exists(),
            Self::MiMoCode => {
                context.mimocode_home.is_some()
                    || context.mimocode_config_dir.is_some()
                    || context.mimocode_config_dir().exists()
            }
            Self::GeminiCli => context.home.join(".gemini").exists(),
            Self::QwenCode => context.home.join(".qwen").exists(),
            Self::Windsurf => context.home.join(".codeium").join("windsurf").exists(),
            Self::Codex
            | Self::ClaudeCode
            | Self::Cursor
            | Self::Antigravity
            | Self::GitHubCopilot
            | Self::Pi
            | Self::Goose
            | Self::Continue => false,
        }
    }

    fn install_plan(self, project: bool, context: &PathContext) -> SlashCommandPlan {
        match self {
            Self::OpenCode => SlashCommandPlan::File(CommandFileTarget {
                agent: self,
                scope: scope(project),
                base_dir: if project {
                    context.cwd.join(".opencode").join("commands")
                } else {
                    context.xdg_config_home.join("opencode").join("commands")
                },
                filename: format!("{COMMAND_NAME}.md"),
                body: opencode_command_body(),
            }),
            Self::MiMoCode => SlashCommandPlan::File(CommandFileTarget {
                agent: self,
                scope: scope(project),
                base_dir: if project {
                    context.cwd.join(".mimocode").join("commands")
                } else {
                    context.mimocode_config_dir().join("commands")
                },
                filename: format!("{COMMAND_NAME}.md"),
                body: opencode_command_body(),
            }),
            Self::GeminiCli => SlashCommandPlan::File(CommandFileTarget {
                agent: self,
                scope: scope(project),
                base_dir: if project {
                    context.cwd.join(".gemini").join("commands")
                } else {
                    context.home.join(".gemini").join("commands")
                },
                filename: format!("{COMMAND_NAME}.toml"),
                body: gemini_command_body(),
            }),
            Self::QwenCode => SlashCommandPlan::File(CommandFileTarget {
                agent: self,
                scope: scope(project),
                base_dir: if project {
                    context.cwd.join(".qwen").join("commands")
                } else {
                    context.home.join(".qwen").join("commands")
                },
                filename: format!("{COMMAND_NAME}.md"),
                body: qwen_command_body(),
            }),
            Self::Windsurf => SlashCommandPlan::File(CommandFileTarget {
                agent: self,
                scope: scope(project),
                base_dir: if project {
                    context.cwd.join(".windsurf").join("workflows")
                } else {
                    context
                        .home
                        .join(".codeium")
                        .join("windsurf")
                        .join("global_workflows")
                },
                filename: format!("{COMMAND_NAME}.md"),
                body: WINDSURF_WORKFLOW.to_owned(),
            }),
            Self::Codex | Self::ClaudeCode | Self::Cursor | Self::Antigravity => {
                SlashCommandPlan::SkillOnly {
                    agent: self,
                    note: "slash-style invocation is covered by Agent Skills; run `ctx integrations install skills --agent <agent>`",
                }
            }
            Self::GitHubCopilot | Self::Pi => SlashCommandPlan::SkillOnly {
                agent: self,
                note: "ctx supports this provider through the bundled Agent Skill; run `ctx integrations install skills --agent <agent>`",
            },
            Self::Goose => SlashCommandPlan::ManualOnly {
                agent: self,
                note: "Goose slash commands map to recipes in config.yaml; ctx does not edit that YAML safely yet",
            },
            Self::Continue => SlashCommandPlan::ManualOnly {
                agent: self,
                note: "Continue slash commands are invokable prompts referenced from config.yaml; ctx does not edit that YAML safely yet",
            },
        }
    }
}

fn scope(project: bool) -> SlashCommandScope {
    if project {
        SlashCommandScope::Project
    } else {
        SlashCommandScope::Global
    }
}

pub(crate) fn insert_install_analytics(
    properties: &mut AnalyticsProperties,
    args: &SlashCommandInstallArgs,
) {
    analytics::insert_str(
        properties,
        "slash_command_scope",
        if args.project { "project" } else { "global" },
    );
    analytics::insert_str(
        properties,
        "target_agent_group",
        if args.all_agents {
            "all"
        } else if args.agent.is_empty() {
            "detected"
        } else {
            "explicit"
        },
    );
    let count = if args.all_agents {
        SlashCommandAgentArg::ALL.len()
    } else {
        args.agent.len()
    };
    analytics::insert_count_bucket(properties, "target_agents_count_bucket", count as u64);
}

#[derive(Debug, Clone)]
pub(crate) struct PathContext {
    home: PathBuf,
    xdg_config_home: PathBuf,
    cwd: PathBuf,
    mimocode_home: Option<PathBuf>,
    mimocode_config_dir: Option<PathBuf>,
}

impl PathContext {
    pub(crate) fn from_env() -> Result<Self> {
        let home = home_dir().context("resolve home directory")?;
        let xdg_config_home =
            non_empty_env_path("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        let mimocode_home = non_empty_absolute_env_path("MIMOCODE_HOME")?;
        let mimocode_config_dir = non_empty_env_path("MIMOCODE_CONFIG_DIR");
        Ok(Self {
            home,
            xdg_config_home,
            cwd: env::current_dir().context("resolve current directory")?,
            mimocode_home,
            mimocode_config_dir,
        })
    }

    #[cfg(test)]
    fn for_tests(home: PathBuf, cwd: PathBuf) -> Self {
        Self {
            xdg_config_home: home.join(".config"),
            home,
            cwd,
            mimocode_home: None,
            mimocode_config_dir: None,
        }
    }

    #[cfg(test)]
    fn with_xdg_config_home(mut self, value: PathBuf) -> Self {
        self.xdg_config_home = value;
        self
    }

    fn mimocode_config_dir(&self) -> PathBuf {
        if let Some(path) = &self.mimocode_config_dir {
            return path.clone();
        }
        self.mimocode_home
            .as_ref()
            .map(|home| home.join("config"))
            .unwrap_or_else(|| self.xdg_config_home.join("mimocode"))
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

fn non_empty_absolute_env_path(key: &str) -> Result<Option<PathBuf>> {
    let Some(path) = non_empty_env_path(key) else {
        return Ok(None);
    };
    if !path.is_absolute() {
        return Err(anyhow!(
            "{key} must be an absolute path: {}",
            path.display()
        ));
    }
    Ok(Some(path))
}

#[derive(Debug, Clone)]
enum SlashCommandPlan {
    File(CommandFileTarget),
    SkillOnly {
        agent: SlashCommandAgentArg,
        note: &'static str,
    },
    ManualOnly {
        agent: SlashCommandAgentArg,
        note: &'static str,
    },
}

#[derive(Debug, Clone)]
struct CommandFileTarget {
    agent: SlashCommandAgentArg,
    scope: SlashCommandScope,
    base_dir: PathBuf,
    filename: String,
    body: String,
}

impl CommandFileTarget {
    fn command_path(&self) -> PathBuf {
        self.base_dir.join(&self.filename)
    }

    fn bundled_hash(&self) -> String {
        sha256_hex(self.body.as_bytes())
    }
}

#[derive(Debug, Clone, Copy)]
enum SlashCommandScope {
    Global,
    Project,
}

impl SlashCommandScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlashCommandInstallStatus {
    Current,
    Stale,
    Modified,
    Missing,
    SkillOnly,
    ManualOnly,
}

impl SlashCommandInstallStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Stale => "stale",
            Self::Modified => "modified",
            Self::Missing => "missing",
            Self::SkillOnly => "skill_only",
            Self::ManualOnly => "manual_only",
        }
    }
}

#[derive(Debug)]
struct InstallResult {
    agent: SlashCommandAgentArg,
    scope: Option<SlashCommandScope>,
    path: Option<PathBuf>,
    success: bool,
    previous_status: SlashCommandInstallStatus,
    status: SlashCommandInstallStatus,
    already_installed: bool,
    updated: bool,
    error: Option<String>,
    note: Option<String>,
}

impl InstallResult {
    fn to_json(&self) -> Value {
        json!({
            "agent": self.agent.id(),
            "agent_display_name": self.agent.display_name(),
            "scope": self.scope.map(SlashCommandScope::as_str),
            "path": self.path,
            "success": self.success,
            "previous_status": self.previous_status.as_str(),
            "status": self.status.as_str(),
            "already_installed": self.already_installed,
            "updated": self.updated,
            "error": self.error,
            "note": self.note,
        })
    }
}

#[derive(Debug)]
struct StatusResult {
    status: SlashCommandInstallStatus,
    metadata: Option<SlashCommandMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SlashCommandMetadata {
    schema_version: u32,
    installer: String,
    command_name: String,
    files: BTreeMap<String, String>,
    ctx_cli_version: String,
    installed_at: String,
}

impl SlashCommandMetadata {
    fn current(target: &CommandFileTarget) -> Self {
        Self {
            schema_version: 1,
            installer: "ctx-cli".to_owned(),
            command_name: COMMAND_NAME.to_owned(),
            files: BTreeMap::from([(target.filename.clone(), target.bundled_hash())]),
            ctx_cli_version: env!("CARGO_PKG_VERSION").to_owned(),
            installed_at: utc_now().to_rfc3339(),
        }
    }
}

pub(crate) fn run_install(
    args: SlashCommandInstallArgs,
    context: &PathContext,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let agents = selected_agents(&args, context);
    analytics::insert_count_bucket(
        analytics_properties,
        "slash_command_target_agents_count_bucket",
        agents.len() as u64,
    );
    let mut results = Vec::with_capacity(agents.len());
    for agent in agents {
        let plan = agent.install_plan(args.project, context);
        results.push(install_plan(plan, args.force)?);
    }
    let failed = results.iter().filter(|result| !result.success).count();
    let already_installed = !results.is_empty()
        && results.iter().all(|result| {
            result.already_installed
                || matches!(
                    result.status,
                    SlashCommandInstallStatus::SkillOnly | SlashCommandInstallStatus::ManualOnly
                )
        });
    let updated = results.iter().any(|result| result.updated);
    analytics::insert_str(
        analytics_properties,
        "install_result",
        if failed == 0 { "ok" } else { "partial_error" },
    );
    analytics::insert_bool(analytics_properties, "already_installed", already_installed);
    analytics::insert_bool(analytics_properties, "updated", updated);
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "integration": "slash-commands",
                "command": COMMAND_NAME,
                "scope": if args.project { "project" } else { "global" },
                "results": results.iter().map(InstallResult::to_json).collect::<Vec<_>>(),
            }))?
        );
    } else {
        print_install_results(&results);
    }
    if failed > 0 {
        return Err(anyhow!(
            "failed to install slash commands for {failed} target(s)"
        ));
    }
    Ok(())
}

fn selected_agents(
    args: &SlashCommandInstallArgs,
    context: &PathContext,
) -> Vec<SlashCommandAgentArg> {
    if args.all_agents {
        return SlashCommandAgentArg::ALL.to_vec();
    }
    if !args.agent.is_empty() {
        return dedupe_agents(args.agent.iter().copied());
    }
    SlashCommandAgentArg::WRITABLE
        .iter()
        .copied()
        .filter(|agent| agent.detected(context))
        .collect()
}

fn dedupe_agents(
    agents: impl IntoIterator<Item = SlashCommandAgentArg>,
) -> Vec<SlashCommandAgentArg> {
    let mut deduped = Vec::new();
    for agent in agents {
        if !deduped.contains(&agent) {
            deduped.push(agent);
        }
    }
    deduped
}

fn install_plan(plan: SlashCommandPlan, force: bool) -> Result<InstallResult> {
    match plan {
        SlashCommandPlan::File(target) => install_file_target(&target, force),
        SlashCommandPlan::SkillOnly { agent, note } => Ok(InstallResult {
            agent,
            scope: None,
            path: None,
            success: true,
            previous_status: SlashCommandInstallStatus::SkillOnly,
            status: SlashCommandInstallStatus::SkillOnly,
            already_installed: true,
            updated: false,
            error: None,
            note: Some(note.replace("<agent>", agent.id())),
        }),
        SlashCommandPlan::ManualOnly { agent, note } => Ok(InstallResult {
            agent,
            scope: None,
            path: None,
            success: true,
            previous_status: SlashCommandInstallStatus::ManualOnly,
            status: SlashCommandInstallStatus::ManualOnly,
            already_installed: true,
            updated: false,
            error: None,
            note: Some(note.to_owned()),
        }),
    }
}

fn install_file_target(target: &CommandFileTarget, force: bool) -> Result<InstallResult> {
    let previous = status_file_target(target)?;
    if previous.status == SlashCommandInstallStatus::Current {
        if !metadata_is_current(target, previous.metadata.as_ref()) {
            write_metadata(target)?;
        }
        return Ok(InstallResult {
            agent: target.agent,
            scope: Some(target.scope),
            path: Some(target.command_path()),
            success: true,
            previous_status: previous.status,
            status: SlashCommandInstallStatus::Current,
            already_installed: true,
            updated: false,
            error: None,
            note: None,
        });
    }
    if previous.status == SlashCommandInstallStatus::Modified && !force {
        return Ok(InstallResult {
            agent: target.agent,
            scope: Some(target.scope),
            path: Some(target.command_path()),
            success: false,
            previous_status: previous.status,
            status: previous.status,
            already_installed: false,
            updated: false,
            error: Some("local command edits detected; rerun with --force to overwrite".to_owned()),
            note: None,
        });
    }
    write_command_file(target)?;
    Ok(InstallResult {
        agent: target.agent,
        scope: Some(target.scope),
        path: Some(target.command_path()),
        success: true,
        previous_status: previous.status,
        status: SlashCommandInstallStatus::Current,
        already_installed: false,
        updated: matches!(
            previous.status,
            SlashCommandInstallStatus::Stale | SlashCommandInstallStatus::Modified
        ),
        error: None,
        note: None,
    })
}

fn status_file_target(target: &CommandFileTarget) -> Result<StatusResult> {
    ensure_path_inside(&target.base_dir, &target.command_path())?;
    let command_path = target.command_path();
    let metadata = read_metadata(&target.base_dir);
    let installed_hash = match fs::symlink_metadata(&command_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_dir() => None,
        Ok(_) => {
            let body = fs::read(&command_path)
                .with_context(|| format!("read {}", command_path.display()))?;
            Some(sha256_hex(&body))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err).with_context(|| format!("read {}", command_path.display())),
    };
    let status = match installed_hash.as_deref() {
        None if command_path.exists() => SlashCommandInstallStatus::Modified,
        None => SlashCommandInstallStatus::Missing,
        Some(hash) if hash == target.bundled_hash() => SlashCommandInstallStatus::Current,
        Some(hash) => match metadata
            .as_ref()
            .and_then(|metadata| metadata.files.get(&target.filename))
        {
            Some(metadata_hash) if metadata_hash == hash => SlashCommandInstallStatus::Stale,
            _ => SlashCommandInstallStatus::Modified,
        },
    };
    Ok(StatusResult { status, metadata })
}

fn write_command_file(target: &CommandFileTarget) -> Result<()> {
    ensure_path_inside(&target.base_dir, &target.command_path())?;
    if let Ok(metadata) = fs::symlink_metadata(target.command_path()) {
        if metadata.file_type().is_symlink() || metadata.is_dir() {
            return Err(anyhow!(
                "refusing to overwrite non-regular command path: {}",
                target.command_path().display()
            ));
        }
    }
    fs::create_dir_all(&target.base_dir)
        .with_context(|| format!("create {}", target.base_dir.display()))?;
    fs::write(target.command_path(), &target.body)
        .with_context(|| format!("write {}", target.command_path().display()))?;
    write_metadata(target)
}

fn write_metadata(target: &CommandFileTarget) -> Result<()> {
    fs::create_dir_all(&target.base_dir)
        .with_context(|| format!("create {}", target.base_dir.display()))?;
    let metadata = serde_json::to_vec_pretty(&SlashCommandMetadata::current(target))?;
    fs::write(target.base_dir.join(METADATA_FILE), metadata)
        .with_context(|| format!("write {}", target.base_dir.join(METADATA_FILE).display()))
}

fn read_metadata(base_dir: &Path) -> Option<SlashCommandMetadata> {
    let path = base_dir.join(METADATA_FILE);
    let body = fs::read(path).ok()?;
    serde_json::from_slice(&body).ok()
}

fn metadata_is_current(
    target: &CommandFileTarget,
    metadata: Option<&SlashCommandMetadata>,
) -> bool {
    metadata.is_some_and(|metadata| {
        metadata.schema_version == 1
            && metadata.installer == "ctx-cli"
            && metadata.command_name == COMMAND_NAME
            && metadata
                .files
                .get(&target.filename)
                .is_some_and(|hash| hash == &target.bundled_hash())
    })
}

fn print_install_results(results: &[InstallResult]) {
    if results.is_empty() {
        println!("No separate slash-command targets detected");
        println!(
            "Use --agent opencode, --agent mimocode, --agent gemini-cli, --agent qwen-code, or --agent windsurf to install explicitly."
        );
        println!("For skill-based agents, run `ctx integrations install skills`.");
        return;
    }
    println!("ctx slash commands: /{COMMAND_NAME}");
    for result in results {
        let verb = if result.already_installed {
            match result.status {
                SlashCommandInstallStatus::SkillOnly => "skill-only",
                SlashCommandInstallStatus::ManualOnly => "manual",
                _ => "current",
            }
        } else if !result.success {
            "skipped"
        } else if result.updated {
            "updated"
        } else {
            "installed"
        };
        let path = result
            .path
            .as_ref()
            .map(|path| format!(" -> {}", path.display()))
            .unwrap_or_default();
        let detail = result
            .error
            .as_deref()
            .or(result.note.as_deref())
            .map(|message| format!(" - {message}"))
            .unwrap_or_default();
        println!("  {verb}: {}{path}{detail}", result.agent.display_name());
    }
}

fn opencode_command_body() -> String {
    format!(
        "---\ndescription: Search local agent history with ctx\nargument-hint: [question or topic]\n---\n\n{COMMAND_INSTRUCTIONS}"
    )
}

fn gemini_command_body() -> String {
    let prompt = COMMAND_INSTRUCTIONS.replace("$ARGUMENTS", "{{args}}");
    format!(
        "description = \"{}\"\nprompt = '''\n{}'''\n",
        toml_basic_string("Search local agent history with ctx"),
        prompt
    )
}

fn qwen_command_body() -> String {
    let prompt = COMMAND_INSTRUCTIONS.replace("$ARGUMENTS", "{{args}}");
    format!("---\ndescription: Search local agent history with ctx\n---\n\n{prompt}")
}

fn toml_basic_string(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| match ch {
            '\\' => "\\\\".chars().collect::<Vec<_>>(),
            '"' => "\\\"".chars().collect::<Vec<_>>(),
            '\n' => "\\n".chars().collect::<Vec<_>>(),
            '\r' => "\\r".chars().collect::<Vec<_>>(),
            '\t' => "\\t".chars().collect::<Vec<_>>(),
            other => vec![other],
        })
        .collect()
}

fn ensure_path_inside(base: &Path, target: &Path) -> Result<()> {
    if has_parent_component(base) || has_parent_component(target) {
        return Err(anyhow!("slash command path contains parent traversal"));
    }
    if !target.starts_with(base) {
        return Err(anyhow!("slash command path escapes target directory"));
    }
    Ok(())
}

fn has_parent_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

fn sha256_hex(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    format!("sha256:{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn selected_agents_default_to_detected_file_based_targets() {
        let temp = tempdir().unwrap();
        let xdg = temp.path().join("xdg");
        fs::create_dir_all(xdg.join("opencode")).unwrap();
        fs::create_dir_all(xdg.join("mimocode")).unwrap();
        let context = PathContext::for_tests(temp.path().to_owned(), temp.path().to_owned())
            .with_xdg_config_home(xdg);
        let args = SlashCommandInstallArgs {
            agent: Vec::new(),
            all_agents: false,
            project: false,
            json: true,
            force: false,
        };

        assert_eq!(
            selected_agents(&args, &context),
            vec![
                SlashCommandAgentArg::OpenCode,
                SlashCommandAgentArg::MiMoCode
            ]
        );
    }

    #[test]
    fn opencode_install_is_idempotent_and_refreshes_stale_owned_file() {
        let temp = tempdir().unwrap();
        let xdg = temp.path().join("xdg");
        let context = PathContext::for_tests(temp.path().to_owned(), temp.path().to_owned())
            .with_xdg_config_home(xdg.clone());
        let target = match SlashCommandAgentArg::OpenCode.install_plan(false, &context) {
            SlashCommandPlan::File(target) => target,
            _ => panic!("expected file target"),
        };

        let first = install_file_target(&target, false).unwrap();
        assert_eq!(first.previous_status, SlashCommandInstallStatus::Missing);
        assert!(!first.already_installed);
        assert!(xdg
            .join("opencode")
            .join("commands")
            .join("ctx-history.md")
            .exists());

        let second = install_file_target(&target, false).unwrap();
        assert_eq!(second.previous_status, SlashCommandInstallStatus::Current);
        assert!(second.already_installed);

        let old_body = "---\ndescription: old\n---\n\nold\n";
        fs::write(target.command_path(), old_body).unwrap();
        let mut metadata = SlashCommandMetadata::current(&target);
        metadata
            .files
            .insert(target.filename.clone(), sha256_hex(old_body.as_bytes()));
        fs::write(
            target.base_dir.join(METADATA_FILE),
            serde_json::to_vec_pretty(&metadata).unwrap(),
        )
        .unwrap();

        let refreshed = install_file_target(&target, false).unwrap();
        assert_eq!(refreshed.previous_status, SlashCommandInstallStatus::Stale);
        assert!(refreshed.updated);
        assert!(fs::read_to_string(target.command_path())
            .unwrap()
            .contains("Search local agent history with ctx"));
    }

    #[test]
    fn modified_command_requires_force() {
        let temp = tempdir().unwrap();
        let context = PathContext::for_tests(temp.path().to_owned(), temp.path().to_owned());
        let target = match SlashCommandAgentArg::GeminiCli.install_plan(true, &context) {
            SlashCommandPlan::File(target) => target,
            _ => panic!("expected file target"),
        };
        fs::create_dir_all(&target.base_dir).unwrap();
        fs::write(target.command_path(), "prompt = 'local'\n").unwrap();

        let skipped = install_file_target(&target, false).unwrap();
        assert!(!skipped.success);
        assert_eq!(skipped.previous_status, SlashCommandInstallStatus::Modified);
        assert!(fs::read_to_string(target.command_path())
            .unwrap()
            .contains("local"));

        let forced = install_file_target(&target, true).unwrap();
        assert!(forced.success);
        assert_eq!(forced.previous_status, SlashCommandInstallStatus::Modified);
        assert!(fs::read_to_string(target.command_path())
            .unwrap()
            .contains("{{args}}"));
    }

    #[test]
    fn skill_only_agents_do_not_write_codex_prompts() {
        let temp = tempdir().unwrap();
        let context = PathContext::for_tests(temp.path().to_owned(), temp.path().to_owned());
        let result = install_plan(
            SlashCommandAgentArg::Codex.install_plan(false, &context),
            false,
        )
        .unwrap();

        assert_eq!(result.status, SlashCommandInstallStatus::SkillOnly);
        assert!(!temp.path().join(".codex").join("prompts").exists());
    }
}
