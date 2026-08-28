use std::time::Instant;

#[cfg(not(ctx_semantic_fastembed))]
use anyhow::anyhow;
use anyhow::Result;

#[cfg(not(ctx_semantic_fastembed))]
use crate::SEMANTIC_MODEL_ID;
use crate::{
    semantic_model_contract, PreparedSemanticDocuments, PreparedSemanticQuery, SemanticModelConfig,
    SemanticModelContract, SharedSemanticRuntime,
};

/// Produces vectors in one declared semantic compatibility space.
///
/// This is an internal execution interface, not a user-selectable plugin or an
/// admission/security boundary. Product composition must choose a trusted
/// implementation. A future implementation may use another process or host,
/// but its client is responsible for explicit privacy authorization,
/// authenticated contract negotiation, conformance checks, and fail-closed
/// routing before it reaches this interface. Local artifact acquisition and
/// backend selection remain implementation details of the built-in executor.
/// Inputs carry the fingerprint of the contract that prepared them.
pub trait SemanticEmbeddingExecutor: Send + Sync {
    fn contract(&self) -> &SemanticModelContract;

    fn embed_query(&self, query: PreparedSemanticQuery) -> Result<Vec<f32>>;

    /// Embeds one atomic document page.
    ///
    /// `pacing_deadline` bounds cooperative quiet time between internal
    /// batches. It does not cancel inference or permit a partial page result.
    fn embed_documents(
        &self,
        documents: PreparedSemanticDocuments,
        pacing_deadline: Option<Instant>,
    ) -> Result<Vec<Vec<f32>>>;
}

/// The default local semantic embedding executor shipped with ctx.
///
/// Clones share one loaded model runtime while retaining owned configuration
/// and vector-space contract values.
#[derive(Clone)]
pub struct BuiltinSemanticEmbeddingExecutor {
    runtime: SharedSemanticRuntime,
    config: SemanticModelConfig,
    contract: SemanticModelContract,
}

impl BuiltinSemanticEmbeddingExecutor {
    pub fn new(runtime: SharedSemanticRuntime, config: SemanticModelConfig) -> Self {
        Self {
            runtime,
            config,
            contract: semantic_model_contract().clone(),
        }
    }

    pub fn contract(&self) -> &SemanticModelContract {
        &self.contract
    }

    /// Provides the local lifecycle, acquisition, and status surface used by
    /// the built-in executor without adding those operations to the portable
    /// inference trait.
    pub fn shared_runtime(&self) -> &SharedSemanticRuntime {
        &self.runtime
    }

    pub fn config(&self) -> &SemanticModelConfig {
        &self.config
    }
}

impl SemanticEmbeddingExecutor for BuiltinSemanticEmbeddingExecutor {
    fn contract(&self) -> &SemanticModelContract {
        self.contract()
    }

    fn embed_query(&self, query: PreparedSemanticQuery) -> Result<Vec<f32>> {
        ensure_prepared_contract(query.contract_fingerprint(), self.contract())?;
        #[cfg(ctx_semantic_fastembed)]
        {
            self.runtime
                .embed_query(&self.config, query)
                .map(|(embedding, _runtime)| embedding)
        }
        #[cfg(not(ctx_semantic_fastembed))]
        {
            let _ = query;
            Err(anyhow!(
                "semantic embedding model {SEMANTIC_MODEL_ID} is not supported on this platform"
            ))
        }
    }

    fn embed_documents(
        &self,
        documents: PreparedSemanticDocuments,
        pacing_deadline: Option<Instant>,
    ) -> Result<Vec<Vec<f32>>> {
        ensure_prepared_contract(documents.contract_fingerprint(), self.contract())?;
        #[cfg(ctx_semantic_fastembed)]
        {
            self.runtime
                .embed_documents(&self.config, documents, pacing_deadline)
                .map(|(embeddings, _quiet_policy)| embeddings)
        }
        #[cfg(not(ctx_semantic_fastembed))]
        {
            let _ = (documents, pacing_deadline);
            Err(anyhow!(
                "semantic embedding model {SEMANTIC_MODEL_ID} is not supported on this platform"
            ))
        }
    }
}

fn ensure_prepared_contract(
    prepared_fingerprint: &str,
    contract: &SemanticModelContract,
) -> Result<()> {
    if prepared_fingerprint != contract.fingerprint() {
        return Err(anyhow::anyhow!(
            "semantic input was prepared for a different model contract"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::Mutex,
        time::{Duration, Instant},
    };

    use super::*;
    use crate::{
        SemanticModelPaths, SemanticOnnxRuntimePaths, SEMANTIC_DIMENSIONS, SEMANTIC_MODEL_KEY,
    };

    #[derive(Debug, PartialEq)]
    enum TestCall {
        Query(String),
        Documents(Vec<String>, Option<Instant>),
    }

    struct TestExecutor {
        contract: SemanticModelContract,
        calls: Mutex<Vec<TestCall>>,
    }

    impl SemanticEmbeddingExecutor for TestExecutor {
        fn contract(&self) -> &SemanticModelContract {
            &self.contract
        }

        fn embed_query(&self, query: PreparedSemanticQuery) -> Result<Vec<f32>> {
            self.calls
                .lock()
                .unwrap()
                .push(TestCall::Query(query.into_text()));
            let mut embedding = vec![0.0; self.contract.dimensions()];
            embedding[0] = 1.0;
            Ok(embedding)
        }

        fn embed_documents(
            &self,
            documents: PreparedSemanticDocuments,
            pacing_deadline: Option<Instant>,
        ) -> Result<Vec<Vec<f32>>> {
            let documents = documents.into_texts();
            self.calls
                .lock()
                .unwrap()
                .push(TestCall::Documents(documents.clone(), pacing_deadline));
            Ok(documents
                .into_iter()
                .map(|_| {
                    let mut embedding = vec![0.0; self.contract.dimensions()];
                    embedding[1] = 1.0;
                    embedding
                })
                .collect())
        }
    }

    #[test]
    fn trait_dispatch_returns_only_contract_vectors_and_propagates_pacing_deadline() {
        let test_executor = TestExecutor {
            contract: semantic_model_contract().clone(),
            calls: Mutex::new(Vec::new()),
        };
        let executor: &dyn SemanticEmbeddingExecutor = &test_executor;
        fn assert_send_sync<T: Send + Sync + ?Sized>() {}
        assert_send_sync::<dyn SemanticEmbeddingExecutor>();

        let deadline = Instant::now() + Duration::from_secs(1);

        assert_eq!(executor.contract().model_key(), SEMANTIC_MODEL_KEY);
        assert_eq!(
            executor
                .embed_query(executor.contract().prepare_query("needle".to_owned()))
                .unwrap()
                .len(),
            SEMANTIC_DIMENSIONS
        );
        assert_eq!(
            executor
                .embed_documents(
                    executor
                        .contract()
                        .prepare_documents(vec!["one".to_owned(), "two".to_owned()]),
                    Some(deadline),
                )
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            *test_executor.calls.lock().unwrap(),
            [
                TestCall::Query("query: needle".to_owned()),
                TestCall::Documents(
                    vec!["passage: one".to_owned(), "passage: two".to_owned()],
                    Some(deadline),
                ),
            ]
        );
    }

    #[test]
    fn builtin_executor_seam_owns_config_and_contract_without_loading_assets() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<BuiltinSemanticEmbeddingExecutor>();

        let runtime = SharedSemanticRuntime::default();
        let model_cache_dir = PathBuf::from("test-model-cache");
        let runtime_cache_dir = PathBuf::from("test-runtime-cache");
        let config = SemanticModelConfig::new(SemanticModelPaths::new(
            model_cache_dir.clone(),
            SemanticOnnxRuntimePaths::new(runtime_cache_dir),
        ));
        let executor = BuiltinSemanticEmbeddingExecutor::new(runtime, config);
        let trait_object: &dyn SemanticEmbeddingExecutor = &executor;
        let cloned = executor.clone();

        assert_eq!(trait_object.contract(), semantic_model_contract());
        assert_eq!(executor.config().paths().model_cache_dir(), model_cache_dir);
        assert!(!executor.shared_runtime().is_loaded());
        assert_eq!(cloned.contract(), executor.contract());
        let _busy = executor.shared_runtime().lock_for_test().unwrap();
        assert!(!cloned.shared_runtime().release_if_idle().unwrap());
    }

    #[test]
    fn builtin_executor_rejects_input_prepared_by_another_contract() {
        let runtime = SharedSemanticRuntime::default();
        let config = SemanticModelConfig::new(SemanticModelPaths::new(
            PathBuf::from("test-model-cache"),
            SemanticOnnxRuntimePaths::new(PathBuf::from("test-runtime-cache")),
        ));
        let executor = BuiltinSemanticEmbeddingExecutor::new(runtime, config);
        let other_contract = semantic_model_contract()
            .clone()
            .with_test_language_scope("test-only-incompatible-language-scope");
        let error = executor
            .embed_query(other_contract.prepare_query("needle".to_owned()))
            .expect_err("cross-contract prepared input must fail closed");

        assert_eq!(
            error.to_string(),
            "semantic input was prepared for a different model contract"
        );
    }
}
