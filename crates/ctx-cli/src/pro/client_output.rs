use std::path::Path;

use anyhow::Result;
use ctx_pro_host_protocol::CoreMaterializationReceipt;

use super::{protocol_error, ProClient, BATCH_TIMEOUT};

pub(crate) struct CoreMaterializationSyncOutcome {
    pub(crate) receipt: CoreMaterializationReceipt,
    pub(crate) did_work: bool,
}

/// Synchronizes Pro from one already-published, generation-pinned Core feed.
pub(crate) fn sync_core_materialization(
    data_root: &Path,
    index: &ctx_history_index::VerifiedIndex,
) -> Result<CoreMaterializationSyncOutcome> {
    core_materialization_feed::sync_generation_pinned_core(data_root, index).map(|report| {
        CoreMaterializationSyncOutcome {
            receipt: report.receipt,
            did_work: !report.replayed,
        }
    })
}

#[path = "client_output/core_materialization_feed.rs"]
mod core_materialization_feed;
