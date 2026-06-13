#![deny(clippy::print_stdout)]

mod app_server;
mod builtins;
mod protocol;
mod runtime;

use crate::builtins::parse_cli_config_overrides;
use clap::Parser;
use serde_json::Value;
#[cfg(test)]
use std::sync::OnceLock;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

#[cfg(test)]
static TEST_ENV_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[cfg(test)]
pub(crate) fn test_env_lock() -> &'static tokio::sync::Mutex<()> {
    TEST_ENV_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[derive(Debug, Parser)]
#[command(version)]
struct Cli {
    #[arg(short = 'c', long = "config", value_name = "KEY=VALUE")]
    config_overrides: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RuntimeOptions {
    pub(crate) config_overrides: Option<Value>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn,codex_crp=info"));
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(env_filter)
        .init();

    let options = RuntimeOptions {
        config_overrides: parse_cli_config_overrides(&cli.config_overrides)?,
    };

    runtime::run(options).await
}
