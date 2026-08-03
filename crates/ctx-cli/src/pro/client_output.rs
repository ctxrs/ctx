use std::path::Path;

use anyhow::Result;
use ctx_pro_host_protocol::CoreMaterializationReceipt;

use super::{protocol_error, ProClient, BATCH_TIMEOUT};

pub(crate) struct CoreMaterializationSyncOutcome {
    pub(crate) receipt: CoreMaterializationReceipt,
    pub(crate) did_work: bool,
    pub(crate) helper_artifact_sha256: String,
}

/// Synchronizes Pro from one already-published, generation-pinned Core feed.
pub(crate) fn sync_core_materialization(
    data_root: &Path,
    index: &ctx_history_index::VerifiedIndex,
) -> Result<CoreMaterializationSyncOutcome> {
    let report = core_materialization_feed::sync_generation_pinned_core(data_root, index)?;
    Ok(CoreMaterializationSyncOutcome {
        did_work: !report.replayed,
        receipt: report.receipt,
        helper_artifact_sha256: report.helper_artifact_sha256,
    })
}

#[path = "client_output/core_materialization_feed.rs"]
mod core_materialization_feed;
