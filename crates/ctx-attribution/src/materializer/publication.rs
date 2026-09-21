#[path = "publication/active.rs"]
mod active;
#[path = "publication/build.rs"]
mod build;
#[path = "publication/gc.rs"]
mod gc;
#[path = "publication/io.rs"]
mod io;

#[cfg(test)]
pub(super) use active::event_index_reader_open_count;
#[allow(unused_imports)]
pub(super) use active::{
    ActiveControlSnapshot, ActiveGeneration, ObservedEvent, active_event_page, load_active,
    load_active_control, load_active_for_materializer, lookup_event_metadata,
};
pub(super) use build::DirectCandidate;
#[allow(unused_imports)]
#[cfg(test)]
pub(super) use build::{
    PublicationTransactionTestFault, install_publication_test_hook,
    install_publication_transaction_failure_test_hook,
};
pub(super) use gc::{cleanup_candidate_orphans, remove_unreferenced_segments};
#[cfg(test)]
pub(super) use io::write_event_index_segment_for_test;

#[cfg(test)]
pub(super) fn write_source_segment_for_test<T: serde::Serialize>(
    root: &std::path::Path,

    publication_generation: u64,
    role: u32,
    ordinal: u32,
    value: &T,
) -> Result<crate::graph::segment::SegmentRef, super::SegmentMaterializerError> {
    io::write_json_segment(root, publication_generation, role, ordinal, value)
}

#[cfg(test)]
pub(super) fn open_event_index_for_test(
    root: &std::path::Path,
    reference: &crate::graph::segment::SegmentRef,
) -> Result<crate::graph::segment::EventIndexReader, super::SegmentMaterializerError> {
    io::open_event_index(root, reference)
}
