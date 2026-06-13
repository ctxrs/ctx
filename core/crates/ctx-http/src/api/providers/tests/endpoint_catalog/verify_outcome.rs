use super::*;

#[test]
fn endpoint_selection_is_active_requires_selected_endpoint_record() {
    let active = harness_sources::HarnessProviderSourceConfig {
        provider_id: "codex".to_string(),
        selected_source_kind: HarnessSourceKind::Endpoint,
        selected_endpoint_id: Some("ep-1".to_string()),
        endpoints: vec![test_endpoint("ep-1")],
    };
    assert!(endpoint_selection_is_active(&active));

    let missing = harness_sources::HarnessProviderSourceConfig {
        provider_id: "codex".to_string(),
        selected_source_kind: HarnessSourceKind::Endpoint,
        selected_endpoint_id: Some("ep-2".to_string()),
        endpoints: vec![test_endpoint("ep-1")],
    };
    assert!(!endpoint_selection_is_active(&missing));
}

#[test]
fn endpoint_supports_model_catalog_verify_requires_openai_shape_and_base_url() {
    let mut endpoint = test_endpoint("ep-1");
    assert!(endpoint_supports_model_catalog_verify(&endpoint));

    endpoint.api_shape = HarnessApiShape::AnthropicMessages;
    assert!(!endpoint_supports_model_catalog_verify(&endpoint));

    endpoint.api_shape = HarnessApiShape::OpenaiResponses;
    endpoint.base_url = None;
    assert!(!endpoint_supports_model_catalog_verify(&endpoint));
}

#[test]
fn endpoint_catalog_verify_outcome_ready_and_manual_only_are_valid() {
    let mut ready = test_endpoint("ep-ready");
    ready.model_catalog_status = harness_sources::EndpointModelCatalogStatus::Ready;
    let (status, auth_required, message, endpoint_status) = endpoint_catalog_verify_outcome(&ready);
    assert_eq!(status, "ok");
    assert_eq!(auth_required, Some(false));
    assert!(message.is_none());
    assert_eq!(endpoint_status, HarnessEndpointVerificationStatus::Valid);

    let mut manual = test_endpoint("ep-manual");
    manual.model_catalog_status = harness_sources::EndpointModelCatalogStatus::ManualOnly;
    let (status, auth_required, message, endpoint_status) =
        endpoint_catalog_verify_outcome(&manual);
    assert_eq!(status, "ok");
    assert_eq!(auth_required, Some(false));
    assert!(message.is_none());
    assert_eq!(endpoint_status, HarnessEndpointVerificationStatus::Valid);
}

#[test]
fn endpoint_catalog_verify_outcome_classifies_auth_errors() {
    let mut endpoint = test_endpoint("ep-auth");
    endpoint.model_catalog_status = harness_sources::EndpointModelCatalogStatus::Error;
    endpoint.model_catalog_error = Some("model discovery failed with status 401".to_string());
    let (status, auth_required, message, endpoint_status) =
        endpoint_catalog_verify_outcome(&endpoint);
    assert_eq!(status, "auth_required");
    assert_eq!(auth_required, Some(true));
    assert!(message
        .as_deref()
        .is_some_and(|value| value.contains("status 401")));
    assert_eq!(endpoint_status, HarnessEndpointVerificationStatus::Invalid);
}

#[test]
fn endpoint_catalog_runtime_probe_failure_preserves_endpoint_status() {
    let (status, auth_required, message, endpoint_status) = endpoint_catalog_runtime_probe_failure(
        "connection refused while launching bundled runtime".to_string(),
        HarnessEndpointVerificationStatus::Valid,
    );
    assert_eq!(status, "network_error");
    assert_eq!(auth_required, Some(false));
    assert!(message
        .as_deref()
        .is_some_and(|value| value.contains("connection refused")));
    assert_eq!(endpoint_status, HarnessEndpointVerificationStatus::Valid);
}

#[test]
fn provider_auth_mode_prefers_endpoint_for_active_endpoint_selection() {
    let endpoint = harness_sources::HarnessProviderSourceConfig {
        provider_id: "codex".to_string(),
        selected_source_kind: HarnessSourceKind::Endpoint,
        selected_endpoint_id: Some("ep-1".to_string()),
        endpoints: vec![test_endpoint("ep-1")],
    };
    assert_eq!(provider_auth_mode(true, Some(&endpoint)), "endpoint");

    let subscription = harness_sources::HarnessProviderSourceConfig {
        provider_id: "codex".to_string(),
        selected_source_kind: HarnessSourceKind::Subscription,
        selected_endpoint_id: None,
        endpoints: vec![],
    };
    assert_eq!(
        provider_auth_mode(true, Some(&subscription)),
        "subscription"
    );
    assert_eq!(provider_auth_mode(false, Some(&subscription)), "none");
}
