use anyhow::{bail, Context, Result};
use ctx_semantic_model::{ExternalSemanticSpace, SemanticEmbeddingExecutorConfig};

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
