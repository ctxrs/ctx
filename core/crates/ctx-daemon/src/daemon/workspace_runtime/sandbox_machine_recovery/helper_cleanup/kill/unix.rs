use std::collections::HashMap;
use std::path::Path;
use std::process::Command as StdCommand;

use super::super::detection::{
    collect_ctx_managed_sandbox_helper_processes_from_ps_rows, snapshot_ps_process_rows,
};
use super::{literal_pkill_pattern, SandboxHelperCleanupOutcome};

fn kill_ctx_managed_sandbox_helper_command(command: &str) -> bool {
    let literal_pattern = literal_pkill_pattern(command);
    StdCommand::new("pkill")
        .arg("-9")
        .arg("-f")
        .arg("-x")
        .arg(literal_pattern)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub(in crate::daemon::workspace_runtime) fn kill_ctx_managed_sandbox_helper_processes(
    data_root: &Path,
    machine_name: &str,
) -> SandboxHelperCleanupOutcome {
    let Some(before_rows) = snapshot_ps_process_rows() else {
        return SandboxHelperCleanupOutcome::default();
    };
    let helpers = collect_ctx_managed_sandbox_helper_processes_from_ps_rows(
        before_rows,
        data_root,
        machine_name,
    );
    let mut outcome = SandboxHelperCleanupOutcome::default();
    if helpers.is_empty() {
        return outcome;
    }

    let mut kill_results = HashMap::new();
    for helper in &helpers {
        kill_results
            .entry(helper.command.clone())
            .or_insert_with(|| kill_ctx_managed_sandbox_helper_command(&helper.command));
    }

    let after_rows = snapshot_ps_process_rows().map(|rows| {
        rows.into_iter()
            .map(|row| (row.pid, row.command))
            .collect::<HashMap<_, _>>()
    });

    for helper in helpers {
        let pid = helper.pid;
        match after_rows
            .as_ref()
            .and_then(|rows| rows.get(&pid))
            .map(String::as_str)
        {
            Some(command_after) if command_after == helper.command => outcome.failed.push(pid),
            Some(_) => outcome.skipped.push(pid),
            None => {
                if kill_results.get(&helper.command).copied().unwrap_or(false) {
                    outcome.killed.push(pid);
                } else if after_rows.is_some() {
                    outcome.skipped.push(pid);
                } else {
                    outcome.failed.push(pid);
                }
            }
        }
    }
    outcome.killed.sort_unstable();
    outcome.failed.sort_unstable();
    outcome.skipped.sort_unstable();
    outcome
}
