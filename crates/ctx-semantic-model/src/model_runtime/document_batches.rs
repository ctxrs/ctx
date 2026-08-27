use std::time::Instant;

use anyhow::{anyhow, Context, Result};

use super::{
    reacquire_semantic_embedder, throttle_semantic_batch, SemanticIndexingExecutionPolicy,
    SemanticIndexingIntensity, SemanticModelConfig, SemanticQuietPolicy, SharedSemanticRuntime,
};

impl SharedSemanticRuntime {
    /// Embeds documents using the default quiet indexing intensity.
    ///
    /// New indexing callers that expose an intensity control should use
    /// [`Self::embed_documents_with_intensity`].
    pub fn embed_documents(
        &self,
        config: &SemanticModelConfig,
        texts: Vec<String>,
        deadline: Option<Instant>,
    ) -> Result<(Vec<Vec<f32>>, SemanticQuietPolicy)> {
        let (embeddings, execution_policy) = self.embed_documents_with_intensity(
            config,
            texts,
            SemanticIndexingIntensity::default(),
            deadline,
        )?;
        Ok((embeddings, execution_policy.quiet_policy()))
    }

    /// Embeds documents under an explicit document-indexing intensity.
    ///
    /// Intensity changes only inter-batch duty-cycle rest. Model selection,
    /// resource sizing, batch semantics, admission checks, and deadlines are
    /// shared with the quiet path.
    pub fn embed_documents_with_intensity(
        &self,
        config: &SemanticModelConfig,
        texts: Vec<String>,
        intensity: SemanticIndexingIntensity,
        deadline: Option<Instant>,
    ) -> Result<(Vec<Vec<f32>>, SemanticIndexingExecutionPolicy)> {
        self.embed_documents_with_intensity_resolver(config, texts, || intensity, deadline)
    }

    /// Embeds documents while resolving indexing intensity before every
    /// inference batch.
    ///
    /// Daemon callers use this form so a released or expired temporary lease
    /// restores quiet pacing without waiting for the current page to finish.
    pub fn embed_documents_with_intensity_resolver(
        &self,
        config: &SemanticModelConfig,
        texts: Vec<String>,
        mut resolve_intensity: impl FnMut() -> SemanticIndexingIntensity,
        deadline: Option<Instant>,
    ) -> Result<(Vec<Vec<f32>>, SemanticIndexingExecutionPolicy)> {
        let mut pending = texts.into_iter();
        if pending.len() == 0 {
            let embedder = self.lock()?;
            let quiet_policy = embedder
                .as_ref()
                .ok_or_else(|| anyhow!("semantic embedder was not initialized"))?
                .quiet_policy();
            return Ok((
                Vec::new(),
                resolve_batch_execution_policy(&mut resolve_intensity, quiet_policy),
            ));
        }

        let mut embeddings = Vec::with_capacity(pending.len());
        let mut final_execution_policy = None;
        while pending.len() != 0 {
            let mut embedder = self.lock()?;
            let batch_size = embedder
                .as_ref()
                .ok_or_else(|| anyhow!("semantic embedder was not initialized"))?
                .batch_size
                .max(1);
            let batch = next_document_batch(&mut pending, batch_size);
            let started = Instant::now();
            let first = embedder
                .as_mut()
                .ok_or_else(|| anyhow!("semantic embedder was not initialized"))?
                .embed_documents(batch.clone());
            let batch_embeddings = match first {
                Ok(embeddings) => embeddings,
                Err(first_error) => {
                    let runtime = embedder
                        .as_ref()
                        .ok_or_else(|| {
                            anyhow!("semantic embedder disappeared after inference failure")
                        })?
                        .runtime_info();
                    *embedder = None;
                    let mut replacement = reacquire_semantic_embedder(config, &runtime).context(
                        "reinitialize semantic embedder after document inference failure",
                    )?;
                    let retry = replacement.embed_documents(batch).with_context(|| {
                        format!(
                            "semantic document inference failed twice; first failure: {first_error:#}"
                        )
                    })?;
                    *embedder = Some(replacement);
                    retry
                }
            };
            let quiet_policy = embedder
                .as_ref()
                .ok_or_else(|| anyhow!("semantic embedder was not initialized"))?
                .quiet_policy();
            let execution_policy =
                resolve_batch_execution_policy(&mut resolve_intensity, quiet_policy);
            drop(embedder);

            embeddings.extend(batch_embeddings);
            final_execution_policy = Some(execution_policy);
            let active = started.elapsed();
            let remaining =
                deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
            throttle_semantic_batch(active, execution_policy, remaining);
        }

        Ok((
            embeddings,
            final_execution_policy.expect("non-empty semantic input must execute a batch"),
        ))
    }
}

fn resolve_batch_execution_policy(
    resolve_intensity: &mut impl FnMut() -> SemanticIndexingIntensity,
    quiet_policy: SemanticQuietPolicy,
) -> SemanticIndexingExecutionPolicy {
    SemanticIndexingExecutionPolicy::new(resolve_intensity(), quiet_policy)
}

fn next_document_batch(
    pending: &mut impl Iterator<Item = String>,
    batch_size: usize,
) -> Vec<String> {
    pending.take(batch_size.max(1)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    type CompatibilityDocumentOperation = fn(
        &SharedSemanticRuntime,
        &SemanticModelConfig,
        Vec<String>,
        Option<Instant>,
    ) -> Result<(Vec<Vec<f32>>, SemanticQuietPolicy)>;
    type ExplicitDocumentOperation = fn(
        &SharedSemanticRuntime,
        &SemanticModelConfig,
        Vec<String>,
        SemanticIndexingIntensity,
        Option<Instant>,
    )
        -> Result<(Vec<Vec<f32>>, SemanticIndexingExecutionPolicy)>;
    type QueryOperation = fn(
        &SharedSemanticRuntime,
        &SemanticModelConfig,
        String,
    ) -> Result<(Vec<f32>, crate::SemanticEmbeddingRuntimeInfo)>;

    #[test]
    fn document_embedding_apis_keep_quiet_compatibility_and_explicit_intensity() {
        let _compatibility_operation: CompatibilityDocumentOperation =
            SharedSemanticRuntime::embed_documents;
        let _explicit_operation: ExplicitDocumentOperation =
            SharedSemanticRuntime::embed_documents_with_intensity;
    }

    #[test]
    fn query_embedding_api_remains_intensity_independent() {
        let _query_operation: QueryOperation = SharedSemanticRuntime::embed_query;
    }

    fn collect_batches(count: usize, batch_size: usize) -> Vec<Vec<String>> {
        let mut pending = (0..count).map(|index| index.to_string());
        let mut batches = Vec::new();
        loop {
            let batch = next_document_batch(&mut pending, batch_size);
            if batch.is_empty() {
                return batches;
            }
            batches.push(batch);
        }
    }

    #[test]
    fn document_batches_preserve_empty_and_exact_inputs() {
        assert!(collect_batches(0, 4).is_empty());
        assert_eq!(collect_batches(4, 4), [vec!["0", "1", "2", "3"]]);
    }

    #[test]
    fn document_batches_preserve_order_across_full_and_tail_batches() {
        assert_eq!(
            collect_batches(7, 3),
            [vec!["0", "1", "2"], vec!["3", "4", "5"], vec!["6"]]
        );
    }

    #[test]
    fn each_batch_rechecks_dynamic_intensity_authority() {
        let quiet_policy = SemanticQuietPolicy {
            threads: 1,
            batch_size: 4,
            memory_budget_bytes: 1024,
            active_percent: 25,
        };
        let mut intensities = [
            SemanticIndexingIntensity::Full,
            SemanticIndexingIntensity::Quiet,
        ]
        .into_iter();
        let mut resolve = || intensities.next().expect("one intensity per batch");

        assert_eq!(
            resolve_batch_execution_policy(&mut resolve, quiet_policy).intensity(),
            SemanticIndexingIntensity::Full
        );
        assert_eq!(
            resolve_batch_execution_policy(&mut resolve, quiet_policy).intensity(),
            SemanticIndexingIntensity::Quiet
        );
    }
}
