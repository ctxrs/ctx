use std::path::Path;

use crate::daemon::workspace_runtime::{
    sandbox_machine_home_root, sandbox_machine_runtime_root, sandbox_machine_temp_root,
};

mod ps;

#[cfg(unix)]
pub(super) fn snapshot_ps_process_rows() -> Option<Vec<PsProcessRow>> {
    ps::snapshot_ps_process_rows()
}

fn collect_ps_process_rows(output: &str) -> Vec<PsProcessRow> {
    ps::collect_ps_process_rows(output)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PsProcessRow {
    pub(super) pid: u32,
    pub(super) command: String,
}

#[cfg(any(test, not(unix)))]
pub(in crate::daemon::workspace_runtime) fn is_ctx_managed_sandbox_helper_process_command(
    command: &[String],
    data_root: &Path,
    machine_name: &str,
) -> bool {
    let rendered = command.join("\n");
    is_ctx_managed_sandbox_helper_process_rendered(&rendered, data_root, machine_name)
}

pub(super) fn is_ctx_managed_sandbox_helper_process_rendered(
    rendered: &str,
    data_root: &Path,
    machine_name: &str,
) -> bool {
    if !rendered.contains("/gvproxy") && !rendered.contains("/vfkit") {
        return false;
    }

    if !rendered.contains(machine_name) {
        return false;
    }

    let scoped_roots = [
        data_root
            .join("managed")
            .join("runtimes")
            .join("sandbox-cli"),
        sandbox_machine_runtime_root(data_root),
        sandbox_machine_home_root(data_root),
        sandbox_machine_temp_root(data_root),
        data_root.join("sandbox-cli"),
    ];
    scoped_roots.iter().any(|root| {
        let root = root.to_string_lossy();
        rendered.contains(root.as_ref())
    })
}

#[cfg(any(test, not(unix)))]
pub(in crate::daemon::workspace_runtime) fn collect_ctx_managed_sandbox_helper_pids<I>(
    rows: I,
    data_root: &Path,
    machine_name: &str,
) -> Vec<u32>
where
    I: IntoIterator<Item = (u32, Vec<String>)>,
{
    let mut pids = rows
        .into_iter()
        .filter_map(|(pid, command)| {
            is_ctx_managed_sandbox_helper_process_command(&command, data_root, machine_name)
                .then_some(pid)
        })
        .collect::<Vec<_>>();
    pids.sort_unstable();
    pids.dedup();
    pids
}

pub(super) fn collect_ctx_managed_sandbox_helper_processes_from_ps_rows<I>(
    rows: I,
    data_root: &Path,
    machine_name: &str,
) -> Vec<PsProcessRow>
where
    I: IntoIterator<Item = PsProcessRow>,
{
    let mut processes = rows
        .into_iter()
        .filter(|row| {
            is_ctx_managed_sandbox_helper_process_rendered(&row.command, data_root, machine_name)
        })
        .collect::<Vec<_>>();
    processes.sort_by(|left, right| {
        left.pid
            .cmp(&right.pid)
            .then_with(|| left.command.cmp(&right.command))
    });
    processes.dedup_by(|left, right| left.pid == right.pid && left.command == right.command);
    processes
}

#[cfg(test)]
pub(in crate::daemon::workspace_runtime) fn collect_ctx_managed_sandbox_helper_pids_from_ps_output(
    output: &str,
    data_root: &Path,
    machine_name: &str,
) -> Vec<u32> {
    let mut pids = collect_ps_process_rows(output)
        .into_iter()
        .filter_map(|row| {
            is_ctx_managed_sandbox_helper_process_rendered(&row.command, data_root, machine_name)
                .then_some(row.pid)
        })
        .collect::<Vec<_>>();
    pids.sort_unstable();
    pids.dedup();
    pids
}
