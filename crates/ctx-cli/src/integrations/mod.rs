use anyhow::Result;
use clap::{Args, Subcommand};

use crate::{analytics, skill, AnalyticsProperties};

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
                IntegrationInstallTarget::Mcp(args) => args.json,
                IntegrationInstallTarget::Skills(args) => args.json_output(),
                IntegrationInstallTarget::SlashCommands(args) => args.json,
            },
            IntegrationCommand::Status(args) => match &args.target {
                IntegrationStatusTarget::Mcp(args) => args.json,
                IntegrationStatusTarget::Skills(args) => args.json_output(),
            },
        }
    }

    pub(crate) fn add_initial_analytics(&self, properties: &mut AnalyticsProperties) {
        match &self.command {
            IntegrationCommand::Install(args) => {
                analytics::insert_str(properties, "integration_action", "install");
                match &args.target {
                    IntegrationInstallTarget::Mcp(args) => args.add_initial_analytics(properties),
                    IntegrationInstallTarget::Skills(args) => {
                        analytics::insert_str(properties, "integration_target", "skills");
                        args.add_initial_analytics(properties);
                    }
                    IntegrationInstallTarget::SlashCommands(args) => {
                        analytics::insert_str(properties, "integration_target", "slash_commands");
                        slash_commands::insert_install_analytics(properties, args);
                    }
                }
            }
            IntegrationCommand::Status(args) => {
                analytics::insert_str(properties, "integration_action", "status");
                match &args.target {
                    IntegrationStatusTarget::Mcp(args) => args.add_initial_analytics(properties),
                    IntegrationStatusTarget::Skills(args) => {
                        analytics::insert_str(properties, "integration_target", "skills");
                        args.add_initial_analytics(properties);
                    }
                }
            }
        }
    }
}

pub(crate) fn run(
    args: IntegrationsArgs,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    match args.command {
        IntegrationCommand::Install(args) => match args.target {
            IntegrationInstallTarget::Mcp(args) => {
                let context = mcp::McpPathContext::from_env()?;
                run_mcp_install(args, &context, analytics_properties)
            }
            IntegrationInstallTarget::Skills(args) => {
                skill::run_install_command(args, analytics_properties)
            }
            IntegrationInstallTarget::SlashCommands(args) => {
                let context = slash_commands::PathContext::from_env()?;
                slash_commands::run_install(args, &context, analytics_properties)
            }
        },
        IntegrationCommand::Status(args) => match args.target {
            IntegrationStatusTarget::Mcp(args) => {
                let context = mcp::McpPathContext::from_env()?;
                run_mcp_status(args, &context)
            }
            IntegrationStatusTarget::Skills(args) => {
                skill::run_status_command(args, analytics_properties)
            }
        },
    }
}
