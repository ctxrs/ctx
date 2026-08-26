use crate::HistorySemanticQuery;

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn collect_prepared_semantic_query<Query>(
    request: &SearchRequest,
    index: &VerifiedIndex,
    filter: &CompiledSearchFilter,
    requested_backend: SearchBackend,
    normalized_query: NormalizedSearchQuery,
    semantic_query: &mut Query,
    tracker: &mut SearchWorkTracker,
) -> SearchExecutionResult<RankedSearchCollection>
where
    Query: HistorySemanticQuery,
{
    tracker.set_phase(SearchFailurePhase::SemanticRetrieval);
    let queries = normalized_query.texts();
    let mut semantic_query_diagnostics = Vec::with_capacity(queries.len());
    for query in &queries {
        let diagnostics = match semantic_query.prepare_alternative(query) {
            Ok(diagnostics) => diagnostics,
            Err(error) => {
                return semantic_retrieval_failure(
                    request,
                    index,
                    filter,
                    requested_backend,
                    error,
                    semantic_query_diagnostics,
                    tracker,
                )
            }
        };
        semantic_query_diagnostics.push(json!({
            "diagnostics": diagnostics,
        }));
    }
    tracker.record_retrieval_round()?;
    let batch = match semantic_query.candidates(filter, SOURCE_FUSION_CANDIDATES) {
        Ok(batch) => batch,
        Err(error) => {
            return semantic_retrieval_failure(
                request,
                index,
                filter,
                requested_backend,
                error,
                semantic_query_diagnostics,
                tracker,
            )
        }
    };
    finish_prepared_semantic_search(
        request,
        index,
        filter,
        requested_backend,
        &queries,
        batch,
        semantic_query_diagnostics,
        tracker,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn collect_prepared_semantic_search_using<SemanticSearch>(
    request: &SearchRequest,
    index: &VerifiedIndex,
    filter: &CompiledSearchFilter,
    requested_backend: SearchBackend,
    normalized_query: NormalizedSearchQuery,
    mut semantic_search: SemanticSearch,
    tracker: &mut SearchWorkTracker,
) -> SearchExecutionResult<RankedSearchCollection>
where
    SemanticSearch: FnMut(
        &[&str],
        &CompiledSearchFilter,
        usize,
    ) -> std::result::Result<HistorySemanticBatch, HistorySemanticError>,
{
    tracker.set_phase(SearchFailurePhase::SemanticRetrieval);
    let queries = normalized_query.texts();
    tracker.record_retrieval_round()?;
    let batch = match semantic_search(&queries, filter, SOURCE_FUSION_CANDIDATES) {
        Ok(batch) => batch,
        Err(error) => {
            return semantic_retrieval_failure(
                request,
                index,
                filter,
                requested_backend,
                error,
                Vec::new(),
                tracker,
            )
        }
    };
    finish_prepared_semantic_search(
        request,
        index,
        filter,
        requested_backend,
        &queries,
        batch,
        Vec::new(),
        tracker,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn semantic_retrieval_failure(
    request: &SearchRequest,
    index: &VerifiedIndex,
    filter: &CompiledSearchFilter,
    requested_backend: SearchBackend,
    error: HistorySemanticError,
    semantic_query_diagnostics: Vec<Value>,
    tracker: &mut SearchWorkTracker,
) -> SearchExecutionResult<RankedSearchCollection> {
    if requested_backend == SearchBackend::Hybrid {
        lexical_fallback_with_diagnostics(
            request,
            index,
            filter,
            requested_backend,
            error,
            "unavailable",
            semantic_query_diagnostics,
            tracker,
        )
    } else {
        Err(error.into())
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_prepared_semantic_search(
    request: &SearchRequest,
    index: &VerifiedIndex,
    filter: &CompiledSearchFilter,
    requested_backend: SearchBackend,
    queries: &[&str],
    batch: HistorySemanticBatch,
    semantic_query_diagnostics: Vec<Value>,
    tracker: &mut SearchWorkTracker,
) -> SearchExecutionResult<RankedSearchCollection> {
    let HistorySemanticBatch {
        candidates,
        diagnostics: semantic_scan_diagnostics,
    } = batch;
    if candidates.len() > SOURCE_FUSION_CANDIDATES {
        return Err(anyhow!(
            "semantic backend returned {} candidates, maximum is {SOURCE_FUSION_CANDIDATES}",
            candidates.len()
        )
        .into());
    }
    // The semantic backend owns strict-max per-event deduplication and final
    // rank order. Re-sorting here would be a second product semantics path.
    let semantic_candidates = candidates;
    let semantic_candidates_truncated = semantic_candidates.len() == SOURCE_FUSION_CANDIDATES;
    let semantic_diagnostics = json!({
        "query_count": queries.len(),
        "queries": semantic_query_diagnostics,
        "scan": semantic_scan_diagnostics,
    });

    let (candidates, lexical_diagnostics, candidate_pool_truncated) =
        if requested_backend == SearchBackend::Semantic {
            (semantic_candidates, None, semantic_candidates_truncated)
        } else {
            tracker.set_phase(SearchFailurePhase::IndexQueryDecode);
            let lexical_batch = record_lexical_batch(
                tracker,
                index.search_event_candidates_any_with_compiled_filter_batch_diagnosed(
                    &queries,
                    filter,
                    SOURCE_FUSION_CANDIDATES,
                ),
            )?;
            let lexical_diagnostics = lexical_diagnostics(&lexical_batch);
            let lexical_candidates_truncated = !lexical_batch.candidate_set_exhaustive;
            let lexical_candidates = lexical_batch
                .candidates
                .into_iter()
                .map(Into::into)
                .collect();
            (
                fuse_source_candidates(
                    lexical_candidates,
                    semantic_candidates,
                    request.semantic_weight,
                ),
                Some(lexical_diagnostics),
                lexical_candidates_truncated || semantic_candidates_truncated,
            )
        };
    let candidate_pool = candidates.len();
    tracker.set_phase(SearchFailurePhase::ResultProjection);
    let (result_window, diversification) = shape_search_candidates_using(
        &candidates,
        request.limit,
        dense_search(request),
        DiversificationCompleteness::BackendUnknown,
        |coordinates| index.session_grouping_claims_for_search(coordinates),
    )?;
    Ok(SearchCollection {
        result_window,
        candidate_pool,
        candidate_pool_truncated,
        lexical_diagnostics,
        diversification,
        requested_backend,
        effective_backend: requested_backend,
        semantic_weight: if requested_backend == SearchBackend::Semantic {
            1.0
        } else {
            request.semantic_weight
        },
        semantic_status: "ready",
        semantic_fallback: None,
        semantic_diagnostics: Some(semantic_diagnostics),
        work: tracker.work,
        stop_reason: Some(SearchStopReason::FixedPool),
    })
}
