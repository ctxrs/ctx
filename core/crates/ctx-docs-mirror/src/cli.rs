use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "ctx-docs-mirror")]
#[command(about = "Deterministic docs mirror helper", long_about = None)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Subcommand, Debug)]
pub(crate) enum Commands {
    Plan {
        #[arg(long)]
        config: PathBuf,
    },
    Mirror {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long, default_value_t = true)]
        clean: bool,
    },
}
