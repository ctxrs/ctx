pub(crate) mod events;

use std::path::PathBuf;

use anyhow::Result;

use crate::analytics::ShowTelemetry;
use crate::local_usage::CliUsage;
use crate::ui::Ui;
use crate::{ListArgs, ListTarget};

pub(crate) use events::{EventQueryFormat, ListEventsArgs};

pub(crate) fn run_list(
    args: ListArgs,
    data_root: PathBuf,
    telemetry: &mut ShowTelemetry,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    match args.target {
        ListTarget::Events(args) => events::run(*args, data_root, telemetry, local_usage, ui),
    }
}
