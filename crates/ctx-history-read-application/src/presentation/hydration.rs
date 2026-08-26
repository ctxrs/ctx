use std::{collections::BTreeSet, fmt};

use anyhow::{anyhow, Result};
use ctx_history_core::{MAX_CORE_CONTENT_BYTES, MAX_ENCODED_CORE_RECORD_BYTES};
use ctx_history_index_format::search_projection::project_search_content;
use ctx_history_index_query::{
    CompiledSearchFilter, CoreEventPageBudget, CoreEventRecord, RankedEventRef, VerifiedIndex,
};
use uuid::Uuid;

use super::{
    analyzed_query_terms, search_excerpt, AnalyzedQueryTerms, MAX_SEARCH_RESULTS,
    SEARCH_SNIPPET_MAX_BYTES,
};
use crate::search::RankedSearchCollection;
use crate::{
    NormalizedSearchQuery, SearchCollection, SearchEventMetadata, SearchHit, SearchResultWindow,
};

const SEARCH_CORE_RECORD_BUDGET: CoreEventPageBudget =
    CoreEventPageBudget::new(MAX_ENCODED_CORE_RECORD_BYTES, MAX_CORE_CONTENT_BYTES);
pub const SEARCH_PRESENTATION_MAX_RETAINED_SNIPPET_BYTES: usize =
    MAX_SEARCH_RESULTS * SEARCH_SNIPPET_MAX_BYTES;

/// Bounded query result state derived from one complete stored Core record.
#[derive(Debug, PartialEq, Eq)]
pub struct SearchPresentation {
    pub event_id: Uuid,
    pub snippet: String,
    pub snippet_truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchPresentationHydrationBudget {
    pub maximum_retained_snippet_bytes: usize,
}

pub const SEARCH_PRESENTATION_HYDRATION_BUDGET: SearchPresentationHydrationBudget =
    SearchPresentationHydrationBudget {
        maximum_retained_snippet_bytes: SEARCH_PRESENTATION_MAX_RETAINED_SNIPPET_BYTES,
    };

pub(crate) fn hydrate_ranked_search_collection(
    index: &VerifiedIndex,
    collection: RankedSearchCollection,
    query: &NormalizedSearchQuery,
    filter: &CompiledSearchFilter,
) -> Result<(SearchCollection, Vec<SearchPresentation>)> {
    hydrate_ranked_search_collection_with_budget(
        index,
        collection,
        query,
        filter,
        SEARCH_PRESENTATION_HYDRATION_BUDGET,
    )
}

pub(crate) fn hydrate_ranked_search_collection_with_budget(
    index: &VerifiedIndex,
    collection: RankedSearchCollection,
    query: &NormalizedSearchQuery,
    filter: &CompiledSearchFilter,
    budget: SearchPresentationHydrationBudget,
) -> Result<(SearchCollection, Vec<SearchPresentation>)> {
    let RankedSearchCollection {
        result_window,
        candidate_pool,
        candidate_pool_truncated,
        lexical_diagnostics,
        diversification,
        requested_backend,
        effective_backend,
        semantic_weight,
        semantic_status,
        semantic_fallback,
        semantic_diagnostics,
        work,
        stop_reason,
    } = collection;
    if result_window.hits.len() > MAX_SEARCH_RESULTS {
        return Err(anyhow!(
            "search presentation cannot hydrate more than {MAX_SEARCH_RESULTS} hits"
        ));
    }
    if budget.maximum_retained_snippet_bytes == 0 {
        return Err(anyhow!(
            "search presentation hydration budget must be positive"
        ));
    }
    let mut requested = BTreeSet::new();
    for hit in &result_window.hits {
        if !requested.insert(hit.event.event_id) {
            return Err(anyhow!(
                "search result duplicated Core event {}",
                hit.event.event_id
            ));
        }
    }
    let event_ids = result_window
        .hits
        .iter()
        .map(|hit| hit.event.event_id)
        .collect::<Vec<_>>();
    let mut records = index
        .stream_core_events_by_ids_with_strict_per_record_budget(
            &event_ids,
            result_window.hits.len(),
            SEARCH_CORE_RECORD_BUDGET,
        )?
        .ok_or_else(|| {
            anyhow!(
                "pinned Core lookup omitted search event {}",
                event_ids.first().copied().unwrap_or_else(Uuid::nil)
            )
        })?;
    let query_texts = query.texts();
    let query_terms = analyzed_query_terms(&query_texts);
    let mut hits = Vec::with_capacity(result_window.hits.len());
    let mut presentations = Vec::with_capacity(result_window.hits.len());
    let mut retained_snippet_bytes = 0_usize;
    for hit in result_window.hits {
        let event_id = hit.event.event_id;
        let record = records
            .next()
            .transpose()?
            .ok_or_else(|| anyhow!("pinned Core lookup omitted search event {event_id}"))?;
        let (event, presentation, snippet_bytes) =
            ranked_search_projection(record, &hit.event, &query_terms, filter)?;
        let next_retained_snippet_bytes = retained_snippet_bytes
            .checked_add(snippet_bytes)
            .ok_or_else(|| {
                search_presentation_retention_budget_error(event_id, retained_snippet_bytes, budget)
            })?;
        if next_retained_snippet_bytes > budget.maximum_retained_snippet_bytes {
            return Err(search_presentation_retention_budget_error(
                event_id,
                next_retained_snippet_bytes,
                budget,
            ));
        }
        retained_snippet_bytes = next_retained_snippet_bytes;
        hits.push(SearchHit {
            event,
            score: hit.score,
            more_matches_in_session: hit.more_matches_in_session,
        });
        presentations.push(presentation);
    }
    if records.next().transpose()?.is_some() {
        return Err(anyhow!(
            "pinned Core lookup returned more search records than requested"
        ));
    }
    Ok((
        SearchCollection {
            result_window: SearchResultWindow {
                limit: result_window.limit,
                hits,
                more_available: result_window.more_available,
            },
            candidate_pool,
            candidate_pool_truncated,
            lexical_diagnostics,
            diversification,
            requested_backend,
            effective_backend,
            semantic_weight,
            semantic_status,
            semantic_fallback,
            semantic_diagnostics,
            work,
            stop_reason,
        },
        presentations,
    ))
}

fn ranked_search_projection(
    record: CoreEventRecord,
    expected: &RankedEventRef,
    query_terms: &AnalyzedQueryTerms,
    filter: &CompiledSearchFilter,
) -> Result<(SearchEventMetadata, SearchPresentation, usize)> {
    if !filter.matches_core(&record)? {
        return Err(anyhow!(
            "pinned Core lookup no longer matches the compiled Search filter for event {}",
            expected.event_id
        ));
    }
    let CoreEventRecord { event, core_record } = record;
    if event.event_id != core_record.event_id
        || event.session_id != core_record.session_id
        || event.event_id.as_uuid() != expected.event_id
        || event.event_id.digest() != expected.event_identity_digest
        || event.session_id.as_uuid() != expected.session_id
        || event.source.identity().digest() != expected.source_owner_digest
        || event.event_sequence != expected.event_sequence
        || event.occurred_at_unix_ms != expected.occurred_at_unix_ms
        || event.event_copy.is_some() != expected.has_event_copy
    {
        return Err(anyhow!(
            "pinned Core lookup returned misaligned ranked metadata for search event {}",
            expected.event_id
        ));
    }
    let projection = project_search_content(core_record.content)?.ok_or_else(|| {
        anyhow!(
            "Core search event {} has no searchable body projection",
            event.event_id
        )
    })?;
    let (snippet, snippet_truncated) = search_excerpt(&projection, query_terms);
    let retained_snippet_bytes = snippet.len();
    let metadata = SearchEventMetadata::from(&event);
    drop(projection);
    Ok((
        metadata,
        SearchPresentation {
            event_id: expected.event_id,
            snippet,
            snippet_truncated,
        },
        retained_snippet_bytes,
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPresentationRetentionBudgetExceeded {
    pub event_id: Uuid,
    pub retained_snippet_bytes: usize,
    pub maximum_retained_snippet_bytes: usize,
}

impl fmt::Display for SearchPresentationRetentionBudgetExceeded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Core search event {} cannot fit the bounded search presentation retention budget (retained snippets: {}/{})",
            self.event_id,
            self.retained_snippet_bytes,
            self.maximum_retained_snippet_bytes,
        )
    }
}

impl std::error::Error for SearchPresentationRetentionBudgetExceeded {}

fn search_presentation_retention_budget_error(
    event_id: Uuid,
    retained_snippet_bytes: usize,
    budget: SearchPresentationHydrationBudget,
) -> anyhow::Error {
    anyhow::Error::new(SearchPresentationRetentionBudgetExceeded {
        event_id,
        retained_snippet_bytes,
        maximum_retained_snippet_bytes: budget.maximum_retained_snippet_bytes,
    })
}
