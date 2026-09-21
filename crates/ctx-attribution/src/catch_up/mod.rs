mod feed;
mod worker_budget;
pub use feed::{CoreMaterializationSyncOutcome, sync_generation_pinned_core};
#[cfg(test)]
pub(crate) fn open_exact_core_snapshot(
    data_root: &std::path::Path,
    generation: &str,
) -> ctx_history_snapshot_reader::Result<ctx_history_snapshot_reader::CoreSnapshot> {
    ctx_history_snapshot_reader::CoreSnapshot::open(
        data_root,
        generation,
        &ctx_history_snapshot_reader::SnapshotContract::current()?,
    )
}
