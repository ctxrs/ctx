use super::*;

async fn seed_options_probe_cache(
    daemon: &TestDaemon,
    key: &str,
    provider_id: &str,
    probe_ok: bool,
) {
    daemon
        .seed_provider_options_probe_cache_for_test(key, provider_id, probe_ok)
        .await;
}

async fn seed_verify_cache_status(daemon: &TestDaemon, key: &str, status: &str) {
    daemon
        .seed_provider_verify_cache_status_for_test(key, status)
        .await;
}

#[tokio::test]
async fn select_provider_harness_source_invalidates_only_matching_provider_probe_caches() {
    let fixture = crate::test_support::TestDaemonFixture::new("http://127.0.0.1:4310").await;
    let daemon = fixture.daemon();

    seed_options_probe_cache(&daemon, "ws-a/host/codex", "codex", false).await;
    seed_options_probe_cache(&daemon, "ws-b/container/claude-crp", "claude-crp", true).await;
    seed_verify_cache_status(&daemon, "ws-a/host/codex", "error").await;
    seed_verify_cache_status(&daemon, "ws-b/container/claude-crp", "ok").await;

    let Json(config) = select_provider_harness_source(
        State(fixture.provider_harness_config()),
        Path("codex".to_string()),
        Json(SelectProviderHarnessSourceRouteRequest::new(
            HarnessSourceKind::Subscription,
            None,
        )),
    )
    .await
    .expect("select provider harness source");

    assert_eq!(config.provider_id, "codex");
    assert_eq!(config.selected_source_kind, HarnessSourceKind::Subscription);

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
}
