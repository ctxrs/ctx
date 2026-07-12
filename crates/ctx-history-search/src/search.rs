use std::{cmp::Ordering, collections::BTreeMap};

use ctx_history_core::{utc_now, ContextTruncation};
use ctx_history_store::{FileTouchScope, Store};
use uuid::Uuid;

use crate::filters::{
    event_hit_matches_filters, file_filter_scope, has_filters, has_history_source_filter,
};
use crate::model::CandidateSearch;
use crate::packet::{
    empty_search_packet, pagination, SearchPacket, SearchPacketResult, SearchResultScope,
    SemanticEventHit, SEARCH_PACKET_SCHEMA_VERSION,
};
use crate::query::{
    composed_search_terms, normalized_options, PacketOptions, Result, SearchResultMode,
    FILTERED_SEARCH_MAX_PAGES, FILTERED_SEARCH_PAGE_SIZE, LARGE_EVENT_CORPUS_THRESHOLD,
    MAX_RESULT_LIMIT,
};
use crate::ranking::ranked_candidates;
use crate::results::{
    compare_search_results, event_reason, event_search_result, merge_search_result,
    normalize_search_result_ranks, push_candidate_results, push_unique_why,
    search_result_merge_key, session_importance,
};

pub fn search_packet(store: &Store, query: &str, options: &PacketOptions) -> Result<SearchPacket> {
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
        filters: options.filters,
        generated_at: utc_now(),
        results,
        pagination: pagination(Some(cursor_offset), has_more),
        truncation,
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
    })
}

pub fn search_packet_terms(
    store: &Store,
    query: &str,
    terms: &[String],
    options: &PacketOptions,
) -> Result<SearchPacket> {
    let options = normalized_options(options);
    let search_terms = composed_search_terms(query, terms);
    if search_terms.len() <= 1 {
        return search_packet(
            store,
            search_terms.first().map_or(query, String::as_str),
            &options,
        );
    }

    let mut child_options = options.clone();
    child_options.limit = options
        .limit
        .saturating_mul(2)
        .max(options.limit)
        .min(MAX_RESULT_LIMIT);

    let mut merged_results = Vec::<SearchPacketResult>::new();
    let mut result_index = BTreeMap::<Uuid, usize>::new();
    let mut truncated = false;
    let mut omitted_results = 0_u32;
    for term in &search_terms {
        let packet = search_packet(store, term, &child_options)?;
        truncated |= packet.truncation.truncated;
        omitted_results = omitted_results.saturating_add(packet.truncation.omitted_results);
        for mut result in packet.results {
            push_unique_why(&mut result.why_matched, format!("term:{term}"));
            let result_key = search_result_merge_key(&result, options.result_mode);
            if let Some(index) = result_index.get(&result_key).copied() {
                merge_search_result(&mut merged_results[index], result);
            } else {
                result_index.insert(result_key, merged_results.len());
                merged_results.push(result);
            }
        }
    }

    merged_results.sort_by(compare_search_results);
    let has_more = merged_results.len() > options.limit || truncated;
    if merged_results.len() > options.limit {
        omitted_results =
            omitted_results.saturating_add((merged_results.len() - options.limit) as u32);
        merged_results.truncate(options.limit);
    }
    normalize_search_result_ranks(&mut merged_results);

    let truncation = if has_more {
        ContextTruncation {
            truncated: true,
            reason: Some(if truncated { "source_limit" } else { "limit" }.to_owned()),
            omitted_results: omitted_results.max(1),
        }
    } else {
        ContextTruncation::default()
    };
    let cursor_offset = merged_results.len();

    Ok(SearchPacket {
        schema_version: SEARCH_PACKET_SCHEMA_VERSION,
        query: search_terms.join(" OR "),
        filters: options.filters,
        generated_at: utc_now(),
        results: merged_results,
        pagination: pagination(Some(cursor_offset), has_more),
        truncation,
    })
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
        filters: options.filters.clone(),
        generated_at: utc_now(),
        results,
        pagination: pagination(Some(cursor_offset), has_more),
        truncation,
    }))
}
