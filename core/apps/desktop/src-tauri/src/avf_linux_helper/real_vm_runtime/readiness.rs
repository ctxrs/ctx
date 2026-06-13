use super::*;

const SHARED_VM_READINESS_PHASE_PREFIX: &str = "[ctx-avf-linux] readiness phase ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in super::super) struct SharedVmGuestReadinessReport {
    pub(in super::super) attempts: u32,
    pub(in super::super) elapsed: Duration,
    pub(in super::super) phase_lines: Vec<String>,
}

pub(in super::super) fn format_duration_ms(duration: Duration) -> String {
    format!("{} ms", duration.as_millis())
}

pub(in super::super) fn extract_shared_vm_readiness_phase_lines(
    stdout: &[u8],
    stderr: &[u8],
) -> Vec<String> {
    [stdout, stderr]
        .into_iter()
        .flat_map(|buffer| {
            String::from_utf8_lossy(buffer)
                .lines()
                .map(str::trim)
                .filter(|line| line.starts_with(SHARED_VM_READINESS_PHASE_PREFIX))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

pub(in super::super) fn summarize_shared_vm_readiness_phase_lines(
    phase_lines: &[String],
) -> String {
    if phase_lines.is_empty() {
        return "no per-phase readiness timings were emitted".to_string();
    }

    phase_lines
        .iter()
        .map(|line| {
            line.strip_prefix(SHARED_VM_READINESS_PHASE_PREFIX)
                .unwrap_or(line.as_str())
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub(in super::super) fn reset_writable_shared_vm_runtime_state(data_root: &Path) -> Result<()> {
    clear_shared_vm_shutdown_request(data_root);
    clear_shared_vm_memory_pressure_stop_request(data_root);
    for path in [
        shared_vm_control_socket_path(data_root),
        shared_vm_guest_agent_socket_path(data_root),
        shared_vm_guest_control_ready_path(data_root),
        shared_vm_guest_control_failed_path(data_root),
        shared_vm_saved_state_path(data_root),
        shared_vm_rootfs_path(data_root),
    ] {
        if path.exists() {
            fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        }
    }
    Ok(())
}

pub(in super::super) fn shared_vm_readiness_failure_requires_writable_rootfs_reset(
    err: &anyhow::Error,
) -> bool {
    let rendered = format!("{err:#}");
    rendered.contains("[ctx-avf-linux] bridge_probe_failed")
        || rendered.contains("writable-root-separate")
        || rendered.contains("root-on-writable-root")
        || rendered.contains("tmp-on-writable-root")
        || rendered.contains("var-tmp-on-writable-root")
        || rendered.contains("var-log-on-writable-root")
        || rendered.contains("containerd-root-on-writable-root")
        || rendered.contains("buildkit-root-on-writable-root")
        || rendered.contains("nerdctl-root-on-writable-root")
        || rendered.contains("cni-config-on-writable-root")
        || rendered.contains("cni-state-on-writable-root")
        || rendered.contains("guest-policy-masked-units")
}

#[cfg_attr(not(test), allow(dead_code))]
pub(in super::super) fn wait_for_guest_control_ready_marker(
    data_root: &Path,
    timeout: Duration,
) -> Result<()> {
    let marker_path = shared_vm_guest_control_ready_path(data_root);
    let failure_path = shared_vm_guest_control_failed_path(data_root);
    let guest_agent_log_path = shared_vm_guest_agent_log_path(data_root);
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if marker_path.exists() {
            return Ok(());
        }
        if failure_path.exists() {
            let failure = fs::read_to_string(&failure_path)
                .ok()
                .map(|contents| contents.trim().to_string())
                .filter(|contents| !contents.is_empty())
                .unwrap_or_else(|| {
                    format!(
                        "guest-control failure marker {} is present but empty",
                        failure_path.display()
                    )
                });
            bail!(
                "guest control failed before ready marker: {}; {}",
                failure,
                render_shared_vm_guest_agent_log_tail(&guest_agent_log_path)
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    bail!(
        "timed out waiting for guest control ready marker {}; {}",
        marker_path.display(),
        render_shared_vm_guest_agent_log_tail(&guest_agent_log_path)
    )
}

fn backfill_guest_control_ready_marker_for_restore_hit(data_root: &Path) -> Result<bool> {
    let state_path = shared_vm_state_path(data_root);
    let restore_hit = load_state(&state_path)?.and_then(|state| state.last_start_outcome)
        == Some(AvfLinuxSharedVmStartOutcome::Restored);
    if !restore_hit {
        return Ok(false);
    }

    let ready_marker = shared_vm_guest_control_ready_path(data_root);
    if let Some(parent) = ready_marker.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(
        &ready_marker,
        format!("listening:{SHARED_VM_GUEST_CONTROL_VSOCK_PORT}\n"),
    )
    .with_context(|| format!("writing {}", ready_marker.display()))?;

    let failure_marker = shared_vm_guest_control_failed_path(data_root);
    if failure_marker.exists() {
        fs::remove_file(&failure_marker)
            .with_context(|| format!("removing {}", failure_marker.display()))?;
    }

    Ok(true)
}

#[cfg(unix)]
fn render_shared_vm_guest_agent_log_tail(log_path: &Path) -> String {
    match fs::read_to_string(log_path) {
        Ok(contents) => {
            let tail = contents
                .lines()
                .rev()
                .take(20)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            if tail.is_empty() {
                format!("guest-agent log {} is empty", log_path.display())
            } else {
                format!(
                    "guest-agent log tail from {}:\n{}",
                    log_path.display(),
                    tail
                )
            }
        }
        Err(err) => format!(
            "unable to read guest-agent log {}: {err}",
            log_path.display()
        ),
    }
}

#[cfg(unix)]
pub(in super::super) fn shared_vm_guest_readiness_args() -> Vec<String> {
    let masked_units = SHARED_VM_GUEST_POLICY_MASKED_UNITS
        .iter()
        .map(|unit| format!("'{unit}'"))
        .collect::<Vec<_>>()
        .join(" ");
    vec![
        String::from("-lc"),
        format!(
            "set -e; ctx_uptime_ms() {{ awk '{{print int($1 * 1000)}}' /proc/uptime; }}; ctx_run_phase() {{ phase=\"$1\"; shift; start_ms=$(ctx_uptime_ms); if timeout --kill-after=1s --preserve-status {phase_timeout_seconds}s \"$@\"; then end_ms=$(ctx_uptime_ms); echo \"{phase_prefix}${{phase}} ok in $((end_ms-start_ms))ms\" >&2; else status=$?; end_ms=$(ctx_uptime_ms); echo \"{phase_prefix}${{phase}} failed with exit $status after $((end_ms-start_ms))ms\" >&2; return $status; fi; }}; ctx_assert_same_fs() {{ phase=\"$1\"; path=\"$2\"; ctx_run_phase \"$phase\" sh -lc '[ \"$(stat -fc %d \"$1\")\" = \"$(stat -fc %d \"$2\")\" ]' sh {writable_root} \"$path\"; }}; ctx_run_phase containerd systemctl is-active --quiet {containerd_service}; ctx_run_phase buildkit systemctl is-active --quiet {buildkit_service}; ctx_run_phase writable-root-separate sh -lc '[ \"$(stat -fc %d \"$1\")\" != \"$(stat -fc %d \"$2\")\" ]' sh / {writable_root}; ctx_assert_same_fs root-on-writable-root /root; ctx_assert_same_fs tmp-on-writable-root /tmp; ctx_assert_same_fs var-tmp-on-writable-root /var/tmp; ctx_assert_same_fs var-log-on-writable-root /var/log; ctx_assert_same_fs containerd-root-on-writable-root /var/lib/containerd; ctx_assert_same_fs buildkit-root-on-writable-root /var/lib/buildkit; ctx_assert_same_fs nerdctl-root-on-writable-root /var/lib/nerdctl; ctx_assert_same_fs cni-config-on-writable-root /etc/cni/net.d; ctx_assert_same_fs cni-state-on-writable-root /var/lib/cni; ctx_run_phase guest-policy-masked-units sh -lc 'set -e; for unit in {masked_units}; do mask_path=\"/etc/systemd/system/$unit\"; [ \"$(readlink \"$mask_path\")\" = \"/dev/null\" ]; enabled_state=\"$(systemctl is-enabled \"$unit\" 2>/dev/null || true)\"; case \"$enabled_state\" in masked|masked-runtime) ;; *) echo \"$unit expected masked state, got $enabled_state\" >&2; exit 1 ;; esac; if systemctl is-active --quiet \"$unit\"; then echo \"$unit unexpectedly active\" >&2; exit 1; fi; done'; ctx_run_phase nerdctl sh -lc '{nerdctl_bin} version >/dev/null 2>&1'; ctx_run_phase buildctl sh -lc '{buildctl_bin} --addr {buildkit_socket} debug workers >/dev/null 2>&1'; ctx_run_phase bridge-probe sh -lc 'probe_bridge=ctxavfbr0; ip link delete \"$probe_bridge\" >/dev/null 2>&1 || true; if ! ip link add name \"$probe_bridge\" type bridge >/tmp/ctx-avf-bridge-probe.out 2>/tmp/ctx-avf-bridge-probe.err; then cat /tmp/ctx-avf-bridge-probe.out >&2 || true; cat /tmp/ctx-avf-bridge-probe.err >&2 || true; echo \"[ctx-avf-linux] bridge_probe_failed\" >&2; exit 41; fi; ip link delete \"$probe_bridge\" >/dev/null 2>&1 || true'",
            containerd_service = SHARED_VM_CONTAINERD_SERVICE_NAME,
            buildkit_service = SHARED_VM_BUILDKIT_SERVICE_NAME,
            nerdctl_bin = SHARED_VM_GUEST_NERDCTL_BIN,
            buildctl_bin = SHARED_VM_GUEST_BUILDKITCTL_BIN,
            buildkit_socket = SHARED_VM_GUEST_BUILDKIT_SOCKET,
            writable_root = SHARED_VM_GUEST_WRITABLE_ROOT,
            masked_units = masked_units,
            phase_prefix = SHARED_VM_READINESS_PHASE_PREFIX,
            phase_timeout_seconds = SHARED_VM_READINESS_PHASE_TIMEOUT_SECONDS,
        ),
    ]
}

#[cfg(not(unix))]
pub(in super::super) fn shared_vm_guest_readiness_args() -> Vec<String> {
    Vec::new()
}

pub(in super::super) fn default_real_guest_exec_ready_timeout() -> Duration {
    Duration::from_secs(30)
}

pub(in super::super) fn cold_boot_real_guest_exec_ready_timeout() -> Duration {
    // Fresh Ubuntu cloud-image boots can spend several minutes in first-boot
    // cloud-init before the guest agent service is enabled and able to publish the
    // guest-control ready marker. Keep the cold-boot budget aligned with that real
    // first-run contract until the guest image is slimmed and preprovisioned enough
    // to move readiness materially earlier.
    Duration::from_secs(600)
}

pub(in super::super) fn real_guest_exec_ready_timeout_for_start(
    rootfs_materialization_note: Option<&str>,
    saved_state_exists: bool,
) -> Duration {
    if rootfs_materialization_note.is_some() || !saved_state_exists {
        cold_boot_real_guest_exec_ready_timeout()
    } else {
        default_real_guest_exec_ready_timeout()
    }
}

#[cfg(unix)]
fn shared_vm_readiness_owner_exit_error(
    data_root: &Path,
    status: std::process::ExitStatus,
) -> anyhow::Error {
    let log_path = shared_vm_log_path(data_root);
    let log_tail = match fs::read_to_string(&log_path) {
        Ok(contents) => {
            let tail = contents
                .lines()
                .rev()
                .take(20)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            if tail.is_empty() {
                format!("shared-vm log {} is empty", log_path.display())
            } else {
                format!("shared-vm log tail from {}:\n{}", log_path.display(), tail)
            }
        }
        Err(err) => format!("unable to read shared-vm log {}: {err}", log_path.display()),
    };
    anyhow::anyhow!(
        "shared AVF VM owner exited before guest exec readiness with status {status}; {log_tail}"
    )
}

#[cfg(unix)]
pub(in super::super) fn wait_for_real_guest_exec_ready_with_owner_process(
    data_root: &Path,
    timeout: Duration,
    mut owner_process: Option<&mut std::process::Child>,
) -> Result<SharedVmGuestReadinessReport> {
    let control_socket = shared_vm_control_socket_path(data_root);
    let started_at = std::time::Instant::now();
    let deadline = std::time::Instant::now() + timeout;
    let mut last_err: Option<anyhow::Error> = None;
    let readiness_args = shared_vm_guest_readiness_args();
    let mut attempts = 0_u32;
    while std::time::Instant::now() < deadline {
        if let Some(owner_process) = owner_process.as_deref_mut() {
            if let Some(status) = owner_process
                .try_wait()
                .context("polling shared AVF VM owner process")?
            {
                return Err(shared_vm_readiness_owner_exit_error(data_root, status));
            }
        }
        attempts += 1;
        match run_guest_exec_capture_with_socket_timeout(
            &control_socket,
            Path::new("/"),
            "/bin/sh",
            &readiness_args,
            Some("root"),
            HashMap::new(),
            None,
            Some(SHARED_VM_READINESS_GUEST_EXEC_IO_TIMEOUT),
        ) {
            Ok(result) if result.exit_code == 0 => {
                return Ok(SharedVmGuestReadinessReport {
                    attempts,
                    elapsed: started_at.elapsed(),
                    phase_lines: extract_shared_vm_readiness_phase_lines(
                        &result.stdout,
                        &result.stderr,
                    ),
                });
            }
            Ok(result) => {
                let stderr = String::from_utf8_lossy(&result.stderr).trim().to_string();
                let stdout = String::from_utf8_lossy(&result.stdout).trim().to_string();
                last_err = Some(anyhow::anyhow!(
                    "guest exec readiness probe exited {} (stdout='{}', stderr='{}')",
                    result.exit_code,
                    stdout,
                    stderr
                ));
            }
            Err(err) => {
                last_err = Some(err);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    if let Some(owner_process) = owner_process.as_deref_mut() {
        if let Some(status) = owner_process
            .try_wait()
            .context("polling shared AVF VM owner process")?
        {
            return Err(shared_vm_readiness_owner_exit_error(data_root, status));
        }
    }
    let timeout_message = format!(
        "timed out waiting for real AVF guest exec readiness via {} after {} attempt(s) over {}",
        control_socket.display(),
        attempts,
        format_duration_ms(started_at.elapsed())
    );
    match last_err {
        Some(err) => Err(err.context(timeout_message)),
        None => Err(anyhow::anyhow!(timeout_message)),
    }
}

#[cfg(unix)]
pub(crate) fn wait_for_real_guest_launch_ready_with_owner_process(
    data_root: &Path,
    timeout: Duration,
    owner_process: Option<&mut std::process::Child>,
) -> Result<SharedVmGuestReadinessReport> {
    let started_at = std::time::Instant::now();
    let readiness =
        wait_for_real_guest_exec_ready_with_owner_process(data_root, timeout, owner_process)?;
    if shared_vm_guest_control_ready_path(data_root).is_file()
        || backfill_guest_control_ready_marker_for_restore_hit(data_root)?
    {
        return Ok(readiness);
    }
    let marker_timeout = timeout.saturating_sub(started_at.elapsed());
    wait_for_guest_control_ready_marker(data_root, marker_timeout).with_context(|| {
        format!(
            "guest exec readiness succeeded but the guest control ready marker did not appear within {}",
            format_duration_ms(timeout)
        )
    })?;
    Ok(readiness)
}

#[cfg(not(unix))]
pub(crate) fn wait_for_real_guest_launch_ready_with_owner_process(
    _data_root: &Path,
    _timeout: Duration,
    _owner_process: Option<&mut std::process::Child>,
) -> Result<SharedVmGuestReadinessReport> {
    Ok(SharedVmGuestReadinessReport {
        attempts: 0,
        elapsed: Duration::ZERO,
        phase_lines: Vec::new(),
    })
}

#[cfg(not(unix))]
pub(in super::super) fn wait_for_real_guest_exec_ready_with_owner_process(
    _data_root: &Path,
    _timeout: Duration,
    _owner_process: Option<&mut std::process::Child>,
) -> Result<SharedVmGuestReadinessReport> {
    Ok(SharedVmGuestReadinessReport {
        attempts: 0,
        elapsed: Duration::ZERO,
        phase_lines: Vec::new(),
    })
}

#[cfg(unix)]
#[cfg_attr(not(test), allow(dead_code))]
pub(in super::super) fn wait_for_real_guest_exec_ready(
    data_root: &Path,
    timeout: Duration,
) -> Result<SharedVmGuestReadinessReport> {
    wait_for_real_guest_exec_ready_with_owner_process(data_root, timeout, None)
}

#[cfg(not(unix))]
#[cfg_attr(not(test), allow(dead_code))]
pub(in super::super) fn wait_for_real_guest_exec_ready(
    _data_root: &Path,
    _timeout: Duration,
) -> Result<SharedVmGuestReadinessReport> {
    Ok(SharedVmGuestReadinessReport {
        attempts: 0,
        elapsed: Duration::ZERO,
        phase_lines: Vec::new(),
    })
}
