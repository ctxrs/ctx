use super::*;
use chrono::{Duration, Utc};
use ctx_core::ids::{OrgId, OrgPolicySnapshotId, WorkspaceId};
use ctx_core::models::{
    ArchiveMode, ArchivePolicy, DaemonEnrollment, DaemonEnrollmentStatus, NetworkProfile,
    OrgMembershipRole, OrgPolicySnapshot, PlanType, PolicyFeatureState, PolicySignatureAlgorithm,
    RoutePolicy, RouteType, VcsKind, WorkspacePolicyOverlay,
};
use std::collections::BTreeMap;

#[tokio::test]
async fn daemon_enrollment_routes_do_not_return_policy_signing_keys() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = test_daemon_fixture_for_test(data_dir.path(), None).await;
    let app = fixture.router();
    let org_id = ctx_core::ids::OrgId::new();
    let secret = "policy-signing-secret";
    let now = Utc::now();
    let enrollment = DaemonEnrollment {
        id: ctx_core::ids::DaemonEnrollmentId::new(),
        account_id: ctx_core::ids::AccountId::new(),
        org_id,
        org_membership_id: ctx_core::ids::OrgMembershipId::new(),
        membership_role: OrgMembershipRole::Owner,
        plan_type: PlanType::Team,
        status: DaemonEnrollmentStatus::Active,
        policy_signature_algorithm: PolicySignatureAlgorithm::Hs256,
        policy_signing_key: secret.to_string(),
        active_policy_snapshot_id: None,
        enrolled_at: now,
        updated_at: now,
        revoked_at: None,
    };

    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/orgs/{}/daemon_enrollment", org_id.0))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&enrollment).unwrap()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let body_text = String::from_utf8(body.to_vec()).unwrap();
    assert!(!body_text.contains(secret));
    assert!(!body_text.contains("\"policy_signing_key\":"));
    assert!(body_text.contains("policy_signing_key_present"));

    let req = Request::builder()
        .method("GET")
        .uri("/api/orgs/daemon_enrollments")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let body_text = String::from_utf8(body.to_vec()).unwrap();
    assert!(!body_text.contains(secret));
    assert!(!body_text.contains("\"policy_signing_key\":"));
    assert!(body_text.contains("policy_signing_key_present"));
}

fn daemon_enrollment(org_id: OrgId, secret: &str) -> DaemonEnrollment {
    let now = Utc::now();
    DaemonEnrollment {
        id: ctx_core::ids::DaemonEnrollmentId::new(),
        account_id: ctx_core::ids::AccountId::new(),
        org_id,
        org_membership_id: ctx_core::ids::OrgMembershipId::new(),
        membership_role: OrgMembershipRole::Owner,
        plan_type: PlanType::Team,
        status: DaemonEnrollmentStatus::Active,
        policy_signature_algorithm: PolicySignatureAlgorithm::Hs256,
        policy_signing_key: secret.to_string(),
        active_policy_snapshot_id: None,
        enrolled_at: now,
        updated_at: now,
        revoked_at: None,
    }
}

fn policy_snapshot(org_id: OrgId) -> OrgPolicySnapshot {
    let now = Utc::now();
    OrgPolicySnapshot {
        id: OrgPolicySnapshotId::new(),
        org_id,
        policy_version: "2026-05-16.1".to_string(),
        issued_at: now,
        expires_at: now + Duration::minutes(30),
        grace_expires_at: now + Duration::minutes(60),
        allowed_providers: Some(vec!["fake".to_string()]),
        allowed_models: BTreeMap::new(),
        required_execution_environment: None,
        allowed_network_profiles: vec![NetworkProfile::LlmOnly],
        route_policy: RoutePolicy {
            allowed_route_types: vec![RouteType::UserProviderAccount],
        },
        archive_policy: ArchivePolicy {
            mode: ArchiveMode::OrgSummary,
        },
        features: BTreeMap::from([("org_policy".to_string(), PolicyFeatureState::Enabled)]),
        signature: "invalid".to_string(),
    }
}

fn workspace_overlay(workspace_id: WorkspaceId, org_id: OrgId) -> WorkspacePolicyOverlay {
    WorkspacePolicyOverlay {
        workspace_id,
        org_id,
        allowed_providers: Some(vec!["fake".to_string()]),
        allowed_models: BTreeMap::new(),
        required_execution_environment: None,
        allowed_network_profiles: None,
        allowed_route_types: None,
        features: BTreeMap::new(),
    }
}

#[tokio::test]
async fn policy_snapshot_invalid_signature_returns_bad_request() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = test_daemon_fixture_for_test(data_dir.path(), None).await;
    let app = fixture.router();
    let org_id = OrgId::new();
    fixture
        .daemon()
        .org_policy_handle_for_test()
        .upsert_daemon_enrollment_checked(daemon_enrollment(org_id, "policy-signing-secret"))
        .await
        .expect("seed enrollment");

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/orgs/{}/policy_snapshots", org_id.0))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&policy_snapshot(org_id)).unwrap(),
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn policy_snapshot_missing_enrollment_returns_conflict() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = test_daemon_fixture_for_test(data_dir.path(), None).await;
    let app = fixture.router();
    let org_id = OrgId::new();

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/orgs/{}/policy_snapshots", org_id.0))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&policy_snapshot(org_id)).unwrap(),
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn workspace_policy_overlay_missing_enrollment_returns_conflict() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = test_daemon_fixture_for_test(data_dir.path(), None).await;
    let app = fixture.router();
    let workspace = fixture
        .daemon()
        .seed_workspace_for_test(
            "workspace",
            &data_dir.path().join("workspace"),
            VcsKind::Git,
        )
        .await
        .expect("create workspace");

    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/workspaces/{}/org_policy", workspace.id.0))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&workspace_overlay(workspace.id, OrgId::new())).unwrap(),
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn workspace_policy_overlay_missing_workspace_returns_not_found() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = test_daemon_fixture_for_test(data_dir.path(), None).await;
    let app = fixture.router();
    let org_id = OrgId::new();
    fixture
        .daemon()
        .org_policy_handle_for_test()
        .upsert_daemon_enrollment_checked(daemon_enrollment(org_id, "policy-signing-secret"))
        .await
        .expect("seed enrollment");
    let workspace_id = WorkspaceId::new();

    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/workspaces/{}/org_policy", workspace_id.0))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&workspace_overlay(workspace_id, org_id)).unwrap(),
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn workspace_policy_overlay_get_returns_null_then_stored_overlay() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = test_daemon_fixture_for_test(data_dir.path(), None).await;
    let app = fixture.router();
    let workspace = fixture
        .daemon()
        .seed_workspace_for_test(
            "workspace",
            &data_dir.path().join("workspace"),
            VcsKind::Git,
        )
        .await
        .expect("create workspace");

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/workspaces/{}/org_policy", workspace.id.0))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value, serde_json::Value::Null);

    let org_id = OrgId::new();
    fixture
        .daemon()
        .org_policy_handle_for_test()
        .upsert_daemon_enrollment_checked(daemon_enrollment(org_id, "policy-signing-secret"))
        .await
        .expect("seed enrollment");
    let overlay = workspace_overlay(workspace.id, org_id);
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/workspaces/{}/org_policy", workspace.id.0))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&overlay).unwrap()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/workspaces/{}/org_policy", workspace.id.0))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let stored: WorkspacePolicyOverlay = serde_json::from_slice(&body).unwrap();
    assert_eq!(stored.workspace_id, workspace.id);
    assert_eq!(stored.org_id, org_id);
}

#[tokio::test]
async fn daemon_enrollment_unsupported_plan_returns_bad_request() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = test_daemon_fixture_for_test(data_dir.path(), None).await;
    let app = fixture.router();
    let org_id = OrgId::new();
    let mut enrollment = daemon_enrollment(org_id, "policy-signing-secret");
    enrollment.plan_type = PlanType::Pro;

    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/orgs/{}/daemon_enrollment", org_id.0))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&enrollment).unwrap()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let body_text = String::from_utf8(body.to_vec()).unwrap();
    assert!(body_text.contains("daemon enrollment requires a team or enterprise plan"));
}

#[tokio::test]
async fn daemon_enrollment_blank_signing_key_returns_bad_request() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = test_daemon_fixture_for_test(data_dir.path(), None).await;
    let app = fixture.router();
    let org_id = OrgId::new();
    let enrollment = daemon_enrollment(org_id, "   ");

    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/orgs/{}/daemon_enrollment", org_id.0))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&enrollment).unwrap()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let body_text = String::from_utf8(body.to_vec()).unwrap();
    assert!(body_text.contains("daemon enrollment requires a policy signing key"));
}

#[tokio::test]
async fn daemon_enrollment_org_mismatch_precedes_domain_validation() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = test_daemon_fixture_for_test(data_dir.path(), None).await;
    let app = fixture.router();
    let route_org_id = OrgId::new();
    let mut enrollment = daemon_enrollment(OrgId::new(), "policy-signing-secret");
    enrollment.plan_type = PlanType::FreeLocal;

    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/orgs/{}/daemon_enrollment", route_org_id.0))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&enrollment).unwrap()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let body_text = String::from_utf8(body.to_vec()).unwrap();
    assert!(body_text.contains("enrollment org_id must match route org id"));
    assert!(!body_text.contains("team or enterprise plan"));
}
