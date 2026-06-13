use std::path::Path;

use super::super::detection::{
    collect_ctx_managed_sandbox_helper_pids, is_ctx_managed_sandbox_helper_process_command,
};
use super::SandboxHelperCleanupOutcome;

pub(in crate::daemon::workspace_runtime) fn kill_ctx_managed_sandbox_helper_processes(
    data_root: &Path,
    machine_name: &str,
) -> SandboxHelperCleanupOutcome {
    let mut system = sysinfo::System::new_all();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let rows = system.processes().iter().map(|(pid, process)| {
        let pid_u32 = (*pid).as_u32();
        let command = process
            .cmd()
            .iter()
            .map(|part| part.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        (pid_u32, command)
    });
    let pids = collect_ctx_managed_sandbox_helper_pids(rows, data_root, machine_name);
    let mut outcome = SandboxHelperCleanupOutcome::default();
    for pid in pids {
        system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        let still_matches = system
            .process(sysinfo::Pid::from_u32(pid))
            .map(|process| {
                let command = process
                    .cmd()
                    .iter()
                    .map(|part| part.to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                is_ctx_managed_sandbox_helper_process_command(&command, data_root, machine_name)
            })
            .unwrap_or(false);
        if !still_matches {
            outcome.skipped.push(pid);
            continue;
        }
        let killed = if let Some(process) = system.process(sysinfo::Pid::from_u32(pid)) {
            process.kill_with(sysinfo::Signal::Kill).unwrap_or(false)
        } else {
            false
        };
        if killed {
            outcome.killed.push(pid);
        } else {
            outcome.failed.push(pid);
        }
    }
    outcome
}
