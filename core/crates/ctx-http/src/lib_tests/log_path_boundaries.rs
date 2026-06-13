use super::*;

use ctx_core::ids::MergeQueueEntryId;
use ctx_core::models::WorktreeBootstrapStatus;

mod merge_queue;
mod worktree_bootstrap;

struct LogPathFixture {
    app: axum::Router,
    daemon: DataRootTestDaemonFixture,
    workspace: ctx_core::models::Workspace,
    _home_lock: tokio::sync::MutexGuard<'static, ()>,
    _home: EnvVarGuard,
    _home_dir: tempfile::TempDir,
    data_dir: tempfile::TempDir,
    git_repo: tempfile::TempDir,
}

impl LogPathFixture {
    fn daemon(&self) -> &TestDaemon {
        self.daemon.daemon()
    }
}

async fn build_log_path_fixture() -> LogPathFixture {
    let home_lock = home_env_test_lock().lock().await;
    let git_repo = setup_git_repo().await;
    let home_dir = tempfile::tempdir().unwrap();
    let home = EnvVarGuard::set("HOME", &home_dir.path().to_string_lossy());

    let data_dir = tempfile::tempdir().unwrap();
    let daemon = test_daemon_fixture_with_fake_provider_for_test(data_dir.path(), None).await;
    let app = daemon.router();

    let workspace = create_workspace_via_api(&app, &git_repo.path().to_string_lossy()).await;

    LogPathFixture {
        _home_lock: home_lock,
        _home: home,
        _home_dir: home_dir,
        data_dir,
        git_repo,
        app,
        daemon,
        workspace,
    }
}

async fn create_task_with_primary_session(
    fixture: &LogPathFixture,
) -> (ctx_core::models::Task, ctx_core::models::Session) {
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/workspaces/{}/tasks", fixture.workspace.id.0))
        .header("content-type", "application/json")
        .body(Body::from(json!({"title":"t1"}).to_string()))
        .unwrap();
    let res = fixture.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let task: ctx_core::models::Task = serde_json::from_slice(&body).unwrap();
    let session = load_primary_session_via_api(&fixture.app, &task).await;

    (task, session)
}

async fn bootstrap_logs_response(
    app: &axum::Router,
    worktree_id: ctx_core::ids::WorktreeId,
) -> axum::response::Response {
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/worktrees/{}/bootstrap/logs", worktree_id.0))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    res
}

async fn merge_queue_logs_response(
    app: &axum::Router,
    workspace_id: ctx_core::ids::WorkspaceId,
    entry_id: MergeQueueEntryId,
) -> axum::response::Response {
    merge_queue_logs_response_raw(app, &workspace_id.0.to_string(), &entry_id.0.to_string()).await
}

async fn merge_queue_logs_response_raw(
    app: &axum::Router,
    workspace_id: &str,
    entry_id: &str,
) -> axum::response::Response {
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/workspaces/{workspace_id}/merge_queue/entries/{entry_id}/logs"
        ))
        .body(Body::empty())
        .unwrap();
    app.clone().oneshot(req).await.unwrap()
}
