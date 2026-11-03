//! Test-only metadata I/O, separate from artifact hashing, logical verification
//! and source reads. Counts successful reads (including bytes later rejected by
//! validation), not physical disk traffic or memory-mapped page faults.
//! Counters belong to the executing thread; worker activity must be collected
//! on that worker rather than inferred from a coordinator's snapshot.

use std::cell::Cell;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadActivity {
    pub operations: u64,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MetadataReadActivity {
    pub manifest: ReadActivity,
    pub certification: ReadActivity,
    pub pointer: ReadActivity,
    pub tantivy_control: ReadActivity,
}

thread_local! {
    static READS: Cell<MetadataReadActivity> = Cell::new(MetadataReadActivity::default());
}

pub fn reset_metadata_read_activity() {
    READS.set(MetadataReadActivity::default());
}

pub fn metadata_read_activity() -> MetadataReadActivity {
    READS.get()
}

pub(crate) fn record(field: fn(&mut MetadataReadActivity) -> &mut ReadActivity, bytes: usize) {
    let mut reads = READS.get();
    let activity = field(&mut reads);
    activity.operations += 1;
    activity.bytes += bytes as u64;
    READS.set(reads);
}
