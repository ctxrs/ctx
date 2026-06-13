use super::*;
use ctx_provider_accounts as provider_accounts;
use ctx_providers::adapters::{
    ProviderAdapter, ProviderHealth, ProviderProcessInfo, ProviderRestartMode, ProviderStatus,
    RunHandle, TurnInput,
};
use std::collections::HashMap;
use std::path::PathBuf;

#[tokio::test]
async fn codex_login_persistence_requires_auth_file() {
    let fixture = crate::test_support::TestDaemonFixture::new("http://127.0.0.1:4399").await;
    let daemon = fixture.daemon();
    let account_id = "acct-missing-auth";
    provider_accounts::ensure_codex_account_dir(daemon.data_root(), account_id)
        .await
        .unwrap();

    let err = daemon
        .persist_successful_codex_login_for_test(account_id, "Missing Auth".to_string(), None, None)
        .await
        .unwrap_err();

    assert!(err
        .to_string()
        .contains("missing persisted codex auth file"));
    let registry = provider_accounts::load_codex_registry(daemon.data_root())
        .await
        .unwrap();
    assert!(registry.accounts.is_empty());
    assert!(registry.active_account_id.is_none());
    assert!(!provider_accounts::codex_broker_home(daemon.data_root(), account_id).exists());
}

struct RestartFailingAdapter;

#[async_trait::async_trait]
impl ProviderAdapter for RestartFailingAdapter {
    async fn inspect(&self) -> anyhow::Result<ProviderStatus> {
        Ok(ProviderStatus {
            provider_id: "codex".to_string(),
            installed: true,
            detected_path: None,
            version: Some("test".to_string()),
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ctx_providers::adapters::ProviderUsability::default(),
        })
    }

    async fn run(
        &self,
        _input: TurnInput,
        _workdir: PathBuf,
        _env: HashMap<String, String>,
        _event_sink: tokio::sync::mpsc::Sender<ctx_providers::events::NormalizedEvent>,
        _hooks: ctx_providers::adapters::ProviderRunHooks,
    ) -> anyhow::Result<RunHandle> {
        anyhow::bail!("run not used in this test")
    }

    async fn cancel(&self, _handle: &mut RunHandle) -> anyhow::Result<()> {
        Ok(())
    }

    async fn list_processes(&self) -> Vec<ProviderProcessInfo> {
        Vec::new()
    }

    async fn restart(&self, _reason: &str, _mode: ProviderRestartMode) -> anyhow::Result<()> {
        anyhow::bail!("restart failed")
    }

    fn supports_restart_mode(&self, _mode: ProviderRestartMode) -> bool {
        true
    }
}

#[tokio::test]
async fn codex_login_persistence_rolls_back_when_restart_fails() {
    let fixture = crate::test_support::TestDaemonFixture::with_providers(
        HashMap::from([(
            "codex".to_string(),
            Arc::new(RestartFailingAdapter) as Arc<dyn ProviderAdapter>,
        )]),
        "http://127.0.0.1:4399",
    )
    .await;
    let daemon = fixture.daemon();
    let account_id = "acct-restart-fails";
    let account_dir = provider_accounts::ensure_codex_account_dir(daemon.data_root(), account_id)
        .await
        .unwrap();
    tokio::fs::write(
        account_dir.join("auth.json"),
        "{\"tokens\":{\"access_token\":\"token\",\"refresh_token\":\"refresh\"}}",
    )
    .await
    .unwrap();

    let err = daemon
        .persist_successful_codex_login_for_test(
            account_id,
            "Restart Fails".to_string(),
            Some("restart@example.com".to_string()),
            None,
        )
        .await
        .expect_err("restart failure should bubble up");
    assert!(!err.to_string().is_empty());

    let registry = provider_accounts::load_codex_registry(daemon.data_root())
        .await
        .unwrap();
    assert!(registry.accounts.is_empty());
    assert!(registry.active_account_id.is_none());
}

#[tokio::test]
async fn codex_login_persistence_removes_account_home_auth_after_secret_ingest() {
    let fixture = crate::test_support::TestDaemonFixture::new("http://127.0.0.1:4399").await;
    let daemon = fixture.daemon();
    let account_id = "acct-secret-store";
    let account_dir = provider_accounts::ensure_codex_account_dir(daemon.data_root(), account_id)
        .await
        .unwrap();
    tokio::fs::write(
        account_dir.join("auth.json"),
        "{\"tokens\":{\"access_token\":\"token\",\"refresh_token\":\"refresh\"}}",
    )
    .await
    .unwrap();

    daemon
        .persist_successful_codex_login_for_test(
            account_id,
            "Secret Store".to_string(),
            Some("secret@example.com".to_string()),
            Some("pro".to_string()),
        )
        .await
        .unwrap();

    let registry = provider_accounts::load_codex_registry(daemon.data_root())
        .await
        .unwrap();
    let entry = registry
        .accounts
        .iter()
        .find(|entry| entry.id == account_id)
        .expect("persisted account");
    let secret_ref = entry.secret_ref.as_deref().expect("secret_ref");
    assert_eq!(registry.active_account_id.as_deref(), Some(account_id));
    assert!(provider_accounts::codex_secrets_root(daemon.data_root())
        .join(secret_ref)
        .exists());
    assert!(tokio::fs::metadata(account_dir.join("auth.json"))
        .await
        .is_err());
}
