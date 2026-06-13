use std::path::PathBuf;

use super::*;

pub(super) struct DownloadHttpFixture {
    pub(super) base: SessionArtifactFixture,
    pub(super) wrong_session: ctx_core::models::Session,
    pub(super) artifact_path: PathBuf,
    pub(super) artifact_id: String,
}

impl DownloadHttpFixture {
    pub(super) async fn build() -> Self {
        let base = build_session_artifact_fixture().await;
        let wrong_session =
            create_subagent_session_via_api(&base.app, &base.task, base.session.id).await;

        let worktree_root = base
            .daemon()
            .session_worktree_root_path_for_test(&base.session)
            .await
            .unwrap();
        let artifact_path = worktree_root.join("artifact.txt");
        std::fs::write(&artifact_path, b"artifact-body\n").unwrap();

        let res = post_session_artifacts(
            &base.app,
            base.session.id,
            json!([{
                "absolute_file_path": artifact_path.to_string_lossy(),
                "name": "artifact.txt",
                "mime_type": "text/plain"
            }]),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let artifacts: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let artifact_id = artifacts[0]["id"]
            .as_str()
            .expect("artifact id")
            .to_string();

        Self {
            base,
            wrong_session,
            artifact_path,
            artifact_id,
        }
    }

    pub(super) fn app(&self) -> &axum::Router {
        &self.base.app
    }

    pub(super) fn session_id(&self) -> ctx_core::ids::SessionId {
        self.base.session.id
    }
}

pub(super) async fn get_artifact_response(
    fixture: &DownloadHttpFixture,
    session_id: ctx_core::ids::SessionId,
    headers: &[(&str, &str)],
) -> axum::response::Response {
    let mut req = Request::builder().method("GET").uri(format!(
        "/api/sessions/{}/artifacts/{}",
        session_id.0, fixture.artifact_id
    ));
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    fixture
        .app()
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}
