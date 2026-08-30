use std::env;

use anyhow::{bail, Context, Result};
use ctx_daemon_cli::{ExternalSemanticSpace, SemanticEmbeddingExecutorConfig};

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
    space_id: Option<&str>,
    dimensions: Option<u64>,
) -> Result<SemanticEmbeddingExecutorConfig> {
    match (executor, space_id, dimensions) {
        (None | Some("builtin"), None, None) => Ok(SemanticEmbeddingExecutorConfig::builtin()),
        (Some("builtin"), _, _) => {
            bail!("semantic.space_id and semantic.dimensions are not allowed with the builtin semantic executor")
        }
        (None, _, _) => bail!(
            "semantic.space_id and semantic.dimensions require semantic.executor to be an HTTP endpoint"
        ),
        (Some(endpoint), None, None) => {
            SemanticEmbeddingExecutorConfig::legacy_fixed_http(endpoint)
        }
        (Some(endpoint), Some(space_id), Some(dimensions)) => {
            let dimensions = usize::try_from(dimensions)
                .context("semantic.dimensions exceeds this platform's supported integer range")?;
            SemanticEmbeddingExecutorConfig::http(
                endpoint,
                ExternalSemanticSpace::new(space_id, dimensions)?,
            )
        }
        (Some(_), _, _) => bail!(
            "semantic.space_id and semantic.dimensions must either both be present or both be absent for a legacy HTTP selection"
        ),
    }
}

pub(crate) fn rebind_semantic_embedding_auth_endpoint_for_explicit_selection(executor: &str) {
    if executor == "builtin"
        || env::var_os(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV).is_none()
    {
        env::remove_var(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV);
    } else {
        env::set_var(
            ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV,
            executor,
        );
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
