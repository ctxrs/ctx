use clap::{Args, Subcommand};

mod apply;
mod commands;
mod diagnostics;
mod lock;
mod marker;
mod metadata;
mod state;
mod types;
mod util;

pub(crate) use commands::{maybe_spawn_auto_upgrade, run};

#[derive(Debug, Args)]
pub struct UpgradeArgs {
    #[command(subcommand)]
    pub command: Option<UpgradeCommand>,
    #[arg(long)]
    pub channel: Option<String>,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub json: bool,
    #[arg(long, hide = true)]
    pub background: bool,
}

#[derive(Debug, Subcommand)]
pub enum UpgradeCommand {
    #[command(about = "Check whether a newer ctx release is available")]
    Check(UpgradeCheckArgs),
    #[command(about = "Show local upgrade state")]
    Status(UpgradeStatusArgs),
    #[command(about = "Enable managed background auto-upgrades")]
    Enable,
    #[command(about = "Disable background auto-upgrades")]
    Disable,
}

#[derive(Debug, Args)]
pub struct UpgradeCheckArgs {
    #[arg(long)]
    pub channel: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct UpgradeStatusArgs {
    #[arg(long)]
    pub json: bool,
}

impl UpgradeArgs {
    pub fn json_output(&self) -> bool {
        match &self.command {
            Some(UpgradeCommand::Check(args)) => args.json || self.json,
            Some(UpgradeCommand::Status(args)) => args.json || self.json,
            Some(UpgradeCommand::Enable | UpgradeCommand::Disable) | None => self.json,
        }
    }

    pub fn background(&self) -> bool {
        self.background
    }
}
