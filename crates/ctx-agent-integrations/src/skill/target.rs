use std::path::PathBuf;

use anyhow::{Context, Result};

use super::{
    agents::SkillAgentArg,
    paths::{ensure_path_inside, sanitize_skill_name, PathContext},
    BUNDLED_SKILL_NAME,
};

#[derive(Debug, Clone)]
pub struct SkillTarget {
    pub agent: SkillAgentArg,
    pub scope: SkillScope,
    pub authority_root: PathBuf,
    pub base_dir: PathBuf,
    pub skill_dir: PathBuf,
}

#[derive(Debug, Clone, Copy)]
pub enum SkillScope {
    Global,
    Project,
}

impl SkillScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

pub fn single_target(
    agent: SkillAgentArg,
    project: bool,
    context: &PathContext,
) -> Result<SkillTarget> {
    let skill_name = sanitize_skill_name(BUNDLED_SKILL_NAME)?;
    let (scope, authority_root, base_dir) = if project {
        (
            SkillScope::Project,
            context.cwd.clone(),
            context.cwd.join(agent.project_skills_dir()),
        )
    } else {
        let base_dir = agent.global_skills_dir(context);
        let authority_root = agent.global_skills_authority_root(context);
        (SkillScope::Global, authority_root, base_dir)
    };
    let skill_dir = base_dir.join(&skill_name);
    ensure_path_inside(&base_dir, &skill_dir)
        .with_context(|| format!("resolve {} skill path", agent.id()))?;
    Ok(SkillTarget {
        agent,
        scope,
        authority_root,
        base_dir,
        skill_dir,
    })
}

pub fn resolve_targets_for_agents(
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
