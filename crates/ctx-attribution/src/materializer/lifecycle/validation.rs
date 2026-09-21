use super::*;

pub(crate) fn validate_event_delta(
    store: &mut SegmentMaterializer,
    state: &mut super::super::model::CandidateState,
    cursor: &super::super::reconciliation_cursor::ReconciliationCursor,
    prepared: &crate::core_materialization::PreparedCoreEventDeltaPage,
    materializer_revision: &str,
) -> Result<ValidatedEventPage, SegmentMaterializerError> {
    let page = prepared.page();
    validate_prepared_request(prepared)?;
    let source_id = source_storage_id(page.reconciliation.delta.source());
    ensure_current_frontier(store, state, cursor, &page.reconciliation)?;
    let candidate = require_candidate(
        state,
        &page.materialization_id,
        &page.core_generation_id,
        materializer_revision,
    )?;
    if !candidate.source_terminal {
        return Err(SegmentMaterializerError::Conflict);
    }
    let expected_index = candidate
        .pending_sources
        .iter()
        .map(|frontier| frontier.materialize_index)
        .min();
    let frontier = require_frontier(candidate, &page.reconciliation)?;
    if frontier.next_event_page != page.page_index
        || expected_index != Some(frontier.materialize_index)
        || frontier
            .last_event_order_id
            .as_deref()
            .zip(page.deltas.first())
            .is_some_and(|(prior, first)| prior >= event_order_id(first.event_id()).as_str())
    {
        return Err(SegmentMaterializerError::Conflict);
    }

    let mut additions = 0_u32;
    let mut replacements = 0_u32;
    let mut tombstones = 0_u32;
    let mut coverage = candidate.coverage.clone();
    let mut event_count = candidate.event_count;
    let mut source_event_count = frontier.event_count;
    let mut mutations = Vec::with_capacity(page.deltas.len());
    for delta in &page.deltas {
        let prior = if let Some(active) = store
            .active
            .as_mut()
            .filter(|active| !active.requires_clean_rebuild)
        {
            super::super::publication::lookup_event_metadata(
                active,
                &store.root,
                page.reconciliation.delta.source(),
                delta.event_id(),
            )?
        } else {
            None
        };
        match delta {
            CoreEventDelta::Added(record) => {
                if prior.is_some() {
                    return Err(SegmentMaterializerError::Conflict);
                }
                let event = prepare_event(&source_id, record, prepared)?;
                coverage = add_coverage(&coverage, &event.prepared.coverage)?;
                event_count = event_count
                    .checked_add(1)
                    .ok_or(SegmentMaterializerError::Bounds)?;
                source_event_count = source_event_count
                    .checked_add(1)
                    .ok_or(SegmentMaterializerError::Bounds)?;
                mutations.push(SegmentPublicationMutation::Added(event));
                additions = additions
                    .checked_add(1)
                    .ok_or(SegmentMaterializerError::Bounds)?;
            }
            CoreEventDelta::Replaced(replacement) => {
                let prior = require_prior(prior, &replacement.prior_core_record_sha256)?;
                let event = prepare_event(&source_id, &replacement.record, prepared)?;
                coverage = subtract_coverage(&coverage, &prior.coverage)?;
                coverage = add_coverage(&coverage, &event.prepared.coverage)?;
                mutations.push(SegmentPublicationMutation::Replaced {
                    tombstone: publication_tombstone(&prior),
                    replacement: event,
                });
                replacements = replacements
                    .checked_add(1)
                    .ok_or(SegmentMaterializerError::Bounds)?;
            }
            CoreEventDelta::Tombstoned(tombstone) => {
                let prior = require_prior(prior, &tombstone.prior_core_record_sha256)?;
                coverage = subtract_coverage(&coverage, &prior.coverage)?;
                event_count = event_count
                    .checked_sub(1)
                    .ok_or(SegmentMaterializerError::Conflict)?;
                source_event_count = source_event_count
                    .checked_sub(1)
                    .ok_or(SegmentMaterializerError::Conflict)?;
                mutations.push(SegmentPublicationMutation::Tombstoned(
                    publication_tombstone(&prior),
                ));
                tombstones = tombstones
                    .checked_add(1)
                    .ok_or(SegmentMaterializerError::Bounds)?;
            }
        }
    }
    if page.terminal {
        match &page.reconciliation.delta {
            CoreSourceDelta::Present(state) if state.event_count != source_event_count => {
                return Err(SegmentMaterializerError::Conflict);
            }
            CoreSourceDelta::Removed(_) if source_event_count != 0 => {
                return Err(SegmentMaterializerError::Conflict);
            }
            _ => {}
        }
    }
    let response = CoreEventDeltaPageApplied {
        materialization_id: page.materialization_id.clone(),
        core_generation_id: page.core_generation_id.clone(),
        source: page.reconciliation.delta.source().clone(),
        page_index: page.page_index,
        additions,
        replacements,
        tombstones,
        terminal: page.terminal,
        replayed: false,
    };
    let next_materialize_index = if page.terminal {
        Some(
            state
                .next_materialize_index
                .checked_add(1)
                .ok_or(SegmentMaterializerError::Bounds)?,
        )
    } else {
        None
    };
    let candidate = &mut state.control;
    let index = candidate
        .pending_sources
        .iter()
        .position(|frontier| frontier.source_id == source_id)
        .ok_or(SegmentMaterializerError::Conflict)?;
    let page_mutations = u64::from(additions)
        .checked_add(u64::from(replacements))
        .and_then(|count| count.checked_add(u64::from(tombstones)))
        .ok_or(SegmentMaterializerError::Bounds)?;
    candidate.coverage = coverage;
    candidate.event_count = event_count;
    candidate.event_pages = candidate
        .event_pages
        .checked_add(1)
        .ok_or(SegmentMaterializerError::Bounds)?;
    candidate.event_mutations = candidate
        .event_mutations
        .checked_add(page_mutations)
        .ok_or(SegmentMaterializerError::Bounds)?;
    if page.terminal {
        candidate.pending_sources.remove(index);
    } else {
        let frontier = candidate
            .pending_sources
            .get_mut(index)
            .ok_or(SegmentMaterializerError::Conflict)?;
        frontier.next_event_page = frontier
            .next_event_page
            .checked_add(1)
            .ok_or(SegmentMaterializerError::Bounds)?;
        frontier.last_event_order_id = page
            .deltas
            .last()
            .map(|delta| event_order_id(delta.event_id()));
        frontier.event_mutations = frontier
            .event_mutations
            .checked_add(page_mutations)
            .ok_or(SegmentMaterializerError::Bounds)?;
        frontier.event_count = source_event_count;
    }
    let graph_generation = candidate.graph_generation;
    if let Some(next_materialize_index) = next_materialize_index {
        state.next_materialize_index = next_materialize_index;
    }
    let projection_source = match &page.reconciliation.delta {
        CoreSourceDelta::Present(state) => state.clone(),
        CoreSourceDelta::Removed(_) => store
            .active
            .as_ref()
            .and_then(|active| active.sources.get(&source_id))
            .map(|source| source.state.clone())
            .ok_or(SegmentMaterializerError::Conflict)?,
    };
    let event_identities = mutation_identities(&page.deltas, &mutations)?;
    Ok(ValidatedEventPage {
        output: SegmentCorePageOutput {
            materialization_id: page.materialization_id.clone(),
            graph_generation,
            request_sha256: prepared.request_sha256().to_owned(),
            effect: response,
            mutations,
        },
        event_identities,
        projection_source,
    })
}
