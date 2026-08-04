use std::{collections::BTreeSet, fmt};

use anyhow::{anyhow, Result};
use ctx_history_core::{MAX_CORE_CONTENT_BYTES, MAX_ENCODED_CORE_RECORD_BYTES};
use ctx_history_index::{project_body_search, CoreEventPageBudget, CoreEventRecord, VerifiedIndex};
use uuid::Uuid;

use crate::MAX_SEARCH_LIMIT;

use super::{NormalizedSearchQuery, SearchEventMetadata, SearchHit};
use crate::commands::source_index::render::{search_snippet_fragment, SEARCH_SNIPPET_MAX_BYTES};

const SEARCH_CORE_RECORD_BUDGET: CoreEventPageBudget =
    CoreEventPageBudget::new(MAX_ENCODED_CORE_RECORD_BYTES, MAX_CORE_CONTENT_BYTES);
pub(in crate::commands::source_index) const SEARCH_PRESENTATION_MAX_RETAINED_SNIPPET_BYTES: usize =
    MAX_SEARCH_LIMIT * SEARCH_SNIPPET_MAX_BYTES;

/// Compact, non-authoritative search state derived from one complete stored
/// Core record. Event metadata is borrowed from the already compact result
/// window; only the snippet is newly retained.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::commands::source_index) struct SearchPresentation<'event> {
    pub(in crate::commands::source_index) event: &'event SearchEventMetadata,
    pub(in crate::commands::source_index) snippet: String,
    pub(in crate::commands::source_index) snippet_truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::commands::source_index) struct SearchPresentationHydrationBudget {
    pub(in crate::commands::source_index) maximum_retained_snippet_bytes: usize,
}

pub(in crate::commands::source_index) const SEARCH_PRESENTATION_HYDRATION_BUDGET:
    SearchPresentationHydrationBudget = SearchPresentationHydrationBudget {
    maximum_retained_snippet_bytes: SEARCH_PRESENTATION_MAX_RETAINED_SNIPPET_BYTES,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::commands::source_index) struct SearchPresentationRetentionBudgetExceeded {
    pub(in crate::commands::source_index) event_id: Uuid,
    pub(in crate::commands::source_index) retained_snippet_bytes: usize,
    pub(in crate::commands::source_index) maximum_retained_snippet_bytes: usize,
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

pub(super) fn presentations_for_search_hits<'event>(
    index: &VerifiedIndex,
    hits: &'event [SearchHit],
    query: &NormalizedSearchQuery,
) -> Result<Vec<SearchPresentation<'event>>> {
    presentations_for_search_hits_with_budget(
        index,
        hits,
        query,
        SEARCH_PRESENTATION_HYDRATION_BUDGET,
    )
}

pub(in crate::commands::source_index) fn presentations_for_search_hits_with_budget<'event>(
    index: &VerifiedIndex,
    hits: &'event [SearchHit],
    query: &NormalizedSearchQuery,
    budget: SearchPresentationHydrationBudget,
) -> Result<Vec<SearchPresentation<'event>>> {
    if budget.maximum_retained_snippet_bytes == 0 {
        return Err(anyhow!(
            "search presentation hydration budget must be positive"
        ));
    }

    let mut requested = BTreeSet::new();
    for hit in hits {
        if !requested.insert(hit.event.event_id) {
            return Err(anyhow!(
                "search result duplicated Core event {}",
                hit.event.event_id
            ));
        }
    }

    let event_ids = hits
        .iter()
        .map(|hit| hit.event.event_id)
        .collect::<Vec<_>>();
    // Execute one generation-pinned Tantivy selection. The returned iterator
    // decodes exactly one complete Core record at a time, allowing each body
    // to be projected and discarded before the next record is materialized.
    let mut records = index
        .stream_core_events_by_ids_with_strict_per_record_budget(
            &event_ids,
            hits.len(),
            SEARCH_CORE_RECORD_BUDGET,
        )?
        .ok_or_else(|| {
            anyhow!(
                "pinned Core lookup omitted search event {}",
                event_ids.first().copied().unwrap_or_else(Uuid::nil)
            )
        })?;
    let query_texts = query.texts();
    let mut presentations = Vec::with_capacity(hits.len());
    let mut retained_snippet_bytes = 0_usize;
    for hit in hits {
        let event_id = hit.event.event_id;
        let record = records
            .next()
            .transpose()?
            .ok_or_else(|| anyhow!("pinned Core lookup omitted search event {event_id}"))?;

        let (presentation, snippet_bytes) =
            search_presentation_projection(record, &hit.event, &query_texts)?;
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
        presentations.push(presentation);
    }
    if records.next().transpose()?.is_some() {
        return Err(anyhow!(
            "pinned Core lookup returned more search records than requested"
        ));
    }
    Ok(presentations)
}

fn search_presentation_projection<'event>(
    record: CoreEventRecord,
    expected_event: &'event SearchEventMetadata,
    query_texts: &[&str],
) -> Result<(SearchPresentation<'event>, usize)> {
    let CoreEventRecord { event, core_record } = record;
    if event.event_id != core_record.event_id
        || event.session_id != core_record.session_id
        || SearchEventMetadata::from(&event) != *expected_event
    {
        return Err(anyhow!(
            "pinned Core lookup returned misaligned metadata for search event {}",
            expected_event.event_id
        ));
    }
    let body = project_body_search(core_record.content)?.ok_or_else(|| {
        anyhow!(
            "Core search event {} has no searchable body projection",
            event.event_id
        )
    })?;
    let (snippet, snippet_truncated) = search_snippet_fragment(&body, query_texts);
    let retained_snippet_bytes = snippet.len();

    // Neither the complete searchable projection nor the remainder of Core
    // crosses the search presentation boundary.
    drop(body);
    drop(event);
    Ok((
        SearchPresentation {
            event: expected_event,
            snippet,
            snippet_truncated,
        },
        retained_snippet_bytes,
    ))
}

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
