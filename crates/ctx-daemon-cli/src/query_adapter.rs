use std::{path::Path, time::Duration as StdDuration, time::Instant};

use anyhow::{anyhow, Result};
use ctx_history_index::{CompiledSearchFilter, EventSearchCandidate, VerifiedIndex};
use ctx_history_read_application::{
    HistorySemanticBatch, HistorySemanticError, HistorySemanticPort, HistorySemanticQuery,
    SemanticReason,
};
use ctx_semantic_index::{
    source_backed_semantic_vector_path, SemanticBatchEmbedder, SemanticChunkDocument,
    SemanticNotReady, SemanticQueryPin, SemanticVectorStore, SourceBackedSemanticDocumentBuilder,
};
use ctx_semantic_model::{
    semantic_model_key, SemanticModelConfig, SharedSemanticRuntime, SEMANTIC_DIMENSIONS,
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
        runtime: SharedSemanticRuntime,
        model_config: Box<SemanticModelConfig>,
    },
}

impl<'data_root> SemanticQueryAdapter<'data_root> {
    pub fn new(data_root: &'data_root Path) -> Self {
        Self {
            data_root,
            execution: SemanticQueryExecution::Daemon,
        }
    }

    /// Uses one foreground runtime for semantic reconciliation and the query
    /// embedding. Intended for explicit manual `--refresh wait`.
    pub fn foreground(data_root: &'data_root Path) -> Self {
        Self {
            data_root,
            execution: SemanticQueryExecution::Foreground {
                runtime: SharedSemanticRuntime::default(),
                model_config: Box::new(crate::model_config::semantic_model_config(data_root)),
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
            SemanticQueryExecution::Foreground {
                runtime,
                model_config,
            } => {
                reconcile_foreground_semantic(index, self.data_root, runtime, model_config)
                    .map_err(SemanticQueryError::from)?;
                SemanticQuerySession::begin_foreground(index, self.data_root, runtime, model_config)
            }
        }
        .map_err(HistorySemanticError::from)
    }
}

struct ForegroundSemanticEmbedder<'a> {
    runtime: &'a SharedSemanticRuntime,
    model_config: &'a SemanticModelConfig,
}

impl SemanticBatchEmbedder for ForegroundSemanticEmbedder<'_> {
    fn embed_chunks(&mut self, chunks: &[SemanticChunkDocument]) -> Result<Vec<Vec<f32>>> {
        ensure_foreground_runtime(self.runtime, self.model_config)?;
        let texts = chunks
            .iter()
            .map(|chunk| chunk.text().to_owned())
            .collect::<Vec<_>>();
        self.runtime
            .embed_documents(self.model_config, texts, None)
            .map(|(embeddings, _)| embeddings)
    }
}

fn reconcile_foreground_semantic(
    index: &VerifiedIndex,
    data_root: &Path,
    runtime: &SharedSemanticRuntime,
    model_config: &SemanticModelConfig,
) -> Result<()> {
    if index.semantic_eligible_event_count()? > 0 {
        ensure_foreground_runtime(runtime, model_config)?;
    }
    let mut store = SemanticVectorStore::open(&source_backed_semantic_vector_path(data_root))?;
    let mut builder = SourceBackedSemanticDocumentBuilder::new(index);
    let mut embedder = ForegroundSemanticEmbedder {
        runtime,
        model_config,
    };
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
    embedding_source: SemanticQueryEmbeddingSource<'a>,
    embeddings: Vec<Vec<f32>>,
}

#[derive(Clone, Copy)]
enum SemanticQueryEmbeddingSource<'a> {
    Daemon,
    Foreground {
        runtime: &'a SharedSemanticRuntime,
        model_config: &'a SemanticModelConfig,
    },
}

impl SemanticQuerySession<'_> {
    fn begin<'a>(
        index: &'a VerifiedIndex,
        data_root: &'a Path,
    ) -> std::result::Result<SemanticQuerySession<'a>, SemanticQueryError> {
        let pin =
            SemanticQueryPin::preflight(index, data_root).map_err(SemanticQueryError::from)?;
        Ok(SemanticQuerySession {
            pin,
            index,
            data_root,
            embedding_source: SemanticQueryEmbeddingSource::Daemon,
            embeddings: Vec::new(),
        })
    }

    fn begin_foreground<'a>(
        index: &'a VerifiedIndex,
        data_root: &'a Path,
        runtime: &'a SharedSemanticRuntime,
        model_config: &'a SemanticModelConfig,
    ) -> std::result::Result<SemanticQuerySession<'a>, SemanticQueryError> {
        let mut session = Self::begin(index, data_root)?;
        session.embedding_source = SemanticQueryEmbeddingSource::Foreground {
            runtime,
            model_config,
        };
        Ok(session)
    }

    fn prepare_alternative(
        &mut self,
        query: &str,
    ) -> std::result::Result<Value, SemanticQueryError> {
        match self.embedding_source {
            SemanticQueryEmbeddingSource::Daemon => {
                self.prepare_alternative_with(query, daemon_query_embedding)
            }
            SemanticQueryEmbeddingSource::Foreground {
                runtime,
                model_config,
            } => self.prepare_alternative_with(query, |_, query| {
                foreground_query_embedding(runtime, model_config, query).map(Some)
            }),
        }
    }

    fn prepare_alternative_with<EmbedQuery>(
        &mut self,
        query: &str,
        mut embed_query: EmbedQuery,
    ) -> std::result::Result<Value, SemanticQueryError>
    where
        EmbedQuery: FnMut(&Path, &str) -> Result<Option<(Vec<f32>, u64)>>,
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
        let (embedding, query_embed_ms) = embed_query(self.data_root, query)
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
            embedding_source: SemanticQueryEmbeddingSource::Daemon,
            embeddings: Vec::new(),
        }
    }
}

fn foreground_query_embedding(
    runtime: &SharedSemanticRuntime,
    model_config: &SemanticModelConfig,
    semantic_text: &str,
) -> Result<(Vec<f32>, u64)> {
    ensure_foreground_runtime(runtime, model_config)?;
    let started = Instant::now();
    let (embedding, _) = runtime.embed_query(model_config, semantic_text.to_owned())?;
    Ok((embedding, started.elapsed().as_millis() as u64))
}

fn ensure_foreground_runtime(
    runtime: &SharedSemanticRuntime,
    model_config: &SemanticModelConfig,
) -> Result<()> {
    runtime.ensure_loaded_with_acquisition(
        model_config,
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
            Err(error) => Self::failed(format!("{error:#}")),
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
    semantic_text: &str,
) -> Result<Option<(Vec<f32>, u64)>> {
    let Some(response) = daemon_query_request(
        data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": "embed_query",
            "model_key": semantic_model_key(),
            "text": semantic_text,
        })),
        StdDuration::from_secs(30),
        1024 * 1024,
    )?
    else {
        return Ok(None);
    };
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        let message = response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("daemon query failed");
        return Err(anyhow!("{message}"));
    }
    let model_key = response
        .get("model_key")
        .and_then(Value::as_str)
        .unwrap_or("");
    if model_key != semantic_model_key() {
        return Err(anyhow!("daemon query response model key mismatch"));
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
    if embedding.len() != SEMANTIC_DIMENSIONS {
        return Err(anyhow!(
            "daemon query embedding returned {} dimensions, expected {}",
            embedding.len(),
            SEMANTIC_DIMENSIONS
        ));
    }
    Ok(Some((embedding, query_embed_ms)))
}

#[cfg(test)]
mod tests;
