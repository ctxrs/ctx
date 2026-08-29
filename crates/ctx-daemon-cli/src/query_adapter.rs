use std::{path::Path, time::Duration as StdDuration, time::Instant};

use anyhow::{anyhow, Result};
use ctx_daemon_service::DaemonQueryServiceUnavailable;
use ctx_history_index::{CompiledSearchFilter, EventSearchCandidate, VerifiedIndex};
use ctx_history_read_application::{
    HistorySemanticBatch, HistorySemanticError, HistorySemanticPort, HistorySemanticQuery,
    SemanticReason,
};
use ctx_semantic_index::{
    semantic_model_contract, source_backed_semantic_vector_path, SemanticBatchEmbedder,
    SemanticChunkDocument, SemanticModelContract, SemanticNotReady, SemanticQueryPin,
    SemanticVectorStore, SourceBackedSemanticDocumentBuilder,
};
use ctx_semantic_model::{
    BuiltinSemanticEmbeddingExecutor, SemanticEmbeddingExecutor, SharedSemanticRuntime,
};
use serde_json::{json, Value};

use crate::compact_json;

use super::query_service::daemon_query_request;

pub struct SemanticQueryAdapter<'data_root> {
    data_root: &'data_root Path,
    execution: SemanticQueryExecution,
}

enum SemanticQueryExecution {
    Daemon,
    Foreground {
        executor: Box<BuiltinSemanticEmbeddingExecutor>,
        mode: ForegroundSemanticMode,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ForegroundSemanticMode {
    ReadOnly,
    Reconcile,
}

impl<'data_root> SemanticQueryAdapter<'data_root> {
    pub fn new(data_root: &'data_root Path) -> Self {
        Self {
            data_root,
            execution: SemanticQueryExecution::Daemon,
        }
    }

    /// Uses one foreground built-in executor for semantic reconciliation and
    /// the query embedding. Intended for explicit manual `--refresh wait`.
    pub fn foreground(data_root: &'data_root Path) -> Self {
        Self::foreground_with_mode(data_root, ForegroundSemanticMode::Reconcile)
    }

    /// Uses a foreground built-in executor only to query a ready semantic
    /// projection. Intended for daemon-free `--refresh off`.
    pub fn foreground_read_only(data_root: &'data_root Path) -> Self {
        Self::foreground_with_mode(data_root, ForegroundSemanticMode::ReadOnly)
    }

    fn foreground_with_mode(data_root: &'data_root Path, mode: ForegroundSemanticMode) -> Self {
        let executor = BuiltinSemanticEmbeddingExecutor::new(
            SharedSemanticRuntime::default(),
            crate::model_config::semantic_model_config(data_root),
        );
        Self {
            data_root,
            execution: SemanticQueryExecution::Foreground {
                executor: Box::new(executor),
                mode,
            },
        }
    }
}

impl HistorySemanticPort for SemanticQueryAdapter<'_> {
    type Query<'a>
        = SemanticQuerySession<'a>
    where
        Self: 'a;

    fn begin_query<'a>(
        &'a self,
        index: &'a VerifiedIndex,
    ) -> std::result::Result<Self::Query<'a>, HistorySemanticError> {
        match &self.execution {
            SemanticQueryExecution::Daemon => SemanticQuerySession::begin(index, self.data_root),
            SemanticQueryExecution::Foreground { executor, mode } => {
                match SemanticQuerySession::begin_foreground(index, self.data_root, executor, *mode)
                {
                    Ok(session) => Ok(session),
                    Err(SemanticQueryError::NotReady { .. })
                        if *mode == ForegroundSemanticMode::Reconcile =>
                    {
                        reconcile_foreground_semantic(index, self.data_root, executor)
                            .map_err(SemanticQueryError::from)?;
                        SemanticQuerySession::begin_foreground(
                            index,
                            self.data_root,
                            executor,
                            *mode,
                        )
                    }
                    Err(error) => Err(error),
                }
            }
        }
        .map_err(HistorySemanticError::from)
    }
}

struct ForegroundSemanticEmbedder<'a> {
    executor: &'a BuiltinSemanticEmbeddingExecutor,
}

impl SemanticBatchEmbedder for ForegroundSemanticEmbedder<'_> {
    fn embed_chunks(&mut self, chunks: &[SemanticChunkDocument]) -> Result<Vec<Vec<f32>>> {
        ensure_foreground_executor(self.executor)?;
        let texts = chunks
            .iter()
            .map(|chunk| chunk.text().to_owned())
            .collect::<Vec<_>>();
        let executor: &dyn SemanticEmbeddingExecutor = self.executor;
        executor.embed_documents(executor.contract().prepare_documents(texts), None)
    }
}

fn reconcile_foreground_semantic(
    index: &VerifiedIndex,
    data_root: &Path,
    executor: &BuiltinSemanticEmbeddingExecutor,
) -> Result<()> {
    if index.semantic_eligible_event_count()? > 0 {
        ensure_foreground_executor(executor)?;
    }
    let contract = semantic_index_contract(executor)?;
    let mut store =
        SemanticVectorStore::open(&source_backed_semantic_vector_path(data_root), &contract)?;
    let mut builder = SourceBackedSemanticDocumentBuilder::new(index);
    let mut embedder = ForegroundSemanticEmbedder { executor };
    loop {
        let outcome = store.reconcile_source_backed_index(index, &mut builder, &mut embedder)?;
        if outcome.ready() {
            return Ok(());
        }
        if !outcome.work_remaining() {
            return Err(anyhow!(
                "semantic reconciliation stopped before the pinned Core generation was ready"
            ));
        }
    }
}

pub struct SemanticQuerySession<'a> {
    pin: SemanticQueryPin,
    index: &'a VerifiedIndex,
    data_root: &'a Path,
    contract: SemanticModelContract,
    embedding_source: SemanticQueryEmbeddingSource<'a>,
    embeddings: Vec<Vec<f32>>,
}

#[derive(Clone, Copy)]
enum SemanticQueryEmbeddingSource<'a> {
    Daemon,
    Foreground {
        executor: &'a BuiltinSemanticEmbeddingExecutor,
        mode: ForegroundSemanticMode,
    },
}

impl SemanticQuerySession<'_> {
    fn begin<'a>(
        index: &'a VerifiedIndex,
        data_root: &'a Path,
    ) -> std::result::Result<SemanticQuerySession<'a>, SemanticQueryError> {
        let contract = semantic_model_contract();
        let pin = SemanticQueryPin::preflight(index, data_root, contract)
            .map_err(SemanticQueryError::from)?;
        Ok(SemanticQuerySession {
            pin,
            index,
            data_root,
            contract: contract.clone(),
            embedding_source: SemanticQueryEmbeddingSource::Daemon,
            embeddings: Vec::new(),
        })
    }

    fn begin_foreground<'a>(
        index: &'a VerifiedIndex,
        data_root: &'a Path,
        executor: &'a BuiltinSemanticEmbeddingExecutor,
        mode: ForegroundSemanticMode,
    ) -> std::result::Result<SemanticQuerySession<'a>, SemanticQueryError> {
        let contract = semantic_index_contract(executor).map_err(SemanticQueryError::from)?;
        let pin = SemanticQueryPin::preflight(index, data_root, &contract)
            .map_err(SemanticQueryError::from)?;
        Ok(SemanticQuerySession {
            pin,
            index,
            data_root,
            contract,
            embedding_source: SemanticQueryEmbeddingSource::Foreground { executor, mode },
            embeddings: Vec::new(),
        })
    }

    fn prepare_alternative(
        &mut self,
        query: &str,
    ) -> std::result::Result<Value, SemanticQueryError> {
        match self.embedding_source {
            SemanticQueryEmbeddingSource::Daemon => {
                self.prepare_alternative_with(query, daemon_query_embedding)
            }
            SemanticQueryEmbeddingSource::Foreground { executor, mode } => self
                .prepare_alternative_with(query, |_, _, query| {
                    foreground_query_embedding(executor, query, mode).map(Some)
                }),
        }
    }

    fn prepare_alternative_with<EmbedQuery>(
        &mut self,
        query: &str,
        mut embed_query: EmbedQuery,
    ) -> std::result::Result<Value, SemanticQueryError>
    where
        EmbedQuery: FnMut(&Path, &SemanticModelContract, &str) -> Result<Option<(Vec<f32>, u64)>>,
    {
        if !self
            .pin
            .requires_embedding(self.index)
            .map_err(SemanticQueryError::from)?
        {
            return Ok(compact_json(json!({
                "query_embed_ms": null,
            })));
        }
        let (embedding, query_embed_ms) = embed_query(self.data_root, &self.contract, query)
            .map_err(SemanticQueryError::from)?
            .ok_or_else(|| {
                SemanticQueryError::not_ready(
                    "semantic_query_service_unavailable",
                    "the daemon query embedding service is unavailable",
                    true,
                )
            })?;
        self.embeddings.push(embedding);
        Ok(compact_json(json!({
            "query_embed_ms": query_embed_ms,
        })))
    }

    fn search(
        &mut self,
        filter: &CompiledSearchFilter,
        candidate_limit: usize,
    ) -> std::result::Result<(Vec<EventSearchCandidate>, Value), SemanticQueryError> {
        self.pin
            .search(self.index, filter, &self.embeddings, candidate_limit)
            .map_err(SemanticQueryError::from)
    }

    #[cfg(test)]
    fn from_pin<'a>(
        index: &'a VerifiedIndex,
        data_root: &'a Path,
        pin: SemanticQueryPin,
    ) -> SemanticQuerySession<'a> {
        SemanticQuerySession {
            pin,
            index,
            data_root,
            contract: semantic_model_contract().clone(),
            embedding_source: SemanticQueryEmbeddingSource::Daemon,
            embeddings: Vec::new(),
        }
    }
}

fn semantic_index_contract(
    executor: &BuiltinSemanticEmbeddingExecutor,
) -> Result<SemanticModelContract> {
    // Bazel may materialize the model crate separately across this dependency
    // boundary. Compare the portable fingerprint, then return the index
    // crate's own contract type.
    let contract = semantic_model_contract();
    if executor.contract().fingerprint() != contract.fingerprint() {
        return Err(anyhow!(
            "semantic executor model contract does not match the semantic index contract"
        ));
    }
    Ok(contract.clone())
}

fn foreground_query_embedding(
    executor: &BuiltinSemanticEmbeddingExecutor,
    semantic_text: &str,
    mode: ForegroundSemanticMode,
) -> Result<(Vec<f32>, u64)> {
    match mode {
        ForegroundSemanticMode::ReadOnly => {
            executor
                .shared_runtime()
                .ensure_loaded_from_cache(executor.config())?;
        }
        ForegroundSemanticMode::Reconcile => ensure_foreground_executor(executor)?,
    }
    let started = Instant::now();
    let executor: &dyn SemanticEmbeddingExecutor = executor;
    let embedding =
        executor.embed_query(executor.contract().prepare_query(semantic_text.to_owned()))?;
    Ok((embedding, started.elapsed().as_millis() as u64))
}

fn ensure_foreground_executor(executor: &BuiltinSemanticEmbeddingExecutor) -> Result<()> {
    executor.shared_runtime().ensure_loaded_with_acquisition(
        executor.config(),
        &crate::daemon_service_ports::ARTIFACT_FETCHER,
    )?;
    Ok(())
}

impl HistorySemanticQuery for SemanticQuerySession<'_> {
    fn prepare_alternative(
        &mut self,
        query: &str,
    ) -> std::result::Result<Value, HistorySemanticError> {
        self.prepare_alternative(query)
            .map_err(HistorySemanticError::from)
    }

    fn candidates(
        &mut self,
        filter: &CompiledSearchFilter,
        candidate_limit: usize,
    ) -> std::result::Result<HistorySemanticBatch, HistorySemanticError> {
        self.search(filter, candidate_limit)
            .map(|(candidates, diagnostics)| HistorySemanticBatch {
                candidates,
                diagnostics,
            })
            .map_err(HistorySemanticError::from)
    }
}

#[derive(Debug, thiserror::Error)]
enum SemanticQueryError {
    #[error("source-backed semantic search is not ready ({code}): {detail}")]
    NotReady {
        code: &'static str,
        detail: String,
        retryable: bool,
    },
    #[error("{detail}")]
    Failed { detail: String },
}

impl SemanticQueryError {
    fn not_ready(code: &'static str, detail: impl Into<String>, retryable: bool) -> Self {
        Self::NotReady {
            code,
            detail: detail.into(),
            retryable,
        }
    }

    fn failed(detail: impl Into<String>) -> Self {
        Self::Failed {
            detail: detail.into(),
        }
    }
}

impl From<anyhow::Error> for SemanticQueryError {
    fn from(error: anyhow::Error) -> Self {
        match error.downcast::<SemanticNotReady>() {
            Ok(not_ready) => {
                Self::not_ready(not_ready.code(), not_ready.detail(), not_ready.retryable())
            }
            Err(error) => match error.downcast::<DaemonQueryServiceUnavailable>() {
                Ok(error) => Self::not_ready(
                    "semantic_query_service_unavailable",
                    error.to_string(),
                    true,
                ),
                Err(error) => Self::failed(format!("{error:#}")),
            },
        }
    }
}

impl From<SemanticQueryError> for HistorySemanticError {
    fn from(error: SemanticQueryError) -> Self {
        match error {
            SemanticQueryError::NotReady {
                code,
                detail,
                retryable,
            } => Self::not_ready(SemanticReason::from_adapter_code(code), detail, retryable),
            SemanticQueryError::Failed { detail } => Self::failed(detail),
        }
    }
}

fn daemon_query_embedding(
    data_root: &Path,
    contract: &SemanticModelContract,
    semantic_text: &str,
) -> Result<Option<(Vec<f32>, u64)>> {
    let Some(response) = daemon_query_request(
        data_root,
        daemon_query_embedding_request(contract, semantic_text),
        StdDuration::from_secs(30),
        1024 * 1024,
    )?
    else {
        return Ok(None);
    };
    parse_daemon_query_embedding_response(&response, contract).map(Some)
}

fn daemon_query_embedding_request(contract: &SemanticModelContract, semantic_text: &str) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "op": "embed_query",
        "model_key": contract.model_key(),
        "model_contract_fingerprint": contract.fingerprint(),
        "text": semantic_text,
    }))
}

fn parse_daemon_query_embedding_response(
    response: &Value,
    contract: &SemanticModelContract,
) -> Result<(Vec<f32>, u64)> {
    let ok = response.get("ok").and_then(Value::as_bool);
    let legacy_v1 = contract.supports_frozen_legacy_v1()
        && response.get("model_contract_fingerprint").is_none();
    if !legacy_v1 {
        if response.get("schema_version").and_then(Value::as_u64) != Some(1) {
            return Err(anyhow!("daemon query response schema_version mismatch"));
        }
        let model_key = response
            .get("model_key")
            .and_then(Value::as_str)
            .unwrap_or("");
        if model_key != contract.model_key() {
            return Err(anyhow!("daemon query response model key mismatch"));
        }
        let model_contract_fingerprint = response
            .get("model_contract_fingerprint")
            .and_then(Value::as_str)
            .unwrap_or("");
        if model_contract_fingerprint != contract.fingerprint() {
            return Err(anyhow!(
                "daemon query response model contract fingerprint mismatch"
            ));
        }
    } else if ok == Some(true)
        && (response
            .get("schema_version")
            .is_some_and(|value| value.as_u64() != Some(1))
            || response.get("model_key").and_then(Value::as_str) != Some(contract.model_key()))
    {
        return Err(anyhow!(
            "legacy daemon query response protocol identity mismatch"
        ));
    }
    if ok != Some(true) {
        let message = response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("daemon query failed");
        return Err(anyhow!("{message}"));
    }
    let query_embed_ms = response
        .get("query_embed_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let embedding = response
        .get("embedding")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("daemon query response missing embedding"))?
        .iter()
        .map(|value| {
            value
                .as_f64()
                .map(|value| value as f32)
                .ok_or_else(|| anyhow!("daemon query embedding contains a non-number"))
        })
        .collect::<Result<Vec<_>>>()?;
    if embedding.len() != contract.dimensions() {
        return Err(anyhow!(
            "daemon query embedding returned {} dimensions, expected {}",
            embedding.len(),
            contract.dimensions()
        ));
    }
    if embedding.iter().any(|value| !value.is_finite()) {
        return Err(anyhow!(
            "daemon query embedding contains a non-finite value"
        ));
    }
    let norm_squared = embedding.iter().fold(0.0_f64, |norm_squared, value| {
        f64::from(*value).mul_add(f64::from(*value), norm_squared)
    });
    const NORMALIZED_NORM_SQUARED_TOLERANCE: f64 = 1.0e-3;
    if !norm_squared.is_finite() || (norm_squared - 1.0).abs() > NORMALIZED_NORM_SQUARED_TOLERANCE {
        return Err(anyhow!(
            "daemon query embedding is not L2-normalized (norm squared {norm_squared})"
        ));
    }
    Ok((embedding, query_embed_ms))
}

#[cfg(test)]
mod tests;
