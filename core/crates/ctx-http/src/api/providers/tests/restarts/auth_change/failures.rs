use super::fixtures::{fixture_with_adapter, seed_options_probe_cache};
use super::*;
use ctx_provider_accounts as provider_accounts;

#[tokio::test]
async fn restart_provider_for_auth_change_returns_error_when_adapter_restart_fails() {
    let adapter = Arc::new(RestartFailingAdapter::default());
    let fixture = fixture_with_adapter(adapter.clone() as Arc<dyn ProviderAdapter>).await;
    let daemon = fixture.daemon();

    seed_options_probe_cache(daemon, "ws-a/host/codex", "codex", false).await;

    let err = fixture
        .restart_provider_for_auth_change("codex", "test auth updated")
        .await
        .expect_err("restart failure should bubble up");
    assert!(err
        .to_string()
        .contains("provider auth updated but drain-restart failed for codex"));

    let options_cached = daemon
        .provider_options_probe_cache_contains_for_test("ws-a/host/codex")
        .await;
    assert!(!options_cached);

    assert_eq!(adapter.restart_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn set_codex_active_account_returns_error_when_restart_fails() {
    let fixture = fixture_with_adapter(Arc::new(RestartFailingAdapter::default())).await;
    let daemon = fixture.daemon();
    provider_accounts::upsert_codex_account(
        daemon.data_root(),
        provider_accounts::CodexAccountEntry {
            id: "acct".to_string(),
            label: "Account".to_string(),
            kind: provider_accounts::CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: Some("acct@example.com".to_string()),
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: provider_accounts::CodexEndpointProfile::default(),
        },
    )
    .await
    .expect("seed codex account");

    let err = set_codex_active_account(
        State(fixture.provider_accounts()),
        Json(
            serde_json::from_value(serde_json::json!({ "account_id": "acct" }))
                .expect("deserialize active-account request"),
        ),
    )
    .await
    .expect_err("restart failure should surface");
    assert_eq!(err.0, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(err
        .1
         .0
        .error
        .contains("provider auth updated but drain-restart failed"));
}

#[tokio::test]
async fn delete_codex_account_keeps_account_when_restart_fails() {
    let adapter = Arc::new(RestartFailingAdapter::default());
    let fixture = fixture_with_adapter(adapter.clone() as Arc<dyn ProviderAdapter>).await;
    let daemon = fixture.daemon();
    provider_accounts::upsert_codex_account(
        daemon.data_root(),
        provider_accounts::CodexAccountEntry {
            id: "acct-delete".to_string(),
            label: "Account".to_string(),
            kind: provider_accounts::CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: Some("delete@example.com".to_string()),
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: provider_accounts::CodexEndpointProfile::default(),
        },
    )
    .await
    .expect("seed codex account");
    let broker_home = provider_accounts::codex_broker_home(daemon.data_root(), "acct-delete");
    tokio::fs::create_dir_all(&broker_home)
        .await
        .expect("create broker home");
    tokio::fs::write(
        broker_home.join("auth.json"),
        br#"{"tokens":{"access_token":"token","refresh_token":"refresh"}}"#,
    )
    .await
    .expect("write broker auth");

    let err = match fixture
        .provider_accounts()
        .delete_codex_account_for_route("acct-delete")
        .await
    {
        Ok(_) => panic!("restart failure should surface before deletion"),
        Err(err) => err,
    };
    assert!(err
        .message()
        .contains("provider auth removed but immediate restart failed"));

    let registry = provider_accounts::load_codex_registry(daemon.data_root())
        .await
        .expect("registry");
    assert_eq!(registry.accounts.len(), 1);
    assert_eq!(registry.accounts[0].id, "acct-delete");
    assert!(broker_home.join("auth.json").exists());
    assert_eq!(adapter.restart_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        adapter
            .restart_modes
            .lock()
            .expect("restart modes")
            .as_slice(),
        &[ProviderRestartMode::Immediate]
    );
}

#[tokio::test]
async fn delete_codex_account_stops_provider_immediately_before_broker_cleanup() {
    let adapter = Arc::new(RestartTrackingAdapter::default());
    let fixture = fixture_with_adapter(adapter.clone() as Arc<dyn ProviderAdapter>).await;
    let daemon = fixture.daemon();
    provider_accounts::upsert_codex_account(
        daemon.data_root(),
        provider_accounts::CodexAccountEntry {
            id: "acct-delete".to_string(),
            label: "Account".to_string(),
            kind: provider_accounts::CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: Some("delete@example.com".to_string()),
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: provider_accounts::CodexEndpointProfile::default(),
        },
    )
    .await
    .expect("seed codex account");
    let broker_home = provider_accounts::codex_broker_home(daemon.data_root(), "acct-delete");
    tokio::fs::create_dir_all(&broker_home)
        .await
        .expect("create broker home");
    tokio::fs::write(
        broker_home.join("auth.json"),
        br#"{"tokens":{"access_token":"token","refresh_token":"refresh"}}"#,
    )
    .await
    .expect("write broker auth");

    fixture
        .provider_accounts()
        .delete_codex_account_for_route("acct-delete")
        .await
        .expect("remove account");

    let registry = provider_accounts::load_codex_registry(daemon.data_root())
        .await
        .expect("registry");
    assert!(registry.accounts.is_empty());
    assert!(
        tokio::fs::metadata(&broker_home).await.is_err(),
        "broker home should be cleaned only after the immediate provider stop"
    );
    assert_eq!(adapter.restart_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        adapter
            .restart_modes
            .lock()
            .expect("restart modes")
            .as_slice(),
        &[ProviderRestartMode::Immediate]
    );
}

#[tokio::test]
async fn delete_unknown_codex_account_does_not_stop_provider() {
    let adapter = Arc::new(RestartTrackingAdapter::default());
    let fixture = fixture_with_adapter(adapter.clone() as Arc<dyn ProviderAdapter>).await;

    let err = match fixture
        .provider_accounts()
        .delete_codex_account_for_route("missing")
        .await
    {
        Ok(_) => panic!("missing account should not be deleted"),
        Err(err) => err,
    };
    assert!(err.message().contains("unknown account"));
    assert_eq!(adapter.restart_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn delete_codex_account_keeps_account_when_broker_cleanup_fails() {
    let adapter = Arc::new(RestartTrackingAdapter::default());
    let fixture = fixture_with_adapter(adapter.clone() as Arc<dyn ProviderAdapter>).await;
    let daemon = fixture.daemon();
    provider_accounts::upsert_codex_account(
        daemon.data_root(),
        provider_accounts::CodexAccountEntry {
            id: "acct-delete".to_string(),
            label: "Account".to_string(),
            kind: provider_accounts::CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: Some("delete@example.com".to_string()),
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: provider_accounts::CodexEndpointProfile::default(),
        },
    )
    .await
    .expect("seed codex account");
    let broker_home = provider_accounts::codex_broker_home(daemon.data_root(), "acct-delete");
    let broker_root = broker_home.parent().expect("broker root");
    tokio::fs::create_dir_all(broker_root.parent().expect("brokers dir"))
        .await
        .expect("create brokers dir");
    tokio::fs::write(broker_root, b"not a directory")
        .await
        .expect("write broker root file");

    let err = match fixture
        .provider_accounts()
        .delete_codex_account_for_route("acct-delete")
        .await
    {
        Ok(_) => panic!("broker cleanup failure should surface before deletion"),
        Err(err) => err,
    };
    assert!(err
        .message()
        .contains("removing Codex broker home directory"));

    let registry = provider_accounts::load_codex_registry(daemon.data_root())
        .await
        .expect("registry");
    assert_eq!(registry.accounts.len(), 1);
    assert_eq!(registry.accounts[0].id, "acct-delete");
    assert_eq!(adapter.restart_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        adapter
            .restart_modes
            .lock()
            .expect("restart modes")
            .as_slice(),
        &[ProviderRestartMode::Immediate]
    );
}
