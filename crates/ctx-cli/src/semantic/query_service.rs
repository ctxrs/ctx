use std::{
    collections::HashMap,
    path::Path,
    time::{Duration as StdDuration, Instant},
};

use anyhow::{anyhow, Result};
use ctx_history_core::HydrationFailure;
#[cfg(test)]
use ctx_history_core::HydrationFailureKind;
#[cfg(test)]
use ctx_history_core::{CaptureProvider, EventRole, EventType};
use ctx_history_index::{EventRecord, EventSearchCandidate, EventSearchFilters, VerifiedIndex};
use serde_json::{json, Value};
use thiserror::Error;
use uuid::Uuid;

use crate::compact_json;

use super::{
    health_search::daemon_query_embedding,
    runtime_limits::SEMANTIC_EXACT_TOP_K_MAX,
    source_backed_refresh_coordinator::PinnedSourceBackedGeneration,
    vector_store::{
        flat_segments::PinnedFlatGeneration, source_backed_semantic_vector_path,
        SemanticVectorSearchStats, SemanticVectorStore,
    },
    vector_store_search::scan_exact_generation,
};
#[cfg(test)]
use super::{
    model_contract::SEMANTIC_DIMENSIONS,
    vector_store::{
        SemanticChunkDocument, SourceBackedSemanticDocumentBuilder, SourceBackedSemanticEmbedder,
    },
    SemanticEventDocument,
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
mod semantic_filters;
mod server;
use self::semantic_filters::source_event_matches_filters;
#[cfg(test)]
pub(in crate::semantic) use server::*;
#[cfg(not(test))]
pub(in crate::semantic) use server::{
    daemon_can_begin_idle_shutdown, observe_daemon_query_activity, start_daemon_query_service,
    start_daemon_source_refresh_service, DaemonQueryActivity, DaemonQueryService,
};

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
}

// Temporary test-only bridge for the P3 presentation lane while its old
// public hydration-error tests are removed. Production Core reads never
// construct this error and never retry provider access.
#[cfg(test)]
#[derive(Debug, Error)]
#[error("{code}/{failure_kind}")]
struct RetiredSourceHydrationTestError {
    code: String,
    failure_kind: &'static str,
}

impl PinnedSourceBackedGeneration {
    #[cfg(test)]
    pub(crate) fn source_hydration_error_for_test(
        code: &'static str,
        failure_kind: &'static str,
    ) -> anyhow::Error {
        RetiredSourceHydrationTestError {
            code: code.to_owned(),
            failure_kind,
        }
        .into()
    }

    pub(crate) fn source_hydration_code(error: &anyhow::Error) -> Option<&str> {
        #[cfg(test)]
        if let Some(error) = error.downcast_ref::<RetiredSourceHydrationTestError>() {
            return Some(&error.code);
        }
        #[cfg(not(test))]
        let _ = error;
        None
    }

    pub(crate) fn source_hydration_retryable(error: &anyhow::Error) -> bool {
        #[cfg(test)]
        if let Some(error) = error.downcast_ref::<RetiredSourceHydrationTestError>() {
            return matches!(
                error.failure_kind,
                "temporarily_unavailable"
                    | "confirmed_deleted"
                    | "stale_source_evidence"
                    | "stale_record_evidence"
                    | "missing_record"
            );
        }
        #[cfg(not(test))]
        let _ = error;
        false
    }

    pub(crate) fn source_hydration_failure(error: &anyhow::Error) -> Option<HydrationFailure> {
        #[cfg(test)]
        if let Some(error) = error.downcast_ref::<RetiredSourceHydrationTestError>() {
            return HydrationFailureKind::parse(error.failure_kind).map(|kind| HydrationFailure {
                kind,
                detail: "source hydration exceeds the aggregate byte budget".to_owned(),
            });
        }
        #[cfg(not(test))]
        let _ = error;
        None
    }

    pub(crate) fn hydrate_source_search_page(
        index: &VerifiedIndex,
        _data_root: &Path,
        events: &[&EventRecord],
    ) -> Result<HashMap<Uuid, String>> {
        core_content_for_events(index, events, Some(2_048))
    }

    pub(crate) fn hydrate_source_complete_events(
        index: &VerifiedIndex,
        _data_root: &Path,
        events: &[&EventRecord],
    ) -> Result<HashMap<Uuid, String>> {
        core_content_for_events(index, events, None)
    }

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
        let ready = vector_store
            .source_backed_generation_ready_exact(index.generation_id(), semantic_documents)
            .map_err(|error| {
                source_semantic_not_ready(
                    "semantic_generation_unreadable",
                    format!("semantic source acknowledgement could not be verified: {error:#}"),
                )
            })?;
        if !ready {
            return Err(source_semantic_not_ready(
                "semantic_generation_not_acknowledged",
                format!(
                    "flat-F32 projection is missing, stale, partial, or not pinned to Core generation {}",
                    index.generation_id()
                ),
            ));
        }
        let pinned = vector_store
            .pin_source_backed_generation(index.generation_id(), semantic_documents)
            .map_err(|error| {
                source_semantic_not_ready(
                    "semantic_generation_unreadable",
                    format!("semantic source generation could not be pinned: {error:#}"),
                )
            })?;
        Ok(SourceBackedSemanticQueryPin {
            core_generation_id: index.generation_id().to_owned(),
            pinned,
        })
    }

    pub(crate) fn semantic_candidates_for_pinned_source_generation(
        index: &VerifiedIndex,
        data_root: &Path,
        query: &str,
        filters: &EventSearchFilters,
        candidate_limit: usize,
        pin: &SourceBackedSemanticQueryPin,
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
        source_semantic_candidates_with_embedding(
            index,
            pinned,
            filters,
            candidate_limit,
            &embedding,
            Some(query_embed_ms),
        )
    }

    #[cfg(test)]
    pub(crate) fn semantic_candidates_for_source_generation_with_embedding(
        index: &VerifiedIndex,
        data_root: &Path,
        filters: &EventSearchFilters,
        candidate_limit: usize,
        embedding: &[f32],
    ) -> Result<(Vec<EventSearchCandidate>, Value)> {
        let pin = Self::pin_semantic_query_for_source_generation(index, data_root)?;
        let Some(pinned) = pin.pinned.as_ref() else {
            return Ok((Vec::new(), json!({"vector_backend": "flat_f32"})));
        };
        source_semantic_candidates_with_embedding(
            index,
            pinned,
            filters,
            candidate_limit,
            embedding,
            None,
        )
    }

    #[cfg(test)]
    pub(crate) fn install_source_generation_flat_fixture(
        index: &VerifiedIndex,
        data_root: &Path,
        embedding: &[f32],
        _retired_provider_texts: HashMap<Uuid, String>,
    ) -> Result<()> {
        if embedding.len() != SEMANTIC_DIMENSIONS {
            return Err(anyhow!(
                "source generation fixture embedding has {} dimensions, expected {SEMANTIC_DIMENSIONS}",
                embedding.len()
            ));
        }
        let mut vector_store =
            SemanticVectorStore::open(&source_backed_semantic_vector_path(data_root))?;
        let mut builder = ExactCoreFixtureBuilder;
        let mut embedder = ExactSourceFixtureEmbedder {
            embedding: embedding.to_vec(),
        };
        for _ in 0..1_024 {
            let outcome =
                vector_store.reconcile_source_backed_index(index, &mut builder, &mut embedder)?;
            if outcome.ready {
                let semantic_documents = index.semantic_eligible_event_count()?;
                if !vector_store.source_backed_generation_ready_exact(
                    index.generation_id(),
                    semantic_documents,
                )? {
                    return Err(anyhow!(
                        "source generation fixture did not publish an exact flat-F32 acknowledgement"
                    ));
                }
                return Ok(());
            }
            if !outcome.work_remaining {
                return Err(anyhow!(
                    "source generation fixture stopped before publishing its flat-F32 acknowledgement"
                ));
            }
        }
        Err(anyhow!(
            "source generation fixture exceeded its bounded projection page count"
        ))
    }
}

fn core_content_for_events(
    index: &VerifiedIndex,
    events: &[&EventRecord],
    max_chars: Option<usize>,
) -> Result<HashMap<Uuid, String>> {
    let mut contents = HashMap::with_capacity(events.len());
    for event in events {
        let record = index
            .core_event_by_id(event.event_id.as_uuid())?
            .ok_or_else(|| {
                anyhow!(
                    "Core generation {} omitted event {}",
                    index.generation_id(),
                    event.event_id
                )
            })?;
        if record.event_id != event.event_id || record.session_id != event.session_id {
            return Err(anyhow!(
                "Core event {} does not match its pinned citation",
                event.event_id
            ));
        }
        let mut text = record.core_record.content.meaningful_text().to_owned();
        if text.is_empty() {
            return Err(anyhow!(
                "Core event {} has no display content",
                event.event_id
            ));
        }
        if let Some(max_chars) = max_chars {
            if let Some((byte_index, _)) = text.char_indices().nth(max_chars) {
                text.truncate(byte_index);
            }
        }
        contents.insert(event.event_id.as_uuid(), text);
    }
    Ok(contents)
}

#[cfg(test)]
struct ExactCoreFixtureBuilder;

#[cfg(test)]
impl SourceBackedSemanticDocumentBuilder for ExactCoreFixtureBuilder {
    fn build_document(
        &mut self,
        record: &ctx_history_index::CoreEventRecord,
    ) -> Result<Option<SemanticEventDocument>> {
        let text = record.core_record.content.meaningful_text().to_owned();
        if text.is_empty() {
            return Ok(None);
        }
        Ok(Some(SemanticEventDocument {
            event_id: record.event_id.as_uuid(),
            history_record_id: None,
            session_id: Some(record.session_id.as_uuid()),
            seq: record.event_sequence,
            occurred_at_ms: record.occurred_at_unix_ms.unwrap_or_default(),
            anchor_occurred_at_ms: record.occurred_at_unix_ms.unwrap_or_default(),
            event_type: EventType::Message,
            role: Some(EventRole::User),
            rank_bucket: "core_generation_fixture".to_owned(),
            provider: Some(CaptureProvider::Codex),
            source_format: Some(record.source_format.clone()),
            agent_type: None,
            session_is_primary: Some(record.is_primary),
            cwd: record.cwd.clone(),
            raw_source_path: None,
            record_title: None,
            record_kind: Some(record.event_type.clone()),
            record_workspace: record.workspace.clone(),
            text,
        }))
    }
}

#[cfg(test)]
struct ExactSourceFixtureEmbedder {
    embedding: Vec<f32>,
}

#[cfg(test)]
impl SourceBackedSemanticEmbedder for ExactSourceFixtureEmbedder {
    fn embed_chunks(&mut self, chunks: &[SemanticChunkDocument]) -> Result<Vec<Vec<f32>>> {
        Ok(chunks
            .iter()
            .map(|_| self.embedding.clone())
            .collect::<Vec<_>>())
    }
}

fn source_semantic_candidates_with_embedding(
    index: &VerifiedIndex,
    pinned: &PinnedFlatGeneration,
    filters: &EventSearchFilters,
    candidate_limit: usize,
    embedding: &[f32],
    query_embed_ms: Option<u64>,
) -> Result<(Vec<EventSearchCandidate>, Value)> {
    if candidate_limit == 0 || candidate_limit > SEMANTIC_EXACT_TOP_K_MAX {
        return Err(anyhow!(
            "source-backed semantic candidate limit must be between 1 and {SEMANTIC_EXACT_TOP_K_MAX}"
        ));
    }
    let active_events = pinned.stats().active_events;
    let mut requested_k = candidate_limit.min(active_events.max(1));
    let initial_k = requested_k;
    let mut iterations = 0_usize;
    loop {
        iterations = iterations.saturating_add(1);
        let search = scan_exact_generation(pinned, embedding, requested_k, None, Instant::now())?;
        let stats = search.stats.clone();
        let raw_candidates = search.hits.len();
        let mut filtered = 0_usize;
        let mut non_positive = 0_usize;
        let mut candidates = Vec::with_capacity(raw_candidates);
        for hit in search.hits {
            if !hit.similarity.is_finite() || hit.similarity <= 0.0 {
                non_positive = non_positive.saturating_add(1);
                continue;
            }
            let record = index.core_event_by_id(hit.event_id)?.ok_or_else(|| {
                source_semantic_not_ready(
                    "semantic_projection_event_mismatch",
                    format!(
                        "flat-F32 event {} is absent from Core generation {}",
                        hit.event_id,
                        index.generation_id()
                    ),
                )
            })?;
            if record.event_type != "message" || record.role.as_deref() != Some("user") {
                return Err(source_semantic_not_ready(
                    "semantic_projection_event_mismatch",
                    format!(
                        "flat-F32 event {} is not metadata-eligible in Core generation {}",
                        hit.event_id,
                        index.generation_id()
                    ),
                ));
            }
            if !source_event_matches_filters(&record.event, filters) {
                filtered = filtered.saturating_add(1);
                continue;
            }
            candidates.push(EventSearchCandidate {
                event: record.event,
                score: hit.similarity,
            });
        }
        let exhausted = requested_k >= active_events;
        if candidates.len() >= candidate_limit
            || exhausted
            || requested_k >= SEMANTIC_EXACT_TOP_K_MAX
        {
            candidates.truncate(candidate_limit);
            let diagnostics = source_semantic_diagnostics(
                index,
                Some(pinned),
                Some(&stats),
                initial_k,
                requested_k,
                iterations,
                raw_candidates,
                candidates.len(),
                filtered,
                non_positive,
                query_embed_ms,
            );
            return Ok((candidates, diagnostics));
        }
        requested_k = requested_k
            .saturating_mul(2)
            .min(active_events)
            .min(SEMANTIC_EXACT_TOP_K_MAX)
            .max(requested_k.saturating_add(1));
    }
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
        "exhausted": pinned.is_none_or(|pinned| final_k >= pinned.stats().active_events),
        "cap_reached": final_k >= SEMANTIC_EXACT_TOP_K_MAX,
    }))
}

fn source_semantic_not_ready(code: &'static str, detail: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(SourceBackedSemanticNotReady::new(code, detail))
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
