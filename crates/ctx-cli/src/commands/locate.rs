use std::path::PathBuf;

use anyhow::Result;

use crate::analytics::LocateTelemetry;
use crate::commands::source_index;
use crate::LocateArgs;

pub(crate) fn run_locate(
    args: LocateArgs,
    data_root: PathBuf,
    _telemetry: &mut LocateTelemetry,
) -> Result<()> {
    source_index::run_locate(args, data_root)
}
