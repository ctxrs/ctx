use anyhow::Result;
use clap::Args;

use crate::analytics::{count_bucket, IntegrationScope, IntegrationTelemetry, TargetSelection};
use crate::output::JsonOutputFormat;

mod format;
mod operation;
mod registry;

pub(crate) use registry::{McpAgentArg, McpPathContext};

const SERVER_NAME: &str = "ctx";
const SERVER_COMMAND: &str = "ctx";
const SERVER_ARGS: &[&str] = &["mcp", "serve"];

#[derive(Debug, Args)]
pub(crate) struct McpInstallArgs {
    #[arg(
        long = "agent",
        alias = "provider",
        value_enum,
        conflicts_with = "all_agents",
        help = "Install for one coding-agent client; --provider is accepted as an alias"
    )]
    pub(crate) agent: Vec<McpAgentArg>,
    #[arg(long, conflicts_with = "agent")]
    pub(crate) all_agents: bool,
    #[arg(
        long,
        help = "Install into the current project's MCP config when supported"
    )]
    pub(crate) project: bool,
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
    #[arg(
        long = "agent",
        alias = "provider",
        value_enum,
        conflicts_with = "all_agents",
        help = "Inspect one coding-agent client; --provider is accepted as an alias"
    )]
    pub(crate) agent: Vec<McpAgentArg>,
    #[arg(long, conflicts_with = "agent")]
    pub(crate) all_agents: bool,
    #[arg(long, help = "Inspect the current project's MCP config when supported")]
    pub(crate) project: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
}

impl McpInstallArgs {
    pub(crate) fn add_initial_analytics(&self, telemetry: &mut IntegrationTelemetry) {
        insert_target_analytics(telemetry, &self.agent, self.all_agents, self.project);
        telemetry.force = Some(self.force);
    }
}

impl McpStatusArgs {
    pub(crate) fn add_initial_analytics(&self, telemetry: &mut IntegrationTelemetry) {
        insert_target_analytics(telemetry, &self.agent, self.all_agents, self.project);
    }
}

fn insert_target_analytics(
    telemetry: &mut IntegrationTelemetry,
    agents: &[McpAgentArg],
    all_agents: bool,
    project: bool,
) {
    telemetry.scope = Some(if project {
        IntegrationScope::Project
    } else {
        IntegrationScope::Global
    });
    telemetry.selection = Some(if all_agents {
        TargetSelection::All
    } else if agents.is_empty() {
        TargetSelection::Detected
    } else {
        TargetSelection::Explicit
    });
    let count = if all_agents && project {
        McpAgentArg::PROJECT_CAPABLE.len()
    } else if all_agents {
        McpAgentArg::ALL.len()
    } else {
        agents.len()
    };
    telemetry.target_agents = Some(count_bucket(count as u64));
}

pub(crate) fn run_install(
    args: McpInstallArgs,
    context: &McpPathContext,
    telemetry: &mut IntegrationTelemetry,
) -> Result<()> {
    operation::run_install(args, context, telemetry)
}

pub(crate) fn run_status(
    args: McpStatusArgs,
    context: &McpPathContext,
    telemetry: &mut IntegrationTelemetry,
) -> Result<()> {
    operation::run_status(args, context, telemetry)
}
