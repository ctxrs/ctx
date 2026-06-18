use clap::{Parser, Subcommand};

use crate::agent_work_cli::AgentWorkCommand;

#[derive(Parser)]
#[command(name = "ctx")]
#[command(about = "ctx daemon and CLI", long_about = None)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,
}

impl Cli {
    pub(crate) fn parse_args() -> Self {
        Self::parse()
    }
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    #[command(name = "agent-work")]
    AgentWork(AgentWorkCommand),
    Serve {
        #[arg(long, action = clap::ArgAction::Append)]
        bind: Vec<String>,
        #[arg(long)]
        data_dir: Option<String>,
    },
    Init {
        #[arg(long)]
        root: Option<String>,
    },
    SelfUpdate {
        /// Release channel (e.g. stable, nightly)
        #[arg(long, default_value = "stable")]
        channel: String,
        /// Base URL for release manifests and downloads (e.g. https://api.ctx.rs/functions/v1)
        #[arg(long)]
        base_url: Option<String>,
        /// Run non-interactively.
        #[arg(long)]
        yes: bool,
        /// Only check whether an update exists; do not download/apply.
        #[arg(long)]
        check: bool,
    },
}
