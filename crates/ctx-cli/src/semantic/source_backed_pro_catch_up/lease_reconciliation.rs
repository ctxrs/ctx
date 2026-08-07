use std::path::Path;

use anyhow::Result;
use ctx_history_index::{release_generation_retention_lease, GenerationRetentionLease};

use crate::pro::{
    core_finalization_generation_lease, reconstruct_core_finalization_generation_lease,
    release_core_finalization_generation_lease, validate_core_finalization_generation_lease,
};

use super::super::source_backed_refresh_coordinator::source_backed_index_root;
use super::status::{
    persist_status, require_durable_status, SourceBackedProCatchUpError,
    SourceBackedProCatchUpState, SourceBackedProCatchUpStatus,
    SOURCE_BACKED_PRO_CATCH_UP_SCHEMA_VERSION,
};

/// Reconciles the single durable Core-generation lease with the daemon's
/// durable Pro job before the scheduler attempts to pin that job's target.
pub(in crate::semantic) fn reconcile_core_finalization_generation_lease(
    data_root: &Path,
) -> Result<()> {
    let lease = core_finalization_generation_lease(data_root)?;
    let Some(status) = require_durable_status(data_root)? else {
        if lease.is_some() {
            cancel_core_finalization_generation_lease(
                data_root,
                "stale Pro finalization lease had no durable job",
            )?;
        }
        return Ok(());
    };
    if status.schema_version != SOURCE_BACKED_PRO_CATCH_UP_SCHEMA_VERSION
        || status.owner != "daemon"
        || status.kind != "source_backed_pro_catch_up"
    {
        if lease.is_some() {
            cancel_core_finalization_generation_lease(
                data_root,
                "durable Pro finalization job identity was invalid",
            )?;
            return Ok(());
        }
        anyhow::bail!(
            "invalid_response: durable source-backed Pro catch-up job identity is invalid"
        );
    }

    let terminal = !status.pending
        || status.status == SourceBackedProCatchUpState::Completed
        || (status.status == SourceBackedProCatchUpState::Error && !status.retryable);
    if terminal {
        if let Some(lease) = lease {
            release_observed_generation_lease(data_root, &lease)?;
        }
        return Ok(());
    }

    if let Some(progress) = &status.finalization_progress {
        let validation = (|| -> Result<()> {
            progress
                .validate()
                .map_err(|error| anyhow::anyhow!("invalid_response: {}", error.message))?;
            if progress.core_generation_id != status.core_generation_id {
                anyhow::bail!(
                    "invalid_response: durable Pro finalization progress targets a foreign Core generation"
                );
            }
            if lease.is_some() {
                validate_core_finalization_generation_lease(data_root, progress)
            } else {
                reconstruct_core_finalization_generation_lease(data_root, progress)
            }
        })();
        if let Err(error) = validation {
            if lease.is_some() {
                cancel_core_finalization_generation_lease(data_root, &error.to_string())?;
            } else {
                let failed = status
                    .clone()
                    .error(SourceBackedProCatchUpError::projection(error));
                persist_status(data_root, &failed)?;
            }
        }
        return Ok(());
    }

    let Some(lease) = lease else {
        return Ok(());
    };
    if lease.generation_id() != status.core_generation_id {
        cancel_core_finalization_generation_lease(
            data_root,
            "durable Core generation lease targeted a foreign Pro job",
        )?;
        return Ok(());
    }
    if status.pending && status.status != SourceBackedProCatchUpState::Completed {
        // Finish can commit while its response is lost. Until helper status
        // supplies the finalization tuple, the generation-bound pending job is
        // the only durable identity available and the bounded lease must remain.
        return Ok(());
    }
    release_core_finalization_generation_lease(data_root, Some(&status.core_generation_id))?;
    Ok(())
}

pub(crate) fn cancel_core_finalization_generation_lease(
    data_root: &Path,
    reason: &str,
) -> Result<bool> {
    let Some(lease) = core_finalization_generation_lease(data_root)? else {
        return Ok(false);
    };
    let attempts = require_durable_status(data_root)?
        .as_ref()
        .filter(|status| status.core_generation_id == lease.generation_id())
        .map(|status| status.attempts)
        .unwrap_or(1);
    let cancelled =
        SourceBackedProCatchUpStatus::pending(lease.generation_id(), attempts).cancelled(reason);
    persist_status(data_root, &cancelled)?;
    release_observed_generation_lease(data_root, &lease)
}

fn release_observed_generation_lease(
    data_root: &Path,
    lease: &GenerationRetentionLease,
) -> Result<bool> {
    release_generation_retention_lease(source_backed_index_root(data_root), lease).map_err(|error| {
        anyhow::anyhow!(
            "invalid_response: durable Core generation lease changed before terminal release: {error}"
        )
    })
}
