use super::*;

#[test]
fn classify_probe_error_detects_auth_required_messages() {
    let (status, auth_required, endpoint_status) =
        classify_probe_error("401 unauthorized: missing api key");
    assert_eq!(status, "auth_required");
    assert_eq!(auth_required, Some(true));
    assert_eq!(endpoint_status, HarnessEndpointVerificationStatus::Invalid);
}

#[test]
fn classify_probe_error_treats_models_list_protocol_failures_as_generic_errors() {
    let message = "CRP models.list probe timed out after 10s; stderr_tail=Cursor CLI authenticated | Invalid message { type: 'models.list' }";
    let (status, auth_required, endpoint_status) = classify_probe_error(message);
    assert_eq!(status, "error");
    assert_eq!(auth_required, Some(false));
    assert_eq!(endpoint_status, HarnessEndpointVerificationStatus::Error);
}

#[test]
fn selected_endpoint_from_harness_config_prefers_endpoint_selection() {
    let endpoint =
        selected_endpoint_from_harness_config(Some(harness_sources::HarnessProviderSourceConfig {
            provider_id: "codex".to_string(),
            selected_source_kind: HarnessSourceKind::Endpoint,
            selected_endpoint_id: Some("ep-123".to_string()),
            endpoints: Vec::new(),
        }));
    assert_eq!(endpoint.as_deref(), Some("ep-123"));

    let subscription =
        selected_endpoint_from_harness_config(Some(harness_sources::HarnessProviderSourceConfig {
            provider_id: "codex".to_string(),
            selected_source_kind: HarnessSourceKind::Subscription,
            selected_endpoint_id: Some("ep-123".to_string()),
            endpoints: Vec::new(),
        }));
    assert!(subscription.is_none());
}

#[test]
fn selected_endpoint_record_from_harness_config_returns_selected_record() {
    let selected = selected_endpoint_record_from_harness_config(Some(
        &harness_sources::HarnessProviderSourceConfig {
            provider_id: "codex".to_string(),
            selected_source_kind: HarnessSourceKind::Endpoint,
            selected_endpoint_id: Some("ep-2".to_string()),
            endpoints: vec![test_endpoint("ep-1"), test_endpoint("ep-2")],
        },
    ))
    .expect("selected endpoint");
    assert_eq!(selected.id, "ep-2");

    let missing = selected_endpoint_record_from_harness_config(Some(
        &harness_sources::HarnessProviderSourceConfig {
            provider_id: "codex".to_string(),
            selected_source_kind: HarnessSourceKind::Endpoint,
            selected_endpoint_id: Some("ep-3".to_string()),
            endpoints: vec![test_endpoint("ep-1"), test_endpoint("ep-2")],
        },
    ));
    assert!(missing.is_none());
}
