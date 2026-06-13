use super::*;
use std::collections::HashMap;

use crate::test_support::DataRootTestDaemonFixture;
use ctx_core::models::VcsKind;
use ctx_daemon::test_support::{
    managed_worktree_path, standaloneize_worktree_git_dir, TaskLifecycleWorktreeSeed, TestDaemon,
};

fn git(args: &[&str], cwd: &StdPath) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn git_output(args: &[&str], cwd: &StdPath) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run git output");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn init_git_workspace(root: &StdPath) -> String {
    git(&["init"], root);
    git(&["symbolic-ref", "HEAD", "refs/heads/main"], root);
    git(&["config", "user.email", "ctx@example.com"], root);
    git(&["config", "user.name", "Ctx Test"], root);
    std::fs::write(root.join("README.md"), "hello\n").expect("write readme");
    git(&["add", "README.md"], root);
    git(&["commit", "-m", "initial"], root);
    git_output(&["rev-parse", "HEAD"], root)
}

async fn test_state(data_root: &StdPath) -> DataRootTestDaemonFixture {
    DataRootTestDaemonFixture::with_providers(data_root, HashMap::new(), "http://127.0.0.1:4310")
        .await
}

async fn insert_managed_worktree(
    state: &TestDaemon,
    data_root: &StdPath,
    workspace: &Workspace,
    owner_task_id: TaskId,
    repo_root: &StdPath,
    base_commit: &str,
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
            make_primary: true,
        })
        .await
        .expect("insert worktree");
    (worktree, managed_root)
}

#[tokio::test]
async fn delete_task_prunes_and_deletes_branch_for_standalone_managed_worktree() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_root = temp.path().join("repo");
    std::fs::create_dir_all(&repo_root).expect("create repo root");
    let base_commit = init_git_workspace(&repo_root);
    let fixture = test_state(temp.path()).await;
    let state = fixture.daemon();
    let workspace = state
        .seed_task_lifecycle_workspace_for_test("ws", &repo_root, VcsKind::Git)
        .await
        .expect("create workspace");
    let task = state
        .seed_task_lifecycle_task_for_test(workspace.id, "task")
        .await
        .expect("create task");

    let (worktree, managed_root) = insert_managed_worktree(
        &state,
        temp.path(),
        &workspace,
        task.id,
        &repo_root,
        &base_commit,
    )
    .await;
    let branch = worktree
        .git_branch
        .clone()
        .expect("managed worktree branch should exist");
    standaloneize_worktree_git_dir(&managed_root)
        .await
        .expect("standaloneize managed worktree");

    let tasks = task_api_lifecycle_state(&state);
    let status = delete_task(tasks, Path(task.id.0.to_string()))
        .await
        .expect("delete task");
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        tokio::fs::metadata(&managed_root).await.is_err(),
        "standalone managed worktree root should be removed on task delete"
    );
    assert_eq!(
        git_output(&["branch", "--list", &branch], &repo_root),
        "",
        "standalone managed worktree branch should be pruned and deleted"
    );
    assert!(
        !git_output(&["worktree", "list", "--porcelain"], &repo_root)
            .contains(managed_root.to_string_lossy().as_ref()),
        "standalone managed worktree should be pruned from git worktree metadata"
    );
}
