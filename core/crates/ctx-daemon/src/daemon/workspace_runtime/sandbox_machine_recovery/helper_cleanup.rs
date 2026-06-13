use std::path::Path;

use ctx_harness_setup::{
    observe_log, HarnessSetupLogLevel, HarnessSetupObserver, HarnessSetupPhase,
};

mod detection;
mod kill;

#[cfg(test)]
pub(in crate::daemon::workspace_runtime) use detection::collect_ctx_managed_sandbox_helper_pids_from_ps_output;
#[cfg(any(test, not(unix)))]
pub(in crate::daemon::workspace_runtime) use detection::{
    collect_ctx_managed_sandbox_helper_pids, is_ctx_managed_sandbox_helper_process_command,
};
pub(in crate::daemon::workspace_runtime) use kill::kill_ctx_managed_sandbox_helper_processes;
#[cfg(test)]
pub(in crate::daemon::workspace_runtime) use kill::literal_pkill_pattern;

#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
pub(super) fn cleanup_ctx_managed_sandbox_helper_processes(
    data_root: &Path,
    machine_name: &str,
    observer: Option<&dyn HarnessSetupObserver>,
) {
    let outcome = kill_ctx_managed_sandbox_helper_processes(data_root, machine_name);
    if !outcome.killed.is_empty() {
        observe_log(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            HarnessSetupLogLevel::Warn,
            &format!(
                "killed stale ctx-managed sandbox helper process(es) before recovery: {}",
                outcome
                    .killed
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
    if !outcome.failed.is_empty() {
        observe_log(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            HarnessSetupLogLevel::Warn,
            &format!(
                "failed to kill stale ctx-managed sandbox helper process(es) before recovery: {}",
                outcome
                    .failed
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
    if !outcome.skipped.is_empty() {
        observe_log(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            HarnessSetupLogLevel::Warn,
            &format!(
                "skipped killing stale ctx-managed sandbox helper process(es) after command-scoped cleanup no longer matched the original helper identity: {}",
                outcome
                    .skipped
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
}
