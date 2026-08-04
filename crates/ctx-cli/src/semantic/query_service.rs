use std::{
    path::Path,
    time::{Duration as StdDuration, Instant},
};

use anyhow::{anyhow, Result};
use ctx_history_index::{
    EventSearchCandidate, EventSearchFilters, SemanticFilterProjection, VerifiedIndex,
};
use serde_json::{json, Value};
use thiserror::Error;

use crate::compact_json;

use super::{
    health_search::daemon_query_embedding,
    runtime_limits::SEMANTIC_EXACT_TOP_K_MAX,
    source_backed_refresh_coordinator::PinnedSourceBackedGeneration,
    vector_store::{
        flat_segments::PinnedFlatGeneration, source_backed_semantic_vector_path,
        SemanticVectorSearchStats, SemanticVectorStore, SourceBackedGenerationPin,
    },
    vector_store_search::scan_exact_generation,
};
mod transport;
#[cfg(test)]
pub(in crate::semantic) use transport::*;
#[cfg(not(test))]
pub(in crate::semantic) use transport::{
    daemon_query_request, daemon_service_endpoint_path, daemon_source_refresh_request,
    read_daemon_service_endpoint_identity, DaemonIpcService, DaemonQueryEndpoint,
    DaemonSourceRefreshServiceUnavailable,
};
mod server;
#[cfg(test)]
pub(in crate::semantic) use server::*;
#[cfg(not(test))]
pub(in crate::semantic) use server::{
    daemon_can_begin_idle_shutdown, observe_daemon_query_activity, start_daemon_query_service,
    start_daemon_source_refresh_service, start_daemon_source_refresh_service_with_coordinator,
    DaemonQueryActivity, DaemonQueryService,
};

const MAX_SEMANTIC_CORE_BATCH_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Error)]
#[error("source-backed semantic search is not ready ({code}): {detail}")]
pub(crate) struct SourceBackedSemanticNotReady {
    code: &'static str,
    detail: String,
}

impl SourceBackedSemanticNotReady {
    pub(crate) fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }

    pub(crate) fn code(&self) -> &'static str {
        self.code
    }

    pub(crate) fn detail(&self) -> &str {
        &self.detail
    }

    pub(crate) fn retryable(&self) -> bool {
        matches!(
            self.code,
            "semantic_store_unavailable"
                | "semantic_store_missing"
                | "semantic_generation_unreadable"
                | "semantic_generation_not_acknowledged"
                | "semantic_query_service_unavailable"
                | "semantic_projection_event_mismatch"
                | "semantic_generation_receipt_mismatch"
        )
    }

    pub(crate) fn structured(&self) -> Value {
        json!({
            "error": self.to_string(),
            "error_code": self.code,
            "detail": self.detail,
            "retryable": self.retryable(),
        })
    }
}

pub(crate) struct SourceBackedSemanticQueryPin {
    core_generation_id: String,
    pinned: Option<PinnedFlatGeneration>,
    filter_projection: Option<(EventSearchFilters, SemanticFilterProjection)>,
}

impl PinnedSourceBackedGeneration {
    pub(crate) fn pin_semantic_query_for_source_generation(
        index: &VerifiedIndex,
        data_root: &Path,
    ) -> Result<SourceBackedSemanticQueryPin> {
        let vector_root = source_backed_semantic_vector_path(data_root);
        let vector_store = SemanticVectorStore::open_read_only(&vector_root)
            .map_err(|error| {
                source_semantic_not_ready("semantic_store_unavailable", format!("{error:#}"))
            })?
            .ok_or_else(|| {
                source_semantic_not_ready(
                    "semantic_store_missing",
                    "the fresh flat-F32 semantic projection does not exist",
                )
            })?;
        let semantic_documents = index.semantic_eligible_event_count().map_err(|error| {
            source_semantic_not_ready(
                "semantic_generation_unreadable",
                format!("semantic-eligible event count failed: {error}"),
            )
        })?;
        let readiness = vector_store
            .source_backed_generation_pin_exact(index.generation_id(), semantic_documents)
            .map_err(|error| {
                source_semantic_not_ready(
                    "semantic_generation_unreadable",
                    format!("semantic source acknowledgement could not be verified: {error:#}"),
                )
            })?;
        source_backed_query_pin_from_readiness(index.generation_id(), readiness)
    }

    pub(crate) fn semantic_candidates_for_pinned_source_generation(
        index: &VerifiedIndex,
        data_root: &Path,
        query: &str,
        filters: &EventSearchFilters,
        candidate_limit: usize,
        pin: &mut SourceBackedSemanticQueryPin,
    ) -> Result<(Vec<EventSearchCandidate>, Value)> {
        validate_semantic_query_generation(index.generation_id(), pin)?;
        let Some(pinned) = pin.pinned.as_ref() else {
            return Ok((
                Vec::new(),
                source_semantic_diagnostics(
                    index,
                    None,
                    None,
                    candidate_limit,
                    candidate_limit,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    None,
                ),
            ));
        };
        let (embedding, query_embed_ms) =
            daemon_query_embedding(data_root, query)?.ok_or_else(|| {
                source_semantic_not_ready(
                    "semantic_query_service_unavailable",
                    "the daemon query embedding service is unavailable",
                )
            })?;
        if pin
            .filter_projection
            .as_ref()
            .is_none_or(|(cached_filters, _)| cached_filters != filters)
        {
            pin.filter_projection =
                Some((filters.clone(), index.semantic_filter_projection(filters)?));
        }
        let projection = &pin
            .filter_projection
            .as_ref()
            .ok_or_else(|| anyhow!("semantic filter projection is unavailable"))?
            .1;
        source_semantic_candidates_with_embedding(
            index,
            pinned,
            projection,
            candidate_limit,
            &embedding,
            Some(query_embed_ms),
        )
    }
}

fn source_semantic_candidates_with_embedding(
    index: &VerifiedIndex,
    pinned: &PinnedFlatGeneration,
    projection: &SemanticFilterProjection,
    candidate_limit: usize,
    embedding: &[f32],
    query_embed_ms: Option<u64>,
) -> Result<(Vec<EventSearchCandidate>, Value)> {
    if candidate_limit == 0 || candidate_limit > SEMANTIC_EXACT_TOP_K_MAX {
        return Err(anyhow!(
            "source-backed semantic candidate limit must be between 1 and {SEMANTIC_EXACT_TOP_K_MAX}"
        ));
    }
    if projection.generation_id() != index.generation_id() {
        return Err(source_semantic_not_ready(
            "semantic_generation_receipt_mismatch",
            format!(
                "semantic filter projection belongs to Core generation {}, not {}",
                projection.generation_id(),
                index.generation_id()
            ),
        ));
    }
    let active_events = pinned.stats().active_events;
    let eligible_events = projection.len();
    if eligible_events > active_events {
        return Err(source_semantic_not_ready(
            "semantic_projection_event_mismatch",
            format!(
                "Core generation {} selected {eligible_events} semantic events but the flat-F32 generation contains only {active_events}",
                index.generation_id()
            ),
        ));
    }
    let requested_k = candidate_limit.min(eligible_events.max(1));
    let event_is_eligible = |event_id| projection.contains(event_id);
    let search = scan_exact_generation(
        pinned,
        embedding,
        requested_k,
        Some(&event_is_eligible),
        Instant::now(),
    )?;
    let stats = search.stats.clone();
    if stats.events_scored != eligible_events {
        return Err(source_semantic_not_ready(
            "semantic_projection_event_mismatch",
            format!(
                "flat-F32 generation scored {} of {eligible_events} metadata-eligible events from Core generation {}",
                stats.events_scored,
                index.generation_id()
            ),
        ));
    }
    let raw_candidates = search.hits.len();
    let mut non_positive = 0_usize;
    let mut positive_hits = Vec::with_capacity(raw_candidates);
    for hit in search.hits {
        if !hit.similarity.is_finite() || hit.similarity <= 0.0 {
            non_positive = non_positive.saturating_add(1);
            continue;
        }
        positive_hits.push(hit);
    }
    let event_ids = positive_hits
        .iter()
        .map(|hit| hit.event_id)
        .collect::<Vec<_>>();
    let records = index
        .core_events_by_ids_if_bounded(
            &event_ids,
            SEMANTIC_EXACT_TOP_K_MAX,
            MAX_SEMANTIC_CORE_BATCH_BYTES,
        )?
        .ok_or_else(|| {
            source_semantic_not_ready(
                "semantic_projection_event_mismatch",
                format!(
                    "flat-F32 event batch does not map exactly to Core generation {}",
                    index.generation_id()
                ),
            )
        })?;
    let mut candidates = Vec::with_capacity(records.len());
    for (hit, record) in positive_hits.into_iter().zip(records) {
        if record.event_id.as_uuid() != hit.event_id
            || record.event_type != "message"
            || record.role.as_deref() != Some("user")
            || !projection.contains(hit.event_id)
        {
            return Err(source_semantic_not_ready(
                "semantic_projection_event_mismatch",
                format!(
                    "flat-F32 event {} does not match its eligible Core record in generation {}",
                    hit.event_id,
                    index.generation_id()
                ),
            ));
        }
        candidates.push(EventSearchCandidate {
            event: record.event,
            score: hit.similarity,
        });
    }
    candidates.truncate(candidate_limit);
    let diagnostics = source_semantic_diagnostics(
        index,
        Some(pinned),
        Some(&stats),
        requested_k,
        requested_k,
        1,
        raw_candidates,
        candidates.len(),
        active_events.saturating_sub(eligible_events),
        non_positive,
        eligible_events,
        query_embed_ms,
    );
    Ok((candidates, diagnostics))
}

#[allow(clippy::too_many_arguments)]
fn source_semantic_diagnostics(
    index: &VerifiedIndex,
    pinned: Option<&PinnedFlatGeneration>,
    stats: Option<&SemanticVectorSearchStats>,
    initial_k: usize,
    final_k: usize,
    iterations: usize,
    raw_candidates: usize,
    eligible_candidates: usize,
    filtered_candidates: usize,
    non_positive_candidates: usize,
    eligible_event_count: usize,
    query_embed_ms: impl Into<Option<u64>>,
) -> Value {
    let query_embed_ms = query_embed_ms.into();
    compact_json(json!({
        "vector_backend": "flat_f32",
        "core_generation_id": index.generation_id(),
        "flat_generation": pinned.map(PinnedFlatGeneration::generation),
        "flat_generation_hash": pinned.map(PinnedFlatGeneration::generation_hash),
        "query_embed_ms": query_embed_ms,
        "vector_scan_ms": stats.map(|stats| stats.scan_ms),
        "chunks_scanned": stats.map(|stats| stats.chunks_scanned),
        "vector_bytes_read": stats.map(|stats| stats.vector_bytes_read),
        "events_scored": stats.map(|stats| stats.events_scored),
        "initial_k": initial_k,
        "final_k": final_k,
        "iterations": iterations,
        "raw_candidates": raw_candidates,
        "eligible_candidates": eligible_candidates,
        "filtered_candidates": filtered_candidates,
        "non_positive_candidates": non_positive_candidates,
        "exhausted": final_k >= eligible_event_count,
        "cap_reached": final_k >= SEMANTIC_EXACT_TOP_K_MAX
            && final_k < eligible_event_count,
    }))
}

fn source_semantic_not_ready(code: &'static str, detail: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(SourceBackedSemanticNotReady::new(code, detail))
}

fn source_backed_query_pin_from_readiness(
    core_generation_id: &str,
    readiness: SourceBackedGenerationPin,
) -> Result<SourceBackedSemanticQueryPin> {
    let pinned = match readiness {
        SourceBackedGenerationPin::NotReady => {
            return Err(source_semantic_not_ready(
                "semantic_generation_not_acknowledged",
                format!(
                    "flat-F32 projection is missing, stale, partial, or not pinned to Core generation {core_generation_id}"
                ),
            ));
        }
        SourceBackedGenerationPin::ReadyEmpty => None,
        SourceBackedGenerationPin::Ready(pinned) => Some(pinned),
    };
    Ok(SourceBackedSemanticQueryPin {
        core_generation_id: core_generation_id.to_owned(),
        pinned,
        filter_projection: None,
    })
}

fn validate_semantic_query_generation(
    core_generation_id: &str,
    pin: &SourceBackedSemanticQueryPin,
) -> Result<()> {
    if pin.core_generation_id == core_generation_id {
        return Ok(());
    }
    Err(source_semantic_not_ready(
        "semantic_generation_receipt_mismatch",
        format!(
            "flat-F32 query pin belongs to Core generation {}, not {}",
            pin.core_generation_id, core_generation_id
        ),
    ))
}

pub(crate) fn semantic_query_service_supported() -> bool {
    cfg!(ctx_semantic_fastembed)
}

#[cfg(test)]
mod tests;

pub(in crate::semantic) fn daemon_query_service_transport_supported() -> bool {
    cfg!(any(unix, windows))
}

pub(crate) fn daemon_query_service_available(data_root: &Path) -> bool {
    daemon_query_service_ping(data_root).unwrap_or(false)
}

fn daemon_query_service_ping(data_root: &Path) -> Result<bool> {
    let response = daemon_query_request(
        data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": "ping",
        })),
        StdDuration::from_secs(1),
        1024,
    )?;
    Ok(response
        .as_ref()
        .and_then(|value| value.get("ok").and_then(Value::as_bool))
        == Some(true))
}

pub(crate) fn wait_for_daemon_query_service(data_root: &Path, timeout: StdDuration) -> bool {
    if !semantic_query_service_supported() {
        return false;
    }
    let started = Instant::now();
    loop {
        if daemon_query_service_available(data_root) {
            return true;
        }
        if started.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(StdDuration::from_millis(100));
    }
}
