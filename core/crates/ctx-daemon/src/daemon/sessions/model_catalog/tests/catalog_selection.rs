use super::*;
use ctx_provider_runtime::CachedProviderOptions;

#[tokio::test]
async fn load_provider_model_catalog_reads_target_scoped_options_cache() {
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture = test_model_catalog_fixture(temp.path(), 4310).await;
    save_sandbox_execution_mode(&fixture).await;

    fixture
        .providers()
        .with_provider_options_cache(|cache| {
            cache.insert(
                format!("{}/container/codex", fixture.workspace().id.0),
                CachedProviderOptions {
                    cached_at: std::time::Instant::now(),
                    value: serde_json::json!({
                        "models": {
                            "models": [
                                { "id": "gpt-5" },
                                { "id": "gpt-5/high" }
                            ],
                            "current_model_id": "gpt-5",
                            "meta": {
                                "source_kind": "subscription",
                                "catalog_source": "runtime_probe_live",
                                "refresh_pending": false
                            }
                        }
                    }),
                },
            );
        })
        .await;

    let catalog = load_provider_model_catalog(fixture.host(), fixture.workspace(), "codex")
        .await
        .expect("load catalog")
        .expect("catalog");

    assert!(catalog.full_ids().iter().any(|id| id == "gpt-5"));
    assert!(catalog.full_ids().iter().any(|id| id == "gpt-5/high"));
    assert_eq!(catalog.current_model_id(), Some("gpt-5"));
}

#[tokio::test]
async fn load_provider_model_catalog_falls_back_to_pinned_gemini_catalog() {
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture = test_model_catalog_fixture(temp.path(), 4311).await;
    seed_ready_gemini_status(&fixture).await;

    let catalog = load_provider_model_catalog(fixture.host(), fixture.workspace(), "gemini")
        .await
        .expect("load catalog")
        .expect("catalog");

    assert_eq!(catalog.current_model_id(), Some("auto-gemini-3"));
    assert!(catalog.full_ids().iter().any(|id| id == "auto-gemini-3"));
    assert!(catalog
        .full_ids()
        .iter()
        .any(|id| id == "gemini-3-pro-preview"));
}
