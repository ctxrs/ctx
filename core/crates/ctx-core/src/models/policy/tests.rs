use super::*;
use chrono::Duration;

fn sample_snapshot(now: DateTime<Utc>) -> OrgPolicySnapshot {
    let mut allowed_models = BTreeMap::new();
    allowed_models.insert(
        "anthropic".to_string(),
        vec!["claude-sonnet-4".to_string(), "claude-opus-4".to_string()],
    );
    allowed_models.insert("openai".to_string(), vec!["gpt-4.1".to_string()]);

    let mut features = BTreeMap::new();
    features.insert("mobile_relay".to_string(), PolicyFeatureState::Enabled);
    features.insert("llm_token_relay".to_string(), PolicyFeatureState::Enabled);

    OrgPolicySnapshot {
        id: OrgPolicySnapshotId::new(),
        org_id: OrgId::new(),
        policy_version: "2026-04-28.1".to_string(),
        issued_at: now,
        expires_at: now + Duration::minutes(30),
        grace_expires_at: now + Duration::minutes(60),
        allowed_providers: Some(vec!["anthropic".to_string(), "openai".to_string()]),
        allowed_models,
        required_execution_environment: None,
        allowed_network_profiles: vec![NetworkProfile::LlmOnly, NetworkProfile::All],
        route_policy: RoutePolicy {
            allowed_route_types: vec![
                RouteType::CtxManaged,
                RouteType::CustomerGateway,
                RouteType::UserOauth,
                RouteType::UserApiKey,
            ],
        },
        archive_policy: ArchivePolicy {
            mode: ArchiveMode::OrgTranscript,
        },
        features,
        signature: "signed".to_string(),
    }
}

#[test]
fn workspace_overlay_only_narrows_org_policy() {
    let now = Utc::now();
    let snapshot = sample_snapshot(now);
    let mut overlay_models = BTreeMap::new();
    overlay_models.insert("anthropic".to_string(), vec!["claude-sonnet-4".to_string()]);

    let mut overlay_features = BTreeMap::new();
    overlay_features.insert("llm_token_relay".to_string(), PolicyFeatureState::Disabled);

    let overlay = WorkspacePolicyOverlay {
        workspace_id: WorkspaceId::new(),
        org_id: snapshot.org_id,
        allowed_providers: Some(vec!["anthropic".to_string()]),
        allowed_models: overlay_models,
        required_execution_environment: Some(RequiredExecutionEnvironment::Sandbox),
        allowed_network_profiles: Some(vec![NetworkProfile::LlmOnly]),
        allowed_route_types: Some(vec![RouteType::CtxManaged]),
        features: overlay_features,
    };

    let merged = merge_org_policy_with_overlay(&snapshot, Some(&overlay));
    assert_eq!(merged.workspace_id, Some(overlay.workspace_id));
    assert_eq!(
        merged.allowed_providers,
        Some(vec!["anthropic".to_string()])
    );
    assert_eq!(
        merged.allowed_models.get("anthropic"),
        Some(&vec!["claude-sonnet-4".to_string()])
    );
    assert!(!merged.allowed_models.contains_key("openai"));
    assert_eq!(
        merged.required_execution_environment,
        Some(RequiredExecutionEnvironment::Sandbox)
    );
    assert_eq!(
        merged.allowed_network_profiles,
        vec![NetworkProfile::LlmOnly]
    );
    assert_eq!(
        merged.route_policy.allowed_route_types,
        vec![RouteType::CtxManaged]
    );
    assert_eq!(
        merged.features.get("mobile_relay"),
        Some(&PolicyFeatureState::Enabled)
    );
    assert_eq!(
        merged.features.get("llm_token_relay"),
        Some(&PolicyFeatureState::Disabled)
    );
}

#[test]
fn org_policy_window_allows_grace_and_denies_hard_expiry() {
    let now = Utc::now();
    let snapshot = sample_snapshot(now);

    assert_eq!(
        policy_window_state(&snapshot, now),
        PolicyWindowState::Fresh
    );
    assert_eq!(
        policy_window_state(&snapshot, now + Duration::minutes(45)),
        PolicyWindowState::Grace
    );
    assert_eq!(
        policy_window_state(&snapshot, now + Duration::minutes(61)),
        PolicyWindowState::Expired
    );

    let grace_result = org_policy_allows_run(
        &snapshot,
        None,
        OrgPolicyRunRequest {
            provider_id: "anthropic",
            model_id: "claude-sonnet-4",
            execution_environment: ExecutionEnvironment::Sandbox,
            network_profile: NetworkProfile::LlmOnly,
            route_type: Some(RouteType::CtxManaged),
            now: now + Duration::minutes(45),
        },
    );
    assert_eq!(grace_result, Ok(PolicyWindowState::Grace));

    let expired_result = org_policy_allows_run(
        &snapshot,
        None,
        OrgPolicyRunRequest {
            provider_id: "anthropic",
            model_id: "claude-sonnet-4",
            execution_environment: ExecutionEnvironment::Sandbox,
            network_profile: NetworkProfile::LlmOnly,
            route_type: Some(RouteType::CtxManaged),
            now: now + Duration::minutes(61),
        },
    );
    assert_eq!(expired_result, Err(PolicyDenyReason::PolicyHardExpired));
}

#[test]
fn required_sandbox_denies_org_managed_host_mode() {
    let now = Utc::now();
    let mut snapshot = sample_snapshot(now);
    snapshot.required_execution_environment = Some(RequiredExecutionEnvironment::Sandbox);

    let result = org_policy_allows_run(
        &snapshot,
        None,
        OrgPolicyRunRequest {
            provider_id: "anthropic",
            model_id: "claude-sonnet-4",
            execution_environment: ExecutionEnvironment::Host,
            network_profile: NetworkProfile::LlmOnly,
            route_type: Some(RouteType::CtxManaged),
            now,
        },
    );

    assert_eq!(
        result,
        Err(PolicyDenyReason::ExecutionEnvironmentNotAllowed)
    );
}

#[test]
fn personal_routes_are_blocked_when_policy_disallows_them() {
    let now = Utc::now();
    let mut snapshot = sample_snapshot(now);
    snapshot.route_policy = RoutePolicy {
        allowed_route_types: vec![RouteType::CtxManaged],
    };

    assert!(is_personal_route_blocked(
        &snapshot.route_policy,
        RouteType::UserApiKey
    ));
    assert!(!is_personal_route_allowed(
        &snapshot.route_policy,
        RouteType::UserApiKey
    ));

    let result = org_policy_allows_run(
        &snapshot,
        None,
        OrgPolicyRunRequest {
            provider_id: "anthropic",
            model_id: "claude-sonnet-4",
            execution_environment: ExecutionEnvironment::Sandbox,
            network_profile: NetworkProfile::LlmOnly,
            route_type: Some(RouteType::UserApiKey),
            now,
        },
    );
    assert_eq!(result, Err(PolicyDenyReason::PersonalRouteNotAllowed));
}
