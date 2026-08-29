use std::env;

use anyhow::Result;
use ctx_daemon_cli::SemanticEmbeddingExecutorConfig;

use super::AppConfig;

#[derive(Debug, Clone)]
pub struct SemanticConfig {
    pub executor: SemanticEmbeddingExecutorConfig,
}

impl Default for SemanticConfig {
    fn default() -> Self {
        Self {
            executor: SemanticEmbeddingExecutorConfig::builtin(),
        }
    }
}

pub(super) fn parse_semantic_embedding_executor(
    executor: Option<&str>,
) -> Result<SemanticEmbeddingExecutorConfig> {
    match executor {
        None | Some("builtin") => Ok(SemanticEmbeddingExecutorConfig::builtin()),
        Some(endpoint) => SemanticEmbeddingExecutorConfig::http(endpoint),
    }
}

pub(crate) fn bind_semantic_embedding_auth_endpoint(config: &AppConfig) {
    bind_semantic_embedding_auth_endpoint_with(config, false);
}

pub(crate) fn rebind_semantic_embedding_auth_endpoint(config: &AppConfig) {
    bind_semantic_embedding_auth_endpoint_with(config, true);
}

pub(crate) fn clear_semantic_embedding_auth_endpoint() {
    env::remove_var(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV);
}

fn bind_semantic_embedding_auth_endpoint_with(config: &AppConfig, explicit_selection: bool) {
    let endpoint = config
        .semantic_search_enabled()
        .then(|| env::var_os(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV))
        .flatten()
        .and_then(|_| config.semantic_embedding_executor().http_endpoint());
    match endpoint {
        Some(endpoint)
            if !config
                .semantic_embedding_executor()
                .scope()
                .content_leaves_machine() =>
        {
            if explicit_selection {
                env::set_var(
                    ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV,
                    endpoint,
                );
            } else if !env::var(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV)
                .is_ok_and(|binding| binding == endpoint)
            {
                env::remove_var(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV);
            }
        }
        Some(endpoint)
            if config
                .semantic_embedding_executor()
                .scope()
                .content_leaves_machine()
                && (explicit_selection
                    || env::var_os(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV)
                        .is_none()) =>
        {
            env::set_var(
                ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV,
                endpoint,
            );
        }
        // Preserve an independently supplied endpoint binding. A mismatch is
        // rejected by the executor before any request. Loopback receives an
        // inherited token only when the caller explicitly pre-bound it.
        Some(_) => {}
        None if explicit_selection => {
            env::remove_var(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV);
        }
        None => {}
    }
}
