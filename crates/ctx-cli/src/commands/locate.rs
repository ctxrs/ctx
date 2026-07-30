use std::path::PathBuf;

use anyhow::Result;

use crate::analytics::LocateTelemetry;
use crate::commands::source_index;
use crate::local_usage::CliUsage;
use crate::ui::Ui;
use crate::LocateArgs;

pub(crate) fn run_locate(
    args: LocateArgs,
    data_root: PathBuf,
    _telemetry: &mut LocateTelemetry,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    source_index::run_locate(args, data_root, local_usage, ui)
}
