use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use fs2::FileExt;
use tokio::sync::Mutex;

use ctx_resource_utilization::{disk_for_path, DiskSnapshot};

use crate::{
    StorageGuardLevel, StorageGuardPathStatus, StorageGuardStatus,
    STORAGE_GUARD_EMERGENCY_FREE_BYTES, STORAGE_GUARD_RESERVE_BYTES,
    STORAGE_GUARD_WARNING_FREE_BYTES,
};

pub const STORAGE_GUARD_MONITOR_INTERVAL: Duration = Duration::from_secs(2);
pub const STORAGE_GUARD_RESERVE_FILE_NAME: &str = ".storage-guard.reserve";

#[derive(Default)]
struct StorageGuardController {
    reserve_file_active: bool,
}

pub struct StorageGuardRuntime {
    controller: Mutex<StorageGuardController>,
    reserve_file_path: PathBuf,
    snapshot: RwLock<StorageGuardStatus>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageGuardReserveAction {
    Allocate,
    Release,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageGuardReserveWarning {
    pub action: StorageGuardReserveAction,
    pub reserve_file_path: PathBuf,
    pub message: String,
}

impl StorageGuardRuntime {
    pub fn new(data_root: &Path) -> Self {
        Self {
            controller: Mutex::new(StorageGuardController::default()),
            reserve_file_path: data_root.join(STORAGE_GUARD_RESERVE_FILE_NAME),
            snapshot: RwLock::new(StorageGuardStatus::default()),
        }
    }

    pub fn snapshot(&self) -> StorageGuardStatus {
        self.snapshot
            .read()
            .expect("storage guard snapshot lock poisoned") // EXCEPTION: panic-trap - critical section is a trivial clone; poisoning means a prior panic already occurred
            .clone()
    }

    pub fn publish(&self, snapshot: StorageGuardStatus) {
        *self
            .snapshot
            .write()
            .expect("storage guard snapshot lock poisoned") = snapshot; // EXCEPTION: panic-trap - critical section is a trivial assignment; poisoning means a prior panic already occurred
    }

    pub async fn evaluate(
        &self,
        data_root: &Path,
        observed_paths: &[StorageGuardObservedPath],
        disks: &[DiskSnapshot],
    ) -> (
        StorageGuardStatus,
        StorageGuardStatus,
        Vec<StorageGuardReserveWarning>,
    ) {
        let mut controller = self.controller.lock().await;
        let previous = self.snapshot();
        let mut warnings = Vec::new();

        let mut assessment = build_storage_assessment(
            data_root,
            observed_paths,
            disks,
            controller.reserve_file_active,
        );

        if assessment.status.level == StorageGuardLevel::Normal && !controller.reserve_file_active {
            match ensure_reserve_file_async(self.reserve_file_path.clone()).await {
                Ok(()) => {
                    controller.reserve_file_active = true;
                    assessment = build_storage_assessment(
                        data_root,
                        observed_paths,
                        disks,
                        controller.reserve_file_active,
                    );
                }
                Err(err) => warnings.push(StorageGuardReserveWarning {
                    action: StorageGuardReserveAction::Allocate,
                    reserve_file_path: self.reserve_file_path.clone(),
                    message: format!("{err:#}"),
                }),
            }
        }

        let should_release_reserve = assessment.status.level == StorageGuardLevel::Emergency
            && controller.reserve_file_active
            && assessment
                .status
                .active
                .as_ref()
                .and_then(|active| {
                    assessment
                        .reserve_mount_point
                        .as_ref()
                        .map(|reserve_mount| active.mount_point == *reserve_mount)
                })
                .unwrap_or(false);

        if should_release_reserve {
            match release_reserve_file_async(self.reserve_file_path.clone()).await {
                Ok(()) => {
                    controller.reserve_file_active = false;
                    assessment = build_storage_assessment(
                        data_root,
                        observed_paths,
                        disks,
                        controller.reserve_file_active,
                    );
                }
                Err(err) => warnings.push(StorageGuardReserveWarning {
                    action: StorageGuardReserveAction::Release,
                    reserve_file_path: self.reserve_file_path.clone(),
                    message: format!("{err:#}"),
                }),
            }
        }

        (previous, assessment.status, warnings)
    }

    pub fn sample_preflight(
        &self,
        data_root: &Path,
        observed_paths: &[StorageGuardObservedPath],
        disks: &[DiskSnapshot],
    ) -> (StorageGuardStatus, StorageGuardStatus) {
        let previous = self.snapshot();
        let snapshot = build_storage_assessment(
            data_root,
            observed_paths,
            disks,
            previous.reserve_file_active,
        )
        .status;
        (previous, snapshot)
    }
}

#[derive(Clone)]
pub struct StorageGuardObservedPath {
    pub label: &'static str,
    pub path: PathBuf,
}

impl StorageGuardObservedPath {
    pub fn new(label: &'static str, path: PathBuf) -> Self {
        Self { label, path }
    }
}

struct StorageAssessment {
    status: StorageGuardStatus,
    reserve_mount_point: Option<String>,
}

fn build_storage_assessment(
    data_root: &Path,
    observed_paths: &[StorageGuardObservedPath],
    disks: &[DiskSnapshot],
    reserve_file_active: bool,
) -> StorageAssessment {
    let reserve_mount_point = disk_for_path(data_root, disks).map(|disk| disk.mount_point);
    let mut active: Option<StorageGuardPathStatus> = None;
    for observed in observed_paths {
        let Some(disk) = disk_for_path(&observed.path, disks) else {
            continue;
        };
        let reserve_bonus = if reserve_file_active
            && reserve_mount_point
                .as_deref()
                .map(|mount| mount == disk.mount_point)
                .unwrap_or(false)
        {
            STORAGE_GUARD_RESERVE_BYTES
        } else {
            0
        };
        let sample = StorageGuardPathStatus {
            label: observed.label.to_string(),
            path: observed.path.to_string_lossy().to_string(),
            mount_point: disk.mount_point.clone(),
            free_bytes: disk.available_bytes.saturating_add(reserve_bonus),
            total_bytes: disk.total_bytes,
        };
        let should_replace = match active.as_ref() {
            Some(current) => sample.free_bytes < current.free_bytes,
            None => true,
        };
        if should_replace {
            active = Some(sample);
        }
    }

    let level = match active.as_ref().map(|path| path.free_bytes) {
        Some(bytes) if bytes <= STORAGE_GUARD_EMERGENCY_FREE_BYTES => StorageGuardLevel::Emergency,
        Some(bytes) if bytes <= STORAGE_GUARD_WARNING_FREE_BYTES => StorageGuardLevel::Warning,
        _ => StorageGuardLevel::Normal,
    };

    StorageAssessment {
        status: StorageGuardStatus {
            level,
            warning_threshold_bytes: STORAGE_GUARD_WARNING_FREE_BYTES,
            emergency_threshold_bytes: STORAGE_GUARD_EMERGENCY_FREE_BYTES,
            reserve_bytes: STORAGE_GUARD_RESERVE_BYTES,
            reserve_file_active,
            active,
            updated_at: Utc::now().to_rfc3339(),
        },
        reserve_mount_point,
    }
}

fn ensure_reserve_file(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create storage reserve directory {}",
                parent.to_string_lossy()
            )
        })?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| {
            format!(
                "failed to open storage reserve file {}",
                path.to_string_lossy()
            )
        })?;
    file.allocate(STORAGE_GUARD_RESERVE_BYTES)
        .with_context(|| {
            format!(
                "failed to allocate {} bytes for storage reserve file {}",
                STORAGE_GUARD_RESERVE_BYTES,
                path.to_string_lossy()
            )
        })?;
    file.set_len(STORAGE_GUARD_RESERVE_BYTES).with_context(|| {
        format!(
            "failed to set storage reserve file size for {}",
            path.to_string_lossy()
        )
    })?;
    Ok(())
}

async fn ensure_reserve_file_async(path: PathBuf) -> Result<()> {
    tokio::task::spawn_blocking(move || ensure_reserve_file(&path))
        .await
        .map_err(|error| anyhow!("storage reserve allocation task failed: {error}"))?
}

fn release_reserve_file(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    std::fs::remove_file(path).with_context(|| {
        format!(
            "failed to remove storage reserve file {}",
            path.to_string_lossy()
        )
    })
}

async fn release_reserve_file_async(path: PathBuf) -> Result<()> {
    tokio::task::spawn_blocking(move || release_reserve_file(&path))
        .await
        .map_err(|error| anyhow!("storage reserve release task failed: {error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{STORAGE_BYTES_GIB, STORAGE_BYTES_MIB};

    fn disk(mount_point: &str, available_bytes: u64, total_bytes: u64) -> DiskSnapshot {
        DiskSnapshot {
            name: mount_point.to_string(),
            mount_point: mount_point.to_string(),
            total_bytes,
            available_bytes,
            file_system: "apfs".to_string(),
        }
    }

    #[test]
    fn storage_assessment_uses_reserve_bytes_for_data_root_mount() {
        let data_root = PathBuf::from("/ctx-data");
        let observed = vec![
            StorageGuardObservedPath::new("CTX data root", data_root.clone()),
            StorageGuardObservedPath::new("temp storage", PathBuf::from("/tmp")),
        ];
        let assessment = build_storage_assessment(
            &data_root,
            &observed,
            &[disk("/", 768 * STORAGE_BYTES_MIB, 10 * STORAGE_BYTES_GIB)],
            true,
        );

        let active = assessment.status.active.expect("active path");
        assert_eq!(
            active.free_bytes,
            768 * STORAGE_BYTES_MIB + STORAGE_GUARD_RESERVE_BYTES
        );
        assert_eq!(assessment.status.level, StorageGuardLevel::Warning);
    }

    #[test]
    fn storage_assessment_prefers_lowest_free_mount() {
        let data_root = PathBuf::from("/ctx-data");
        let observed = vec![
            StorageGuardObservedPath::new("CTX data root", data_root.clone()),
            StorageGuardObservedPath::new("active worktree", PathBuf::from("/Volumes/work/repo")),
        ];
        let assessment = build_storage_assessment(
            &data_root,
            &observed,
            &[
                disk("/", 20 * STORAGE_BYTES_GIB, 100 * STORAGE_BYTES_GIB),
                disk(
                    "/Volumes/work",
                    900 * STORAGE_BYTES_MIB,
                    100 * STORAGE_BYTES_GIB,
                ),
            ],
            false,
        );

        let active = assessment.status.active.expect("active path");
        assert_eq!(active.label, "active worktree");
        assert_eq!(active.mount_point, "/Volumes/work");
        assert_eq!(assessment.status.level, StorageGuardLevel::Emergency);
    }
}
