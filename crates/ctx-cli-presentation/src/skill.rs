use anyhow::Result;
use clap::Args;

use crate::analytics::{count_bucket, IntegrationScope, IntegrationTelemetry, TargetSelection};
use crate::output::JsonOutputFormat;
use crate::ui::Ui;

mod install;
mod remove;
mod selection;

mod agents {
    pub(super) use ctx_agent_integrations::skill::{picker_agents, SkillAgentArg};
}

mod paths {
    pub(super) use ctx_agent_integrations::skill::PathContext;
}

#[cfg(test)]
mod tests;

use agents::SkillAgentArg;
use install::{run_install, run_status};
use paths::PathContext;
use remove::run_remove;

use ctx_agent_integrations::skill::BUNDLED_SKILL_NAME;

#[derive(Debug, Args)]
pub struct SkillInstallArgs {
    #[arg(
        long = "agent",
        value_parser = ctx_agent_integrations::skill::parse_skill_agent,
        conflicts_with = "all_agents"
    )]
    agent: Vec<SkillAgentArg>,
    #[arg(long, conflicts_with = "agent")]
    all_agents: bool,
    #[arg(
        long,
        help = "Install into the current project instead of global agent dirs"
    )]
    project: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    format: JsonOutputFormat,
    #[arg(long, help = "Overwrite locally modified bundled skill files")]
    force: bool,
}

#[derive(Debug, Args)]
pub struct SkillStatusArgs {
    #[arg(
        long = "agent",
        value_parser = ctx_agent_integrations::skill::parse_skill_agent,
        conflicts_with = "all_agents"
    )]
    agent: Vec<SkillAgentArg>,
    #[arg(long, conflicts_with = "agent")]
    all_agents: bool,
    #[arg(
        long,
        help = "Check the current project's skill dirs instead of global dirs"
    )]
    project: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    format: JsonOutputFormat,
}

#[derive(Debug, Args)]
pub struct SkillRemoveArgs {
    #[arg(
        long = "agent",
        value_parser = ctx_agent_integrations::skill::parse_skill_agent,
        conflicts_with = "all_agents"
    )]
    agent: Vec<SkillAgentArg>,
    #[arg(long, conflicts_with = "agent")]
    all_agents: bool,
    #[arg(
        long,
        help = "Remove from the current project's skill dirs instead of global dirs"
    )]
    project: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    format: JsonOutputFormat,
    #[arg(long, help = "Remove unowned or locally modified exact skill files")]
    force: bool,
}

impl SkillInstallArgs {
    pub fn json_output(&self) -> bool {
        self.format.is_json()
    }

    pub fn add_initial_analytics(&self, telemetry: &mut IntegrationTelemetry) {
        insert_target_analytics(
            telemetry,
            self.agent.len(),
            self.all_agents,
            self.project,
            self.force,
        );
    }
}

impl SkillStatusArgs {
    pub fn json_output(&self) -> bool {
        self.format.is_json()
    }

    pub fn add_initial_analytics(&self, telemetry: &mut IntegrationTelemetry) {
        insert_target_analytics(
            telemetry,
            self.agent.len(),
            self.all_agents,
            self.project,
            false,
        );
    }
}

impl SkillRemoveArgs {
    pub fn json_output(&self) -> bool {
        self.format.is_json()
    }

    pub fn add_initial_analytics(&self, telemetry: &mut IntegrationTelemetry) {
        insert_target_analytics(
            telemetry,
            self.agent.len(),
            self.all_agents,
            self.project,
            self.force,
        );
    }
}

pub fn run_install_command(
    args: SkillInstallArgs,
    identity: ctx_agent_application::ProductIdentity<'_>,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let context = PathContext::from_env()?;
    run_install(args, &context, identity, telemetry, ui)
}

pub fn run_status_command(
    args: SkillStatusArgs,
    identity: ctx_agent_application::ProductIdentity<'_>,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let context = PathContext::from_env()?;
    run_status(args, &context, identity, telemetry, ui)
}

pub fn run_remove_command(
    args: SkillRemoveArgs,
    identity: ctx_agent_application::ProductIdentity<'_>,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let context = PathContext::from_env()?;
    run_remove(args, &context, identity, telemetry, ui)
}

fn insert_target_analytics(
    telemetry: &mut IntegrationTelemetry,
    explicit_agents: usize,
    all_agents: bool,
    project: bool,
    force: bool,
) {
    telemetry.scope = Some(if project {
        IntegrationScope::Project
    } else {
        IntegrationScope::Global
    });
    telemetry.selection = Some(if all_agents {
        TargetSelection::All
    } else if explicit_agents == 0 {
        TargetSelection::Fallback
    } else {
        TargetSelection::Explicit
    });
    telemetry.target_agents = Some(count_bucket(if all_agents {
        SkillAgentArg::ALL.len() as u64
    } else {
        explicit_agents.max(1) as u64
    }));
    telemetry.force = Some(force);
}
