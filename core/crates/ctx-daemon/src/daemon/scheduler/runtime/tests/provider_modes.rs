use super::*;

#[test]
fn returns_full_when_no_emitted() {
    assert_eq!(strip_emitted_prefix("Hello", ""), Some("Hello".to_string()));
}

#[test]
fn full_provider_control_maps_known_full_access_modes() {
    assert_eq!(
        provider_mode_id_for("codex", &ProviderControlMode::Full),
        Some("full-access")
    );
    assert_eq!(
        provider_mode_id_for("claude-crp", &ProviderControlMode::Full),
        Some("bypassPermissions")
    );
    assert_eq!(
        provider_mode_id_for("droid", &ProviderControlMode::Full),
        Some("auto_high")
    );
}

#[test]
fn non_full_provider_control_does_not_force_provider_modes() {
    assert_eq!(
        provider_mode_id_for("droid", &ProviderControlMode::HarnessNative),
        None
    );
    assert_eq!(
        provider_mode_id_for("droid", &ProviderControlMode::CtxEnforced),
        None
    );
}

#[test]
fn full_provider_control_sets_crp_launch_policy_env() {
    let mut provider_env = HashMap::new();

    apply_crp_launch_policy_env_for_control_mode(&mut provider_env, &ProviderControlMode::Full);

    assert_eq!(
        provider_env
            .get(CTX_CRP_LAUNCH_POLICY_ENV)
            .map(String::as_str),
        Some(CTX_CRP_LAUNCH_POLICY_FULL)
    );
}

#[test]
fn non_full_provider_control_removes_spoofed_crp_launch_policy_env() {
    let mut provider_env = HashMap::from([(
        CTX_CRP_LAUNCH_POLICY_ENV.to_string(),
        CTX_CRP_LAUNCH_POLICY_FULL.to_string(),
    )]);

    apply_crp_launch_policy_env_for_control_mode(
        &mut provider_env,
        &ProviderControlMode::CtxEnforced,
    );

    assert!(
        !provider_env.contains_key(CTX_CRP_LAUNCH_POLICY_ENV),
        "daemon must strip externally supplied CRP launch policy when policy is unset"
    );
}

#[test]
fn returns_suffix_when_full_contains_emitted_prefix() {
    let full = "Planning:Done.";
    let emitted = "Planning:";
    assert_eq!(
        strip_emitted_prefix(full, emitted),
        Some("Done.".to_string())
    );
}

#[test]
fn returns_none_when_full_equals_emitted() {
    assert_eq!(strip_emitted_prefix("Same", "Same"), None);
}

#[test]
fn returns_full_when_prefix_does_not_match() {
    assert_eq!(
        strip_emitted_prefix("Hello", "Nope"),
        Some("Hello".to_string())
    );
}

#[test]
fn gemini_bearer_endpoint_keeps_gemini_runtime_provider() {
    let source = ResolvedHarnessSource {
        source_kind: HarnessSourceKind::Endpoint,
        endpoint: Some(HarnessEndpointRecord {
            id: "ep".to_string(),
            provider_id: "gemini".to_string(),
            name: "Gemini Legacy Bearer".to_string(),
            base_url: Some("https://openrouter.ai/api/v1".to_string()),
            api_shape: HarnessApiShape::OpenaiResponses,
            auth_type: "bearer".to_string(),
            model_override: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_verification_status: HarnessEndpointVerificationStatus::Unknown,
            last_verification_at: None,
            last_error: None,
            has_api_key: true,
            model_catalog_status: EndpointModelCatalogStatus::Unknown,
            model_catalog_fetched_at: None,
            model_catalog_error: None,
            model_catalog_models: Vec::new(),
            manual_model_ids: Vec::new(),
            model_catalog_source: None,
        }),
        env: std::collections::HashMap::new(),
    };
    assert_eq!(
        runtime_provider_id_for_session_provider("gemini", &source),
        "gemini"
    );
}

#[test]
fn gemini_subscription_keeps_gemini_runtime_provider() {
    let source = ResolvedHarnessSource {
        source_kind: HarnessSourceKind::Subscription,
        endpoint: None,
        env: std::collections::HashMap::new(),
    };
    assert_eq!(
        runtime_provider_id_for_session_provider("gemini", &source),
        "gemini"
    );
}
