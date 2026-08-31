use std::fs;

use super::*;

fn external_executor(
    endpoint: &str,
    space_id: &str,
    dimensions: usize,
) -> ctx_semantic_model::SemanticEmbeddingExecutorConfig {
    ctx_semantic_model::SemanticEmbeddingExecutorConfig::http(
        endpoint,
        ctx_semantic_model::ExternalSemanticSpace::new(space_id, dimensions).unwrap(),
    )
    .unwrap()
}

fn load_config_error(contents: &str) -> String {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join(CONFIG_FILE), contents).unwrap();
    format!("{:#}", AppConfig::load(temp.path()).unwrap_err())
}

#[test]
fn executor_defaults_to_builtin_and_parses_external_endpoint() {
    let mut default = AppConfig::default();
    assert_eq!(
        default.semantic_embedding_executor().kind().as_str(),
        "builtin"
    );
    assert_eq!(default.semantic_embedding_executor().http_endpoint(), None);

    let values = parse_toml_subset(
        "[semantic]\nexecutor = \"https://embed.example.test/base\"\nspace_id = \"acme/multilingual-v2\"\ndimensions = 768\n",
    )
    .unwrap();
    default.apply_values(&values).unwrap();

    assert_eq!(
        default.semantic_embedding_executor().kind().as_str(),
        "http"
    );
    assert_eq!(
        default.semantic_embedding_executor().http_endpoint(),
        Some("https://embed.example.test/base/")
    );
    let space = default
        .semantic_embedding_executor()
        .external_space()
        .unwrap();
    assert_eq!(space.space_id(), "acme/multilingual-v2");
    assert_eq!(space.dimensions(), 768);

    let values = parse_toml_subset("[semantic]\nexecutor = \"builtin\"\n").unwrap();
    default.apply_values(&values).unwrap();
    assert!(default.semantic_embedding_executor().is_builtin());
}

#[test]
fn executor_config_rejects_the_retired_endpoint_key_and_unsafe_urls() {
    for (contents, expected) in [
        (
            "[semantic]\nendpoint = \"https://embed.example.test\"\n",
            "unknown config key `semantic.endpoint`",
        ),
        (
            "[semantic]\nexecutor = \"http\"\nspace_id = \"space-v1\"\ndimensions = 384\n",
            "endpoint is invalid",
        ),
        (
            "[semantic]\nexecutor = \"http://example.test\"\nspace_id = \"space-v1\"\ndimensions = 384\n",
            "plain HTTP",
        ),
    ] {
        let error = load_config_error(contents);
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn executor_config_requires_one_coherent_offline_selection() {
    let legacy =
        parse_toml_subset("[semantic]\nexecutor = \"https://embed.example.test\"\n").unwrap();
    let mut legacy_config = AppConfig::default();
    legacy_config.apply_values(&legacy).unwrap();
    let legacy_executor = legacy_config.semantic_embedding_executor();
    assert_eq!(
        legacy_executor.http_endpoint(),
        Some("https://embed.example.test/")
    );
    assert!(legacy_executor.external_space().is_none());
    assert!(legacy_executor.is_legacy_fixed_http());
    assert_eq!(legacy_executor.http_protocol_schema_version(), Some(1));
    assert_eq!(
        legacy_executor.contract().fingerprint(),
        ctx_semantic_model::SemanticEmbeddingExecutorConfig::builtin()
            .contract()
            .fingerprint()
    );

    for (contents, expected) in [
        (
            "[semantic]\nexecutor = \"https://embed.example.test\"\nspace_id = \"space-v1\"\n",
            "must either both be present or both be absent",
        ),
        (
            "[semantic]\nspace_id = \"space-v1\"\ndimensions = 384\n",
            "require semantic.executor",
        ),
        (
            "[semantic]\nexecutor = \"builtin\"\nspace_id = \"space-v1\"\ndimensions = 384\n",
            "not allowed with the builtin",
        ),
    ] {
        let error = load_config_error(contents);
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn executor_enable_mutation_is_atomic_validated_and_builtin_is_invisible() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(CONFIG_FILE);
    fs::write(&path, "# retained\n[search]\nsemantic = false\n").unwrap();
    let before = fs::read_to_string(&path).unwrap();

    let invalid = ctx_semantic_model::SemanticEmbeddingExecutorConfig::http(
        "http://example.test",
        ctx_semantic_model::ExternalSemanticSpace::new("space-v1", 384).unwrap(),
    )
    .unwrap_err();
    assert!(format!("{invalid:#}").contains("plain HTTP"));
    assert_eq!(fs::read_to_string(&path).unwrap(), before);

    let external = external_executor("https://embed.example.test", "acme/multilingual-v2", 768);
    set_semantic_search_enabled_with_executor(temp.path(), &external).unwrap();
    let external = AppConfig::load(temp.path()).unwrap();
    assert!(external.semantic_search_enabled());
    assert_eq!(
        external.semantic_embedding_executor().http_endpoint(),
        Some("https://embed.example.test/")
    );
    let space = external
        .semantic_embedding_executor()
        .external_space()
        .unwrap();
    assert_eq!(space.space_id(), "acme/multilingual-v2");
    assert_eq!(space.dimensions(), 768);
    let once = fs::read_to_string(&path).unwrap();
    assert!(once.contains("# retained"));
    assert!(once.contains("[semantic]"));
    assert!(once.contains("executor = \"https://embed.example.test/\""));
    assert!(once.contains("space_id = \"acme/multilingual-v2\""));
    assert!(once.contains("dimensions = 768"));
    assert!(!once.contains("endpoint ="));

    set_semantic_search_enabled(temp.path(), false).unwrap();
    let disabled = AppConfig::load(temp.path()).unwrap();
    assert!(!disabled.semantic_search_enabled());
    assert_eq!(
        disabled.semantic_embedding_executor(),
        external.semantic_embedding_executor()
    );

    set_semantic_search_enabled_with_executor(
        temp.path(),
        &ctx_semantic_model::SemanticEmbeddingExecutorConfig::builtin(),
    )
    .unwrap();
    let builtin = AppConfig::load(temp.path()).unwrap();
    assert!(builtin.semantic_embedding_executor().is_builtin());
    assert!(builtin.semantic_search_enabled());
    let reset = fs::read_to_string(&path).unwrap();
    assert!(reset.contains("# retained"));
    assert!(!reset.contains("[semantic]"));
    assert!(!reset.contains("space_id"));
    assert!(!reset.contains("dimensions"));
}

#[test]
fn explicit_discovery_replaces_legacy_endpoint_only_selection_atomically() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(CONFIG_FILE);
    fs::write(
        &path,
        "[search]\nsemantic = true\n[semantic]\nexecutor = \"https://old.example.test\"\n",
    )
    .unwrap();
    let accepted = ctx_semantic_model::SemanticEmbeddingExecutorConfig::http(
        "https://new.example.test/base",
        ctx_semantic_model::ExternalSemanticSpace::new("operator/model-v2", 512).unwrap(),
    )
    .unwrap();

    super::mutation::set_semantic_search_enabled_with_executor(temp.path(), &accepted).unwrap();

    let updated = AppConfig::load(temp.path()).unwrap();
    assert!(updated.semantic_search_enabled());
    assert_eq!(updated.semantic_embedding_executor(), &accepted);
    let persisted = fs::read_to_string(path).unwrap();
    assert!(persisted.contains("space_id = \"operator/model-v2\""));
    assert!(persisted.contains("dimensions = 512"));
    assert!(!persisted.contains("old.example.test"));
}

#[test]
fn legacy_fixed_http_persistence_remains_endpoint_only() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(CONFIG_FILE);
    fs::write(
        &path,
        "[search]\nsemantic = false\n[semantic]\nexecutor = \"https://old.example.test\"\nspace_id = \"stale\"\ndimensions = 7\n",
    )
    .unwrap();
    let legacy = ctx_semantic_model::SemanticEmbeddingExecutorConfig::legacy_fixed_http(
        "https://legacy.example.test/base",
    )
    .unwrap();

    set_semantic_search_enabled_with_executor(temp.path(), &legacy).unwrap();

    let updated = AppConfig::load(temp.path()).unwrap();
    assert!(updated.semantic_search_enabled());
    assert!(updated.semantic_embedding_executor().is_legacy_fixed_http());
    assert_eq!(updated.semantic_embedding_executor(), &legacy);
    let persisted = fs::read_to_string(path).unwrap();
    assert!(persisted.contains("executor = \"https://legacy.example.test/base/\""));
    assert!(!persisted.contains("space_id"));
    assert!(!persisted.contains("dimensions"));
}
