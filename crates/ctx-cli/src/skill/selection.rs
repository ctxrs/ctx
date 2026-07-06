use std::io::{self, IsTerminal, Write};

use anyhow::{anyhow, Context, Result};

use crate::{analytics, AnalyticsProperties};

use super::{
    agents::{agent_from_name, picker_agents, SkillAgentArg},
    paths::PathContext,
    target::single_target,
    SkillInstallArgs, SkillStatusArgs, BUNDLED_SKILL_NAME,
};

pub(super) fn insert_target_analytics(
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

fn dedupe_agents(agents: impl IntoIterator<Item = SkillAgentArg>) -> Vec<SkillAgentArg> {
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

fn detected_agent_specific_agents(context: &PathContext) -> Vec<SkillAgentArg> {
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

fn can_prompt() -> bool {
    io::stdin().is_terminal() && io::stderr().is_terminal()
}

fn prompt_for_agents(context: &PathContext) -> Result<Vec<SkillAgentArg>> {
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
        .split(|ch: char| ch == ',' || ch == ' ' || ch == '\t')
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
