use std::fmt;

use chrono::Utc;
use serde::Serialize;

mod guard;

pub use guard::{
    StorageGuardObservedPath, StorageGuardReserveAction, StorageGuardReserveWarning,
    StorageGuardRuntime, STORAGE_GUARD_MONITOR_INTERVAL, STORAGE_GUARD_RESERVE_FILE_NAME,
};

pub const STORAGE_BYTES_MIB: u64 = 1024 * 1024;
pub const STORAGE_BYTES_GIB: u64 = 1024 * STORAGE_BYTES_MIB;
const EMERGENCY_FREE_BYTES: u64 = STORAGE_BYTES_GIB;

pub const STORAGE_GUARD_WARNING_FREE_BYTES: u64 = 2 * STORAGE_BYTES_GIB;
pub const STORAGE_GUARD_EMERGENCY_FREE_BYTES: u64 = STORAGE_BYTES_GIB;
pub const STORAGE_GUARD_RESERVE_BYTES: u64 = 512 * STORAGE_BYTES_MIB;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageAdmissionOperation {
    DiskIsolatedWorktreeMaterialization,
    DiskIsolatedWorkspaceMaterialization,
}

impl StorageAdmissionOperation {
    fn action_label(self) -> &'static str {
        match self {
            Self::DiskIsolatedWorktreeMaterialization => "creating an isolated task worktree",
            Self::DiskIsolatedWorkspaceMaterialization => "creating an isolated workspace copy",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StorageAdmissionPathStatus {
    pub label: String,
    pub path: String,
    pub mount_point: String,
    pub free_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageGuardLevel {
    #[default]
    Normal,
    Warning,
    Emergency,
}

pub type StorageGuardPathStatus = StorageAdmissionPathStatus;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StorageGuardStatus {
    pub level: StorageGuardLevel,
    pub warning_threshold_bytes: u64,
    pub emergency_threshold_bytes: u64,
    pub reserve_bytes: u64,
    pub reserve_file_active: bool,
    pub active: Option<StorageGuardPathStatus>,
    pub updated_at: String,
}

impl Default for StorageGuardStatus {
    fn default() -> Self {
        Self {
            level: StorageGuardLevel::Normal,
            warning_threshold_bytes: STORAGE_GUARD_WARNING_FREE_BYTES,
            emergency_threshold_bytes: STORAGE_GUARD_EMERGENCY_FREE_BYTES,
            reserve_bytes: STORAGE_GUARD_RESERVE_BYTES,
            reserve_file_active: false,
            active: None,
            updated_at: Utc::now().to_rfc3339(),
        }
    }
}

impl StorageGuardStatus {
    pub fn is_emergency(&self) -> bool {
        self.level == StorageGuardLevel::Emergency
    }

    pub fn same_meaningful_state(&self, other: &Self) -> bool {
        self.level == other.level
            && self.warning_threshold_bytes == other.warning_threshold_bytes
            && self.emergency_threshold_bytes == other.emergency_threshold_bytes
            && self.reserve_bytes == other.reserve_bytes
            && self.reserve_file_active == other.reserve_file_active
            && self.active == other.active
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageAdmissionSample {
    pub label: String,
    pub path: String,
    pub mount_point: String,
    pub free_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageAdmissionFailure {
    operation: StorageAdmissionOperation,
    required_bytes: u64,
    active: StorageAdmissionPathStatus,
}

impl StorageAdmissionFailure {
    pub fn operation(&self) -> StorageAdmissionOperation {
        self.operation
    }

    pub fn required_bytes(&self) -> u64 {
        self.required_bytes
    }

    pub fn active(&self) -> &StorageAdmissionPathStatus {
        &self.active
    }
}

impl fmt::Display for StorageAdmissionFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            storage_admission_message(self.operation, self.required_bytes, &self.active)
        )
    }
}

impl std::error::Error for StorageAdmissionFailure {}

pub fn storage_admission_required_bytes(estimated_write_bytes: u64) -> u64 {
    estimated_write_bytes.saturating_add(EMERGENCY_FREE_BYTES)
}

pub fn storage_exhaustion_message(active: Option<&StorageGuardPathStatus>) -> String {
    match active {
        Some(path) => format!(
            "Storage exhausted while saving assistant output on {}. Free space, then retry the session.",
            format_storage_guard_path_label(path)
        ),
        None => {
            "Storage exhausted while saving assistant output. Free space, then retry the session."
                .to_string()
        }
    }
}

pub fn storage_emergency_message(active: Option<&StorageGuardPathStatus>) -> String {
    match active {
        Some(path) => format!(
            "Storage is critically low on {}. CTX stopped agent runs to protect local data. Free space, then retry the session.",
            format_storage_guard_path_label(path)
        ),
        None => {
            "Storage is critically low. CTX stopped agent runs to protect local data. Free space, then retry the session."
                .to_string()
        }
    }
}

pub fn is_storage_exhaustion_error(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    normalized.contains("database or disk is full")
        || normalized.contains("no space left on device")
        || normalized.contains("sqlite_full")
        || normalized.contains("os error 28")
        || normalized.contains("insufficient storage capacity")
}

pub fn storage_admission_message(
    operation: StorageAdmissionOperation,
    required_bytes: u64,
    active: &StorageAdmissionPathStatus,
) -> String {
    format!(
        "Insufficient storage capacity for {} on {}. CTX needs {} free before starting this operation, but only {} is available. Free space, then retry.",
        operation.action_label(),
        format_path_label(active),
        format_storage_bytes(required_bytes),
        format_storage_bytes(active.free_bytes),
    )
}

pub fn check_storage_admission(
    operation: StorageAdmissionOperation,
    required_bytes: u64,
    samples: &[StorageAdmissionSample],
) -> std::result::Result<(), StorageAdmissionFailure> {
    let mut active: Option<StorageAdmissionPathStatus> = None;
    for sample in samples {
        let path = StorageAdmissionPathStatus {
            label: sample.label.clone(),
            path: sample.path.clone(),
            mount_point: sample.mount_point.clone(),
            free_bytes: sample.free_bytes,
            total_bytes: sample.total_bytes,
        };
        let should_replace = active
            .as_ref()
            .map(|current| path.free_bytes < current.free_bytes)
            .unwrap_or(true);
        if should_replace {
            active = Some(path);
        }
    }

    let Some(active) = active else {
        return Ok(());
    };
    if active.free_bytes >= required_bytes {
        return Ok(());
    }
    Err(StorageAdmissionFailure {
        operation,
        required_bytes,
        active,
    })
}

pub fn format_storage_guard_path_label(path: &StorageGuardPathStatus) -> String {
    if path.mount_point == path.path {
        return path.label.clone();
    }
    format!("{} ({})", path.label, path.mount_point)
}

fn format_storage_bytes(bytes: u64) -> String {
    if bytes >= STORAGE_BYTES_GIB {
        format!("{:.1} GiB", bytes as f64 / STORAGE_BYTES_GIB as f64)
    } else {
        format!("{:.0} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn format_path_label(path: &StorageAdmissionPathStatus) -> String {
    if path.label.is_empty() {
        path.path.clone()
    } else {
        format!("{} ({})", path.label, path.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_admission_denial_mentions_task_worktree() {
        let required_bytes = storage_admission_required_bytes(512 * 1024 * 1024);
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
        assert_eq!(
            err.operation(),
            StorageAdmissionOperation::DiskIsolatedWorktreeMaterialization
        );
        assert!(err.to_string().contains("isolated task worktree"));
        assert!(err.to_string().contains("sandbox workspace volume"));
    }

    #[test]
    fn storage_admission_denial_mentions_workspace_copy() {
        let required_bytes = storage_admission_required_bytes(2 * 1024 * 1024 * 1024);
        let err = check_storage_admission(
            StorageAdmissionOperation::DiskIsolatedWorkspaceMaterialization,
            required_bytes,
            &[StorageAdmissionSample {
                label: "sandbox workspace volume".to_string(),
                path: "/ctx/ws".to_string(),
                mount_point: "/ctx/ws".to_string(),
                free_bytes: 512 * 1024 * 1024,
                total_bytes: 20 * 1024 * 1024 * 1024,
            }],
        )
        .expect_err("low sandbox capacity should deny admission");
        assert!(err.to_string().contains("isolated workspace copy"));
    }

    #[test]
    fn storage_guard_transitions_ignore_updated_at_only_changes() {
        let base = StorageGuardStatus {
            level: StorageGuardLevel::Warning,
            warning_threshold_bytes: STORAGE_GUARD_WARNING_FREE_BYTES,
            emergency_threshold_bytes: STORAGE_GUARD_EMERGENCY_FREE_BYTES,
            reserve_bytes: STORAGE_GUARD_RESERVE_BYTES,
            reserve_file_active: true,
            active: Some(StorageGuardPathStatus {
                label: "CTX data root".to_string(),
                path: "/ctx-data".to_string(),
                mount_point: "/".to_string(),
                free_bytes: STORAGE_GUARD_WARNING_FREE_BYTES,
                total_bytes: 10 * STORAGE_BYTES_GIB,
            }),
            updated_at: "2026-04-11T20:10:00Z".to_string(),
        };
        let later = StorageGuardStatus {
            updated_at: "2026-04-11T20:10:02Z".to_string(),
            ..base.clone()
        };

        assert!(base.same_meaningful_state(&later));
    }

    #[test]
    fn storage_guard_messages_name_active_mount() {
        let active = StorageGuardPathStatus {
            label: "active worktree".to_string(),
            path: "/Volumes/work/repo".to_string(),
            mount_point: "/Volumes/work".to_string(),
            free_bytes: STORAGE_GUARD_EMERGENCY_FREE_BYTES,
            total_bytes: 10 * STORAGE_BYTES_GIB,
        };

        assert!(
            storage_emergency_message(Some(&active)).contains("active worktree (/Volumes/work)")
        );
        assert!(
            storage_exhaustion_message(Some(&active)).contains("active worktree (/Volumes/work)")
        );
    }

    #[test]
    fn storage_exhaustion_detection_matches_expected_errors() {
        assert!(is_storage_exhaustion_error("database or disk is full"));
        assert!(is_storage_exhaustion_error(
            "No space left on device (os error 28)"
        ));
        assert!(is_storage_exhaustion_error(
            "Insufficient storage capacity for creating an isolated task worktree"
        ));
        assert!(!is_storage_exhaustion_error("permission denied"));
    }
}
