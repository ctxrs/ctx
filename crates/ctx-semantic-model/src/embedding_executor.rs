use std::time::Instant;

#[cfg(not(ctx_semantic_fastembed))]
use anyhow::anyhow;
use anyhow::Result;

#[cfg(not(ctx_semantic_fastembed))]
use crate::SEMANTIC_MODEL_ID;
use crate::{
    http_embedding_executor::ValidatedHttpEndpoint, semantic_model_contract,
    HttpSemanticEmbeddingExecutor, PreparedSemanticDocuments, PreparedSemanticQuery,
    SemanticEmbeddingExecutorAuth, SemanticModelConfig, SemanticModelContract,
    SharedSemanticRuntime,
};

/// The selected implementation of the pinned semantic embedding contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticEmbeddingExecutorKind {
    Builtin,
    Http,
}

impl SemanticEmbeddingExecutorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Http => "http",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticEmbeddingExecutorScope {
    Builtin,
    Loopback,
    Remote,
}

impl SemanticEmbeddingExecutorScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Loopback => "loopback",
            Self::Remote => "remote",
        }
    }

    pub const fn content_leaves_machine(self) -> bool {
        matches!(self, Self::Remote)
    }
}

/// Validated product-composition selection for semantic embedding execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticEmbeddingExecutorConfig {
    selection: SemanticEmbeddingExecutorSelection,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SemanticEmbeddingExecutorSelection {
    Builtin,
    Http(ValidatedHttpEndpoint),
}

impl Default for SemanticEmbeddingExecutorConfig {
    fn default() -> Self {
        Self::builtin()
    }
}

impl SemanticEmbeddingExecutorConfig {
    pub const fn builtin() -> Self {
        Self {
            selection: SemanticEmbeddingExecutorSelection::Builtin,
        }
    }

    /// Selects an exact-contract HTTP endpoint after validating its URL policy.
    pub fn http(endpoint: impl AsRef<str>) -> Result<Self> {
        Ok(Self {
            selection: SemanticEmbeddingExecutorSelection::Http(ValidatedHttpEndpoint::parse(
                endpoint.as_ref(),
            )?),
        })
    }

    pub const fn kind(&self) -> SemanticEmbeddingExecutorKind {
        match &self.selection {
            SemanticEmbeddingExecutorSelection::Builtin => SemanticEmbeddingExecutorKind::Builtin,
            SemanticEmbeddingExecutorSelection::Http(_) => SemanticEmbeddingExecutorKind::Http,
        }
    }

    pub fn http_endpoint(&self) -> Option<&str> {
        match &self.selection {
            SemanticEmbeddingExecutorSelection::Builtin => None,
            SemanticEmbeddingExecutorSelection::Http(endpoint) => Some(endpoint.as_str()),
        }
    }

    pub fn endpoint(&self) -> Option<&str> {
        self.http_endpoint()
    }

    pub const fn is_builtin(&self) -> bool {
        matches!(&self.selection, SemanticEmbeddingExecutorSelection::Builtin)
    }

    pub const fn scope(&self) -> SemanticEmbeddingExecutorScope {
        match &self.selection {
            SemanticEmbeddingExecutorSelection::Builtin => SemanticEmbeddingExecutorScope::Builtin,
            SemanticEmbeddingExecutorSelection::Http(endpoint) if endpoint.is_loopback() => {
                SemanticEmbeddingExecutorScope::Loopback
            }
            SemanticEmbeddingExecutorSelection::Http(_) => SemanticEmbeddingExecutorScope::Remote,
        }
    }
}

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

/// Owns the selected trusted executor for product composition.
pub struct SemanticEmbeddingExecutorHandle {
    executor: SemanticEmbeddingExecutorHandleInner,
}

enum SemanticEmbeddingExecutorHandleInner {
    Builtin(BuiltinSemanticEmbeddingExecutor),
    Http(HttpSemanticEmbeddingExecutor),
}

impl std::fmt::Debug for SemanticEmbeddingExecutorHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SemanticEmbeddingExecutorHandle")
            .field("kind", &self.kind())
            .finish()
    }
}

impl SemanticEmbeddingExecutorHandle {
    /// Builds exactly the configured executor. HTTP construction never loads
    /// or falls back to the built-in runtime.
    pub fn build(
        config: SemanticEmbeddingExecutorConfig,
        runtime: SharedSemanticRuntime,
        model_config: SemanticModelConfig,
    ) -> Result<Self> {
        Self::build_with_auth(
            config,
            SemanticEmbeddingExecutorAuth::none(),
            runtime,
            model_config,
        )
    }

    pub fn build_with_auth(
        config: SemanticEmbeddingExecutorConfig,
        auth: SemanticEmbeddingExecutorAuth,
        runtime: SharedSemanticRuntime,
        model_config: SemanticModelConfig,
    ) -> Result<Self> {
        let executor = match config.selection {
            SemanticEmbeddingExecutorSelection::Builtin => {
                SemanticEmbeddingExecutorHandleInner::Builtin(
                    BuiltinSemanticEmbeddingExecutor::new(runtime, model_config),
                )
            }
            SemanticEmbeddingExecutorSelection::Http(endpoint) => {
                SemanticEmbeddingExecutorHandleInner::Http(
                    HttpSemanticEmbeddingExecutor::from_validated_endpoint(endpoint, auth)?,
                )
            }
        };
        Ok(Self { executor })
    }

    pub fn executor(&self) -> &dyn SemanticEmbeddingExecutor {
        match &self.executor {
            SemanticEmbeddingExecutorHandleInner::Builtin(executor) => executor,
            SemanticEmbeddingExecutorHandleInner::Http(executor) => executor,
        }
    }

    pub fn builtin_executor(&self) -> Option<&BuiltinSemanticEmbeddingExecutor> {
        match &self.executor {
            SemanticEmbeddingExecutorHandleInner::Builtin(executor) => Some(executor),
            SemanticEmbeddingExecutorHandleInner::Http(_) => None,
        }
    }

    pub fn http_executor(&self) -> Option<&HttpSemanticEmbeddingExecutor> {
        match &self.executor {
            SemanticEmbeddingExecutorHandleInner::Builtin(_) => None,
            SemanticEmbeddingExecutorHandleInner::Http(executor) => Some(executor),
        }
    }

    pub const fn kind(&self) -> SemanticEmbeddingExecutorKind {
        match &self.executor {
            SemanticEmbeddingExecutorHandleInner::Builtin(_) => {
                SemanticEmbeddingExecutorKind::Builtin
            }
            SemanticEmbeddingExecutorHandleInner::Http(_) => SemanticEmbeddingExecutorKind::Http,
        }
    }

    pub fn endpoint(&self) -> Option<&str> {
        self.http_executor()
            .map(HttpSemanticEmbeddingExecutor::endpoint)
    }

    pub const fn is_builtin(&self) -> bool {
        matches!(
            &self.executor,
            SemanticEmbeddingExecutorHandleInner::Builtin(_)
        )
    }
}

pub(super) fn ensure_prepared_contract(
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
