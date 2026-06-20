use super::*;
use ctx_core::ids::{OrgId, WorkspaceId};

async fn assert_route_unavailable(method: &str, uri: String) {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = test_daemon_fixture_for_test(data_dir.path(), None).await;
    let app = fixture.router();

    let req = Request::builder()
        .method(method)
        .uri(&uri)
        .header("content-type", "application/json")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    assert!(
        matches!(
            res.status(),
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ),
        "{method} {uri} unexpectedly returned {}",
        res.status()
    );
}

#[tokio::test]
async fn org_policy_org_routes_are_not_mounted_in_public_local_router() {
    let org_id = OrgId::new();
    for (method, uri) in [
        ("GET", "/api/orgs/daemon_enrollments".to_string()),
        ("PUT", format!("/api/orgs/{}/daemon_enrollment", org_id.0)),
        ("POST", format!("/api/orgs/{}/policy_snapshots", org_id.0)),
    ] {
        assert_route_unavailable(method, uri).await;
    }
}

#[tokio::test]
async fn org_policy_workspace_overlay_routes_are_not_mounted_in_public_local_router() {
    let workspace_id = WorkspaceId::new();
    for method in ["GET", "PUT"] {
        assert_route_unavailable(
            method,
            format!("/api/workspaces/{}/org_policy", workspace_id.0),
        )
        .await;
    }
}
