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
const LEGACY_BUNDLED_SKILL_HASHES: &[&str] = &[
    // Historical bundled ctx-agent-history-search SKILL.md hashes from before
    // ctx wrote .ctx-skill.json metadata for managed installs.
    "sha256:9c2ddb5ed64da0471050af225addd5823ef7fc2b9bbcea27e72a3c8553234774",
    "sha256:b4210c5e3c4fd8a8e62335ca61879bb88d026c092b4b663a9ae3ad15f34ee2ba",
    "sha256:59623e2cabd7857a518da19f995ca86e65fe67e6337fa334a0c86bef78891c6f",
    "sha256:287e5470664e6225114c6676d56e6f98eb6f83f3ebe7bac980532c6dabbee0c6",
    "sha256:64e3cf9c676e5edfdb1a825b27abc1971e5959577c15709934421def71405ae2",
    "sha256:c72dbfae7d0af06c18d119f586e22e6cd3ba9444cc6a01e7d4662f2cf98d86d8",
    "sha256:87f435ad67bc5afdc4120f1ca9090aa6c2b71ee87c0bdeeb7e0bde33778c32ed",
    "sha256:3da0ddcff0409cc9d5912cf2019fdaf00d4faa84f000fb76041b670f94aa2986",
    "sha256:b606132c882a0ce0db2c049c599cfdb7db113d2a6690a58a6c329b5101c752c9",
    "sha256:c0647d2368714b09a5f652583b9f2c34e88502b0ab441ba44c4698313675dbcc",
];

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
