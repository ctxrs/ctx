use clap::{Args, Subcommand, ValueEnum};

#[derive(Debug, Args)]
pub(crate) struct AgentWorkCommand {
    #[command(subcommand)]
    pub(crate) command: AgentWorkSubcommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum AgentWorkSubcommand {
    /// Print a public ctx agent-work schema.
    Schema(AgentWorkSchemaArgs),
}

#[derive(Debug, Args)]
pub(crate) struct AgentWorkSchemaArgs {
    /// Schema to print.
    #[arg(long, value_enum, default_value_t = AgentWorkSchemaKind::AgentWork)]
    pub(crate) kind: AgentWorkSchemaKind,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum AgentWorkSchemaKind {
    AgentWork,
    ChangeSet,
    Contribution,
    Events,
    ToolCall,
    Transcripts,
    PluginManifest,
}

pub(crate) fn run(command: AgentWorkCommand) {
    match command.command {
        AgentWorkSubcommand::Schema(args) => {
            println!("{}", schema_for_kind(args.kind));
        }
    }
}

fn schema_for_kind(kind: AgentWorkSchemaKind) -> &'static str {
    match kind {
        AgentWorkSchemaKind::AgentWork => {
            include_str!("../../../../schemas/agent-work/v1.schema.json")
        }
        AgentWorkSchemaKind::ChangeSet => {
            include_str!("../../../../schemas/agent-work/change-set.v1.schema.json")
        }
        AgentWorkSchemaKind::Contribution => {
            include_str!("../../../../schemas/agent-work/contribution.v1.schema.json")
        }
        AgentWorkSchemaKind::Events => include_str!("../../../../schemas/events/v1.schema.json"),
        AgentWorkSchemaKind::ToolCall => {
            include_str!("../../../../schemas/events/tool-call.v1.schema.json")
        }
        AgentWorkSchemaKind::Transcripts => {
            include_str!("../../../../schemas/transcripts/v1.schema.json")
        }
        AgentWorkSchemaKind::PluginManifest => {
            include_str!("../../../../schemas/plugins/plugin-manifest.v1.schema.json")
        }
    }
}
