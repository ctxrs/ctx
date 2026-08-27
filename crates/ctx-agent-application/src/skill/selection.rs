use anyhow::Result;
use ctx_agent_integrations::skill::{
    default_maintenance_selection, detected_agents, explicit_agent_selection,
    picker_agent_selection, single_target, PathContext, SkillAgentArg, SkillAgentSelection,
    BUNDLED_SKILL_NAME,
};

pub struct SkillSelectionRequest<'a> {
    pub agents: &'a [SkillAgentArg],
    pub all_agents: bool,
    pub allow_picker: bool,
    pub project: bool,
}

#[derive(Debug)]
pub enum SkillInstallSelectionPlan {
    Selected(SkillAgentSelection),
    Prompt(SkillPickerPrompt),
}

#[derive(Debug)]
pub struct SkillPickerPrompt {
    pub skill_name: &'static str,
    pub options: Vec<SkillPickerOption>,
}

#[derive(Debug)]
pub struct SkillPickerOption {
    pub agent: SkillAgentArg,
    pub selected_by_default: bool,
    pub detected: bool,
    pub target: ctx_agent_integrations::skill::SkillTarget,
}

pub fn plan_install_selection(
    request: SkillSelectionRequest<'_>,
    context: &PathContext,
) -> Result<SkillInstallSelectionPlan> {
    if let Some(selection) = explicit_agent_selection(request.agents, request.all_agents) {
        return Ok(SkillInstallSelectionPlan::Selected(selection));
    }
    let defaults = default_maintenance_selection(request.project, context)?;
    if !request.allow_picker {
        return Ok(SkillInstallSelectionPlan::Selected(defaults));
    }

    let detected = detected_agents(context);
    let options = ctx_agent_integrations::skill::picker_agents()
        .iter()
        .copied()
        .map(|agent| {
            Ok(SkillPickerOption {
                agent,
                selected_by_default: defaults.agents.contains(&agent),
                detected: detected.contains(&agent),
                target: single_target(agent, request.project, context)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(SkillInstallSelectionPlan::Prompt(SkillPickerPrompt {
        skill_name: BUNDLED_SKILL_NAME,
        options,
    }))
}

pub fn complete_picker_selection(agents: Vec<SkillAgentArg>) -> SkillAgentSelection {
    picker_agent_selection(agents)
}

pub fn status_selection(
    agents: &[SkillAgentArg],
    all_agents: bool,
    project: bool,
    context: &PathContext,
) -> Result<SkillAgentSelection> {
    match explicit_agent_selection(agents, all_agents) {
        Some(selection) => Ok(selection),
        None => default_maintenance_selection(project, context),
    }
}
