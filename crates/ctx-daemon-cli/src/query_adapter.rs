use std::{
    path::Path,
    time::{Duration as StdDuration, Instant},
};

use anyhow::{anyhow, Result};
use ctx_history_index::{EventSearchCandidate, EventSearchFilters, VerifiedIndex};
use ctx_history_read_application::{
    HistorySemanticBatch, HistorySemanticError, HistorySemanticPort, HistorySemanticQuery,
    SemanticReason,
};
use ctx_semantic_index::{SemanticNotReady, SemanticQueryPin};
use ctx_semantic_model::{semantic_model_key, SharedSemanticRuntime, SEMANTIC_DIMENSIONS};
use serde_json::{json, Value};

use crate::compact_json;

use super::query_service::daemon_query_request;

#[derive(Debug, Clone, Copy)]
pub struct SemanticQueryAdapter<'data_root> {
    data_root: &'data_root Path,
    wait_for_ready: bool,
}

impl<'data_root> SemanticQueryAdapter<'data_root> {
    pub fn new(data_root: &'data_root Path) -> Self {
        Self {
            data_root,
            wait_for_ready: false,
        }
    }

    pub fn for_wait_refresh(data_root: &'data_root Path) -> Self {
        Self {
            data_root,
            wait_for_ready: true,
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
        if self.wait_for_ready {
            SemanticQuerySession::begin_waiting(index, self.data_root)
                .map_err(HistorySemanticError::from)
        } else {
            SemanticQuerySession::begin(index, self.data_root).map_err(HistorySemanticError::from)
        }
    }
}

pub struct SemanticQuerySession<'a> {
    pin: SemanticQueryPin,
    index: &'a VerifiedIndex,
    data_root: &'a Path,
}

impl SemanticQuerySession<'_> {
    const WAIT_REFRESH_TIMEOUT: StdDuration = StdDuration::from_secs(30 * 60);

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
        })
    }

    fn begin_waiting<'a>(
        index: &'a VerifiedIndex,
        data_root: &'a Path,
    ) -> std::result::Result<SemanticQuerySession<'a>, SemanticQueryError> {
        let started = Instant::now();
        loop {
            match Self::begin(index, data_root) {
                Ok(session) => return Ok(session),
                Err(
                    error @ SemanticQueryError::NotReady {
                        retryable: true, ..
                    },
                ) => {
                    if started.elapsed() >= Self::WAIT_REFRESH_TIMEOUT
                        || semantic_worker_finished_without_ready_generation(data_root)
                    {
                        return Err(error);
                    }
                    std::thread::sleep(StdDuration::from_millis(100));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn search(
        &mut self,
        query: &str,
        filters: &EventSearchFilters,
        candidate_limit: usize,
    ) -> std::result::Result<(Vec<EventSearchCandidate>, Value), SemanticQueryError> {
        self.search_with(query, filters, candidate_limit, daemon_query_embedding)
    }

    fn search_with<EmbedQuery>(
        &mut self,
        query: &str,
        filters: &EventSearchFilters,
        candidate_limit: usize,
        mut embed_query: EmbedQuery,
    ) -> std::result::Result<(Vec<EventSearchCandidate>, Value), SemanticQueryError>
    where
        EmbedQuery: FnMut(&Path, &str) -> Result<Option<(Vec<f32>, u64)>>,
    {
        if !self
            .pin
            .requires_embedding(self.index)
            .map_err(SemanticQueryError::from)?
        {
            return self
                .pin
                .search(self.index, filters, &[], candidate_limit, None)
                .map_err(SemanticQueryError::from);
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
        self.pin
            .search(
                self.index,
                filters,
                &embedding,
                candidate_limit,
                Some(query_embed_ms),
            )
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
        }
    }
}

fn semantic_worker_finished_without_ready_generation(data_root: &Path) -> bool {
    ctx_daemon_runtime::read_daemon_status(data_root).is_some_and(|status| {
        matches!(
            status.get("status").and_then(Value::as_str),
            Some("disabled" | "failed" | "stopped")
        )
    })
}

impl HistorySemanticQuery for SemanticQuerySession<'_> {
    fn candidates(
        &mut self,
        query: &str,
        filters: &EventSearchFilters,
        candidate_limit: usize,
    ) -> std::result::Result<HistorySemanticBatch, HistorySemanticError> {
        self.search(query, filters, candidate_limit)
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
    let response = daemon_query_request(
        data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": "embed_query",
            "model_key": semantic_model_key(),
            "text": semantic_text,
        })),
        StdDuration::from_secs(30),
        1024 * 1024,
    )?;
    let Some(response) = response else {
        return local_query_embedding(data_root, semantic_text).map(Some);
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

fn local_query_embedding(data_root: &Path, semantic_text: &str) -> Result<(Vec<f32>, u64)> {
    let config = super::model_config::semantic_model_config(data_root);
    let runtime = SharedSemanticRuntime::default();
    runtime
        .ensure_loaded_from_cache(&config)?
        .ok_or_else(|| anyhow!("semantic query model was not loaded from cache"))?;
    let started = Instant::now();
    let (embedding, _) = runtime.embed_query(&config, semantic_text.to_owned())?;
    Ok((embedding, started.elapsed().as_millis() as u64))
}

#[cfg(test)]
mod tests;
