use super::super::{apply_workspace_snapshot_hydration_payload, WorkspaceSnapshotHydrationPayload};
use super::fixtures::{test_head, test_session_metadata, test_task};
use crate::daemon::state::WorkspaceRuntime;
use crate::daemon::workspaces::attachments::WorkspaceAttachmentMaterializationRuntime;
use chrono::Utc;
use ctx_core::ids::{SessionId, TaskId, WorkspaceId, WorktreeId};
use ctx_core::models::{
    SessionActivityState, SessionHeadSnapshot, SessionSnapshotSummary, SessionTurnStatus,
    WorkspaceActiveTaskSummary,
};
use ctx_workspace_active_snapshot::WorkspaceActiveSnapshotHub;
use ctx_worktree_vcs_service::WorktreeVcsSchedulerRuntime;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;

#[tokio::test]
async fn applying_workspace_hydration_payload_seeds_hub_with_loaded_snapshot_rev() {
    let workspace_id = WorkspaceId::new();
    let task_id = TaskId::new();
    let session_id = SessionId::new();
    let worktree_id = WorktreeId::new();
    let runtime = test_workspace_runtime();
    let payload = WorkspaceSnapshotHydrationPayload {
        snapshot_rev: 23,
        archived_rev: 6,
        tasks: vec![WorkspaceActiveTaskSummary {
            task: test_task(workspace_id, task_id, session_id),
            primary_session: SessionSnapshotSummary {
                session: test_session_metadata(workspace_id, task_id, session_id),
                last_message_at: None,
                last_message_preview: Some("canonical-summary".to_string()),
                last_event_seq: Some(44),
                projection_rev: 44,
                state_rev: 44,
                activity: SessionActivityState {
                    is_working: false,
                    last_turn_status: Some(SessionTurnStatus::Completed),
                },
                unread: None,
            },
            primary_session_head: None,
            sessions: Vec::new(),
            sort_at: Utc::now(),
        }],
        heads: vec![SessionHeadSnapshot {
            session: ctx_core::models::SessionMetadata {
                worktree_id,
                ..test_head(workspace_id, task_id, session_id).session
            },
            ..test_head(workspace_id, task_id, session_id)
        }],
    };

    apply_workspace_snapshot_hydration_payload(
        &runtime.workspace_active_snapshot,
        workspace_id,
        payload,
    )
    .await;

    let snapshot = runtime
        .workspace_active_snapshot
        .active_snapshot(workspace_id, i64::MAX)
        .await;
    assert_eq!(snapshot.snapshot_rev, 23);
    assert_eq!(snapshot.archived_rev, 6);
    assert_eq!(snapshot.active.tasks.len(), 1);

    let heads = runtime
        .workspace_active_snapshot
        .active_heads(workspace_id)
        .await;
    assert_eq!(heads.snapshot_rev, 23);
    assert_eq!(heads.heads.len(), 1);
    assert_eq!(heads.heads[0].session.id, session_id);
}

fn test_workspace_runtime() -> WorkspaceRuntime {
    WorkspaceRuntime {
        worktree_vcs_enabled: true,
        file_completions_cache: Arc::new(AsyncMutex::new(HashMap::new())),
        workspace_file_completions_cache: Arc::new(AsyncMutex::new(HashMap::new())),
        git_status_snapshots: AsyncMutex::new(HashMap::new()),
        worktree_vcs_snapshots: Arc::new(AsyncMutex::new(HashMap::new())),
        worktree_vcs_active: Arc::new(AsyncMutex::new(HashMap::new())),
        worktree_vcs_refresh_locks: Arc::new(AsyncMutex::new(HashMap::new())),
        worktree_vcs_open_panes: Arc::new(AsyncMutex::new(HashMap::new())),
        worktree_vcs_summary_gen: Arc::new(AsyncMutex::new(HashMap::new())),
        worktree_vcs_runtime: Arc::new(AsyncMutex::new(HashMap::new())),
        worktree_vcs_scheduler: WorktreeVcsSchedulerRuntime::with_concurrency(1),
        worktree_vcs_events: tokio::sync::broadcast::channel(1024).0,
        git_status_watchers: Arc::new(AsyncMutex::new(HashSet::new())),
        workspace_active_snapshot: Arc::new(WorkspaceActiveSnapshotHub::new()),
        workspace_active_snapshot_cache: Arc::new(AsyncMutex::new(HashMap::new())),
        workspace_active_heads_cache: Arc::new(AsyncMutex::new(HashMap::new())),
        worktree_bootstrap_gates: Arc::new(AsyncMutex::new(HashMap::new())),
        attachment_materialization: Arc::new(WorkspaceAttachmentMaterializationRuntime::new()),
    }
}
