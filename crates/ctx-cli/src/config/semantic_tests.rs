use std::{ffi::OsString, fs};

use super::*;

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

    let values =
        parse_toml_subset("[semantic]\nexecutor = \"https://embed.example.test/base\"\n").unwrap();
    default.apply_values(&values).unwrap();

    assert_eq!(
        default.semantic_embedding_executor().kind().as_str(),
        "http"
    );
    assert_eq!(
        default.semantic_embedding_executor().http_endpoint(),
        Some("https://embed.example.test/base/")
    );

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
        ("[semantic]\nexecutor = \"http\"\n", "endpoint is invalid"),
        (
            "[semantic]\nexecutor = \"http://example.test\"\n",
            "plain HTTP",
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

    let error = set_semantic_search_enabled_with_executor(temp.path(), Some("http://example.test"))
        .unwrap_err();
    assert!(format!("{error:#}").contains("plain HTTP"));
    assert_eq!(fs::read_to_string(&path).unwrap(), before);

    set_semantic_search_enabled_with_executor(temp.path(), Some("https://embed.example.test"))
        .unwrap();
    let external = AppConfig::load(temp.path()).unwrap();
    assert!(external.semantic_search_enabled());
    assert_eq!(
        external.semantic_embedding_executor().http_endpoint(),
        Some("https://embed.example.test/")
    );
    let once = fs::read_to_string(&path).unwrap();
    assert!(once.contains("# retained"));
    assert!(once.contains("[semantic]"));
    assert!(once.contains("executor = \"https://embed.example.test/\""));
    assert!(!once.contains("endpoint ="));

    set_semantic_search_enabled_with_executor(temp.path(), None).unwrap();
    let builtin = AppConfig::load(temp.path()).unwrap();
    assert!(builtin.semantic_embedding_executor().is_builtin());
    assert!(builtin.semantic_search_enabled());
    let reset = fs::read_to_string(&path).unwrap();
    assert!(reset.contains("# retained"));
    assert!(!reset.contains("[semantic]"));
}

struct EnvRestore {
    name: &'static str,
    original: Option<OsString>,
}

impl EnvRestore {
    fn capture(name: &'static str) -> Self {
        Self {
            name,
            original: std::env::var_os(name),
        }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        match &self.original {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

#[test]
fn auth_endpoint_binding_requires_enabled_http_and_the_token() {
    let _lock = TEST_LOCAL_USAGE_ENV_LOCK.lock().unwrap();
    let _token = EnvRestore::capture(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV);
    let _binding = EnvRestore::capture(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV);
    let mut config = AppConfig::default();
    config.semantic.executor =
        ctx_daemon_cli::SemanticEmbeddingExecutorConfig::http("https://embed.example.test/base")
            .unwrap();
    config.search.semantic = Some(true);

    std::env::set_var(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV, "secret");
    bind_semantic_embedding_auth_endpoint(&config);
    assert_eq!(
        std::env::var(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV).unwrap(),
        "https://embed.example.test/base/"
    );

    std::env::set_var(
        ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV,
        "https://independently-bound.example.test/",
    );
    bind_semantic_embedding_auth_endpoint(&config);
    assert_eq!(
        std::env::var(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV).unwrap(),
        "https://independently-bound.example.test/"
    );
    rebind_semantic_embedding_auth_endpoint(&config);
    assert_eq!(
        std::env::var(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV).unwrap(),
        "https://embed.example.test/base/"
    );

    config.semantic.executor =
        ctx_daemon_cli::SemanticEmbeddingExecutorConfig::http("http://127.0.0.1:8080").unwrap();
    bind_semantic_embedding_auth_endpoint(&config);
    assert!(std::env::var_os(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV).is_none());
    rebind_semantic_embedding_auth_endpoint(&config);
    assert_eq!(
        std::env::var(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV).unwrap(),
        "http://127.0.0.1:8080/"
    );
    clear_semantic_embedding_auth_endpoint();
    std::env::set_var(
        ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV,
        "http://127.0.0.1:8080/",
    );
    bind_semantic_embedding_auth_endpoint(&config);
    assert_eq!(
        std::env::var(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV).unwrap(),
        "http://127.0.0.1:8080/"
    );

    config.semantic.executor =
        ctx_daemon_cli::SemanticEmbeddingExecutorConfig::http("https://embed.example.test/base")
            .unwrap();

    std::env::remove_var(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV);
    bind_semantic_embedding_auth_endpoint(&config);
    assert_eq!(
        std::env::var(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV).unwrap(),
        "http://127.0.0.1:8080/"
    );
    clear_semantic_embedding_auth_endpoint();
    assert!(std::env::var_os(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV).is_none());

    std::env::set_var(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV, "secret");
    std::env::set_var(
        ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV,
        "https://independently-bound.example.test/",
    );
    config.search.semantic = Some(false);
    bind_semantic_embedding_auth_endpoint(&config);
    assert_eq!(
        std::env::var(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV).unwrap(),
        "https://independently-bound.example.test/"
    );
    clear_semantic_embedding_auth_endpoint();
    assert!(std::env::var_os(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV).is_none());

    config.search.semantic = Some(true);
    config.semantic.executor = ctx_daemon_cli::SemanticEmbeddingExecutorConfig::builtin();
    std::env::set_var(
        ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV,
        "https://independently-bound.example.test/",
    );
    bind_semantic_embedding_auth_endpoint(&config);
    assert_eq!(
        std::env::var(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV).unwrap(),
        "https://independently-bound.example.test/"
    );
    rebind_semantic_embedding_auth_endpoint(&config);
    assert!(std::env::var_os(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV).is_none());
}
