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
        SemanticChunkDocument, SourceBackedSemanticEmbedder, SourceBackedSemanticResolver,
    },
    SemanticEventDocument,
};

mod transport;
#[cfg(test)]
pub(in crate::semantic) use transport::*;
#[cfg(not(test))]
pub(in crate::semantic) use transport::{
    daemon_query_request, daemon_source_hydration_request, daemon_source_refresh_request,
    DaemonQueryResponseTooLarge, DaemonSourceRefreshServiceUnavailable,
};
mod hydration_budget;
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

    #[cfg(test)]
    pub(crate) fn code(&self) -> &str {
        &self.code
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
const SOURCE_HYDRATION_RETAINED_ITEM_OVERHEAD_BYTES: usize = 512;
const SOURCE_HYDRATION_TIMEOUT: StdDuration = StdDuration::from_secs(10);
const SOURCE_HYDRATION_RECOVERY_TIMEOUT: StdDuration = StdDuration::from_secs(5);
const SOURCE_HYDRATION_RECOVERY_RETRY: StdDuration = StdDuration::from_millis(50);
const SOURCE_SEARCH_DISPLAY_MAX_CHARS: usize = 2_048;

#[derive(Debug)]
struct SourceHydrationOperationBudget {
    limit_bytes: usize,
    retained_bytes: usize,
}

impl SourceHydrationOperationBudget {
    fn new(limit_bytes: usize) -> Self {
        Self {
            limit_bytes,
            retained_bytes: 0,
        }
    }

    fn remaining_response_bytes(&self) -> Result<u64> {
        let remaining = self.limit_bytes.saturating_sub(self.retained_bytes);
        if remaining == 0 {
            return Err(source_hydration_budget_exceeded(
                "operation-wide source hydration allowance is exhausted before the next daemon request",
            ));
        }
        u64::try_from(remaining).map_err(|_| {
            source_hydration_budget_exceeded(
                "operation-wide source hydration allowance exceeds the transport byte domain",
            )
        })
    }

    fn retain_batch(&mut self, items: &[(Uuid, String)]) -> Result<()> {
        let batch_bytes = items.iter().try_fold(0usize, |total, (_, text)| {
            retained_source_hydration_text_bytes(text)
                .and_then(|bytes| total.checked_add(bytes))
                .ok_or_else(|| {
                    source_hydration_budget_exceeded(
                        "source hydration retained-byte accounting overflowed",
                    )
                })
        })?;
        let retained_bytes = self
            .retained_bytes
            .checked_add(batch_bytes)
            .ok_or_else(|| {
                source_hydration_budget_exceeded(
                    "source hydration operation retained-byte accounting overflowed",
                )
            })?;
        if retained_bytes > self.limit_bytes {
            return Err(source_hydration_budget_exceeded(
                "source hydration response exceeds the operation-wide retained-byte allowance",
            ));
        }
        self.retained_bytes = retained_bytes;
        Ok(())
    }
}

fn retained_source_hydration_text_bytes(text: &String) -> Option<usize> {
    escaped_json_string_bytes(text)
        .max(text.capacity())
        .checked_add(SOURCE_HYDRATION_RETAINED_ITEM_OVERHEAD_BYTES)
}

fn escaped_json_string_bytes(value: &str) -> usize {
    value.bytes().fold(0usize, |total, byte| {
        total.saturating_add(match byte {
            b'"' | b'\\' | b'\x08' | b'\x09' | b'\x0a' | b'\x0c' | b'\x0d' => 2,
            0x00..=0x1f => 6,
            _ => 1,
        })
    })
}

fn source_hydration_budget_exceeded(detail: impl Into<String>) -> anyhow::Error {
    SourceHydrationUnavailable::new(
        "hydration_budget_exceeded",
        "content_too_large",
        detail,
        false,
    )
    .into()
}

fn map_source_hydration_request_error(error: anyhow::Error) -> anyhow::Error {
    if error
        .downcast_ref::<DaemonQueryResponseTooLarge>()
        .is_some()
    {
        source_hydration_budget_exceeded(
            "daemon source hydration response exceeded the remaining operation-wide transport allowance",
        )
    } else {
        error.context(
            "request daemon generation-bound source hydration without rediscovery fallback",
        )
    }
}

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
    let mut hydrated = HashMap::with_capacity(events.len().min(SOURCE_HYDRATION_BATCH_MAX_ITEMS));
    let mut operation_budget =
        SourceHydrationOperationBudget::new(SOURCE_HYDRATION_RESPONSE_MAX_BYTES as usize);
    let recovery_deadline = Instant::now() + SOURCE_HYDRATION_RECOVERY_TIMEOUT;
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
        let payload = compact_json(json!({
            "schema_version": 1,
            "op": "source_hydrate_batch",
            "generation_id": index.generation_id(),
            "mode": mode,
            "max_chars": max_chars,
            "items": items,
        }));
        let remaining_response_bytes = operation_budget.remaining_response_bytes()?;
        let mut response = loop {
            let response = match daemon_source_hydration_request(
                data_root,
                payload.clone(),
                SOURCE_HYDRATION_TIMEOUT,
                remaining_response_bytes,
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
                Err(error) => return Err(map_source_hydration_request_error(error)),
            };
            let resolver_recovery_pending = response.get("ok").and_then(Value::as_bool)
                != Some(true)
                && response.get("code").and_then(Value::as_str)
                    == Some("resolver_generation_unavailable")
                && response.get("refresh_scheduled").and_then(Value::as_bool) == Some(true);
            if !resolver_recovery_pending || Instant::now() >= recovery_deadline {
                break response;
            }
            std::thread::sleep(SOURCE_HYDRATION_RECOVERY_RETRY);
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
            .get_mut("items")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| anyhow!("daemon source hydration response has no item array"))?;
        if results.len() != batch.len() {
            return Err(anyhow!(
                "daemon source hydration returned {} items for a {}-item batch",
                results.len(),
                batch.len()
            ));
        }
        let mut batch_hydrated = Vec::with_capacity(batch.len());
        for (expected, value) in batch.iter().zip(results) {
            let event_id = value
                .get("event_id")
                .and_then(Value::as_str)
                .and_then(|value| Uuid::parse_str(value).ok())
                .ok_or_else(|| anyhow!("daemon source hydration item has no valid event ID"))?;
            if event_id != expected.event_id.as_uuid()
                || hydrated.contains_key(&event_id)
                || batch_hydrated
                    .iter()
                    .any(|(retained_event_id, _)| *retained_event_id == event_id)
            {
                return Err(anyhow!(
                    "daemon source hydration response is reordered, duplicated, or mismatched at event {}",
                    expected.event_id
                ));
            }
            let text_value = value.get_mut("text").ok_or_else(|| {
                anyhow!(
                    "daemon source hydration returned no content for event {}",
                    expected.event_id
                )
            })?;
            let mut text = match text_value.take() {
                Value::String(text) if !text.is_empty() => text,
                _ => {
                    return Err(anyhow!(
                        "daemon source hydration returned empty content for event {}",
                        expected.event_id
                    ))
                }
            };
            if let Some(max_chars) = max_chars {
                if let Some((byte_index, _)) = text.char_indices().nth(max_chars) {
                    text.truncate(byte_index);
                }
            }
            batch_hydrated.push((event_id, text));
        }
        operation_budget.retain_batch(&batch_hydrated)?;
        hydrated.extend(batch_hydrated);
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
        "content_too_large" => Some("content_too_large"),
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
    ) -> std::result::Result<SemanticEventDocument, HydrationFailure> {
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
        Ok(SemanticEventDocument {
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
