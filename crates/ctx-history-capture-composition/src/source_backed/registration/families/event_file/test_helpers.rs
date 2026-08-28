use super::*;
use ctx_history_core::CoreRecord;
use ctx_history_index::VerifiedIndex;
use std::path::Path;

pub(super) fn indexed_bodies(index: &Path, receipt: &SourceBackedRefreshReceipt) -> Vec<String> {
    let verified = VerifiedIndex::open_pinned(index).unwrap();
    let mut bodies = receipt
        .sources
        .iter()
        .flat_map(|source| {
            verified
                .core_source_event_page(source.observation().source(), None, 32)
                .unwrap()
                .items
                .into_iter()
                .map(|event| event.core_record.content.meaningful_text().to_owned())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    bodies.sort();
    bodies
}

pub(super) fn indexed_events(
    index: &Path,
    receipt: &SourceBackedRefreshReceipt,
) -> Vec<CoreRecord> {
    let verified = VerifiedIndex::open_pinned(index).unwrap();
    let mut events = receipt
        .sources
        .iter()
        .flat_map(|source| {
            verified
                .core_source_event_page(source.observation().source(), None, 64)
                .unwrap()
                .items
                .into_iter()
                .map(|event| event.core_record)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    events.sort_by(|left, right| {
        left.source
            .exact_descriptor_digest()
            .cmp(&right.source.exact_descriptor_digest())
            .then_with(|| left.event_sequence.cmp(&right.event_sequence))
    });
    events
}
