use super::fixtures::{fixture_with_adapter, seed_options_probe_cache};
use super::*;

#[tokio::test]
async fn restart_provider_for_auth_change_skips_adapters_without_drain_restart() {
    let fixture =
        fixture_with_adapter(Arc::new(UnsupportedRestartAdapter) as Arc<dyn ProviderAdapter>).await;
    let daemon = fixture.daemon();

    seed_options_probe_cache(daemon, "ws-a/host/codex", "codex", false).await;

    fixture
        .restart_provider_for_auth_change("codex", "test auth updated")
        .await
        .expect("unsupported restart should be skipped");

    let options_cached = daemon
        .provider_options_probe_cache_contains_for_test("ws-a/host/codex")
        .await;
    assert!(!options_cached);
}
