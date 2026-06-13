use super::*;
use std::collections::HashMap;

use crate::test_support::DataRootTestDaemonFixture;
use ctx_core::models::{Task, VcsKind, Workspace, Worktree};
use ctx_daemon::test_support::{TaskLifecycleSnapshot, TaskLifecycleWorktreeSeed, TestDaemon};

#[path = "fixtures/git.rs"]
mod git;

pub(super) use git::{create_branch_lock, git, init_git_workspace};

pub(super) struct ManagedTaskFixture {
    pub(super) repo_root: PathBuf,
    pub(super) state: DataRootTestDaemonFixture,
    pub(super) workspace: Workspace,
    pub(super) task: Task,
    pub(super) worktree: Worktree,
    pub(super) managed_root: PathBuf,
}

pub(super) async fn create_managed_task_fixture(data_root: &StdPath) -> ManagedTaskFixture {
    let repo_root = data_root.join("repo");
    std::fs::create_dir_all(&repo_root).expect("create repo root");
    let base_commit = init_git_workspace(&repo_root);
    let state = test_state(data_root).await;
    let daemon = state.daemon();
    let workspace = daemon
        .seed_task_lifecycle_workspace_for_test("ws", &repo_root, VcsKind::Git)
        .await
        .expect("create workspace");
    let task = daemon
        .seed_task_lifecycle_task_for_test(workspace.id, "task")
        .await
        .expect("create task");
    let (worktree, managed_root) = insert_managed_worktree(
        daemon,
        data_root,
        &workspace,
        task.id,
        &repo_root,
        &base_commit,
        true,
    )
    .await;

    ManagedTaskFixture {
        repo_root,
        state,
        workspace,
        task,
        worktree,
        managed_root,
    }
}

pub(super) async fn test_state(data_root: &StdPath) -> DataRootTestDaemonFixture {
    DataRootTestDaemonFixture::with_providers(data_root, HashMap::new(), "http://127.0.0.1:4310")
        .await
}

pub(super) async fn save_test_execution_settings(
    state: &TestDaemon,
    execution: ctx_settings_model::ExecutionSettings,
) {
    state
        .save_task_lifecycle_execution_settings_for_test(execution)
        .await
        .expect("save runtime settings");
}

pub(super) struct EnvVarGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvVarGuard {
    pub(super) fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(prev) = self.prev.take() {
            std::env::set_var(self.key, prev);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

pub(super) async fn insert_managed_worktree(
    state: &TestDaemon,
    data_root: &StdPath,
    workspace: &Workspace,
    owner_task_id: TaskId,
    repo_root: &StdPath,
    base_commit: &str,
    make_primary: bool,
) -> (Worktree, PathBuf) {
    let worktree_id = WorktreeId::new();
    let managed_root = managed_worktree_path(data_root, workspace.id, worktree_id);
    let branch_name = format!("ctx/{}/{}", owner_task_id.0, worktree_id.0);
    git(
        &[
            "worktree",
            "add",
            "-b",
            &branch_name,
            managed_root.to_string_lossy().as_ref(),
            base_commit,
        ],
        repo_root,
    );
    let worktree = state
        .seed_task_lifecycle_worktree_for_test(TaskLifecycleWorktreeSeed {
            workspace_id: workspace.id,
            owner_task_id,
            worktree_id,
            root_path: managed_root.clone(),
            base_commit: base_commit.to_string(),
            git_branch: branch_name,
            make_primary,
        })
        .await
        .expect("insert worktree");
    (worktree, managed_root)
}

pub(super) async fn task_lifecycle_snapshot(
    state: &TestDaemon,
    workspace: WorkspaceId,
    task: TaskId,
    worktree: WorktreeId,
) -> TaskLifecycleSnapshot {
    state
        .task_lifecycle_snapshot_for_test(workspace, task, worktree)
        .await
        .expect("load task lifecycle snapshot")
}
