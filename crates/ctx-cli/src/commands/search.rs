use std::path::PathBuf;

use anyhow::Result;
pub(crate) use ctx_cli_presentation::commands::search::*;

pub(crate) fn run_search(
    args: SearchArgs,
    data_root: PathBuf,
    telemetry: &mut crate::analytics::SearchTelemetry,
    config: ctx_history_cli::HistoryCliConfig,
    local_usage: &mut crate::local_usage::CliUsage,
    ui: &mut crate::ui::Ui,
) -> Result<()> {
    let human_output = !args.format.is_json();
    crate::semantic::take_spawned_daemon();
    let result = ctx_cli_presentation::commands::search::run_search(
        args,
        data_root,
        telemetry,
        config,
        local_usage,
        ui,
    );
    // Consume on success and failure: a persistent child can outlive an empty
    // index error or a failed readiness wait. Machine output stays unchanged.
    if let Some(root) = crate::semantic::take_spawned_daemon() {
        if human_output
            && !ctx_history_platform::managed_data_root().is_ok_and(|managed| managed == root)
        {
            ui.write_stderr(&render_persistent_daemon_notice(ui.stderr_context(), &root))?;
        }
    }
    result
}
