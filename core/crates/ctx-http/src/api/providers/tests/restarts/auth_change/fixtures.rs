use super::*;

pub(super) struct ProviderRestartFixture {
    fixture: crate::test_support::TestDaemonFixture,
}

impl ProviderRestartFixture {
    pub(super) fn daemon(&self) -> &TestDaemon {
        self.fixture.daemon()
    }

    pub(super) fn provider_accounts(&self) -> ctx_daemon::daemon::ProviderAccountsHandle {
        self.fixture.provider_accounts()
    }

    pub(super) async fn restart_provider_for_auth_change(
        &self,
        provider_id: &str,
        reason: &str,
    ) -> anyhow::Result<()> {
        self.fixture
            .daemon()
            .restart_provider_for_auth_change_for_test(provider_id, reason)
            .await
    }
}

pub(super) async fn fixture_with_adapter(
    adapter: Arc<dyn ProviderAdapter>,
) -> ProviderRestartFixture {
    let fixture = crate::test_support::TestDaemonFixture::with_providers(
        HashMap::from([("codex".to_string(), adapter)]),
        "http://127.0.0.1:4310",
    )
    .await;

    ProviderRestartFixture { fixture }
}

pub(super) async fn seed_options_probe_cache(
    daemon: &TestDaemon,
    key: &str,
    provider_id: &str,
    probe_ok: bool,
) {
    daemon
        .seed_provider_options_probe_cache_for_test(key, provider_id, probe_ok)
        .await;
}

pub(super) async fn seed_verify_cache_status(daemon: &TestDaemon, key: &str, status: &str) {
    daemon
        .seed_provider_verify_cache_status_for_test(key, status)
        .await;
}
