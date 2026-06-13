use std::path::Path;

use serde_json::json;
use tokio::process::Command;

use ctx_core::models::{Task, Workspace};

mod common;

async fn setup_git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    run_git(root, &["init"]).await;
    run_git(root, &["config", "user.email", "test@example.com"]).await;
    run_git(root, &["config", "user.name", "Test"]).await;
    tokio::fs::write(root.join("file.txt"), "hello\n")
        .await
        .unwrap();
    run_git(root, &["add", "."]).await;
    run_git(root, &["commit", "-m", "init"]).await;
    dir
}

async fn run_git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

async fn git_worktree_list(root: &Path) -> String {
    run_git(root, &["worktree", "list", "--porcelain"]).await
}

async fn branch_exists(root: &Path, branch: &str) -> bool {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .await
        .unwrap();
    output.status.success()
}

#[tokio::test]
async fn archive_and_unarchive_recreates_managed_worktrees() {
    let repo = setup_git_repo().await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;
    let base = &server.base_url;
    let client = &server.client;

    let ws: Workspace = client
        .post(format!("{base}/api/workspaces"))
        .json(&json!({"root_path": repo.path(), "name": "ws"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let task: Task = client
        .post(format!("{base}/api/workspaces/{}/tasks", ws.id.0))
        .json(&json!({
            "title": "archive me",
            "default_session": {
                "provider_id":"fake",
                "model_id":"fake-model",
                "execution_environment":"host"
            }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let primary_session_id = task
        .primary_session_id
        .expect("task creation should create a primary session");

    let first_child_resp = client
        .post(format!("{base}/api/tasks/{}/sessions", task.id.0))
        .json(&json!({
            "provider_id":"fake",
            "model_id":"fake-model",
            "execution_environment":"host",
            "parent_session_id": primary_session_id.0.to_string(),
            "relationship": "sub_agent"
        }))
        .send()
        .await
        .unwrap();
    assert!(
        first_child_resp.status().is_success(),
        "first child session creation failed: {}",
        first_child_resp.status()
    );

    let second_child_resp = client
        .post(format!("{base}/api/tasks/{}/sessions", task.id.0))
        .json(&json!({
            "provider_id":"fake",
            "model_id":"fake-model",
            "execution_environment":"host",
            "parent_session_id": primary_session_id.0.to_string(),
            "relationship": "sub_agent"
        }))
        .send()
        .await
        .unwrap();
    assert!(
        second_child_resp.status().is_success(),
        "second child session creation failed: {}",
        second_child_resp.status()
    );

    let managed_snapshot = fixture
        .daemon
        .task_archive_managed_worktrees_snapshot_for_test(ws.id, task.id)
        .await
        .unwrap();
    assert_eq!(managed_snapshot.session_count, 3);
    assert_eq!(
        managed_snapshot.managed_worktree_count,
        managed_snapshot.worktree_count
    );
    let managed_roots = managed_snapshot.managed_roots;
    let managed_branches = managed_snapshot.managed_branches;
    assert!(!managed_roots.is_empty());
    assert!(!managed_branches.is_empty());

    tokio::fs::write(managed_roots[0].join("dirty.txt"), "dirty")
        .await
        .unwrap();

    let list_before = git_worktree_list(repo.path()).await;
    for root in &managed_roots {
        let root_str = root.to_string_lossy();
        assert!(list_before.contains(root_str.as_ref()));
    }
    for branch in &managed_branches {
        assert!(branch_exists(repo.path(), branch).await);
    }

    let resp = client
        .post(format!("{base}/api/tasks/{}/archive", task.id.0))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    for root in &managed_roots {
        assert!(tokio::fs::metadata(root).await.is_err());
    }
    let list_archived = git_worktree_list(repo.path()).await;
    for root in &managed_roots {
        let root_str = root.to_string_lossy();
        assert!(!list_archived.contains(root_str.as_ref()));
    }
    for branch in &managed_branches {
        assert!(!branch_exists(repo.path(), branch).await);
    }

    let resp = client
        .post(format!("{base}/api/tasks/{}/unarchive", task.id.0))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    let list_after = git_worktree_list(repo.path()).await;
    for root in &managed_roots {
        assert!(tokio::fs::metadata(root).await.is_ok());
        let root_str = root.to_string_lossy();
        assert!(list_after.contains(root_str.as_ref()));
    }
    for branch in &managed_branches {
        assert!(branch_exists(repo.path(), branch).await);
    }
}
