use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use ctx_sandbox_container_runtime::{
    command_output_with_timeout, sandbox_container_command, SandboxCommandMode,
};
use ctx_storage_admission::{
    check_storage_admission, storage_admission_required_bytes, StorageAdmissionOperation,
    StorageAdmissionSample,
};

const DISK_ISOLATED_COPY_OVERHEAD_BYTES: u64 = 64 * 1024 * 1024;

fn disk_isolated_copy_budget_bytes(source_bytes: u64) -> u64 {
    source_bytes.saturating_add(DISK_ISOLATED_COPY_OVERHEAD_BYTES)
}

fn host_storage_sample(path: &Path, label: &str) -> Result<StorageAdmissionSample> {
    let free_bytes = fs2::available_space(path)
        .with_context(|| format!("checking free space for {}", path.display()))?;
    let total_bytes = fs2::total_space(path)
        .with_context(|| format!("checking total space for {}", path.display()))?;
    Ok(StorageAdmissionSample {
        label: label.to_string(),
        path: path.to_string_lossy().to_string(),
        mount_point: path.to_string_lossy().to_string(),
        free_bytes,
        total_bytes,
    })
}

fn parse_df_pk_line(line: &str, label: &str, path: &Path) -> Result<StorageAdmissionSample> {
    let parts = line.split_whitespace().collect::<Vec<_>>();
    if parts.len() < 6 {
        anyhow::bail!(
            "unexpected df output for {}: {}",
            path.display(),
            line.trim()
        );
    }
    let total_bytes = parts[1]
        .parse::<u64>()
        .with_context(|| format!("parsing total blocks from df output: {line}"))?
        .saturating_mul(1024);
    let free_bytes = parts[3]
        .parse::<u64>()
        .with_context(|| format!("parsing free blocks from df output: {line}"))?
        .saturating_mul(1024);
    let mount_point = parts[5..].join(" ");
    Ok(StorageAdmissionSample {
        label: label.to_string(),
        path: path.to_string_lossy().to_string(),
        mount_point,
        free_bytes,
        total_bytes,
    })
}

fn parse_df_pk_output(output: &str, label: &str, path: &Path) -> Result<StorageAdmissionSample> {
    let line = output
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty() && !line.starts_with("Filesystem"))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unexpected df output for {}: {}",
                path.display(),
                output.trim()
            )
        })?;
    parse_df_pk_line(line, label, path)
}

async fn sandbox_storage_sample(
    data_root: &Path,
    mode: &SandboxCommandMode,
    container_id: &str,
    path: &Path,
    label: &str,
) -> Result<StorageAdmissionSample> {
    const SANDBOX_EXEC_TIMEOUT: Duration = Duration::from_secs(60);
    let mut cmd = sandbox_container_command(data_root, mode)?;
    cmd.arg("exec")
        .arg("--interactive")
        .arg(container_id)
        .arg("df")
        .arg("-Pk")
        .arg("--")
        .arg(path);
    let out = command_output_with_timeout(cmd, SANDBOX_EXEC_TIMEOUT)
        .await
        .with_context(|| format!("querying sandbox free space for {}", path.display()))?;
    if !out.status.success() {
        anyhow::bail!(
            "failed to query sandbox free space for {} (status {}): {}",
            path.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    parse_df_pk_output(String::from_utf8_lossy(&out.stdout).trim(), label, path)
}

type TestPreflightStorageSamplesFn = dyn Fn(
        &Path,
        &SandboxCommandMode,
        &str,
        u64,
        &Path,
        StorageAdmissionOperation,
        u64,
    ) -> Result<(StorageAdmissionSample, StorageAdmissionSample)>
    + Send
    + Sync
    + 'static;

fn test_preflight_storage_samples_override(
) -> &'static Mutex<Option<std::sync::Arc<TestPreflightStorageSamplesFn>>> {
    static OVERRIDE: OnceLock<Mutex<Option<std::sync::Arc<TestPreflightStorageSamplesFn>>>> =
        OnceLock::new();
    OVERRIDE.get_or_init(|| Mutex::new(None))
}

fn lock_test_preflight_storage_samples_override(
) -> MutexGuard<'static, Option<std::sync::Arc<TestPreflightStorageSamplesFn>>> {
    test_preflight_storage_samples_override()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub struct TestPreflightStorageSamplesOverrideGuard;

impl Drop for TestPreflightStorageSamplesOverrideGuard {
    fn drop(&mut self) {
        let mut slot = lock_test_preflight_storage_samples_override();
        *slot = None;
    }
}

pub fn set_test_preflight_storage_samples_override(
    override_fn: std::sync::Arc<TestPreflightStorageSamplesFn>,
) -> TestPreflightStorageSamplesOverrideGuard {
    let mut slot = lock_test_preflight_storage_samples_override();
    assert!(
        slot.is_none(),
        "test storage override already installed for disk-isolated preflight"
    );
    *slot = Some(override_fn);
    TestPreflightStorageSamplesOverrideGuard
}

fn maybe_test_preflight_storage_samples(
    data_root: &Path,
    mode: &SandboxCommandMode,
    container_id: &str,
    estimated_copy_bytes: u64,
    destination_probe_root: &Path,
    operation: StorageAdmissionOperation,
    required_bytes: u64,
) -> Result<Option<(StorageAdmissionSample, StorageAdmissionSample)>> {
    let override_fn = lock_test_preflight_storage_samples_override().clone();
    match override_fn {
        Some(override_fn) => override_fn(
            data_root,
            mode,
            container_id,
            estimated_copy_bytes,
            destination_probe_root,
            operation,
            required_bytes,
        )
        .map(Some),
        None => Ok(None),
    }
}

pub(super) async fn preflight_disk_isolated_copy(
    data_root: &Path,
    mode: &SandboxCommandMode,
    container_id: &str,
    estimated_copy_bytes: u64,
    destination_probe_root: &Path,
    operation: StorageAdmissionOperation,
) -> Result<()> {
    let required_bytes =
        storage_admission_required_bytes(disk_isolated_copy_budget_bytes(estimated_copy_bytes));
    let (host_sample, sandbox_sample) = if let Some(samples) = maybe_test_preflight_storage_samples(
        data_root,
        mode,
        container_id,
        estimated_copy_bytes,
        destination_probe_root,
        operation,
        required_bytes,
    )? {
        samples
    } else {
        (
            host_storage_sample(data_root, "CTX data root")?,
            sandbox_storage_sample(
                data_root,
                mode,
                container_id,
                destination_probe_root,
                "sandbox workspace volume",
            )
            .await?,
        )
    };
    check_storage_admission(
        operation,
        required_bytes,
        &[host_sample.clone(), sandbox_sample.clone()],
    )
    .map_err(|err| {
        tracing::warn!(
            operation = ?operation,
            estimated_copy_bytes,
            required_bytes,
            host_path = %host_sample.path,
            host_free_bytes = host_sample.free_bytes,
            sandbox_path = %sandbox_sample.path,
            sandbox_mount_point = %sandbox_sample.mount_point,
            sandbox_free_bytes = sandbox_sample.free_bytes,
            "storage admission denied disk-isolated materialization",
        );
        err.into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_df_pk_line_extracts_capacity_sample() {
        let sample = parse_df_pk_line(
            "overlay 10485760 1024 7340032 1% /ctx/ws",
            "sandbox workspace volume",
            Path::new("/ctx/ws/worktrees"),
        )
        .unwrap_or_else(|err| panic!("parse df output: {err:#}"));
        assert_eq!(sample.label, "sandbox workspace volume");
        assert_eq!(sample.mount_point, "/ctx/ws");
        assert_eq!(sample.total_bytes, 10485760_u64 * 1024);
        assert_eq!(sample.free_bytes, 7340032_u64 * 1024);
    }

    #[test]
    fn parse_df_pk_output_extracts_last_data_row() {
        let sample = parse_df_pk_output(
            "Filesystem 1024-blocks Used Available Capacity Mounted on\noverlay 10485760 1024 7340032 1% /ctx/ws\n",
            "sandbox workspace volume",
            Path::new("/ctx/ws"),
        )
        .unwrap_or_else(|err| panic!("parse df output: {err:#}"));
        assert_eq!(sample.mount_point, "/ctx/ws");
        assert_eq!(sample.total_bytes, 10485760_u64 * 1024);
        assert_eq!(sample.free_bytes, 7340032_u64 * 1024);
    }

    #[test]
    fn disk_isolated_copy_budget_uses_source_size_plus_overhead() {
        assert_eq!(
            disk_isolated_copy_budget_bytes(512),
            512 + DISK_ISOLATED_COPY_OVERHEAD_BYTES
        );
    }

    #[test]
    fn disk_isolated_storage_admission_denial_mentions_task_worktree() {
        let required_bytes =
            storage_admission_required_bytes(disk_isolated_copy_budget_bytes(512 * 1024 * 1024));
        let err = check_storage_admission(
            StorageAdmissionOperation::DiskIsolatedWorktreeMaterialization,
            required_bytes,
            &[
                StorageAdmissionSample {
                    label: "CTX data root".to_string(),
                    path: "/ctx-data".to_string(),
                    mount_point: "/".to_string(),
                    free_bytes: 5 * 1024 * 1024 * 1024,
                    total_bytes: 20 * 1024 * 1024 * 1024,
                },
                StorageAdmissionSample {
                    label: "sandbox workspace volume".to_string(),
                    path: "/ctx/ws/worktrees".to_string(),
                    mount_point: "/ctx/ws".to_string(),
                    free_bytes: 256 * 1024 * 1024,
                    total_bytes: 20 * 1024 * 1024 * 1024,
                },
            ],
        )
        .expect_err("low sandbox capacity should deny admission");
        assert!(err.to_string().contains("isolated task worktree"));
        assert!(err.to_string().contains("sandbox workspace volume"));
    }
}
