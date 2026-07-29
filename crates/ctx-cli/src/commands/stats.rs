use std::path::PathBuf;

use anyhow::Result;

use crate::{
    local_usage::{self, UsageReport},
    output::print_json,
    StatsArgs,
};

/// Read and render the aggregate-only local report.
///
/// Dispatch excludes this command before constructing its completion draft, so
/// the detached read-only snapshot can never count the report itself.
pub(crate) fn run(args: StatsArgs, data_root: PathBuf, local_usage_enabled: bool) -> Result<()> {
    let report = local_usage::read_report(&data_root, local_usage_enabled, true);
    if args.format.is_json() {
        print_json(serde_json::to_value(report)?)
    } else {
        local_usage::render_human_summary(&report, args.detail);
        Ok(())
    }
}

pub(crate) fn malformed_config_failure(json_output: bool) -> Result<()> {
    let report = UsageReport::config_error();
    if json_output {
        eprintln!("{}", serde_json::to_string(&report)?);
    } else {
        eprintln!("local_usage_config_unavailable: local usage configuration could not be read");
    }
    Err(crate::dispatch::rendered_cli_error())
}
