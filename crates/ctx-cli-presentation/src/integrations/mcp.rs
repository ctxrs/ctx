use anyhow::Result;
use clap::Args;

use crate::analytics::{count_bucket, IntegrationScope, IntegrationTelemetry, TargetSelection};
use crate::output::JsonOutputFormat;

mod operation;
mod remove;

pub(crate) use ctx_agent_integrations::mcp_config::{McpAgentArg, McpPathContext};

mod format {
    pub(super) use ctx_agent_integrations::mcp_config::{server_command, ConfigStatus};
}

#[derive(Debug, Args)]
pub(crate) struct McpTargetArgs {
    #[arg(
        long = "agent",
        alias = "provider",
        value_parser = ctx_agent_integrations::mcp_config::parse_mcp_agent,
        conflicts_with = "all_agents",
        help = "Target one coding-agent client; --provider is accepted as an alias"
    )]
    pub(crate) agent: Vec<McpAgentArg>,
    #[arg(long, conflicts_with = "agent")]
    pub(crate) all_agents: bool,
    #[arg(long, help = "Use the current project's MCP config when supported")]
    pub(crate) project: bool,
}

#[derive(Debug, Args)]
pub(crate) struct McpInstallArgs {
    #[command(flatten)]
    pub(crate) target: McpTargetArgs,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
    #[arg(
        long,
        help = "Overwrite an existing ctx MCP server entry with different command or args"
    )]
    pub(crate) force: bool,
}

#[derive(Debug, Args)]
pub(crate) struct McpStatusArgs {
    #[command(flatten)]
    pub(crate) target: McpTargetArgs,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
}

#[derive(Debug, Args)]
pub(crate) struct McpRemoveArgs {
    #[command(flatten)]
    pub(crate) target: McpTargetArgs,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
    #[arg(
        long,
        help = "Remove an existing ctx MCP server entry even when its command or args differ"
    )]
    pub(crate) force: bool,
}

impl McpInstallArgs {
    pub(crate) fn add_initial_analytics(&self, telemetry: &mut IntegrationTelemetry) {
        self.target.add_initial_analytics(telemetry);
        telemetry.force = Some(self.force);
    }
}

impl McpStatusArgs {
    pub(crate) fn add_initial_analytics(&self, telemetry: &mut IntegrationTelemetry) {
        self.target.add_initial_analytics(telemetry);
    }
}

impl McpRemoveArgs {
    pub(crate) fn add_initial_analytics(&self, telemetry: &mut IntegrationTelemetry) {
        self.target.add_initial_analytics(telemetry);
        telemetry.force = Some(self.force);
    }
}

impl McpTargetArgs {
    fn add_initial_analytics(&self, telemetry: &mut IntegrationTelemetry) {
        telemetry.scope = Some(if self.project {
            IntegrationScope::Project
        } else {
            IntegrationScope::Global
        });
        telemetry.selection = Some(if self.all_agents {
            TargetSelection::All
        } else if self.agent.is_empty() {
            TargetSelection::Detected
        } else {
            TargetSelection::Explicit
        });
        let count = if self.all_agents && self.project {
            McpAgentArg::PROJECT_CAPABLE.len()
        } else if self.all_agents {
            McpAgentArg::ALL.len()
        } else {
            self.agent.len()
        };
        telemetry.target_agents = Some(count_bucket(count as u64));
    }
}

pub(crate) fn run_install(
    args: McpInstallArgs,
    context: &McpPathContext,
    identity: ctx_agent_application::ProductIdentity<'_>,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut crate::ui::Ui,
) -> Result<()> {
    operation::run_install(args, context, identity, telemetry, ui)
}

pub(crate) fn run_status(
    args: McpStatusArgs,
    context: &McpPathContext,
    identity: ctx_agent_application::ProductIdentity<'_>,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut crate::ui::Ui,
) -> Result<()> {
    operation::run_status(args, context, identity, telemetry, ui)
}

pub(crate) fn run_remove(
    args: McpRemoveArgs,
    context: &McpPathContext,
    identity: ctx_agent_application::ProductIdentity<'_>,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut crate::ui::Ui,
) -> Result<()> {
    remove::run(args, context, identity, telemetry, ui)
}
