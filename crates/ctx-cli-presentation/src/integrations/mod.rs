use anyhow::Result;
use clap::{Args, Subcommand};

use crate::{
    analytics::{
        count_bucket, IntegrationAction, IntegrationResult, IntegrationTarget,
        IntegrationTelemetry, TargetSelection,
    },
    skill,
};
use ctx_agent_application::{
    IntegrationResultFact, IntegrationTelemetryFacts, ProductIdentity, TargetSelectionFact,
};

mod mcp;
mod slash_commands;

use mcp::{
    run_install as run_mcp_install, run_remove as run_mcp_remove, run_status as run_mcp_status,
};

#[derive(Debug, Args)]
pub struct IntegrationsArgs {
    #[command(subcommand)]
    command: IntegrationCommand,
}

#[derive(Debug, Subcommand)]
enum IntegrationCommand {
    #[command(about = "Install ctx into an external integration")]
    Install(IntegrationInstallArgs),
    #[command(about = "Remove ctx from an external integration")]
    Remove(IntegrationRemoveArgs),
    #[command(about = "Inspect ctx integration install state")]
    Status(IntegrationStatusArgs),
}

#[derive(Debug, Args)]
struct IntegrationInstallArgs {
    #[command(subcommand)]
    target: IntegrationInstallTarget,
}

#[derive(Debug, Subcommand)]
enum IntegrationInstallTarget {
    #[command(about = "Install the local ctx MCP server into coding-agent clients")]
    Mcp(mcp::McpInstallArgs),
    #[command(about = "Install or refresh the bundled ctx agent-history skill")]
    Skills(skill::SkillInstallArgs),
    #[command(
        name = "slash-commands",
        about = "Install ctx slash-command entry points"
    )]
    SlashCommands(slash_commands::SlashCommandInstallArgs),
}

#[derive(Debug, Args)]
struct IntegrationRemoveArgs {
    #[command(subcommand)]
    target: IntegrationRemoveTarget,
}

#[derive(Debug, Subcommand)]
enum IntegrationRemoveTarget {
    #[command(about = "Remove the local ctx MCP server from coding-agent clients")]
    Mcp(mcp::McpRemoveArgs),
}

#[derive(Debug, Args)]
struct IntegrationStatusArgs {
    #[command(subcommand)]
    target: IntegrationStatusTarget,
}

#[derive(Debug, Subcommand)]
enum IntegrationStatusTarget {
    #[command(about = "Inspect local ctx MCP server integration state")]
    Mcp(mcp::McpStatusArgs),
    #[command(about = "Check whether the bundled ctx agent-history skill is installed")]
    Skills(skill::SkillStatusArgs),
}

impl IntegrationsArgs {
    pub fn json_output(&self) -> bool {
        match &self.command {
            IntegrationCommand::Install(args) => match &args.target {
                IntegrationInstallTarget::Mcp(args) => args.format.is_json(),
                IntegrationInstallTarget::Skills(args) => args.json_output(),
                IntegrationInstallTarget::SlashCommands(args) => args.format.is_json(),
            },
            IntegrationCommand::Remove(args) => match &args.target {
                IntegrationRemoveTarget::Mcp(args) => args.format.is_json(),
            },
            IntegrationCommand::Status(args) => match &args.target {
                IntegrationStatusTarget::Mcp(args) => args.format.is_json(),
                IntegrationStatusTarget::Skills(args) => args.json_output(),
            },
        }
    }
}

pub fn run(
    args: IntegrationsArgs,
    identity: ProductIdentity<'_>,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut crate::ui::Ui,
) -> Result<()> {
    match args.command {
        IntegrationCommand::Install(args) => match args.target {
            IntegrationInstallTarget::Mcp(args) => {
                telemetry.action = Some(IntegrationAction::Install);
                telemetry.target = Some(IntegrationTarget::Mcp);
                args.add_initial_analytics(telemetry);
                let context = mcp::McpPathContext::from_env()?;
                run_mcp_install(args, &context, identity, telemetry, ui)
            }
            IntegrationInstallTarget::Skills(args) => {
                telemetry.action = Some(IntegrationAction::Install);
                telemetry.target = Some(IntegrationTarget::Skills);
                args.add_initial_analytics(telemetry);
                skill::run_install_command(args, identity, telemetry, ui)
            }
            IntegrationInstallTarget::SlashCommands(args) => {
                telemetry.action = Some(IntegrationAction::Install);
                telemetry.target = Some(IntegrationTarget::SlashCommands);
                slash_commands::insert_install_analytics(telemetry, &args);
                let context = slash_commands::PathContext::from_env()?;
                slash_commands::run_install(args, &context, identity, telemetry, ui)
            }
        },
        IntegrationCommand::Remove(args) => match args.target {
            IntegrationRemoveTarget::Mcp(args) => {
                telemetry.action = Some(IntegrationAction::Remove);
                telemetry.target = Some(IntegrationTarget::Mcp);
                args.add_initial_analytics(telemetry);
                let context = mcp::McpPathContext::from_env()?;
                run_mcp_remove(args, &context, identity, telemetry, ui)
            }
        },
        IntegrationCommand::Status(args) => match args.target {
            IntegrationStatusTarget::Mcp(args) => {
                telemetry.action = Some(IntegrationAction::Status);
                telemetry.target = Some(IntegrationTarget::Mcp);
                args.add_initial_analytics(telemetry);
                let context = mcp::McpPathContext::from_env()?;
                run_mcp_status(args, &context, identity, telemetry, ui)
            }
            IntegrationStatusTarget::Skills(args) => {
                telemetry.action = Some(IntegrationAction::Status);
                telemetry.target = Some(IntegrationTarget::Skills);
                args.add_initial_analytics(telemetry);
                skill::run_status_command(args, identity, telemetry, ui)
            }
        },
    }
}

pub fn apply_workflow_telemetry(
    facts: IntegrationTelemetryFacts,
    telemetry: &mut IntegrationTelemetry,
) {
    if let Some(selection) = facts.selection {
        telemetry.selection = Some(match selection {
            TargetSelectionFact::Explicit => TargetSelection::Explicit,
            TargetSelectionFact::All => TargetSelection::All,
            TargetSelectionFact::Picker => TargetSelection::Picker,
            TargetSelectionFact::Detected => TargetSelection::Detected,
            TargetSelectionFact::Fallback => TargetSelection::Fallback,
        });
    }
    telemetry.resolved_agents = facts
        .resolved_agents
        .map(|count| count_bucket(count as u64));
    telemetry.result = facts.result.map(|result| match result {
        IntegrationResultFact::Ok => IntegrationResult::Ok,
        IntegrationResultFact::PartialError => IntegrationResult::PartialError,
        IntegrationResultFact::AllCurrent => IntegrationResult::AllCurrent,
        IntegrationResultFact::NoneCurrent => IntegrationResult::NoneCurrent,
        IntegrationResultFact::PartiallyCurrent => IntegrationResult::PartiallyCurrent,
    });
    telemetry.already_installed = facts.already_installed;
    telemetry.updated = facts.updated;
    telemetry.modified_targets = facts
        .modified_targets
        .map(|count| count_bucket(count as u64));
    telemetry.current_targets = facts
        .current_targets
        .map(|count| count_bucket(count as u64));
    telemetry.missing_targets = facts
        .missing_targets
        .map(|count| count_bucket(count as u64));
    telemetry.conflicting_targets = facts
        .conflicting_targets
        .map(|count| count_bucket(count as u64));
    telemetry.invalid_targets = facts
        .invalid_targets
        .map(|count| count_bucket(count as u64));
    telemetry.unsupported_targets = facts
        .unsupported_targets
        .map(|count| count_bucket(count as u64));
}
