use std::collections::HashSet;

use anyhow::Result;
use ctx_core::ids::{SessionId, TaskId, WorktreeId};
use ctx_core::models::Task;
use ctx_store::Store;

#[derive(Debug)]
pub struct TaskTitleUpdateOutcome {
    pub task: Task,
    pub session_ids: HashSet<SessionId>,
    pub worktree_ids: HashSet<WorktreeId>,
}

pub async fn set_task_read_state(
    store: &Store,
    task_id: TaskId,
    read: bool,
) -> Result<Option<Task>> {
    let updated = if read {
        store.mark_task_read(task_id).await?
    } else {
        store.mark_task_unread(task_id).await?
    };
    if !updated {
        return Ok(None);
    }
    store.get_task_with_activity(task_id).await
}

pub async fn update_task_title_record(
    store: &Store,
    task_id: TaskId,
    title: String,
) -> Result<Option<TaskTitleUpdateOutcome>> {
    let updated = store.update_task_title(task_id, title).await?;
    if !updated {
        return Ok(None);
    }
    let Some(task) = store.get_task_with_activity(task_id).await? else {
        return Ok(None);
    };
    let sessions = store
        .list_sessions_for_task(task_id)
        .await
        .unwrap_or_default();
    let session_ids = sessions.iter().map(|session| session.id).collect();
    let mut worktree_ids = sessions
        .iter()
        .map(|session| session.worktree_id)
        .collect::<HashSet<_>>();
    if let Some(primary_worktree_id) = task.primary_worktree_id {
        worktree_ids.insert(primary_worktree_id);
    }

    let mut existing_worktree_ids = HashSet::new();
    for worktree_id in worktree_ids {
        match store.get_worktree(worktree_id).await {
            Ok(Some(worktree)) => {
                existing_worktree_ids.insert(worktree.id);
            }
            Ok(None) => {
                tracing::warn!(
                    task_id = %task_id.0,
                    worktree_id = %worktree_id.0,
                    "worktree missing for task title update"
                );
            }
            Err(error) => {
                tracing::warn!(
                    task_id = %task_id.0,
                    worktree_id = %worktree_id.0,
                    "failed to load worktree for task title update: {error:?}"
                );
            }
        }
    }

    Ok(Some(TaskTitleUpdateOutcome {
        task,
        session_ids,
        worktree_ids: existing_worktree_ids,
    }))
}
