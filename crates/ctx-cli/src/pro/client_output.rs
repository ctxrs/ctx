use std::path::Path;

use anyhow::Result;

use super::{protocol_error, ProClient, BATCH_TIMEOUT};

/// Synchronizes Pro from one already-published, generation-pinned Core feed.
pub(crate) fn sync_core_materialization(
    data_root: &Path,
    index: &ctx_history_index::VerifiedIndex,
) -> Result<ctx_pro_host_protocol::CoreMaterializationReceipt> {
    core_materialization_feed::sync_generation_pinned_core(data_root, index)
        .map(|report| report.receipt)
}

#[path = "client_output/core_materialization_feed.rs"]
mod core_materialization_feed;
