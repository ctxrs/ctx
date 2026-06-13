use std::time::Instant;

use ctx_core::ids::{WorkspaceId, WorktreeId};

use super::super::{CacheSweepConfig, CacheSweepStats, DaemonState};

impl DaemonState {
    pub(super) async fn sweep_workspace_runtime_caches(
        &self,
        now: Instant,
        config: CacheSweepConfig,
        stats: &mut CacheSweepStats,
    ) {
        {
            let mut cache = self.workspaces.file_completions_cache.lock().await;
            let expired: Vec<WorktreeId> = cache
                .iter()
                .filter_map(|(worktree_id, entry)| {
                    if now.duration_since(entry.last_access) >= config.session_ttl {
                        Some(*worktree_id)
                    } else {
                        None
                    }
                })
                .collect();
            for worktree_id in &expired {
                cache.remove(worktree_id);
            }
            stats.file_completions_evicted += expired.len();
        }
        {
            let mut cache = self
                .workspaces
                .workspace_file_completions_cache
                .lock()
                .await;
            let expired: Vec<WorkspaceId> = cache
                .iter()
                .filter_map(|(workspace_id, entry)| {
                    if now.duration_since(entry.last_access) >= config.workspace_ttl {
                        Some(*workspace_id)
                    } else {
                        None
                    }
                })
                .collect();
            for workspace_id in &expired {
                cache.remove(workspace_id);
            }
            stats.workspace_file_completions_evicted += expired.len();
        }
        {
            let mut cache = self.workspaces.git_status_snapshots.lock().await;
            let expired: Vec<WorktreeId> = cache
                .iter()
                .filter_map(|(worktree_id, entry)| {
                    if now.duration_since(entry.last_access) >= config.session_ttl {
                        Some(*worktree_id)
                    } else {
                        None
                    }
                })
                .collect();
            for worktree_id in &expired {
                cache.remove(worktree_id);
            }
            stats.git_status_evicted += expired.len();
        }
        {
            let mut cache = self.workspaces.worktree_vcs_snapshots.lock().await;
            let expired: Vec<WorktreeId> = cache
                .iter()
                .filter_map(|(worktree_id, entry)| {
                    if now.duration_since(entry.last_access) >= config.session_ttl {
                        Some(*worktree_id)
                    } else {
                        None
                    }
                })
                .collect();
            for worktree_id in &expired {
                cache.remove(worktree_id);
            }
            stats.worktree_vcs_evicted += expired.len();
        }
        {
            let mut cache = self.workspaces.workspace_active_snapshot_cache.lock().await;
            let expired: Vec<WorkspaceId> = cache
                .iter()
                .filter_map(|(workspace_id, entry)| {
                    if now.duration_since(entry.last_access) >= config.workspace_ttl {
                        Some(*workspace_id)
                    } else {
                        None
                    }
                })
                .collect();
            for workspace_id in &expired {
                cache.remove(workspace_id);
            }
            stats.workspace_snapshot_evicted += expired.len();
        }
        {
            let mut cache = self.workspaces.workspace_active_heads_cache.lock().await;
            let expired: Vec<WorkspaceId> = cache
                .iter()
                .filter_map(|(workspace_id, entry)| {
                    if now.duration_since(entry.last_access) >= config.workspace_ttl {
                        Some(*workspace_id)
                    } else {
                        None
                    }
                })
                .collect();
            for workspace_id in &expired {
                cache.remove(workspace_id);
            }
            stats.workspace_heads_evicted += expired.len();
        }
        {
            let mut cache = self.workspaces.worktree_bootstrap_gates.lock().await;
            let expired: Vec<WorktreeId> = cache
                .iter()
                .filter_map(|(worktree_id, entry)| {
                    if now.duration_since(entry.last_access) >= config.session_ttl {
                        Some(*worktree_id)
                    } else {
                        None
                    }
                })
                .collect();
            for worktree_id in &expired {
                cache.remove(worktree_id);
            }
            stats.worktree_bootstrap_evicted += expired.len();
        }
    }
}
