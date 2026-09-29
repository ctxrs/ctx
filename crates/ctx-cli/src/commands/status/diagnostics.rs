use std::{path::Path, time::Duration};

use anyhow::Result;
use ctx_cli_presentation::commands::status_diagnostics::{
    render_status_diagnostics, StatusDiagnosticReport,
};
use ctx_daemon_cli::DaemonDiagnosticSnapshot;

use crate::{
    ui::{ColorMode, Ui},
    StatusArgs,
};

pub(crate) fn run(
    args: &StatusArgs,
    data_root: Option<&Path>,
    seconds: u64,
    quiet: bool,
) -> Result<()> {
    let data_root = match data_root {
        Some(path) => path.to_path_buf(),
        None => ctx_history_platform::default_data_root()
            .map_err(|_| anyhow::anyhow!("ctx data root unavailable"))?,
    };
    let first = DaemonDiagnosticSnapshot::observe(&data_root);
    std::thread::sleep(Duration::from_secs(seconds));
    let last = DaemonDiagnosticSnapshot::observe(&data_root);
    let mut observation = last.observation_since(&first);
    // This writer belongs to the store, not necessarily the sampled daemon.
    // Keep only advisory progress; generation IDs and reader errors are private.
    observation["blame"] = match ctx_attribution::materialization_progress(&data_root) {
        Ok(Some(progress)) => serde_json::json!({
            "writer_observed": true, "phase": progress.phase,
            "completed_sources": progress.completed_sources,
            "total_sources": progress.total_sources,
            "applied_changes": progress.applied_changes,
        }),
        Ok(None) => serde_json::json!({"writer_observed": false}),
        Err(_) => serde_json::Value::Null,
    };
    observation["semantic"] = DaemonDiagnosticSnapshot::last_recorded_semantic_job(&data_root);
    let report = StatusDiagnosticReport::from_observation(seconds, &observation)
        .with_product_identity(crate::upgrade::ports::product_identity());
    // Sharing never depends on the terminal or --color setting.
    let mut ui = Ui::stdio(ColorMode::Never);
    if args.format.is_json() {
        let mut bytes = serde_json::to_vec_pretty(&report.to_json())?;
        bytes.push(b'\n');
        ui.write_stdout_bytes(&bytes)?;
    } else if !quiet {
        let document = render_status_diagnostics(ui.stdout_context(), &report);
        ui.write_stdout_bytes(document.render_plain().as_bytes())?;
    }
    ui.flush()?;
    Ok(())
}
