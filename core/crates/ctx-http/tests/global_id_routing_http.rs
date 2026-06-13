use std::path::Path;

use axum::http::StatusCode;
use ctx_core::ids::{ArtifactId, SessionId};
use ctx_daemon::test_support::{GlobalIdRoutingWorkspaceSessionSeed, TestDaemon};
use ctx_providers::adapters::{
    ProviderAdapter, ProviderRecommendedAction, ProviderUsability, ProviderUsabilityStatus,
};
use ctx_providers::fake::FakeProviderAdapter;
use serde_json::json;
use uuid::Uuid;

mod common;

struct SessionFixture {
    session_id: SessionId,
}

struct GlobalIdRoutingHarness {
    server: common::TestServer,
    fixture: common::FakeDaemonFixture,
}

impl GlobalIdRoutingHarness {
    fn daemon(&self) -> &TestDaemon {
        &self.fixture.daemon
    }

    fn server(&self) -> &common::TestServer {
        &self.server
    }
}

fn request_shutdown(daemon: &TestDaemon) {
    daemon.request_shutdown();
}

async fn setup_state() -> GlobalIdRoutingHarness {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let mut status = FakeProviderAdapter::new().inspect().await.unwrap();
    status.usability = ProviderUsability {
        usable: true,
        status: ProviderUsabilityStatus::Ready,
        reason_code: None,
        reason: None,
        blocking_provider_ids: Vec::new(),
        recommended_action: ProviderRecommendedAction::None,
    };
    fixture
        .daemon
        .upsert_provider_status("fake".into(), status)
        .await;
    let server = fixture.spawn_server().await;
    GlobalIdRoutingHarness { server, fixture }
}

async fn create_workspace_session(
    daemon: &TestDaemon,
    name: &str,
    repo_root: &Path,
) -> SessionFixture {
    let vcs = ctx_fs::vcs::driver_for_path(repo_root).await.unwrap();
    let base_commit = vcs.rev_parse_head(repo_root).await.unwrap();
    let fixture = daemon
        .seed_global_id_routing_workspace_session_for_test(GlobalIdRoutingWorkspaceSessionSeed {
            name: name.to_string(),
            root_path: repo_root.to_path_buf(),
            base_commit,
            provider_id: "fake".to_string(),
            model_id: "fake-model".to_string(),
        })
        .await
        .unwrap();

    SessionFixture {
        session_id: fixture.session_id,
    }
}

#[tokio::test]
async fn artifact_route_is_session_scoped() {
    let harness = setup_state().await;
    let daemon = harness.daemon();
    let server = harness.server();
    let repo_a = common::init_git_repo(&[("README.md", "a")]).await;
    let repo_b = common::init_git_repo(&[("README.md", "b")]).await;
    let _workspace_a = create_workspace_session(daemon, "a", repo_a.path()).await;
    let workspace_b = create_workspace_session(daemon, "b", repo_b.path()).await;

    let artifact_path = repo_b.path().join("artifact.txt");
    tokio::fs::write(&artifact_path, b"artifact-body")
        .await
        .unwrap();

    let resp = server
        .client
        .post(format!(
            "{}/api/sessions/{}/artifacts",
            server.base_url, workspace_b.session_id.0
        ))
        .json(&json!({
            "artifacts": [
                { "absolute_file_path": artifact_path.to_string_lossy().to_string() }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let artifacts: serde_json::Value = resp.json().await.unwrap();
    let artifact_id = artifacts[0]["id"].as_str().unwrap();
    let artifact_id = ArtifactId(Uuid::parse_str(artifact_id).unwrap());

    let resp = server
        .client
        .get(format!(
            "{}/api/sessions/{}/artifacts/{}",
            server.base_url, workspace_b.session_id.0, artifact_id.0
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.bytes().await.unwrap().as_ref(), b"artifact-body");
    request_shutdown(daemon);
}

#[tokio::test]
async fn quicktime_artifact_upload_is_accepted() {
    let harness = setup_state().await;
    let daemon = harness.daemon();
    let server = harness.server();
    let repo = common::init_git_repo(&[("README.md", "a")]).await;
    let workspace = create_workspace_session(daemon, "a", repo.path()).await;

    let artifact_path = repo.path().join("artifact.mov");
    tokio::fs::write(&artifact_path, b"quicktime-body")
        .await
        .unwrap();

    let resp = server
        .client
        .post(format!(
            "{}/api/sessions/{}/artifacts",
            server.base_url, workspace.session_id.0
        ))
        .json(&json!({
            "artifacts": [
                {
                    "absolute_file_path": artifact_path.to_string_lossy().to_string(),
                    "mime_type": "video/quicktime"
                }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let artifacts: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(artifacts[0]["mime_type"].as_str(), Some("video/quicktime"));
    assert_eq!(artifacts[0]["name"].as_str(), Some("artifact.mov"));
    request_shutdown(daemon);
}

#[tokio::test]
async fn message_delete_route_is_session_scoped() {
    let harness = setup_state().await;
    let daemon = harness.daemon();
    let server = harness.server();
    let repo_a = common::init_git_repo(&[("README.md", "a")]).await;
    let repo_b = common::init_git_repo(&[("README.md", "b")]).await;
    let _workspace_a = create_workspace_session(daemon, "a", repo_a.path()).await;
    let workspace_b = create_workspace_session(daemon, "b", repo_b.path()).await;
    let message_id = daemon
        .seed_global_id_routing_queued_message_for_test(workspace_b.session_id, "queued")
        .await
        .unwrap();

    let resp = server
        .client
        .delete(format!(
            "{}/api/sessions/{}/messages/{}",
            server.base_url, workspace_b.session_id.0, message_id.0
        ))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.text().await.unwrap();
    assert_eq!(status, StatusCode::NO_CONTENT, "unexpected body: {body}");
    assert!(!daemon
        .global_id_routing_message_exists_for_test(workspace_b.session_id, message_id)
        .await
        .unwrap());
    request_shutdown(daemon);
}

#[tokio::test]
async fn subagent_invocation_route_is_session_scoped() {
    let harness = setup_state().await;
    let daemon = harness.daemon();
    let server = harness.server();
    let repo_a = common::init_git_repo(&[("README.md", "a")]).await;
    let repo_b = common::init_git_repo(&[("README.md", "b")]).await;
    let _workspace_a = create_workspace_session(daemon, "a", repo_a.path()).await;
    let workspace_b = create_workspace_session(daemon, "b", repo_b.path()).await;

    let resp = server
        .client
        .post(format!(
            "{}/api/mcp/sessions/{}/spawn_agent",
            server.base_url, workspace_b.session_id.0
        ))
        .json(&json!({
            "worktree": "inherit",
            "prompt": "hello",
            "task_label": "Worker",
            "harness": "fake",
            "model": "fake-model"
        }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.text().await.unwrap();
    if status != StatusCode::OK {
        panic!("unexpected status {status} body: {body}");
    }

    let resp = server
        .client
        .get(format!(
            "{}/api/sessions/{}/subagent_invocations",
            server.base_url, workspace_b.session_id.0
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let invocations: serde_json::Value = resp.json().await.unwrap();
    let invocation_id = invocations[0]["id"].as_str().unwrap().to_string();

    let resp = server
        .client
        .get(format!(
            "{}/api/sessions/{}/subagent_invocations/{}",
            server.base_url, workspace_b.session_id.0, invocation_id
        ))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.text().await.unwrap();
    if status != StatusCode::OK {
        panic!("unexpected status {status} body: {body}");
    }
    let invocation: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(invocation["id"].as_str().unwrap(), invocation_id);
    request_shutdown(daemon);
}
