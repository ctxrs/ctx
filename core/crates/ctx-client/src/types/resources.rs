use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemSnapshot {
    pub cpu_pct: f32,
    pub memory_total_bytes: u64,
    pub memory_used_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskSnapshot {
    pub name: String,
    pub mount_point: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub file_system: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceChildProcess {
    pub pid: u32,
    #[serde(default)]
    pub parent_pid: Option<u32>,
    pub name: String,
    #[serde(default)]
    pub cmdline: Option<String>,
    pub cpu_pct: f32,
    pub memory_bytes: u64,
    pub virtual_memory_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceProcess {
    pub label: String,
    pub pid: u32,
    pub cpu_pct: f32,
    pub memory_bytes: u64,
    pub virtual_memory_bytes: u64,
    pub child_count: u64,
    pub children: Vec<ResourceChildProcess>,
    pub children_truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceProcesses {
    #[serde(default)]
    pub daemon: Option<ResourceProcess>,
    pub providers: Vec<ResourceProcess>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeDiskSnapshot {
    pub worktree_id: String,
    pub root_path: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceDiskSnapshot {
    pub workspace_id: String,
    pub root_path: String,
    pub size_bytes: u64,
    pub size_collected_at: String,
    pub size_cache_age_ms: u64,
    #[serde(default)]
    pub disk: Option<DiskSnapshot>,
    pub worktrees: Vec<WorktreeDiskSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceUtilizationSnapshot {
    pub collected_at: String,
    pub cache_age_ms: u64,
    pub system: SystemSnapshot,
    pub processes: ResourceProcesses,
    pub workspace: WorkspaceDiskSnapshot,
}
