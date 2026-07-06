use std::path::PathBuf;

use anyhow::{Context, Result};

use super::{
    agents::SkillAgentArg,
    paths::{ensure_path_inside, sanitize_skill_name, PathContext},
    BUNDLED_SKILL_NAME,
};

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
    let selected = super::selection::explicit_selected_agents(agents, all_agents)
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
