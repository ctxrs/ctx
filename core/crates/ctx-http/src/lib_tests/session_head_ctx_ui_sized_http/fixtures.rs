use serde::de::DeserializeOwned;

use super::*;

pub(super) struct CtxUiSizedHeadFixture {
    app: axum::Router,
    daemon: DataRootTestDaemonFixture,
    repo: tempfile::TempDir,
    _projection_flush_ms: EnvVarGuard,
    _data_dir: tempfile::TempDir,
    _serial: tokio::sync::MutexGuard<'static, ()>,
}

impl CtxUiSizedHeadFixture {
    pub(super) async fn new() -> Self {
        let serial = home_env_test_lock().lock().await;
        let repo = setup_git_repo().await;
        let projection_flush_ms = EnvVarGuard::set("CTX_ACTIVE_HEAD_PROJECTION_FLUSH_MS", "600000");
        let data_dir = tempfile::tempdir().unwrap();
        let daemon = test_daemon_fixture_with_fake_provider_for_test(data_dir.path(), None).await;
        let app = daemon.router();

        Self {
            daemon,
            app,
            repo,
            _projection_flush_ms: projection_flush_ms,
            _data_dir: data_dir,
            _serial: serial,
        }
    }

    pub(super) fn daemon(&self) -> &TestDaemon {
        self.daemon.daemon()
    }

    pub(super) async fn create_default_session(
        &self,
    ) -> (
        ctx_core::models::Workspace,
        ctx_core::models::Task,
        ctx_core::models::Session,
    ) {
        let workspace =
            create_workspace_via_api(&self.app, &self.repo.path().to_string_lossy()).await;
        let (task_status, task): (StatusCode, ctx_core::models::Task) = self
            .json_request(
                Method::POST,
                format!("/api/workspaces/{}/tasks", workspace.id.0),
                Some(json!({
                    "title": "ctx-ui sized active recovery",
                    "default_session": {
                        "provider_id": "fake",
                        "model_id": "fake-model"
                    }
                })),
            )
            .await;
        assert_eq!(task_status, StatusCode::OK);
        let session = load_primary_session_via_api(&self.app, &task).await;
        (workspace, task, session)
    }

    pub(super) async fn json_request<T: DeserializeOwned>(
        &self,
        method: Method,
        uri: impl Into<String>,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, T) {
        let req = Request::builder()
            .method(method)
            .uri(uri.into())
            .header("content-type", "application/json")
            .body(Body::from(
                body.unwrap_or(serde_json::Value::Null).to_string(),
            ))
            .unwrap();
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let parsed = serde_json::from_slice(&body).unwrap_or_else(|err| {
            panic!(
                "failed to parse JSON response (status {}): {}\nbody: {}",
                status,
                err,
                String::from_utf8_lossy(&body)
            )
        });
        (status, parsed)
    }
}
