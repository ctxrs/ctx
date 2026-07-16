use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use ctx_history_core::{utc_now, ContextTruncation, EventType, HistoryRecord};
use ctx_history_store::{
    EventCandidateAgentScope, EventCandidateExcludedSession, EventCandidateFileScope,
    EventCandidateScope, FileSearchCandidateScope, FileTouchScope, Store,
};
use ctx_protocol::{
    search_analyzed_token_count, SearchClause, SearchEffectiveBackend, SearchExecutionLimits,
    SearchQuery, SearchQueryV1, SearchRequestEnvelope, SearchSemanticPolicy,
    SearchSemanticReadiness, SEARCH_MAX_CANDIDATES_PER_POSITIVE_SEED,
    SEARCH_POSITIVE_TEXT_RULE_VERSION, SEARCH_QUERY_VERSION,
};
use uuid::Uuid;

use crate::filters::{
    event_hit_matches_filters, file_filter_scope, has_filters, has_history_source_filter,
};
use crate::model::CandidateSearch;
use crate::packet::{
    empty_search_packet, pagination, SearchExecutionDiagnostics, SearchPacket, SearchPacketResult,
    SearchResultScope, SemanticEventHit, SEARCH_PACKET_SCHEMA_VERSION,
};
use crate::query::{
    clause_matches_text, composed_search_terms, normalized_options, query_terms, PacketOptions,
    Result, SearchFilters, SearchResultMode, FILTERED_SEARCH_MAX_PAGES, FILTERED_SEARCH_PAGE_SIZE,
    LARGE_EVENT_CORPUS_THRESHOLD,
};
use crate::ranking::{candidate_for_bounded_file, candidate_for_bounded_record, ranked_candidates};
use crate::results::{
    candidate_search_result, compare_search_results, event_reason, event_search_result,
    merge_search_result, normalize_search_result_ranks, push_candidate_results, push_unique_why,
    search_result_merge_key, session_importance,
};

const STRUCTURED_SEARCH_TIMEOUT: Duration = Duration::from_secs(10);
const RRF_K: f64 = 60.0;

#[derive(Debug, Default)]
struct StructuredCandidate {
    reciprocal_rank_score: f64,
    seed_matches: BTreeSet<usize>,
}

#[derive(Debug, Default)]
struct StructuredRecordCandidate {
    reciprocal_rank_score: f64,
    seed_matches: BTreeSet<usize>,
    updated_at_ms: i64,
    created_at_ms: i64,
}

fn event_candidate_scope(filters: &SearchFilters) -> EventCandidateScope {
    let agent_scope = if filters.primary_only {
        EventCandidateAgentScope::PrimaryOnly
    } else if filters.include_subagents {
        EventCandidateAgentScope::Any
    } else {
        EventCandidateAgentScope::PrimaryOrUnclassified
    };
    EventCandidateScope {
        session_id: filters.session,
        provider: filters.provider,
        history_source: filters.history_source.clone(),
        provider_key: filters.provider_key.clone(),
        source_id: filters.source_id.clone(),
        source_format: filters.source_format.clone(),
        workspace_contains: filters.repo.clone(),
        since: filters.since,
        event_type: filters.event_type,
        agent_scope,
        excluded_session: filters.exclude_provider_session.as_ref().map(|excluded| {
            EventCandidateExcludedSession {
                provider: excluded.provider,
                provider_session_id: excluded.provider_session_id.clone(),
                session_id: excluded.session_id,
            }
        }),
        file: filters
            .file
            .as_deref()
            .and_then(EventCandidateFileScope::new),
    }
    .normalized()
}

fn file_candidate_scope(filters: &SearchFilters) -> FileSearchCandidateScope {
    FileSearchCandidateScope {
        provider: filters.provider,
        history_source: filters.history_source.clone(),
        provider_key: filters.provider_key.clone(),
        source_id: filters.source_id.clone(),
        source_format: filters.source_format.clone(),
    }
}

/// Record FTS only carries title/body/tag plus record timestamps/workspace.
/// Do not spend the bounded candidate share on records when satisfying the
/// active filter would require session, event, source, or file metadata.
fn bounded_record_branch_is_eligible(filters: &SearchFilters) -> bool {
    filters.session.is_none()
        && filters.provider.is_none()
        && filters.history_source.is_none()
        && filters.provider_key.is_none()
        && filters.source_id.is_none()
        && filters.source_format.is_none()
        && filters.event_type.is_none()
        && filters.file.is_none()
        && filters.exclude_provider_session.is_none()
        && !filters.primary_only
}

pub fn search_packet_query(
    store: &Store,
    query: &SearchQuery,
    options: &PacketOptions,
) -> Result<SearchPacket> {
    let mut envelope = SearchRequestEnvelope::new(query.clone());
    envelope.semantic_policy = SearchSemanticPolicy::Disabled;
    search_packet_envelope(store, &envelope, options)
}

/// Execute one validated shared envelope. Semantic input is already a bounded,
/// pre-ranked identity list; this path never indexes, downloads, or starts a
/// semantic backend.
pub fn search_packet_envelope(
    store: &Store,
    envelope: &SearchRequestEnvelope,
    options: &PacketOptions,
) -> Result<SearchPacket> {
    let envelope = envelope.clone().canonicalized()?;
    let semantic_required = envelope.query.semantic_clause().is_some();
    let automatic_rerank_text = envelope.query.automatic_rerank_text();
    let automatic_rerank_requested = !semantic_required
        && envelope.semantic_policy == SearchSemanticPolicy::AutomaticRerank
        && automatic_rerank_text.is_some();
    let semantic = envelope.semantic.as_ref();
    let readiness = semantic.map_or(SearchSemanticReadiness::Unavailable, |input| input.readiness);
    if semantic_required && readiness != SearchSemanticReadiness::Ready {
        return Err(crate::query::SearchError::SemanticNotReady { readiness });
    }

    let mut lexical_query = envelope.query.clone();
    lexical_query
        .any
        .retain(|clause| !matches!(clause, SearchClause::Semantic(_)));
    let has_lexical_positive = lexical_query.positive_clause_count() > 0;
    let mut packet = if has_lexical_positive {
        search_packet_query_lexical(
            store,
            &lexical_query,
            options,
            envelope.requested_limits.as_ref(),
        )?
    } else {
        let options = normalized_options(options);
        empty_search_packet(&envelope.query.canonical_text(), &options)
    };
    if !has_lexical_positive {
        packet.query_execution.resolved = SearchExecutionLimits::resolved(
            envelope.requested_limits.as_ref(),
            options.limit,
            STRUCTURED_SEARCH_TIMEOUT.as_millis() as u64,
        );
    }
    packet.query = envelope.query.canonical_text();
    packet.query_spec = Some(envelope.query.clone());
    packet.query_execution.requested_result_limit = options.limit;
    packet.query_execution.result_limit = packet.query_execution.resolved.results;
    packet.query_execution.semantic.attempted =
        semantic_required || automatic_rerank_requested;
    packet.query_execution.semantic.required = semantic_required;
    packet.query_execution.semantic.readiness = readiness;
    packet.query_execution.semantic.backend = semantic.and_then(|input| input.backend.clone());
    packet.query_execution.semantic.positive_text_rule_version =
        SEARCH_POSITIVE_TEXT_RULE_VERSION.to_owned();
    packet.query_execution.semantic.coverage.requested_candidates =
        semantic.map_or(0, |input| input.candidates.len());
    packet.query_execution.semantic.coverage.indexed_documents =
        semantic.and_then(|input| input.indexed_documents);
    packet.query_execution.semantic.coverage.searchable_documents =
        semantic.and_then(|input| input.searchable_documents);

    let semantic_enabled = readiness == SearchSemanticReadiness::Ready
        && (semantic_required || automatic_rerank_requested);
    if semantic_enabled {
        let Some(input) = semantic else {
            return Err(crate::query::SearchError::SemanticNotReady { readiness });
        };
        let semantic_allocation = packet
            .query_execution
            .resolved
            .candidates_per_positive_seed
            .min(
                packet
                    .query_execution
                    .resolved
                    .candidate_rows
                    .saturating_sub(packet.query_execution.consumed.candidate_rows),
            )
            .min(
                packet
                    .query_execution
                    .resolved
                    .retained_candidate_ids
                    .saturating_sub(
                        packet
                            .query_execution
                            .consumed
                            .retained_candidate_ids,
                    ),
            );
        let semantic_ids = input
            .candidates
            .iter()
            .take(semantic_allocation)
            .map(|candidate| {
                Uuid::parse_str(&candidate.ctx_event_id).map_err(|_| {
                    crate::query::SearchError::InvalidSemanticCandidateId(
                        candidate.ctx_event_id.clone(),
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?;
        packet.query_execution.semantic.coverage.eligible_candidates = semantic_ids.len();
        packet.query_execution.semantic.coverage.truncated =
            input.candidates.len() > semantic_ids.len();
        packet.query_execution.consumed.candidate_rows = packet
            .query_execution
            .consumed
            .candidate_rows
            .saturating_add(semantic_ids.len())
            .min(packet.query_execution.resolved.candidate_rows);
        packet.query_execution.consumed.retained_candidate_ids = packet
            .query_execution
            .consumed
            .retained_candidate_ids
            .saturating_add(semantic_ids.len())
            .min(packet.query_execution.resolved.retained_candidate_ids);
        if semantic_required {
            merge_explicit_semantic_candidates(
                store,
                &envelope.query,
                options,
                &semantic_ids,
                &mut packet,
            )?;
        } else {
            rerank_automatic_semantic_candidates(&semantic_ids, &mut packet);
        }
    }
    packet.query_execution.semantic.effective_backend = match (
        has_lexical_positive,
        packet.query_execution.semantic.coverage.used_candidates > 0,
    ) {
        (true, true) => SearchEffectiveBackend::Hybrid,
        (false, true) => SearchEffectiveBackend::Semantic,
        (true, false) => SearchEffectiveBackend::Lexical,
        (false, false) => SearchEffectiveBackend::None,
    };
    packet.query_execution.semantic.completeness = if !packet.query_execution.semantic.attempted {
        ctx_protocol::SearchSemanticCompleteness::NotAttempted
    } else if readiness == SearchSemanticReadiness::Ready {
        if packet.query_execution.semantic.coverage.truncated
            || semantic.is_none_or(|input| !input.coverage_complete)
        {
            ctx_protocol::SearchSemanticCompleteness::Partial
        } else {
            ctx_protocol::SearchSemanticCompleteness::Complete
        }
    } else {
        ctx_protocol::SearchSemanticCompleteness::Skipped
    };
    packet.query_execution.semantic.skip_reason = match readiness {
        _ if !semantic_required
            && envelope.semantic_policy == SearchSemanticPolicy::AutomaticRerank
            && automatic_rerank_text.is_none() =>
        {
            Some(ctx_protocol::SearchSemanticSkipReason::QueryShapeNotEligible)
        }
        _ if !packet.query_execution.semantic.attempted => {
            Some(ctx_protocol::SearchSemanticSkipReason::Disabled)
        }
        SearchSemanticReadiness::Ready
            if packet.query_execution.semantic.attempted
                && packet.results.is_empty()
                && !semantic_required =>
        {
            Some(ctx_protocol::SearchSemanticSkipReason::NoLexicalCandidates)
        }
        SearchSemanticReadiness::NotReady => {
            Some(ctx_protocol::SearchSemanticSkipReason::NotReady)
        }
        SearchSemanticReadiness::Unsupported => {
            Some(ctx_protocol::SearchSemanticSkipReason::Unsupported)
        }
        SearchSemanticReadiness::Unavailable => {
            Some(ctx_protocol::SearchSemanticSkipReason::Unavailable)
        }
        _ => None,
    };
    finalize_structured_packet(&mut packet)?;
    Ok(packet)
}

fn rerank_automatic_semantic_candidates(
    semantic_ids: &[Uuid],
    packet: &mut SearchPacket,
) {
    let semantic_ranks = semantic_ids
        .iter()
        .enumerate()
        .map(|(index, id)| (*id, index.saturating_add(1)))
        .collect::<BTreeMap<_, _>>();
    let mut used = 0usize;
    let mut ranked = packet
        .results
        .drain(..)
        .enumerate()
        .map(|(lexical_index, mut result)| {
            let lexical = 1.0 / (RRF_K + lexical_index.saturating_add(1) as f64);
            let semantic = result
                .event_id
                .and_then(|id| semantic_ranks.get(&id).copied())
                .map_or(0.0, |rank| {
                    used = used.saturating_add(1);
                    1.0 / (RRF_K + rank as f64)
                });
            result.rank = (lexical + semantic) as f32;
            (result, lexical_index)
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|(left, left_index), (right, right_index)| {
        right
            .rank
            .total_cmp(&left.rank)
            .then_with(|| left_index.cmp(right_index))
            .then_with(|| left.record_id.cmp(&right.record_id))
    });
    packet.results = ranked.into_iter().map(|(result, _)| result).collect();
    normalize_search_result_ranks(&mut packet.results);
    packet.query_execution.semantic.coverage.used_candidates = used;
}

fn merge_explicit_semantic_candidates(
    store: &Store,
    query: &SearchQuery,
    options: &PacketOptions,
    semantic_ids: &[Uuid],
    packet: &mut SearchPacket,
) -> Result<()> {
    let resolved = packet.query_execution.resolved.clone();
    // This store seam must use the canonical lookup first and then exact-ID
    // canonical-row fallback for missing legacy lookup entries. Both reads
    // must apply provider-publication visibility fences and the supplied byte
    // limits; it must never trigger a projection rebuild.
    let hits = store.semantic_event_hits_by_ids_bounded_visible(
        semantic_ids,
        resolved.residual_rows,
        resolved.verification_bytes,
        resolved.verification_lookup_bytes,
        resolved.hydrated_rows,
        resolved.hydration_input_bytes,
        resolved.hydration_input_bytes_per_event,
    )?;
    let file_scope = file_filter_scope(store, &options.filters)?;
    let mut semantic_results = Vec::new();
    for (rank, hit) in hits.into_iter().enumerate() {
        if !consume_hydration_input(&mut packet.query_execution, hit.preview.len()) {
            continue;
        }
        let verification_bytes = hit.preview.len();
        if verification_bytes > resolved.verification_lookup_bytes
            || packet
                .query_execution
                .consumed
                .verification_bytes
                .saturating_add(verification_bytes)
                > resolved.verification_bytes
        {
            packet.query_execution.verification_dropped = packet
                .query_execution
                .verification_dropped
                .saturating_add(1);
            push_truncation_reason(
                &mut packet.query_execution,
                "verification_bytes_budget",
            );
            continue;
        }
        packet.query_execution.consumed.residual_rows = packet
            .query_execution
            .consumed
            .residual_rows
            .saturating_add(1)
            .min(resolved.residual_rows);
        packet.query_execution.consumed.verification_bytes = packet
            .query_execution
            .consumed
            .verification_bytes
            .saturating_add(verification_bytes);
        packet
            .query_execution
            .consumed
            .largest_verification_lookup_bytes = packet
            .query_execution
            .consumed
            .largest_verification_lookup_bytes
            .max(verification_bytes);
        if !query
            .must
            .iter()
            .all(|clause| clause_matches_text(clause, &hit.preview))
            || query
                .must_not
                .iter()
                .any(|clause| clause_matches_text(clause, &hit.preview))
            || !event_hit_matches_filters(&hit, &options.filters, file_scope.as_ref())
        {
            packet.query_execution.verification_dropped = packet
                .query_execution
                .verification_dropped
                .saturating_add(1);
            continue;
        }
        let mut result = event_search_result(
            &hit,
            &query.canonical_positive_text(),
            options.snippet_chars,
        );
        result.rank = reciprocal_rank(rank.saturating_add(1));
        push_unique_why(&mut result.why_matched, "semantic_similarity".to_owned());
        semantic_results.push(result);
    }
    let used = semantic_results.len();
    let mut combined = Vec::<SearchPacketResult>::new();
    let mut index_by_key = BTreeMap::<Uuid, usize>::new();
    for (rank, mut result) in packet
        .results
        .drain(..)
        .chain(semantic_results)
        .enumerate()
    {
        let key = search_result_merge_key(&result, options.result_mode);
        let contribution = reciprocal_rank(rank.saturating_add(1));
        if let Some(index) = index_by_key.get(&key).copied() {
            combined[index].rank += contribution;
            merge_search_result(&mut combined[index], result);
        } else {
            result.rank = contribution;
            index_by_key.insert(key, combined.len());
            combined.push(result);
        }
    }
    combined.sort_by(compare_search_results);
    if combined.len() > resolved.results {
        combined.truncate(resolved.results);
        push_truncation_reason(&mut packet.query_execution, "result_limit");
    }
    normalize_search_result_ranks(&mut combined);
    packet.results = combined;
    packet.query_execution.semantic.coverage.used_candidates = used;
    Ok(())
}

fn finalize_structured_packet(packet: &mut SearchPacket) -> Result<()> {
    let text_budget = packet.query_execution.resolved.returned_text_bytes;
    let mut retained = Vec::with_capacity(packet.results.len());
    let mut text_bytes = 0usize;
    for result in packet.results.drain(..) {
        let bytes = result.title.len().saturating_add(result.snippet.len());
        if text_bytes.saturating_add(bytes) > text_budget {
            push_truncation_reason(&mut packet.query_execution, "returned_text_bytes");
            continue;
        }
        text_bytes = text_bytes.saturating_add(bytes);
        retained.push(result);
    }
    packet.results = retained;
    packet.query_execution.consumed.returned_results = packet.results.len();
    packet.query_execution.consumed.returned_text_bytes = text_bytes;

    loop {
        packet.query_execution.truncated = !packet.query_execution.truncation_reasons.is_empty();
        if packet.query_execution.truncated {
            packet.truncation.truncated = true;
            packet.truncation.omitted_results = packet.truncation.omitted_results.max(1);
            if packet.truncation.reason.is_none() {
                packet.truncation.reason = packet
                    .query_execution
                    .truncation_reasons
                    .first()
                    .cloned();
            }
        }
        let mut bytes = 0usize;
        for _ in 0..8 {
            packet.query_execution.consumed.serialized_response_bytes = bytes;
            let next = serde_json::to_vec(packet)?.len();
            if next == bytes {
                break;
            }
            bytes = next;
        }
        packet.query_execution.consumed.serialized_response_bytes = bytes;
        let exact = serde_json::to_vec(packet)?.len();
        packet.query_execution.consumed.serialized_response_bytes = exact;
        let bytes = serde_json::to_vec(packet)?.len();
        packet.query_execution.consumed.serialized_response_bytes = bytes;
        if bytes <= packet.query_execution.resolved.serialized_response_bytes {
            break;
        }
        let Some(removed) = packet.results.pop() else {
            return Err(crate::query::SearchError::ResponseEnvelopeTooLarge {
                maximum: packet.query_execution.resolved.serialized_response_bytes,
            });
        };
        packet.query_execution.consumed.returned_text_bytes = packet
            .query_execution
            .consumed
            .returned_text_bytes
            .saturating_sub(removed.title.len().saturating_add(removed.snippet.len()));
        packet.query_execution.consumed.returned_results = packet.results.len();
        push_truncation_reason(&mut packet.query_execution, "serialized_response_bytes");
    }
    Ok(())
}

fn search_packet_query_lexical(
    store: &Store,
    query: &SearchQueryV1,
    options: &PacketOptions,
    requested_limits: Option<&SearchExecutionLimits>,
) -> Result<SearchPacket> {
    let started = Instant::now();
    let query = query.clone().canonicalized()?;
    let options = normalized_options(options);
    let resolved = SearchExecutionLimits::resolved(
        requested_limits,
        options.limit,
        STRUCTURED_SEARCH_TIMEOUT.as_millis() as u64,
    );
    if let Some(provider) = options.filters.provider {
        if !store.has_provider_data(provider)? {
            let mut packet = empty_search_packet(&render_structured_query(&query), &options);
            packet.query_spec = Some(query.clone());
            packet.query_execution.query_version = SEARCH_QUERY_VERSION.to_owned();
            packet.query_execution.consumed.query_bytes =
                query.clauses().map(|clause| clause.value().len()).sum();
            packet.query_execution.consumed.clauses = query.clause_count();
            packet.query_execution.consumed.analyzed_tokens = query
                .clauses()
                .map(|clause| search_analyzed_token_count(clause.value()))
                .sum();
            packet.query_execution.resolved = resolved;
            return Ok(packet);
        }
    }
    let candidate_scope = event_candidate_scope(&options.filters);

    // `any` clauses are independent candidate branches. A must-only query is
    // one conjunctive branch rather than one branch per required clause.
    let seeds = if query.any.is_empty() {
        &query.must[..1]
    } else {
        query.any.as_slice()
    };
    let required = if query.any.is_empty() {
        &query.must[1..]
    } else {
        query.must.as_slice()
    };
    let has_selective_record_filter = options.filters.session.is_some()
        || options.filters.provider.is_some()
        || options.filters.history_source.is_some()
        || options.filters.provider_key.is_some()
        || options.filters.source_id.is_some()
        || options.filters.source_format.is_some()
        || options.filters.repo.is_some()
        || options.filters.since.is_some()
        || options.filters.event_type.is_some()
        || options.filters.file.is_some();
    let base_per_clause =
        if has_selective_record_filter || options.result_mode == SearchResultMode::Sessions {
            ctx_history_store::MAX_EVENT_CANDIDATES_PER_CLAUSE
        } else {
            options.limit.saturating_mul(8).max(256)
        }
        .min(ctx_history_store::MAX_EVENT_CANDIDATES_PER_CLAUSE);
    // Reserve half of the shared work budget for exact residual verification
    // after bounded candidate retrieval. Candidate branches already intersect
    // must clauses and safe must_not clauses in FTS before the per-branch cap.
    let row_bounded_per_clause = (resolved.candidate_rows / seeds.len().max(1) / 2)
        .saturating_sub(1)
        .max(1);
    let id_bounded_per_clause =
        (resolved.retained_candidate_ids / seeds.len().max(1)).max(1);
    let per_clause_budget = base_per_clause
        .min(SEARCH_MAX_CANDIDATES_PER_POSITIVE_SEED)
        .min(row_bounded_per_clause)
        .min(id_bounded_per_clause);
    let search_record_candidates = bounded_record_branch_is_eligible(&options.filters);
    let event_clause_budget = if search_record_candidates {
        per_clause_budget.saturating_mul(7).div_ceil(8).max(1)
    } else {
        per_clause_budget
    };
    let mut diagnostics = SearchExecutionDiagnostics {
        query_version: SEARCH_QUERY_VERSION.to_owned(),
        candidate_strategy: "fts5_rank_bounded".to_owned(),
        resolved: resolved.clone(),
        consumed: ctx_protocol::SearchExecutionConsumption {
            query_bytes: query.clauses().map(|clause| clause.value().len()).sum(),
            clauses: query.clause_count(),
            analyzed_tokens: query
                .clauses()
                .map(|clause| search_analyzed_token_count(clause.value()))
                .sum(),
            ..ctx_protocol::SearchExecutionConsumption::default()
        },
        per_branch_candidate_rows: per_clause_budget,
        ..SearchExecutionDiagnostics::default()
    };
    let mut candidates = BTreeMap::<Uuid, StructuredCandidate>::new();
    let mut record_candidates = BTreeMap::<Uuid, StructuredRecordCandidate>::new();

    for (seed_index, seed) in seeds.iter().enumerate() {
        let remaining = STRUCTURED_SEARCH_TIMEOUT.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(structured_timeout_error(started, &diagnostics));
        }
        let batch = store.search_event_candidates_for_branch_scoped(
            seed,
            required,
            &query.must_not,
            &candidate_scope,
            event_clause_budget,
            remaining,
        )?;
        let event_candidate_work = batch.examined.min(per_clause_budget);
        diagnostics.clauses_executed = diagnostics.clauses_executed.saturating_add(1);
        diagnostics.consumed.candidate_rows = diagnostics
            .consumed
            .candidate_rows
            .saturating_add(batch.examined);
        if batch.truncated {
            diagnostics.candidate_budget_exhausted = true;
            push_truncation_reason(&mut diagnostics, "per_clause_candidate_budget");
        }
        if batch.timed_out {
            return Err(structured_timeout_error(started, &diagnostics));
        }
        let mut clause_candidates = batch.candidates;
        clause_candidates.sort_by(|left, right| {
            left.rank
                .total_cmp(&right.rank)
                .then_with(|| left.event_id.cmp(&right.event_id))
        });
        let mut previous_rank = None::<f64>;
        let mut tied_rank_index = 0usize;
        for (position, candidate) in clause_candidates.into_iter().enumerate() {
            if previous_rank.is_none_or(|rank| rank.total_cmp(&candidate.rank) != Ordering::Equal) {
                tied_rank_index = position;
                previous_rank = Some(candidate.rank);
            }
            let rank_index = tied_rank_index;
            if let Some(existing) = candidates.get_mut(&candidate.event_id) {
                if existing.seed_matches.insert(seed_index) {
                    existing.reciprocal_rank_score +=
                        1.0 / (RRF_K + rank_index.saturating_add(1) as f64);
                }
                continue;
            }
            if candidates.len() >= resolved.retained_candidate_ids {
                diagnostics.candidate_budget_exhausted = true;
                push_truncation_reason(&mut diagnostics, "retained_candidate_ids_budget");
                continue;
            }
            candidates.insert(
                candidate.event_id,
                StructuredCandidate {
                    reciprocal_rank_score: 1.0 / (RRF_K + rank_index.saturating_add(1) as f64),
                    seed_matches: BTreeSet::from([seed_index]),
                },
            );
        }

        if !search_record_candidates {
            continue;
        }
        let remaining = STRUCTURED_SEARCH_TIMEOUT.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(structured_timeout_error(started, &diagnostics));
        }
        let record_query_budget = per_clause_budget
            .saturating_sub(event_candidate_work)
            .max(1);
        let batch = store.search_record_candidates_for_branch(
            seed,
            required,
            &query.must_not,
            record_query_budget,
            remaining,
        )?;
        diagnostics.consumed.candidate_rows = diagnostics
            .consumed
            .candidate_rows
            .saturating_add(batch.examined);
        if batch.truncated {
            diagnostics.candidate_budget_exhausted = true;
            push_truncation_reason(&mut diagnostics, "per_clause_candidate_budget");
        }
        if batch.timed_out {
            return Err(structured_timeout_error(started, &diagnostics));
        }
        let mut clause_candidates = batch.candidates;
        clause_candidates.sort_by(|left, right| {
            left.rank
                .total_cmp(&right.rank)
                .then_with(|| right.updated_at_ms.cmp(&left.updated_at_ms))
                .then_with(|| right.created_at_ms.cmp(&left.created_at_ms))
                .then_with(|| left.record_id.cmp(&right.record_id))
        });
        let mut previous_rank = None::<f64>;
        let mut tied_rank_index = 0usize;
        for (position, candidate) in clause_candidates.into_iter().enumerate() {
            if previous_rank.is_none_or(|rank| rank.total_cmp(&candidate.rank) != Ordering::Equal) {
                tied_rank_index = position;
                previous_rank = Some(candidate.rank);
            }
            let rank_index = tied_rank_index;
            if let Some(existing) = record_candidates.get_mut(&candidate.record_id) {
                if existing.seed_matches.insert(seed_index) {
                    existing.reciprocal_rank_score +=
                        1.0 / (RRF_K + rank_index.saturating_add(1) as f64);
                }
                continue;
            }
            if candidates.len().saturating_add(record_candidates.len())
                >= resolved.retained_candidate_ids
            {
                diagnostics.candidate_budget_exhausted = true;
                push_truncation_reason(&mut diagnostics, "retained_candidate_ids_budget");
                continue;
            }
            record_candidates.insert(
                candidate.record_id,
                StructuredRecordCandidate {
                    reciprocal_rank_score: 1.0 / (RRF_K + rank_index.saturating_add(1) as f64),
                    seed_matches: BTreeSet::from([seed_index]),
                    updated_at_ms: candidate.updated_at_ms,
                    created_at_ms: candidate.created_at_ms,
                },
            );
        }
    }

    diagnostics.consumed.retained_candidate_ids =
        candidates.len().saturating_add(record_candidates.len());
    let verification_cost = query.clause_count().max(1);
    let remaining_work = resolved
        .candidate_rows
        .saturating_sub(diagnostics.consumed.candidate_rows);
    let verification_budget = (remaining_work / verification_cost).min(resolved.residual_rows);
    let mut candidate_ids = candidates
        .iter()
        .map(|(event_id, candidate)| (*event_id, candidate.reciprocal_rank_score))
        .collect::<Vec<_>>();
    candidate_ids.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    if candidate_ids.len() > verification_budget {
        candidate_ids.truncate(verification_budget);
        diagnostics.candidate_budget_exhausted = true;
        push_truncation_reason(&mut diagnostics, "candidate_rows_budget");
    }
    diagnostics.consumed.candidate_rows = diagnostics
        .consumed
        .candidate_rows
        .saturating_add(candidate_ids.len().saturating_mul(verification_cost));
    let record_verification_budget = verification_budget.saturating_sub(candidate_ids.len());
    let candidate_ids = candidate_ids
        .into_iter()
        .map(|(event_id, _)| event_id)
        .collect::<Vec<_>>();
    // Store integration must first use event_search_lookup, then hydrate only
    // missing legacy IDs from canonical event rows under the same provider-
    // publication visibility predicate. Both paths share these hard bounds.
    let previews = store.event_search_previews_by_ids_bounded_visible(
        &candidate_ids,
        resolved.residual_rows,
        resolved.verification_bytes,
        resolved.verification_lookup_bytes,
    )?;
    ensure_structured_deadline(started, &diagnostics)?;
    let has_literal = query
        .clauses()
        .any(|clause| matches!(clause, SearchClause::Literal(_)));
    let mut scored_ids = previews
        .into_iter()
        .filter(|preview| {
            let bytes = preview.preview.len();
            if bytes > resolved.verification_lookup_bytes
                || diagnostics
                    .consumed
                    .verification_bytes
                    .saturating_add(bytes)
                    > resolved.verification_bytes
            {
                diagnostics.verification_dropped =
                    diagnostics.verification_dropped.saturating_add(1);
                push_truncation_reason(&mut diagnostics, "verification_bytes_budget");
                return false;
            }
            diagnostics.consumed.residual_rows =
                diagnostics.consumed.residual_rows.saturating_add(1);
            diagnostics.consumed.verification_bytes = diagnostics
                .consumed
                .verification_bytes
                .saturating_add(bytes);
            diagnostics.consumed.largest_verification_lookup_bytes = diagnostics
                .consumed
                .largest_verification_lookup_bytes
                .max(bytes);
            true
        })
        .filter_map(|preview| {
            let candidate = candidates.get(&preview.event_id)?;
            let matches_any = if query.any.is_empty() {
                true
            } else {
                candidate.seed_matches.iter().any(|seed_index| {
                    query
                        .any
                        .get(*seed_index)
                        .is_some_and(|clause| clause_matches_text(clause, &preview.preview))
                })
            };
            let matches_must = query
                .must
                .iter()
                .all(|clause| clause_matches_text(clause, &preview.preview));
            let matches_must_not = query
                .must_not
                .iter()
                .any(|clause| clause_matches_text(clause, &preview.preview));
            if !matches_any || !matches_must || matches_must_not {
                if has_literal {
                    diagnostics.verification_dropped =
                        diagnostics.verification_dropped.saturating_add(1);
                }
                return None;
            }
            // EventSearchHit historically treats a lower score as better.
            Some((preview.event_id, -candidate.reciprocal_rank_score))
        })
        .collect::<Vec<_>>();
    scored_ids.sort_by(|left, right| {
        left.1
            .partial_cmp(&right.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    let full_hydration_budget = (if options.result_mode == SearchResultMode::Sessions {
        event_clause_budget
    } else {
        options.limit.saturating_mul(32).clamp(512, 2_048)
    })
    .min(resolved.hydrated_rows);
    if scored_ids.len() > full_hydration_budget {
        scored_ids.truncate(full_hydration_budget);
        diagnostics.candidate_budget_exhausted = true;
        push_truncation_reason(&mut diagnostics, "compact_hydration_budget");
    }
    let mut hits = store.event_search_hits_by_scores_compact_bounded_visible(
        &scored_ids,
        resolved.hydrated_rows,
        resolved.hydration_input_bytes,
        resolved.hydration_input_bytes_per_event,
    )?;
    ensure_structured_deadline(started, &diagnostics)?;
    hits.retain(|hit| consume_hydration_input(&mut diagnostics, hit.preview.len()));
    let display_query = render_structured_query(&query);
    let snippet_query = query
        .any
        .iter()
        .chain(query.must.iter())
        .map(SearchClause::value)
        .collect::<Vec<_>>()
        .join(" ");
    let mut verified_hits = Vec::new();
    for mut hit in hits.drain(..) {
        if !event_hit_matches_filters(&hit, &options.filters, None) {
            diagnostics.filter_verification_dropped =
                diagnostics.filter_verification_dropped.saturating_add(1);
            continue;
        }
        if matches!(hit.event_type, EventType::Message | EventType::Summary) {
            hit.score -= hit.score.abs() * 0.15;
        }
        verified_hits.push(hit);
    }
    verified_hits.sort_by(|left, right| {
        left.score
            .partial_cmp(&right.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| right.occurred_at.cmp(&left.occurred_at))
            .then_with(|| right.seq.cmp(&left.seq))
            .then_with(|| left.event_id.cmp(&right.event_id))
    });

    let mut record_ids = record_candidates
        .iter()
        .map(|(record_id, candidate)| {
            (
                *record_id,
                candidate.reciprocal_rank_score,
                candidate.updated_at_ms,
                candidate.created_at_ms,
            )
        })
        .collect::<Vec<_>>();
    record_ids.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| right.3.cmp(&left.3))
            .then_with(|| left.0.cmp(&right.0))
    });
    let record_budget = record_verification_budget.min(512);
    if record_ids.len() > record_budget {
        record_ids.truncate(record_budget);
        diagnostics.candidate_budget_exhausted = true;
        push_truncation_reason(&mut diagnostics, "candidate_rows_budget");
    }
    diagnostics.consumed.candidate_rows = diagnostics
        .consumed
        .candidate_rows
        .saturating_add(record_ids.len().saturating_mul(verification_cost));
    let scoring_terms = query_terms(&snippet_query);
    let mut record_results = Vec::new();
    for (record_id, reciprocal_rank_score, _, _) in record_ids {
        ensure_structured_deadline(started, &diagnostics)?;
        let document = store.get_record_search_document(record_id)?;
        ensure_structured_deadline(started, &diagnostics)?;
        let Some(document) = document else {
            continue;
        };
        let record = document.into_history_record();
        let matches_query = record_matches_structured_query(&record, &query);
        ensure_structured_deadline(started, &diagnostics)?;
        if !matches_query {
            if has_literal {
                diagnostics.verification_dropped =
                    diagnostics.verification_dropped.saturating_add(1);
            }
            continue;
        }
        let candidate = candidate_for_bounded_record(record, &scoring_terms, &options.filters);
        ensure_structured_deadline(started, &diagnostics)?;
        let Some(mut candidate) = candidate else {
            diagnostics.filter_verification_dropped =
                diagnostics.filter_verification_dropped.saturating_add(1);
            continue;
        };
        candidate.score = reciprocal_rank_score as f32 + (candidate.score * 0.000_001);
        let mut result = candidate_search_result(&candidate, &snippet_query, &options);
        result.timestamp = Some(candidate.record.updated_at);
        ensure_structured_deadline(started, &diagnostics)?;
        record_results.push(result);
    }
    diagnostics.consumed.hydrated_rows = diagnostics
        .consumed
        .hydrated_rows
        .saturating_add(record_results.len())
        .min(diagnostics.resolved.hydrated_rows);

    let target_results = options.limit.saturating_add(1);
    let mut results = Vec::<SearchPacketResult>::new();
    let mut result_index = BTreeMap::<Uuid, usize>::new();
    for result in verified_hits
        .iter()
        .map(|hit| event_search_result(hit, &snippet_query, options.snippet_chars))
        .chain(record_results)
    {
        let result_key = search_result_merge_key(&result, options.result_mode);
        if let Some(index) = result_index.get(&result_key).copied() {
            merge_search_result(&mut results[index], result);
        } else {
            result_index.insert(result_key, results.len());
            results.push(result);
        }
    }
    results.sort_by(compare_search_results);
    if results.len() > target_results {
        results.truncate(target_results);
    }
    if options.result_mode == SearchResultMode::Sessions {
        for result in &mut results {
            result.result_scope = if result.session_id.is_some() {
                SearchResultScope::Session
            } else {
                SearchResultScope::Event
            };
            result.session_importance =
                session_importance(result.rank, result.more_matches_in_session);
        }
    }

    let result_limit_exhausted = results.len() > options.limit;
    if result_limit_exhausted {
        results.truncate(options.limit);
        push_truncation_reason(&mut diagnostics, "result_limit");
    }
    let final_event_scores = results
        .iter()
        .filter_map(|result| {
            let event_id = result.event_id?;
            let score = candidates
                .get(&event_id)
                .map_or(-(result.rank as f64), |candidate| {
                    -candidate.reciprocal_rank_score
                });
            Some((event_id, score))
        })
        .collect::<Vec<_>>();
    let remaining_hydration_rows = resolved
        .hydrated_rows
        .saturating_sub(diagnostics.consumed.hydrated_rows);
    let remaining_hydration_bytes = resolved
        .hydration_input_bytes
        .saturating_sub(diagnostics.consumed.hydration_input_bytes);
    let mut full_hits = if remaining_hydration_rows == 0 || remaining_hydration_bytes == 0 {
        Vec::new()
    } else {
        store.event_search_hits_by_scores_bounded_visible(
            &final_event_scores,
            remaining_hydration_rows,
            remaining_hydration_bytes,
            resolved.hydration_input_bytes_per_event,
        )?
    };
    full_hits.retain(|hit| consume_hydration_input(&mut diagnostics, hit.preview.len()));
    let full_hits = full_hits
        .into_iter()
        .map(|hit| (hit.event_id, hit))
        .collect::<BTreeMap<_, _>>();
    for result in &mut results {
        let Some(hit) = result
            .event_id
            .and_then(|event_id| full_hits.get(&event_id))
        else {
            continue;
        };
        result.cursor = hit.cursor.clone();
        for citation in &mut result.citations {
            if citation.id == hit.event_id || Some(citation.id) == hit.session_id {
                citation.cursor = hit.cursor.clone();
            }
        }
    }
    normalize_search_result_ranks(&mut results);
    diagnostics.consumed.returned_results = results.len();
    diagnostics.consumed.returned_text_bytes = results
        .iter()
        .map(|result| result.title.len().saturating_add(result.snippet.len()))
        .sum();
    diagnostics.consumed.elapsed_ms =
        started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    diagnostics.truncated = !diagnostics.truncation_reasons.is_empty();
    let has_more =
        result_limit_exhausted || diagnostics.candidate_budget_exhausted || diagnostics.timed_out;
    let truncation = if diagnostics.truncated {
        ContextTruncation {
            truncated: true,
            reason: diagnostics.truncation_reasons.first().cloned(),
            omitted_results: 1,
        }
    } else {
        ContextTruncation::default()
    };
    Ok(SearchPacket {
        schema_version: SEARCH_PACKET_SCHEMA_VERSION,
        query: display_query,
        query_spec: Some(query),
        filters: options.filters,
        generated_at: utc_now(),
        results,
        pagination: pagination(None, has_more),
        truncation,
        query_execution: diagnostics,
    })
}

fn push_truncation_reason(diagnostics: &mut SearchExecutionDiagnostics, reason: &str) {
    if !diagnostics
        .truncation_reasons
        .iter()
        .any(|existing| existing == reason)
    {
        diagnostics.truncation_reasons.push(reason.to_owned());
    }
}

fn consume_hydration_input(
    diagnostics: &mut SearchExecutionDiagnostics,
    event_bytes: usize,
) -> bool {
    if event_bytes > diagnostics.resolved.hydration_input_bytes_per_event
        || diagnostics
            .consumed
            .hydration_input_bytes
            .saturating_add(event_bytes)
            > diagnostics.resolved.hydration_input_bytes
        || diagnostics.consumed.hydrated_rows >= diagnostics.resolved.hydrated_rows
    {
        push_truncation_reason(diagnostics, "hydration_input_budget");
        return false;
    }
    diagnostics.consumed.hydrated_rows =
        diagnostics.consumed.hydrated_rows.saturating_add(1);
    diagnostics.consumed.hydration_input_bytes = diagnostics
        .consumed
        .hydration_input_bytes
        .saturating_add(event_bytes);
    diagnostics.consumed.largest_hydration_input_bytes = diagnostics
        .consumed
        .largest_hydration_input_bytes
        .max(event_bytes);
    true
}

fn structured_timeout_error(
    started: Instant,
    diagnostics: &SearchExecutionDiagnostics,
) -> crate::query::SearchError {
    let mut diagnostics = diagnostics.clone();
    diagnostics.timed_out = true;
    diagnostics.consumed.elapsed_ms =
        started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    diagnostics.truncated = true;
    push_truncation_reason(&mut diagnostics, "elapsed_time_budget");
    crate::query::SearchError::TimedOut {
        timeout_ms: STRUCTURED_SEARCH_TIMEOUT.as_millis() as u64,
        diagnostics: Box::new(diagnostics),
    }
}

fn ensure_structured_deadline(
    started: Instant,
    diagnostics: &SearchExecutionDiagnostics,
) -> Result<()> {
    if started.elapsed() >= STRUCTURED_SEARCH_TIMEOUT {
        return Err(structured_timeout_error(started, diagnostics));
    }
    Ok(())
}

fn render_structured_query(query: &SearchQueryV1) -> String {
    let render = |clause: &SearchClause| match clause {
        SearchClause::All(value) => value.clone(),
        SearchClause::Phrase(value) => format!("phrase({value})"),
        SearchClause::Literal(value) => format!("literal({value})"),
        SearchClause::Semantic(value) => format!("semantic({value})"),
    };
    let mut parts = Vec::new();
    if !query.any.is_empty() {
        parts.push(
            query
                .any
                .iter()
                .map(render)
                .collect::<Vec<_>>()
                .join(" OR "),
        );
    }
    parts.extend(
        query
            .must
            .iter()
            .map(|clause| format!("must({})", render(clause))),
    );
    parts.extend(
        query
            .must_not
            .iter()
            .map(|clause| format!("must_not({})", render(clause))),
    );
    parts.join(" AND ")
}

fn record_matches_structured_query(record: &HistoryRecord, query: &SearchQueryV1) -> bool {
    let matches = |clause: &SearchClause| record_matches_clause(record, clause);
    (query.any.is_empty() || query.any.iter().any(matches))
        && query.must.iter().all(matches)
        && !query.must_not.iter().any(matches)
}

fn record_matches_clause(record: &HistoryRecord, clause: &SearchClause) -> bool {
    let tags = record.tags.join(" ");
    let fields = [record.title.as_str(), record.body.as_str(), tags.as_str()];
    match clause {
        SearchClause::All(_) => clause_matches_text(clause, &fields.join("\n")),
        SearchClause::Phrase(_) | SearchClause::Literal(_) => fields
            .into_iter()
            .any(|field| clause_matches_text(clause, field)),
        SearchClause::Semantic(_) => false,
    }
}

pub fn search_packet(store: &Store, query: &str, options: &PacketOptions) -> Result<SearchPacket> {
    if !query.trim().is_empty() {
        return search_packet_query(
            store,
            &SearchQueryV1::new(vec![SearchClause::all(query)]),
            options,
        );
    }
    if options
        .filters
        .file
        .as_deref()
        .is_some_and(|file| !file.trim().is_empty())
    {
        return bounded_file_search_packet(store, options);
    }
    let options = normalized_options(options);
    if let Some(provider) = options.filters.provider {
        if !store.has_provider_data(provider)? {
            return Ok(empty_search_packet(query, &options));
        }
    }
    let file_scope = file_filter_scope(store, &options.filters)?;
    if file_scope.as_ref().is_some_and(FileTouchScope::is_empty) {
        return Ok(empty_search_packet(query, &options));
    }
    if let Some(packet) = fast_event_search_packet(store, query, &options, file_scope.as_ref())? {
        return Ok(packet);
    }
    let CandidateSearch {
        candidates,
        scan_budget_exhausted,
    } = ranked_candidates(store, Some(query), &options, file_scope.as_ref())?;
    let mut truncation = ContextTruncation::default();
    let mut results = Vec::new();

    push_candidate_results(&mut results, &candidates, query, &options);

    let has_more = candidates.len() > results.len() || scan_budget_exhausted;
    if scan_budget_exhausted {
        truncation.truncated = true;
        truncation.omitted_results = 1;
        truncation.reason = Some("scan_budget".to_owned());
    } else if candidates.len() > results.len() {
        truncation.truncated = true;
        truncation.omitted_results = (candidates.len() - results.len()) as u32;
        truncation.reason = Some("limit".to_owned());
    }

    let cursor_offset = results.len();
    Ok(SearchPacket {
        schema_version: SEARCH_PACKET_SCHEMA_VERSION,
        query: query.to_owned(),
        query_spec: None,
        filters: options.filters,
        generated_at: utc_now(),
        results,
        pagination: pagination(Some(cursor_offset), has_more),
        truncation,
        query_execution: SearchExecutionDiagnostics::default(),
    })
}

pub fn search_packet_file_filter(store: &Store, options: &PacketOptions) -> Result<SearchPacket> {
    bounded_file_search_packet(store, options)
}

fn bounded_file_search_packet(store: &Store, options: &PacketOptions) -> Result<SearchPacket> {
    let started = Instant::now();
    let options = normalized_options(options);
    let Some(file) = options.filters.file.as_deref() else {
        return Ok(empty_search_packet("", &options));
    };
    let batch = store.search_file_record_candidates_scoped(
        file,
        &file_candidate_scope(&options.filters),
        options.limit.saturating_add(1),
        STRUCTURED_SEARCH_TIMEOUT,
    )?;
    if batch.timed_out {
        let diagnostics = SearchExecutionDiagnostics {
            candidate_strategy: "indexed_file_touch_bounded".to_owned(),
            resolved: SearchExecutionLimits::resolved(
                None,
                options.limit,
                STRUCTURED_SEARCH_TIMEOUT.as_millis() as u64,
            ),
            consumed: ctx_protocol::SearchExecutionConsumption {
                candidate_rows: batch.examined,
                retained_candidate_ids: batch.record_ids.len(),
                ..ctx_protocol::SearchExecutionConsumption::default()
            },
            per_branch_candidate_rows: options.limit.saturating_add(1),
            ..SearchExecutionDiagnostics::default()
        };
        return Err(structured_timeout_error(started, &diagnostics));
    }
    let mut results = Vec::new();
    for record_id in &batch.record_ids {
        let diagnostics = SearchExecutionDiagnostics {
            candidate_strategy: "indexed_file_touch_bounded".to_owned(),
            resolved: SearchExecutionLimits::resolved(
                None,
                options.limit,
                STRUCTURED_SEARCH_TIMEOUT.as_millis() as u64,
            ),
            consumed: ctx_protocol::SearchExecutionConsumption {
                candidate_rows: batch.examined,
                retained_candidate_ids: batch.record_ids.len(),
                hydrated_rows: results.len(),
                ..ctx_protocol::SearchExecutionConsumption::default()
            },
            per_branch_candidate_rows: options.limit.saturating_add(1),
            ..SearchExecutionDiagnostics::default()
        };
        ensure_structured_deadline(started, &diagnostics)?;
        let record = store.get_record(*record_id)?;
        let Some(touch_id) = batch.representative_touch_ids.get(record_id).copied() else {
            continue;
        };
        let Some(mut file_touch) = store.get_file_touched(touch_id)? else {
            continue;
        };
        let effective_source_id = batch
            .representative_source_ids
            .get(record_id)
            .copied()
            .or(file_touch.source_id);
        file_touch.source_id = effective_source_id;
        let source = effective_source_id
            .map(|source_id| store.get_capture_source(source_id))
            .transpose()?;
        if let Some(candidate) =
            candidate_for_bounded_file(record, file_touch, source, &options.filters)
        {
            results.push(candidate_search_result(&candidate, "", &options));
        }
    }
    results.sort_by(compare_search_results);
    let result_limit_exhausted = results.len() > options.limit;
    if result_limit_exhausted {
        results.truncate(options.limit);
    }
    normalize_search_result_ranks(&mut results);
    let mut reasons = Vec::new();
    if batch.truncated {
        reasons.push("file_candidate_rows_budget".to_owned());
    }
    if result_limit_exhausted {
        reasons.push("result_limit".to_owned());
    }
    let truncated = !reasons.is_empty();
    let elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let result_count = results.len();
    let returned_text_bytes = results
        .iter()
        .map(|result| result.title.len().saturating_add(result.snippet.len()))
        .sum();
    Ok(SearchPacket {
        schema_version: SEARCH_PACKET_SCHEMA_VERSION,
        query: String::new(),
        query_spec: None,
        filters: options.filters,
        generated_at: utc_now(),
        results,
        pagination: pagination(None, truncated),
        truncation: if truncated {
            ContextTruncation {
                truncated: true,
                reason: reasons.first().cloned(),
                omitted_results: 1,
            }
        } else {
            ContextTruncation::default()
        },
        query_execution: SearchExecutionDiagnostics {
            candidate_strategy: "indexed_file_touch_bounded".to_owned(),
            resolved: SearchExecutionLimits::resolved(
                None,
                options.limit,
                STRUCTURED_SEARCH_TIMEOUT.as_millis() as u64,
            ),
            consumed: ctx_protocol::SearchExecutionConsumption {
                candidate_rows: batch.examined,
                retained_candidate_ids: batch.record_ids.len(),
                hydrated_rows: result_count,
                returned_results: result_count,
                returned_text_bytes,
                elapsed_ms,
                ..ctx_protocol::SearchExecutionConsumption::default()
            },
            per_branch_candidate_rows: options.limit.saturating_add(1),
            candidate_budget_exhausted: batch.truncated,
            elapsed_ms,
            truncated,
            truncation_reasons: reasons,
            ..SearchExecutionDiagnostics::default()
        },
    })
}

pub fn semantic_event_search_packet(
    store: &Store,
    query: &str,
    options: &PacketOptions,
    semantic_hits: &[SemanticEventHit],
    semantic_weight: f32,
    hybrid: bool,
) -> Result<SearchPacket> {
    let options = normalized_options(options);
    if query.trim().is_empty() {
        return Ok(empty_search_packet(query, &options));
    }
    let file_scope = file_filter_scope(store, &options.filters)?;
    if file_scope.as_ref().is_some_and(FileTouchScope::is_empty) {
        return Ok(empty_search_packet(query, &options));
    }
    let file_scope = file_scope.as_ref();
    let target_results = options.limit.saturating_add(1);
    let mut candidates = BTreeMap::<Uuid, HybridCandidate>::new();

    if hybrid {
        let page_size = FILTERED_SEARCH_PAGE_SIZE.max(target_results.saturating_mul(8).max(50));
        let mut lexical_rank = 0_usize;
        let lexical_hits = if options.filters.event_type.is_some() {
            store.search_event_hits_page(query, page_size, 0)?
        } else {
            store.search_event_hits_page_prefer_conversation(query, page_size, 0)?
        };
        for hit in lexical_hits {
            if !event_hit_matches_filters(&hit, &options.filters, file_scope) {
                continue;
            }
            lexical_rank = lexical_rank.saturating_add(1);
            let mut result = event_search_result(&hit, query, options.snippet_chars);
            result.result_scope = SearchResultScope::Event;
            candidates.insert(
                hit.event_id,
                HybridCandidate {
                    result,
                    lexical_score: Some(reciprocal_rank(lexical_rank)),
                    semantic_score: None,
                    occurred_at: hit.occurred_at,
                    seq: hit.seq,
                },
            );
        }
    }

    let mut semantic_rank = 0_usize;
    for semantic_hit in semantic_hits {
        let hit = &semantic_hit.hit;
        if !event_hit_matches_filters(hit, &options.filters, file_scope) {
            continue;
        }
        semantic_rank = semantic_rank.saturating_add(1);
        let semantic_score = if hybrid {
            reciprocal_rank(semantic_rank)
        } else {
            semantic_hit.similarity.max(0.0)
        };
        let entry = candidates.entry(hit.event_id).or_insert_with(|| {
            let mut result = event_search_result(hit, query, options.snippet_chars);
            result.result_scope = SearchResultScope::Event;
            HybridCandidate {
                result,
                lexical_score: None,
                semantic_score: None,
                occurred_at: hit.occurred_at,
                seq: hit.seq,
            }
        });
        entry.semantic_score = Some(
            entry
                .semantic_score
                .map_or(semantic_score, |existing| existing.max(semantic_score)),
        );
        push_unique_why(
            &mut entry.result.why_matched,
            "semantic_similarity".to_owned(),
        );
        push_unique_why(
            &mut entry.result.why_matched,
            format!("semantic:{}", event_reason(hit.event_type)),
        );
    }

    let semantic_weight = semantic_weight.clamp(0.0, 1.0);
    let mut results = candidates
        .into_values()
        .map(|mut candidate| {
            candidate.result.rank = if hybrid {
                let lexical = candidate.lexical_score.unwrap_or(0.0);
                let semantic = candidate.semantic_score.unwrap_or(0.0);
                ((1.0 - semantic_weight) * lexical) + (semantic_weight * semantic)
            } else {
                candidate.semantic_score.unwrap_or(candidate.result.rank)
            };
            candidate
        })
        .collect::<Vec<_>>();
    results.sort_by(compare_hybrid_candidates);
    if options.result_mode == SearchResultMode::Sessions {
        results = cluster_hybrid_candidates_by_session(results);
    }
    let has_more = results.len() > options.limit;
    if results.len() > options.limit {
        results.truncate(options.limit);
    }
    let mut packet_results = results
        .into_iter()
        .map(|candidate| candidate.result)
        .collect::<Vec<_>>();
    normalize_search_result_ranks(&mut packet_results);
    let cursor_offset = packet_results.len();

    Ok(SearchPacket {
        schema_version: SEARCH_PACKET_SCHEMA_VERSION,
        query: query.to_owned(),
        query_spec: None,
        filters: options.filters,
        generated_at: utc_now(),
        results: packet_results,
        pagination: pagination(Some(cursor_offset), has_more),
        truncation: if has_more {
            ContextTruncation {
                truncated: true,
                reason: Some("limit".to_owned()),
                omitted_results: 1,
            }
        } else {
            ContextTruncation::default()
        },
        query_execution: SearchExecutionDiagnostics::default(),
    })
}

pub fn search_packet_terms(
    store: &Store,
    query: &str,
    terms: &[String],
    options: &PacketOptions,
) -> Result<SearchPacket> {
    let search_terms = composed_search_terms(query, terms);
    search_packet_query(
        store,
        &SearchQueryV1::new(search_terms.into_iter().map(SearchClause::all).collect()),
        options,
    )
}

struct HybridCandidate {
    result: SearchPacketResult,
    lexical_score: Option<f32>,
    semantic_score: Option<f32>,
    occurred_at: chrono::DateTime<chrono::Utc>,
    seq: u64,
}

fn reciprocal_rank(rank: usize) -> f32 {
    1.0 / (60.0 + rank.max(1) as f32)
}

fn compare_hybrid_candidates(left: &HybridCandidate, right: &HybridCandidate) -> Ordering {
    right
        .result
        .rank
        .partial_cmp(&left.result.rank)
        .unwrap_or(Ordering::Equal)
        .then_with(|| right.occurred_at.cmp(&left.occurred_at))
        .then_with(|| right.seq.cmp(&left.seq))
        .then_with(|| left.result.record_id.cmp(&right.result.record_id))
}

fn cluster_hybrid_candidates_by_session(candidates: Vec<HybridCandidate>) -> Vec<HybridCandidate> {
    let mut clustered = Vec::<HybridCandidate>::new();
    let mut index_by_cluster = BTreeMap::<Uuid, usize>::new();
    for mut candidate in candidates {
        let cluster_id = candidate
            .result
            .session_id
            .or(candidate.result.event_id)
            .unwrap_or(candidate.result.record_id);
        if let Some(index) = index_by_cluster.get(&cluster_id).copied() {
            clustered[index].result.more_matches_in_session = clustered[index]
                .result
                .more_matches_in_session
                .saturating_add(1);
        } else {
            candidate.result.result_scope = if candidate.result.session_id.is_some() {
                SearchResultScope::Session
            } else {
                SearchResultScope::Event
            };
            candidate.result.session_importance = session_importance(
                candidate.result.rank,
                candidate.result.more_matches_in_session,
            );
            index_by_cluster.insert(cluster_id, clustered.len());
            clustered.push(candidate);
        }
    }
    clustered
}

fn fast_event_search_packet(
    store: &Store,
    query: &str,
    options: &PacketOptions,
    file_scope: Option<&FileTouchScope>,
) -> Result<Option<SearchPacket>> {
    if query.trim().is_empty() {
        return Ok(None);
    }
    if has_history_source_filter(&options.filters) {
        return Ok(None);
    }
    if !store.has_at_least_events(LARGE_EVENT_CORPUS_THRESHOLD)? {
        return Ok(None);
    }

    let target_results = options.limit.saturating_add(1);
    let filtered = has_filters(&options.filters);
    let clustered = options.result_mode == SearchResultMode::Sessions;
    let page_size = if clustered {
        FILTERED_SEARCH_PAGE_SIZE.max(target_results.saturating_mul(8).max(50))
    } else if filtered {
        FILTERED_SEARCH_PAGE_SIZE.max(target_results)
    } else {
        target_results
    };
    let mut results = Vec::new();
    let mut clustered_results = Vec::<SearchPacketResult>::new();
    let mut clustered_index = BTreeMap::<Uuid, usize>::new();
    let mut offset = 0_usize;
    let mut pages_scanned = 0_usize;
    let mut scan_budget_exhausted = false;

    loop {
        pages_scanned = pages_scanned.saturating_add(1);
        let hits = if options.filters.event_type.is_some() {
            store.search_event_hits_page(query, page_size, offset)?
        } else {
            store.search_event_hits_page_prefer_conversation(query, page_size, offset)?
        };
        let page_len = hits.len();

        for hit in hits {
            if !event_hit_matches_filters(&hit, &options.filters, file_scope) {
                continue;
            }
            if clustered {
                let cluster_id = hit.session_id.unwrap_or(hit.event_id);
                if let Some(index) = clustered_index.get(&cluster_id).copied() {
                    let existing = &mut clustered_results[index];
                    existing.more_matches_in_session =
                        existing.more_matches_in_session.saturating_add(1);
                    existing.session_importance =
                        session_importance(existing.rank, existing.more_matches_in_session);
                } else {
                    let mut result = event_search_result(&hit, query, options.snippet_chars);
                    result.result_scope = if result.session_id.is_some() {
                        SearchResultScope::Session
                    } else {
                        SearchResultScope::Event
                    };
                    result.session_importance = session_importance(result.rank, 0);
                    clustered_index.insert(cluster_id, clustered_results.len());
                    clustered_results.push(result);
                }
                if clustered_results.len() >= target_results {
                    break;
                }
            } else {
                let result = event_search_result(&hit, query, options.snippet_chars);
                results.push(result);
                if results.len() >= target_results {
                    break;
                }
            }
        }

        let enough_results = if clustered {
            clustered_results.len() >= target_results
        } else {
            results.len() >= target_results
        };
        if (!filtered && !clustered) || enough_results || page_len < page_size {
            break;
        }
        if pages_scanned >= FILTERED_SEARCH_MAX_PAGES {
            scan_budget_exhausted = true;
            break;
        }
        let next_offset = offset.saturating_add(page_size);
        if next_offset == offset {
            break;
        }
        offset = next_offset;
    }

    if clustered {
        results = clustered_results;
    }
    let has_more = results.len() > options.limit || scan_budget_exhausted;
    if results.len() > options.limit {
        results.truncate(options.limit);
    }
    normalize_search_result_ranks(&mut results);

    let truncation = if scan_budget_exhausted {
        ContextTruncation {
            truncated: true,
            reason: Some("scan_budget".to_owned()),
            omitted_results: 1,
        }
    } else if has_more {
        ContextTruncation {
            truncated: true,
            reason: Some("limit".to_owned()),
            omitted_results: 1,
        }
    } else {
        ContextTruncation::default()
    };

    let cursor_offset = results.len();
    Ok(Some(SearchPacket {
        schema_version: SEARCH_PACKET_SCHEMA_VERSION,
        query: query.to_owned(),
        query_spec: None,
        filters: options.filters.clone(),
        generated_at: utc_now(),
        results,
        pagination: pagination(Some(cursor_offset), has_more),
        truncation,
        query_execution: SearchExecutionDiagnostics::default(),
    }))
}

#[cfg(test)]
mod timeout_tests {
    use super::*;

    #[test]
    fn timeout_error_preserves_consumed_work_and_marks_budget_exhaustion() {
        let diagnostics = SearchExecutionDiagnostics {
            resolved: SearchExecutionLimits::resolved(
                None,
                10,
                STRUCTURED_SEARCH_TIMEOUT.as_millis() as u64,
            ),
            consumed: ctx_protocol::SearchExecutionConsumption {
                query_bytes: 37,
                clauses: 3,
                candidate_rows: 417,
                retained_candidate_ids: 211,
                ..ctx_protocol::SearchExecutionConsumption::default()
            },
            clauses_executed: 2,
            ..SearchExecutionDiagnostics::default()
        };
        let started = Instant::now()
            .checked_sub(STRUCTURED_SEARCH_TIMEOUT)
            .expect("test instant supports the structured-search timeout");

        let error = structured_timeout_error(started, &diagnostics);
        let crate::query::SearchError::TimedOut {
            timeout_ms,
            diagnostics,
        } = error
        else {
            panic!("expected a structured-search timeout");
        };

        assert_eq!(timeout_ms, STRUCTURED_SEARCH_TIMEOUT.as_millis() as u64);
        assert_eq!(diagnostics.consumed.query_bytes, 37);
        assert_eq!(diagnostics.consumed.clauses, 3);
        assert_eq!(diagnostics.consumed.candidate_rows, 417);
        assert_eq!(diagnostics.consumed.retained_candidate_ids, 211);
        assert_eq!(diagnostics.clauses_executed, 2);
        assert!(diagnostics.timed_out);
        assert!(diagnostics.truncated);
        assert!(diagnostics.consumed.elapsed_ms >= timeout_ms);
        assert_eq!(diagnostics.truncation_reasons, vec!["elapsed_time_budget"]);
    }

    #[test]
    fn oversized_event_payload_is_rejected_before_hydration_accounting() {
        let mut diagnostics = SearchExecutionDiagnostics {
            resolved: SearchExecutionLimits::resolved(
                None,
                10,
                STRUCTURED_SEARCH_TIMEOUT.as_millis() as u64,
            ),
            ..SearchExecutionDiagnostics::default()
        };
        let oversized = diagnostics
            .resolved
            .hydration_input_bytes_per_event
            .saturating_add(1);

        assert!(!consume_hydration_input(&mut diagnostics, oversized));
        assert_eq!(diagnostics.consumed.hydrated_rows, 0);
        assert_eq!(diagnostics.consumed.hydration_input_bytes, 0);
        assert_eq!(diagnostics.truncation_reasons, vec!["hydration_input_budget"]);
    }
}
