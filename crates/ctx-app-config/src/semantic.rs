use anyhow::{bail, Context, Result};
use ctx_semantic_model::{ExternalSemanticSpace, SemanticEmbeddingExecutorConfig};

use super::{AppConfig, SEMANTIC_BUILTIN_THROTTLING_DEFAULT_ENABLED};

#[derive(Debug, Clone)]
pub struct SemanticConfig {
    pub executor: SemanticEmbeddingExecutorConfig,
    pub(super) builtin_throttling: Option<bool>,
}

impl Default for SemanticConfig {
    fn default() -> Self {
        Self {
            executor: SemanticEmbeddingExecutorConfig::builtin_with_throttling(
                SEMANTIC_BUILTIN_THROTTLING_DEFAULT_ENABLED,
            ),
            builtin_throttling: None,
        }
    }
}

impl SemanticConfig {
    pub const fn builtin_throttling_configured(&self) -> bool {
        match self.builtin_throttling {
            Some(enabled) => enabled,
            None => SEMANTIC_BUILTIN_THROTTLING_DEFAULT_ENABLED,
        }
    }

    pub const fn builtin_throttling_effective(&self) -> Option<bool> {
        self.executor.builtin_throttling()
    }

    pub const fn builtin_throttling_source(&self) -> &'static str {
        if self.builtin_throttling.is_some() {
            "config"
        } else {
            "default"
        }
    }

    pub const fn builtin_throttling_reason(&self) -> Option<&'static str> {
        if self.builtin_throttling_effective().is_none() {
            Some("external_executor")
        } else {
            None
        }
    }
}

impl AppConfig {
    pub const fn semantic_builtin_throttling_configured(&self) -> bool {
        self.semantic.builtin_throttling_configured()
    }

    pub const fn semantic_builtin_throttling_effective(&self) -> Option<bool> {
        self.semantic.builtin_throttling_effective()
    }

    pub const fn semantic_builtin_throttling_source(&self) -> &'static str {
        self.semantic.builtin_throttling_source()
    }

    pub const fn semantic_builtin_throttling_reason(&self) -> Option<&'static str> {
        self.semantic.builtin_throttling_reason()
    }
}

pub(super) fn parse_semantic_embedding_executor(
    executor: Option<&str>,
    space_id: Option<&str>,
    dimensions: Option<u64>,
    builtin_throttling: Option<bool>,
) -> Result<SemanticEmbeddingExecutorConfig> {
    match (executor, space_id, dimensions) {
        (None | Some("builtin"), None, None) => Ok(
            SemanticEmbeddingExecutorConfig::builtin_with_throttling(
                builtin_throttling.unwrap_or(SEMANTIC_BUILTIN_THROTTLING_DEFAULT_ENABLED),
            ),
        ),
        (Some("builtin"), _, _) => {
            bail!("semantic.space_id and semantic.dimensions are not allowed with the builtin semantic executor")
        }
        (None, _, _) => bail!(
            "semantic.space_id and semantic.dimensions require semantic.executor to be an HTTP endpoint"
        ),
        (Some(_), _, _) if builtin_throttling.is_some() => bail!(
            "semantic.builtin_throttling is only valid with the builtin semantic executor"
        ),
        (Some(endpoint), None, None) => SemanticEmbeddingExecutorConfig::legacy_fixed_http(endpoint),
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
