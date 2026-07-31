use anyhow::Result;
use clap::{Args, Subcommand};

use crate::{
    analytics::{IntegrationAction, IntegrationTarget, IntegrationTelemetry},
    skill,
};

mod mcp;
mod slash_commands;

use mcp::{run_install as run_mcp_install, run_status as run_mcp_status};

#[derive(Debug, Args)]
pub(crate) struct IntegrationsArgs {
    #[command(subcommand)]
    command: IntegrationCommand,
}

#[derive(Debug, Subcommand)]
enum IntegrationCommand {
    #[command(about = "Install ctx into an external integration")]
    Install(IntegrationInstallArgs),
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
    pub(crate) fn json_output(&self) -> bool {
        match &self.command {
            IntegrationCommand::Install(args) => match &args.target {
                IntegrationInstallTarget::Mcp(args) => args.format.is_json(),
                IntegrationInstallTarget::Skills(args) => args.json_output(),
                IntegrationInstallTarget::SlashCommands(args) => args.format.is_json(),
            },
            IntegrationCommand::Status(args) => match &args.target {
                IntegrationStatusTarget::Mcp(args) => args.format.is_json(),
                IntegrationStatusTarget::Skills(args) => args.json_output(),
            },
        }
    }
}

pub(crate) fn run(
    args: IntegrationsArgs,
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
                run_mcp_install(args, &context, telemetry, ui)
            }
            IntegrationInstallTarget::Skills(args) => {
                telemetry.action = Some(IntegrationAction::Install);
                telemetry.target = Some(IntegrationTarget::Skills);
                args.add_initial_analytics(telemetry);
                skill::run_install_command(args, telemetry, ui)
            }
            IntegrationInstallTarget::SlashCommands(args) => {
                telemetry.action = Some(IntegrationAction::Install);
                telemetry.target = Some(IntegrationTarget::SlashCommands);
                slash_commands::insert_install_analytics(telemetry, &args);
                let context = slash_commands::PathContext::from_env()?;
                slash_commands::run_install(args, &context, telemetry, ui)
            }
        },
        IntegrationCommand::Status(args) => match args.target {
            IntegrationStatusTarget::Mcp(args) => {
                telemetry.action = Some(IntegrationAction::Status);
                telemetry.target = Some(IntegrationTarget::Mcp);
                args.add_initial_analytics(telemetry);
                let context = mcp::McpPathContext::from_env()?;
                run_mcp_status(args, &context, telemetry, ui)
            }
            IntegrationStatusTarget::Skills(args) => {
                telemetry.action = Some(IntegrationAction::Status);
                telemetry.target = Some(IntegrationTarget::Skills);
                args.add_initial_analytics(telemetry);
                skill::run_status_command(args, telemetry, ui)
            }
        },
    }
}
