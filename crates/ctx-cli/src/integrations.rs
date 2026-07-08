use anyhow::Result;
use clap::{Args, Subcommand};

use crate::{analytics, AnalyticsProperties};

mod slash_commands;

#[derive(Debug, Args)]
pub(crate) struct IntegrationsArgs {
    #[command(subcommand)]
    command: IntegrationsCommand,
}

#[derive(Debug, Subcommand)]
enum IntegrationsCommand {
    #[command(about = "Install ctx integrations")]
    Install(IntegrationsInstallArgs),
}

#[derive(Debug, Args)]
struct IntegrationsInstallArgs {
    #[command(subcommand)]
    target: IntegrationsInstallTarget,
}

#[derive(Debug, Subcommand)]
enum IntegrationsInstallTarget {
    #[command(
        name = "slash-commands",
        about = "Install ctx slash-command entry points"
    )]
    SlashCommands(slash_commands::SlashCommandInstallArgs),
}

impl IntegrationsArgs {
    pub(crate) fn json_output(&self) -> bool {
        match &self.command {
            IntegrationsCommand::Install(args) => match &args.target {
                IntegrationsInstallTarget::SlashCommands(args) => args.json,
            },
        }
    }

    pub(crate) fn add_initial_analytics(&self, properties: &mut AnalyticsProperties) {
        match &self.command {
            IntegrationsCommand::Install(args) => {
                analytics::insert_str(properties, "integration_action", "install");
                match &args.target {
                    IntegrationsInstallTarget::SlashCommands(args) => {
                        analytics::insert_str(properties, "integration_target", "slash_commands");
                        slash_commands::insert_install_analytics(properties, args);
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
    let context = slash_commands::PathContext::from_env()?;
    match args.command {
        IntegrationsCommand::Install(args) => match args.target {
            IntegrationsInstallTarget::SlashCommands(args) => {
                slash_commands::run_install(args, &context, analytics_properties)
            }
        },
    }
}
