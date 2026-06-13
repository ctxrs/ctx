use super::fixtures::{fixture_with_adapter, seed_options_probe_cache, seed_verify_cache_status};
use super::*;

#[tokio::test]
async fn restart_provider_for_auth_change_invalidates_only_matching_provider_probe_caches() {
    let adapter = Arc::new(RestartTrackingAdapter::default());
    let fixture = fixture_with_adapter(adapter.clone() as Arc<dyn ProviderAdapter>).await;
    let daemon = fixture.daemon();

    seed_options_probe_cache(daemon, "ws-a/host/codex", "codex", false).await;
    seed_options_probe_cache(daemon, "ws-b/container/claude-crp", "claude-crp", true).await;
    seed_verify_cache_status(daemon, "ws-a/host/codex", "error").await;
    seed_verify_cache_status(daemon, "ws-b/container/claude-crp", "ok").await;

    fixture
        .restart_provider_for_auth_change("codex", "test auth updated")
        .await
        .expect("restart should succeed");

    let codex_options_cached = daemon
        .provider_options_probe_cache_contains_for_test("ws-a/host/codex")
        .await;
    let claude_options_cached = daemon
        .provider_options_probe_cache_contains_for_test("ws-b/container/claude-crp")
        .await;
    assert!(!codex_options_cached);
    assert!(claude_options_cached);

    let codex_verify_cached = daemon
        .provider_verify_cache_contains_for_test("ws-a/host/codex")
        .await;
    let claude_verify_cached = daemon
        .provider_verify_cache_contains_for_test("ws-b/container/claude-crp")
        .await;
    assert!(!codex_verify_cached);
    assert!(claude_verify_cached);

    assert_eq!(adapter.restart_calls.load(Ordering::SeqCst), 1);
}
