use std::path::Path;

use anyhow::Result;
use ctx_pro_host_protocol::{CoreMaterializationFinalizationPending, CoreMaterializationReceipt};

use super::{protocol_error, ProClient, BATCH_TIMEOUT};

pub(crate) enum CoreMaterializationSyncOutcome {
    Finished {
        receipt: CoreMaterializationReceipt,
        did_work: bool,
        helper_artifact_sha256: String,
    },
    FinalizationPending {
        pending: CoreMaterializationFinalizationPending,
    },
}

/// Synchronizes Pro from one already-published, generation-pinned Core feed.
pub(crate) fn sync_core_materialization(
    data_root: &Path,
    index: &ctx_history_index::VerifiedIndex,
) -> Result<CoreMaterializationSyncOutcome> {
    match core_materialization_feed::sync_generation_pinned_core(data_root, index)? {
        core_materialization_feed::CoreMaterializationSyncProgress::Finished(report) => {
            Ok(CoreMaterializationSyncOutcome::Finished {
                did_work: !report.replayed,
                receipt: report.receipt,
                helper_artifact_sha256: report.helper_artifact_sha256,
            })
        }
        core_materialization_feed::CoreMaterializationSyncProgress::FinalizationPending(
            pending,
        ) => Ok(CoreMaterializationSyncOutcome::FinalizationPending { pending }),
    }
}

#[path = "client_output/core_materialization_feed.rs"]
mod core_materialization_feed;
pub(crate) use core_materialization_feed::{
    core_finalization_generation_lease, reconstruct_core_finalization_generation_lease,
    release_core_finalization_generation_lease, validate_core_finalization_generation_lease,
};
