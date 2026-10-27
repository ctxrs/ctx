//! Public argument schema for native Agent Session Record export.
use clap::{Args, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub enum ExportArgs {
    /// Convert native sessions to an Agent Session Record 0.1 archive bundle.
    Asr(AsrArgs),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AsrProvider {
    #[value(alias = "claude-code")]
    Claude,
    Codex,
    Firebender,
    Forgecode,
    #[value(alias = "deep-agents")]
    Deepagents,
    Devin,
}
#[derive(Debug, Args)]
pub struct AsrArgs {
    /// Exact native session ID (a Claude subagent uses its agentId).
    #[arg(long, required_unless_present = "all", conflicts_with = "all")]
    pub session: Option<String>,
    /// Export every session from the selected supported converters.
    #[arg(long)]
    pub all: bool,
    #[arg(long, value_enum)]
    pub provider: Option<AsrProvider>,
    /// Read this native transcript or directory instead of automatic discovery.
    #[arg(long, requires = "provider")]
    pub path: Option<PathBuf>,
    /// Destination archive directory, including bundle.json and session streams.
    #[arg(long)]
    pub out: PathBuf,
    /// Maximum bytes in a native line or assembled content field.
    #[arg(long,default_value_t=16*1024*1024)]
    pub max_record_bytes: usize,
}
