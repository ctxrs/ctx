mod common;

use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use axum::{
    http::{Method, StatusCode},
    Router,
};
use ctx_core::ids::WorkspaceId;
use ctx_core::models::{DiffUnavailableReason, WorktreeVcsFreshness};
use ctx_daemon::daemon::AppRuntimeFlags;
use serde_json::Value;
use tokio::process::Command;

async fn run_git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .kill_on_drop(true)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .await
        .expect("run git command");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn git_stdout(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .kill_on_drop(true)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .await
        .expect("run git command");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

async fn remove_git_marker(root: &Path) {
    let git_path = root.join(".git");
    let Ok(metadata) = tokio::fs::metadata(&git_path).await else {
        return;
    };
    if metadata.is_dir() {
        tokio::fs::remove_dir_all(&git_path)
            .await
            .expect("remove .git directory");
    } else {
        tokio::fs::remove_file(&git_path)
            .await
            .expect("remove .git file");
    }
}

async fn poison_worktree_git_marker(worktree_root: &Path) {
    remove_git_marker(worktree_root).await;
    tokio::fs::write(
        worktree_root.join(".git"),
        "gitdir: /definitely/missing/ctx-test-gitdir\n",
    )
    .await
    .expect("write poisoned .git marker");
}

fn worktree_vcs_snapshot_test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn set_primary_branch(app: &Router, workspace_id: WorkspaceId, primary_branch: &str) {
    let (status, _resp): (StatusCode, Value) = common::json_request(
        app,
        Method::POST,
        format!("/api/workspaces/{}/primary_branch", workspace_id.0),
        Some(serde_json::json!({ "primary_branch": primary_branch })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test(flavor = "current_thread")]
async fn worktree_vcs_disabled_mode_suppresses_projection_work() {
    let _guard = worktree_vcs_snapshot_test_lock().lock().await;
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture_with_runtime_flags(
        "http://127.0.0.1:0",
        AppRuntimeFlags {
            worktree_vcs_enabled: false,
        },
    )
    .await;
    let app = fixture.router();
    let daemon = &fixture.daemon;

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (_task, session) =
        common::create_task_with_session(&app, ws.id.0, "vcs-disabled", "fake", "fake-model").await;
    let worktree = daemon
        .load_worktree_for_test(session.worktree_id)
        .await
        .expect("worktree should exist");
    daemon.mark_worktree_vcs_active_for_test(worktree.id).await;

    assert!(!daemon.worktree_vcs_enabled_for_test());
    assert!(!daemon.is_worktree_vcs_active_for_test(worktree.id).await);
    daemon
        .emit_worktree_vcs_snapshot_for_worktree(&worktree, true)
        .await
        .expect("disabled VCS emission should be a no-op");
    daemon
        .request_worktree_vcs_refresh_for_test(&worktree, true, true)
        .await
        .expect("disabled VCS refresh should be a no-op");
    daemon
        .run_git_status_watcher_for_test(worktree.clone())
        .await
        .expect("disabled VCS watcher should be a no-op");

    assert!(
        daemon.worktree_vcs_snapshot(worktree.id).await.is_none(),
        "disabled VCS mode must not compute or cache worktree VCS snapshots"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn worktree_vcs_snapshot_clears_stale_counts_when_repo_becomes_unavailable() {
    let _guard = worktree_vcs_snapshot_test_lock().lock().await;
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();
    let daemon = &fixture.daemon;

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (_task, session) =
        common::create_task_with_session(&app, ws.id.0, "snapshot", "fake", "fake-model").await;
    let worktree = daemon
        .load_worktree_for_test(session.worktree_id)
        .await
        .expect("worktree should exist");
    daemon.mark_worktree_vcs_active_for_test(worktree.id).await;
    tokio::fs::write(
        Path::new(&worktree.root_path).join("file.txt"),
        "hello\nchanged\n",
    )
    .await
    .expect("write changed file");
    daemon
        .emit_worktree_vcs_snapshot_for_worktree(&worktree, true)
        .await
        .expect("initial snapshot emission should succeed");
    let start = Instant::now();
    loop {
        let snapshot = daemon.worktree_vcs_snapshot(worktree.id).await;
        if let Some(snapshot) = snapshot {
            if snapshot.summary.file_count.unwrap_or(0) > 0 {
                break;
            }
        }
        if start.elapsed() > Duration::from_secs(5) {
            panic!("timed out waiting for non-empty vcs summary");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let worktree_root = Path::new(&worktree.root_path);
    let _ = repo;
    poison_worktree_git_marker(worktree_root).await;

    daemon
        .emit_worktree_vcs_snapshot_for_worktree(&worktree, true)
        .await
        .expect("snapshot emission should not fail for no-repo");

    let snapshot = daemon
        .worktree_vcs_snapshot(worktree.id)
        .await
        .expect("snapshot should be present");
    assert!(!snapshot.available);
    assert_eq!(
        snapshot.unavailable_reason,
        Some(DiffUnavailableReason::NoRepo)
    );
    assert_eq!(snapshot.summary.file_count, None);
    assert_eq!(snapshot.summary.line_additions, None);
    assert_eq!(snapshot.summary.line_deletions, None);
    assert!(snapshot.touched_files.items.is_empty());
    assert_eq!(
        snapshot.touched_files_state,
        ctx_core::models::WorktreeVcsTouchedFilesState::NotLoaded
    );
}

#[tokio::test(flavor = "current_thread")]
async fn worktree_vcs_snapshot_populates_jj_head_commit_metadata() {
    if !common::jj_available().await {
        eprintln!(
            "skipping worktree_vcs_snapshot_populates_jj_head_commit_metadata: jj not installed or too old"
        );
        return;
    }

    let _guard = worktree_vcs_snapshot_test_lock().lock().await;
    let repo = common::init_jj_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();
    let daemon = &fixture.daemon;

    let ws = common::create_workspace(&app, repo.path(), "jj-ws").await;
    let (_task, session) =
        common::create_task_with_session(&app, ws.id.0, "jj-vcs", "fake", "fake-model").await;
    let worktree = daemon
        .load_worktree_for_test(session.worktree_id)
        .await
        .expect("worktree should exist");

    daemon.mark_worktree_vcs_active_for_test(worktree.id).await;

    daemon
        .emit_worktree_vcs_snapshot_for_worktree(&worktree, true)
        .await
        .expect("snapshot emission should succeed for jj worktree");

    let expected_head = common::run_jj_output(
        repo.path(),
        &["log", "-r", "@", "-T", "commit_id ++ \"\\n\""],
    )
    .await
    .trim()
    .to_string();
    let snapshot = daemon
        .worktree_vcs_snapshot(worktree.id)
        .await
        .expect("snapshot should be present");
    assert_eq!(snapshot.head_commit_sha, expected_head);
}

#[tokio::test(flavor = "current_thread")]
async fn worktree_vcs_snapshot_recovers_when_repo_is_reinitialized() {
    let _guard = worktree_vcs_snapshot_test_lock().lock().await;
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();
    let daemon = &fixture.daemon;

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let primary_branch = daemon
        .workspace_primary_branch_for_test(ws.id)
        .await
        .expect("loading primary branch should succeed")
        .expect("workspace primary branch should be configured");
    let (_task, session) =
        common::create_task_with_session(&app, ws.id.0, "snapshot-recovery", "fake", "fake-model")
            .await;
    let worktree = daemon
        .load_worktree_for_test(session.worktree_id)
        .await
        .expect("worktree should exist");

    daemon.mark_worktree_vcs_active_for_test(worktree.id).await;

    let worktree_root = Path::new(&worktree.root_path);
    let _ = repo;
    poison_worktree_git_marker(worktree_root).await;

    daemon
        .emit_worktree_vcs_snapshot_for_worktree(&worktree, true)
        .await
        .expect("no-repo snapshot emission should succeed");
    let unavailable = daemon
        .worktree_vcs_snapshot(worktree.id)
        .await
        .expect("unavailable snapshot should be present");
    assert!(!unavailable.available);
    assert_eq!(
        unavailable.unavailable_reason,
        Some(DiffUnavailableReason::NoRepo)
    );

    remove_git_marker(worktree_root).await;
    run_git(worktree_root, &["init"]).await;
    let primary_ref = format!("refs/heads/{primary_branch}");
    run_git(
        worktree_root,
        &["symbolic-ref", "HEAD", primary_ref.as_str()],
    )
    .await;
    run_git(worktree_root, &["config", "user.email", "test@example.com"]).await;
    run_git(worktree_root, &["config", "user.name", "Test"]).await;
    run_git(worktree_root, &["add", "."]).await;
    run_git(worktree_root, &["commit", "-m", "restore"]).await;

    daemon
        .emit_worktree_vcs_snapshot_for_worktree(&worktree, true)
        .await
        .expect("recovered repo snapshot emission should succeed");

    let start = Instant::now();
    loop {
        let recovered = daemon
            .worktree_vcs_snapshot(worktree.id)
            .await
            .expect("recovered snapshot should be present");
        if recovered.available {
            assert_eq!(recovered.unavailable_reason, None);

            break;
        }

        if start.elapsed() > Duration::from_secs(5) {
            panic!(
                "timed out waiting for recovered vcs snapshot to become available: {recovered:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn worktree_vcs_snapshot_noop_emit_preserves_freshness() {
    let _guard = worktree_vcs_snapshot_test_lock().lock().await;
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();
    let daemon = &fixture.daemon;

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (_task, session) =
        common::create_task_with_session(&app, ws.id.0, "noop-fresh", "fake", "fake-model").await;
    let worktree = daemon
        .load_worktree_for_test(session.worktree_id)
        .await
        .expect("worktree should exist");

    daemon.mark_worktree_vcs_active_for_test(worktree.id).await;

    daemon
        .refresh_worktree_vcs_summary_for_test(worktree.clone())
        .await
        .expect("seed fresh snapshot");

    let seeded = daemon
        .worktree_vcs_snapshot(worktree.id)
        .await
        .expect("seeded snapshot should exist");
    assert_eq!(seeded.freshness, WorktreeVcsFreshness::Fresh);

    daemon
        .emit_worktree_vcs_snapshot_for_worktree(&worktree, false)
        .await
        .expect("noop emit should succeed");

    let cached = daemon
        .worktree_vcs_snapshot(worktree.id)
        .await
        .expect("cached snapshot should remain present");
    assert_eq!(
        cached.freshness,
        WorktreeVcsFreshness::Fresh,
        "noop emit should preserve cached fresh snapshot"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn worktree_vcs_snapshot_does_not_repopulate_cache_after_activity_eviction() {
    let _guard = worktree_vcs_snapshot_test_lock().lock().await;
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();
    let daemon = &fixture.daemon;

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (_task, session) =
        common::create_task_with_session(&app, ws.id.0, "snapshot-eviction", "fake", "fake-model")
            .await;
    let worktree = daemon
        .load_worktree_for_test(session.worktree_id)
        .await
        .expect("worktree should exist");

    daemon.mark_worktree_vcs_active_for_test(worktree.id).await;

    daemon
        .emit_worktree_vcs_snapshot_for_worktree(&worktree, true)
        .await
        .expect("initial snapshot emission should succeed");
    assert!(
        daemon.worktree_vcs_snapshot(worktree.id).await.is_some(),
        "expected initial active snapshot to populate cache",
    );

    daemon
        .mark_worktree_vcs_inactive_for_test(worktree.id)
        .await;
    assert!(
        daemon.worktree_vcs_snapshot(worktree.id).await.is_none(),
        "expected activity eviction to clear worktree vcs cache",
    );

    daemon
        .emit_worktree_vcs_snapshot_for_worktree(&worktree, true)
        .await
        .expect("inactive snapshot emission should not fail");
    assert!(
        daemon.worktree_vcs_snapshot(worktree.id).await.is_none(),
        "inactive emit should not recreate worktree vcs cache",
    );
}

#[tokio::test(flavor = "current_thread")]
async fn worktree_vcs_dirty_invalidation_recomputes_when_target_branch_ref_moves() {
    let _guard = worktree_vcs_snapshot_test_lock().lock().await;
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    run_git(repo.path(), &["branch", "merge-target"]).await;

    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();
    let daemon = &fixture.daemon;
    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    set_primary_branch(&app, ws.id, "merge-target").await;

    let (_task, session) =
        common::create_task_with_session(&app, ws.id.0, "dirty-ref-move", "fake", "fake-model")
            .await;
    let worktree = daemon
        .load_worktree_for_test(session.worktree_id)
        .await
        .expect("worktree should exist");

    daemon.mark_worktree_vcs_active_for_test(worktree.id).await;

    let worktree_root = Path::new(&worktree.root_path);
    tokio::fs::write(worktree_root.join("file.txt"), "hello\nphase1\n")
        .await
        .expect("write changed file");
    daemon
        .mark_worktree_vcs_filesystem_dirty_for_test(&worktree, "file.txt")
        .await
        .expect("initial vcs dirty invalidation should succeed");

    let start = Instant::now();
    loop {
        if let Some(snapshot) = daemon.worktree_vcs_snapshot(worktree.id).await {
            if snapshot.summary.file_count.unwrap_or(0) > 0 {
                break;
            }
        }
        if start.elapsed() > Duration::from_secs(30) {
            panic!("timed out waiting for non-zero vcs summary");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    run_git(worktree_root, &["add", "file.txt"]).await;
    run_git(worktree_root, &["commit", "-m", "phase1"]).await;
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree_root)
        .args(["rev-parse", "HEAD"])
        .output()
        .await
        .expect("read worktree head sha");
    assert!(output.status.success(), "rev-parse HEAD should succeed");
    let phase1_sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    run_git(repo.path(), &["branch", "-f", "merge-target", &phase1_sha]).await;
    daemon
        .mark_worktree_vcs_metadata_dirty_for_test(&worktree, ".git/refs/heads/merge-target")
        .await
        .expect("target-branch metadata invalidation should succeed");

    let start = Instant::now();
    loop {
        let snapshot = daemon
            .worktree_vcs_snapshot(worktree.id)
            .await
            .expect("expected refreshed worktree vcs snapshot");
        if snapshot.summary.file_count.unwrap_or(-1) == 0 {
            return;
        }
        if start.elapsed() > Duration::from_secs(30) {
            panic!("timed out waiting for merge-target ref move to clear vcs summary");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn worktree_vcs_snapshot_preserves_head_when_configured_target_branch_disappears() {
    let _guard = worktree_vcs_snapshot_test_lock().lock().await;
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    run_git(repo.path(), &["branch", "merge-target"]).await;

    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();
    let daemon = &fixture.daemon;
    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    set_primary_branch(&app, ws.id, "merge-target").await;

    let (_task, session) = common::create_task_with_session(
        &app,
        ws.id.0,
        "missing-target-head",
        "fake",
        "fake-model",
    )
    .await;
    let worktree = daemon
        .load_worktree_for_test(session.worktree_id)
        .await
        .expect("worktree should exist");

    daemon.mark_worktree_vcs_active_for_test(worktree.id).await;

    let worktree_root = Path::new(&worktree.root_path);
    tokio::fs::write(worktree_root.join("file.txt"), "hello\nadvanced\n")
        .await
        .expect("write changed file");
    run_git(worktree_root, &["add", "file.txt"]).await;
    run_git(worktree_root, &["commit", "-m", "advance head"]).await;
    let expected_head = git_stdout(worktree_root, &["rev-parse", "HEAD"]).await;
    assert_ne!(expected_head, worktree.base_commit_sha);

    run_git(repo.path(), &["branch", "-D", "merge-target"]).await;

    daemon
        .emit_worktree_vcs_snapshot_for_worktree(&worktree, true)
        .await
        .expect("snapshot emission should succeed when target branch disappears");

    let snapshot = daemon
        .worktree_vcs_snapshot(worktree.id)
        .await
        .expect("snapshot should be present");
    assert!(!snapshot.available);
    assert_eq!(
        snapshot.unavailable_reason,
        Some(DiffUnavailableReason::NoTargetBranch)
    );
    assert_eq!(snapshot.base_commit_sha, worktree.base_commit_sha);
    assert_eq!(snapshot.head_commit_sha, expected_head);
    assert_eq!(snapshot.target_branch.as_deref(), Some("merge-target"));
    assert_eq!(snapshot.target_branch_commit_sha, None);
}
