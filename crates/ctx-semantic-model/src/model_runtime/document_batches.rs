use std::time::Instant;

use anyhow::{anyhow, Context, Result};

use super::{
    reacquire_semantic_embedder, throttle_semantic_batch, SemanticModelConfig, SemanticQuietPolicy,
    SharedSemanticRuntime,
};

impl SharedSemanticRuntime {
    pub fn embed_documents(
        &self,
        config: &SemanticModelConfig,
        texts: Vec<String>,
        deadline: Option<Instant>,
    ) -> Result<(Vec<Vec<f32>>, SemanticQuietPolicy)> {
        let mut pending = texts.into_iter();
        if pending.len() == 0 {
            let embedder = self.lock()?;
            let quiet_policy = embedder
                .as_ref()
                .ok_or_else(|| anyhow!("semantic embedder was not initialized"))?
                .quiet_policy();
            return Ok((Vec::new(), quiet_policy));
        }

        let mut embeddings = Vec::with_capacity(pending.len());
        let mut final_quiet_policy = None;
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
            drop(embedder);

            embeddings.extend(batch_embeddings);
            final_quiet_policy = Some(quiet_policy);
            let active = started.elapsed();
            let remaining =
                deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
            throttle_semantic_batch(active, quiet_policy, remaining);
        }

        Ok((
            embeddings,
            final_quiet_policy.expect("non-empty semantic input must execute a batch"),
        ))
    }
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
}
