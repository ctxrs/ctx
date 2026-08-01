use std::{collections::HashMap, fmt};

use anyhow::{anyhow, Result};
use ctx_history_core::{CoreRecord, MAX_CORE_CONTENT_BYTES, MAX_ENCODED_CORE_RECORD_BYTES};
use ctx_history_index::{CoreEventPageBudget, CoreEventRecord, VerifiedIndex};
use uuid::Uuid;

use crate::MAX_SEARCH_LIMIT;

use super::SearchHit;

// Search renders 2,048 characters and needs one more character to preserve a
// truthful truncation bit. Four bytes is the maximum UTF-8 width of one char.
pub(in crate::commands::source_index) const SEARCH_CORE_BODY_PREFIX_CHARS: usize = 2_049;
const MAX_UTF8_CHAR_BYTES: usize = 4;
pub(in crate::commands::source_index) const SEARCH_CORE_MAX_RETAINED_BODY_BYTES: usize =
    MAX_SEARCH_LIMIT * SEARCH_CORE_BODY_PREFIX_CHARS * MAX_UTF8_CHAR_BYTES;
const SEARCH_CORE_MAX_AGGREGATE_ENCODED_BYTES: usize = MAX_ENCODED_CORE_RECORD_BYTES;
const SEARCH_CORE_MAX_AGGREGATE_CONTENT_BYTES: usize = MAX_CORE_CONTENT_BYTES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::commands::source_index) struct SearchCoreHydrationBudget {
    pub(in crate::commands::source_index) maximum_encoded_core_bytes: usize,
    pub(in crate::commands::source_index) maximum_content_bytes: usize,
    pub(in crate::commands::source_index) maximum_retained_body_bytes: usize,
}

pub(in crate::commands::source_index) const SEARCH_CORE_HYDRATION_BUDGET:
    SearchCoreHydrationBudget = SearchCoreHydrationBudget {
    maximum_encoded_core_bytes: SEARCH_CORE_MAX_AGGREGATE_ENCODED_BYTES,
    maximum_content_bytes: SEARCH_CORE_MAX_AGGREGATE_CONTENT_BYTES,
    maximum_retained_body_bytes: SEARCH_CORE_MAX_RETAINED_BODY_BYTES,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::commands::source_index) enum SearchCoreHydrationBudgetStage {
    Decode,
    Retention,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::commands::source_index) struct SearchCoreHydrationBudgetExceeded {
    pub(in crate::commands::source_index) event_id: Uuid,
    pub(in crate::commands::source_index) stage: SearchCoreHydrationBudgetStage,
    pub(in crate::commands::source_index) admitted_encoded_core_bytes: usize,
    pub(in crate::commands::source_index) maximum_encoded_core_bytes: usize,
    pub(in crate::commands::source_index) admitted_content_bytes: usize,
    pub(in crate::commands::source_index) maximum_content_bytes: usize,
    pub(in crate::commands::source_index) retained_body_bytes: usize,
    pub(in crate::commands::source_index) maximum_retained_body_bytes: usize,
}

impl fmt::Display for SearchCoreHydrationBudgetExceeded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Core search event {} cannot fit the aggregate search {:?} budget (encoded Core: {}/{}, decoded content: {}/{}, retained snippet bodies: {}/{})",
            self.event_id,
            self.stage,
            self.admitted_encoded_core_bytes,
            self.maximum_encoded_core_bytes,
            self.admitted_content_bytes,
            self.maximum_content_bytes,
            self.retained_body_bytes,
            self.maximum_retained_body_bytes,
        )
    }
}

impl std::error::Error for SearchCoreHydrationBudgetExceeded {}

pub(super) fn core_records_for_search_hits(
    index: &VerifiedIndex,
    hits: &[SearchHit],
) -> Result<HashMap<Uuid, CoreEventRecord>> {
    core_records_for_search_hits_with_budget(index, hits, SEARCH_CORE_HYDRATION_BUDGET)
}

pub(in crate::commands::source_index) fn core_records_for_search_hits_with_budget(
    index: &VerifiedIndex,
    hits: &[SearchHit],
    budget: SearchCoreHydrationBudget,
) -> Result<HashMap<Uuid, CoreEventRecord>> {
    if budget.maximum_encoded_core_bytes == 0
        || budget.maximum_content_bytes == 0
        || budget.maximum_retained_body_bytes == 0
    {
        return Err(anyhow!("Core search hydration budgets must be positive"));
    }

    let event_ids = hits
        .iter()
        .map(|hit| hit.event.event_id.as_uuid())
        .collect::<Vec<_>>();
    let page_budget = CoreEventPageBudget::new(
        budget.maximum_encoded_core_bytes,
        budget.maximum_content_bytes,
    );
    // Resolve all selected addresses with one bounded Tantivy query. The
    // aggregate encoded/content ceilings bound complete records retained by
    // this batch; each record is then reduced to its presentation prefix.
    // This keeps top-200 memory bounded without issuing one index query per
    // result.
    let batch = index
        .core_events_by_ids_with_strict_budget(&event_ids, event_ids.len(), page_budget)?
        .ok_or_else(|| {
            search_core_hydration_budget_error(
                event_ids.first().copied().unwrap_or_else(Uuid::nil),
                SearchCoreHydrationBudgetStage::Decode,
                0,
                0,
                0,
                budget,
            )
        })?;
    let admitted_encoded_core_bytes = batch.encoded_core_bytes;
    let admitted_content_bytes = batch.content_bytes;
    let mut records = HashMap::with_capacity(hits.len());
    let mut retained_body_bytes = 0_usize;
    for (event_id, record) in event_ids.into_iter().zip(batch.items) {
        if record.event_id.as_uuid() != event_id {
            return Err(anyhow!(
                "pinned Core lookup returned an invalid record for search event {event_id}"
            ));
        }
        let (record, body_bytes) = search_core_presentation_projection(record)?;
        let next_retained_body_bytes =
            retained_body_bytes.checked_add(body_bytes).ok_or_else(|| {
                search_core_hydration_budget_error(
                    event_id,
                    SearchCoreHydrationBudgetStage::Retention,
                    admitted_encoded_core_bytes,
                    admitted_content_bytes,
                    retained_body_bytes,
                    budget,
                )
            })?;
        if next_retained_body_bytes > budget.maximum_retained_body_bytes {
            return Err(search_core_hydration_budget_error(
                event_id,
                SearchCoreHydrationBudgetStage::Retention,
                admitted_encoded_core_bytes,
                admitted_content_bytes,
                next_retained_body_bytes,
                budget,
            ));
        }
        retained_body_bytes = next_retained_body_bytes;
        if records.insert(event_id, record).is_some() {
            return Err(anyhow!("search result duplicated Core event {event_id}"));
        }
    }
    Ok(records)
}

fn search_core_presentation_projection(
    record: CoreEventRecord,
) -> Result<(CoreEventRecord, usize)> {
    let CoreEventRecord {
        event,
        mut core_record,
    } = record;
    let body = core_record
        .content
        .normalized_body
        .take()
        .filter(|body| !body.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "Core search event {} has no normalized body",
                event.event_id
            )
        })?;
    let body_prefix = body
        .chars()
        .take(SEARCH_CORE_BODY_PREFIX_CHARS)
        .collect::<String>();
    let retained_body_bytes = body_prefix.len();

    // The renderer currently accepts CoreEventRecord. Construct a valid,
    // ephemeral Core-owned presentation record so the complete body,
    // structured content, annotations, and repository observations are
    // dropped before the next result is decoded. This projection is never
    // exposed as complete Core data.
    let core_record = CoreRecord::new_selected(
        core_record.event_id,
        core_record.session_id,
        core_record.root_session_id,
        core_record.source,
        core_record.event_sequence,
        core_record.event_type,
        core_record.agent_type,
        core_record.is_primary,
        "search-presentation-v1",
        body_prefix,
    )?;
    Ok((CoreEventRecord { event, core_record }, retained_body_bytes))
}

fn search_core_hydration_budget_error(
    event_id: Uuid,
    stage: SearchCoreHydrationBudgetStage,
    admitted_encoded_core_bytes: usize,
    admitted_content_bytes: usize,
    retained_body_bytes: usize,
    budget: SearchCoreHydrationBudget,
) -> anyhow::Error {
    anyhow::Error::new(SearchCoreHydrationBudgetExceeded {
        event_id,
        stage,
        admitted_encoded_core_bytes,
        maximum_encoded_core_bytes: budget.maximum_encoded_core_bytes,
        admitted_content_bytes,
        maximum_content_bytes: budget.maximum_content_bytes,
        retained_body_bytes,
        maximum_retained_body_bytes: budget.maximum_retained_body_bytes,
    })
}
