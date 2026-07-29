use std::path::PathBuf;

use anyhow::Result;
use clap::ValueEnum;

use crate::analytics::SearchTelemetry;
use crate::commands::import::ProviderRefreshCollector;
use crate::{config, SearchArgs};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum RefreshArg {
    Background,
    Off,
    Wait,
}

impl RefreshArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Off => "off",
            Self::Wait => "wait",
        }
    }
}

pub(crate) fn run_search(
    args: SearchArgs,
    data_root: PathBuf,
    telemetry: &mut SearchTelemetry,
    _provider_refreshes: &mut ProviderRefreshCollector,
    _config: &config::AppConfig,
) -> Result<()> {
    crate::commands::source_index::run_search(args, data_root, telemetry)
}
