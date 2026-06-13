use super::*;
use chrono::Duration;
use std::collections::BTreeMap;

fn sample_enrollment(org_id: OrgId, snapshot_id: OrgPolicySnapshotId) -> DaemonEnrollment {
    let now = Utc::now();
    DaemonEnrollment {
        id: DaemonEnrollmentId::new(),
        account_id: AccountId::new(),
        org_id,
        org_membership_id: OrgMembershipId::new(),
        membership_role: OrgMembershipRole::Admin,
        plan_type: PlanType::Team,
        status: DaemonEnrollmentStatus::Active,
        policy_signature_algorithm: PolicySignatureAlgorithm::Hs256,
        policy_signing_key: "test-policy-signing-key".to_string(),
        active_policy_snapshot_id: Some(snapshot_id),
        enrolled_at: now,
        updated_at: now,
        revoked_at: None,
    }
}

fn sample_snapshot(org_id: OrgId) -> OrgPolicySnapshot {
    let now = Utc::now();
    let mut allowed_models = BTreeMap::new();
    allowed_models.insert("anthropic".to_string(), vec!["claude-sonnet-4".to_string()]);

    let mut features = BTreeMap::new();
    features.insert("mobile_relay".to_string(), PolicyFeatureState::Enabled);

    OrgPolicySnapshot {
        id: OrgPolicySnapshotId::new(),
        org_id,
        policy_version: "2026-04-28.1".to_string(),
        issued_at: now,
        expires_at: now + Duration::minutes(30),
        grace_expires_at: now + Duration::minutes(60),
        allowed_providers: Some(vec!["anthropic".to_string()]),
        allowed_models,
        required_execution_environment: Some(RequiredExecutionEnvironment::Sandbox),
        allowed_network_profiles: vec![NetworkProfile::LlmOnly],
        route_policy: RoutePolicy {
            allowed_route_types: vec![RouteType::CtxManaged],
        },
        archive_policy: ArchivePolicy {
            mode: ArchiveMode::OrgTranscript,
        },
        features,
        signature: "signed".to_string(),
    }
}

#[tokio::test]
async fn daemon_enrollment_and_policy_snapshot_roundtrip() {
    let (_dir, store) = crate::store::tests::setup_store().await;
    let org_id = OrgId::new();
    let snapshot = sample_snapshot(org_id);
    store
        .upsert_org_policy_snapshot(snapshot.clone())
        .await
        .unwrap();

    let enrollment = sample_enrollment(org_id, snapshot.id);
    let stored_enrollment = store
        .upsert_daemon_enrollment(enrollment.clone())
        .await
        .unwrap();
    assert_eq!(stored_enrollment, enrollment);

    let loaded_enrollment = store
        .get_daemon_enrollment_by_org_id(org_id)
        .await
        .unwrap()
        .expect("enrollment");
    assert_eq!(loaded_enrollment, enrollment);

    let loaded_snapshot = store
        .get_latest_org_policy_snapshot(org_id)
        .await
        .unwrap()
        .expect("snapshot");
    assert_eq!(loaded_snapshot, snapshot);

    let overlay = WorkspacePolicyOverlay {
        workspace_id: crate::store::tests::create_session_with_turn(&store, None)
            .await
            .0
            .workspace_id,
        org_id,
        allowed_providers: Some(vec!["anthropic".to_string()]),
        allowed_models: BTreeMap::new(),
        required_execution_environment: Some(RequiredExecutionEnvironment::Sandbox),
        allowed_network_profiles: Some(vec![NetworkProfile::LlmOnly]),
        allowed_route_types: Some(vec![RouteType::CtxManaged]),
        features: BTreeMap::new(),
    };
    store
        .upsert_workspace_policy_overlay(overlay.clone())
        .await
        .unwrap();
    let loaded_overlay = store
        .get_workspace_policy_overlay(overlay.workspace_id)
        .await
        .unwrap()
        .expect("overlay");
    assert_eq!(loaded_overlay, overlay);
}

#[tokio::test]
async fn run_grant_and_policy_decision_event_roundtrip() {
    let (_dir, store) = crate::store::tests::setup_store().await;
    let (session, _turn_id) = crate::store::tests::create_session_with_turn(&store, None).await;
    let run_id = RunId::new();
    let issued_at = Utc::now();

    let run_grant = RunGrant {
        id: RunGrantId::new(),
        run_id,
        session_id: session.id,
        workspace_id: session.workspace_id,
        account_id: AccountId::new(),
        org_id: OrgId::new(),
        membership_role: Some(OrgMembershipRole::Member),
        policy_version: "2026-04-28.1".to_string(),
        provider_id: "anthropic".to_string(),
        model_id: "claude-sonnet-4".to_string(),
        execution_environment: ExecutionEnvironment::Sandbox,
        network_profile: NetworkProfile::LlmOnly,
        route_type: Some(RouteType::CtxManaged),
        archive_mode: ArchiveMode::OrgTranscript,
        issued_at,
        expires_at: Some(issued_at + Duration::minutes(60)),
        decision_source: PolicyDecisionSource::CachedPolicy,
    };

    store.create_run_grant(run_grant.clone()).await.unwrap();
    let loaded_grant = store
        .get_run_grant_by_run_id(run_id)
        .await
        .unwrap()
        .expect("run grant");
    assert_eq!(loaded_grant, run_grant);

    let event = PolicyDecisionEvent {
        id: PolicyDecisionEventId::new(),
        run_grant_id: Some(run_grant.id),
        run_id: Some(run_id),
        session_id: Some(session.id),
        workspace_id: Some(session.workspace_id),
        account_id: Some(run_grant.account_id),
        org_id: Some(run_grant.org_id),
        policy_snapshot_id: Some(OrgPolicySnapshotId::new()),
        policy_version: Some(run_grant.policy_version.clone()),
        decision_source: PolicyDecisionSource::CachedPolicy,
        outcome: PolicyDecisionOutcome::Denied,
        deny_reason: Some(PolicyDenyReason::PersonalRouteNotAllowed),
        requested_provider_id: Some(run_grant.provider_id.clone()),
        requested_model_id: Some(run_grant.model_id.clone()),
        requested_execution_environment: Some(ExecutionEnvironment::Sandbox),
        requested_network_profile: Some(NetworkProfile::LlmOnly),
        requested_route_type: Some(RouteType::UserApiKey),
        detail: Some("personal route blocked by org policy".to_string()),
        created_at: issued_at,
    };

    store
        .append_policy_decision_event(event.clone())
        .await
        .unwrap();
    let events = store
        .list_policy_decision_events_for_run(run_id)
        .await
        .unwrap();
    assert_eq!(events, vec![event]);
}
