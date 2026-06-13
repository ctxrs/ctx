use super::*;
use ctx_provider_accounts as provider_accounts;

#[tokio::test]
async fn cursor_subscription_selection_requires_managed_account() {
    let root = tempfile::tempdir().expect("tempdir");
    let source = harness_sources::HarnessProviderSourceConfig {
        provider_id: "cursor".to_string(),
        selected_source_kind: HarnessSourceKind::Subscription,
        selected_endpoint_id: None,
        endpoints: vec![],
    };
    let active = provider_has_active_auth_config(root.path(), "cursor", Some(&source))
        .await
        .unwrap();
    assert!(!active);
}

#[tokio::test]
async fn amp_subscription_selection_requires_managed_account() {
    let root = tempfile::tempdir().expect("tempdir");
    let source = harness_sources::HarnessProviderSourceConfig {
        provider_id: "amp".to_string(),
        selected_source_kind: HarnessSourceKind::Subscription,
        selected_endpoint_id: None,
        endpoints: vec![],
    };
    let active = provider_has_active_auth_config(root.path(), "amp", Some(&source))
        .await
        .unwrap();
    assert!(!active);
}

#[tokio::test]
async fn amp_active_account_counts_as_active_auth_config() {
    let root = tempfile::tempdir().expect("tempdir");
    provider_accounts::upsert_amp_account(
        root.path(),
        Some("Amp Test".to_string()),
        Some("amp@example.com".to_string()),
    )
    .await
    .expect("upsert amp account");
    let source = harness_sources::HarnessProviderSourceConfig {
        provider_id: "amp".to_string(),
        selected_source_kind: HarnessSourceKind::Subscription,
        selected_endpoint_id: None,
        endpoints: vec![],
    };
    let active = provider_has_active_auth_config(root.path(), "amp", Some(&source))
        .await
        .unwrap();
    assert!(active);
}
