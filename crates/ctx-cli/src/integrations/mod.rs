use anyhow::Result;
use clap::{Args, Subcommand};

use crate::{analytics, AnalyticsProperties};

mod mcp;

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
}

impl IntegrationsArgs {
    pub(crate) fn json_output(&self) -> bool {
        match &self.command {
            IntegrationCommand::Install(args) => match &args.target {
                IntegrationInstallTarget::Mcp(args) => args.json,
            },
            IntegrationCommand::Status(args) => match &args.target {
                IntegrationStatusTarget::Mcp(args) => args.json,
            },
        }
    }

    pub(crate) fn add_initial_analytics(&self, properties: &mut AnalyticsProperties) {
        match &self.command {
            IntegrationCommand::Install(args) => {
                analytics::insert_str(properties, "integration_action", "install");
                match &args.target {
                    IntegrationInstallTarget::Mcp(args) => args.add_initial_analytics(properties),
                }
            }
            IntegrationCommand::Status(args) => {
                analytics::insert_str(properties, "integration_action", "status");
                match &args.target {
                    IntegrationStatusTarget::Mcp(args) => args.add_initial_analytics(properties),
                }
            }
        }
    }
}

pub(crate) fn run(
    args: IntegrationsArgs,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let context = mcp::McpPathContext::from_env()?;
    match args.command {
        IntegrationCommand::Install(args) => match args.target {
            IntegrationInstallTarget::Mcp(args) => {
                run_mcp_install(args, &context, analytics_properties)
            }
        },
        IntegrationCommand::Status(args) => match args.target {
            IntegrationStatusTarget::Mcp(args) => run_mcp_status(args, &context),
        },
    }
}
