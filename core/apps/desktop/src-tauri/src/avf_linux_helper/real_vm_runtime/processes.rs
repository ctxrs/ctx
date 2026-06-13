use super::guest_control::shared_vm_owner_guest_probe_ready;
use super::readiness::{
    cold_boot_real_guest_exec_ready_timeout, format_duration_ms,
    reset_writable_shared_vm_runtime_state,
    shared_vm_readiness_failure_requires_writable_rootfs_reset,
    summarize_shared_vm_readiness_phase_lines, wait_for_real_guest_launch_ready_with_owner_process,
};
use super::*;

pub(in super::super) fn stop_shared_vm_owner_after_readiness_failure(
    data_root: &Path,
    owner_pid: u32,
) -> Result<()> {
    append_shared_vm_log_line(
        data_root,
        &format!(
            "shared AVF Linux VM launch-ready failed; waiting up to {} for owner {owner_pid} to exit before writable-rootfs reset",
            format_duration_ms(SHARED_VM_SHUTDOWN_WAIT_TIMEOUT)
        ),
    )?;
    stop_shared_vm_server(owner_pid);
    if wait_for_process_exit(owner_pid, SHARED_VM_SHUTDOWN_WAIT_TIMEOUT) {
        return Ok(());
    }
    bail!(
        "shared AVF Linux VM owner {owner_pid} did not exit within {} after launch-ready failure",
        format_duration_ms(SHARED_VM_SHUTDOWN_WAIT_TIMEOUT)
    );
}

pub(super) fn spawn_shared_vm_memory_watchdog(data_root: &Path, owner_pid: u32) -> Result<u32> {
    let current_exe = std::env::current_exe().context("resolving helper executable path")?;
    let log_path = shared_vm_log_path(data_root);
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    let log_file_err = log_file
        .try_clone()
        .with_context(|| format!("cloning {}", log_path.display()))?;
    let child = Command::new(current_exe)
        .arg("watch-workspace-vm-memory")
        .arg(data_root)
        .arg(owner_pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file_err))
        .spawn()
        .context("spawning workspace AVF memory watchdog")?;
    Ok(child.id())
}

fn spawn_real_shared_vm_owner_once(data_root: &Path, readiness_timeout: Duration) -> Result<u32> {
    let current_exe = std::env::current_exe().context("resolving helper executable path")?;
    let log_path = shared_vm_log_path(data_root);
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    let log_file_err = log_file
        .try_clone()
        .with_context(|| format!("cloning {}", log_path.display()))?;
    let mut child = Command::new(current_exe)
        .arg("run-workspace-vm")
        .arg(data_root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file_err))
        .spawn()
        .context("spawning workspace AVF VM owner process")?;
    let owner_started_at = std::time::Instant::now();
    let control_socket_wait_started_at = std::time::Instant::now();
    wait_for_control_socket(data_root)?;
    append_shared_vm_log_line(
        data_root,
        &format!(
            "shared AVF Linux VM control socket became available in {} after owner spawn",
            format_duration_ms(control_socket_wait_started_at.elapsed())
        ),
    )?;
    let remaining_after_control_socket =
        readiness_timeout.saturating_sub(owner_started_at.elapsed());
    let readiness = match wait_for_real_guest_launch_ready_with_owner_process(
        data_root,
        remaining_after_control_socket,
        Some(&mut child),
    ) {
        Ok(report) => report,
        Err(err) => {
            stop_shared_vm_owner_after_readiness_failure(data_root, child.id())
                .context("stopping failed shared AVF VM owner after launch-ready error")?;
            return Err(err);
        }
    };
    debug_assert!(shared_vm_owner_guest_probe_ready(data_root));
    append_shared_vm_log_line(
        data_root,
        &format!(
            "shared AVF Linux guest control ready marker was observed before launch-ready completed after {}",
            format_duration_ms(owner_started_at.elapsed())
        ),
    )?;
    for phase_line in &readiness.phase_lines {
        append_shared_vm_log_line(data_root, phase_line)?;
    }
    append_shared_vm_log_line(
        data_root,
        &format!(
            "shared AVF Linux guest readiness completed in {} across {} attempt(s): {}",
            format_duration_ms(readiness.elapsed),
            readiness.attempts,
            summarize_shared_vm_readiness_phase_lines(&readiness.phase_lines),
        ),
    )?;
    append_shared_vm_log_line(
        data_root,
        &format!(
            "shared AVF Linux VM owner reached launch-ready in {} total",
            format_duration_ms(owner_started_at.elapsed())
        ),
    )?;
    Ok(child.id())
}

pub(in super::super) fn spawn_real_shared_vm_owner(
    data_root: &Path,
    readiness_timeout: Duration,
) -> Result<u32> {
    match spawn_real_shared_vm_owner_once(data_root, readiness_timeout) {
        Ok(pid) => Ok(pid),
        Err(err) if shared_vm_readiness_failure_requires_writable_rootfs_reset(&err) => {
            eprintln!(
                "[ctx-avf-linux] guest readiness detected a writable-surface contract failure; resetting writable rootfs and retrying once: {err:#}"
            );
            reset_writable_shared_vm_runtime_state(data_root)
                .context(
                    "resetting writable shared VM runtime state after a writable-surface readiness failure",
                )?;
            spawn_real_shared_vm_owner_once(data_root, cold_boot_real_guest_exec_ready_timeout())
                .context("retrying shared AVF VM owner boot after writable rootfs reset")
        }
        Err(err) => Err(err),
    }
}

pub(in super::super) fn spawn_shared_vm_server(data_root: &Path) -> Result<u32> {
    let current_exe = std::env::current_exe().context("resolving helper executable path")?;
    let log_path = shared_vm_log_path(data_root);
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    let log_file_err = log_file
        .try_clone()
        .with_context(|| format!("cloning {}", log_path.display()))?;
    let child = Command::new(current_exe)
        .arg("serve-workspace-vm")
        .arg(data_root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file_err))
        .spawn()
        .context("spawning workspace VM relay process")?;
    wait_for_control_socket(data_root)?;
    Ok(child.id())
}

pub(in super::super) fn spawn_guest_agent_server(data_root: &Path) -> Result<u32> {
    let current_exe = std::env::current_exe().context("resolving helper executable path")?;
    let log_path = shared_vm_log_path(data_root);
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    let log_file_err = log_file
        .try_clone()
        .with_context(|| format!("cloning {}", log_path.display()))?;
    let child = Command::new(current_exe)
        .arg("serve-guest-agent")
        .arg(data_root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file_err))
        .spawn()
        .context("spawning guest-agent relay process")?;
    wait_for_guest_agent_socket(data_root)?;
    Ok(child.id())
}

pub(in super::super) fn wait_for_socket_accepting_connections(
    socket_path: &Path,
    timeout: Duration,
    socket_label: &str,
) -> Result<()> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last_connect_error = None;
    while std::time::Instant::now() < deadline {
        match std::os::unix::net::UnixStream::connect(socket_path) {
            Ok(_) => return Ok(()),
            Err(err) => last_connect_error = Some(err),
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if let Some(err) = last_connect_error {
        bail!(
            "timed out waiting for {} {} to accept connections: {}",
            socket_label,
            socket_path.display(),
            err
        );
    }
    bail!(
        "timed out waiting for {} {} to accept connections",
        socket_label,
        socket_path.display()
    )
}

pub(in super::super) fn wait_for_control_socket(data_root: &Path) -> Result<()> {
    wait_for_socket_accepting_connections(
        &shared_vm_control_socket_path(data_root),
        std::time::Duration::from_secs(2),
        "shared VM control socket",
    )
}

pub(in super::super) fn wait_for_guest_agent_socket(data_root: &Path) -> Result<()> {
    wait_for_socket_accepting_connections(
        &shared_vm_guest_agent_socket_path(data_root),
        std::time::Duration::from_secs(2),
        "guest-agent control socket",
    )
}
