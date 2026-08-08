use std::{
    path::Path,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use ctx_history_index::VerifiedIndex;
use ctx_pro_host_protocol::{CoreMaterializationFinalizationPending, CoreMaterializationReceipt};

use crate::pro::{
    preflight_core_materialization, reconstruct_core_finalization_generation_lease,
    release_core_finalization_generation_lease, sync_core_materialization,
    CoreMaterializationSyncOutcome,
};

use super::super::source_backed_refresh_coordinator::{
    nonzero_duration_micros, open_verified_index, source_backed_index_root,
};
use super::recheck::{
    complete as complete_observed_recheck, read as read_recheck_request,
    read_unlocked as read_recheck_request_unlocked, with_lock as with_recheck_lock,
};
use super::status::{
    persist_status, read_status, SourceBackedProCatchUpError, SourceBackedProCatchUpState,
    SourceBackedProCatchUpStatus,
};
use super::{SourceBackedProCatchUpRun, SourceBackedProCoreAuthority};

const SOURCE_BACKED_PRO_CATCH_UP_POLL_INTERVAL: Duration = Duration::from_millis(50);
const SOURCE_BACKED_PRO_CATCH_UP_WAIT_TIMEOUT: Duration = Duration::from_secs(60 * 60);

pub(in crate::semantic) fn run_after_core_publication(
    data_root: &Path,
    core_generation_id: &str,
    authority: SourceBackedProCoreAuthority<'_>,
    should_yield: &mut dyn FnMut() -> bool,
) -> Result<SourceBackedProCatchUpRun> {
    if authority.generation_id() != core_generation_id {
        return record_preflight_error(
            data_root,
            core_generation_id,
            SourceBackedProCatchUpError::GenerationMismatch {
                expected: core_generation_id.to_owned(),
                authority: authority.generation_id().to_owned(),
                surface: authority.surface(),
            },
        );
    }
    run_with(
        data_root,
        core_generation_id,
        ProCatchUpAuthority {
            generation_id: Some(authority.generation_id()),
            verified_index: Some(authority.verified_index()),
        },
        preflight_core_materialization,
        |data_root, index| {
            Ok(
                match sync_core_materialization(data_root, index, should_yield)? {
                    CoreMaterializationSyncOutcome::Finished {
                        receipt,
                        did_work,
                        helper_artifact_sha256,
                    } => ProCatchUpSyncOutcome::Finished {
                        receipt,
                        did_work,
                        helper_artifact_sha256,
                    },
                    CoreMaterializationSyncOutcome::FinalizationPending { pending } => {
                        ProCatchUpSyncOutcome::FinalizationPending { pending }
                    }
                },
            )
        },
    )
}

pub(super) struct ProCatchUpAuthority<'a> {
    pub(super) generation_id: Option<&'a str>,
    pub(super) verified_index: Option<&'a VerifiedIndex>,
}

pub(super) enum ProCatchUpSyncOutcome {
    Finished {
        receipt: CoreMaterializationReceipt,
        did_work: bool,
        helper_artifact_sha256: String,
    },
    FinalizationPending {
        pending: CoreMaterializationFinalizationPending,
    },
}

pub(super) fn run_with<Preflight, Sync>(
    data_root: &Path,
    core_generation_id: &str,
    authority: ProCatchUpAuthority<'_>,
    preflight: Preflight,
    sync: Sync,
) -> Result<SourceBackedProCatchUpRun>
where
    Preflight: FnOnce(&Path) -> Result<()>,
    Sync: FnOnce(&Path, &VerifiedIndex) -> Result<ProCatchUpSyncOutcome>,
{
    // Capture the exact target this run is allowed to satisfy. The completion
    // path additionally requires the helper identity reported by this exact
    // materialization session, so an old helper cannot clear a newer intent.
    let observed_recheck = read_recheck_request(data_root)?;
    let prior = read_status(data_root);
    let attempts = next_attempt(prior.as_ref(), core_generation_id);
    let attempt_started = Instant::now();
    let pending = prior
        .as_ref()
        .filter(|status| status.core_generation_id == core_generation_id)
        .and_then(|status| status.finalization_progress.clone())
        .map(|progress| {
            SourceBackedProCatchUpStatus::pending(core_generation_id, attempts).finalizing(progress)
        })
        .unwrap_or_else(|| SourceBackedProCatchUpStatus::pending(core_generation_id, attempts));
    persist_status(data_root, &pending)?;

    let result: std::result::Result<ProCatchUpSyncOutcome, SourceBackedProCatchUpError> = (|| {
        if let Some(generation_id) = authority.generation_id {
            require_generation(
                core_generation_id,
                generation_id,
                "retained Core generation pin",
            )?;
        }
        preflight(data_root).map_err(SourceBackedProCatchUpError::projection)?;
        let opened_index: VerifiedIndex;
        let index = match authority.verified_index {
            Some(index) => index,
            None => {
                opened_index = open_exact_index(data_root, core_generation_id)?;
                &opened_index
            }
        };
        require_generation(
            core_generation_id,
            index.generation_id(),
            "pinned VerifiedIndex",
        )?;
        let outcome = sync(data_root, index).map_err(SourceBackedProCatchUpError::projection)?;
        match &outcome {
            ProCatchUpSyncOutcome::Finished { receipt, .. } => require_generation(
                core_generation_id,
                &receipt.core_generation_id,
                "Pro Core materialization receipt",
            )?,
            ProCatchUpSyncOutcome::FinalizationPending { pending } => {
                require_generation(
                    core_generation_id,
                    &pending.progress.core_generation_id,
                    "Pro Core finalization progress",
                )?;
                // Production acquires this before sending Finish/Continue. This
                // idempotent reconstruction also covers restart migration and
                // injected consumers while the exact generation is still pinned.
                reconstruct_core_finalization_generation_lease(data_root, &pending.progress)
                    .map_err(SourceBackedProCatchUpError::projection)?;
            }
        }
        Ok(outcome)
    })();

    match result {
        Ok(ProCatchUpSyncOutcome::Finished {
            receipt,
            did_work,
            helper_artifact_sha256,
        }) => {
            let completed = pending
                .completed(receipt.core_generation_id)
                .with_duration(nonzero_duration_micros(attempt_started.elapsed()));
            persist_status(data_root, &completed)?;
            // Status is the durable job authority. Publish terminal state before
            // dropping the target lease so a crash can only retain, never reclaim
            // an unfinished target.
            release_core_finalization_generation_lease(data_root, Some(core_generation_id))?;
            complete_observed_recheck(
                data_root,
                observed_recheck.as_ref(),
                &helper_artifact_sha256,
            )?;
            Ok(SourceBackedProCatchUpRun {
                status: completed.to_json()?,
                did_work,
                continuation_pending: false,
            })
        }
        Ok(ProCatchUpSyncOutcome::FinalizationPending {
            pending: finalization,
        }) => {
            let did_work = !finalization.replayed;
            let finalizing = pending
                .finalizing(finalization.progress)
                .with_duration(nonzero_duration_micros(attempt_started.elapsed()));
            persist_status(data_root, &finalizing)?;
            Ok(SourceBackedProCatchUpRun {
                status: finalizing.to_json()?,
                did_work,
                continuation_pending: true,
            })
        }
        Err(error) => {
            let terminal = !error.retryable();
            let failed = pending
                .error(error)
                .with_duration(nonzero_duration_micros(attempt_started.elapsed()));
            persist_status(data_root, &failed)?;
            if terminal {
                release_core_finalization_generation_lease(data_root, Some(core_generation_id))?;
            }
            Ok(SourceBackedProCatchUpRun {
                status: failed.to_json()?,
                did_work: false,
                continuation_pending: false,
            })
        }
    }
}

fn record_preflight_error(
    data_root: &Path,
    core_generation_id: &str,
    error: SourceBackedProCatchUpError,
) -> Result<SourceBackedProCatchUpRun> {
    let prior = read_status(data_root);
    let attempts = next_attempt(prior.as_ref(), core_generation_id);
    let failed = SourceBackedProCatchUpStatus::pending(core_generation_id, attempts).error(error);
    persist_status(data_root, &failed)?;
    Ok(SourceBackedProCatchUpRun {
        status: failed.to_json()?,
        did_work: false,
        continuation_pending: false,
    })
}

fn next_attempt(prior: Option<&SourceBackedProCatchUpStatus>, core_generation_id: &str) -> u64 {
    prior
        .filter(|status| status.core_generation_id == core_generation_id)
        .map(|status| status.attempts.saturating_add(1))
        .unwrap_or(1)
}

fn require_generation(
    expected: &str,
    authority: &str,
    surface: &'static str,
) -> std::result::Result<(), SourceBackedProCatchUpError> {
    if authority == expected {
        return Ok(());
    }
    Err(SourceBackedProCatchUpError::GenerationMismatch {
        expected: expected.to_owned(),
        authority: authority.to_owned(),
        surface,
    })
}

fn open_exact_index(
    data_root: &Path,
    core_generation_id: &str,
) -> std::result::Result<VerifiedIndex, SourceBackedProCatchUpError> {
    let index_root = source_backed_index_root(data_root);
    let index = open_verified_index(&index_root)
        .with_context(|| format!("open verified Core index {}", index_root.display()))
        .map_err(|error| SourceBackedProCatchUpError::IndexUnavailable(format!("{error:#}")))?;
    require_generation(
        core_generation_id,
        index.generation_id(),
        "pinned VerifiedIndex",
    )?;
    Ok(index)
}

/// Waits for the daemon-owned Pro projection of one exact Core generation.
///
/// This is deliberately a status-only seam. The CLI must not reread provider
/// sources or materialize Pro itself; that work remains owned by the daemon
/// tick that retained the generation-bound Core authority.
pub(crate) fn wait_for_completed_generation(
    data_root: &Path,
    core_generation_id: &str,
) -> Result<()> {
    wait_for_completed_generation_with(
        data_root,
        core_generation_id,
        SOURCE_BACKED_PRO_CATCH_UP_WAIT_TIMEOUT,
        || thread::sleep(SOURCE_BACKED_PRO_CATCH_UP_POLL_INTERVAL),
    )
}

pub(super) fn wait_for_completed_generation_with(
    data_root: &Path,
    core_generation_id: &str,
    timeout: Duration,
    mut wait: impl FnMut(),
) -> Result<()> {
    let started = Instant::now();
    loop {
        let (pending_recheck, status) = with_recheck_lock(data_root, || {
            Ok((
                read_recheck_request_unlocked(data_root)?.is_some(),
                read_status(data_root),
            ))
        })?;
        if let Some(status) = status {
            if !pending_recheck && status.is_completed_for(core_generation_id) {
                return Ok(());
            }
            if status.core_generation_id == core_generation_id
                && status.status == SourceBackedProCatchUpState::Error
            {
                let code = status.error_code.as_deref().unwrap_or("not_materialized");
                let detail = status
                    .last_error
                    .as_deref()
                    .unwrap_or("daemon source-backed Pro catch-up failed");
                anyhow::bail!("{code}: {detail}");
            }
        }
        if started.elapsed() >= timeout {
            anyhow::bail!(
                "not_materialized: timed out waiting for daemon source-backed Pro generation {core_generation_id}"
            );
        }
        wait();
    }
}
