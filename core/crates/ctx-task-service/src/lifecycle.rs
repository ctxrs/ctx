use std::collections::HashSet;

use ctx_core::ids::{SessionId, TaskId, WorktreeId};
use ctx_core::models::{SandboxBinding, Session, Task, Worktree};
use ctx_store::Store;

#[derive(Debug)]
pub enum TaskLifecycleError {
    NotFound,
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for TaskLifecycleError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

#[derive(Debug)]
pub struct LifecycleCleanupTarget {
    pub worktree: Worktree,
    pub sandbox_binding: Option<SandboxBinding>,
    pub destroy_worktree_on_cleanup: bool,
}

#[derive(Debug)]
pub struct ArchiveTaskPlan {
    pub sessions: Vec<Session>,
    pub session_ids: Vec<SessionId>,
    pub worktrees: Vec<Worktree>,
}

#[derive(Debug)]
pub struct UnarchiveWorktreePlan {
    pub session_ids: Vec<SessionId>,
    pub worktrees: Vec<Worktree>,
}

#[derive(Debug)]
pub struct DeleteTaskPlan {
    pub sessions: Vec<Session>,
    pub cleanup_targets: Vec<LifecycleCleanupTarget>,
}

pub async fn load_archive_task_plan(
    store: &Store,
    task: &Task,
) -> Result<ArchiveTaskPlan, TaskLifecycleError> {
    let sessions = store
        .list_all_sessions_for_task(task.id)
        .await
        .map_err(TaskLifecycleError::Internal)?;
    let session_ids = sessions.iter().map(|session| session.id).collect();
    let worktrees = load_task_worktrees(store, task, &sessions).await?;

    Ok(ArchiveTaskPlan {
        sessions,
        session_ids,
        worktrees,
    })
}

pub async fn archive_task_record(
    store: &Store,
    task_id: TaskId,
) -> Result<Task, TaskLifecycleError> {
    let updated = store
        .archive_task(task_id)
        .await
        .map_err(TaskLifecycleError::Internal)?;
    if !updated {
        return Err(TaskLifecycleError::NotFound);
    }
    load_task_with_activity(store, task_id).await
}

pub async fn collect_archive_cleanup_targets(
    store: &Store,
    task_id: TaskId,
    worktrees: &[Worktree],
) -> Vec<LifecycleCleanupTarget> {
    let mut cleanup_targets = Vec::new();
    for worktree in worktrees {
        let other_active = match store
            .count_active_tasks_for_worktree(worktree.id, Some(task_id))
            .await
        {
            Ok(count) => count > 0,
            Err(error) => {
                tracing::warn!(
                    task_id = %task_id.0,
                    worktree_id = %worktree.id.0,
                    "failed to check worktree usage: {error:#}"
                );
                true
            }
        };
        if other_active {
            continue;
        }
        cleanup_targets.push(LifecycleCleanupTarget {
            sandbox_binding: load_sandbox_binding_for_cleanup(
                store,
                task_id,
                worktree.id,
                "cleanup",
            )
            .await,
            worktree: worktree.clone(),
            destroy_worktree_on_cleanup: true,
        });
    }
    cleanup_targets
}

pub async fn load_unarchive_worktree_plan(
    store: &Store,
    task: &Task,
) -> Result<UnarchiveWorktreePlan, TaskLifecycleError> {
    let sessions = store
        .list_sessions_for_task(task.id)
        .await
        .map_err(TaskLifecycleError::Internal)?;
    let session_ids = sessions.iter().map(|session| session.id).collect();
    let worktrees = load_task_worktrees(store, task, &sessions).await?;

    Ok(UnarchiveWorktreePlan {
        session_ids,
        worktrees,
    })
}

pub async fn unarchive_task_record(
    store: &Store,
    task_id: TaskId,
) -> Result<Task, TaskLifecycleError> {
    let updated = store
        .unarchive_task(task_id)
        .await
        .map_err(TaskLifecycleError::Internal)?;
    if !updated {
        return Err(TaskLifecycleError::NotFound);
    }
    load_task_with_activity(store, task_id).await
}

pub async fn load_delete_task_plan(
    store: &Store,
    task: &Task,
) -> Result<DeleteTaskPlan, TaskLifecycleError> {
    let sessions = store
        .list_all_sessions_for_task(task.id)
        .await
        .map_err(TaskLifecycleError::Internal)?;
    let cleanup_targets = collect_task_delete_cleanup_targets(store, task, &sessions).await;

    Ok(DeleteTaskPlan {
        sessions,
        cleanup_targets,
    })
}

pub async fn delete_task_record(store: &Store, task_id: TaskId) -> Result<(), TaskLifecycleError> {
    let deleted = store
        .delete_task(task_id)
        .await
        .map_err(TaskLifecycleError::Internal)?;
    if !deleted {
        return Err(TaskLifecycleError::NotFound);
    }
    Ok(())
}

pub async fn delete_unused_worktree_records_after_cleanup(
    store: &Store,
    task: &Task,
    cleanup_targets: &[LifecycleCleanupTarget],
    cleanup_succeeded: bool,
) -> Vec<WorktreeId> {
    if !cleanup_succeeded {
        return Vec::new();
    }

    let mut deleted_worktree_ids = Vec::new();
    for target in cleanup_targets {
        if !target.destroy_worktree_on_cleanup {
            continue;
        }
        let deleted_worktree_row = match store.delete_worktree(target.worktree.id).await {
            Ok(deleted) => deleted,
            Err(error) => {
                tracing::warn!(
                    task_id = %task.id.0,
                    worktree_id = %target.worktree.id.0,
                    "failed to delete worktree row after task delete: {error:#}"
                );
                false
            }
        };
        if !deleted_worktree_row {
            tracing::warn!(
                task_id = %task.id.0,
                worktree_id = %target.worktree.id.0,
                "skipping worktree index deletion because worktree row was not deleted"
            );
            continue;
        }
        deleted_worktree_ids.push(target.worktree.id);
    }
    deleted_worktree_ids
}

pub async fn load_task_with_activity(
    store: &Store,
    task_id: TaskId,
) -> Result<Task, TaskLifecycleError> {
    store
        .get_task_with_activity(task_id)
        .await
        .map_err(TaskLifecycleError::Internal)?
        .ok_or(TaskLifecycleError::NotFound)
}

async fn collect_task_delete_cleanup_targets(
    store: &Store,
    task: &Task,
    sessions: &[Session],
) -> Vec<LifecycleCleanupTarget> {
    let mut worktree_ids: HashSet<WorktreeId> =
        sessions.iter().map(|session| session.worktree_id).collect();
    if let Some(primary_worktree_id) = task.primary_worktree_id {
        worktree_ids.insert(primary_worktree_id);
    }

    let mut cleanup_targets = Vec::new();
    for worktree_id in worktree_ids {
        let other_active = match store
            .count_active_tasks_for_worktree(worktree_id, Some(task.id))
            .await
        {
            Ok(count) => count > 0,
            Err(error) => {
                tracing::warn!(
                    task_id = %task.id.0,
                    worktree_id = %worktree_id.0,
                    "failed to check worktree usage: {error:#}"
                );
                true
            }
        };
        if other_active {
            continue;
        }
        let other_tasks = match store
            .count_tasks_for_worktree(worktree_id, Some(task.id))
            .await
        {
            Ok(count) => count > 0,
            Err(error) => {
                tracing::warn!(
                    task_id = %task.id.0,
                    worktree_id = %worktree_id.0,
                    "failed to check total worktree usage: {error:#}"
                );
                true
            }
        };
        let worktree = match store.get_worktree(worktree_id).await {
            Ok(Some(worktree)) => worktree,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(
                    task_id = %task.id.0,
                    worktree_id = %worktree_id.0,
                    "failed to load worktree for delete cleanup: {error:#}"
                );
                continue;
            }
        };
        cleanup_targets.push(LifecycleCleanupTarget {
            sandbox_binding: load_sandbox_binding_for_cleanup(
                store,
                task.id,
                worktree_id,
                "delete cleanup",
            )
            .await,
            worktree,
            destroy_worktree_on_cleanup: !other_tasks,
        });
    }
    cleanup_targets
}

async fn load_task_worktrees(
    store: &Store,
    task: &Task,
    sessions: &[Session],
) -> Result<Vec<Worktree>, TaskLifecycleError> {
    let mut worktree_ids: HashSet<WorktreeId> =
        sessions.iter().map(|session| session.worktree_id).collect();
    if let Some(primary_worktree_id) = task.primary_worktree_id {
        worktree_ids.insert(primary_worktree_id);
    }

    let mut seen = HashSet::new();
    let mut worktrees = Vec::new();
    for worktree_id in worktree_ids {
        if !seen.insert(worktree_id) {
            continue;
        }
        let worktree = store
            .get_worktree(worktree_id)
            .await
            .map_err(TaskLifecycleError::Internal)?
            .ok_or(TaskLifecycleError::NotFound)?;
        worktrees.push(worktree);
    }
    Ok(worktrees)
}

async fn load_sandbox_binding_for_cleanup(
    store: &Store,
    task_id: TaskId,
    worktree_id: WorktreeId,
    context: &'static str,
) -> Option<SandboxBinding> {
    match store.get_sandbox_binding(worktree_id).await {
        Ok(binding) => binding,
        Err(error) => {
            tracing::warn!(
                task_id = %task_id.0,
                worktree_id = %worktree_id.0,
                "failed to load sandbox binding for {context}: {error:#}"
            );
            None
        }
    }
}
