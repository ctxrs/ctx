use std::{path::Path, time::Duration as StdDuration};

use anyhow::{anyhow, Result};
use ctx_history_index::{EventSearchCandidate, EventSearchFilters, VerifiedIndex};
use ctx_semantic_index::{SemanticNotReady, SemanticQueryPin};
use ctx_semantic_model::{semantic_model_key, SEMANTIC_DIMENSIONS};
use serde_json::{json, Value};

use crate::compact_json;

use super::query_service::daemon_query_request;

#[derive(Default)]
pub(crate) struct SemanticQueryAdapter {
    pin: Option<SemanticQueryPin>,
}

impl SemanticQueryAdapter {
    pub(crate) fn search(
        &mut self,
        index: &VerifiedIndex,
        data_root: &Path,
        query: &str,
        filters: &EventSearchFilters,
        candidate_limit: usize,
    ) -> Result<(Vec<EventSearchCandidate>, Value)> {
        self.search_with(
            index,
            data_root,
            query,
            filters,
            candidate_limit,
            daemon_query_embedding,
        )
    }

    fn search_with<EmbedQuery>(
        &mut self,
        index: &VerifiedIndex,
        data_root: &Path,
        query: &str,
        filters: &EventSearchFilters,
        candidate_limit: usize,
        mut embed_query: EmbedQuery,
    ) -> Result<(Vec<EventSearchCandidate>, Value)>
    where
        EmbedQuery: FnMut(&Path, &str) -> Result<Option<(Vec<f32>, u64)>>,
    {
        if self.pin.is_none() {
            self.pin = Some(SemanticQueryPin::preflight(index, data_root)?);
        }
        let pin = self
            .pin
            .as_mut()
            .ok_or_else(|| anyhow!("source-backed semantic query pin is unavailable"))?;
        if !pin.requires_embedding(index)? {
            return pin.search(index, filters, &[], candidate_limit, None);
        }
        let (embedding, query_embed_ms) = embed_query(data_root, query)?.ok_or_else(|| {
            anyhow::Error::new(SemanticNotReady::new(
                "semantic_query_service_unavailable",
                "the daemon query embedding service is unavailable",
            ))
        })?;
        pin.search(
            index,
            filters,
            &embedding,
            candidate_limit,
            Some(query_embed_ms),
        )
    }

    #[cfg(test)]
    fn from_pin(pin: SemanticQueryPin) -> Self {
        Self { pin: Some(pin) }
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
