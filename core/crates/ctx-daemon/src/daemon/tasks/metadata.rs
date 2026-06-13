use anyhow::Result;
use ctx_core::ids::TaskId;
use ctx_core::models::Task;

use crate::daemon::task_route_handles::{TaskReadStateHandle, TaskTitleHandle};

impl TaskReadStateHandle {
    pub async fn mark_task_read(&self, task_id: TaskId) -> Result<Option<Task>> {
        self.set_task_read_state(task_id, true).await
    }

    pub async fn mark_task_unread(&self, task_id: TaskId) -> Result<Option<Task>> {
        self.set_task_read_state(task_id, false).await
    }

    async fn set_task_read_state(&self, task_id: TaskId, read: bool) -> Result<Option<Task>> {
        let Some(store) = self.task_store_or_none(task_id).await? else {
            return Ok(None);
        };
        let task = ctx_task_service::metadata::set_task_read_state(&store, task_id, read).await?;
        if let Some(task) = task.as_ref() {
            self.effects()
                .publish_task_updated(task_id, task.clone())
                .await;
        }
        Ok(task)
    }
}

impl TaskTitleHandle {
    pub async fn update_task_title(&self, task_id: TaskId, title: String) -> Result<Option<Task>> {
        let Some(store) = self.task_store_or_none(task_id).await? else {
            return Ok(None);
        };
        let Some(outcome) =
            ctx_task_service::metadata::update_task_title_record(&store, task_id, title).await?
        else {
            return Ok(None);
        };
        let session_ids = outcome
            .session_ids
            .iter()
            .map(|session_id| session_id.0.to_string())
            .collect();
        let worktree_id_strings = outcome
            .worktree_ids
            .iter()
            .map(|worktree_id| worktree_id.0.to_string())
            .collect();

        self.effects()
            .publish_task_updated(task_id, outcome.task.clone())
            .await;
        if let Err(error) = self
            .close_web_sessions_for_task(session_ids, worktree_id_strings)
            .await
        {
            tracing::warn!(task_id = %task_id.0, "failed to close web sessions for title update: {error:?}");
        }

        Ok(Some(outcome.task))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    use anyhow::anyhow;
    use ctx_core::ids::{SessionId, TaskId, WorktreeId};
    use ctx_core::models::{ExecutionEnvironment, Task, TaskDeltaKind, VcsKind};

    use crate::daemon::task_route_handles::{
        TaskCloseWebSessionsForTask, TaskMetadataEffects, TaskMetadataFuture,
    };
    use crate::test_support::{TaskLifecycleSessionSeed, TaskLifecycleWorktreeSeed, TestDaemon};

    #[derive(Default)]
    struct RecordedEffects {
        deltas: Mutex<Vec<(TaskId, TaskDeltaKind)>>,
        upserts: Mutex<Vec<TaskId>>,
    }

    fn recording_effects(recorded: Arc<RecordedEffects>) -> Arc<TaskMetadataEffects> {
        let deltas = Arc::clone(&recorded);
        let emit_workspace_task_delta = Arc::new(move |task: Task, kind: TaskDeltaKind| {
            let deltas = Arc::clone(&deltas);
            Box::pin(async move {
                deltas
                    .deltas
                    .lock()
                    .expect("delta calls")
                    .push((task.id, kind));
            }) as TaskMetadataFuture<_>
        });
        let upserts = Arc::clone(&recorded);
        let emit_workspace_task_upsert = Arc::new(move |task_id: TaskId| {
            let upserts = Arc::clone(&upserts);
            Box::pin(async move {
                upserts.upserts.lock().expect("upsert calls").push(task_id);
                Ok(())
            }) as TaskMetadataFuture<_>
        });
        TaskMetadataEffects::new(emit_workspace_task_delta, emit_workspace_task_upsert)
    }

    fn assert_single_updated_delta(recorded: &RecordedEffects, task_id: TaskId) {
        let deltas = recorded.deltas.lock().expect("deltas");
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].0, task_id);
        assert!(matches!(deltas[0].1, TaskDeltaKind::Updated));
    }

    async fn seeded_task_fixture() -> (tempfile::TempDir, TestDaemon, Task) {
        let temp = tempfile::tempdir().expect("tempdir");
        let daemon = TestDaemon::new_for_test(
            temp.path().join("data"),
            "http://127.0.0.1:4310".to_string(),
        )
        .await
        .expect("daemon");
        let workspace_root = temp.path().join("repo");
        std::fs::create_dir_all(&workspace_root).expect("workspace root");
        let workspace = daemon
            .seed_task_lifecycle_workspace_for_test("ws", &workspace_root, VcsKind::Git)
            .await
            .expect("workspace");
        let task = daemon
            .seed_task_lifecycle_task_for_test(workspace.id, "task")
            .await
            .expect("task");
        (temp, daemon, task)
    }

    async fn seeded_title_fixture() -> (tempfile::TempDir, TestDaemon, Task, SessionId, WorktreeId)
    {
        let (temp, daemon, task) = seeded_task_fixture().await;
        let worktree_id = WorktreeId::new();
        let worktree_root = temp.path().join("worktree");
        std::fs::create_dir_all(&worktree_root).expect("worktree root");
        let worktree = daemon
            .seed_task_lifecycle_worktree_for_test(TaskLifecycleWorktreeSeed {
                workspace_id: task.workspace_id,
                owner_task_id: task.id,
                worktree_id,
                root_path: worktree_root,
                base_commit: "base".to_string(),
                git_branch: "task-branch".to_string(),
                make_primary: true,
            })
            .await
            .expect("worktree");
        let session = daemon
            .seed_task_lifecycle_session_for_test(TaskLifecycleSessionSeed {
                task_id: task.id,
                workspace_id: task.workspace_id,
                worktree_id: worktree.id,
                execution_environment: ExecutionEnvironment::Host,
                title: "session".to_string(),
                parent_session_id: None,
                role: None,
            })
            .await
            .expect("session");
        (temp, daemon, task, session.id, worktree.id)
    }

    #[tokio::test]
    async fn read_state_publishes_updated_delta_and_upsert() {
        let (_temp, daemon, task) = seeded_task_fixture().await;
        let recorded = Arc::new(RecordedEffects::default());
        let handle = daemon
            .task_read_state_handle_for_test()
            .with_effects_for_test(recording_effects(Arc::clone(&recorded)));

        let updated = handle
            .mark_task_read(task.id)
            .await
            .expect("mark read")
            .expect("task");

        assert!(updated.assistant_seen_at.is_some());
        assert_single_updated_delta(&recorded, task.id);
        assert_eq!(
            recorded.upserts.lock().expect("upserts").as_slice(),
            &[task.id]
        );
    }

    #[tokio::test]
    async fn title_update_publishes_updated_delta_upsert_and_close_ids() {
        let (_temp, daemon, task, session_id, worktree_id) = seeded_title_fixture().await;
        let recorded = Arc::new(RecordedEffects::default());
        let close_calls = Arc::new(Mutex::new(Vec::<(HashSet<String>, HashSet<String>)>::new()));
        let close_web_sessions_for_task: TaskCloseWebSessionsForTask = Arc::new({
            let close_calls = Arc::clone(&close_calls);
            move |session_ids, worktree_ids| {
                let close_calls = Arc::clone(&close_calls);
                Box::pin(async move {
                    close_calls
                        .lock()
                        .expect("close calls")
                        .push((session_ids, worktree_ids));
                    Ok(1_usize)
                }) as TaskMetadataFuture<_>
            }
        });
        let handle = daemon
            .task_title_handle_for_test()
            .with_effects_and_close_for_test(
                recording_effects(Arc::clone(&recorded)),
                close_web_sessions_for_task,
            );

        let updated = handle
            .update_task_title(task.id, "renamed".to_string())
            .await
            .expect("update title")
            .expect("task");

        assert_eq!(updated.title, "renamed");
        assert_single_updated_delta(&recorded, task.id);
        assert_eq!(
            recorded.upserts.lock().expect("upserts").as_slice(),
            &[task.id]
        );
        let calls = close_calls.lock().expect("close calls");
        let (session_ids, worktree_ids) = calls.as_slice().first().expect("close call");
        assert_eq!(calls.len(), 1);
        assert!(session_ids.contains(&session_id.0.to_string()));
        assert!(worktree_ids.contains(&worktree_id.0.to_string()));
    }

    #[tokio::test]
    async fn title_update_close_failure_is_nonfatal() {
        let (_temp, daemon, task, _session_id, _worktree_id) = seeded_title_fixture().await;
        let recorded = Arc::new(RecordedEffects::default());
        let close_web_sessions_for_task: TaskCloseWebSessionsForTask =
            Arc::new(move |_session_ids, _worktree_ids| {
                Box::pin(async move { Err(anyhow!("close failed")) }) as TaskMetadataFuture<_>
            });
        let handle = daemon
            .task_title_handle_for_test()
            .with_effects_and_close_for_test(
                recording_effects(Arc::clone(&recorded)),
                close_web_sessions_for_task,
            );

        let updated = handle
            .update_task_title(task.id, "renamed despite close failure".to_string())
            .await
            .expect("title close failure should be nonfatal")
            .expect("task");

        assert_eq!(updated.title, "renamed despite close failure");
        assert_single_updated_delta(&recorded, task.id);
        assert_eq!(
            recorded.upserts.lock().expect("upserts").as_slice(),
            &[task.id]
        );
    }
}
