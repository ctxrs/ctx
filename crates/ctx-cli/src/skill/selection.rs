use super::*;

#[derive(Debug, Clone)]
pub(super) struct PathContext {
    pub(super) home: PathBuf,
    pub(super) xdg_config_home: PathBuf,
    pub(super) cwd: PathBuf,
    pub(super) env_overrides: BTreeMap<String, PathBuf>,
}

impl PathContext {
    pub(super) fn from_env() -> Result<Self> {
        let home = home_dir().context("resolve home directory")?;
        let xdg_config_home =
            non_empty_env_path("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        let mut env_overrides = BTreeMap::new();
        for key in ["CODEX_HOME", "CLAUDE_CONFIG_DIR"] {
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
    pub(super) fn for_tests(home: PathBuf, cwd: PathBuf) -> Self {
        Self {
            xdg_config_home: home.join(".config"),
            home,
            cwd,
            env_overrides: BTreeMap::new(),
        }
    }

    #[cfg(test)]
    pub(super) fn with_env_override(mut self, key: &str, value: PathBuf) -> Self {
        self.env_overrides.insert(key.to_owned(), value);
        self
    }

    #[cfg(test)]
    pub(super) fn with_xdg_config_home(mut self, value: PathBuf) -> Self {
        self.xdg_config_home = value;
        self
    }

    pub(super) fn env_or_home_child(&self, key: &str, fallback_child: &str) -> PathBuf {
        self.env_overrides
            .get(key)
            .cloned()
            .unwrap_or_else(|| self.home.join(fallback_child))
    }

    fn agent_detected(&self, agent: SkillAgentArg) -> bool {
        if agent == SkillAgentArg::Codex
            && !self.env_overrides.contains_key("CODEX_HOME")
            && Path::new("/etc/codex").exists()
        {
            return true;
        }
        agent.detect_dir(self).is_some_and(|path| path.exists())
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

#[derive(Debug, Clone)]
pub(super) struct SkillTarget {
    pub(super) agent: SkillAgentArg,
    pub(super) scope: SkillScope,
    pub(super) base_dir: PathBuf,
    pub(super) skill_dir: PathBuf,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum SkillScope {
    Global,
    Project,
}

impl SkillScope {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

#[cfg(test)]
pub(super) fn explicit_selected_agents(
    agents: &[SkillAgentArg],
    all_agents: bool,
) -> Option<Vec<SkillAgentArg>> {
    if all_agents {
        Some(SkillAgentArg::ALL.to_vec())
    } else if agents.is_empty() {
        None
    } else {
        Some(dedupe_agents(agents.iter().copied()))
    }
}

pub(super) fn dedupe_agents(agents: impl IntoIterator<Item = SkillAgentArg>) -> Vec<SkillAgentArg> {
    let mut deduped = Vec::new();
    for agent in agents {
        if !deduped.contains(&agent) {
            deduped.push(agent);
        }
    }
    deduped
}

pub(super) fn detected_agents(context: &PathContext) -> Vec<SkillAgentArg> {
    picker_agents()
        .iter()
        .copied()
        .filter(|agent| context.agent_detected(*agent))
        .collect()
}

pub(super) fn detected_agent_specific_agents(context: &PathContext) -> Vec<SkillAgentArg> {
    detected_agents(context)
        .into_iter()
        .filter(|agent| agent.needs_agent_specific_default())
        .collect()
}

pub(super) fn default_noninteractive_agents(
    context: &PathContext,
) -> (Vec<SkillAgentArg>, SkillSelectionSource) {
    let mut agents = vec![SkillAgentArg::Universal];
    let detected_specific = detected_agent_specific_agents(context);
    let source = if detected_specific.is_empty() {
        SkillSelectionSource::Fallback
    } else {
        agents.extend(detected_specific);
        SkillSelectionSource::Detected
    };
    (agents, source)
}

pub(super) fn default_picker_agents(context: &PathContext) -> Vec<SkillAgentArg> {
    let (agents, _) = default_noninteractive_agents(context);
    agents
}

pub(super) fn picker_agents() -> &'static [SkillAgentArg] {
    &[
        SkillAgentArg::Universal,
        SkillAgentArg::ClaudeCode,
        SkillAgentArg::Codex,
        SkillAgentArg::Cursor,
        SkillAgentArg::OpenCode,
        SkillAgentArg::GeminiCli,
        SkillAgentArg::Antigravity,
        SkillAgentArg::AntigravityCli,
        SkillAgentArg::GitHubCopilot,
        SkillAgentArg::Pi,
        SkillAgentArg::Goose,
        SkillAgentArg::Amp,
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SkillSelectionSource {
    Explicit,
    All,
    Picker,
    Detected,
    Fallback,
}

impl SkillSelectionSource {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::All => "all",
            Self::Picker => "picker",
            Self::Detected => "detected",
            Self::Fallback => "fallback",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct SkillAgentSelection {
    pub(super) agents: Vec<SkillAgentArg>,
    pub(super) source: SkillSelectionSource,
}

pub(super) fn install_agent_selection(
    args: &SkillInstallArgs,
    context: &PathContext,
) -> Result<SkillAgentSelection> {
    if args.all_agents {
        return Ok(SkillAgentSelection {
            agents: SkillAgentArg::ALL.to_vec(),
            source: SkillSelectionSource::All,
        });
    }
    if !args.agent.is_empty() {
        return Ok(SkillAgentSelection {
            agents: dedupe_agents(args.agent.iter().copied()),
            source: SkillSelectionSource::Explicit,
        });
    }
    if args.json || !can_prompt() {
        let (agents, source) = default_noninteractive_agents(context);
        return Ok(SkillAgentSelection { agents, source });
    }
    let agents = prompt_for_agents(context)?;
    Ok(SkillAgentSelection {
        agents,
        source: SkillSelectionSource::Picker,
    })
}

pub(super) fn status_agent_selection(
    args: &SkillStatusArgs,
    context: &PathContext,
) -> SkillAgentSelection {
    if args.all_agents {
        return SkillAgentSelection {
            agents: SkillAgentArg::ALL.to_vec(),
            source: SkillSelectionSource::All,
        };
    }
    if !args.agent.is_empty() {
        return SkillAgentSelection {
            agents: dedupe_agents(args.agent.iter().copied()),
            source: SkillSelectionSource::Explicit,
        };
    }
    let (agents, source) = default_noninteractive_agents(context);
    SkillAgentSelection { agents, source }
}

pub(super) fn can_prompt() -> bool {
    io::stdin().is_terminal() && io::stderr().is_terminal()
}

pub(super) fn prompt_for_agents(context: &PathContext) -> Result<Vec<SkillAgentArg>> {
    let options = picker_agents();
    let detected = detected_agents(context);
    let defaults = default_picker_agents(context);
    let mut stderr = io::stderr();
    writeln!(
        stderr,
        "Select where to install {BUNDLED_SKILL_NAME}. Detected agents are preselected."
    )?;
    writeln!(
        stderr,
        "Press Enter for the marked defaults, or enter numbers like 1,2."
    )?;
    for (index, agent) in options.iter().enumerate() {
        let marker = if defaults.contains(agent) { "*" } else { " " };
        let detected_hint = if detected.contains(agent) {
            " detected"
        } else {
            ""
        };
        let target = single_target(*agent, false, context)?;
        writeln!(
            stderr,
            "  {}. [{}] {} -> {}{}",
            index + 1,
            marker,
            agent.display_name(),
            target.skill_dir.display(),
            detected_hint
        )?;
    }
    loop {
        write!(stderr, "Install target(s): ")?;
        stderr.flush()?;
        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .context("read skill install selection")?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(defaults);
        }
        if matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "q" | "quit" | "cancel"
        ) {
            return Err(anyhow!("skill install canceled"));
        }
        match parse_picker_selection(trimmed, options) {
            Ok(agents) => return Ok(agents),
            Err(err) => {
                writeln!(stderr, "{err}")?;
            }
        }
    }
}

pub(super) fn parse_picker_selection(
    input: &str,
    options: &[SkillAgentArg],
) -> Result<Vec<SkillAgentArg>> {
    let input = input.trim();
    if input.eq_ignore_ascii_case("all") {
        return Ok(options.to_vec());
    }
    let mut selected = Vec::new();
    for raw in input
        .split([',', ' ', '\t'])
        .filter(|part| !part.trim().is_empty())
    {
        let token = raw.trim();
        let agent = if let Ok(index) = token.parse::<usize>() {
            options
                .get(index.saturating_sub(1))
                .copied()
                .ok_or_else(|| anyhow!("invalid selection {token}: choose 1-{}", options.len()))?
        } else {
            agent_from_name(token).ok_or_else(|| anyhow!("unknown agent: {token}"))?
        };
        if !selected.contains(&agent) {
            selected.push(agent);
        }
    }
    if selected.is_empty() {
        return Err(anyhow!("choose at least one install target"));
    }
    Ok(selected)
}

pub(super) fn agent_from_name(value: &str) -> Option<SkillAgentArg> {
    match value.to_ascii_lowercase().as_str() {
        "universal" | "agents" | ".agents" => Some(SkillAgentArg::Universal),
        "codex" => Some(SkillAgentArg::Codex),
        "claude" | "claude-code" | "claudecode" => Some(SkillAgentArg::ClaudeCode),
        "cursor" => Some(SkillAgentArg::Cursor),
        "opencode" | "open-code" => Some(SkillAgentArg::OpenCode),
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

pub(super) fn single_target(
    agent: SkillAgentArg,
    project: bool,
    context: &PathContext,
) -> Result<SkillTarget> {
    let skill_name = sanitize_skill_name(BUNDLED_SKILL_NAME)?;
    let (scope, base_dir) = if project {
        (
            SkillScope::Project,
            context.cwd.join(agent.project_skills_dir()),
        )
    } else {
        (SkillScope::Global, agent.global_skills_dir(context))
    };
    let skill_dir = base_dir.join(&skill_name);
    ensure_path_inside(&base_dir, &skill_dir)
        .with_context(|| format!("resolve {} skill path", agent.id()))?;
    Ok(SkillTarget {
        agent,
        scope,
        base_dir,
        skill_dir,
    })
}

#[cfg(test)]
pub(super) fn resolve_targets(
    agents: &[SkillAgentArg],
    all_agents: bool,
    project: bool,
    context: &PathContext,
) -> Result<Vec<SkillTarget>> {
    let selected = explicit_selected_agents(agents, all_agents)
        .unwrap_or_else(|| vec![SkillAgentArg::Universal]);
    resolve_targets_for_agents(&selected, project, context)
}

pub(super) fn resolve_targets_for_agents(
    agents: &[SkillAgentArg],
    project: bool,
    context: &PathContext,
) -> Result<Vec<SkillTarget>> {
    agents
        .iter()
        .copied()
        .map(|agent| single_target(agent, project, context))
        .collect()
}
