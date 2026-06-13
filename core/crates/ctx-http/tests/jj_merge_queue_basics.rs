use axum::body::Body;
use axum::http::{Method, StatusCode};
use serde_json::json;

use ctx_core::models::{MergeQueueEntry, MergeQueueEntryStatus};

mod common;

#[tokio::test]
async fn merge_queue_basic_jj_flow() {
    if !common::jj_available().await {
        eprintln!("skipping merge_queue_basic_jj_flow: jj not installed or too old");
        return;
    }

    let repo = common::init_jj_repo(&[("file.txt", "hello\n")]).await;
    let ctx_dir = repo.path().join(".ctx");
    tokio::fs::create_dir_all(&ctx_dir).await.unwrap();
    tokio::fs::write(
        ctx_dir.join("config.toml"),
        "[merge_queue]\n\
enabled = true\n\
target_branch = \"main\"\n",
    )
    .await
    .unwrap();

    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    fixture.daemon.spawn_merge_queue_runner();
    let app = fixture.router();

    let workspace = common::create_workspace(&app, repo.path(), "jj-ws").await;
    let (_task, session) = common::create_task_with_session(
        &app,
        workspace.id.into(),
        "jj merge queue",
        "fake",
        "fake-model",
    )
    .await;

    let worktree_root = fixture
        .daemon
        .session_worktree_root_path_for_test(&session)
        .await
        .unwrap();

    tokio::fs::write(worktree_root.join("file.txt"), "hello\njj\n")
        .await
        .unwrap();

    let req = axum::http::Request::builder()
        .method(Method::POST)
        .uri("/api/merge-queue/entries")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "worktree_id": session.worktree_id.0.to_string(),
                "message": "jj merge"
            })
            .to_string(),
        ))
        .unwrap();
    let (status, body) = common::oneshot_bytes(&app, req).await;
    let entry: MergeQueueEntry = serde_json::from_slice(&body).unwrap_or_else(|err| {
        panic!(
            "merge queue response parse failed: {} ({})",
            err,
            String::from_utf8_lossy(&body)
        )
    });
    assert_eq!(status, StatusCode::OK);
    assert_eq!(entry.status, MergeQueueEntryStatus::Passed);

    let target_head = common::run_jj_output(
        repo.path(),
        &["log", "-r", "main", "--no-graph", "-T", "commit_id"],
    )
    .await;
    let target_head = target_head
        .split_whitespace()
        .last()
        .expect("jj log produced no revision output");
    let merged_sha = entry
        .result_commit_sha
        .as_deref()
        .expect("merge queue commit sha");
    assert_eq!(target_head.trim(), merged_sha);
}
