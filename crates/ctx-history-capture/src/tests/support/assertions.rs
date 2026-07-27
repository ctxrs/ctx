use crate::{ProviderImportSummary, MAX_PROVIDER_JSONL_LINE_BYTES};
use ctx_history_core::{CaptureProvider, Event, EventRole, EventType};
use ctx_history_store::Store;

pub(in crate::tests) fn assert_event_type_count(
    events: &[Event],
    event_type: EventType,
    expected: usize,
) {
    let actual = events
        .iter()
        .filter(|event| event.event_type == event_type)
        .count();
    let event_types = events
        .iter()
        .map(|event| event.event_type.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        expected,
        "expected {expected} {} event(s), found {actual} in {event_types:?}",
        event_type.as_str()
    );
}

pub(in crate::tests) fn assert_event_with_role(
    events: &[Event],
    event_type: EventType,
    role: EventRole,
) {
    assert!(
        events
            .iter()
            .any(|event| event.event_type == event_type && event.role == Some(role)),
        "missing {} event with {} role",
        event_type.as_str(),
        role.as_str()
    );
}

pub(in crate::tests) fn assert_events_have_provider_citations(store: &Store, events: &[Event]) {
    assert!(!events.is_empty(), "expected at least one event");
    for event in events {
        let source_id = event
            .capture_source_id
            .unwrap_or_else(|| panic!("event {} is missing a capture source", event.id));
        let source = store.get_capture_source(source_id).unwrap_or_else(|error| {
            panic!("event {} has an invalid capture source: {error}", event.id)
        });
        assert!(
            source
                .descriptor
                .source_format
                .as_deref()
                .is_some_and(|source_format| !source_format.is_empty()),
            "event {} capture source is missing its source format",
            event.id
        );
        assert!(
            source
                .descriptor
                .source_identity
                .as_deref()
                .is_some_and(|source_identity| !source_identity.is_empty()),
            "event {} capture source is missing its source identity",
            event.id
        );
    }
}

pub(in crate::tests) fn assert_search_hits_provider(
    store: &Store,
    query: &str,
    provider: CaptureProvider,
) {
    let hits = store.search_event_hits(query, 10).unwrap();
    assert!(
        hits.iter().any(|hit| hit.provider == Some(provider)),
        "expected {provider:?} search hit for {query:?}, got {hits:?}"
    );
}

pub(in crate::tests) fn assert_search_misses(store: &Store, query: &str) {
    let hits = store.search_event_hits(query, 10).unwrap();
    assert!(
        hits.is_empty(),
        "expected no hits for {query:?}, got {hits:?}"
    );
}

pub(in crate::tests) fn assert_structural_oversize_failure(
    summary: &ProviderImportSummary,
    line: usize,
) {
    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.failures.len(), 1);
    assert_eq!(summary.failures[0].line, line);
    assert_eq!(
        summary.failures[0].error,
        format!(
            "provider record exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit (observed {} bytes)",
            // `oversized_jsonl_line` writes limit + 1 payload bytes plus its
            // terminating newline, and structural diagnostics report all
            // consumed source bytes.
            MAX_PROVIDER_JSONL_LINE_BYTES + 2
        )
    );
}
