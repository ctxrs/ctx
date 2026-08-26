use ctx_history_index::{
    CompiledSearchFilter, EventRecord, EventSearchFilters, LexicalExecution, LexicalMode,
    VerifiedIndex,
};

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct HydratedEventSearchCandidate {
    pub(crate) event: EventRecord,
}

fn execute_event_candidates(
    index: &VerifiedIndex,
    mode: LexicalMode<'_>,
    filters: &EventSearchFilters,
    limit: usize,
) -> Vec<HydratedEventSearchCandidate> {
    let filter = CompiledSearchFilter::compile(filters.clone()).unwrap();
    let batch = index
        .execute_lexical(LexicalExecution::new(mode, &filter, limit))
        .unwrap()
        .batch;
    assert!(
        batch.complete,
        "test lexical execution must complete: {:?}",
        batch.exhaustion
    );
    batch
        .candidates
        .into_iter()
        .map(|candidate| {
            let event = index
                .event_by_id(candidate.event.event_id)
                .unwrap()
                .expect("selected lexical event must hydrate");
            assert_eq!(
                event.event_id.digest(),
                candidate.event.event_identity_digest,
                "selected lexical event must preserve its exact identity"
            );
            HydratedEventSearchCandidate { event }
        })
        .collect()
}

pub(crate) fn search_event_candidates(
    index: &VerifiedIndex,
    natural_text: &str,
    limit: usize,
) -> Vec<HydratedEventSearchCandidate> {
    search_event_candidates_with_filters(
        index,
        &[natural_text],
        &EventSearchFilters::default(),
        limit,
    )
}

pub(crate) fn search_event_candidates_with_filters(
    index: &VerifiedIndex,
    natural_texts: &[&str],
    filters: &EventSearchFilters,
    limit: usize,
) -> Vec<HydratedEventSearchCandidate> {
    execute_event_candidates(index, LexicalMode::Search(natural_texts), filters, limit)
}
