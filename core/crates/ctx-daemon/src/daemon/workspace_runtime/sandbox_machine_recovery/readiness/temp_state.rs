use super::*;

pub(super) fn sandbox_machine_temp_state_paths(
    data_root: &Path,
    machine_name: &str,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let tmp_root = sandbox_machine_temp_root(data_root).join("sandbox-cli");
    paths.push(tmp_root.join("gvproxy.pid"));
    paths.push(tmp_root.join(format!("{machine_name}-api.sock")));
    paths.push(tmp_root.join(format!("{machine_name}-gvproxy.sock")));
    paths.push(tmp_root.join(format!("{machine_name}.sock")));
    let home_root = sandbox_machine_home_root(data_root).join(".sandbox-cli");
    paths.push(home_root.join(format!("{machine_name}-api.sock")));
    paths.push(home_root.join(format!("{machine_name}-gvproxy.sock")));
    paths
}

#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
pub(super) fn clear_stale_sandbox_machine_temp_state(
    data_root: &Path,
    machine_name: &str,
    observer: Option<&dyn HarnessSetupObserver>,
) {
    for path in sandbox_machine_temp_state_paths(data_root, machine_name) {
        if path.exists() {
            match std::fs::remove_file(&path) {
                Ok(()) => observe_log(
                    observer,
                    HarnessSetupPhase::MachineStartOrInit,
                    HarnessSetupLogLevel::Warn,
                    &format!("removed stale sandbox temp state {}", path.display()),
                ),
                Err(err) => observe_log(
                    observer,
                    HarnessSetupPhase::MachineStartOrInit,
                    HarnessSetupLogLevel::Warn,
                    &format!(
                        "failed to remove stale sandbox temp state {}: {err}",
                        path.display()
                    ),
                ),
            }
        }
    }
}
