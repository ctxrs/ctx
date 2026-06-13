use super::super::*;
use super::parse_single_u64_output;
#[cfg(target_os = "macos")]
use super::state::SharedVmResourceState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in super::super::super) enum SharedVmDataDiskGrowthDecision {
    NoAction,
    Grow {
        new_size_bytes: u64,
        additional_bytes: u64,
    },
    HostReserveBlocked {
        available_host_bytes: u64,
        reserve_bytes: u64,
        requested_additional_bytes: u64,
    },
}

pub(in super::super::super) fn resolve_shared_vm_data_disk_growth_decision(
    current_size_bytes: u64,
    guest_free_bytes: u64,
    host_available_bytes: u64,
) -> SharedVmDataDiskGrowthDecision {
    if guest_free_bytes >= SHARED_VM_DATA_DISK_GROWTH_THRESHOLD_BYTES {
        return SharedVmDataDiskGrowthDecision::NoAction;
    }

    let requested_additional_bytes = SHARED_VM_DATA_DISK_GROWTH_STEP_BYTES;
    let host_growth_budget = host_available_bytes.saturating_sub(SHARED_VM_HOST_DISK_RESERVE_BYTES);
    if host_growth_budget == 0 {
        return SharedVmDataDiskGrowthDecision::HostReserveBlocked {
            available_host_bytes: host_available_bytes,
            reserve_bytes: SHARED_VM_HOST_DISK_RESERVE_BYTES,
            requested_additional_bytes,
        };
    }

    let additional_bytes = requested_additional_bytes.min(host_growth_budget);
    SharedVmDataDiskGrowthDecision::Grow {
        new_size_bytes: current_size_bytes.saturating_add(additional_bytes),
        additional_bytes,
    }
}

#[cfg(all(target_os = "macos", unix))]
fn guest_mount_available_bytes(
    queue: &DispatchQueue,
    virtual_machine: &Retained<VZVirtualMachine>,
    mount_path: &str,
) -> Result<u64> {
    let result = run_owner_guest_exec_capture(
        queue,
        virtual_machine,
        Path::new("/"),
        "/bin/sh",
        &[
            "-lc".to_string(),
            format!("df -B1 {mount_path} | awk 'NR==2 {{print $4}}'"),
        ],
        Some("root"),
        HashMap::new(),
    )?;
    ensure_guest_exec_success(
        &format!("reading guest free bytes for {mount_path}"),
        GuestExecCaptureResult {
            exit_code: result.exit_code,
            stdout: result.stdout.clone(),
            stderr: result.stderr.clone(),
        },
    )?;
    parse_single_u64_output(
        &result.stdout,
        &format!("guest free bytes for {mount_path}"),
    )
}

#[cfg(all(target_os = "macos", unix))]
fn guest_data_disk_device_path(
    queue: &DispatchQueue,
    virtual_machine: &Retained<VZVirtualMachine>,
) -> Result<String> {
    let result = run_owner_guest_exec_capture(
        queue,
        virtual_machine,
        Path::new("/"),
        "/bin/sh",
        &["-lc".to_string(), "findmnt -n -o SOURCE /ctx".to_string()],
        Some("root"),
        HashMap::new(),
    )?;
    ensure_guest_exec_success(
        "reading guest data-disk device for /ctx",
        GuestExecCaptureResult {
            exit_code: result.exit_code,
            stdout: result.stdout.clone(),
            stderr: result.stderr.clone(),
        },
    )?;
    let device = String::from_utf8_lossy(&result.stdout).trim().to_string();
    if device.is_empty() {
        bail!("guest data disk mount `/ctx` resolved to an empty device path");
    }
    Ok(device)
}

#[cfg(all(target_os = "macos", unix))]
fn grow_guest_data_disk_filesystem(
    queue: &DispatchQueue,
    virtual_machine: &Retained<VZVirtualMachine>,
    device_path: &str,
) -> Result<()> {
    let device_path_escaped = shell_escape_single_quotes(device_path);
    let result = run_owner_guest_exec_capture(
        queue,
        virtual_machine,
        Path::new("/"),
        "/bin/sh",
        &[
            "-lc".to_string(),
            format!(
                "set -eu; device='{device_path}'; device_name=\"$(basename \"$device\")\"; echo 1 > \"/sys/class/block/$device_name/device/rescan\" 2>/dev/null || true; blockdev --rereadpt \"$device\" 2>/dev/null || true; resize2fs \"$device\"",
                device_path = device_path_escaped,
            ),
        ],
        Some("root"),
        HashMap::new(),
    )?;
    ensure_guest_exec_success(
        &format!("growing guest data-disk filesystem on {device_path}"),
        result,
    )
}

#[cfg(unix)]
fn host_available_disk_bytes(path: &Path) -> Result<u64> {
    use std::os::unix::ffi::OsStrExt;

    let path_cstr = std::ffi::CString::new(path.as_os_str().as_bytes())
        .with_context(|| format!("building C string for {}", path.display()))?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    let status = unsafe { libc::statfs(path_cstr.as_ptr(), stat.as_mut_ptr()) };
    if status != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("reading filesystem stats for {}", path.display()));
    }
    let stat = unsafe { stat.assume_init() };
    Ok((stat.f_bavail as u64).saturating_mul(stat.f_bsize as u64))
}

#[cfg(target_os = "macos")]
pub(in super::super) fn maybe_grow_shared_vm_data_disk(
    queue: &DispatchQueue,
    virtual_machine: &Retained<VZVirtualMachine>,
    data_root: &Path,
    resource_state: &mut SharedVmResourceState,
) -> Result<()> {
    if !shared_vm_owner_guest_probe_ready(data_root) {
        return Ok(());
    }
    let now = std::time::Instant::now();
    if now < resource_state.data_disk.next_check_at {
        return Ok(());
    }
    resource_state.data_disk.next_check_at = now + SHARED_VM_DATA_DISK_POLL_INTERVAL;

    let data_disk_path = shared_vm_data_disk_path(data_root);
    let current_size_bytes = fs::metadata(&data_disk_path)
        .with_context(|| format!("reading {}", data_disk_path.display()))?
        .len();
    let guest_free_bytes = guest_mount_available_bytes(queue, virtual_machine, "/ctx")?;
    let host_available_bytes = host_available_disk_bytes(&data_disk_path)?;

    match resolve_shared_vm_data_disk_growth_decision(
        current_size_bytes,
        guest_free_bytes,
        host_available_bytes,
    ) {
        SharedVmDataDiskGrowthDecision::NoAction => {
            resource_state.data_disk.last_growth_blocked = false;
            Ok(())
        }
        SharedVmDataDiskGrowthDecision::Grow {
            new_size_bytes,
            additional_bytes,
        } => {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(&data_disk_path)
                .with_context(|| format!("opening {} for growth", data_disk_path.display()))?;
            file.set_len(new_size_bytes).with_context(|| {
                format!(
                    "growing shared AVF data disk {} to {} bytes",
                    data_disk_path.display(),
                    new_size_bytes
                )
            })?;
            let device_path = guest_data_disk_device_path(queue, virtual_machine)?;
            grow_guest_data_disk_filesystem(queue, virtual_machine, &device_path)?;
            resource_state.data_disk.last_growth_blocked = false;
            append_shared_vm_log_line(
                data_root,
                &format!(
                    "grew AVF data disk {} by {:.2} GiB to {:.2} GiB after guest free space fell to {:.2} GiB",
                    data_disk_path.display(),
                    additional_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                    new_size_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                    guest_free_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                ),
            )?;
            Ok(())
        }
        SharedVmDataDiskGrowthDecision::HostReserveBlocked {
            available_host_bytes,
            reserve_bytes,
            requested_additional_bytes,
        } => {
            if !resource_state.data_disk.last_growth_blocked {
                append_shared_vm_log_line(
                    data_root,
                    &format!(
                        "AVF data-disk growth is blocked by the host reserve: available_host={:.2} GiB reserve={:.2} GiB requested_growth={:.2} GiB guest_free={:.2} GiB",
                        available_host_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                        reserve_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                        requested_additional_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                        guest_free_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                    ),
                )?;
            }
            resource_state.data_disk.last_growth_blocked = true;
            if guest_free_bytes < SHARED_VM_DATA_DISK_CRITICAL_FREE_BYTES {
                bail!(
                    "guest data disk at {} is below the critical free-space floor ({:.2} GiB free) and the host reserve prevents further growth",
                    data_disk_path.display(),
                    guest_free_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
                );
            }
            Ok(())
        }
    }
}
