use std::time::Duration;

use anyhow::{anyhow, Result};
use ctx_history_index_query::{EventSearchFilters, VerifiedIndex};
use serde_json::{json, Value};

use crate::json::{compact_json, event_copy_json, timestamp_json};
use crate::{
    NormalizedSearchQuery, SearchCollection, SearchHit, SearchPresentation, SearchRequest,
    SEARCH_SNIPPET_MAX_CHARS,
};

pub struct SearchJsonInput<'input> {
    pub request: &'input SearchRequest,
    pub index: &'input VerifiedIndex,
    pub collection: &'input SearchCollection,
    pub filters: &'input EventSearchFilters,
    pub presentations: &'input [SearchPresentation],
    pub copied_lineages: &'input [Value],
    pub commands: &'input [SearchResultCommands],
    pub freshness_mode: &'input str,
    pub generated_at: &'input str,
    pub semantic_fallback_code: Option<&'input str>,
    pub semantic_fallback_detail: Option<&'input str>,
    pub metrics: SearchRenderMetrics<'input>,
}

pub struct SearchRenderMetrics<'a> {
    pub refresh_status: &'a str,
    pub refresh_source_count: usize,
    pub query_duration: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResultCommands {
    pub suggested_next_commands: Vec<String>,
}

pub fn search_json(input: SearchJsonInput<'_>) -> Result<Value> {
    render_search_json(input)
}

pub fn render_search_json(input: SearchJsonInput<'_>) -> Result<Value> {
    let SearchJsonInput {
        request,
        index,
        collection,
        filters,
        presentations,
        copied_lineages,
        commands,
        freshness_mode,
        generated_at,
        semantic_fallback_code,
        semantic_fallback_detail,
        metrics,
    } = input;
    let normalized_query = NormalizedSearchQuery::from_request(request);
    let result_scope = if request.events { "event" } else { "session" };
    let expected = collection.result_window.hits.len();
    if presentations.len() != expected {
        return Err(anyhow!(
            "pinned Core lookup returned {} search presentations for {} hits",
            presentations.len(),
            expected
        ));
    }
    if copied_lineages.len() != expected {
        return Err(anyhow!(
            "pinned Core lookup returned {} copied-lineage values for {} hits",
            copied_lineages.len(),
            expected
        ));
    }
    if commands.len() != expected {
        return Err(anyhow!(
            "read-model adapter returned {} command projections for {} hits",
            commands.len(),
            expected
        ));
    }
    let results = collection
        .result_window
        .hits
        .iter()
        .zip(presentations)
        .zip(copied_lineages)
        .zip(commands)
        .enumerate()
        .map(
            |(offset, (((hit, presentation), copied_lineage), commands))| {
                if presentation.event_id != hit.event.event_id {
                    return Err(anyhow!(
                        "out-of-order search presentation for event {}",
                        presentation.event_id
                    ));
                }
                search_result_json(
                    hit,
                    presentation,
                    result_scope,
                    offset.saturating_add(1),
                    copied_lineage,
                    commands,
                )
            },
        )
        .collect::<Result<Vec<_>>>()?;
    let phase_attribution = phase_attribution(metrics.query_duration);
    let semantic_diagnostics = semantic_diagnostics_read_model(
        collection,
        semantic_fallback_code,
        semantic_fallback_detail,
    );
    Ok(compact_json(json!({
        "schema_version": 1,
        "payload_type": "search_results",
        "query": normalized_query.display(),
        "filters": {
            "provider": filters.provider,
            "history_source": filters.history_source,
            "provider_key": filters.provider_key,
            "source_id": filters.source_id,
            "source_format": filters.source_format,
            "source_root": (!request.source_roots.is_empty()).then_some(&request.source_roots),
            "source_groups": (!request.source_groups.is_empty()).then_some(&request.source_groups),
            "workspace": request.workspace,
            "since": request.since,
            "content_scope": filters.content_scope.as_str(),
            "event_type": request.event_type,
            "file": request.file.as_ref().map(|path| path.display().to_string()),
            "session": request.session,
            "exclude_session": (!request.exclude_sessions.is_empty())
                .then_some(&request.exclude_sessions),
            "primary_only": request.primary_only.then_some(true),
            "include_current_session": request.include_current_session.then_some(true),
        },
        "freshness": {
            "mode": freshness_mode,
            "status": metrics.refresh_status,
            "source_count": metrics.refresh_source_count,
        },
        "retrieval": {
            "requested_mode": collection.requested_backend.as_str(),
            "effective_mode": collection.effective_backend.as_str(),
            "semantic_weight": collection.semantic_weight,
            "semantic_status": collection.semantic_status,
            "semantic_fallback_code": semantic_fallback_code,
            "semantic_fallback": semantic_fallback_detail,
            "semantic_diagnostics": semantic_diagnostics,
            "index": "core",
            "generation_id": index.generation_id(),
            "indexed_documents": index.document_count(),
            "phase_attribution": phase_attribution,
        },
        "phase_attribution": phase_attribution,
        "generated_at": generated_at,
        "results": results,
        "result_window": {
            "limit": collection.result_window.limit,
            "returned": results.len(),
            "more_available": collection.result_window.more_available,
        },
        "truncation": {
            "candidate_pool": collection.candidate_pool,
            "candidate_pool_truncated": collection.candidate_pool_truncated,
        },
    })))
}

pub fn semantic_diagnostics_read_model(
    collection: &SearchCollection,
    fallback_code: Option<&str>,
    fallback_detail: Option<&str>,
) -> Option<Value> {
    let mut diagnostics = collection.semantic_diagnostics.clone()?;
    if collection.semantic_fallback.is_none() {
        return Some(diagnostics);
    }
    let Some(object) = diagnostics.as_object_mut() else {
        return Some(diagnostics);
    };
    object.insert(
        "fallback".to_owned(),
        json!({
            "code": fallback_code,
            "detail": fallback_detail,
        }),
    );
    Some(diagnostics)
}

pub fn search_result_json(
    hit: &SearchHit,
    presentation: &SearchPresentation,
    result_scope: &str,
    rank: usize,
    copied_lineage: &Value,
    commands: &SearchResultCommands,
) -> Result<Value> {
    let (snippet, snippet_truncated) = search_snippet(presentation);
    let event = &hit.event;
    let event_id = event.event_id;
    let session_id = event.session_id;
    let item_id = if result_scope == "session" {
        session_id
    } else {
        event_id
    };
    let title = match event.role.as_deref() {
        Some(role) => format!("{} {role} {}", event.provider, event.event_type),
        None => format!("{} {}", event.provider, event.event_type),
    };
    Ok(compact_json(json!({
        "item_id": item_id,
        "result_type": if result_scope == "session" { "session_result" } else { "event" },
        "ctx_event_id": event_id,
        "ctx_session_id": session_id,
        "session_id": session_id,
        "event_id": event_id,
        "event_seq": event.event_sequence,
        "title": title,
        "snippet": snippet,
        "snippet_truncated": snippet_truncated,
        "snippet_max_chars": SEARCH_SNIPPET_MAX_CHARS,
        "rank": rank,
        "retrieval_score": hit.score,
        "result_scope": result_scope,
        "session_importance": (result_scope == "session").then_some(hit.score),
        "more_matches_in_session": (result_scope == "session")
            .then_some(hit.more_matches_in_session),
        "provider": event.provider,
        "provider_key": event.provider_key,
        "source_id": event.source_id,
        "provider_session_id": event.provider_session_id,
        "source_format": event.source_format,
        "parent_ctx_session_id": event.parent_session_id,
        "root_ctx_session_id": event.root_session_id,
        "session_relationship": event.session_relationship,
        "event_copy": event_copy_json(event.event_copy.as_ref()),
        "copied_lineage": copied_lineage,
        "agent_scope": event.agent_scope,
        "timestamp": timestamp_json(event.occurred_at_unix_ms),
        "suggested_next_commands": commands.suggested_next_commands,
        "citations": [{
            "item_id": event_id,
            "target_type": "event",
            "ctx_event_id": event_id,
            "ctx_session_id": session_id,
            "provider": event.provider,
            "session_id": session_id,
            "event_seq": event.event_sequence,
        }],
        "visibility": "local",
    })))
}

pub fn search_snippet(presentation: &SearchPresentation) -> (&str, bool) {
    (
        presentation.snippet.as_str(),
        presentation.snippet_truncated,
    )
}

pub fn phase_attribution(query: Duration) -> Value {
    json!({
        "discovery_seconds": 0.0,
        "writer_open_seconds": 0.0,
        "scan_and_stage_seconds": 0.0,
        "scanner_worker_busy_seconds": 0.0,
        "writer_add_document_seconds": 0.0,
        "certification_seconds": 0.0,
        "index_commit_seconds": 0.0,
        "refresh_total_seconds": 0.0,
        "query_seconds": query.as_secs_f64(),
        "catalog_sources": 0,
        "catalog_source_bytes": 0,
        "cold_sources": 0,
        "appended_sources": 0,
        "replaced_sources": 0,
        "replayed_sources": 0,
        "deleted_sources": 0,
        "scanner_bytes_read": 0,
        "checkpoint_validation_bytes": 0,
        "scanner_workers": 0,
        "complete_records_scanned": 0,
        "retained_records_scanned": 0,
        "rejected_records_scanned": 0,
        "ignored_records_scanned": 0,
        "staged_documents": 0,
    })
}
