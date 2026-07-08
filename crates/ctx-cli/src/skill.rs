use anyhow::Result;
use clap::Args;

use crate::{analytics, AnalyticsProperties};

mod agents;
mod install;
mod paths;
mod selection;
mod target;

#[cfg(test)]
mod tests;

use agents::SkillAgentArg;
use install::{run_install, run_status};
use paths::PathContext;
use selection::insert_target_analytics;

const BUNDLED_SKILL_NAME: &str = "ctx-agent-history-search";
const BUNDLED_SKILL_BODY: &str = include_str!("../../../skills/ctx-agent-history-search/SKILL.md");
const METADATA_FILE: &str = ".ctx-skill.json";

#[derive(Debug, Args)]
pub(crate) struct SkillInstallArgs {
    #[arg(long = "agent", value_enum, conflicts_with = "all_agents")]
    agent: Vec<SkillAgentArg>,
    #[arg(long, conflicts_with = "agent")]
    all_agents: bool,
    #[arg(
        long,
        help = "Install into the current project instead of global agent dirs"
    )]
    project: bool,
    #[arg(long)]
    json: bool,
    #[arg(long, help = "Overwrite locally modified bundled skill files")]
    force: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SkillStatusArgs {
    #[arg(long = "agent", value_enum, conflicts_with = "all_agents")]
    agent: Vec<SkillAgentArg>,
    #[arg(long, conflicts_with = "agent")]
    all_agents: bool,
    #[arg(
        long,
        help = "Check the current project's skill dirs instead of global dirs"
    )]
    project: bool,
    #[arg(long)]
    json: bool,
}

impl SkillInstallArgs {
    pub(crate) fn json_output(&self) -> bool {
        self.json
    }

    pub(crate) fn add_initial_analytics(&self, properties: &mut AnalyticsProperties) {
        analytics::insert_str(properties, "skill_name", BUNDLED_SKILL_NAME);
        analytics::insert_str(properties, "skill_action", "install");
        insert_target_analytics(properties, &self.agent, self.all_agents, self.project);
    }
}

impl SkillStatusArgs {
    pub(crate) fn json_output(&self) -> bool {
        self.json
    }

    pub(crate) fn add_initial_analytics(&self, properties: &mut AnalyticsProperties) {
        analytics::insert_str(properties, "skill_name", BUNDLED_SKILL_NAME);
        analytics::insert_str(properties, "skill_action", "status");
        insert_target_analytics(properties, &self.agent, self.all_agents, self.project);
    }
}

pub(crate) fn run_install_command(
    args: SkillInstallArgs,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let context = PathContext::from_env()?;
    run_install(args, &context, analytics_properties)
}

pub(crate) fn run_status_command(
    args: SkillStatusArgs,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let context = PathContext::from_env()?;
    run_status(args, &context, analytics_properties)
}
