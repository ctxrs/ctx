use std::{
    collections::HashMap,
    path::Path,
    time::{Duration as StdDuration, Instant},
};

use anyhow::{anyhow, Result};
use ctx_history_core::{
    BatchHydrationRequest, EventHydrationRequest, HydrationFailure, HydrationFailureKind,
};
#[cfg(test)]
use ctx_history_core::{CaptureProvider, EventRole, EventType};
use ctx_history_index::{
    AgentScope, EventRecord, EventSearchCandidate, EventSearchFilters, VerifiedIndex,
};
#[cfg(test)]
use ctx_history_store::EventEmbeddingDocument;
use ctx_history_store::Store;
use serde_json::{json, Value};
use thiserror::Error;
use uuid::Uuid;

use crate::{commands::search::RefreshArg, compact_json, SearchBackendArg};

use super::{
    health_search::{daemon_query_embedding, semantic_filters_need_overfetch},
    reports::SemanticRetrievalReport,
    runtime_limits::{
        SEMANTIC_EXACT_TOP_K_MAX, SEMANTIC_SEARCH_CANDIDATES,
        SEMANTIC_SOFT_FILTER_SEARCH_CANDIDATES,
    },
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
        SemanticChunkDocument, SourceBackedSemanticEmbedder, SourceBackedSemanticResolver,
    },
};

mod transport;
#[cfg(test)]
pub(in crate::semantic) use transport::*;
#[cfg(not(test))]
pub(in crate::semantic) use transport::{
    daemon_query_request, daemon_source_hydration_request, daemon_source_refresh_request,
    DaemonSourceRefreshServiceUnavailable,
};
mod server;
#[cfg(test)]
pub(in crate::semantic) use server::*;
#[cfg(not(test))]
pub(in crate::semantic) use server::{
    daemon_can_begin_idle_shutdown, observe_daemon_query_activity, start_daemon_query_service,
    start_daemon_source_refresh_service, DaemonQueryActivity, DaemonQueryService,
};

#[allow(clippy::too_many_arguments)]
pub(crate) fn search_packet_with_backend(
    store: &Store,
    _data_root: &Path,
    query: &str,
    terms: &[String],
    options: &ctx_history_search::PacketOptions,
    requested_backend: SearchBackendArg,
    _semantic_enabled: bool,
    _semantic_weight: f32,
    _refresh_mode: RefreshArg,
    _emit_warnings: bool,
) -> Result<(ctx_history_search::SearchPacket, SemanticRetrievalReport)> {
    let uses_composed_terms = terms.iter().any(|term| !term.trim().is_empty());
    if requested_backend != SearchBackendArg::Lexical {
        return Err(anyhow!(
            "semantic and hybrid search require a fresh source-backed Core generation; the legacy Store is available only for explicit lexical rollback/recovery"
        ));
    }
    let packet = if uses_composed_terms {
        ctx_history_search::search_packet_terms(store, query, terms, options)?
    } else {
        ctx_history_search::search_packet(store, query, options)?
    };
    Ok((
        packet,
        SemanticRetrievalReport::lexical(SearchBackendArg::Lexical, 0),
    ))
}

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
}

pub(crate) struct SourceBackedSemanticQueryPin {
    core_generation_id: String,
    pinned: Option<PinnedFlatGeneration>,
}

#[derive(Debug, Error)]
#[error(
    "generation-bound source hydration failed ({code}/{failure_kind}, refresh_scheduled={refresh_scheduled}): {detail}"
)]
pub(crate) struct SourceHydrationUnavailable {
    code: String,
    failure_kind: &'static str,
    detail: String,
    refresh_scheduled: bool,
}

impl SourceHydrationUnavailable {
    fn new(
        code: impl Into<String>,
        failure_kind: &'static str,
        detail: impl Into<String>,
        refresh_scheduled: bool,
    ) -> Self {
        Self {
            code: code.into(),
            failure_kind,
            detail: detail.into(),
            refresh_scheduled,
        }
    }

    fn retryable_after_refresh(&self) -> bool {
        matches!(
            self.failure_kind,
            "temporarily_unavailable"
                | "confirmed_deleted"
                | "stale_source_evidence"
                | "stale_record_evidence"
                | "missing_record"
        )
    }

    fn hydration_failure(&self) -> HydrationFailure {
        HydrationFailure {
            kind: match self.failure_kind {
                "confirmed_deleted" => HydrationFailureKind::ConfirmedDeleted,
                "stale_source_evidence" => HydrationFailureKind::StaleSourceEvidence,
                "stale_record_evidence" => HydrationFailureKind::StaleRecordEvidence,
                "missing_record" => HydrationFailureKind::MissingRecord,
                "unsupported_parser_revision" => HydrationFailureKind::UnsupportedParserRevision,
                "invalid_locator" => HydrationFailureKind::InvalidLocator,
                _ => HydrationFailureKind::TemporarilyUnavailable,
            },
            detail: self.to_string(),
        }
    }
}

const SOURCE_HYDRATION_BATCH_MAX_ITEMS: usize = 128;
const SOURCE_HYDRATION_RESPONSE_MAX_BYTES: u64 = 64 * 1024 * 1024;
const SOURCE_HYDRATION_TIMEOUT: StdDuration = StdDuration::from_secs(10);
const SOURCE_SEARCH_DISPLAY_MAX_CHARS: usize = 2_048;

impl PinnedSourceBackedGeneration {
    pub(crate) fn source_hydration_retryable(error: &anyhow::Error) -> bool {
        error
            .downcast_ref::<SourceHydrationUnavailable>()
            .is_some_and(SourceHydrationUnavailable::retryable_after_refresh)
    }

    pub(crate) fn source_hydration_failure(error: &anyhow::Error) -> Option<HydrationFailure> {
        error
            .downcast_ref::<SourceHydrationUnavailable>()
            .map(SourceHydrationUnavailable::hydration_failure)
    }

    pub(crate) fn hydrate_source_search_page(
        index: &VerifiedIndex,
        data_root: &Path,
        events: &[&EventRecord],
    ) -> Result<HashMap<Uuid, String>> {
        hydrate_source_events_via_daemon(
            index,
            data_root,
            events,
            "search_display",
            Some(SOURCE_SEARCH_DISPLAY_MAX_CHARS),
        )
    }

    pub(crate) fn hydrate_source_complete_events(
        index: &VerifiedIndex,
        data_root: &Path,
        events: &[&EventRecord],
    ) -> Result<HashMap<Uuid, String>> {
        hydrate_source_events_via_daemon(index, data_root, events, "complete", None)
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

    pub(crate) fn semantic_candidates_for_source_generation(
        index: &VerifiedIndex,
        data_root: &Path,
        query: &str,
        filters: &EventSearchFilters,
        candidate_limit: usize,
    ) -> Result<(Vec<EventSearchCandidate>, Value)> {
        let pin = Self::pin_semantic_query_for_source_generation(index, data_root)?;
        Self::semantic_candidates_for_pinned_source_generation(
            index,
            data_root,
            query,
            filters,
            candidate_limit,
            &pin,
        )
    }

    pub(crate) fn semantic_candidates_for_pinned_source_generation(
        index: &VerifiedIndex,
        data_root: &Path,
        query: &str,
        filters: &EventSearchFilters,
        candidate_limit: usize,
        pin: &SourceBackedSemanticQueryPin,
    ) -> Result<(Vec<EventSearchCandidate>, Value)> {
        if pin.core_generation_id != index.generation_id() {
            return Err(source_semantic_not_ready(
                "semantic_generation_receipt_mismatch",
                format!(
                    "flat-F32 query pin belongs to Core generation {}, not {}",
                    pin.core_generation_id,
                    index.generation_id()
                ),
            ));
        }
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
        exact_source_texts: HashMap<Uuid, String>,
    ) -> Result<()> {
        if embedding.len() != SEMANTIC_DIMENSIONS {
            return Err(anyhow!(
                "source generation fixture embedding has {} dimensions, expected {SEMANTIC_DIMENSIONS}",
                embedding.len()
            ));
        }
        let mut vector_store =
            SemanticVectorStore::open(&source_backed_semantic_vector_path(data_root))?;
        let mut resolver = ExactSourceFixtureResolver {
            texts: exact_source_texts,
        };
        let mut embedder = ExactSourceFixtureEmbedder {
            embedding: embedding.to_vec(),
        };
        for _ in 0..1_024 {
            let outcome =
                vector_store.reconcile_source_backed_index(index, &mut resolver, &mut embedder)?;
            if let Some(unavailable) = outcome.unavailable {
                return Err(anyhow!(
                    "source generation fixture hydration was unavailable: {unavailable:?}"
                ));
            }
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

fn hydrate_source_events_via_daemon(
    index: &VerifiedIndex,
    data_root: &Path,
    events: &[&EventRecord],
    mode: &'static str,
    max_chars: Option<usize>,
) -> Result<HashMap<Uuid, String>> {
    let mut hydrated = HashMap::with_capacity(events.len());
    for batch in events.chunks(SOURCE_HYDRATION_BATCH_MAX_ITEMS) {
        let requests = batch
            .iter()
            .map(|event| {
                event.event_id.validate_contract()?;
                event.session_id.validate_contract()?;
                event.locator.validate_contract()?;
                if event.event_id.source_digest() != event.locator.source().identity().digest()
                    || event.event_id.source_descriptor_digest()
                        != event.locator.source().exact_descriptor_digest()
                    || event.session_id.source_digest()
                        != event.locator.source().identity().digest()
                    || event.session_id.source_descriptor_digest()
                        != event.locator.source().exact_descriptor_digest()
                {
                    return Err(anyhow!(
                        "source-backed presentation identity does not match its generation locator"
                    ));
                }
                EventHydrationRequest::new(event.event_id, event.locator.clone())
                    .map_err(anyhow::Error::from)
            })
            .collect::<Result<Vec<_>>>()?;
        let request = BatchHydrationRequest::new(requests)?;
        let items = request
            .events()
            .iter()
            .map(|event| {
                json!({
                    "event_identity": event.event_id(),
                    "locator": event.locator(),
                })
            })
            .collect::<Vec<_>>();
        let response = match daemon_source_hydration_request(
            data_root,
            compact_json(json!({
                "schema_version": 1,
                "op": "source_hydrate_batch",
                "generation_id": index.generation_id(),
                "mode": mode,
                "max_chars": max_chars,
                "items": items,
            })),
            SOURCE_HYDRATION_TIMEOUT,
            SOURCE_HYDRATION_RESPONSE_MAX_BYTES,
        ) {
            Ok(Some(response)) => response,
            Ok(None) => {
                return Err(SourceHydrationUnavailable::new(
                    "resolver_service_unavailable",
                    "temporarily_unavailable",
                    "daemon generation-bound source hydration service is unavailable; no provider rediscovery or stored preview fallback was attempted",
                    false,
                )
                .into())
            }
            Err(error)
                if error
                    .downcast_ref::<DaemonSourceRefreshServiceUnavailable>()
                    .is_some() =>
            {
                return Err(SourceHydrationUnavailable::new(
                    "resolver_service_unavailable",
                    "temporarily_unavailable",
                    format!("{error:#}; no provider rediscovery or stored preview fallback was attempted"),
                    false,
                )
                .into())
            }
            Err(error) => {
                return Err(error.context(
                    "request daemon generation-bound source hydration without rediscovery fallback",
                ))
            }
        };
        if response.get("ok").and_then(Value::as_bool) != Some(true) {
            let code = response
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("source_hydration_unavailable");
            let kind = response
                .get("failure_kind")
                .and_then(Value::as_str)
                .unwrap_or("temporarily_unavailable");
            let detail = response
                .get("detail")
                .or_else(|| response.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("daemon source hydration failed");
            let refresh_scheduled = response
                .get("refresh_scheduled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let kind = source_hydration_failure_kind(kind).ok_or_else(|| {
                anyhow!("daemon source hydration returned unknown failure kind {kind:?}")
            })?;
            return Err(
                SourceHydrationUnavailable::new(code, kind, detail, refresh_scheduled).into(),
            );
        }
        if response.get("generation_id").and_then(Value::as_str) != Some(index.generation_id()) {
            return Err(SourceHydrationUnavailable::new(
                "resolver_generation_mismatch",
                "stale_source_evidence",
                format!(
                    "daemon source hydration response does not match pinned generation {}",
                    index.generation_id()
                ),
                true,
            )
            .into());
        }
        let results = response
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("daemon source hydration response has no item array"))?;
        if results.len() != batch.len() {
            return Err(anyhow!(
                "daemon source hydration returned {} items for a {}-item batch",
                results.len(),
                batch.len()
            ));
        }
        for (expected, value) in batch.iter().zip(results) {
            let event_id = value
                .get("event_id")
                .and_then(Value::as_str)
                .and_then(|value| Uuid::parse_str(value).ok())
                .ok_or_else(|| anyhow!("daemon source hydration item has no valid event ID"))?;
            if event_id != expected.event_id.as_uuid() || hydrated.contains_key(&event_id) {
                return Err(anyhow!(
                    "daemon source hydration response is reordered, duplicated, or mismatched at event {}",
                    expected.event_id
                ));
            }
            let text = value
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .ok_or_else(|| {
                    anyhow!(
                        "daemon source hydration returned empty content for event {}",
                        expected.event_id
                    )
                })?;
            let text = if let Some(max_chars) = max_chars {
                text.chars().take(max_chars).collect()
            } else {
                text.to_owned()
            };
            hydrated.insert(event_id, text);
        }
    }
    Ok(hydrated)
}

fn source_hydration_failure_kind(value: &str) -> Option<&'static str> {
    match value {
        "temporarily_unavailable" => Some("temporarily_unavailable"),
        "confirmed_deleted" => Some("confirmed_deleted"),
        "stale_source_evidence" => Some("stale_source_evidence"),
        "stale_record_evidence" => Some("stale_record_evidence"),
        "missing_record" => Some("missing_record"),
        "unsupported_parser_revision" => Some("unsupported_parser_revision"),
        "invalid_locator" => Some("invalid_locator"),
        _ => None,
    }
}

#[cfg(test)]
struct ExactSourceFixtureResolver {
    texts: HashMap<Uuid, String>,
}

#[cfg(test)]
impl SourceBackedSemanticResolver for ExactSourceFixtureResolver {
    fn resolve_document(
        &mut self,
        event: &EventRecord,
        request: &EventHydrationRequest,
    ) -> std::result::Result<EventEmbeddingDocument, HydrationFailure> {
        if request.event_id() != event.event_id || request.locator() != &event.locator {
            return Err(HydrationFailure {
                kind: HydrationFailureKind::InvalidLocator,
                detail: "fixture source request did not match the pinned event".to_owned(),
            });
        }
        let text = self
            .texts
            .get(&event.event_id.as_uuid())
            .cloned()
            .ok_or_else(|| HydrationFailure {
                kind: HydrationFailureKind::MissingRecord,
                detail: "fixture exact provider content is absent".to_owned(),
            })?;
        Ok(EventEmbeddingDocument {
            event_id: event.event_id.as_uuid(),
            history_record_id: None,
            session_id: Some(event.session_id.as_uuid()),
            seq: event.event_sequence,
            occurred_at_ms: event.occurred_at_unix_ms.unwrap_or_default(),
            anchor_occurred_at_ms: event.occurred_at_unix_ms.unwrap_or_default(),
            event_type: EventType::Message,
            role: Some(EventRole::User),
            rank_bucket: "source_generation_fixture".to_owned(),
            provider: Some(CaptureProvider::Codex),
            source_format: Some(event.source_format.clone()),
            agent_type: None,
            session_is_primary: Some(event.is_primary),
            cwd: event.cwd.clone(),
            raw_source_path: event.source_path.clone(),
            record_title: None,
            record_kind: Some(event.event_type.clone()),
            record_workspace: event.workspace.clone(),
            text,
        })
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
            let event = index.event_by_id(hit.event_id)?.ok_or_else(|| {
                source_semantic_not_ready(
                    "semantic_projection_event_mismatch",
                    format!(
                        "flat-F32 event {} is absent from Core generation {}",
                        hit.event_id,
                        index.generation_id()
                    ),
                )
            })?;
            if event.event_type != "message" || event.role.as_deref() != Some("user") {
                return Err(source_semantic_not_ready(
                    "semantic_projection_event_mismatch",
                    format!(
                        "flat-F32 event {} is not metadata-eligible in Core generation {}",
                        hit.event_id,
                        index.generation_id()
                    ),
                ));
            }
            if !source_event_matches_filters(&event, filters) {
                filtered = filtered.saturating_add(1);
                continue;
            }
            candidates.push(EventSearchCandidate {
                event,
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

fn source_event_matches_filters(event: &EventRecord, filters: &EventSearchFilters) -> bool {
    if filters
        .session_id
        .is_some_and(|id| event.session_id.as_uuid() != id)
        || filters
            .parent_session_id
            .is_some_and(|id| event.parent_session_id.map(|value| value.as_uuid()) != Some(id))
        || filters
            .root_session_id
            .is_some_and(|id| event.root_session_id.as_uuid() != id)
        || filters
            .provider
            .as_deref()
            .is_some_and(|value| event.provider != value)
        || filters
            .source_format
            .as_deref()
            .is_some_and(|value| event.source_format != value)
        || filters
            .provider_session_id
            .as_deref()
            .is_some_and(|value| event.provider_session_id.as_deref() != Some(value))
        || filters
            .branch
            .as_deref()
            .is_some_and(|value| event.branch.as_deref() != Some(value))
        || filters
            .event_type
            .as_deref()
            .is_some_and(|value| event.event_type != value)
        || filters
            .role
            .as_deref()
            .is_some_and(|value| event.role.as_deref() != Some(value))
        || filters
            .agent_type
            .as_deref()
            .is_some_and(|value| event.agent_type != value)
        || filters
            .since_unix_ms
            .is_some_and(|since| event.occurred_at_unix_ms.is_none_or(|value| value < since))
    {
        return false;
    }
    if filters.agent_scope == AgentScope::Primary
        && filters.session_id.is_none()
        && !event.is_primary
        && event.agent_type != "primary"
    {
        return false;
    }
    if filters.workspace.as_deref().is_some_and(|needle| {
        !event
            .workspace
            .as_deref()
            .is_some_and(|value| metadata_contains(value, needle))
    }) {
        return false;
    }
    if filters.file.as_deref().is_some_and(|needle| {
        !event
            .touched_files
            .iter()
            .any(|value| metadata_contains(value, needle))
    }) {
        return false;
    }
    !filters
        .exclude_session_tree
        .as_ref()
        .is_some_and(|excluded| {
            let provider_thread = event.provider == excluded.provider
                && event.provider_session_id.as_deref()
                    == Some(excluded.provider_session_id.as_str());
            provider_thread
                || excluded.session_id.is_some_and(|session_id| {
                    event.session_id.as_uuid() == session_id
                        || event.parent_session_id.map(|id| id.as_uuid()) == Some(session_id)
                        || event.root_session_id.as_uuid() == session_id
                })
        })
}

fn metadata_contains(value: &str, needle: &str) -> bool {
    value.to_lowercase().contains(&needle.trim().to_lowercase())
}

fn source_semantic_not_ready(code: &'static str, detail: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(SourceBackedSemanticNotReady::new(code, detail))
}

pub(super) fn semantic_candidate_limit(options: &ctx_history_search::PacketOptions) -> usize {
    let overfetch = if semantic_filters_need_overfetch(&options.filters) {
        SEMANTIC_SOFT_FILTER_SEARCH_CANDIDATES.max(options.limit.saturating_mul(100))
    } else {
        SEMANTIC_SEARCH_CANDIDATES.max(options.limit.saturating_mul(8))
    };
    overfetch.min(SEMANTIC_EXACT_TOP_K_MAX)
}

pub(crate) fn semantic_query_service_supported() -> bool {
    cfg!(ctx_semantic_fastembed)
}

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
