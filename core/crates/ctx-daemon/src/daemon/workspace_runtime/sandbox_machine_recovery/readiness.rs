use super::*;

mod memory;
mod temp_state;

pub(super) use memory::configured_sandbox_machine_memory_mb;

pub(super) fn sandbox_machine_heartbeat_interval() -> Duration {
    Duration::from_millis(if cfg!(test) { 100 } else { 5_000 })
}

pub(in crate::daemon::workspace_runtime) fn sandbox_machine_temp_state_paths(
    data_root: &Path,
    machine_name: &str,
) -> Vec<PathBuf> {
    temp_state::sandbox_machine_temp_state_paths(data_root, machine_name)
}

#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
pub(super) fn clear_stale_sandbox_machine_temp_state(
    data_root: &Path,
    machine_name: &str,
    observer: Option<&dyn HarnessSetupObserver>,
) {
    temp_state::clear_stale_sandbox_machine_temp_state(data_root, machine_name, observer);
}

pub(super) async fn best_effort_start_machine_after_init(
    data_root: &Path,
    machine_name: &str,
    observer: Option<&dyn HarnessSetupObserver>,
    last_err: &mut String,
) -> Result<()> {
    let mut start = sandbox_container_command(data_root)?;
    start.arg("machine").arg("start").arg(machine_name);
    match command_output_with_timeout(start, SANDBOX_MACHINE_START_TIMEOUT).await {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let combined = command_output_message(&out);
            if !combined.is_empty() {
                *last_err = combined.clone();
            }
            observe_log(
                observer,
                HarnessSetupPhase::MachineStartOrInit,
                HarnessSetupLogLevel::Warn,
                &format!("sandbox machine start after init returned non-zero: {combined}"),
            );
            Ok(())
        }
        Err(err) => {
            *last_err = err.to_string();
            observe_log(
                observer,
                HarnessSetupPhase::MachineStartOrInit,
                HarnessSetupLogLevel::Warn,
                &format!("sandbox machine start after init failed: {err}"),
            );
            Ok(())
        }
    }
}

pub(super) fn format_heartbeat_elapsed(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    if minutes == 0 {
        format!("{seconds}s")
    } else {
        format!("{minutes}m {seconds}s")
    }
}

#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
pub(super) async fn wait_for_sandbox_machine_ready(
    data_root: &Path,
    observer: Option<&dyn HarnessSetupObserver>,
    success_message: &str,
    last_err: &mut String,
) -> Result<bool> {
    let deadline = tokio::time::Instant::now() + sandbox_machine_ready_timeout();
    let started = tokio::time::Instant::now();
    let mut last_heartbeat = started;
    while tokio::time::Instant::now() < deadline {
        let mut cmd = sandbox_container_command(data_root)?;
        cmd.arg("info");
        match command_output_with_timeout(cmd, SANDBOX_INFO_TIMEOUT).await {
            Ok(out) if out.status.success() => {
                observe_log(
                    observer,
                    HarnessSetupPhase::MachineStartOrInit,
                    HarnessSetupLogLevel::Info,
                    success_message,
                );
                persist_sandbox_machine_cache_to_shared_best_effort(data_root, observer).await;
                return Ok(true);
            }
            Ok(out) => {
                let combined = command_output_message(&out);
                if !combined.is_empty() {
                    *last_err = combined;
                }
            }
            Err(err) => *last_err = err.to_string(),
        }
        let now = tokio::time::Instant::now();
        if now.duration_since(last_heartbeat) >= sandbox_machine_heartbeat_interval() {
            observe_log(
                observer,
                HarnessSetupPhase::MachineStartOrInit,
                HarnessSetupLogLevel::Info,
                &format!(
                    "still waiting for local sandbox runtime readiness ({} elapsed)",
                    format_heartbeat_elapsed(started.elapsed())
                ),
            );
            observe_progress(
                observer,
                HarnessSetupProgressUpdate {
                    phase: HarnessSetupPhase::MachineStartOrInit,
                    active_download: None,
                },
            );
            last_heartbeat = now;
        }
        tokio::time::sleep(sandbox_machine_ready_poll_interval()).await;
    }
    Ok(false)
}
