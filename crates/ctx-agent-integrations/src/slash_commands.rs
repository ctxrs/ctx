use std::{
    collections::BTreeMap,
    env, fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use ctx_history_core::utc_now;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::filesystem::{atomic_remove_if_unchanged, atomic_update};

mod lifecycle;

pub use lifecycle::{
    execute_remove, execute_status, SlashCommandRemoveReceipt, SlashCommandRemoveRequest,
    SlashCommandRemoveResult, SlashCommandStatusReceipt, SlashCommandStatusRequest,
    SlashCommandStatusResult,
};

pub const COMMAND_NAME: &str = "ctx";
const LEGACY_COMMAND_NAME: &str = "ctx-history";
const METADATA_FILE: &str = ".ctx-slash-commands.json";

const COMMAND_INSTRUCTIONS: &str = r#"# ctx

Use ctx to search coding-agent history or trace code to its original agent
session for this request.

User request: $ARGUMENTS

Choose local history search or ctx pro blame based on the request. Inspect cited
events or sessions before making claims, preserve the distinction between Core
and paid pro capabilities, and return a concise answer grounded in ctx citations.
Prefer default text output for agent reading; use `--format json` only for
scripts or exact machine-readable fields.
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SlashCommandAgent {
    Codex,
    GrokBuild,
    ClaudeCode,
    Cursor,
    OpenCode,
    MiMoCode,
    GeminiCli,
    QwenCode,
    Antigravity,
    GitHubCopilot,
    Pi,
    Goose,
    Continue,
}

impl SlashCommandAgent {
    pub const ALL: &'static [Self] = &[
        Self::Codex,
        Self::GrokBuild,
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
    ];

    const WRITABLE: &'static [Self] = &[
        Self::OpenCode,
        Self::MiMoCode,
        Self::GeminiCli,
        Self::QwenCode,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::GrokBuild => "grok-build",
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
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::GrokBuild => "Grok Build",
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
        }
    }

    fn root_detected(self, project: bool, context: &PathContext) -> bool {
        if self == Self::MiMoCode
            && !project
            && (context.mimocode_home.is_some() || context.mimocode_config_dir.is_some())
        {
            return true;
        }
        let root = match self {
            Self::OpenCode if project => Some(context.cwd.join(".opencode")),
            Self::OpenCode => Some(context.xdg_config_home.join("opencode")),
            Self::MiMoCode if project => Some(context.cwd.join(".mimocode")),
            Self::MiMoCode => Some(context.mimocode_config_dir()),
            Self::GeminiCli if project => Some(context.cwd.join(".gemini")),
            Self::GeminiCli => Some(context.home.join(".gemini")),
            Self::QwenCode if project => Some(context.cwd.join(".qwen")),
            Self::QwenCode => Some(context.home.join(".qwen")),
            Self::Codex
            | Self::GrokBuild
            | Self::ClaudeCode
            | Self::Cursor
            | Self::Antigravity
            | Self::GitHubCopilot
            | Self::Pi
            | Self::Goose
            | Self::Continue => None,
        };
        root.is_some_and(|root| {
            fs::symlink_metadata(root).is_ok_and(|metadata| metadata.file_type().is_dir())
        })
    }

    fn detected(self, project: bool, context: &PathContext) -> bool {
        if self.root_detected(project, context) {
            return true;
        }
        let SlashCommandPlan::File(target) = self.install_plan(project, context) else {
            return false;
        };
        [target.command_path(), target.legacy_command_path()]
            .iter()
            .any(|path| safe_regular_file(path))
    }

    fn install_plan(self, project: bool, context: &PathContext) -> SlashCommandPlan {
        let file = |base_dir, filename, body| {
            SlashCommandPlan::File(CommandFileTarget {
                agent: self,
                scope: scope(project),
                base_dir,
                filename,
                body,
            })
        };
        match self {
            Self::OpenCode => file(
                if project {
                    context.cwd.join(".opencode").join("commands")
                } else {
                    context.xdg_config_home.join("opencode").join("commands")
                },
                format!("{COMMAND_NAME}.md"),
                opencode_command_body(),
            ),
            Self::MiMoCode => file(
                if project {
                    context.cwd.join(".mimocode").join("commands")
                } else {
                    context.mimocode_config_dir().join("commands")
                },
                format!("{COMMAND_NAME}.md"),
                opencode_command_body(),
            ),
            Self::GeminiCli => file(
                if project {
                    context.cwd.join(".gemini").join("commands")
                } else {
                    context.home.join(".gemini").join("commands")
                },
                format!("{COMMAND_NAME}.toml"),
                gemini_command_body(),
            ),
            Self::QwenCode => file(
                if project {
                    context.cwd.join(".qwen").join("commands")
                } else {
                    context.home.join(".qwen").join("commands")
                },
                format!("{COMMAND_NAME}.md"),
                qwen_command_body(),
            ),
            Self::Codex
            | Self::GrokBuild
            | Self::ClaudeCode
            | Self::Cursor
            | Self::Antigravity => {
                SlashCommandPlan::SkillOnly {
                    agent: self,
                note: "slash-style invocation is covered by Agent Skills; run `ctx integrations install skill --agent <agent>`",
                }
            }
            Self::GitHubCopilot | Self::Pi => SlashCommandPlan::SkillOnly {
                agent: self,
                note: "ctx supports this provider through the bundled Agent Skill; run `ctx integrations install skill --agent <agent>`",
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

#[derive(Debug, Clone)]
pub struct PathContext {
    home: PathBuf,
    xdg_config_home: PathBuf,
    cwd: PathBuf,
    mimocode_home: Option<PathBuf>,
    mimocode_config_dir: Option<PathBuf>,
}

impl PathContext {
    pub fn from_env() -> Result<Self> {
        let home = home_dir().context("resolve home directory")?;
        let xdg_config_home =
            non_empty_env_path("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        Ok(Self {
            home,
            xdg_config_home,
            cwd: env::current_dir().context("resolve current directory")?,
            mimocode_home: non_empty_absolute_env_path("MIMOCODE_HOME")?,
            mimocode_config_dir: non_empty_env_path("MIMOCODE_CONFIG_DIR"),
        })
    }

    pub fn for_tests(home: PathBuf, cwd: PathBuf) -> Self {
        Self {
            xdg_config_home: home.join(".config"),
            home,
            cwd,
            mimocode_home: None,
            mimocode_config_dir: None,
        }
    }

    pub fn with_xdg_config_home(mut self, value: PathBuf) -> Self {
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

#[derive(Debug, Clone)]
pub struct SlashCommandInstallRequest {
    pub agents: Vec<SlashCommandAgent>,
    pub all_agents: bool,
    pub project: bool,
    pub force: bool,
    pub product_version: String,
}

#[derive(Debug)]
pub struct SlashCommandInstallReceipt {
    pub project: bool,
    pub results: Vec<SlashCommandInstallResult>,
    pub failed: usize,
    pub already_installed: bool,
    pub updated: bool,
    pub modified_targets: usize,
}

#[derive(Debug, Clone, Copy)]
pub enum SlashCommandScope {
    Global,
    Project,
}

impl SlashCommandScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashCommandInstallStatus {
    Current,
    Stale,
    Modified,
    Missing,
    SkillOnly,
    ManualOnly,
}

impl SlashCommandInstallStatus {
    pub const fn as_str(self) -> &'static str {
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
pub struct SlashCommandInstallResult {
    pub agent: SlashCommandAgent,
    pub scope: Option<SlashCommandScope>,
    pub path: Option<PathBuf>,
    pub success: bool,
    pub previous_status: SlashCommandInstallStatus,
    pub status: SlashCommandInstallStatus,
    pub already_installed: bool,
    pub updated: bool,
    pub migrated: bool,
    pub legacy_path: Option<PathBuf>,
    pub error: Option<String>,
    pub note: Option<String>,
}

pub fn execute_install(
    request: SlashCommandInstallRequest,
    context: &PathContext,
) -> Result<SlashCommandInstallReceipt> {
    let agents = selected_agents(
        &request.agents,
        request.all_agents,
        request.project,
        context,
    );
    let results = agents
        .into_iter()
        .map(|agent| {
            install_plan(
                agent.install_plan(request.project, context),
                request.force,
                &request.product_version,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let failed = results.iter().filter(|result| !result.success).count();
    let already_installed = !results.is_empty()
        && results.iter().all(|result| {
            result.already_installed
                || matches!(
                    result.status,
                    SlashCommandInstallStatus::SkillOnly | SlashCommandInstallStatus::ManualOnly
                )
        });
    Ok(SlashCommandInstallReceipt {
        project: request.project,
        failed,
        already_installed,
        updated: results.iter().any(|result| result.updated),
        modified_targets: results.iter().filter(|result| result.updated).count(),
        results,
    })
}

fn selected_agents(
    agents: &[SlashCommandAgent],
    all_agents: bool,
    project: bool,
    context: &PathContext,
) -> Vec<SlashCommandAgent> {
    if all_agents {
        return SlashCommandAgent::ALL.to_vec();
    }
    if !agents.is_empty() {
        return dedupe_agents(agents.iter().copied());
    }
    SlashCommandAgent::WRITABLE
        .iter()
        .copied()
        .filter(|agent| agent.detected(project, context))
        .collect()
}

fn dedupe_agents(agents: impl IntoIterator<Item = SlashCommandAgent>) -> Vec<SlashCommandAgent> {
    let mut deduped = Vec::new();
    for agent in agents {
        if !deduped.contains(&agent) {
            deduped.push(agent);
        }
    }
    deduped
}

#[derive(Debug, Clone)]
enum SlashCommandPlan {
    File(CommandFileTarget),
    SkillOnly {
        agent: SlashCommandAgent,
        note: &'static str,
    },
    ManualOnly {
        agent: SlashCommandAgent,
        note: &'static str,
    },
}

#[derive(Debug, Clone)]
struct CommandFileTarget {
    agent: SlashCommandAgent,
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

    fn legacy_filename(&self) -> String {
        let suffix = self
            .filename
            .strip_prefix(COMMAND_NAME)
            .expect("managed command filename starts with the command name");
        format!("{LEGACY_COMMAND_NAME}{suffix}")
    }

    fn legacy_command_path(&self) -> PathBuf {
        self.base_dir.join(self.legacy_filename())
    }
}

#[derive(Debug)]
struct StatusResult {
    status: SlashCommandInstallStatus,
    metadata: Option<SlashCommandMetadata>,
    installed_body: Option<Vec<u8>>,
}

#[derive(Debug)]
struct LegacyStatusResult {
    path: PathBuf,
    status: SlashCommandInstallStatus,
    body: Option<Vec<u8>>,
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
    fn current(target: &CommandFileTarget, product_version: &str) -> Self {
        Self {
            schema_version: 1,
            installer: "ctx-cli".to_owned(),
            command_name: COMMAND_NAME.to_owned(),
            files: BTreeMap::from([(target.filename.clone(), target.bundled_hash())]),
            ctx_cli_version: product_version.to_owned(),
            installed_at: utc_now().to_rfc3339(),
        }
    }
}

fn install_plan(
    plan: SlashCommandPlan,
    force: bool,
    product_version: &str,
) -> Result<SlashCommandInstallResult> {
    match plan {
        SlashCommandPlan::File(target) => install_file_target(&target, force, product_version),
        SlashCommandPlan::SkillOnly { agent, note } => Ok(SlashCommandInstallResult {
            agent,
            scope: None,
            path: None,
            success: true,
            previous_status: SlashCommandInstallStatus::SkillOnly,
            status: SlashCommandInstallStatus::SkillOnly,
            already_installed: true,
            updated: false,
            migrated: false,
            legacy_path: None,
            error: None,
            note: Some(note.replace("<agent>", agent.id())),
        }),
        SlashCommandPlan::ManualOnly { agent, note } => Ok(SlashCommandInstallResult {
            agent,
            scope: None,
            path: None,
            success: true,
            previous_status: SlashCommandInstallStatus::ManualOnly,
            status: SlashCommandInstallStatus::ManualOnly,
            already_installed: true,
            updated: false,
            migrated: false,
            legacy_path: None,
            error: None,
            note: Some(note.to_owned()),
        }),
    }
}

fn install_file_target(
    target: &CommandFileTarget,
    force: bool,
    product_version: &str,
) -> Result<SlashCommandInstallResult> {
    let previous = status_file_target(target)?;
    let legacy = status_legacy_file_target(target)?;
    let effective_previous_status = combined_file_status(previous.status, legacy.as_ref());
    if legacy
        .as_ref()
        .is_some_and(|legacy| legacy.status == SlashCommandInstallStatus::Modified)
        && !force
    {
        return Ok(SlashCommandInstallResult {
            agent: target.agent,
            scope: Some(target.scope),
            path: Some(target.command_path()),
            success: false,
            previous_status: effective_previous_status,
            status: SlashCommandInstallStatus::Modified,
            already_installed: false,
            updated: false,
            migrated: false,
            legacy_path: legacy.map(|legacy| legacy.path),
            error: Some(
                "local edits detected in the legacy /ctx-history command; rerun with --force to replace it with /ctx"
                    .to_owned(),
            ),
            note: None,
        });
    }
    if previous.status == SlashCommandInstallStatus::Current {
        let migrated = if let Some(legacy) = &legacy {
            remove_legacy_command_file(target, legacy)?;
            true
        } else {
            false
        };
        if !metadata_is_current(target, previous.metadata.as_ref()) {
            write_metadata(target, product_version)?;
        }
        return Ok(SlashCommandInstallResult {
            agent: target.agent,
            scope: Some(target.scope),
            path: Some(target.command_path()),
            success: true,
            previous_status: effective_previous_status,
            status: SlashCommandInstallStatus::Current,
            already_installed: !migrated,
            updated: migrated,
            migrated,
            legacy_path: legacy.map(|legacy| legacy.path),
            error: None,
            note: None,
        });
    }
    if previous.status == SlashCommandInstallStatus::Modified && !force {
        return Ok(SlashCommandInstallResult {
            agent: target.agent,
            scope: Some(target.scope),
            path: Some(target.command_path()),
            success: false,
            previous_status: previous.status,
            status: previous.status,
            already_installed: false,
            updated: false,
            migrated: false,
            legacy_path: legacy.map(|legacy| legacy.path),
            error: Some("local command edits detected; rerun with --force to overwrite".to_owned()),
            note: None,
        });
    }
    let migrated = if let Some(legacy) = &legacy {
        write_command_body(target)?;
        if let Err(cleanup) = remove_legacy_command_file(target, legacy) {
            if let Err(rollback) = rollback_command_body(target, previous.installed_body.as_deref())
            {
                return Err(anyhow!(
                    "{cleanup:#}; failed to roll back {} after migration cleanup failed: {rollback:#}",
                    target.command_path().display()
                ));
            }
            return Err(cleanup);
        }
        write_metadata(target, product_version)?;
        true
    } else {
        write_command_file(target, product_version)?;
        false
    };
    Ok(SlashCommandInstallResult {
        agent: target.agent,
        scope: Some(target.scope),
        path: Some(target.command_path()),
        success: true,
        previous_status: effective_previous_status,
        status: SlashCommandInstallStatus::Current,
        already_installed: false,
        updated: migrated
            || matches!(
                effective_previous_status,
                SlashCommandInstallStatus::Stale | SlashCommandInstallStatus::Modified
            ),
        migrated,
        legacy_path: legacy.map(|legacy| legacy.path),
        error: None,
        note: None,
    })
}

fn status_legacy_file_target(target: &CommandFileTarget) -> Result<Option<LegacyStatusResult>> {
    let path = target.legacy_command_path();
    ensure_path_inside(&target.base_dir, &path)?;
    let file_metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    if !file_metadata.file_type().is_file() {
        return Ok(Some(LegacyStatusResult {
            path,
            status: SlashCommandInstallStatus::Modified,
            body: None,
        }));
    }
    let body = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let hash = sha256_hex(&body);
    let metadata = read_metadata(&target.base_dir);
    let status = if legacy_metadata_manages_hash(target, metadata.as_ref(), &hash) {
        SlashCommandInstallStatus::Stale
    } else {
        SlashCommandInstallStatus::Modified
    };
    Ok(Some(LegacyStatusResult {
        path,
        status,
        body: Some(body),
    }))
}

fn remove_legacy_command_file(
    target: &CommandFileTarget,
    legacy: &LegacyStatusResult,
) -> Result<()> {
    ensure_path_inside(&target.base_dir, &legacy.path)?;
    let Some(body) = &legacy.body else {
        return Err(anyhow!(
            "legacy command path is not a regular file and was not removed: {}",
            legacy.path.display()
        ));
    };
    atomic_remove_if_unchanged(&legacy.path, body)
        .with_context(|| format!("remove legacy command {}", legacy.path.display()))?;
    Ok(())
}

fn status_file_target(target: &CommandFileTarget) -> Result<StatusResult> {
    ensure_path_inside(&target.base_dir, &target.command_path())?;
    let command_path = target.command_path();
    let metadata = read_metadata(&target.base_dir);
    let (command_exists, installed_body) = match fs::symlink_metadata(&command_path) {
        Ok(metadata) if !metadata.file_type().is_file() => (true, None),
        Ok(_) => (
            true,
            Some(
                fs::read(&command_path)
                    .with_context(|| format!("read {}", command_path.display()))?,
            ),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (false, None),
        Err(error) => {
            return Err(error).with_context(|| format!("read {}", command_path.display()))
        }
    };
    let installed_hash = installed_body.as_deref().map(sha256_hex);
    let status = match installed_hash.as_deref() {
        None if command_exists => SlashCommandInstallStatus::Modified,
        None => SlashCommandInstallStatus::Missing,
        Some(hash) if metadata_manages_hash(target, metadata.as_ref(), hash) => {
            if hash == target.bundled_hash() {
                SlashCommandInstallStatus::Current
            } else {
                SlashCommandInstallStatus::Stale
            }
        }
        Some(_) => SlashCommandInstallStatus::Modified,
    };
    Ok(StatusResult {
        status,
        metadata,
        installed_body,
    })
}

fn write_command_file(target: &CommandFileTarget, product_version: &str) -> Result<()> {
    write_command_body(target)?;
    write_metadata(target, product_version)
}

fn write_command_body(target: &CommandFileTarget) -> Result<()> {
    ensure_path_inside(&target.base_dir, &target.command_path())?;
    atomic_update(&target.command_path(), |_| {
        Ok(target.body.as_bytes().to_vec())
    })
    .with_context(|| format!("write {}", target.command_path().display()))
}

fn rollback_command_body(target: &CommandFileTarget, prior_body: Option<&[u8]>) -> Result<()> {
    let path = target.command_path();
    match prior_body {
        Some(prior_body) => atomic_update(&path, |existing| {
            if existing != Some(target.body.as_bytes()) {
                return Err(anyhow!(
                    "refusing to overwrite concurrently changed target {}",
                    path.display()
                ));
            }
            Ok(prior_body.to_vec())
        }),
        None => atomic_remove_if_unchanged(&path, target.body.as_bytes()).map(|_| ()),
    }
}

fn write_metadata(target: &CommandFileTarget, product_version: &str) -> Result<()> {
    let metadata =
        serde_json::to_vec_pretty(&SlashCommandMetadata::current(target, product_version))?;
    let path = target.base_dir.join(METADATA_FILE);
    atomic_update(&path, |_| Ok(metadata)).with_context(|| format!("write {}", path.display()))
}

fn read_metadata(base_dir: &Path) -> Option<SlashCommandMetadata> {
    let body = fs::read(base_dir.join(METADATA_FILE)).ok()?;
    serde_json::from_slice(&body).ok()
}

fn metadata_is_current(
    target: &CommandFileTarget,
    metadata: Option<&SlashCommandMetadata>,
) -> bool {
    let hash = target.bundled_hash();
    metadata_manages_hash(target, metadata, &hash)
}

fn metadata_manages_hash(
    target: &CommandFileTarget,
    metadata: Option<&SlashCommandMetadata>,
    hash: &str,
) -> bool {
    metadata.is_some_and(|metadata| {
        metadata.schema_version == 1
            && metadata.installer == "ctx-cli"
            && metadata.command_name == COMMAND_NAME
            && metadata
                .files
                .get(&target.filename)
                .is_some_and(|metadata_hash| metadata_hash == hash)
    })
}

fn legacy_metadata_manages_hash(
    target: &CommandFileTarget,
    metadata: Option<&SlashCommandMetadata>,
    hash: &str,
) -> bool {
    let legacy_filename = target.legacy_filename();
    metadata.is_some_and(|metadata| {
        metadata.schema_version == 1
            && metadata.installer == "ctx-cli"
            && metadata.command_name == LEGACY_COMMAND_NAME
            && metadata
                .files
                .get(&legacy_filename)
                .is_some_and(|metadata_hash| metadata_hash == hash)
    })
}

fn combined_file_status(
    current: SlashCommandInstallStatus,
    legacy: Option<&LegacyStatusResult>,
) -> SlashCommandInstallStatus {
    if current == SlashCommandInstallStatus::Modified
        || legacy.is_some_and(|legacy| legacy.status == SlashCommandInstallStatus::Modified)
    {
        SlashCommandInstallStatus::Modified
    } else if legacy.is_some() || current == SlashCommandInstallStatus::Stale {
        SlashCommandInstallStatus::Stale
    } else {
        current
    }
}

fn scope(project: bool) -> SlashCommandScope {
    if project {
        SlashCommandScope::Project
    } else {
        SlashCommandScope::Global
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

fn opencode_command_body() -> String {
    format!(
        "---\ndescription: Search agent history or trace code with ctx\nargument-hint: [question, topic, file, line, commit, or PR]\n---\n\n{COMMAND_INSTRUCTIONS}"
    )
}

fn gemini_command_body() -> String {
    let prompt = COMMAND_INSTRUCTIONS.replace("$ARGUMENTS", "{{args}}");
    format!(
        "description = \"{}\"\nprompt = '''\n{}'''\n",
        toml_basic_string("Search agent history or trace code with ctx"),
        prompt
    )
}

fn qwen_command_body() -> String {
    let prompt = COMMAND_INSTRUCTIONS.replace("$ARGUMENTS", "{{args}}");
    format!("---\ndescription: Search agent history or trace code with ctx\n---\n\n{prompt}")
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

fn safe_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
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
mod tests;
