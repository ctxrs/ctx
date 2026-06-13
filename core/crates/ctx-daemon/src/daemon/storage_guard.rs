use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;

use ctx_storage_admission::{
    storage_emergency_message, StorageGuardStatus, STORAGE_GUARD_MONITOR_INTERVAL,
};

use crate::daemon::DaemonState;

#[cfg(test)]
const GIB: u64 = ctx_storage_admission::STORAGE_BYTES_GIB;
#[cfg(test)]
const MIB: u64 = ctx_storage_admission::STORAGE_BYTES_MIB;

mod observations;
mod publication;

use observations::{collect_observed_paths, sample_storage_disks};
#[cfg(test)]
use publication::dispatch_storage_emergency_interrupt;
use publication::{emit_reserve_warnings, publish_storage_guard_snapshot};

pub fn spawn_storage_guard(state: Arc<DaemonState>) {
    let mut shutdown_rx = state.core.shutdown_tx.subscribe();
    tokio::spawn(async move {
        if let Err(err) = evaluate_storage_guard(&state, &[]).await {
            tracing::warn!("storage guard initial evaluation failed: {err:#}");
        }

        loop {
            tokio::select! {
                _ = shutdown_rx.recv() => break,
                _ = tokio::time::sleep(STORAGE_GUARD_MONITOR_INTERVAL) => {
                    if let Err(err) = evaluate_storage_guard(&state, &[]).await {
                        tracing::warn!("storage guard tick failed: {err:#}");
                    }
                }
            }
        }
    });
}

pub async fn preflight_turn_start(state: &Arc<DaemonState>, workdir: &Path) -> Result<()> {
    let current = state.storage_guard_snapshot();
    if current.is_emergency() {
        anyhow::bail!(storage_emergency_message(current.active.as_ref()));
    }
    let snapshot = refresh_preflight_storage_guard(state, &[workdir.to_path_buf()]).await;
    if snapshot.is_emergency() {
        anyhow::bail!(storage_emergency_message(snapshot.active.as_ref()));
    }
    Ok(())
}

pub async fn evaluate_storage_guard(
    state: &Arc<DaemonState>,
    extra_paths: &[PathBuf],
) -> Result<StorageGuardStatus> {
    let observed_paths = collect_observed_paths(state, extra_paths).await;
    let disks = sample_storage_disks(state).await;
    let (previous, snapshot, warnings) = state
        .core
        .storage_guard
        .evaluate(&state.core.data_root, &observed_paths, &disks)
        .await;
    emit_reserve_warnings(warnings);

    publish_storage_guard_snapshot(state, &previous, &snapshot).await;
    Ok(snapshot)
}

async fn refresh_preflight_storage_guard(
    state: &Arc<DaemonState>,
    extra_paths: &[PathBuf],
) -> StorageGuardStatus {
    let observed_paths = collect_observed_paths(state, extra_paths).await;
    let disks = sample_storage_disks(state).await;
    let (previous, snapshot) =
        state
            .core
            .storage_guard
            .sample_preflight(&state.core.data_root, &observed_paths, &disks);
    publish_storage_guard_snapshot(state, &previous, &snapshot).await;
    snapshot
}

impl DaemonState {
    pub fn storage_guard_snapshot(&self) -> StorageGuardStatus {
        self.core.storage_guard.snapshot()
    }
}

#[cfg(test)]
mod tests;
