use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use super::DaemonShutdownHost;
use crate::daemon::{reconcile_running_turns_with_reason, DaemonState};

pub async fn shutdown_shared_substrate(
    state: &Arc<DaemonState>,
    reason: &str,
) -> Result<Option<ctx_avf_linux_runtime::SubstrateLifecycleRecord>> {
    let Some(record) = state
        .execution
        .harness
        .save_or_stop_selected_shared_substrate()
        .await?
    else {
        return Ok(None);
    };

    tracing::info!(
        shutdown_reason = reason,
        substrate = ?record.substrate,
        shutdown_outcome = ?record.shutdown_outcome,
        shutdown_detail = ?record.shutdown_reason,
        save_error_present = record.save_error_present,
        saved_state_written_on_shutdown = record.saved_state_written_on_shutdown,
        simulated = record.simulated,
        "shared substrate save-or-stop requested for daemon shutdown"
    );
    Ok(Some(record))
}

async fn trigger_daemon_shutdown(host: DaemonShutdownHost, reason: &str) {
    tracing::info!("daemon shutdown requested: {reason}");
    let _ = host.acquire_shutdown_drain(reason).await;
    if let Err(err) = host.reconcile_running_turns_with_reason(reason).await {
        tracing::warn!("failed to reconcile running turns during daemon shutdown: {err:#}");
    }
    host.shutdown_provider_adapters(reason).await;
    match host.save_or_stop_selected_shared_substrate().await {
        Ok(Some(record)) => {
            tracing::info!(
                shutdown_reason = reason,
                substrate = ?record.substrate,
                shutdown_outcome = ?record.shutdown_outcome,
                shutdown_detail = ?record.shutdown_reason,
                save_error_present = record.save_error_present,
                saved_state_written_on_shutdown = record.saved_state_written_on_shutdown,
                simulated = record.simulated,
                "shared substrate save-or-stop requested for daemon shutdown"
            );
        }
        Ok(None) => {}
        Err(err) => {
            tracing::warn!(
                "failed to save-or-stop shared substrate during daemon shutdown: {err:#}"
            );
        }
    }
    host.broadcast_shutdown();
}

async fn trigger_daemon_shutdown_from_state(state: Arc<DaemonState>, reason: &str) {
    tracing::info!("daemon shutdown requested: {reason}");
    let _ = state
        .core
        .update_drain
        .acquire(reason, "daemon_shutdown")
        .await;
    if let Err(err) = reconcile_running_turns_with_reason(&state, reason).await {
        tracing::warn!("failed to reconcile running turns during daemon shutdown: {err:#}");
    }
    state.providers.shutdown_provider_adapters(reason).await;
    if let Err(err) = shutdown_shared_substrate(&state, reason).await {
        tracing::warn!("failed to save-or-stop shared substrate during daemon shutdown: {err:#}");
    }
    let _ = state.core.shutdown_tx.send(());
}

pub(in crate::daemon) fn spawn_deferred_daemon_shutdown(
    host: DaemonShutdownHost,
    reason: String,
    delay: Duration,
) {
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        trigger_daemon_shutdown(host, &reason).await;
    });
}

pub(in crate::daemon) fn spawn_process_shutdown_listener(state: Arc<DaemonState>) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            let mut sigterm =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    Ok(signal) => signal,
                    Err(err) => {
                        tracing::warn!("failed to register SIGTERM handler: {err:#}");
                        return;
                    }
                };
            tokio::select! {
                result = tokio::signal::ctrl_c() => {
                    if let Err(err) = result {
                        tracing::warn!("failed to listen for ctrl_c: {err:#}");
                        return;
                    }
                    trigger_daemon_shutdown_from_state(state, "ctrl_c").await;
                }
                _ = sigterm.recv() => {
                    trigger_daemon_shutdown_from_state(state, "sigterm").await;
                }
            }
        }

        #[cfg(not(unix))]
        {
            if let Err(err) = tokio::signal::ctrl_c().await {
                tracing::warn!("failed to listen for ctrl_c: {err:#}");
                return;
            }
            trigger_daemon_shutdown_from_state(state, "ctrl_c").await;
        }
    });
}
