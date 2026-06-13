use super::*;

#[tokio::test]
async fn load_provider_model_catalog_surfaces_harness_config_errors() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_invalid_harness_registry(temp.path());
    let fixture = test_model_catalog_fixture(temp.path(), 4312).await;

    let err = load_provider_model_catalog(fixture.host(), fixture.workspace(), "qwen")
        .await
        .expect_err("harness config error should surface");
    assert!(err.contains("parsing harness source registry"));
}

#[tokio::test]
async fn load_provider_model_catalog_surfaces_agent_server_config_errors() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_invalid_agent_server_config(temp.path());
    let fixture = test_model_catalog_fixture(temp.path(), 4313).await;

    let err = load_provider_model_catalog(fixture.host(), fixture.workspace(), "qwen")
        .await
        .expect_err("managed config error should surface");
    assert!(err.contains("parsing agent server config"));
}

#[tokio::test]
async fn load_provider_model_catalog_surfaces_agent_server_config_errors_for_pinned_catalogs() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_invalid_agent_server_config(temp.path());
    let fixture = test_model_catalog_fixture(temp.path(), 4314).await;

    let err = load_provider_model_catalog(fixture.host(), fixture.workspace(), "gemini")
        .await
        .expect_err("managed config error should surface for pinned catalogs too");
    assert!(err.contains("parsing agent server config"));
}
