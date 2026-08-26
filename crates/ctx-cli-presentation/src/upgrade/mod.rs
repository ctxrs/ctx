use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use ctx_upgrade_engine::HostedTransactionAction;

use crate::output::JsonOutputFormat;

mod diagnostics;
mod human;
mod status;

pub use diagnostics::{present_upgrade_diagnostics, UpgradeDiagnostics};
pub use human::{render_auto_mode, render_error, render_outcome};
pub use status::{reconcile_scheduled_state, render_status, UpgradeStatusView};

#[derive(Debug, Args)]
pub struct UpgradeArgs {
    #[command(subcommand)]
    pub command: Option<UpgradeCommand>,
    #[arg(long)]
    pub channel: Option<String>,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
    #[arg(long, hide = true)]
    pub replacement_helper: bool,
    #[arg(long, hide = true)]
    pub install_path: Option<PathBuf>,
    #[arg(long, hide = true)]
    pub attempt_id: Option<String>,
    #[arg(long, hide = true)]
    pub parent_pid: Option<u32>,
    #[arg(long, value_enum, hide = true)]
    pub hosted_transaction: Option<HostedTransactionActionArg>,
    #[arg(long, hide = true)]
    pub marker_source: Option<PathBuf>,
    #[arg(long, hide = true)]
    pub ownership_source: Option<PathBuf>,
    #[arg(long, hide = true)]
    pub binary_sha256: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum UpgradeCommand {
    #[command(about = "Check whether a newer ctx release is available")]
    Check(UpgradeCheckArgs),
    #[command(about = "Show local upgrade state")]
    Status(UpgradeStatusArgs),
    #[command(about = "Enable automatic upgrades")]
    Enable,
    #[command(about = "Disable automatic upgrades")]
    Disable,
}

#[derive(Debug, Args)]
pub struct UpgradeCheckArgs {
    #[arg(long)]
    pub channel: Option<String>,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
}

#[derive(Debug, Args)]
pub struct UpgradeStatusArgs {
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum HostedTransactionActionArg {
    Install,
    UninstallPrepare,
    UninstallArm,
    UninstallCommit,
}

impl From<HostedTransactionActionArg> for HostedTransactionAction {
    fn from(value: HostedTransactionActionArg) -> Self {
        match value {
            HostedTransactionActionArg::Install => Self::Install,
            HostedTransactionActionArg::UninstallPrepare => Self::UninstallPrepare,
            HostedTransactionActionArg::UninstallArm => Self::UninstallArm,
            HostedTransactionActionArg::UninstallCommit => Self::UninstallCommit,
        }
    }
}

impl UpgradeArgs {
    pub fn json_output(&self) -> bool {
        self.format.is_json()
            || matches!(
                &self.command,
                Some(UpgradeCommand::Check(args)) if args.format.is_json()
            )
            || matches!(
                &self.command,
                Some(UpgradeCommand::Status(args)) if args.format.is_json()
            )
            || self.replacement_helper
            || self.hosted_transaction.is_some()
    }

    pub fn operation(&self) -> &'static str {
        match &self.command {
            Some(UpgradeCommand::Check(_)) => "check",
            Some(UpgradeCommand::Status(_)) => "status",
            Some(UpgradeCommand::Enable) => "enable",
            Some(UpgradeCommand::Disable) => "disable",
            None => "apply",
        }
    }
}
