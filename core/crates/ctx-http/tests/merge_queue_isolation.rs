use std::path::Path;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use ctx_core::models::{MergeQueueEntry, MergeQueueEntryStatus};
use ctx_fs::git::git_status_porcelain;
use serde_json::json;
use tokio::process::Command;

mod common;

const MERGE_QUEUE_CONFLICT_MESSAGE: &str = concat!(
    "Your merge queue submission produces conflicts with the current head. ",
    "Please rebase your changes, carefully considering the intent of your changes and the intent of the upstream changes. ",
    "If in doubt about how to resolve conflicts, please ask for help."
);

async fn append_file(path: &Path, text: &str) {
    let mut contents = tokio::fs::read_to_string(path).await.unwrap_or_default();
    contents.push_str(text);
    tokio::fs::write(path, contents).await.unwrap();
}

async fn git_output(root: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

async fn git_success(root: &Path, args: &[&str]) -> bool {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .await
        .unwrap();
    output.status.success()
}

#[tokio::test]
async fn merge_queue_accepts_unrebased_changes() {
    let repo =
        common::init_git_repo(&[("note.txt", "base\n"), (".gitignore", ".ctx/merge-queue\n")])
            .await;
    let target_branch = git_output(repo.path(), &["rev-parse", "--abbrev-ref", "HEAD"]).await;

    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let daemon = &fixture.daemon;
    let app = fixture.router();
    daemon.spawn_merge_queue_runner();

    let workspace = common::create_workspace(&app, repo.path(), "mq-unrebased").await;
    daemon
        .configure_merge_queue_for_test(
            workspace.id,
            &target_branch,
            "never",
            &["true"],
            false,
            None,
            None,
        )
        .await
        .unwrap();

    let worktree_root = tempfile::tempdir().unwrap();
    let feature_path = worktree_root.path().join("feature");
    common::run_git(
        repo.path(),
        &[
            "worktree",
            "add",
            feature_path.to_str().unwrap(),
            "-b",
            "feature",
        ],
    )
    .await;

    append_file(&feature_path.join("note.txt"), "feature\n").await;
    common::run_git(&feature_path, &["add", "note.txt"]).await;
    common::run_git(&feature_path, &["commit", "-m", "feature"]).await;

    append_file(&repo.path().join("target.txt"), "target\n").await;
    common::run_git(repo.path(), &["add", "target.txt"]).await;
    common::run_git(repo.path(), &["commit", "-m", "target"]).await;

    let feature_head = git_output(&feature_path, &["rev-parse", "HEAD"]).await;
    let worktree = daemon
        .seed_merge_queue_worktree_for_test(
            workspace.id,
            &feature_path,
            &feature_head,
            Some("feature"),
        )
        .await
        .unwrap();

    let (status, entry): (StatusCode, MergeQueueEntry) = common::json_request(
        &app,
        Method::POST,
        "/api/merge-queue/entries",
        Some(json!({
            "worktree_id": worktree.id.0.to_string(),
            "message": "mq-unrebased",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry = daemon
        .wait_for_merge_queue_entry_for_test(workspace.id, entry.id, Duration::from_secs(15))
        .await
        .unwrap();
    assert_eq!(entry.status, MergeQueueEntryStatus::Passed);
    let merge_queue_repo = repo.path().join(".ctx/merge-queue/repo");
    let mq_head = git_output(&merge_queue_repo, &["rev-parse", &target_branch]).await;
    assert_eq!(mq_head, entry.result_commit_sha.clone().unwrap());
}

#[tokio::test]
async fn merge_queue_conflict_message_and_cleanup() {
    let repo =
        common::init_git_repo(&[("note.txt", "base\n"), (".gitignore", ".ctx/merge-queue\n")])
            .await;
    let target_branch = git_output(repo.path(), &["rev-parse", "--abbrev-ref", "HEAD"]).await;

    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let daemon = &fixture.daemon;
    let app = fixture.router();
    daemon.spawn_merge_queue_runner();

    let workspace = common::create_workspace(&app, repo.path(), "mq-conflict").await;
    daemon
        .configure_merge_queue_for_test(
            workspace.id,
            &target_branch,
            "never",
            &["true"],
            false,
            None,
            None,
        )
        .await
        .unwrap();

    let worktree_root = tempfile::tempdir().unwrap();
    let feature_path = worktree_root.path().join("feature");
    common::run_git(
        repo.path(),
        &[
            "worktree",
            "add",
            feature_path.to_str().unwrap(),
            "-b",
            "feature",
        ],
    )
    .await;

    tokio::fs::write(feature_path.join("note.txt"), "feature\n")
        .await
        .unwrap();
    common::run_git(&feature_path, &["add", "note.txt"]).await;
    common::run_git(&feature_path, &["commit", "-m", "feature"]).await;

    tokio::fs::write(repo.path().join("note.txt"), "target\n")
        .await
        .unwrap();
    common::run_git(repo.path(), &["add", "note.txt"]).await;
    common::run_git(repo.path(), &["commit", "-m", "target"]).await;

    let feature_head = git_output(&feature_path, &["rev-parse", "HEAD"]).await;
    let worktree = daemon
        .seed_merge_queue_worktree_for_test(
            workspace.id,
            &feature_path,
            &feature_head,
            Some("feature"),
        )
        .await
        .unwrap();

    let (status, _body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        Method::POST,
        "/api/merge-queue/entries",
        Some(json!({
            "worktree_id": worktree.id.0.to_string(),
            "message": "mq-conflict",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let entry = daemon
        .latest_merge_queue_entry_for_test(workspace.id)
        .await
        .unwrap();
    let entry = daemon
        .wait_for_merge_queue_entry_for_test(workspace.id, entry.id, Duration::from_secs(15))
        .await
        .unwrap();
    assert_eq!(entry.status, MergeQueueEntryStatus::Conflict);
    assert_eq!(
        entry.error_message.as_deref(),
        Some(MERGE_QUEUE_CONFLICT_MESSAGE)
    );

    let worktree_path = repo
        .path()
        .join(".ctx/merge-queue/worktrees")
        .join(workspace.id.0.to_string())
        .join(entry.id.0.to_string());
    assert!(!worktree_path.exists());

    let merge_queue_repo = repo.path().join(".ctx/merge-queue/repo");
    let branch_ref = format!("refs/heads/ctx-merge-queue/{}", entry.id.0);
    let branch_exists = git_success(
        &merge_queue_repo,
        &["show-ref", "--verify", "--quiet", &branch_ref],
    )
    .await;
    assert!(!branch_exists);
}

#[tokio::test]
async fn merge_queue_verify_failure_keeps_target_branch_and_records_commit() {
    let repo =
        common::init_git_repo(&[("note.txt", "base\n"), (".gitignore", ".ctx/merge-queue\n")])
            .await;
    let target_branch = git_output(repo.path(), &["rev-parse", "--abbrev-ref", "HEAD"]).await;
    let base_head = git_output(repo.path(), &["rev-parse", "HEAD"]).await;

    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let daemon = &fixture.daemon;
    let app = fixture.router();
    daemon.spawn_merge_queue_runner();

    let workspace = common::create_workspace(&app, repo.path(), "mq-verify-fail").await;
    daemon
        .configure_merge_queue_for_test(
            workspace.id,
            &target_branch,
            "never",
            &["printf 'verify failed\\n' >&2; exit 7"],
            false,
            None,
            None,
        )
        .await
        .unwrap();

    let worktree_root = tempfile::tempdir().unwrap();
    let feature_path = worktree_root.path().join("feature");
    common::run_git(
        repo.path(),
        &[
            "worktree",
            "add",
            feature_path.to_str().unwrap(),
            "-b",
            "feature",
        ],
    )
    .await;

    append_file(&feature_path.join("note.txt"), "verify\n").await;
    common::run_git(&feature_path, &["add", "note.txt"]).await;
    common::run_git(&feature_path, &["commit", "-m", "verify"]).await;

    let feature_head = git_output(&feature_path, &["rev-parse", "HEAD"]).await;
    let worktree = daemon
        .seed_merge_queue_worktree_for_test(
            workspace.id,
            &feature_path,
            &feature_head,
            Some("feature"),
        )
        .await
        .unwrap();

    let (status, _body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        Method::POST,
        "/api/merge-queue/entries",
        Some(json!({
            "worktree_id": worktree.id.0.to_string(),
            "message": "mq-verify-fail",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let entry = daemon
        .latest_merge_queue_entry_for_test(workspace.id)
        .await
        .unwrap();
    let entry = daemon
        .wait_for_merge_queue_entry_for_test(workspace.id, entry.id, Duration::from_secs(15))
        .await
        .unwrap();
    assert_eq!(entry.status, MergeQueueEntryStatus::Failed);
    assert!(entry
        .error_message
        .as_deref()
        .unwrap_or_default()
        .starts_with("verify failed:"));
    let result_commit_sha = entry
        .result_commit_sha
        .clone()
        .expect("verify failure should retain candidate commit sha");
    assert_ne!(result_commit_sha, base_head);

    let merge_queue_repo = repo.path().join(".ctx/merge-queue/repo");
    let mq_head = git_output(&merge_queue_repo, &["rev-parse", &target_branch]).await;
    assert_eq!(mq_head, base_head);

    let run = daemon
        .latest_merge_queue_run_for_test(workspace.id, entry.id)
        .await
        .unwrap();
    assert_eq!(run.exit_code, Some(7));
    assert_eq!(
        run.result_commit_sha.as_deref(),
        Some(result_commit_sha.as_str())
    );

    let log_path = run.log_path.expect("log path missing");
    let log_contents = tokio::fs::read_to_string(log_path).await.unwrap();
    assert!(log_contents.contains("verify: printf 'verify failed\\n' >&2; exit 7"));
    assert!(log_contents.contains("verify failed"));
}

#[tokio::test]
async fn merge_queue_push_failure_does_not_advance_target_branch() {
    let repo =
        common::init_git_repo(&[("note.txt", "base\n"), (".gitignore", ".ctx/merge-queue\n")])
            .await;
    let target_branch = git_output(repo.path(), &["rev-parse", "--abbrev-ref", "HEAD"]).await;
    let base_head = git_output(repo.path(), &["rev-parse", "HEAD"]).await;

    let origin = tempfile::tempdir().unwrap();
    common::run_git(origin.path(), &["init", "--bare"]).await;
    common::run_git(
        repo.path(),
        &["remote", "add", "origin", origin.path().to_str().unwrap()],
    )
    .await;
    common::run_git(
        repo.path(),
        &["push", "-u", "origin", &format!("HEAD:{target_branch}")],
    )
    .await;
    let missing_origin = repo.path().join("missing-origin.git");
    common::run_git(
        repo.path(),
        &[
            "remote",
            "set-url",
            "origin",
            missing_origin.to_str().unwrap(),
        ],
    )
    .await;

    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let daemon = &fixture.daemon;
    let app = fixture.router();
    daemon.spawn_merge_queue_runner();

    let workspace = common::create_workspace(&app, repo.path(), "mq-push-fail").await;
    daemon
        .configure_merge_queue_for_test(
            workspace.id,
            &target_branch,
            "never",
            &["true"],
            true,
            Some("origin"),
            Some(&target_branch),
        )
        .await
        .unwrap();

    let worktree_root = tempfile::tempdir().unwrap();
    let feature_path = worktree_root.path().join("feature");
    common::run_git(
        repo.path(),
        &[
            "worktree",
            "add",
            feature_path.to_str().unwrap(),
            "-b",
            "feature",
        ],
    )
    .await;

    append_file(&feature_path.join("note.txt"), "push-fail\n").await;
    common::run_git(&feature_path, &["add", "note.txt"]).await;
    common::run_git(&feature_path, &["commit", "-m", "push-fail"]).await;

    let feature_head = git_output(&feature_path, &["rev-parse", "HEAD"]).await;
    let worktree = daemon
        .seed_merge_queue_worktree_for_test(
            workspace.id,
            &feature_path,
            &feature_head,
            Some("feature"),
        )
        .await
        .unwrap();

    let (status, _body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        Method::POST,
        "/api/merge-queue/entries",
        Some(json!({
            "worktree_id": worktree.id.0.to_string(),
            "message": "mq-push-fail",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let entry = daemon
        .latest_merge_queue_entry_for_test(workspace.id)
        .await
        .unwrap();
    let entry = daemon
        .wait_for_merge_queue_entry_for_test(workspace.id, entry.id, Duration::from_secs(15))
        .await
        .unwrap();
    assert_eq!(entry.status, MergeQueueEntryStatus::Failed);
    assert!(entry
        .error_message
        .as_deref()
        .unwrap_or_default()
        .starts_with("push_failed:"));
    assert_ne!(entry.result_commit_sha.as_deref(), Some(base_head.as_str()));

    let merge_queue_repo = repo.path().join(".ctx/merge-queue/repo");
    let mq_head = git_output(&merge_queue_repo, &["rev-parse", &target_branch]).await;
    assert_eq!(mq_head, base_head);

    let run = daemon
        .latest_merge_queue_run_for_test(workspace.id, entry.id)
        .await
        .unwrap();
    assert_eq!(run.result_commit_sha, entry.result_commit_sha);
    let log_path = run.log_path.expect("log path missing");
    let log_contents = tokio::fs::read_to_string(log_path).await.unwrap();
    assert!(log_contents.contains("push origin "));
    assert!(!log_contents.contains("advance target branch"));
}

#[tokio::test]
async fn merge_queue_push_on_success_updates_remote_after_success() {
    let repo =
        common::init_git_repo(&[("note.txt", "base\n"), (".gitignore", ".ctx/merge-queue\n")])
            .await;
    let target_branch = git_output(repo.path(), &["rev-parse", "--abbrev-ref", "HEAD"]).await;

    let origin = tempfile::tempdir().unwrap();
    common::run_git(origin.path(), &["init", "--bare"]).await;
    common::run_git(
        repo.path(),
        &["remote", "add", "origin", origin.path().to_str().unwrap()],
    )
    .await;
    common::run_git(
        repo.path(),
        &["push", "-u", "origin", &format!("HEAD:{target_branch}")],
    )
    .await;

    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let daemon = &fixture.daemon;
    let app = fixture.router();
    daemon.spawn_merge_queue_runner();

    let workspace = common::create_workspace(&app, repo.path(), "mq-push-success").await;
    daemon
        .configure_merge_queue_for_test(
            workspace.id,
            &target_branch,
            "never",
            &["true"],
            true,
            Some("origin"),
            Some(&target_branch),
        )
        .await
        .unwrap();

    let worktree_root = tempfile::tempdir().unwrap();
    let feature_path = worktree_root.path().join("feature");
    common::run_git(
        repo.path(),
        &[
            "worktree",
            "add",
            feature_path.to_str().unwrap(),
            "-b",
            "feature",
        ],
    )
    .await;

    append_file(&feature_path.join("note.txt"), "push-success\n").await;
    common::run_git(&feature_path, &["add", "note.txt"]).await;
    common::run_git(&feature_path, &["commit", "-m", "push-success"]).await;

    let feature_head = git_output(&feature_path, &["rev-parse", "HEAD"]).await;
    let worktree = daemon
        .seed_merge_queue_worktree_for_test(
            workspace.id,
            &feature_path,
            &feature_head,
            Some("feature"),
        )
        .await
        .unwrap();

    let (status, entry): (StatusCode, MergeQueueEntry) = common::json_request(
        &app,
        Method::POST,
        "/api/merge-queue/entries",
        Some(json!({
            "worktree_id": worktree.id.0.to_string(),
            "message": "mq-push-success",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let entry = daemon
        .wait_for_merge_queue_entry_for_test(workspace.id, entry.id, Duration::from_secs(15))
        .await
        .unwrap();
    assert_eq!(entry.status, MergeQueueEntryStatus::Passed);
    let result_commit_sha = entry
        .result_commit_sha
        .clone()
        .expect("result commit sha missing");

    let merge_queue_repo = repo.path().join(".ctx/merge-queue/repo");
    let mq_head = git_output(&merge_queue_repo, &["rev-parse", &target_branch]).await;
    assert_eq!(mq_head, result_commit_sha);

    let remote_head = git_output(
        origin.path(),
        &["rev-parse", &format!("refs/heads/{target_branch}")],
    )
    .await;
    assert_eq!(remote_head, result_commit_sha);
}

#[tokio::test]
async fn merge_queue_push_on_success_does_not_push_if_target_branch_advanced() {
    let repo =
        common::init_git_repo(&[("note.txt", "base\n"), (".gitignore", ".ctx/merge-queue\n")])
            .await;
    let target_branch = git_output(repo.path(), &["rev-parse", "--abbrev-ref", "HEAD"]).await;
    let base_head = git_output(repo.path(), &["rev-parse", "HEAD"]).await;

    let origin = tempfile::tempdir().unwrap();
    common::run_git(origin.path(), &["init", "--bare"]).await;
    common::run_git(
        repo.path(),
        &["remote", "add", "origin", origin.path().to_str().unwrap()],
    )
    .await;
    common::run_git(
        repo.path(),
        &["push", "-u", "origin", &format!("HEAD:{target_branch}")],
    )
    .await;

    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let daemon = &fixture.daemon;
    let app = fixture.router();
    daemon.spawn_merge_queue_runner();

    let workspace = common::create_workspace(&app, repo.path(), "mq-push-branch-advanced").await;
    daemon
        .configure_merge_queue_for_test(
            workspace.id,
            &target_branch,
            "never",
            &[&format!("git update-ref refs/heads/{target_branch} HEAD")],
            true,
            Some("origin"),
            Some(&target_branch),
        )
        .await
        .unwrap();

    let worktree_root = tempfile::tempdir().unwrap();
    let feature_path = worktree_root.path().join("feature");
    common::run_git(
        repo.path(),
        &[
            "worktree",
            "add",
            feature_path.to_str().unwrap(),
            "-b",
            "feature",
        ],
    )
    .await;

    append_file(&feature_path.join("note.txt"), "branch-advanced\n").await;
    common::run_git(&feature_path, &["add", "note.txt"]).await;
    common::run_git(&feature_path, &["commit", "-m", "branch-advanced"]).await;

    let feature_head = git_output(&feature_path, &["rev-parse", "HEAD"]).await;
    let worktree = daemon
        .seed_merge_queue_worktree_for_test(
            workspace.id,
            &feature_path,
            &feature_head,
            Some("feature"),
        )
        .await
        .unwrap();

    let (status, _body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        Method::POST,
        "/api/merge-queue/entries",
        Some(json!({
            "worktree_id": worktree.id.0.to_string(),
            "message": "mq-push-branch-advanced",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let entry = daemon
        .latest_merge_queue_entry_for_test(workspace.id)
        .await
        .unwrap();
    let entry = daemon
        .wait_for_merge_queue_entry_for_test(workspace.id, entry.id, Duration::from_secs(15))
        .await
        .unwrap();
    assert_eq!(entry.status, MergeQueueEntryStatus::Failed);
    assert!(entry
        .error_message
        .as_deref()
        .unwrap_or_default()
        .starts_with("target branch advanced"));

    let remote_head = git_output(
        origin.path(),
        &["rev-parse", &format!("refs/heads/{target_branch}")],
    )
    .await;
    assert_eq!(remote_head, base_head);

    let run = daemon
        .latest_merge_queue_run_for_test(workspace.id, entry.id)
        .await
        .unwrap();
    let log_path = run.log_path.expect("log path missing");
    let log_contents = tokio::fs::read_to_string(log_path).await.unwrap();
    assert!(!log_contents.contains("push origin "));
}

#[tokio::test]
async fn merge_queue_isolation_and_canonical_sync() {
    let repo =
        common::init_git_repo(&[("note.txt", "base\n"), (".gitignore", ".ctx/merge-queue\n")])
            .await;
    let target_branch = git_output(repo.path(), &["rev-parse", "--abbrev-ref", "HEAD"]).await;

    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let daemon = &fixture.daemon;
    let app = fixture.router();
    daemon.spawn_merge_queue_runner();

    let workspace = common::create_workspace(&app, repo.path(), "mq-test").await;
    daemon
        .configure_merge_queue_for_test(
            workspace.id,
            &target_branch,
            "never",
            &["true"],
            false,
            None,
            None,
        )
        .await
        .unwrap();

    let worktree_root = tempfile::tempdir().unwrap();
    let feature1_path = worktree_root.path().join("feature-1");
    common::run_git(
        repo.path(),
        &[
            "worktree",
            "add",
            feature1_path.to_str().unwrap(),
            "-b",
            "feature",
        ],
    )
    .await;

    append_file(&feature1_path.join("note.txt"), "mq-1\n").await;
    common::run_git(&feature1_path, &["add", "note.txt"]).await;
    common::run_git(&feature1_path, &["commit", "-m", "mq-1"]).await;

    let base_head = git_output(repo.path(), &["rev-parse", "HEAD"]).await;
    let feature_head = git_output(&feature1_path, &["rev-parse", "HEAD"]).await;
    let worktree1 = daemon
        .seed_merge_queue_worktree_for_test(
            workspace.id,
            &feature1_path,
            &feature_head,
            Some("feature"),
        )
        .await
        .unwrap();

    let (status, entry): (StatusCode, MergeQueueEntry) = common::json_request(
        &app,
        Method::POST,
        "/api/merge-queue/entries",
        Some(json!({
            "worktree_id": worktree1.id.0.to_string(),
            "message": "mq-1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry = daemon
        .wait_for_merge_queue_entry_for_test(workspace.id, entry.id, Duration::from_secs(15))
        .await
        .unwrap();
    assert_eq!(entry.status, MergeQueueEntryStatus::Passed);

    let canonical_head = git_output(repo.path(), &["rev-parse", "HEAD"]).await;
    assert_eq!(canonical_head, base_head);

    let merge_queue_repo = repo.path().join(".ctx/merge-queue/repo");
    let mq_head = git_output(&merge_queue_repo, &["rev-parse", &target_branch]).await;
    assert_ne!(mq_head, base_head);
    common::run_git(
        repo.path(),
        &[
            "fetch",
            merge_queue_repo.to_str().unwrap(),
            &format!("refs/heads/{target_branch}"),
        ],
    )
    .await;
    common::run_git(repo.path(), &["reset", "--hard", "FETCH_HEAD"]).await;
    let canonical_synced = git_output(repo.path(), &["rev-parse", "HEAD"]).await;
    assert_eq!(canonical_synced, mq_head);

    let feature2_path = worktree_root.path().join("feature-2");
    common::run_git(
        repo.path(),
        &[
            "worktree",
            "add",
            feature2_path.to_str().unwrap(),
            "-b",
            "feature-2",
        ],
    )
    .await;

    daemon
        .configure_merge_queue_for_test(
            workspace.id,
            &target_branch,
            "clean_only",
            &["true"],
            false,
            None,
            None,
        )
        .await
        .unwrap();
    append_file(&feature2_path.join("note.txt"), "mq-2\n").await;
    common::run_git(&feature2_path, &["add", "note.txt"]).await;
    common::run_git(&feature2_path, &["commit", "-m", "mq-2"]).await;

    let feature2_head = git_output(&feature2_path, &["rev-parse", "HEAD"]).await;
    let worktree2 = daemon
        .seed_merge_queue_worktree_for_test(
            workspace.id,
            &feature2_path,
            &feature2_head,
            Some("feature-2"),
        )
        .await
        .unwrap();

    let (status, entry): (StatusCode, MergeQueueEntry) = common::json_request(
        &app,
        Method::POST,
        "/api/merge-queue/entries",
        Some(json!({
            "worktree_id": worktree2.id.0.to_string(),
            "message": "mq-2",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry = daemon
        .wait_for_merge_queue_entry_for_test(workspace.id, entry.id, Duration::from_secs(15))
        .await
        .unwrap();
    assert_eq!(entry.status, MergeQueueEntryStatus::Passed);
    let canonical_head = git_output(repo.path(), &["rev-parse", "HEAD"]).await;
    let expected = entry.result_commit_sha.clone().unwrap();
    assert_eq!(canonical_head, expected);

    let feature3_path = worktree_root.path().join("feature-3");
    common::run_git(
        repo.path(),
        &[
            "worktree",
            "add",
            feature3_path.to_str().unwrap(),
            "-b",
            "feature-3",
        ],
    )
    .await;
    append_file(&feature3_path.join("note.txt"), "mq-3\n").await;
    common::run_git(&feature3_path, &["add", "note.txt"]).await;
    common::run_git(&feature3_path, &["commit", "-m", "mq-3"]).await;
    let feature3_head = git_output(&feature3_path, &["rev-parse", "HEAD"]).await;
    let worktree3 = daemon
        .seed_merge_queue_worktree_for_test(
            workspace.id,
            &feature3_path,
            &feature3_head,
            Some("feature-3"),
        )
        .await
        .unwrap();

    append_file(&repo.path().join("note.txt"), "dirty\n").await;

    let (status, entry): (StatusCode, MergeQueueEntry) = common::json_request(
        &app,
        Method::POST,
        "/api/merge-queue/entries",
        Some(json!({
            "worktree_id": worktree3.id.0.to_string(),
            "message": "mq-3",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry = daemon
        .wait_for_merge_queue_entry_for_test(workspace.id, entry.id, Duration::from_secs(15))
        .await
        .unwrap();
    assert_eq!(entry.status, MergeQueueEntryStatus::Passed);

    let canonical_dirty = git_status_porcelain(repo.path()).await.unwrap();
    assert!(!canonical_dirty.is_empty());

    let canonical_after = git_output(repo.path(), &["rev-parse", "HEAD"]).await;
    assert_eq!(canonical_after, expected);

    let run = daemon
        .latest_merge_queue_run_for_test(workspace.id, entry.id)
        .await
        .unwrap();
    let log_path = run.log_path.unwrap();
    let log_contents = tokio::fs::read_to_string(log_path).await.unwrap();
    assert!(log_contents.contains("canonical sync skipped"));
}

#[tokio::test]
async fn merge_queue_submit_uses_worktree_root() {
    let repo =
        common::init_git_repo(&[("note.txt", "base\n"), (".gitignore", ".ctx/merge-queue\n")])
            .await;
    let target_branch = git_output(repo.path(), &["rev-parse", "--abbrev-ref", "HEAD"]).await;

    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let daemon = &fixture.daemon;
    let app = fixture.router();
    daemon.spawn_merge_queue_runner();

    let workspace = common::create_workspace(&app, repo.path(), "mq-root").await;
    daemon
        .configure_merge_queue_for_test(
            workspace.id,
            &target_branch,
            "never",
            &["true"],
            false,
            None,
            None,
        )
        .await
        .unwrap();
    let (_task, session) =
        common::create_task_with_session(&app, workspace.id.0, "mq-root", "fake", "fake-model")
            .await;

    let worktree_root = tempfile::tempdir().unwrap();
    let feature_path = worktree_root.path().join("feature-root");
    common::run_git(
        repo.path(),
        &[
            "worktree",
            "add",
            feature_path.to_str().unwrap(),
            "-b",
            "feature-root",
        ],
    )
    .await;

    append_file(&feature_path.join("note.txt"), "mq-root\n").await;
    common::run_git(&feature_path, &["add", "note.txt"]).await;
    common::run_git(&feature_path, &["commit", "-m", "mq-root"]).await;

    let feature_head = git_output(&feature_path, &["rev-parse", "HEAD"]).await;
    let worktree_root_str = feature_path.to_string_lossy().to_string();
    let (status, entry): (StatusCode, MergeQueueEntry) = common::json_request(
        &app,
        Method::POST,
        "/api/merge-queue/entries",
        Some(json!({
            "session_id": session.id.0.to_string(),
            "worktree_root": worktree_root_str.clone(),
            "message": "mq-root",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry = daemon
        .wait_for_merge_queue_entry_for_test(workspace.id, entry.id, Duration::from_secs(15))
        .await
        .unwrap();
    assert_eq!(entry.status, MergeQueueEntryStatus::Passed);
    assert_eq!(
        entry.head_commit_sha.as_deref(),
        Some(feature_head.as_str())
    );

    let worktree_id = entry.worktree_id.expect("worktree id missing");
    let worktree = daemon
        .merge_queue_worktree_for_test(workspace.id, worktree_id)
        .await
        .unwrap();
    assert_eq!(worktree.root_path, worktree_root_str);
}
